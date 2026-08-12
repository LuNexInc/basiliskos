package main

// Remote compaction v2 support for routed (non-OpenAI) models, mirroring
// opencodex (src/responses/compaction.ts):
//   - A Responses request whose input ends with {"type":"compaction_trigger"}
//     is rewritten into a plain summarizer call (append the compaction prompt,
//     clear tools). The routed model writes the summary.
//   - The response stream is intercepted: the model's ordinary output is
//     swallowed, the text accumulated, and exactly one
//     {"type":"compaction","encrypted_content":"ocx1:"+base64(summary)} item
//     is synthesized before response.completed — the contract
//     codex-rs collect_compaction_output requires.
//   - Stored compaction items are decoded on replay: ocx1 envelopes become
//     plain user messages (SUMMARY_PREFIX framing); opaque OpenAI-encrypted
//     blobs degrade to a short note; context_compaction markers without a
//     payload are dropped.

import (
	"bytes"
	"encoding/base64"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/tidwall/gjson"
	"github.com/tidwall/sjson"
)

const (
	ocxCompactionPrefix  = "ocx1:"
	opaqueCompactionNote = "[earlier conversation was compacted; the summary is stored in a format this model cannot read]"
	summaryPrefix        = "Another language model started to solve this problem and produced a summary of its thinking process. You also have access to the state of the tools that were used by that language model. Use this to build on the work that has already been done and avoid duplicating work. Here is the summary produced by the other language model, use the information in this summary to assist with your own analysis:"
)

// Mirrors codex-rs core/templates/compact/prompt.md (the local-compaction
// instruction), as opencodex does.
const compactPrompt = `You are performing a CONTEXT CHECKPOINT COMPACTION. Create a handoff summary for another LLM that will resume the task.

Include:
- Current progress and key decisions made
- Important context, constraints, or user preferences
- What remains to be done (clear next steps)
- Any critical data, examples, or references needed to continue

Be concise, structured, and focused on helping the next LLM seamlessly continue the work.`

type streamChunkInterceptRequest struct {
	RequestID       string
	ChunkIndex      int
	OriginalRequest []byte
	RequestBody     []byte
	Body            []byte
}

type streamChunkInterceptResponse struct {
	Body      []byte
	DropChunk bool
}

func isCompactionTriggerRequest(body []byte) bool {
	if len(body) == 0 {
		return false
	}
	count := gjson.GetBytes(body, "input.#").Int()
	if count == 0 {
		return false
	}
	last := gjson.GetBytes(body, fmt.Sprintf("input.%d.type", count-1)).String()
	return last == "compaction_trigger"
}

// rewriteCompactionRequestToSummarizer turns the compaction request into a
// plain summarizer call: remove the trigger item, append the compaction prompt
// as the final user message, and clear tools. Encrypted content parts are left
// byte-identical (sjson only touches the edited paths).
func rewriteCompactionRequestToSummarizer(body []byte) ([]byte, error) {
	out := body
	count := gjson.GetBytes(out, "input.#").Int()
	if count > 0 {
		var err error
		out, err = sjson.DeleteBytes(out, fmt.Sprintf("input.%d", count-1))
		if err != nil {
			return nil, err
		}
	}
	var err error
	promptItem := fmt.Sprintf(`{"type":"message","role":"user","content":[{"type":"input_text","text":%s}]}`, mustJSONString(compactPrompt))
	out, err = sjson.SetRawBytes(out, "input.-1", []byte(promptItem))
	if err != nil {
		return nil, err
	}
	for _, path := range []string{"tools", "tool_choice", "parallel_tool_calls"} {
		if gjson.GetBytes(out, path).Exists() {
			out, err = sjson.DeleteBytes(out, path)
			if err != nil {
				return nil, err
			}
		}
	}
	return out, nil
}

func mustJSONString(s string) string {
	b, _ := json.Marshal(s)
	return string(b)
}

func hasCompactionInputItems(body []byte) bool {
	count := gjson.GetBytes(body, "input.#").Int()
	for i := int64(0); i < count; i++ {
		t := gjson.GetBytes(body, fmt.Sprintf("input.%d.type", i)).String()
		if t == "compaction" || t == "compaction_summary" || t == "context_compaction" {
			return true
		}
	}
	return false
}

// decodeCompactionInputItems rewrites stored compaction items into plain user
// messages so the routed model keeps the compacted context.
func decodeCompactionInputItems(body []byte) ([]byte, error) {
	out := body
	count := gjson.GetBytes(out, "input.#").Int()
	for i := count - 1; i >= 0; i-- {
		t := gjson.GetBytes(out, fmt.Sprintf("input.%d.type", i)).String()
		if t != "compaction" && t != "compaction_summary" && t != "context_compaction" {
			continue
		}
		enc := gjson.GetBytes(out, fmt.Sprintf("input.%d.encrypted_content", i)).String()
		var replacement []byte
		if decoded := decodeEnvelope(enc); decoded != "" {
			replacement = []byte(fmt.Sprintf(`{"type":"message","role":"user","content":[{"type":"input_text","text":%s}]}`, mustJSONString(summaryPrefix+"\n\n"+decoded)))
		} else if t == "context_compaction" && enc == "" {
			replacement = nil
		} else {
			replacement = []byte(fmt.Sprintf(`{"type":"message","role":"user","content":[{"type":"input_text","text":%s}]}`, mustJSONString(opaqueCompactionNote)))
		}
		if replacement == nil {
			var err error
			out, err = sjson.DeleteBytes(out, fmt.Sprintf("input.%d", i))
			if err != nil {
				return nil, err
			}
			continue
		}
		var err error
		out, err = sjson.SetRawBytes(out, fmt.Sprintf("input.%d", i), replacement)
		if err != nil {
			return nil, err
		}
	}
	return out, nil
}

func decodeEnvelope(enc string) string {
	if !strings.HasPrefix(enc, ocxCompactionPrefix) {
		return ""
	}
	raw, err := base64.StdEncoding.DecodeString(strings.TrimPrefix(enc, ocxCompactionPrefix))
	if err != nil {
		return ""
	}
	return string(raw)
}

func encodeEnvelope(summary string) string {
	return ocxCompactionPrefix + base64.StdEncoding.EncodeToString([]byte(summary))
}

// interceptStreamChunk handles the response stream of a compaction-mode
// request: it swallows the model's ordinary output, accumulates the summary
// text, and synthesizes exactly one compaction item before response.completed.
func interceptStreamChunk(raw []byte) ([]byte, error) {
	var req streamChunkInterceptRequest
	if err := json.Unmarshal(raw, &req); err != nil {
		return nil, err
	}
	if req.RequestID == "" {
		return okEnvelope(streamChunkInterceptResponse{Body: req.Body})
	}

	state.mu.Lock()
	isCompaction := state.compactionReq[req.RequestID]
	buf := state.buffer[req.RequestID]
	summaryBuilder := state.summary[req.RequestID]
	state.mu.Unlock()

	if !isCompaction || buf == nil || summaryBuilder == nil {
		return okEnvelope(streamChunkInterceptResponse{Body: req.Body})
	}
	// Header-init chunk: pass through (headers only).
	if req.ChunkIndex == -1 {
		return okEnvelope(streamChunkInterceptResponse{Body: req.Body})
	}

	// Persistent SSE reassembly: frames are split on the `\ndata: ` marker.
	buf.Write(req.Body)
	complete := processCompleteSSEEvents(buf, summaryBuilder)
	// The stream ends with response.completed; if it arrived without a
	// trailing blank line, the splitter never emits it as a complete event.
	// Detect it directly and process the remainder as the final event.
	if !complete && bytes.Contains(buf.Bytes(), []byte(`"type":"response.completed"`)) {
		processRemainderAsFinal(buf, summaryBuilder)
		complete = true
	}
	if complete {
		appendMarker("COMPLETED_SEEN " + req.RequestID + " summary_len=" + fmt.Sprint(summaryBuilder.Len()))
	}
	if complete {
		processCompleteSSEEvents(buf, summaryBuilder)
		summary := summaryBuilder.String()
		synth := synthesizeCompactionSSE(summary)
		state.mu.Lock()
		delete(state.compactionReq, req.RequestID)
		delete(state.summary, req.RequestID)
		delete(state.buffer, req.RequestID)
		state.mu.Unlock()
		appendMarker("COMPACTION_SYNTHESIZED " + req.RequestID)
		return okEnvelope(streamChunkInterceptResponse{Body: synth})
	}
	return okEnvelope(streamChunkInterceptResponse{DropChunk: true})
}

// processCompleteSSEEvents consumes complete SSE frames from the reassembly
// buffer. CPA's framing is `event: NAME\ndata: {json}` with NO separator
// before the next frame, so frames are split on the `\ndata: ` marker and a
// frame is complete only when the NEXT frame's marker has arrived. Returns
// true when response.completed was seen.
func processCompleteSSEEvents(buf *bytes.Buffer, summary *strings.Builder) bool {
	raw := normalizeCRLF(buf.Bytes())
	pos := 0
	completed := false
	for {
		dm := bytes.Index(raw[pos:], []byte("\ndata: "))
		if dm < 0 {
			break // no payload marker yet
		}
		payloadStart := pos + dm + len("\ndata: ")
		dm2 := bytes.Index(raw[payloadStart:], []byte("\ndata: "))
		if dm2 < 0 {
			// Last frame not yet confirmed complete (needs the next frame's
			// marker). Keep it WHOLE — with its `event: NAME\ndata: ` prefix —
			// so the next chunk (or the remainder parser) can still find the
			// marker and parse the payload.
			pos = pos + dm
			break
		}
		frameEnd := payloadStart + dm2
		// Strip the next frame's `event: NAME\n` prefix from the window.
		ev := bytes.LastIndex(raw[payloadStart:frameEnd], []byte("event: "))
		var payload []byte
		if ev >= 0 {
			payload = bytes.TrimSpace(raw[payloadStart : payloadStart+ev])
		} else {
			payload = bytes.TrimSpace(raw[payloadStart:frameEnd])
		}
		if len(payload) > 0 && handleSSEPayload(payload, summary) {
			completed = true
		}
		pos = frameEnd
		if completed {
			break
		}
	}
	// Keep only the unconsumed tail (the last, still-unconfirmed frame).
	if pos > 0 {
		rest := append([]byte(nil), raw[pos:]...)
		buf.Reset()
		buf.Write(rest)
	}
	return completed
}

// normalizeCRLF converts CRLF/CR line endings to LF so SSE splitting works
// regardless of the upstream's framing.
func normalizeCRLF(data []byte) []byte {
	if !bytes.ContainsAny(data, "\r") {
		return data
	}
	return bytes.ReplaceAll(bytes.ReplaceAll(data, []byte("\r\n"), []byte("\n")), []byte("\r"), []byte("\n"))
}

// handleSSEPayload processes one SSE data payload, accumulating output text
// and reporting whether response.completed was seen.
func handleSSEPayload(payload []byte, summary *strings.Builder) bool {
	var parsed struct {
		Type  string `json:"type"`
		Delta string `json:"delta"`
		Text  string `json:"text"`
		Item  struct {
			Type    string `json:"type"`
			Content []struct {
				Type string `json:"type"`
				Text string `json:"text"`
			} `json:"content"`
		} `json:"item"`
	}
	if err := json.Unmarshal(payload, &parsed); err != nil {
		return false
	}
	switch parsed.Type {
	case "response.output_text.delta":
		summary.WriteString(parsed.Delta)
	case "response.output_text.done":
		if parsed.Text != "" {
			summary.WriteString(parsed.Text)
		}
	case "response.output_item.done":
		// Some upstreams deliver the final message as a single completed item
		// (no streaming deltas). Extract text from message content parts.
		if parsed.Item.Type == "message" {
			for _, part := range parsed.Item.Content {
				if part.Type == "output_text" && part.Text != "" {
					summary.WriteString(part.Text)
				}
			}
		}
	case "response.completed":
		return true
	}
	return false
}

// processRemainderAsFinal processes the trailing buffer (the final
// response.completed event that may lack a closing newline).
func processRemainderAsFinal(buf *bytes.Buffer, summary *strings.Builder) {
	raw := normalizeCRLF(buf.Bytes())
	buf.Reset()
	for _, line := range bytes.Split(raw, []byte("\n")) {
		line = bytes.TrimSpace(line)
		if !bytes.HasPrefix(line, []byte("data:")) {
			continue
		}
		payload := bytes.TrimSpace(bytes.TrimPrefix(line, []byte("data:")))
		if len(payload) == 0 {
			continue
		}
		handleSSEPayload(payload, summary)
	}
}

// extractDataPayload returns the concatenated `data:` payload of one SSE event.
func extractDataPayload(event []byte) []byte {
	var out []byte
	for _, line := range bytes.Split(event, []byte("\n")) {
		line = bytes.TrimSpace(line)
		if bytes.HasPrefix(line, []byte("data:")) {
			if len(out) > 0 {
				out = append(out, ' ')
			}
			out = append(out, bytes.TrimSpace(bytes.TrimPrefix(line, []byte("data:")))...)
		}
	}
	return out
}

// synthesizeCompactionSSE emits exactly one compaction output item (added +
// done) followed by response.completed — the contract collect_compaction_output
// requires.
func synthesizeCompactionSSE(summary string) []byte {
	enc := encodeEnvelope(summary)
	item := fmt.Sprintf(`{"type":"compaction","id":"cmp_1","encrypted_content":%s}`, mustJSONString(enc))
	var sb strings.Builder
	sb.WriteString("event: response.output_item.added\n")
	sb.WriteString("data: ")
	sb.WriteString(fmt.Sprintf(`{"type":"response.output_item.added","output_index":0,"item":%s}`, item))
	sb.WriteString("\n\n")
	sb.WriteString("event: response.output_item.done\n")
	sb.WriteString("data: ")
	sb.WriteString(fmt.Sprintf(`{"type":"response.output_item.done","output_index":0,"item":%s}`, item))
	sb.WriteString("\n\n")
	sb.WriteString("event: response.completed\n")
	sb.WriteString("data: ")
	sb.WriteString(`{"type":"response.completed","response":{"id":"resp_compaction_1","object":"response","status":"completed","output":[`)
	sb.WriteString(item)
	sb.WriteString(`]}}`)
	sb.WriteString("\n\n")
	return []byte(sb.String())
}

// appendMarker appends a debug line to ~/.hydra-gateway/gateway/plugins/codex-compaction.log
// (best-effort; only for verifying the plugin integration during bring-up).
func appendMarker(line string) {
	home, err := os.UserHomeDir()
	if err != nil {
		return
	}
	f, err := os.OpenFile(filepath.Join(home, ".hydra-gateway", "gateway", "plugins", "codex-compaction.log"),
		os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return
	}
	defer f.Close()
	_, _ = f.WriteString(line + "\n")
}
