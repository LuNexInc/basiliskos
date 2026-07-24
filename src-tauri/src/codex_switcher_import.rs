//! One-time import of already-authenticated Codex accounts from the
//! third-party `codex-switcher` app (github.com/Lampese/codex-switcher,
//! `~/.codex-switcher/accounts.json`) into Basiliskos's own relay account
//! list — without a fresh OAuth login.
//!
//! Read-only against `codex-switcher`'s store: this never writes to, deletes,
//! or otherwise modifies `~/.codex-switcher/accounts.json`. Imported accounts
//! are always written `disabled: true` — never auto-activated — so this can
//! never change which credential Basiliskos's relay is currently using.

use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::Value;
use std::{fs, path::PathBuf};

use crate::persistence::{durable_write, secure_create_dir_all};

fn codex_switcher_accounts_path() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".codex-switcher").join("accounts.json"))
        .unwrap_or_default()
}

fn relay_auth_dir() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".hydra-gateway").join("gateway").join("auth"))
        .ok_or_else(|| "Unable to locate your home directory".to_string())
}

#[derive(Debug, Deserialize)]
struct CodexSwitcherStore {
    #[serde(default)]
    accounts: Vec<CodexSwitcherAccount>,
}

#[derive(Debug, Deserialize)]
struct CodexSwitcherAccount {
    email: Option<String>,
    auth_data: Option<Value>,
}

fn decode_jwt_claims(token: &str) -> Option<Value> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn jwt_expiry(token: &str) -> Option<DateTime<Utc>> {
    let claims = decode_jwt_claims(token)?;
    let exp = claims.get("exp").and_then(Value::as_i64)?;
    DateTime::from_timestamp(exp, 0)
}

/// Builds Basiliskos's own flat, CLIProxyAPI-shaped credential JSON from a
/// codex-switcher `auth_data` block (`{"type":"chat_g_p_t", id_token,
/// access_token, refresh_token, account_id}`). Only ChatGPT-mode accounts are
/// importable this way — API-key-mode accounts have no OAuth material to
/// carry over and are skipped.
fn build_relay_credential(email: &str, auth_data: &Value) -> Result<Value, String> {
    if auth_data.get("type").and_then(Value::as_str) != Some("chat_g_p_t") {
        return Err("Only ChatGPT-mode codex-switcher accounts can be imported".into());
    }
    let id_token = auth_data
        .get("id_token")
        .and_then(Value::as_str)
        .ok_or("codex-switcher account is missing id_token")?;
    let access_token = auth_data
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or("codex-switcher account is missing access_token")?;
    let refresh_token = auth_data
        .get("refresh_token")
        .and_then(Value::as_str)
        .ok_or("codex-switcher account is missing refresh_token")?;
    let account_id = auth_data
        .get("account_id")
        .and_then(Value::as_str)
        .ok_or("codex-switcher account is missing account_id")?;
    let expired = jwt_expiry(access_token)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| Utc::now().to_rfc3339());

    Ok(serde_json::json!({
        "type": "codex",
        "access_token": access_token,
        "id_token": id_token,
        "refresh_token": refresh_token,
        "account_id": account_id,
        "email": email,
        "disabled": true,
        "expired": expired,
        "last_refresh": Utc::now().to_rfc3339(),
    }))
}

fn sanitize_for_filename(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '@' || c == '.' || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

fn existing_account_ids(auth_dir: &std::path::Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(auth_dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                return None;
            }
            let raw = fs::read_to_string(&path).ok()?;
            let value: Value = serde_json::from_str(&raw).ok()?;
            value
                .get("account_id")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// Reads codex-switcher's own account store (never modified) and imports any
/// ChatGPT-mode account not already present in Basiliskos's relay list
/// (matched by `account_id`), skipping duplicates. Every imported account is
/// written `disabled: true` — the user still has to explicitly pick which
/// one becomes active, exactly like a normal login would require.
#[tauri::command]
pub fn import_accounts_from_codex_switcher() -> Result<Vec<String>, String> {
    let store_path = codex_switcher_accounts_path();
    let raw = fs::read_to_string(&store_path).map_err(|error| {
        format!(
            "Could not read codex-switcher's account store ({}): {error}",
            store_path.display()
        )
    })?;
    let store: CodexSwitcherStore = serde_json::from_str(&raw)
        .map_err(|error| format!("codex-switcher's account store is invalid: {error}"))?;

    let auth_dir = relay_auth_dir()?;
    secure_create_dir_all(&auth_dir)?;
    let already_present = existing_account_ids(&auth_dir);

    let mut imported = Vec::new();
    for account in &store.accounts {
        let Some(email) = account.email.as_deref() else {
            continue;
        };
        let Some(auth_data) = account.auth_data.as_ref() else {
            continue;
        };
        let Ok(credential) = build_relay_credential(email, auth_data) else {
            continue;
        };
        let account_id = credential["account_id"].as_str().unwrap_or_default();
        if already_present.iter().any(|id| id == account_id) {
            continue;
        }
        let file_name = format!("codex-{}.json", sanitize_for_filename(email));
        let path = auth_dir.join(&file_name);
        if path.exists() {
            continue;
        }
        let bytes = serde_json::to_vec_pretty(&credential)
            .map_err(|error| format!("Could not serialize the imported credential: {error}"))?;
        durable_write(&path, &bytes)?;
        imported.push(email.to_string());
    }
    Ok(imported)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_jwt(exp: i64) -> String {
        use base64::Engine;
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::json!({"alg": "RS256"}).to_string());
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::json!({"exp": exp}).to_string());
        format!("{header}.{payload}.sig")
    }

    #[test]
    fn build_relay_credential_maps_chat_gpt_auth_data() {
        let auth_data = serde_json::json!({
            "type": "chat_g_p_t",
            "id_token": "id-1",
            "access_token": fake_jwt(1900000000),
            "refresh_token": "rt-1",
            "account_id": "acct-1",
        });
        let credential = build_relay_credential("person@example.com", &auth_data).unwrap();
        assert_eq!(credential["type"], "codex");
        assert_eq!(credential["account_id"], "acct-1");
        assert_eq!(credential["email"], "person@example.com");
        assert_eq!(credential["disabled"], true);
        assert_eq!(credential["refresh_token"], "rt-1");
        assert!(credential["expired"].as_str().unwrap().starts_with("2030"));
    }

    #[test]
    fn build_relay_credential_rejects_api_key_mode() {
        let auth_data = serde_json::json!({"type": "api_key", "key": "sk-..."});
        assert!(build_relay_credential("person@example.com", &auth_data).is_err());
    }

    #[test]
    fn sanitize_for_filename_keeps_email_shape_safe() {
        assert_eq!(sanitize_for_filename("a.b@c.com"), "a.b@c.com");
        assert_eq!(
            sanitize_for_filename("weird name/path\\x"),
            "weird_name_path_x"
        );
    }
}
