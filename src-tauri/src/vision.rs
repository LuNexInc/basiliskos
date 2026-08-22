use serde_json::Value;

// Declarative, per-provider list of client-side request fixups for confirmed
// CLIProxyAPI tool-schema translation gaps. Add an entry here only for a
// gap that's actually confirmed against CLIProxyAPI's issue tracker (or
// reproduced directly) and that Basiliskos's own traffic shape (Claude
// Messages API requests on /v1/messages) can actually trigger — see
// gateway.rs history/handoffs for what was checked and ruled out.
pub(crate) fn tool_compatibility_fixups(
    provider: &str,
) -> &'static [fn(&mut serde_json::Map<String, Value>)] {
    match provider {
        // CLIProxyAPI issue #4339 (v7.2.73+): used to inject a native x_search
        // tool into Grok /v1/responses after translating this request. v7.2.128
        // ships `xai.inject-x-search: false` as the default (and render_config
        // sets it explicitly), so the injection conflict is gone. The strip
        // stays as a guard: Claude Desktop's native web_search declaration is
        // still not valid for xAI, and its forced web_search tool_choice would
        // otherwise reach the upstream.
        "xai" => {
            &[strip_xai_incompatible_native_web_search as fn(&mut serde_json::Map<String, Value>)]
        }
        // CLIProxyAPI issue #4405: Kimi's /v1/messages path returns 400 when a
        // tool_result block's nested content contains an Anthropic deferred-tool
        // tool_reference block (e.g. from Claude Code's own ToolSearch flow).
        // Flattening it to plain text (upstream's own suggested fix) returns 200.
        "kimi" => &[flatten_kimi_tool_reference_blocks as fn(&mut serde_json::Map<String, Value>)],
        _ => &[],
    }
}

pub(crate) fn flatten_kimi_tool_reference_blocks(object: &mut serde_json::Map<String, Value>) {
    let Some(Value::Array(messages)) = object.get_mut("messages") else {
        return;
    };
    for message in messages {
        let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
            continue;
        };
        for block in content {
            if block.get("type").and_then(Value::as_str) != Some("tool_result") {
                continue;
            }
            let Some(nested) = block.get_mut("content").and_then(Value::as_array_mut) else {
                continue;
            };
            for nested_block in nested {
                if nested_block.get("type").and_then(Value::as_str) != Some("tool_reference") {
                    continue;
                }
                let tool_name = nested_block
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                *nested_block = serde_json::json!({"type": "text", "text": tool_name});
            }
        }
    }
}

pub(crate) fn strip_xai_incompatible_native_web_search(
    object: &mut serde_json::Map<String, Value>,
) {
    let mut normalized_native_web_search = false;
    {
        let Some(Value::Array(tools)) = object.get_mut("tools") else {
            return;
        };
        for tool in tools.iter_mut() {
            let tool_type = tool.get("type").and_then(Value::as_str).unwrap_or_default();
            if tool_type == "web_search" || tool_type.starts_with("web_search_") {
                *tool = serde_json::json!({"type": "x_search", "name": "x_search"});
                normalized_native_web_search = true;
            }
        }
    }
    if !normalized_native_web_search {
        return;
    }

    if xai_tool_choice_targets_native_web_search(object.get("tool_choice")) {
        object.insert(
            "tool_choice".into(),
            serde_json::json!({"type": "tool", "name": "x_search"}),
        );
    }
}

pub(crate) fn xai_tool_choice_targets_native_web_search(choice: Option<&Value>) -> bool {
    let Some(choice) = choice.and_then(Value::as_object) else {
        return false;
    };
    let choice_type = choice
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    choice_type == "web_search"
        || choice_type.starts_with("web_search_")
        || (choice_type == "tool"
            && choice.get("name").and_then(Value::as_str) == Some("web_search"))
}
