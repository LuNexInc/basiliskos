package main

import (
	"encoding/json"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"

	"github.com/tidwall/gjson"
)

func TestIsDeepSeekResponsesRequest(t *testing.T) {
	responses := []byte(`{"model":"deepseek-v4-flash","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"hi"}]}]}`)
	if !isDeepSeekResponsesRequest("deepseek-v4-pro", responses) {
		t.Fatal("DeepSeek Responses body must hop")
	}
	if isDeepSeekResponsesRequest("grok-4.6", responses) {
		t.Fatal("Grok must stay on the CPA executor")
	}
	chat := []byte(`{"model":"deepseek-v4-flash","messages":[{"role":"user","content":"hi"}]}`)
	if isDeepSeekResponsesRequest("deepseek-v4-flash", chat) {
		t.Fatal("Chat Completions body must not hop")
	}
}

func TestSanitizeStripsEncryptedReasoning(t *testing.T) {
	in := []byte(`{"model":"deepseek-v4-flash","previous_response_id":"resp_1","input":[{"type":"reasoning","encrypted_content":"ocx-secret","summary":[{"text":"nope"}],"content":[{"type":"reasoning_text","text":"plain"}]},{"type":"function_call","call_id":"call_A","name":"shell","arguments":"{}"}]}`)
	out, err := sanitizeDeepSeekResponsesBody(in)
	if err != nil {
		t.Fatal(err)
	}
	if gjson.GetBytes(out, "previous_response_id").Exists() {
		t.Fatal("previous_response_id must be removed")
	}
	if gjson.GetBytes(out, "input.0.encrypted_content").Exists() {
		t.Fatal("encrypted_content must be stripped")
	}
	if gjson.GetBytes(out, "input.0.summary").Exists() {
		t.Fatal("reasoning summary must be stripped")
	}
	if got := gjson.GetBytes(out, "input.0.content.0.text").String(); got != "plain" {
		t.Fatalf("plain reasoning text = %q", got)
	}
	if gjson.GetBytes(out, "input.1.type").String() != "function_call" {
		t.Fatal("function_call item must stay")
	}
}

func TestLoadEnabledDeepSeekAPIKey(t *testing.T) {
	dir := t.TempDir()
	disabled := []byte(`{"disabled":true,"api_key":"sk-disabledkeyfixture"}`)
	enabled := []byte(`{"disabled":false,"api_key":"sk-enabledkeyfixture"}`)
	if err := os.WriteFile(filepath.Join(dir, "deepseek-off.json"), disabled, 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "xai-other.json"), []byte(`{"api_key":"sk-notdeepseek"}`), 0o600); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dir, "deepseek-on.json"), enabled, 0o600); err != nil {
		t.Fatal(err)
	}
	key, err := loadEnabledDeepSeekAPIKey(dir)
	if err != nil {
		t.Fatal(err)
	}
	if key != "sk-enabledkeyfixture" {
		t.Fatalf("key = %q", key)
	}
}

func TestHopPostsResponsesNotChat(t *testing.T) {
	var gotPath string
	var gotBody []byte
	var gotAuth string
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		gotPath = r.URL.Path
		gotAuth = r.Header.Get("Authorization")
		gotBody, _ = io.ReadAll(r.Body)
		w.Header().Set("Content-Type", "text/event-stream")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("data: {\"type\":\"response.completed\"}\n\n"))
	}))
	defer server.Close()

	payload := []byte(`{"model":"deepseek-v4-flash","input":[{"type":"function_call","call_id":"c1","name":"shell","arguments":"{}"}]}`)
	status, headers, body, err := hopDeepSeekResponses(payload, "sk-testfixture", server.URL+"/responses", server.Client())
	if err != nil {
		t.Fatal(err)
	}
	if status != http.StatusOK {
		t.Fatalf("status = %d", status)
	}
	if gotPath != "/responses" {
		t.Fatalf("path = %q, want /responses", gotPath)
	}
	if gotAuth != "Bearer sk-testfixture" {
		t.Fatal("authorization header missing")
	}
	if gjson.GetBytes(gotBody, "messages").Exists() {
		t.Fatal("must not send a Chat Completions messages array")
	}
	if gjson.GetBytes(gotBody, "input.0.type").String() != "function_call" {
		t.Fatalf("upstream body = %s", gotBody)
	}
	if !strings.Contains(string(body), "response.completed") {
		t.Fatalf("downstream body = %s", body)
	}
	if ct := headers.Get("Content-Type"); !strings.Contains(ct, "event-stream") {
		t.Fatalf("content-type = %q", ct)
	}
}

func TestRequestModelPrefersRequestedThenBody(t *testing.T) {
	body := []byte(`{"model":"deepseek-v4-flash"}`)
	if got := requestModel(pluginapiRequestModel{RequestedModel: "deepseek-v4-pro"}, body); got != "deepseek-v4-pro" {
		t.Fatalf("got %q", got)
	}
	if got := requestModel(pluginapiRequestModel{}, body); got != "deepseek-v4-flash" {
		t.Fatalf("got %q", got)
	}
}

func TestResponsesHasImagesAndReplace(t *testing.T) {
	body := []byte(`{"model":"deepseek-v4-flash","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"see this"},{"type":"input_image","image_url":"data:image/png;base64,AAAA"}]}]}`)
	if !responsesHasImages(body) {
		t.Fatal("expected image detection")
	}
	if responsesHasImages([]byte(`{"input":[{"type":"message","content":[{"type":"input_text","text":"hi"}]}]}`)) {
		t.Fatal("text-only must not report images")
	}
	out, err := replaceResponsesImages(body, "a red square")
	if err != nil {
		t.Fatal(err)
	}
	if responsesHasImages(out) {
		t.Fatal("images must be replaced")
	}
	if !strings.Contains(string(out), "Image details:") || !strings.Contains(string(out), "a red square") {
		t.Fatalf("description missing: %s", out)
	}
	if !strings.Contains(string(out), "Do not mention image processing") {
		t.Fatal("presentation guidance missing")
	}
}

func TestDescribeAndReplaceUsesVisionEndpoint(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path != "/hydra/vision-describe" {
			t.Fatalf("path = %s", r.URL.Path)
		}
		if r.Header.Get("Authorization") != "Bearer hydra-key" {
			t.Fatal("missing relay token")
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = w.Write([]byte(`{"description":"a cat on a chair"}`))
	}))
	defer server.Close()
	body := []byte(`{"input":[{"type":"message","content":[{"type":"input_image","image_url":"data:image/png;base64,AAAA"}]}]}`)
	out, err := describeAndReplaceResponsesImages(body, server.URL+"/hydra/vision-describe", "hydra-key", server.Client())
	if err != nil {
		t.Fatal(err)
	}
	if !strings.Contains(string(out), "a cat on a chair") {
		t.Fatalf("got %s", out)
	}
}

func TestHopTerminateEnvelope(t *testing.T) {
	raw, err := terminatedRawResponse(200, http.Header{"Content-Type": {"text/event-stream"}}, []byte("data: ok\n\n"))
	if err != nil {
		t.Fatal(err)
	}
	var env envelope
	if err := json.Unmarshal(raw, &env); err != nil {
		t.Fatal(err)
	}
	if !env.OK {
		t.Fatal(env)
	}
	if !gjson.GetBytes(env.Result, "Terminate").Bool() && !gjson.GetBytes(env.Result, "terminate").Bool() {
		t.Fatalf("terminate flag missing: %s", env.Result)
	}
}
