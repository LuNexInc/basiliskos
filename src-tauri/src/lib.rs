use chrono::{DateTime, Duration, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::TrayIconBuilder,
    Manager, WindowEvent,
};
use uuid::Uuid;

mod gateway;

const BILLING_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing";
const BILLING_CREDITS_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const DEFAULT_OIDC_ISSUER: &str = "https://auth.x.ai";
// Grok CLI access tokens live ~6h. Renew when this little time is left so a
// stored profile is never handed out with an expired (or about-to-expire) token.
const REFRESH_SKEW_SECONDS: i64 = 120;
// Fallback token lifetime if the token endpoint omits expires_in.
const DEFAULT_TOKEN_LIFETIME_SECONDS: i64 = 21_600;
const RELOGIN_REQUIRED_ERROR: &str = "Session expired — run grok login to sign in again";
// Refresh-token grants rotate credentials. Keep every refresh path behind one
// async lock so startup, timer, usage, and manual requests cannot reuse the
// same one-time refresh token concurrently.
static REFRESH_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Profile {
    id: String,
    name: String,
    email: Option<String>,
    raw_auth_json: String,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
    // Set when a silent token refresh fails (e.g. the refresh token expired);
    // cleared on a successful refresh. Defaulted so pre-existing stores load.
    #[serde(default)]
    refresh_error: Option<String>,
    // Timestamp of the last successful silent refresh, for display/debugging.
    #[serde(default)]
    last_refresh_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ProfileStore {
    profiles: Vec<Profile>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ProfileView {
    id: String,
    name: String,
    email: Option<String>,
    is_active: bool,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
    // When the stored access token expires (RFC3339). None for legacy snapshots
    // saved before this field was tracked, or auth files without an expiry.
    expires_at: Option<String>,
    // True when the profile carries a usable OIDC refresh token. A token that
    // the issuer has permanently revoked remains in re-login-required state.
    can_refresh: bool,
    // Last silent-refresh error for this profile, if any (e.g. the refresh
    // token itself expired and a real re-login is now required).
    refresh_error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct LoginStatus {
    exists: bool,
    fingerprint: Option<String>,
    email: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageView {
    profile_id: String,
    used: Option<f64>,
    limit: Option<f64>,
    percent: Option<f64>,
    label: String,
    period_label: Option<String>,
    resets_at: Option<String>,
    source: String,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GrokInstance {
    pid: u32,
}

fn config_dir() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Unable to locate the home directory")?;
    // Keep the fork isolated from Hydra's ~/.hydra profile store. Provider
    // migration/import will be designed explicitly in a later milestone.
    let dir = home.join(".hydra-gateway");
    Ok(dir)
}

fn store_path() -> Result<PathBuf, String> {
    Ok(config_dir()?.join("profiles.json"))
}

fn live_auth_path() -> Result<PathBuf, String> {
    let home = dirs::home_dir().ok_or("Unable to locate the home directory")?;
    Ok(home.join(".grok").join("auth.json"))
}

fn load_store() -> Result<ProfileStore, String> {
    let path = store_path()?;
    if !path.exists() {
        return Ok(ProfileStore::default());
    }
    let content = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    serde_json::from_str(&content).map_err(|error| format!("Profile store is invalid: {error}"))
}

fn atomic_write(path: &Path, content: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }
    let temp = path.with_extension("tmp");
    fs::write(&temp, content)
        .map_err(|error| format!("Could not write {}: {error}", temp.display()))?;
    if path.exists() {
        let backup = path.with_extension("backup");
        let _ = fs::copy(path, backup);
        fs::remove_file(path)
            .map_err(|error| format!("Could not replace {}: {error}", path.display()))?;
    }
    fs::rename(&temp, path)
        .map_err(|error| format!("Could not finalize {}: {error}", path.display()))
}

fn save_store(store: &ProfileStore) -> Result<(), String> {
    let payload = serde_json::to_vec_pretty(store)
        .map_err(|error| format!("Could not serialize profiles: {error}"))?;
    atomic_write(&store_path()?, &payload)
}

fn canonicalize(value: &Value) -> Value {
    match value {
        Value::Object(object) => {
            let mut keys: Vec<_> = object.keys().collect();
            keys.sort();
            let mut sorted = Map::new();
            for key in keys {
                sorted.insert(key.clone(), canonicalize(&object[key]));
            }
            Value::Object(sorted)
        }
        Value::Array(items) => Value::Array(items.iter().map(canonicalize).collect()),
        other => other.clone(),
    }
}

fn normalized_auth(raw: &str) -> Result<String, String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|error| format!("Invalid auth JSON: {error}"))?;
    serde_json::to_string(&canonicalize(&value))
        .map_err(|error| format!("Could not normalize auth JSON: {error}"))
}

fn fingerprint(raw: &str) -> Result<String, String> {
    let normalized = normalized_auth(raw)?;
    Ok(hex::encode(Sha256::digest(normalized.as_bytes())))
}

fn find_string_by_keys(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(object) => {
            for key in keys {
                if let Some(Value::String(value)) = object.get(*key) {
                    if !value.trim().is_empty() {
                        return Some(value.clone());
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

fn auth_email(raw: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    find_string_by_keys(&value, &["email", "preferred_username"])
}

fn access_token(raw: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    find_string_by_keys(&value, &["access_token", "key", "token"]).filter(|token| token.len() > 40)
}

/// Stable identity for a credential, independent of which access token is
/// currently loaded. Grok rotates the access token roughly every 6 hours, so
/// fingerprinting the whole file is not a reliable way to tell whether a stored
/// profile is the one that is live — the user_id (or email) is.
fn auth_identity(raw: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    find_string_by_keys(&value, &["user_id"]).or_else(|| find_string_by_keys(&value, &["email"]))
}

/// Compares two auth snapshots by stable account identity, falling back to a
/// normalized full-file fingerprint for legacy credentials without identity.
fn auth_snapshots_match(left: &str, right: &str) -> bool {
    match (auth_identity(left), auth_identity(right)) {
        (Some(left_identity), Some(right_identity)) => left_identity == right_identity,
        _ => matches!(
            (fingerprint(left).ok(), fingerprint(right).ok()),
            (Some(left_fingerprint), Some(right_fingerprint))
                if left_fingerprint == right_fingerprint
        ),
    }
}

fn refresh_blocked_by_running_session(
    raw: &str,
    live_raw: Option<&str>,
    grok_running: bool,
) -> bool {
    grok_running && live_raw.is_some_and(|live| auth_snapshots_match(raw, live))
}

/// The RFC3339 expiry timestamp stored alongside the access token, if present.
fn auth_expires_at(raw: &str) -> Option<String> {
    let value: Value = serde_json::from_str(raw).ok()?;
    find_string_by_keys(&value, &["expires_at"])
}

/// True when the token is expired or within the renewal window. A missing or
/// unparseable expiry is treated as stale so legacy snapshots get renewed.
fn token_is_stale(raw: &str) -> bool {
    match auth_expires_at(raw).and_then(|value| DateTime::parse_from_rfc3339(&value).ok()) {
        Some(expiry) => {
            Utc::now() + Duration::seconds(REFRESH_SKEW_SECONDS) >= expiry.with_timezone(&Utc)
        }
        None => true,
    }
}

/// Extracts the material needed to renew a token: (refresh_token, client_id, issuer).
/// The issuer is intentionally pinned: imported JSON must never choose where
/// Hydra sends a refresh token.
fn refresh_material(raw: &str) -> Result<(String, String, String), String> {
    let value: Value =
        serde_json::from_str(raw).map_err(|error| format!("Invalid auth JSON: {error}"))?;
    let refresh = find_string_by_keys(&value, &["refresh_token"])
        .ok_or("This profile has no refresh token; run grok login to re-import it")?;
    let client_id = find_string_by_keys(&value, &["oidc_client_id", "client_id"])
        .ok_or("This profile has no OIDC client id; run grok login to re-import it")?;
    let issuer = find_string_by_keys(&value, &["oidc_issuer", "issuer"])
        .unwrap_or_else(|| DEFAULT_OIDC_ISSUER.to_string());
    if issuer.trim_end_matches('/') != DEFAULT_OIDC_ISSUER {
        return Err("Unsupported login issuer; run the official grok login and re-import".into());
    }
    Ok((refresh, client_id, DEFAULT_OIDC_ISSUER.to_string()))
}

fn can_refresh(raw: &str) -> bool {
    refresh_material(raw).is_ok()
}

fn refresh_available(raw: &str, refresh_error: Option<&str>) -> bool {
    can_refresh(raw) && refresh_error != Some(RELOGIN_REQUIRED_ERROR)
}

/// Rewrites the credential object(s) in the auth JSON with a renewed access
/// token, rotated refresh token, and new expiry, preserving everything else.
fn apply_refresh_to_auth(
    raw: &str,
    expected_refresh: &str,
    access: &str,
    refresh: &str,
    expires_at: &str,
) -> Result<String, String> {
    fn walk(
        value: &mut Value,
        expected_refresh: &str,
        access: &str,
        refresh: &str,
        expires_at: &str,
    ) -> bool {
        match value {
            Value::Object(map) => {
                let is_target = map.get("refresh_token").and_then(Value::as_str)
                    == Some(expected_refresh)
                    && map.contains_key("key");
                if is_target {
                    map.insert("key".into(), Value::String(access.to_string()));
                    map.insert("refresh_token".into(), Value::String(refresh.to_string()));
                    map.insert("expires_at".into(), Value::String(expires_at.to_string()));
                    return true;
                }
                for child in map.values_mut() {
                    if walk(child, expected_refresh, access, refresh, expires_at) {
                        return true;
                    }
                }
            }
            Value::Array(items) => {
                for child in items.iter_mut() {
                    if walk(child, expected_refresh, access, refresh, expires_at) {
                        return true;
                    }
                }
            }
            _ => {}
        }
        false
    }

    let mut value: Value =
        serde_json::from_str(raw).map_err(|error| format!("Invalid auth JSON: {error}"))?;
    if !walk(&mut value, expected_refresh, access, refresh, expires_at) {
        return Err("Could not locate the credential to update in the auth file".into());
    }
    serde_json::to_string_pretty(&value)
        .map_err(|error| format!("Could not serialize refreshed auth JSON: {error}"))
}

/// Calls the OIDC token endpoint with the refresh grant and returns a new raw
/// auth JSON string with the renewed token applied. Never logs token values.
async fn perform_refresh(raw: &str) -> Result<String, String> {
    let (refresh, client_id, issuer) = refresh_material(raw)?;
    let token_url = format!("{}/oauth2/token", issuer.trim_end_matches('/'));
    let params = [
        ("grant_type", "refresh_token"),
        ("refresh_token", refresh.as_str()),
        ("client_id", client_id.as_str()),
    ];
    let response = reqwest::Client::new()
        .post(&token_url)
        .form(&params)
        .send()
        .await
        .map_err(|error| format!("Token refresh request failed: {error}"))?;
    let status = response.status();
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("Token refresh response was invalid: {error}"))?;
    if !status.is_success() {
        let code = body
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("unknown_error");
        // invalid_grant means the refresh token itself is dead — a real
        // grok login is the only way back.
        return Err(if code == "invalid_grant" {
            RELOGIN_REQUIRED_ERROR.to_string()
        } else {
            format!("Token endpoint rejected the refresh ({code})")
        });
    }
    let access = body
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or("Token refresh returned no access_token")?;
    let new_refresh = body
        .get("refresh_token")
        .and_then(Value::as_str)
        .unwrap_or(refresh.as_str());
    let expires_in = body
        .get("expires_in")
        .and_then(Value::as_i64)
        .unwrap_or(DEFAULT_TOKEN_LIFETIME_SECONDS);
    let expires_at =
        (Utc::now() + Duration::seconds(expires_in)).to_rfc3339_opts(SecondsFormat::Micros, true);
    apply_refresh_to_auth(raw, &refresh, access, new_refresh, &expires_at)
}

fn read_live_auth() -> Result<Option<String>, String> {
    let path = live_auth_path()?;
    if !path.exists() {
        return Ok(None);
    }
    fs::read_to_string(&path)
        .map(Some)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))
}

fn profile_view(
    profile: &Profile,
    live_fingerprint: Option<&str>,
    live_identity: Option<&str>,
) -> ProfileView {
    // Prefer stable identity (user_id/email) to decide which profile is live,
    // falling back to a full-file fingerprint only when identity is unknown.
    let is_active = match (auth_identity(&profile.raw_auth_json), live_identity) {
        (Some(profile_identity), Some(live)) => profile_identity == live,
        _ => fingerprint(&profile.raw_auth_json).ok().as_deref() == live_fingerprint,
    };
    let refresh_error = profile.refresh_error.clone();
    ProfileView {
        id: profile.id.clone(),
        name: profile.name.clone(),
        email: profile.email.clone(),
        is_active,
        created_at: profile.created_at,
        last_used_at: profile.last_used_at,
        expires_at: auth_expires_at(&profile.raw_auth_json),
        can_refresh: refresh_available(&profile.raw_auth_json, refresh_error.as_deref()),
        refresh_error,
    }
}

fn upsert_auth(raw: String, requested_name: Option<String>) -> Result<ProfileView, String> {
    normalized_auth(&raw)?;
    let email = auth_email(&raw);
    let raw_fingerprint = fingerprint(&raw)?;
    let mut store = load_store()?;
    let existing = store.profiles.iter_mut().find(|profile| {
        fingerprint(&profile.raw_auth_json).ok().as_deref() == Some(raw_fingerprint.as_str())
            || (email.is_some() && profile.email == email)
    });

    let id = if let Some(profile) = existing {
        profile.raw_auth_json = raw;
        profile.email = email.clone();
        profile.last_used_at = Some(Utc::now());
        // A fresh import supplies a working token, so any stale refresh error
        // (from a previously expired snapshot) no longer applies.
        profile.refresh_error = None;
        if let Some(name) = requested_name.filter(|name| !name.trim().is_empty()) {
            profile.name = name.trim().to_string();
        }
        profile.id.clone()
    } else {
        let id = Uuid::new_v4().to_string();
        let name = requested_name
            .filter(|name| !name.trim().is_empty())
            .or_else(|| email.clone())
            .unwrap_or_else(|| format!("Profile {}", store.profiles.len() + 1));
        store.profiles.push(Profile {
            id: id.clone(),
            name,
            email: email.clone(),
            raw_auth_json: raw,
            created_at: Utc::now(),
            last_used_at: Some(Utc::now()),
            refresh_error: None,
            last_refresh_at: None,
        });
        id
    };
    save_store(&store)?;
    let profile = store
        .profiles
        .iter()
        .find(|profile| profile.id == id)
        .ok_or("Imported profile was not found")?;
    let identity = auth_identity(&profile.raw_auth_json);
    Ok(profile_view(
        profile,
        Some(&raw_fingerprint),
        identity.as_deref(),
    ))
}

fn list_profiles_inner() -> Result<Vec<ProfileView>, String> {
    let live_raw = read_live_auth()?;
    let live_fingerprint = live_raw.as_deref().and_then(|raw| fingerprint(raw).ok());
    let live_identity = live_raw.as_deref().and_then(auth_identity);

    let mut store = load_store()?;
    // Grok CLI renews the live token on its own schedule. When it does, the
    // matching stored profile would otherwise fall out of sync (wrong "active"
    // state, stale token for usage). Adopt the live credential for whichever
    // profile shares its identity so the store tracks Grok automatically.
    if let (Some(live_raw), Some(live_identity)) = (live_raw.as_deref(), live_identity.as_deref()) {
        let mut changed = false;
        for profile in store.profiles.iter_mut() {
            if auth_identity(&profile.raw_auth_json).as_deref() == Some(live_identity)
                && profile.raw_auth_json != live_raw
            {
                profile.raw_auth_json = live_raw.to_string();
                profile.email = auth_email(live_raw).or_else(|| profile.email.clone());
                profile.refresh_error = None;
                changed = true;
            }
        }
        if changed {
            save_store(&store)?;
        }
    }

    Ok(store
        .profiles
        .iter()
        .map(|profile| {
            profile_view(
                profile,
                live_fingerprint.as_deref(),
                live_identity.as_deref(),
            )
        })
        .collect())
}

#[tauri::command]
async fn list_profiles() -> Result<Vec<ProfileView>, String> {
    // Live-auth adoption writes the profile store. Serialize it with refresh
    // grants so an older live snapshot cannot overwrite a freshly rotated one.
    let _refresh_guard = REFRESH_LOCK.lock().await;
    list_profiles_inner()
}

#[tauri::command]
fn login_status() -> Result<LoginStatus, String> {
    let raw = read_live_auth()?;
    Ok(LoginStatus {
        exists: raw.is_some(),
        fingerprint: raw.as_deref().and_then(|value| fingerprint(value).ok()),
        email: raw.as_deref().and_then(auth_email),
    })
}

#[tauri::command]
fn import_current_profile(name: Option<String>) -> Result<ProfileView, String> {
    let raw = read_live_auth()?.ok_or("Run grok login first; no auth.json was found")?;
    upsert_auth(raw, name)
}

#[tauri::command]
fn import_profile_file(path: String, name: Option<String>) -> Result<ProfileView, String> {
    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read selected auth file: {error}"))?;
    upsert_auth(raw, name)
}

#[cfg(target_os = "windows")]
fn parse_tasklist_csv_line(line: &str) -> Vec<String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = line.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '"' if in_quotes && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => in_quotes = !in_quotes,
            ',' if !in_quotes => {
                fields.push(field.clone());
                field.clear();
            }
            _ => field.push(ch),
        }
    }
    fields.push(field);
    fields
}

fn grok_instances_impl() -> Vec<GrokInstance> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        Command::new("tasklist")
            .args(["/FI", "IMAGENAME eq grok.exe", "/FO", "CSV", "/NH"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .filter_map(|line| {
                        let fields = parse_tasklist_csv_line(line);
                        if fields
                            .first()
                            .map(|name| name.eq_ignore_ascii_case("grok.exe"))
                            == Some(true)
                        {
                            fields
                                .get(1)?
                                .parse::<u32>()
                                .ok()
                                .map(|pid| GrokInstance { pid })
                        } else {
                            None
                        }
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
    #[cfg(not(target_os = "windows"))]
    {
        Vec::new()
    }
}

fn grok_cli_running() -> bool {
    !grok_instances_impl().is_empty()
}

#[tauri::command]
fn grok_instances() -> Vec<GrokInstance> {
    grok_instances_impl()
}

#[tauri::command]
fn close_grok_instances() -> Result<(), String> {
    stop_grok_cli()
}

fn stop_grok_cli() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let output = Command::new("taskkill")
            .args(["/F", "/IM", "grok.exe", "/T"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|error| format!("Could not close Grok CLI: {error}"))?;
        if !output.status.success() && grok_cli_running() {
            return Err(format!(
                "Could not close Grok CLI: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
    }
    Ok(())
}

#[tauri::command]
fn switch_profile(profile_id: String, close_running: bool) -> Result<(), String> {
    if grok_cli_running() {
        if !close_running {
            return Err("Close the running Grok session before switching".into());
        }
        stop_grok_cli()?;
    }
    let mut store = load_store()?;
    let profile = store
        .profiles
        .iter_mut()
        .find(|profile| profile.id == profile_id)
        .ok_or("Profile not found")?;
    normalized_auth(&profile.raw_auth_json)?;
    let expected = fingerprint(&profile.raw_auth_json)?;
    atomic_write(&live_auth_path()?, profile.raw_auth_json.as_bytes())?;
    let written = read_live_auth()?.ok_or("The live auth file disappeared after switching")?;
    if fingerprint(&written)? != expected {
        return Err("Switch verification failed; the live auth file does not match".into());
    }
    profile.last_used_at = Some(Utc::now());
    save_store(&store)?;
    Ok(())
}

#[tauri::command]
fn rename_profile(profile_id: String, name: String) -> Result<(), String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("Name cannot be empty".into());
    }
    let mut store = load_store()?;
    let profile = store
        .profiles
        .iter_mut()
        .find(|profile| profile.id == profile_id)
        .ok_or("Profile not found")?;
    profile.name = name.to_string();
    save_store(&store)
}

#[tauri::command]
fn delete_profile(profile_id: String) -> Result<(), String> {
    let mut store = load_store()?;
    let original_len = store.profiles.len();
    store.profiles.retain(|profile| profile.id != profile_id);
    if store.profiles.len() == original_len {
        return Err("Profile not found".into());
    }
    save_store(&store)
}

#[tauri::command]
fn launch_grok_login() -> Result<(), String> {
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
    }
    #[cfg(not(target_os = "windows"))]
    {
        return Err("Automatic terminal launch is currently available on Windows only".into());
    }
    Ok(())
}

#[tauri::command]
fn launch_grok() -> Result<(), String> {
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
                "grok",
            ])
            .spawn()
            .map_err(|error| format!("Could not launch grok: {error}"))?;
    }
    #[cfg(not(target_os = "windows"))]
    {
        return Err("Automatic terminal launch is currently available on Windows only".into());
    }
    Ok(())
}

fn find_number(value: &Value, keys: &[&str]) -> Option<f64> {
    match value {
        Value::Object(object) => {
            for key in keys {
                if let Some(candidate) = object.get(*key) {
                    if let Some(value) = candidate.as_f64() {
                        return Some(value);
                    }
                    if let Some(value) = candidate.get("val").and_then(Value::as_f64) {
                        return Some(value);
                    }
                }
            }
            object.values().find_map(|value| find_number(value, keys))
        }
        Value::Array(items) => items.iter().find_map(|value| find_number(value, keys)),
        _ => None,
    }
}

fn value_at_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn find_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(object) => {
            for key in keys {
                if let Some(candidate) = object.get(*key).and_then(Value::as_str) {
                    if !candidate.trim().is_empty() {
                        return Some(candidate.to_string());
                    }
                }
            }
            object.values().find_map(|value| find_string(value, keys))
        }
        Value::Array(items) => items.iter().find_map(|value| find_string(value, keys)),
        _ => None,
    }
}

fn period_label(period_type: Option<&str>) -> Option<String> {
    match period_type {
        Some("USAGE_PERIOD_TYPE_WEEKLY") => Some("Weekly Build limit".into()),
        Some("USAGE_PERIOD_TYPE_MONTHLY") => Some("Monthly credits".into()),
        Some(value) if !value.trim().is_empty() => Some(value.replace("USAGE_PERIOD_TYPE_", "")),
        _ => None,
    }
}

/// Returns whether a stored profile is the live one, by stable identity.
fn profile_is_active(raw: &str) -> Result<bool, String> {
    Ok(read_live_auth()?
        .as_deref()
        .is_some_and(|live| auth_snapshots_match(raw, live)))
}

/// Renews one profile's access token via the OIDC refresh grant and persists it.
/// When the profile is the live one and `write_live` is set, the fresh token is
/// also written to `~/.grok/auth.json` so the next Grok session uses it.
/// A failure is recorded on the profile (surfaced as `refresh_error`) rather
/// than lost, so the UI can prompt for a real re-login when the token is dead.
async fn refresh_profile_inner(
    profile_id: &str,
    write_live: bool,
    only_if_stale: bool,
) -> Result<bool, String> {
    let _refresh_guard = REFRESH_LOCK.lock().await;
    let (raw, refresh_error) = {
        let store = load_store()?;
        store
            .profiles
            .iter()
            .find(|profile| profile.id == profile_id)
            .map(|profile| (profile.raw_auth_json.clone(), profile.refresh_error.clone()))
            .ok_or("Profile not found")?
    };

    if !refresh_available(&raw, refresh_error.as_deref()) {
        return if only_if_stale {
            Ok(false)
        } else if let Some(error) = refresh_error {
            Err(error)
        } else {
            Err(refresh_material(&raw).unwrap_err())
        };
    }
    if only_if_stale && !token_is_stale(&raw) {
        return Ok(false);
    }

    let live_raw = read_live_auth()?;
    if refresh_blocked_by_running_session(&raw, live_raw.as_deref(), grok_cli_running()) {
        return if only_if_stale {
            Ok(false)
        } else {
            Err("Close running Grok sessions before renewing the active profile".into())
        };
    }

    let result = perform_refresh(&raw).await;

    let mut store = load_store()?;
    let profile = store
        .profiles
        .iter_mut()
        .find(|profile| profile.id == profile_id)
        .ok_or("Profile not found")?;
    match result {
        Ok(new_raw) => {
            profile.raw_auth_json = new_raw.clone();
            profile.email = auth_email(&new_raw).or_else(|| profile.email.clone());
            profile.refresh_error = None;
            profile.last_refresh_at = Some(Utc::now());
            save_store(&store)?;
            if write_live && profile_is_active(&new_raw)? {
                atomic_write(&live_auth_path()?, new_raw.as_bytes())?;
            }
            Ok(true)
        }
        Err(error) => {
            profile.refresh_error = Some(error.clone());
            save_store(&store)?;
            Err(error)
        }
    }
}

#[tauri::command]
async fn refresh_profile(profile_id: String) -> Result<ProfileView, String> {
    refresh_profile_inner(&profile_id, true, false).await?;
    let live_raw = read_live_auth()?;
    let store = load_store()?;
    let profile = store
        .profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .ok_or("Profile not found")?;
    Ok(profile_view(
        profile,
        live_raw
            .as_deref()
            .and_then(|raw| fingerprint(raw).ok())
            .as_deref(),
        live_raw.as_deref().and_then(auth_identity).as_deref(),
    ))
}

/// Silently renews every profile whose token is expired or near expiry. The
/// active profile is left to Grok while a CLI session is running (list_profiles
/// syncs it from the live file instead), to avoid a refresh-token rotation race.
#[tauri::command]
async fn refresh_all_stale() -> Result<Vec<ProfileView>, String> {
    // Adopt any token Grok already renewed before deciding which stored
    // snapshots are stale. This prevents startup from refreshing an obsolete
    // token while list_profiles is still synchronizing the live credential.
    let _ = list_profiles().await?;

    let targets: Vec<String> = {
        let store = load_store()?;
        store
            .profiles
            .iter()
            .filter(|profile| {
                can_refresh(&profile.raw_auth_json) && token_is_stale(&profile.raw_auth_json)
            })
            .map(|profile| profile.id.clone())
            .collect()
    };

    for id in targets {
        // Best effort: any failure is recorded on the profile itself.
        let _ = refresh_profile_inner(&id, true, true).await;
    }
    list_profiles().await
}

#[tauri::command]
async fn get_profile_usage(profile_id: String) -> Result<UsageView, String> {
    // The locked helper rechecks staleness after waiting, so concurrent usage,
    // timer, and startup calls cannot rotate the same refresh token twice.
    let _ = refresh_profile_inner(&profile_id, true, true).await;

    let store = load_store()?;
    let profile = store
        .profiles
        .iter()
        .find(|profile| profile.id == profile_id)
        .ok_or("Profile not found")?;
    let token = access_token(&profile.raw_auth_json).ok_or("Re-login required")?;
    let client = reqwest::Client::new();
    let response = client
        .get(BILLING_CREDITS_URL)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| format!("Usage request failed: {error}"))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(UsageView {
            profile_id,
            used: None,
            limit: None,
            percent: None,
            label: "Re-login".into(),
            period_label: None,
            resets_at: None,
            source: "unavailable".into(),
            error: Some("Credentials expired or were revoked".into()),
        });
    }
    if !response.status().is_success() {
        return Err(format!("Usage service returned {}", response.status()));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("Usage response was invalid: {error}"))?;
    let weekly_percent = find_number(&body, &["creditUsagePercent"]);
    let current_period_type = value_at_path(&body, &["config", "currentPeriod", "type"])
        .and_then(Value::as_str)
        .or_else(|| value_at_path(&body, &["currentPeriod", "type"]).and_then(Value::as_str));
    let current_period_end = value_at_path(&body, &["config", "currentPeriod", "end"])
        .and_then(Value::as_str)
        .or_else(|| value_at_path(&body, &["currentPeriod", "end"]).and_then(Value::as_str))
        .map(str::to_string)
        .or_else(|| find_string(&body, &["billingPeriodEnd"]));
    let weekly_label = period_label(current_period_type);
    if weekly_percent.is_some() || current_period_type == Some("USAGE_PERIOD_TYPE_WEEKLY") {
        let percent = weekly_percent
            .or_else(|| (current_period_type == Some("USAGE_PERIOD_TYPE_WEEKLY")).then_some(0.0))
            .map(|value| value.clamp(0.0, 100.0));
        return Ok(UsageView {
            profile_id,
            used: None,
            limit: None,
            percent,
            label: percent
                .map(|value| format!("{value:.0}% used"))
                .unwrap_or_else(|| "Usage available".into()),
            period_label: weekly_label,
            resets_at: current_period_end,
            source: "weekly".into(),
            error: None,
        });
    }

    let response = client
        .get(BILLING_URL)
        .bearer_auth(access_token(&profile.raw_auth_json).ok_or("Re-login required")?)
        .send()
        .await
        .map_err(|error| format!("Usage request failed: {error}"))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        return Ok(UsageView {
            profile_id,
            used: None,
            limit: None,
            percent: None,
            label: "Re-login".into(),
            period_label: None,
            resets_at: None,
            source: "unavailable".into(),
            error: Some("Credentials expired or were revoked".into()),
        });
    }
    if !response.status().is_success() {
        return Err(format!("Usage service returned {}", response.status()));
    }
    let body: Value = response
        .json()
        .await
        .map_err(|error| format!("Usage response was invalid: {error}"))?;
    let used = find_number(&body, &["used", "usage", "amount_used", "spent"]);
    let limit = find_number(
        &body,
        &["monthlyLimit", "limit", "quota", "total", "amount_limit"],
    );
    let percent = match (used, limit) {
        (Some(used), Some(limit)) if limit > 0.0 => Some((used / limit * 100.0).clamp(0.0, 100.0)),
        _ => None,
    };
    Ok(UsageView {
        profile_id,
        used,
        limit,
        percent,
        label: percent
            .map(|value| format!("{value:.0}% used"))
            .unwrap_or_else(|| "Usage available".into()),
        period_label: Some("Monthly credits".into()),
        resets_at: find_string(&body, &["billingPeriodEnd"]),
        source: "monthly".into(),
        error: None,
    })
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;

        let app_id: Vec<u16> = std::ffi::OsStr::new("com.threereadylab.hydragateway")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        unsafe {
            let _ = SetCurrentProcessExplicitAppUserModelID(app_id.as_ptr());
        }
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let open = MenuItem::with_id(app, "open", "Open Basiliskos", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&open, &quit])?;
            TrayIconBuilder::new()
                .icon(app.default_window_icon().cloned().expect("app icon"))
                .tooltip("Basiliskos")
                .menu(&menu)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "open" => {
                        if let Some(window) = app.get_webview_window("main") {
                            let _ = window.show();
                            let _ = window.set_focus();
                        }
                    }
                    "quit" => {
                        gateway::stop_gateway_internal();
                        app.exit(0);
                    }
                    _ => {}
                })
                .build(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_profiles,
            login_status,
            import_current_profile,
            import_profile_file,
            switch_profile,
            rename_profile,
            delete_profile,
            launch_grok_login,
            launch_grok,
            get_profile_usage,
            refresh_profile,
            refresh_all_stale,
            grok_instances,
            close_grok_instances,
            gateway::gateway_snapshot,
            gateway::start_gateway,
            gateway::stop_gateway,
            gateway::select_gateway_account,
            gateway::rename_gateway_account,
            gateway::get_gateway_account_usage,
            gateway::set_gateway_route,
            gateway::remove_gateway_account,
            gateway::launch_provider_login,
            gateway::launch_hydra_claude,
            gateway::stop_hydra_claude
        ])
        .build(tauri::generate_context!())
        .expect("error while building Basiliskos")
        .run(|app, event| match event {
            tauri::RunEvent::WindowEvent {
                label,
                event: WindowEvent::CloseRequested { api, .. },
                ..
            } if label == "main" => {
                api.prevent_close();
                if let Some(window) = app.get_webview_window("main") {
                    let _ = window.hide();
                }
            }
            tauri::RunEvent::Exit | tauri::RunEvent::ExitRequested { .. } => {
                gateway::stop_gateway_internal();
            }
            _ => {}
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_auth_key_order() {
        let left = r#"{"b":2,"a":{"y":1,"x":0}}"#;
        let right = r#"{"a":{"x":0,"y":1},"b":2}"#;
        assert_eq!(fingerprint(left).unwrap(), fingerprint(right).unwrap());
    }

    #[test]
    fn extracts_nested_identity_and_token() {
        let raw = r#"{"provider":{"email":"person@example.com","key":"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ"}}"#;
        assert_eq!(auth_email(raw).as_deref(), Some("person@example.com"));
        assert!(access_token(raw).is_some());
    }

    #[test]
    fn identity_is_stable_across_token_rotation() {
        let before = r#"{"https://auth.x.ai::c1":{"key":"AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA","user_id":"u-123","refresh_token":"r1"}}"#;
        let after = r#"{"https://auth.x.ai::c1":{"key":"BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB","user_id":"u-123","refresh_token":"r2"}}"#;
        assert_ne!(fingerprint(before).unwrap(), fingerprint(after).unwrap());
        assert_eq!(auth_identity(before), auth_identity(after));
        assert_eq!(auth_identity(before).as_deref(), Some("u-123"));
        assert!(auth_snapshots_match(before, after));
    }

    #[test]
    fn auth_match_falls_back_to_normalized_fingerprint() {
        let left = r#"{"credential":{"key":"same","refresh_token":"same-r"},"b":2}"#;
        let right = r#"{"b":2,"credential":{"refresh_token":"same-r","key":"same"}}"#;
        assert!(auth_snapshots_match(left, right));
        assert!(!auth_snapshots_match("not-json", "also-not-json"));
    }

    #[test]
    fn active_running_profile_blocks_refresh() {
        let active = r#"{"credential":{"key":"old","refresh_token":"r1","user_id":"u-1"}}"#;
        let rotated_live = r#"{"credential":{"key":"new","refresh_token":"r2","user_id":"u-1"}}"#;
        let other = r#"{"credential":{"key":"other","refresh_token":"r3","user_id":"u-2"}}"#;
        assert!(refresh_blocked_by_running_session(
            active,
            Some(rotated_live),
            true
        ));
        assert!(!refresh_blocked_by_running_session(
            other,
            Some(rotated_live),
            true
        ));
        assert!(!refresh_blocked_by_running_session(
            active,
            Some(rotated_live),
            false
        ));
    }

    #[test]
    fn stale_when_expiry_passed_or_missing() {
        let expired =
            r#"{"c":{"key":"k","refresh_token":"r","expires_at":"2000-01-01T00:00:00Z"}}"#;
        let missing = r#"{"c":{"key":"k","refresh_token":"r"}}"#;
        let future = format!(
            r#"{{"c":{{"key":"k","refresh_token":"r","expires_at":"{}"}}}}"#,
            (Utc::now() + Duration::hours(5)).to_rfc3339()
        );
        assert!(token_is_stale(expired));
        assert!(token_is_stale(missing));
        assert!(!token_is_stale(&future));
    }

    #[test]
    fn refresh_material_reads_oidc_fields() {
        let raw = r#"{"https://auth.x.ai::c1":{"key":"k","refresh_token":"r-abc","oidc_client_id":"client-1","oidc_issuer":"https://auth.x.ai"}}"#;
        let (refresh, client, issuer) = refresh_material(raw).unwrap();
        assert_eq!(refresh, "r-abc");
        assert_eq!(client, "client-1");
        assert_eq!(issuer, "https://auth.x.ai");
        assert!(can_refresh(raw));
        assert!(!can_refresh(r#"{"c":{"key":"k"}}"#));
    }

    #[test]
    fn refresh_material_rejects_untrusted_issuer() {
        let raw = r#"{"c":{"key":"k","refresh_token":"r-abc","oidc_client_id":"client-1","oidc_issuer":"https://example.invalid"}}"#;
        assert!(!can_refresh(raw));
        assert_eq!(
            refresh_material(raw).unwrap_err(),
            "Unsupported login issuer; run the official grok login and re-import"
        );
    }

    #[test]
    fn revoked_refresh_token_requires_relogin() {
        let raw = r#"{"c":{"key":"k","refresh_token":"r-abc","oidc_client_id":"client-1","oidc_issuer":"https://auth.x.ai"}}"#;
        assert!(refresh_available(raw, None));
        assert!(!refresh_available(raw, Some(RELOGIN_REQUIRED_ERROR)));
        assert!(refresh_available(raw, Some("Temporary network failure")));
    }

    #[test]
    fn apply_refresh_replaces_token_fields_only() {
        let raw = r#"{"https://auth.x.ai::c1":{"key":"old","refresh_token":"old-r","expires_at":"2000-01-01T00:00:00Z","email":"person@example.com","user_id":"u-9"},"other":{"key":"other-key","refresh_token":"other-r","expires_at":"2040-01-01T00:00:00Z"}}"#;
        let updated =
            apply_refresh_to_auth(raw, "old-r", "new-key", "new-r", "2030-01-01T00:00:00Z")
                .unwrap();
        let value: Value = serde_json::from_str(&updated).unwrap();
        let cred = value.get("https://auth.x.ai::c1").unwrap();
        assert_eq!(cred.get("key").unwrap(), "new-key");
        assert_eq!(cred.get("refresh_token").unwrap(), "new-r");
        assert_eq!(cred.get("expires_at").unwrap(), "2030-01-01T00:00:00Z");
        // Non-token fields are preserved.
        assert_eq!(cred.get("email").unwrap(), "person@example.com");
        assert_eq!(cred.get("user_id").unwrap(), "u-9");
        let other = value.get("other").unwrap();
        assert_eq!(other.get("key").unwrap(), "other-key");
        assert_eq!(other.get("refresh_token").unwrap(), "other-r");
    }

    #[test]
    fn reads_wrapped_billing_numbers() {
        let billing = serde_json::json!({
            "config": {
                "monthlyLimit": { "val": 100 },
                "used": { "val": 25 }
            }
        });
        assert_eq!(find_number(&billing, &["used"]), Some(25.0));
        assert_eq!(
            find_number(&billing, &["monthlyLimit", "limit"]),
            Some(100.0)
        );
    }
}
