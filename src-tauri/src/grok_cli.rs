//! Manages real Grok CLI accounts — a narrow, single-purpose credential-file
//! switcher, distinct from Basiliskos's own relay routing.
//!
//! Translation from Basiliskos's CLIProxyAPI-obtained xai credential to the
//! real `~/.grok/auth.json` shape looked infeasible at first glance (the
//! native file is a composite-keyed object carrying `principal_id`,
//! `team_id`, `oidc_client_id`, `first_name`, `create_time`, etc. that don't
//! exist as top-level fields in CLIProxyAPI's stored credential) — but nearly
//! all of that data is actually embedded in the access/id token JWTs
//! themselves (`client_id`, `sub`/`principal_id`, `principal_type`,
//! `team_id`, `iss`, `given_name`), because CLIProxyAPI's xai login uses the
//! same OAuth client (`grok-cli:access` scope) as the real `grok login`.
//! Verified empirically against the real `grok` CLI (via a `USERPROFILE`/
//! `HOME` override, never the live file): the only genuinely missing,
//! required field is `create_time`, which the real CLI needs present but
//! doesn't validate the value of — decorative fields
//! (`profile_image_asset_id`, `coding_data_retention_opt_out`) are safely
//! omittable. `translate_xai_cred` below is that proven translation.
//!
//! A verbatim-capture path (mirroring grok-hydra: shell out to the real
//! `grok login`, poll, import unchanged) is kept as a fallback for any
//! credential shape the translator can't handle.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use uuid::Uuid;

use crate::persistence::{durable_write, load_json_with_recovery, secure_create_dir_all};

fn grok_cli_root() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".hydra-gateway").join("external").join("grok"))
        .ok_or_else(|| "Unable to locate your home directory".to_string())
}

fn grok_cli_store_path() -> Result<PathBuf, String> {
    Ok(grok_cli_root()?.join("accounts.json"))
}

/// The real Grok CLI's own credential file. No env-var override exists for
/// this (confirmed: `grok --help` documents no `GROK_HOME`), unlike Codex's
/// `CODEX_HOME` — always `~/.grok/auth.json`.
fn real_grok_auth_path() -> PathBuf {
    dirs::home_dir()
        .map(|home| home.join(".grok").join("auth.json"))
        .unwrap_or_default()
}

fn relay_auth_dir() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".hydra-gateway").join("gateway").join("auth"))
        .ok_or_else(|| "Unable to locate your home directory".to_string())
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

/// Recursively searches a JSON value for the first of the given keys with a
/// non-empty string value — needed because the real Grok auth.json wraps
/// everything under an opaque composite key
/// (`"https://auth.x.ai::<uuid>"`), so identity fields have to be found by
/// walking the structure rather than assumed to be at a fixed path. Mirrors
/// grok-hydra's own `find_string_by_keys`.
fn find_string_by_keys(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(object) => {
            for key in keys {
                if let Some(Value::String(found)) = object.get(*key) {
                    if !found.trim().is_empty() {
                        return Some(found.clone());
                    }
                }
            }
            object
                .values()
                .find_map(|child| find_string_by_keys(child, keys))
        }
        Value::Array(items) => items
            .iter()
            .find_map(|child| find_string_by_keys(child, keys)),
        _ => None,
    }
}

/// Decodes a JWT's middle (payload) segment without verifying its signature
/// — Basiliskos never trusts this for authorization, only to read the same
/// public claims (client id, subject, team, name) the real Grok CLI itself
/// would have received when the token was minted. The signature was already
/// validated by xAI when CLIProxyAPI obtained the token.
fn decode_jwt_claims(token: &str) -> Option<Value> {
    use base64::Engine;
    let payload = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

/// Translates CLIProxyAPI's flat xai credential into the real, native
/// composite-keyed `~/.grok/auth.json` shape. See the module doc comment for
/// how this was verified against the real `grok` CLI.
fn translate_xai_cred(cliproxy: &Value) -> Result<Value, String> {
    let access_token = cliproxy
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or("Grok credential is missing access_token")?;
    let refresh_token = cliproxy
        .get("refresh_token")
        .and_then(Value::as_str)
        .ok_or("Grok credential is missing refresh_token")?;
    let expires_at = cliproxy
        .get("expired")
        .and_then(Value::as_str)
        .or_else(|| cliproxy.get("expires_at").and_then(Value::as_str))
        .ok_or("Grok credential is missing an expiry")?;

    let claims = decode_jwt_claims(access_token)
        .ok_or("Could not decode claims from the Grok access token")?;
    let id_claims = cliproxy
        .get("id_token")
        .and_then(Value::as_str)
        .and_then(decode_jwt_claims);

    let oidc_client_id = claims
        .get("client_id")
        .and_then(Value::as_str)
        .or_else(|| claims.get("aud").and_then(Value::as_str))
        .ok_or("Grok access token is missing a client_id/aud claim")?
        .to_string();
    let oidc_issuer = claims
        .get("iss")
        .and_then(Value::as_str)
        .unwrap_or("https://auth.x.ai")
        .to_string();
    let principal_id = claims
        .get("principal_id")
        .and_then(Value::as_str)
        .or_else(|| claims.get("sub").and_then(Value::as_str))
        .ok_or("Grok access token is missing a principal_id/sub claim")?
        .to_string();
    let principal_type = claims
        .get("principal_type")
        .and_then(Value::as_str)
        .unwrap_or("User")
        .to_string();
    let team_id = claims
        .get("team_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    let email = cliproxy
        .get("email")
        .and_then(Value::as_str)
        .or_else(|| {
            id_claims
                .as_ref()
                .and_then(|c| c.get("email"))
                .and_then(Value::as_str)
        })
        .ok_or("Grok credential is missing an email")?
        .to_string();
    let first_name = id_claims
        .as_ref()
        .and_then(|c| c.get("given_name"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();

    let mut entry = serde_json::Map::new();
    entry.insert("auth_mode".into(), Value::String("oidc".into()));
    // Required by the real CLI's strict deserializer but never validated for
    // a specific value — synthesized as "now" since CLIProxyAPI never
    // captured the real xAI account-creation timestamp.
    entry.insert("create_time".into(), Value::String(Utc::now().to_rfc3339()));
    entry.insert("email".into(), Value::String(email));
    entry.insert("expires_at".into(), Value::String(expires_at.to_string()));
    entry.insert("first_name".into(), Value::String(first_name));
    entry.insert("key".into(), Value::String(access_token.to_string()));
    entry.insert(
        "oidc_client_id".into(),
        Value::String(oidc_client_id.clone()),
    );
    entry.insert("oidc_issuer".into(), Value::String(oidc_issuer));
    entry.insert("principal_id".into(), Value::String(principal_id.clone()));
    entry.insert("principal_type".into(), Value::String(principal_type));
    entry.insert(
        "refresh_token".into(),
        Value::String(refresh_token.to_string()),
    );
    entry.insert("user_id".into(), Value::String(principal_id));
    if let Some(team_id) = team_id {
        entry.insert("team_id".into(), Value::String(team_id));
    }

    let mut wrapper = serde_json::Map::new();
    wrapper.insert(
        format!("https://auth.x.ai::{oidc_client_id}"),
        Value::Object(entry),
    );
    Ok(Value::Object(wrapper))
}

/// Stable identity for a Grok credential, independent of which access token
/// happens to be loaded (Grok rotates it periodically) — `user_id`/
/// `principal_id` first, falling back to `email`.
fn grok_identity(raw: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    find_string_by_keys(&value, &["user_id", "principal_id"])
        .or_else(|| find_string_by_keys(&value, &["email"]))
}

fn grok_email(raw: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    find_string_by_keys(&value, &["email"])
}

fn grok_expires_at(raw: &str) -> Option<DateTime<Utc>> {
    let value: Value = serde_json::from_str(raw).ok()?;
    find_string_by_keys(&value, &["expires_at"])
        .and_then(|raw| DateTime::parse_from_rfc3339(&raw).ok())
        .map(|dt| dt.with_timezone(&Utc))
}

#[cfg(target_os = "windows")]
fn grok_cli_running() -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", "IMAGENAME eq grok.exe", "/NH"])
        .output()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .to_lowercase()
                .contains("grok.exe")
        })
        .unwrap_or(false)
}

#[cfg(not(target_os = "windows"))]
fn grok_cli_running() -> bool {
    false
}

#[cfg(target_os = "windows")]
fn stop_grok_cli() -> Result<(), String> {
    std::process::Command::new("taskkill")
        .args(["/F", "/IM", "grok.exe", "/T"])
        .output()
        .map(|_| ())
        .map_err(|error| format!("Could not close the running Grok CLI: {error}"))
}

#[cfg(not(target_os = "windows"))]
fn stop_grok_cli() -> Result<(), String> {
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct GrokCliAccount {
    id: String,
    label: String,
    email: Option<String>,
    identity: Option<String>,
    /// Verbatim native `~/.grok/auth.json` contents, captured from a real
    /// `grok login` (or the currently-live file) — never translated.
    native_auth_json: String,
    added_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct GrokCliStore {
    #[serde(default)]
    accounts: Vec<GrokCliAccount>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GrokCliAccountView {
    pub id: String,
    pub label: String,
    pub email: Option<String>,
    pub added_at: DateTime<Utc>,
    pub last_used_at: Option<DateTime<Utc>>,
}

impl From<&GrokCliAccount> for GrokCliAccountView {
    fn from(account: &GrokCliAccount) -> Self {
        GrokCliAccountView {
            id: account.id.clone(),
            label: account.label.clone(),
            email: account.email.clone(),
            added_at: account.added_at,
            last_used_at: account.last_used_at,
        }
    }
}

fn views(store: &GrokCliStore) -> Vec<GrokCliAccountView> {
    store
        .accounts
        .iter()
        .map(GrokCliAccountView::from)
        .collect()
}

fn load_grok_cli_store() -> Result<GrokCliStore, String> {
    let path = grok_cli_store_path()?;
    if !path.exists() {
        return Ok(GrokCliStore::default());
    }
    load_json_with_recovery(&path, "Basiliskos external Grok CLI accounts")
}

fn save_grok_cli_store(store: &GrokCliStore) -> Result<(), String> {
    let path = grok_cli_store_path()?;
    if let Some(parent) = path.parent() {
        secure_create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(store)
        .map_err(|error| format!("Could not serialize Grok CLI accounts: {error}"))?;
    durable_write(&path, &bytes)
}

/// Same "capture the outgoing live credential back into its matching stored
/// account first" safety property as Codex CLI's switch, using `expires_at`
/// (which advances on every token refresh) as the freshness signal instead
/// of a `last_refresh` field, which the native Grok shape doesn't carry.
fn capture_live_grok_credential_if_known(store: &mut GrokCliStore, live_raw: &str) {
    let Some(live_identity) = grok_identity(live_raw) else {
        return;
    };
    let Some(matched) = store
        .accounts
        .iter_mut()
        .find(|account| account.identity.as_deref() == Some(live_identity.as_str()))
    else {
        return;
    };
    let live_expiry = grok_expires_at(live_raw);
    let stored_expiry = grok_expires_at(&matched.native_auth_json);
    let live_is_newer = match (live_expiry, stored_expiry) {
        (Some(live), Some(stored)) => live > stored,
        (Some(_), None) => true,
        _ => false,
    };
    if live_is_newer {
        matched.native_auth_json = live_raw.to_string();
    }
}

/// Core switch logic with an injectable auth-file path, so it's testable
/// against a temp file instead of the real `~/.grok/auth.json`.
fn switch_grok_account_at(
    store: &mut GrokCliStore,
    account_id: &str,
    auth_path: &Path,
) -> Result<(), String> {
    if let Ok(live_raw) = fs::read_to_string(auth_path) {
        capture_live_grok_credential_if_known(store, &live_raw);
    }

    let account = store
        .accounts
        .iter_mut()
        .find(|account| account.id == account_id)
        .ok_or("Account not found")?;
    serde_json::from_str::<Value>(&account.native_auth_json)
        .map_err(|error| format!("Stored Grok CLI credential is invalid: {error}"))?;

    let bytes = account.native_auth_json.as_bytes().to_vec();
    let expected = fingerprint(&bytes);

    if let Some(parent) = auth_path.parent() {
        secure_create_dir_all(parent)?;
    }
    durable_write(auth_path, &bytes)?;

    let written = fs::read(auth_path)
        .map_err(|error| format!("Could not verify the written Grok credential: {error}"))?;
    if fingerprint(&written) != expected {
        return Err(
            "Switch verification failed; the live Grok auth file does not match what was written"
                .into(),
        );
    }

    account.last_used_at = Some(Utc::now());
    Ok(())
}

#[tauri::command]
pub fn list_grok_cli_accounts() -> Result<Vec<GrokCliAccountView>, String> {
    Ok(views(&load_grok_cli_store()?))
}

#[tauri::command]
pub fn switch_grok_cli_account(
    account_id: String,
    close_running: bool,
) -> Result<Vec<GrokCliAccountView>, String> {
    if grok_cli_running() {
        if !close_running {
            return Err("Close the running Grok CLI before switching".into());
        }
        stop_grok_cli()?;
    }
    let mut store = load_grok_cli_store()?;
    switch_grok_account_at(&mut store, &account_id, &real_grok_auth_path())?;
    save_grok_cli_store(&store)?;
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

/// Finds the store entry for a relay account (matching by identity) or
/// translates and adds it, returning its id either way. Idempotent by
/// design so "serve this account" is always safe to call, whether or not
/// it's been registered before.
fn find_or_add_grok_cli_account_from_relay(
    store: &mut GrokCliStore,
    relay_file_name: &str,
) -> Result<String, String> {
    let cliproxy = read_relay_credential(relay_file_name)?;
    if cliproxy.get("type").and_then(Value::as_str) != Some("xai") {
        return Err("That relay account is not a Grok credential".into());
    }
    let native = translate_xai_cred(&cliproxy)?;
    let native_json = serde_json::to_string(&native)
        .map_err(|error| format!("Could not serialize the translated credential: {error}"))?;
    let identity = grok_identity(&native_json);

    if let Some(ref id) = identity {
        if let Some(existing) = store
            .accounts
            .iter_mut()
            .find(|account| account.identity.as_deref() == Some(id.as_str()))
        {
            // This relay credential may have been silently refreshed since
            // its last Serve. Update the vault copy before any switch writes
            // to the real Grok CLI auth file.
            existing.native_auth_json = native_json;
            existing.email = grok_email(&existing.native_auth_json);
            return Ok(existing.id.clone());
        }
    }

    let label = cliproxy
        .get("email")
        .and_then(Value::as_str)
        .unwrap_or("Grok CLI account")
        .to_string();
    let email = grok_email(&native_json);
    let new_id = Uuid::new_v4().to_string();
    store.accounts.push(GrokCliAccount {
        id: new_id.clone(),
        label,
        email,
        identity,
        native_auth_json: native_json,
        added_at: Utc::now(),
        last_used_at: None,
    });
    Ok(new_id)
}

/// Synchronizes an already served Grok CLI vault entry from the relay after a
/// silent OAuth refresh. It deliberately does not touch `~/.grok/auth.json`:
/// a running CLI owns its live session, and the next explicit Serve gets the
/// renewed snapshot under the same refresh coordination as the relay.
pub fn sync_grok_cli_account_from_relay(relay_file_name: &str) -> Result<(), String> {
    let cliproxy = read_relay_credential(relay_file_name)?;
    if cliproxy.get("type").and_then(Value::as_str) != Some("xai") {
        return Ok(());
    }
    let native = translate_xai_cred(&cliproxy)?;
    let native_json = serde_json::to_string(&native)
        .map_err(|error| format!("Could not serialize the translated credential: {error}"))?;
    let identity = grok_identity(&native_json);
    let mut store = load_grok_cli_store()?;
    let Some(identity) = identity else {
        return Ok(());
    };
    let Some(existing) = store
        .accounts
        .iter_mut()
        .find(|account| account.identity.as_deref() == Some(identity.as_str()))
    else {
        return Ok(());
    };
    existing.native_auth_json = native_json;
    existing.email = grok_email(&existing.native_auth_json);
    save_grok_cli_store(&store)
}

/// One-click "serve real Grok CLI with this relay account": translates (or
/// finds the already-registered translation of) the given relay account,
/// then switches the real `~/.grok/auth.json` to it. This is the primary,
/// preferred way to add a Grok CLI account now that translation is proven —
/// the verbatim-capture login flow below remains as a fallback.
#[tauri::command]
pub async fn serve_grok_cli_from_relay(
    relay_file_name: String,
    close_running: bool,
) -> Result<Vec<GrokCliAccountView>, String> {
    crate::gateway::refresh_xai_relay_credential_if_needed(&relay_file_name).await?;
    if grok_cli_running() {
        if !close_running {
            return Err("Close the running Grok CLI before switching".into());
        }
        stop_grok_cli()?;
    }
    let mut store = load_grok_cli_store()?;
    let account_id = find_or_add_grok_cli_account_from_relay(&mut store, &relay_file_name)?;
    switch_grok_account_at(&mut store, &account_id, &real_grok_auth_path())?;
    save_grok_cli_store(&store)?;
    Ok(views(&store))
}

/// Opens a terminal running the real `grok login` (the same approach
/// grok-hydra uses today) so the user can complete xAI's own browser OAuth.
/// Basiliskos never implements this handshake itself.
#[tauri::command]
pub fn launch_grok_cli_login() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args([
                "/C",
                "start",
                "",
                "powershell",
                "-NoExit",
                "-Command",
                "grok login",
            ])
            .spawn()
            .map_err(|error| format!("Could not launch grok login: {error}"))?;
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Err("Automatic terminal launch is currently available on Windows only".into())
    }
}

/// A SHA-256 fingerprint of the live `~/.grok/auth.json`, for the frontend to
/// poll after `launch_grok_cli_login` so it knows when the user has finished
/// logging in (the same polling contract grok-hydra's own UI already uses).
#[tauri::command]
pub fn grok_cli_login_fingerprint() -> Option<String> {
    fs::read(real_grok_auth_path())
        .ok()
        .map(|bytes| fingerprint(&bytes))
}

/// Captures whatever's currently live in the real `~/.grok/auth.json` right
/// now as a new tracked account — used both to seed the very first account
/// and as the final step once a fresh `grok login` completes.
#[tauri::command]
pub fn import_current_grok_cli_account(label: String) -> Result<Vec<GrokCliAccountView>, String> {
    let raw = fs::read_to_string(real_grok_auth_path())
        .map_err(|_| "No live Grok CLI login was found to import".to_string())?;
    let identity = grok_identity(&raw);
    let email = grok_email(&raw);

    let mut store = load_grok_cli_store()?;
    if let Some(ref id) = identity {
        if store
            .accounts
            .iter()
            .any(|account| account.identity.as_deref() == Some(id.as_str()))
        {
            return Err("This account is already registered".into());
        }
    }
    store.accounts.push(GrokCliAccount {
        id: Uuid::new_v4().to_string(),
        label: normalized_label(&label)?,
        email,
        identity,
        native_auth_json: raw,
        added_at: Utc::now(),
        last_used_at: Some(Utc::now()),
    });
    save_grok_cli_store(&store)?;
    Ok(views(&store))
}

#[tauri::command]
pub fn rename_grok_cli_account(
    id: String,
    label: String,
) -> Result<Vec<GrokCliAccountView>, String> {
    let mut store = load_grok_cli_store()?;
    let account = store
        .accounts
        .iter_mut()
        .find(|account| account.id == id)
        .ok_or("Account not found")?;
    account.label = normalized_label(&label)?;
    save_grok_cli_store(&store)?;
    Ok(views(&store))
}

#[tauri::command]
pub fn remove_grok_cli_account(id: String) -> Result<Vec<GrokCliAccountView>, String> {
    let mut store = load_grok_cli_store()?;
    let before = store.accounts.len();
    store.accounts.retain(|account| account.id != id);
    if store.accounts.len() == before {
        return Err("Account not found".into());
    }
    save_grok_cli_store(&store)?;
    Ok(views(&store))
}

/// Used by the cross-service "currently active for" indicator. Best-effort
/// only — `None` means "unknown," never a hard failure for callers.
pub fn live_grok_cli_email() -> Option<String> {
    let raw = fs::read_to_string(real_grok_auth_path()).ok()?;
    grok_email(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_jwt(claims: &Value) -> String {
        use base64::Engine;
        let header = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::json!({"alg": "ES256", "typ": "JWT"}).to_string());
        let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string());
        format!("{header}.{payload}.fake-signature")
    }

    fn sample_cliproxy_xai_cred() -> Value {
        let access_token = fake_jwt(&serde_json::json!({
            "iss": "https://auth.x.ai",
            "sub": "user-1",
            "aud": "client-abc",
            "client_id": "client-abc",
            "principal_type": "User",
            "principal_id": "user-1",
            "team_id": "team-1",
        }));
        let id_token = fake_jwt(&serde_json::json!({
            "email": "person@example.com",
            "given_name": "Ann",
        }));
        serde_json::json!({
            "access_token": access_token,
            "id_token": id_token,
            "refresh_token": "rt-1",
            "expired": "2026-07-24T00:00:00Z",
            "email": "person@example.com",
            "type": "xai",
        })
    }

    #[test]
    fn translate_xai_cred_recovers_composite_identity_from_jwt_claims() {
        let native = translate_xai_cred(&sample_cliproxy_xai_cred()).unwrap();
        let entry = &native["https://auth.x.ai::client-abc"];
        assert_eq!(entry["oidc_client_id"], "client-abc");
        assert_eq!(entry["oidc_issuer"], "https://auth.x.ai");
        assert_eq!(entry["principal_id"], "user-1");
        assert_eq!(entry["user_id"], "user-1");
        assert_eq!(entry["principal_type"], "User");
        assert_eq!(entry["team_id"], "team-1");
        assert_eq!(entry["email"], "person@example.com");
        assert_eq!(entry["first_name"], "Ann");
        assert_eq!(entry["refresh_token"], "rt-1");
        assert_eq!(entry["expires_at"], "2026-07-24T00:00:00Z");
        assert!(entry["create_time"].is_string(), "create_time must be present (the real CLI requires it, though it doesn't validate the value)");
        assert_eq!(entry["auth_mode"], "oidc");

        let round_tripped = native.to_string();
        assert_eq!(grok_identity(&round_tripped), Some("user-1".to_string()));
        assert_eq!(
            grok_email(&round_tripped),
            Some("person@example.com".to_string())
        );
    }

    #[test]
    fn translate_xai_cred_rejects_a_credential_missing_required_fields() {
        let mut missing_refresh = sample_cliproxy_xai_cred();
        missing_refresh
            .as_object_mut()
            .unwrap()
            .remove("refresh_token");
        assert!(translate_xai_cred(&missing_refresh).is_err());
    }

    fn sample_native_auth(user_id: &str, expires_at: &str) -> String {
        serde_json::json!({
            format!("https://auth.x.ai::{user_id}"): {
                "email": "person@example.com",
                "expires_at": expires_at,
                "user_id": user_id,
                "principal_id": user_id,
                "refresh_token": "rt-1",
                "key": "jwt-access-token",
            }
        })
        .to_string()
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "basiliskos-grok-cli-test-{name}-{}",
            Uuid::new_v4()
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn stored_account(id: &str, identity: &str, native_auth_json: &str) -> GrokCliAccount {
        GrokCliAccount {
            id: id.into(),
            label: "Test account".into(),
            email: Some("person@example.com".into()),
            identity: Some(identity.into()),
            native_auth_json: native_auth_json.into(),
            added_at: Utc::now(),
            last_used_at: None,
        }
    }

    #[test]
    fn grok_identity_finds_user_id_through_the_composite_key_wrapper() {
        let raw = sample_native_auth("user-1", "2026-07-24T00:00:00Z");
        assert_eq!(grok_identity(&raw), Some("user-1".to_string()));
        assert_eq!(grok_email(&raw), Some("person@example.com".to_string()));
    }

    #[test]
    fn switch_writes_the_stored_credential_verbatim_and_verifies_the_write() {
        let dir = temp_dir("switch");
        let auth_path = dir.join("auth.json");
        let native = sample_native_auth("user-1", "2026-07-24T00:00:00Z");
        let mut store = GrokCliStore {
            accounts: vec![stored_account("acc-a", "user-1", &native)],
        };
        switch_grok_account_at(&mut store, "acc-a", &auth_path).unwrap();
        assert_eq!(fs::read_to_string(&auth_path).unwrap(), native);
        assert!(store.accounts[0].last_used_at.is_some());
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn switch_rejects_an_unknown_account_id_without_touching_the_file() {
        let dir = temp_dir("unknown");
        let auth_path = dir.join("auth.json");
        fs::write(&auth_path, "untouched").unwrap();
        let mut store = GrokCliStore { accounts: vec![] };
        let result = switch_grok_account_at(&mut store, "missing", &auth_path);
        assert!(result.is_err());
        assert_eq!(fs::read_to_string(&auth_path).unwrap(), "untouched");
        fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn switch_captures_a_newer_live_credential_back_into_its_matching_account_first() {
        let dir = temp_dir("capture-back");
        let auth_path = dir.join("auth.json");

        let stale = sample_native_auth("user-1", "2026-07-24T00:00:00Z");
        let fresher = sample_native_auth("user-1", "2026-07-24T06:00:00Z");
        fs::write(&auth_path, &fresher).unwrap();

        let other = sample_native_auth("user-2", "2026-07-24T00:00:00Z");
        let mut store = GrokCliStore {
            accounts: vec![
                stored_account("acc-a", "user-1", &stale),
                stored_account("acc-b", "user-2", &other),
            ],
        };

        switch_grok_account_at(&mut store, "acc-b", &auth_path).unwrap();

        assert_eq!(store.accounts[0].native_auth_json, fresher);
        assert_eq!(fs::read_to_string(&auth_path).unwrap(), other);
        fs::remove_dir_all(dir).ok();
    }
}
