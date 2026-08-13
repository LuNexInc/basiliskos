package main

import (
	"bytes"
	"embed"
	"encoding/json"
	"fmt"

	"github.com/tidwall/gjson"
	"github.com/tidwall/sjson"
)

// codex_app_tools.json is a snapshot of the Codex Desktop app's native tool
// namespaces (codex_app + plugin_management) captured from a live session
// (session_meta.dynamic_tools). The Codex app sends a REDUCED codex_app
// namespace on routed (non-native-model) requests — the rest of the tools are
// deferred client-side. The relay must merge the full set back in so a routed
// model (grok) sees and can call the same app tools a native model sees. The
// app still executes these calls natively; the relay never runs them itself.
//
//go:embed codex_app_tools.json
var codexAppToolsFS embed.FS

// appNamespaceTools maps a namespace name to its full tool definitions (raw
// JSON), loaded once at startup.
var appNamespaceTools = loadAppNamespaceTools()

func loadAppNamespaceTools() map[string][]json.RawMessage {
	out := map[string][]json.RawMessage{}
	raw, err := codexAppToolsFS.ReadFile("codex_app_tools.json")
	if err != nil {
		return out
	}
	var namespaces []struct {
		Type  string            `json:"type"`
		Name  string            `json:"name"`
		Tools []json.RawMessage `json:"tools"`
	}
	if err := json.Unmarshal(raw, &namespaces); err != nil {
		return out
	}
	for _, ns := range namespaces {
		if ns.Type != "namespace" || ns.Name == "" {
			continue
		}
		var tools []json.RawMessage
		for _, tool := range ns.Tools {
			var parsed map[string]any
			if err := json.Unmarshal(tool, &parsed); err != nil {
				continue
			}
			schema, ok := parsed["inputSchema"].(map[string]any)
			if !ok || schema["type"] != "object" {
				// Skip non-object schemas (e.g. automation_update's oneOf).
				continue
			}
			sanitizeToolSchema(schema)
			parsed["inputSchema"] = schema
			clean, err := json.Marshal(parsed)
			if err != nil {
				continue
			}
			tools = append(tools, clean)
		}
		out[ns.Name] = tools
	}
	return out
}

// sanitizeToolSchema strips JSON Schema features the relay and grok reject
// ($schema/$defs/$ref, oneOf/anyOf/allOf, const, format, exclusive bounds),
// leaving a plain `type: object` schema that still carries properties and
// enums. A property that used a oneOf/anyOf union collapses to a loose object.
func sanitizeToolSchema(node map[string]any) {
	for _, key := range []string{"$schema", "$id", "$defs", "$ref", "format", "const", "exclusiveMinimum", "exclusiveMaximum", "default", "examples"} {
		delete(node, key)
	}
	for _, key := range []string{"oneOf", "anyOf", "allOf"} {
		if _, ok := node[key]; ok {
			delete(node, key)
			node["type"] = "object"
		}
	}
	for _, value := range node {
		switch v := value.(type) {
		case map[string]any:
			sanitizeToolSchema(v)
		case []any:
			for _, item := range v {
				if m, ok := item.(map[string]any); ok {
					sanitizeToolSchema(m)
				}
			}
		}
	}
}

// mergeCodexAppTools merges the full codex_app/plugin_management tool
// definitions into a Responses request body. It only touches namespaces the
// client already sent (the reduced set), so non-Codex traffic is never
// affected. Returns the original body unchanged when there is nothing to do.
func mergeCodexAppTools(body []byte) []byte {
	if len(body) == 0 || !bytes.Contains(body, []byte(`"codex_app"`)) {
		return body
	}
	count := gjson.GetBytes(body, "tools.#").Int()
	if count == 0 {
		return body
	}
	out := body
	for i := int64(0); i < count; i++ {
		base := fmt.Sprintf("tools.%d", i)
		if gjson.GetBytes(body, base+".type").String() != "namespace" {
			continue
		}
		name := gjson.GetBytes(body, base+".name").String()
		snapshot, ok := appNamespaceTools[name]
		if !ok || len(snapshot) == 0 {
			continue
		}
		existing := map[string]bool{}
		existingCount := gjson.GetBytes(body, base+".tools.#").Int()
		for j := int64(0); j < existingCount; j++ {
			toolName := gjson.GetBytes(body, fmt.Sprintf("%s.tools.%d.name", base, j)).String()
			if toolName != "" {
				existing[toolName] = true
			}
		}
		for _, snapTool := range snapshot {
			var probe struct {
				Name string `json:"name"`
			}
			if err := json.Unmarshal(snapTool, &probe); err != nil || probe.Name == "" {
				continue
			}
			if existing[probe.Name] {
				continue
			}
			var err error
			out, err = sjson.SetRawBytes(out, base+".tools.-1", snapTool)
			if err != nil {
				return body // abort on error; leave the body unchanged
			}
			existing[probe.Name] = true
		}
	}
	return out
}
