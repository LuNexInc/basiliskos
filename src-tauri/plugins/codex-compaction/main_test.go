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

func TestRewriteArgumentsIntegerFloats(t *testing.T) {
	// Whole-number float in a function_call arguments string is rewritten.
	in := []byte("event: response.output_item.done\n" +
		`data: {"type":"response.output_item.done","item":{"type":"function_call","name":"shell_command","arguments":"{\"command\":\"dir\",\"timeout_ms\":15000.0}"}}` +
		"\n\n")
	out := string(rewriteArgumentsIntegerFloats(in))
	if strings.Contains(out, "15000.0") {
		t.Fatalf("whole-number float not rewritten: %s", out)
	}
	if !strings.Contains(out, ":15000") {
		t.Fatalf("integer spelling missing: %s", out)
	}

	// Model prose with "2.0" is never touched (not an arguments field).
	prose := []byte(`data: {"type":"response.output_text.delta","delta":"version 2.0 is out"}`)
	if string(rewriteArgumentsIntegerFloats(prose)) != string(prose) {
		t.Fatalf("prose corrupted: %s", rewriteArgumentsIntegerFloats(prose))
	}

	// String values, real floats, and negatives inside arguments survive.
	in3 := []byte(`data: {"type":"response.output_item.done","item":{"type":"function_call","arguments":"{\"v\":\"2.0.0\",\"ratio\":1.05,\"n\":-15000.0,\"z\":0.0}"}}`)
	out3 := string(rewriteArgumentsIntegerFloats(in3))
	if !strings.Contains(out3, `\"v\":\"2.0.0\"`) {
		t.Fatalf("string value corrupted: %s", out3)
	}
	if !strings.Contains(out3, "1.05") {
		t.Fatalf("real float corrupted: %s", out3)
	}
	if !strings.Contains(out3, "-15000") || strings.Contains(out3, "-15000.0") {
		t.Fatalf("negative integer rewrite broken: %s", out3)
	}
	if !strings.Contains(out3, `\"z\":0}`) {
		t.Fatalf("zero float not rewritten: %s", out3)
	}

	// Grok's other quirk: it sometimes quotes integers as strings.
	// "15000.0" must become the bare integer 15000 (no quotes).
	inS := []byte(`data: {"type":"response.output_item.done","item":{"type":"function_call","arguments":"{\"duration_ms\":\"15000.0\"}"}}`)
	outS := string(rewriteArgumentsIntegerFloats(inS))
	if strings.Contains(outS, "15000.0") || !strings.Contains(outS, ":15000") {
		t.Fatalf("string-wrapped float not rewritten: %s", outS)
	}
}

func TestWholeNumberFloatString(t *testing.T) {
	cases := map[string]string{
		"15000.0":  "15000",
		"15000.00": "15000",
		"-15000.0": "-15000",
		"0.0":      "0",
	}
	for in, want := range cases {
		got, ok := wholeNumberFloatString(in)
		if !ok || got != want {
			t.Fatalf("wholeNumberFloatString(%q) = %q,%v want %q,true", in, got, ok, want)
		}
	}
	for _, bad := range []string{"2.0.0", "1.05", "15000", "abc", "", "1e3"} {
		if _, ok := wholeNumberFloatString(bad); ok {
			t.Fatalf("wholeNumberFloatString(%q) should be false", bad)
		}
	}
}

func TestMergeCodexAppTools(t *testing.T) {
	// A reduced codex_app namespace (1 tool) must be expanded to the full 17.
	body := []byte(`{"model":"grok-4.6","tools":[{"type":"namespace","name":"codex_app","description":"Tools provided by the Codex app.","tools":[{"type":"function","name":"read_thread_terminal","description":"Read terminal","inputSchema":{}}]},{"type":"function","name":"shell_command","parameters":{}}]}`)
	out := mergeCodexAppTools(body)
	var parsed struct {
		Tools []struct {
			Type  string `json:"type"`
			Name  string `json:"name"`
			Tools []struct {
				Name string `json:"name"`
			} `json:"tools"`
		} `json:"tools"`
	}
	if err := json.Unmarshal(out, &parsed); err != nil {
		t.Fatal(err)
	}
	var codexNS []string
	for _, tl := range parsed.Tools {
		if tl.Name == "codex_app" {
			for _, f := range tl.Tools {
				codexNS = append(codexNS, f.Name)
			}
		}
	}
	if len(codexNS) < 16 {
		t.Fatalf("expected >=16 codex_app tools after merge, got %d: %v", len(codexNS), codexNS)
	}
	if !contains(codexNS, "open_in_codex") || !contains(codexNS, "read_thread_terminal") {
		t.Fatalf("merged namespace missing tools: %v", codexNS)
	}
	// automation_update uses a oneOf/$defs schema (no top-level type), which
	// the relay rejects, so it must never be injected.
	if contains(codexNS, "automation_update") {
		t.Fatalf("automation_update must be excluded: %v", codexNS)
	}
	// The reduced tool must not be duplicated.
	if count(codexNS, "read_thread_terminal") != 1 {
		t.Fatalf("read_thread_terminal duplicated: %v", codexNS)
	}

	// A non-Codex body (no codex_app) must be returned byte-identical.
	claude := []byte(`{"model":"claude-sonnet","system":"x","messages":[]}`)
	if string(mergeCodexAppTools(claude)) != string(claude) {
		t.Fatal("non-Codex body must be unchanged")
	}

	// Idempotent: merging an already-merged body adds nothing.
	out2 := mergeCodexAppTools(out)
	if string(out2) != string(out) {
		t.Fatal("merge must be idempotent")
	}
}

func contains(list []string, s string) bool {
	for _, v := range list {
		if v == s {
			return true
		}
	}
	return false
}

func count(list []string, s string) int {
	n := 0
	for _, v := range list {
		if v == s {
			n++
		}
	}
	return n
}

func TestSanitizeToolSchema(t *testing.T) {
	node := map[string]any{
		"$schema": "https://json-schema.org/draft/2020-12/schema",
		"type":    "object",
		"properties": map[string]any{
			"target": map[string]any{
				"anyOf": []any{
					map[string]any{"type": "object", "properties": map[string]any{"type": map[string]any{"type": "string", "const": "file"}}},
				},
			},
			"placement": map[string]any{"type": "string", "enum": []any{"right", "bottom"}},
		},
		"required": []any{"target"},
	}
	sanitizeToolSchema(node)
	if _, ok := node["$schema"]; ok {
		t.Fatal("$schema must be stripped")
	}
	if node["type"] != "object" {
		t.Fatalf("root type must be object, got %v", node["type"])
	}
	target := node["properties"].(map[string]any)["target"].(map[string]any)
	if _, ok := target["anyOf"]; ok {
		t.Fatal("anyOf must be stripped")
	}
	if target["type"] != "object" {
		t.Fatalf("collapsed union must be object, got %v", target["type"])
	}
	// The merged open_in_codex schema must be a plain type:object schema.
	var merged map[string]any
	for _, ns := range appNamespaceTools["codex_app"] {
		var m map[string]any
		if err := json.Unmarshal(ns, &m); err != nil {
			continue
		}
		if m["name"] == "open_in_codex" {
			merged = m
		}
	}
	if merged == nil {
		t.Fatal("open_in_codex missing from loaded tools")
	}
	raw, _ := json.Marshal(merged["inputSchema"])
	if string(raw) == "" || merged["inputSchema"].(map[string]any)["type"] != "object" {
		t.Fatal("open_in_codex schema missing type:object")
	}
	if strings.Contains(string(raw), "$schema") || strings.Contains(string(raw), "anyOf") || strings.Contains(string(raw), "const") {
		t.Fatalf("open_in_codex schema still has complex keywords: %s", raw)
	}
}

func TestCoerceIntegerFloat(t *testing.T) {
	cases := map[string]string{
		"15000.0":   "15000",
		"15000.00":  "15000",
		"-15000.0":  "-15000",
		"0.0":       "0",
		"1.05":      "1.05",
		"2.5":       "2.5",
		"1e3":       "1e3",
		"1.0e3":     "1.0e3",
		"15000":     "15000",
	}
	for in, want := range cases {
		if got := coerceIntegerFloat(in); got != want {
			t.Fatalf("coerceIntegerFloat(%q) = %q, want %q", in, got, want)
		}
	}
}

