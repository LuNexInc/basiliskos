//! Opencode Go integration: registers a `basiliskos` provider in the user's
//! opencode config that points at the Basiliskos relay, and carries the relay
//! API key so opencode inherits the active Basiliskos route.
//!
//! This module never swaps accounts. The relay rewrites models per request
//! (and the backend alias table covers chat-completions routing), so pointing
//! opencode at `127.0.0.1:8317/v1` is the entire integration — the same
//! loopback contract the isolated Codex/Claude windows use.

use serde::Serialize;
use serde_json::{json, Map, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use crate::catalog::{model_specs, SUPPORTED_PROVIDERS};
use crate::persistence::{durable_write, secure_create_dir_all};

const PROVIDER_ID: &str = "basiliskos";
const PROVIDER_NPM: &str = "@ai-sdk/openai-compatible";
const RELAY_BASE_URL: &str = "http://127.0.0.1:8317/v1";

fn home_dir() -> Result<PathBuf, String> {
    dirs::home_dir().ok_or_else(|| "Unable to locate your home directory".to_string())
}

fn opencode_config_dir() -> Result<PathBuf, String> {
    Ok(home_dir()?.join(".config").join("opencode"))
}

fn opencode_config_file() -> Result<PathBuf, String> {
    let dir = opencode_config_dir()?;
    for name in ["opencode.json", "opencode.jsonc"] {
        let candidate = dir.join(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Ok(dir.join("opencode.json"))
}

/// Builds the `basiliskos` provider block: every Basiliskos catalog model as a
/// client-facing model id, served through the relay's openai-compatible
/// base URL. opencode sends chat-completions requests; the backend alias table
/// maps each model id to the active route.
fn basiliskos_provider(api_key: &str) -> Value {
    let mut models = Map::new();
    for provider in SUPPORTED_PROVIDERS {
        for spec in model_specs(provider) {
            models.insert(spec.id.to_string(), json!({ "name": spec.label }));
        }
    }
    json!({
        "npm": PROVIDER_NPM,
        "name": "Basiliskos",
        "options": {
            "baseURL": RELAY_BASE_URL,
            "apiKey": api_key,
        },
        "models": Value::Object(models),
    })
}

/// Strips JSONC comments (line `//` and block `/* */`) outside of strings so a
/// user's hand-edited `opencode.jsonc` can be merged safely. Keeps the rewrite
/// to the same file the user already has.
fn strip_jsonc_comments(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let mut escaped = false;
    let mut block_comment = false;
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i] as char;
        if block_comment {
            if ch == '*' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
                block_comment = false;
                i += 1;
            }
        } else if in_string {
            out.push(ch);
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
        } else if ch == '"' {
            in_string = true;
            out.push(ch);
        } else if ch == '/' && i + 1 < bytes.len() && bytes[i + 1] == b'/' {
            // line comment: skip to end of line
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if ch == '/' && i + 1 < bytes.len() && bytes[i + 1] == b'*' {
            block_comment = true;
            i += 1;
        } else {
            out.push(ch);
        }
        i += 1;
    }
    out
}

fn read_existing_json(path: &Path) -> Value {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&strip_jsonc_comments(&raw)).ok())
        .unwrap_or_else(|| json!({}))
}

fn model_count(provider: &Value) -> usize {
    provider
        .get("models")
        .and_then(Value::as_object)
        .map(|models| models.len())
        .unwrap_or(0)
}

#[derive(Serialize)]
pub struct OpencodeStatus {
    pub configured: bool,
    pub config_path: String,
    pub provider_id: String,
    pub model_count: usize,
    pub running: bool,
}

/// Writes (or updates) the `basiliskos` provider into the user's opencode
/// config, preserving any other providers and settings. The relay API key is
/// embedded as the provider's apiKey, mirroring the isolated Codex profile's
/// anchored credential.
#[tauri::command]
pub fn serve_opencode_from_relay() -> Result<OpencodeStatus, String> {
    let api_key = crate::gateway::relay_api_key()?;
    let config_file = opencode_config_file()?;
    secure_create_dir_all(&opencode_config_dir()?)?;

    let mut config = read_existing_json(&config_file);
    let provider_block = basiliskos_provider(&api_key);
    let count = model_count(&provider_block);
    let providers = config
        .as_object_mut()
        .ok_or_else(|| "The opencode config is not a JSON object".to_string())?
        .entry("provider")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| "The opencode config `provider` field is not an object".to_string())?;
    providers.insert(PROVIDER_ID.to_string(), provider_block);

    durable_write(
        &config_file,
        serde_json::to_vec_pretty(&config)
            .map_err(|error| format!("Could not serialize the opencode config: {error}"))?
            .as_slice(),
    )?;

    Ok(OpencodeStatus {
        configured: true,
        config_path: config_file.to_string_lossy().to_string(),
        provider_id: PROVIDER_ID.to_string(),
        model_count: count,
        running: opencode_running_inner(),
    })
}

/// Ensures opencode is configured, then opens it in a new terminal window.
/// The terminal is interactive (the user watches it), so it launches visibly.
#[tauri::command]
pub fn launch_opencode(project: Option<String>) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        serve_opencode_from_relay()?;
        let command = match project {
            Some(path) => format!("opencode \"{}\"", path.replace('"', "\\\"")),
            None => "opencode".to_string(),
        };
        Command::new("cmd")
            .args([
                "/C",
                "start",
                "",
                "powershell",
                "-NoExit",
                "-Command",
                &command,
            ])
            .spawn()
            .map_err(|error| format!("Could not launch opencode: {error}"))?;
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = project;
        Err("Automatic terminal launch is currently available on Windows only".into())
    }
}

#[tauri::command]
pub fn opencode_running() -> bool {
    opencode_running_inner()
}

#[cfg(target_os = "windows")]
fn opencode_running_inner() -> bool {
    Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq opencode.exe", "/NH"])
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .to_lowercase()
                .contains("opencode.exe")
        })
        .unwrap_or(false)
}

#[cfg(not(target_os = "windows"))]
fn opencode_running_inner() -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_jsonc_comments_keeps_urls_and_drops_comments() {
        let input = r#"{
  // a comment
  "baseURL": "http://127.0.0.1:8317/v1",
  /* block
     comment */
  "name": "Basiliskos"
}"#;
        let stripped = strip_jsonc_comments(input);
        let value: Value = serde_json::from_str(&stripped).expect("valid json after strip");
        assert_eq!(value["baseURL"], "http://127.0.0.1:8317/v1");
        assert_eq!(value["name"], "Basiliskos");
        assert!(value.as_object().unwrap().contains_key("baseURL"));
        assert!(value.as_object().unwrap().contains_key("name"));
    }

    #[test]
    fn basiliskos_provider_lists_catalog_models_with_relay_base() {
        let provider = basiliskos_provider("test-key");
        assert_eq!(provider["npm"], PROVIDER_NPM);
        assert_eq!(provider["options"]["baseURL"], RELAY_BASE_URL);
        assert_eq!(provider["options"]["apiKey"], "test-key");
        assert!(model_count(&provider) > 0);
        // grok-4.6 is part of the catalog and must appear as a model id.
        assert!(provider["models"].get("grok-4.6").is_some());
        assert!(provider["models"].get("deepseek-v4-flash").is_some());
    }

    #[test]
    fn merge_preserves_existing_providers() {
        let mut config = json!({
            "$schema": "https://opencode.ai/config.json",
            "provider": { "openai": { "npm": "@ai-sdk/openai" } }
        });
        let providers = config
            .as_object_mut()
            .unwrap()
            .get_mut("provider")
            .unwrap()
            .as_object_mut()
            .unwrap();
        providers.insert(PROVIDER_ID.to_string(), basiliskos_provider("key"));
        assert!(providers.contains_key("openai"));
        assert!(providers.contains_key(PROVIDER_ID));
    }
}
