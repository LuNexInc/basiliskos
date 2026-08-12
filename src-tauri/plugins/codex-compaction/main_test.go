package main

import (
	"encoding/json"
	"strings"
	"testing"
)

func TestCompactionTriggerDetectionAndRewrite(t *testing.T) {
	req := []byte(`{"model":"gpt-5.6-sol","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"Some context here."}]},{"type":"compaction_trigger"}],"tools":[{"type":"function","name":"bash"}],"stream":true}`)
	if !isCompactionTriggerRequest(req) {
		t.Fatal("expected compaction trigger detection")
	}
	out, err := rewriteCompactionRequestToSummarizer(req)
	if err != nil {
		t.Fatal(err)
	}
	var parsed map[string]any
	if err := json.Unmarshal(out, &parsed); err != nil {
		t.Fatal("transformed body must be valid JSON:", err)
	}
	input := parsed["input"].([]any)
	last := input[len(input)-1].(map[string]any)
	if last["type"] != "message" {
		t.Fatalf("last item should be the summarizer prompt message, got %v", last["type"])
	}
	// No compaction_trigger left.
	if strings.Contains(string(out), "compaction_trigger") {
		t.Fatal("compaction_trigger must be removed")
	}
	// Tools cleared.
	if _, ok := parsed["tools"]; ok {
		t.Fatal("tools must be cleared for the summarizer call")
	}
}

func TestEnvelopeRoundTrip(t *testing.T) {
	enc := encodeEnvelope("the compacted summary text")
	dec := decodeEnvelope(enc)
	if dec != "the compacted summary text" {
		t.Fatalf("round trip failed: %q", dec)
	}
	if decodeEnvelope("random-encrypted-blob") != "" {
		t.Fatal("non-ocx1 blobs must decode to empty")
	}
}

func TestSynthesizeCompactionSSE(t *testing.T) {
	out := synthesizeCompactionSSE("summary here")
	s := string(out)
	if !strings.Contains(s, `"type":"compaction"`) {
		t.Fatal("compaction item missing")
	}
	if !strings.Contains(s, "ocx1:") {
		t.Fatal("ocx1 envelope missing")
	}
	if !strings.Contains(s, "response.completed") {
		t.Fatal("response.completed missing")
	}
	// exactly one compaction item
	if strings.Count(s, `"type":"compaction"`) != 2 { // added + done
		t.Fatalf("expected 2 compaction item occurrences (added+done), got %d", strings.Count(s, `"type":"compaction"`))
	}
}

func TestDecodeCompactionInputItems(t *testing.T) {
	enc := encodeEnvelope("handoff summary")
	req := []byte(`{"model":"gpt-5.6-sol","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"original"}]},{"type":"compaction","encrypted_content":"` + enc + `"}]}`)
	if !hasCompactionInputItems(req) {
		t.Fatal("expected compaction input item")
	}
	out, err := decodeCompactionInputItems(req)
	if err != nil {
		t.Fatal(err)
	}
	var parsed map[string]any
	if err := json.Unmarshal(out, &parsed); err != nil {
		t.Fatal(err)
	}
	input := parsed["input"].([]any)
	last := input[len(input)-1].(map[string]any)
	content := last["content"].([]any)
	text := content[0].(map[string]any)["text"].(string)
	if !strings.Contains(text, "handoff summary") || !strings.Contains(text, summaryPrefix) {
		t.Fatalf("replay decode did not produce a SUMMARY_PREFIX user message: %q", text[:80])
	}
}
