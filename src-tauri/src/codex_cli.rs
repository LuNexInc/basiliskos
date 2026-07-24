//! Manages real Codex CLI/Desktop accounts — a narrow, single-purpose
//! credential-file switcher, distinct from Basiliskos's own relay routing.
//!
//! This module never touches `~/.codex/config.toml` and never attempts to
//! redirect Codex Desktop's own request routing through Basiliskos. It only
//! ever swaps which credential is in `~/.codex/auth.json`, and only when the
//! user explicitly asks. See `AGENTS.md` — Basiliskos "does NOT control
//! Codex" in any form beyond this narrow operation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};
use uuid::Uuid;

use crate::persistence::{durable_write, load_json_with_recovery, secure_create_dir_all};

fn codex_cli_root() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".hydra-gateway").join("external").join("codex"))
        .ok_or_else(|| "Unable to locate your home directory".to_string())
}

fn codex_cli_store_path() -> Result<PathBuf, String> {
    Ok(codex_cli_root()?.join("accounts.json"))
}

fn relay_auth_dir() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".hydra-gateway").join("gateway").join("auth"))
        .ok_or_else(|| "Unable to locate your home directory".to_string())
}

/// The real Codex CLI's own home directory — respects `CODEX_HOME` exactly
/// like the real `codex` binary does (verified empirically).
fn real_codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            dirs::home_dir()
                .map(|home| home.join(".codex"))
                .unwrap_or_default()
        })
}

fn real_codex_auth_path() -> PathBuf {
    real_codex_home().join("auth.json")
}

fn fingerprint(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn normalized_label(name: &str) -> Result<String, String> {
    let label = name.trim();
    if label.is_empty() {
        return Err("Label cannot be empty".into());
    }
    if label.chars().count() > 64 {
        return Err("Label must be 64 characters or fewer".into());
    }
    Ok(label.to_string())
}

#[cfg(target_os = "windows")]
fn codex_cli_running() -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq codex.exe", "/NH"])
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .to_lowercase()
                .contains("codex.exe")
        })
        .unwrap_or(false)
}

#[cfg(not(target_os = "windows"))]
fn codex_cli_running() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn stop_codex_cli() -> Result<(), String> {
    std::process::Command::new("taskkill")
        .args(["/F", "/IM", "codex.exe", "/T"])
        .output()
        .map(|_| ())
        .map_err(|error| format!("Could not close the running Codex CLI: {error}"))
}

#[cfg(not(target_os = "windows"))]
fn stop_codex_cli() -> Result<(), String> {
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CodexCliAccount {
    id: String,
    label: String,
    email: Option<String>,
    account_id: Option<String>,
    /// Verbatim native `~/.codex/auth.json`-shaped JSON, translated once at
    /// import time. Switching is then always a byte-verbatim swap — the same
    /// safety property grok-hydra relies on for Grok CLI.
    native_auth_json: String,
    added_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CodexCliStore {
    #[serde(default)]
    accounts: Vec<CodexCliAccount>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodexCliAccountView {
    pub id: String,
    pub label: String,
    pub email: Option<String>,
    pub added_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl From<&CodexCliAccount> for CodexCliAccountView {
    fn from(account: &CodexCliAccount) -> Self {
        CodexCliAccountView {
            id: account.id.clone(),
            label: account.label.clone(),
            email: account.email.clone(),
            added_at: account.added_at,
            last_used_at: account.last_used_at,
        }
    }
}

fn views(store: &CodexCliStore) -> Vec<CodexCliAccountView> {
    store
        .accounts
        .iter()
        .map(CodexCliAccountView::from)
        .collect()
}

fn load_codex_cli_store() -> Result<CodexCliStore, String> {
    let path = codex_cli_store_path()?;
    if !path.exists() {
        return Ok(CodexCliStore::default());
    }
    load_json_with_recovery(&path, "Basiliskos external Codex CLI accounts")
}

fn save_codex_cli_store(store: &CodexCliStore) -> Result<(), String> {
    let path = codex_cli_store_path()?;
    if let Some(parent) = path.parent() {
        secure_create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(store)
        .map_err(|error| format!("Could not serialize Codex CLI accounts: {error}"))?;
    durable_write(&path, &bytes)
}

/// Direct field re-nesting from CLIProxyAPI's normalized codex credential
/// shape into the real, native `~/.codex/auth.json` shape. Empirically
/// verified against the real `codex` CLI (`codex login status` /
/// `codex doctor`) during planning — this is a proven-correct translation,
/// not a guess.
fn translate_codex_cred(cliproxy: &Value) -> Result<Value, String> {
    let id_token = cliproxy
        .get("id_token")
        .and_then(Value::as_str)
        .ok_or("Codex credential is missing id_token")?;
    let access_token = cliproxy
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or("Codex credential is missing access_token")?;
    let refresh_token = cliproxy
        .get("refresh_token")
        .and_then(Value::as_str)
        .ok_or("Codex credential is missing refresh_token")?;
    let account_id = cliproxy
        .get("account_id")
        .and_then(Value::as_str)
        .ok_or("Codex credential is missing account_id")?;
    let last_refresh = cliproxy.get("last_refresh").and_then(Value::as_str);
    Ok(serde_json::json!({
        "OPENAI_API_KEY": Value::Null,
        "tokens": {
            "id_token": id_token,
            "access_token": access_token,
            "refresh_token": refresh_token,
            "account_id": account_id,
        },
        "last_refresh": last_refresh,
    }))
}

fn parse_timestamp(value: Option<&str>) -> Option<DateTime<Utc>> {
    value
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

/// Before overwriting the live file, check whether it belongs to one of our
/// already-known accounts and, if the live copy looks newer (the real CLI
/// silently refreshed it in the background), capture it back into the store
/// first. Prevents the classic account-switcher bug of reverting to a stale
/// token after switching away and back.
fn capture_live_codex_credential_if_known(store: &mut CodexCliStore, live_raw: &str) {
    let Ok(live) = serde_json::from_str::<Value>(live_raw) else {
        return;
    };
    let Some(live_account_id) = live
        .get("tokens")
        .and_then(|tokens| tokens.get("account_id"))
        .and_then(Value::as_str)
    else {
        return;
    };
    let Some(matched) = store
        .accounts
        .iter_mut()
        .find(|account| account.account_id.as_deref() == Some(live_account_id))
    else {
        return;
    };
    let live_refresh = parse_timestamp(live.get("last_refresh").and_then(Value::as_str));
    let stored_refresh = serde_json::from_str::<Value>(&matched.native_auth_json)
        .ok()
        .and_then(|value| parse_timestamp(value.get("last_refresh").and_then(Value::as_str)));
    let live_is_newer = match (live_refresh, stored_refresh) {
        (Some(live), Some(stored)) => live > stored,
        (Some(_), None) => true,
        _ => false,
    };
    if live_is_newer {
        matched.native_auth_json = live_raw.to_string();
    }
}

/// Core switch logic with an injectable auth-file path, so it's testable
/// against a temp file instead of the real `~/.codex/auth.json`. The Tauri
/// command below supplies the real path.
fn switch_codex_account_at(
    store: &mut CodexCliStore,
    account_id: &str,
    auth_path: &Path,
) -> Result<(), String> {
    if let Ok(live_raw) = fs::read_to_string(auth_path) {
        capture_live_codex_credential_if_known(store, &live_raw);
    }

    let account = store
        .accounts
        .iter_mut()
        .find(|account| account.id == account_id)
        .ok_or("Account not found")?;
    let native: Value = serde_json::from_str(&account.native_auth_json)
        .map_err(|error| format!("Stored Codex CLI credential is invalid: {error}"))?;
    let has_tokens = native.get("tokens").is_some();
    let has_api_key = native
        .get("OPENAI_API_KEY")
        .and_then(Value::as_str)
        .is_some();
    if !has_tokens && !has_api_key {
        return Err("Stored Codex CLI credential has neither tokens nor an API key".into());
    }

    let bytes = serde_json::to_vec_pretty(&native)
        .map_err(|error| format!("Could not serialize the Codex CLI credential: {error}"))?;
    let expected = fingerprint(&bytes);

    if let Some(parent) = auth_path.parent() {
        secure_create_dir_all(parent)?;
    }
    durable_write(auth_path, &bytes)?;

    let written = fs::read(auth_path)
        .map_err(|error| format!("Could not verify the written Codex credential: {error}"))?;
    if fingerprint(&written) != expected {
        return Err(
            "Switch verification failed; the live Codex auth file does not match what was written"
                .into(),
        );
    }

    account.last_used_at = Some(Utc::now());
    Ok(())
}

#[tauri::command]
pub fn list_codex_cli_accounts() -> Result<Vec<CodexCliAccountView>, String> {
    Ok(views(&load_codex_cli_store()?))
}

#[tauri::command]
pub fn switch_codex_cli_account(
    account_id: String,
    close_running: bool,
) -> Result<Vec<CodexCliAccountView>, String> {
    if codex_cli_running() {
        if !close_running {
            return Err("Close the running Codex CLI before switching".into());
        }
        stop_codex_cli()?;
    }
    let mut store = load_codex_cli_store()?;
    switch_codex_account_at(&mut store, &account_id, &real_codex_auth_path())?;
    save_codex_cli_store(&store)?;
    Ok(views(&store))
}

fn read_relay_credential(relay_file_name: &str) -> Result<Value, String> {
    let supplied = Path::new(relay_file_name);
    if supplied.file_name().and_then(|value| value.to_str()) != Some(relay_file_name)
        || supplied.components().count() != 1
        || supplied.extension().and_then(|value| value.to_str()) != Some("json")
    {
        return Err("Invalid relay account file name".into());
    }
    let path = relay_auth_dir()?.join(relay_file_name);
    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read the relay account: {error}"))?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("The relay account credential is invalid: {error}"))
}

/// Finds the store entry for a relay account (matching by account_id) or
/// translates and adds it, returning its id either way. Idempotent by
/// design so "serve this account" is always safe to call, whether or not
/// it's been registered before.
fn find_or_add_codex_cli_account_from_relay(
    store: &mut CodexCliStore,
    relay_file_name: &str,
) -> Result<String, String> {
    let cliproxy = read_relay_credential(relay_file_name)?;
    if cliproxy.get("type").and_then(Value::as_str) != Some("codex") {
        return Err("That relay account is not a Codex credential".into());
    }
    let native = translate_codex_cred(&cliproxy)?;
    let account_id = native["tokens"]["account_id"].as_str().map(str::to_string);

    if let Some(ref id) = account_id {
        if let Some(existing) = store
            .accounts
            .iter()
            .find(|account| account.account_id.as_deref() == Some(id.as_str()))
        {
            return Ok(existing.id.clone());
        }
    }

    let email = cliproxy
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_string);
    let label = email
        .clone()
        .unwrap_or_else(|| "Codex CLI account".to_string());
    let new_id = Uuid::new_v4().to_string();
    store.accounts.push(CodexCliAccount {
        id: new_id.clone(),
        label,
        email,
        account_id,
        native_auth_json: serde_json::to_string(&native)
            .map_err(|error| format!("Could not serialize the translated credential: {error}"))?,
        added_at: Utc::now(),
        last_used_at: None,
    });
    Ok(new_id)
}

/// One-click "serve real Codex CLI with this relay account": translates (or
/// finds the already-registered translation of) the given relay account,
/// then switches the real `~/.codex/auth.json` to it.
#[tauri::command]
pub fn serve_codex_cli_from_relay(
    relay_file_name: String,
    close_running: bool,
) -> Result<Vec<CodexCliAccountView>, String> {
    if codex_cli_running() {
        if !close_running {
            return Err("Close the running Codex CLI before switching".into());
        }
        stop_codex_cli()?;
    }
    let mut store = load_codex_cli_store()?;
    let account_id = find_or_add_codex_cli_account_from_relay(&mut store, &relay_file_name)?;
    switch_codex_account_at(&mut store, &account_id, &real_codex_auth_path())?;
    save_codex_cli_store(&store)?;
    Ok(views(&store))
}

/// Registers one of Basiliskos's already-added Codex relay accounts
/// (`~/.hydra-gateway/gateway/auth/codex-*.json`) for native Codex CLI
/// switching too. Does not trigger a new login — reuses whichever credential
/// the user already obtained via Basiliskos's existing Codex login flow.
#[tauri::command]
pub fn add_codex_cli_account_from_relay(
    relay_file_name: String,
    label: String,
) -> Result<Vec<CodexCliAccountView>, String> {
    let supplied = Path::new(&relay_file_name);
    if supplied.file_name().and_then(|value| value.to_str()) != Some(relay_file_name.as_str())
        || supplied.components().count() != 1
        || supplied.extension().and_then(|value| value.to_str()) != Some("json")
    {
        return Err("Invalid relay account file name".into());
    }
    let path = relay_auth_dir()?.join(&relay_file_name);
    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read the relay account: {error}"))?;
    let cliproxy: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("The relay account credential is invalid: {error}"))?;
    if cliproxy.get("type").and_then(Value::as_str) != Some("codex") {
        return Err("That relay account is not a Codex credential".into());
    }
    let native = translate_codex_cred(&cliproxy)?;
    let account_id = native["tokens"]["account_id"].as_str().map(str::to_string);
    let email = cliproxy
        .get("email")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut store = load_codex_cli_store()?;
    if let Some(ref id) = account_id {
        if store
            .accounts
            .iter()
            .any(|account| account.account_id.as_deref() == Some(id.as_str()))
        {
            return Err("This account is already registered for Codex CLI switching".into());
        }
    }
    store.accounts.push(CodexCliAccount {
        id: Uuid::new_v4().to_string(),
        label: normalized_label(&label)?,
        email,
        account_id,
        native_auth_json: serde_json::to_string(&native)
            .map_err(|error| format!("Could not serialize the translated credential: {error}"))?,
        added_at: Utc::now(),
        last_used_at: None,
    });
    save_codex_cli_store(&store)?;
    Ok(views(&store))
}

/// Zero-risk seeding: capture whatever's currently live in the real
/// `~/.codex/auth.json` right now as a first tracked account, without
/// requiring a new login or a relay account to already exist.
#[tauri::command]
pub fn import_current_codex_cli_account(label: String) -> Result<Vec<CodexCliAccountView>, String> {
    let raw = fs::read_to_string(real_codex_auth_path())
        .map_err(|_| "No live Codex CLI login was found to import".to_string())?;
    let native: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("The live Codex auth file is invalid: {error}"))?;
    let account_id = native
        .get("tokens")
        .and_then(|tokens| tokens.get("account_id"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut store = load_codex_cli_store()?;
    if let Some(ref id) = account_id {
        if store
            .accounts
            .iter()
            .any(|account| account.account_id.as_deref() == Some(id.as_str()))
        {
            return Err("This account is already registered".into());
        }
    }
    store.accounts.push(CodexCliAccount {
        id: Uuid::new_v4().to_string(),
        label: normalized_label(&label)?,
        email: None,
        account_id,
        native_auth_json: raw,
        added_at: Utc::now(),
        last_used_at: Some(Utc::now()),
    });
    save_codex_cli_store(&store)?;
    Ok(views(&store))
}

#[tauri::command]
pub fn rename_codex_cli_account(
    id: String,
    label: String,
) -> Result<Vec<CodexCliAccountView>, String> {
    let mut store = load_codex_cli_store()?;
    let account = store
        .accounts
        .iter_mut()
        .find(|account| account.id == id)
        .ok_or("Account not found")?;
    account.label = normalized_label(&label)?;
    save_codex_cli_store(&store)?;
    Ok(views(&store))
}

#[tauri::command]
pub fn remove_codex_cli_account(id: String) -> Result<Vec<CodexCliAccountView>, String> {
    let mut store = load_codex_cli_store()?;
    let before = store.accounts.len();
    store.accounts.retain(|account| account.id != id);
    if store.accounts.len() == before {
        return Err("Account not found".into());
    }
    save_codex_cli_store(&store)?;
    Ok(views(&store))
}

/// Reads the account_id currently live in the real `~/.codex/auth.json`,
/// used by the cross-service "currently active for" indicator. Returns
/// `None` on any read/parse failure — this is a best-effort display signal,
/// never a hard dependency.
pub fn live_codex_cli_account_id() -> Option<String> {
    let raw = fs::read_to_string(real_codex_auth_path()).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    value
        .get("tokens")
        .and_then(|tokens| tokens.get("account_id"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

pub fn find_email_by_account_id(account_id: &str) -> Option<String> {
    let store = load_codex_cli_store().ok()?;
    store
        .accounts
        .iter()
        .find(|account| account.account_id.as_deref() == Some(account_id))
        .and_then(|account| account.email.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_cliproxy_cred() -> Value {
        serde_json::json!({
            "access_token": "at-1",
            "account_id": "acct-1",
            "email": "person@example.com",
            "id_token": "id-1",
            "last_refresh": "2026-07-20T00:00:00Z",
            "refresh_token": "rt-1",
            "type": "codex"
        })
    }

    #[test]
    fn translate_codex_cred_maps_fields_and_rejects_missing_tokens() {
        let native = translate_codex_cred(&sample_cliproxy_cred()).unwrap();
        assert_eq!(native["OPENAI_API_KEY"], Value::Null);
        assert_eq!(native["tokens"]["access_token"], "at-1");
        assert_eq!(native["tokens"]["id_token"], "id-1");
        assert_eq!(native["tokens"]["refresh_token"], "rt-1");
        assert_eq!(native["tokens"]["account_id"], "acct-1");
        assert_eq!(native["last_refresh"], "2026-07-20T00:00:00Z");

        let mut missing_refresh = sample_cliproxy_cred();
        missing_refresh
            .as_object_mut()
            .unwrap()
            .remove("refresh_token");
        assert!(translate_codex_cred(&missing_refresh).is_err());
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "basiliskos-codex-cli-test-{name}-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn stored_account(id: &str, account_id: &str, native: &Value) -> CodexCliAccount {
        CodexCliAccount {
            id: id.into(),
            label: "Test account".into(),
            email: Some("person@example.com".into()),
            account_id: Some(account_id.into()),
            native_auth_json: serde_json::to_string(native).unwrap(),
            added_at: Utc::now(),
            last_used_at: None,
        }
    }

    #[test]
    fn switch_writes_the_stored_credential_and_verifies_the_write() {
        let dir = temp_dir("switch");
        let auth_path = dir.join("auth.json");
        let native = translate_codex_cred(&sample_cliproxy_cred()).unwrap();
        let mut store = CodexCliStore {
            accounts: vec![stored_account("acc-a", "acct-1", &native)],
        };
        switch_codex_account_at(&mut store, "acc-a", &auth_path).unwrap();
        let written: Value =
            serde_json::from_str(&fs::read_to_string(&auth_path).unwrap()).unwrap();
        assert_eq!(written["tokens"]["account_id"], "acct-1");
        assert!(store.accounts[0].last_used_at.is_some());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn switch_rejects_an_unknown_account_id_without_touching_the_file() {
        let dir = temp_dir("unknown");
        let auth_path = dir.join("auth.json");
        fs::write(&auth_path, "untouched").unwrap();
        let mut store = CodexCliStore { accounts: vec![] };
        let result = switch_codex_account_at(&mut store, "missing", &auth_path);
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&auth_path).unwrap(), "untouched");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn switch_captures_a_newer_live_credential_back_into_its_matching_account_first() {
        let dir = temp_dir("capture-back");
        let auth_path = dir.join("auth.json");

        // acc-a is live right now with a NEWER last_refresh than what's stored.
        let stale_native = translate_codex_cred(&sample_cliproxy_cred()).unwrap();
        let mut fresher_cred = sample_cliproxy_cred();
        fresher_cred["last_refresh"] = Value::String("2026-07-23T12:00:00Z".into());
        fresher_cred["access_token"] = Value::String("at-refreshed".into());
        let fresher_native = translate_codex_cred(&fresher_cred).unwrap();
        fs::write(
            &auth_path,
            serde_json::to_vec_pretty(&fresher_native).unwrap(),
        )
        .unwrap();

        let mut store = CodexCliStore {
            accounts: vec![
                stored_account("acc-a", "acct-1", &stale_native),
                stored_account(
                    "acc-b",
                    "acct-2",
                    &translate_codex_cred(&{
                        let mut other = sample_cliproxy_cred();
                        other["account_id"] = Value::String("acct-2".into());
                        other
                    })
                    .unwrap(),
                ),
            ],
        };

        // Switch to acc-b; acc-a (currently live) should be captured-back first.
        switch_codex_account_at(&mut store, "acc-b", &auth_path).unwrap();

        let acc_a_after: Value = serde_json::from_str(&store.accounts[0].native_auth_json).unwrap();
        assert_eq!(acc_a_after["tokens"]["access_token"], "at-refreshed");
        assert_eq!(acc_a_after["last_refresh"], "2026-07-23T12:00:00Z");

        let written: Value =
            serde_json::from_str(&fs::read_to_string(&auth_path).unwrap()).unwrap();
        assert_eq!(written["tokens"]["account_id"], "acct-2");
        fs::remove_dir_all(dir).ok();
    }
}
