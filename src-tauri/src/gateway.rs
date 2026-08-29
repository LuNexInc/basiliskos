use bytes::Bytes;
use chrono::{DateTime, Duration as ChronoDuration, TimeZone, Utc};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc, Arc, Condvar, Mutex, MutexGuard, OnceLock,
    },
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use tiny_http::{Header, Response, Server, StatusCode};
use uuid::Uuid;

use crate::diagnostics::{self, DiagnosticEvent, ErrorCode};

use crate::catalog::{
    alias_to_picker_entry, all_providers, auth_kinds_for, context_budget_for_request,
    context_window_for_route, default_api_base_url, default_model, default_routes, model_specs,
    picker_entries, thinking_level_label, ModelSpec, ProviderAuth, SUPPORTED_PROVIDERS,
};
use crate::claude_window::{
    claude_icon_path, enum_claude_hwnds_for_pid, log_icon_line, spawn_claude_icon_reapply,
    ClaudeIconKind,
};
use crate::codex_window::{
    codex_icon_path, enum_codex_hwnds_for_pid, log_icon_line as codex_log_icon_line,
    spawn_codex_icon_reapply, CodexIconKind,
};
use crate::usage::{
    parse_claude_usage, parse_codex_usage, parse_kimi_usage, parse_xai_usage, GatewayAccountUsage,
};
use crate::vision::tool_compatibility_fixups;
use crate::zai_oauth::{self, ZaiOAuth};

use crate::persistence::{
    durable_write, load_json_with_recovery, recover_pending_transactions, run_transaction,
    secure_create_dir_all, secure_existing_path, FileMutation,
};

// Pin CLIProxyAPI 7.2.139 (2026-08-22). Re-audited against the 7.2.139 windows
// release: config contract (auth-dir, api-keys, api-key-entries, oauth-model-alias,
// xai.inject-x-search, codex.optimize-multi-agent-v2, plugins) and the
// api-key-entries shape hold; gateway smoke + log-redaction green. The
// `-<provider>-login` flags and the model alias registry are unchanged. If a
// future bump changes any of that, the new pin guard (`scripts/check-cliproxy.ps1`)
// and `render_config_keeps_the_cliproxy_contract` test should be updated together.
const GATEWAY_VERSION: &str = "7.2.139";
pub(crate) const GATEWAY_EXE_SHA256: &str =
    "457d717382189c38a2641dd5ae3b467c86b4cdb5b1833d5c289375fb4a86cf0b";
const GATEWAY_PORT: u16 = 8317;
const BACKEND_PORT: u16 = 8318;
const MAX_RELAY_BODY_BYTES: usize = 8 * 1024 * 1024;
const MAX_RELAY_HEADER_BYTES: usize = 64 * 1024;
const MAX_RELAY_HEADERS: usize = 64;
const RELAY_WORKERS: usize = 8;
const RELAY_QUEUE_CAPACITY: usize = 32;
const RELAY_DRAIN_TIMEOUT: Duration = Duration::from_secs(5);
const FIRST_RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
const BASILISKOS_CONFIG_NAME: &str = "Basiliskos";
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const XAI_USAGE_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
const KIMI_USAGE_URL: &str = "https://api.kimi.com/coding/v1/usages";
const BASILISKOS_LATEST_RELEASE_URL: &str =
    "https://api.github.com/repos/LuNexInc/basiliskos/releases/latest";
const BASILISKOS_RELEASE_DOWNLOAD_BASE: &str =
    "https://github.com/LuNexInc/basiliskos/releases/download";
const MAX_RELEASE_MANIFEST_BYTES: usize = 128 * 1024;
const MAX_RELEASE_INSTALLER_BYTES: u64 = 512 * 1024 * 1024;
const MAX_CONTEXT_COUNT_RESPONSE_BYTES: usize = 64 * 1024;
const DEFAULT_RATE_LIMIT_COOLDOWN_SECS: i64 = 60;
const XAI_CREDENTIAL_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(2 * 60);
const XAI_DEFAULT_TOKEN_LIFETIME_SECS: i64 = 6 * 60 * 60;
const OAUTH_CREDENTIAL_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(2 * 60);
const BACKEND_SUPERVISION_INTERVAL: Duration = Duration::from_secs(1);
const OAUTH_REFRESH_SKEW_SECS: i64 = 5 * 60;
const CODEX_TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";
const CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
const CLAUDE_TOKEN_ENDPOINT: &str = "https://api.anthropic.com/v1/oauth/token";
const CLAUDE_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
const XAI_RELOGIN_REQUIRED: &str =
    "This saved Grok authorization was revoked. Sign in again to renew it.";
const KIMI_CREDENTIAL_MAINTENANCE_INTERVAL: Duration = Duration::from_secs(2 * 60);
const KIMI_REFRESH_SKEW_SECS: i64 = 5 * 60;
const KIMI_DEFAULT_TOKEN_LIFETIME_SECS: i64 = 15 * 60;
const KIMI_TOKEN_ENDPOINT: &str = "https://auth.kimi.com/api/oauth/token";
const KIMI_CLIENT_ID: &str = "17e5f671-d194-4dfb-9706-5516cb48c098";
const KIMI_RELOGIN_REQUIRED: &str =
    "This saved Kimi authorization was revoked. Sign in again to renew it.";

/// The relay codex account that anchors the isolated Basiliskos Codex window's
/// login. It is EXCLUDED from the relay's automatic refresh (one-refresher
/// rule â€” the isolated app owns it), so the normal app's or the relay's
/// refreshes can never rotate away the seeded credential. The account must be
/// one the user's normal Codex app does NOT use.
const CODEX_ANCHOR_FILE_NAME: &str = "codex-charles.3ready@gmail.com.json";

#[derive(Default)]
struct WorkerTracker {
    active: Mutex<usize>,
    changed: Condvar,
}

struct FrontProxy {
    shutdown: mpsc::Sender<()>,
    listener: thread::JoinHandle<()>,
    workers: Vec<thread::JoinHandle<()>>,
    tracker: Arc<WorkerTracker>,
    async_runtime: Arc<tokio::runtime::Runtime>,
}

impl FrontProxy {
    fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.listener.join();
        let deadline = Instant::now() + RELAY_DRAIN_TIMEOUT;
        if let Ok(mut active) = self.tracker.active.lock() {
            while *active > 0 && Instant::now() < deadline {
                let remaining = deadline.saturating_duration_since(Instant::now());
                match self.tracker.changed.wait_timeout(active, remaining) {
                    Ok((next, _)) => active = next,
                    Err(_) => return,
                }
            }
            if *active == 0 {
                drop(active);
                for worker in self.workers {
                    let _ = worker.join();
                }
                return;
            }
        }
        // A client or upstream may still be inside a bounded read timeout. Dropping a
        // JoinHandle detaches it; the client timeout guarantees eventual cleanup while
        // keeping application shutdown bounded.
        drop(self.workers);
        drop(self.async_runtime);
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum GatewayPhase {
    #[default]
    Stopped,
    Starting,
    Running,
    Degraded,
    Stopping,
}

#[derive(Default)]
struct ControllerRuntime {
    phase: GatewayPhase,
    gateway_child: Option<Child>,
    claude_child: Option<Child>,
    #[cfg(target_os = "windows")]
    claude_job: Option<usize>,
    claude_root_pid: Option<u32>,
    claude_executable: Option<PathBuf>,
    claude_profile: Option<PathBuf>,
    codex_child: Option<Child>,
    #[cfg(target_os = "windows")]
    codex_job: Option<usize>,
    codex_root_pid: Option<u32>,
    codex_watcher_generation: Option<u32>,
    codex_executable: Option<PathBuf>,
    codex_home: Option<PathBuf>,
    front_proxy: Option<FrontProxy>,
    backend_exit_reason: Option<String>,
    backend_restart_attempts: u32,
    backend_next_restart: Option<Instant>,
    last_known_good_models: BTreeMap<String, String>,
    last_known_model_catalog: BTreeMap<String, Vec<String>>,
    account_cooldowns: BTreeMap<String, chrono::DateTime<Utc>>,
    last_auto_failover: Option<AutoFailoverInfo>,
    login_claim: Option<String>,
    login: Option<LoginRuntime>,
    last_login: Option<ProviderLoginStatus>,
    #[cfg(target_os = "windows")]
    gateway_job: Option<usize>,
}

/// Identifies the client whose account and route state a command or request uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClientSurface {
    Claude,
    Codex,
}

impl ClientSurface {
    fn parse(value: &str) -> Result<Self, String> {
        match value {
            "claude" => Ok(Self::Claude),
            "codex" => Ok(Self::Codex),
            _ => Err("Client must be claude or codex".into()),
        }
    }
}

#[derive(Default)]
struct ControllerManager {
    runtime: Mutex<ControllerRuntime>,
    mutations: Mutex<()>,
}

static CONTROLLER: OnceLock<ControllerManager> = OnceLock::new();
static PREPARED_UPDATE_INSTALLERS: OnceLock<Mutex<BTreeMap<String, PathBuf>>> = OnceLock::new();
type AccountRefreshLocks = OnceLock<Mutex<BTreeMap<String, Arc<tokio::sync::Mutex<()>>>>>;
// Refresh grants may rotate. Keep one refresh exchange per relay account so a
// simultaneous selection and "Serve Grok CLI" cannot overwrite a newer grant.
static XAI_REFRESH_LOCKS: AccountRefreshLocks = OnceLock::new();
static XAI_REFRESH_STATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static XAI_CREDENTIAL_MAINTENANCE_STARTED: OnceLock<()> = OnceLock::new();
static CODEX_REFRESH_LOCKS: AccountRefreshLocks = OnceLock::new();
static CLAUDE_REFRESH_LOCKS: AccountRefreshLocks = OnceLock::new();
static OAUTH_CREDENTIAL_MAINTENANCE_STARTED: OnceLock<()> = OnceLock::new();
static BACKEND_SUPERVISION_STARTED: OnceLock<()> = OnceLock::new();
static KIMI_REFRESH_LOCKS: AccountRefreshLocks = OnceLock::new();
static KIMI_REFRESH_STATE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static KIMI_CREDENTIAL_MAINTENANCE_STARTED: OnceLock<()> = OnceLock::new();
fn controller() -> &'static ControllerManager {
    CONTROLLER.get_or_init(ControllerManager::default)
}

fn runtime_lock() -> Result<MutexGuard<'static, ControllerRuntime>, String> {
    controller()
        .runtime
        .lock()
        .map_err(|_| "Basiliskos controller runtime state is locked".into())
}

fn mutation_lock() -> Result<MutexGuard<'static, ()>, String> {
    controller()
        .mutations
        .lock()
        .map_err(|_| "Basiliskos controller mutation state is locked".into())
}

fn xai_refresh_lock(file_name: &str) -> Result<Arc<tokio::sync::Mutex<()>>, String> {
    let locks = XAI_REFRESH_LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut locks = locks
        .lock()
        .map_err(|_| "Basiliskos Grok refresh coordination is locked".to_string())?;
    Ok(locks
        .entry(file_name.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone())
}

fn oauth_refresh_lock(
    locks: &'static AccountRefreshLocks,
    file_name: &str,
    provider: &str,
) -> Result<Arc<tokio::sync::Mutex<()>>, String> {
    let locks = locks.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut locks = locks
        .lock()
        .map_err(|_| format!("Basiliskos {provider} refresh coordination is locked"))?;
    Ok(locks
        .entry(file_name.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone())
}

fn kimi_refresh_lock(file_name: &str) -> Result<Arc<tokio::sync::Mutex<()>>, String> {
    let locks = KIMI_REFRESH_LOCKS.get_or_init(|| Mutex::new(BTreeMap::new()));
    let mut locks = locks
        .lock()
        .map_err(|_| "Basiliskos Kimi refresh coordination is locked".to_string())?;
    Ok(locks
        .entry(file_name.to_string())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
        .clone())
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayAccount {
    pub file_name: String,
    pub provider: String,
    pub email: Option<String>,
    pub label: String,
    pub disabled: bool,
    pub active: bool,
    pub active_for_codex: bool,
    pub cooldown_until_ms: Option<i64>,
    /// The provider access-token expiry when the saved credential exposes one.
    /// This is metadata only: no token material ever leaves the local backend.
    pub expires_at_ms: Option<i64>,
    /// A small, user-facing credential health state. `relogin_required` is
    /// deliberately reserved for a provider-confirmed terminal refresh error.
    pub credential_status: String,
    /// `oauth` or `api_key` — which auth method this account uses.
    pub auth: String,
    /// Upstream base URL for API-key accounts; None for OAuth accounts.
    pub base_url: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySnapshot {
    pub running: bool,
    pub base_url: String,
    pub version: String,
    pub claude_running: bool,
    pub codex_running: bool,
    pub accounts: Vec<GatewayAccount>,
    pub active_account: Option<String>,
    pub routes: Vec<ProviderRoute>,
    pub active_codex_account: Option<String>,
    pub codex_routes: Vec<ProviderRoute>,
    /// Latest same-provider auto-failover, when one happened this session.
    /// The UI shows this once so a silent credential switch is not invisible.
    pub auto_failover: Option<AutoFailoverInfo>,
    pub controller: ComponentStatus,
    pub relay: ComponentStatus,
    pub backend: ComponentStatus,
    pub credentials: ComponentStatus,
    pub route: ComponentStatus,
    pub oauth: ComponentStatus,
    pub claude: ComponentStatus,
    pub codex: ComponentStatus,
    pub backend_exit_reason: Option<String>,
    pub active_requests: usize,
    pub diagnostics: Vec<DiagnosticEvent>,
    pub login: Option<ProviderLoginStatus>,
    pub skip_model_switch_confirmation: bool,
    /// Basiliskos setting: re-open the isolated Claude window at launch when an
    /// account is active (default true). Mirrors `ControllerState`.
    pub open_claude_on_launch: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountSelectionResult {
    #[serde(flatten)]
    pub snapshot: GatewaySnapshot,
    pub claude_config_changed: bool,
}

/// Result of changing the route (model/thinking). `route_verified` is false
/// only when an active account exists but the local backend was unreachable at
/// set time, so the saved route could not be checked against the backend's
/// live model catalog. The per-request validator will still fall back to a
/// known-good model if the route turns out to be unavailable.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteUpdateResult {
    #[serde(flatten)]
    pub snapshot: GatewaySnapshot,
    pub route_verified: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentStatus {
    pub state: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AutoFailoverInfo {
    pub from_label: String,
    pub to_label: String,
    pub at_ms: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteModelOption {
    pub id: String,
    pub label: String,
    pub thinking_levels: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCatalogEntry {
    pub id: String,
    pub label: String,
    pub hidden: bool,
    /// None when the backend's live model catalog hasn't been fetched for
    /// this provider yet (e.g. no account of this provider has been active
    /// this session). Some(false) means the backend was reachable and did
    /// not report this model as available.
    pub live: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRoute {
    pub provider: String,
    pub selected_model: String,
    pub selected_model_label: String,
    pub thinking: String,
    pub context_window: Option<u64>,
    pub model_options: Vec<RouteModelOption>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderLoginLaunch {
    pub session_id: String,
    pub authorization_url: String,
    pub user_code: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderLoginStatus {
    pub session_id: String,
    pub provider: String,
    pub state: String,
    pub started_at: String,
    pub result_file_name: Option<String>,
    pub detail: String,
}

struct LoginRuntime {
    status: ProviderLoginStatus,
    child: Option<Arc<Mutex<Child>>>,
    cancel: Arc<AtomicBool>,
    staging_dir: PathBuf,
    #[cfg(target_os = "windows")]
    job: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct RouteSelection {
    pub(crate) model: String,
    pub(crate) thinking: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ClaudeWindowIcon {
    Black,
    System,
}

fn default_claude_window_icon() -> ClaudeWindowIcon {
    ClaudeWindowIcon::Black
}

fn should_apply_claude_window_icon(icon: ClaudeWindowIcon) -> bool {
    matches!(icon, ClaudeWindowIcon::Black)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ControllerState {
    api_key: String,
    claude_config_id: String,
    #[serde(default)]
    previous_claude_applied_id: Option<String>,
    #[serde(default)]
    active_account: Option<String>,
    #[serde(default)]
    active_codex_account: Option<String>,
    #[serde(default = "default_routes")]
    routes: BTreeMap<String, RouteSelection>,
    #[serde(default = "default_routes")]
    codex_routes: BTreeMap<String, RouteSelection>,
    /// Basiliskos-owned preference: recolor the isolated Claude window/tray icons.
    /// Never written into Claude's own profile. Default black (distinct from stock Claude).
    #[serde(default = "default_claude_window_icon")]
    claude_window_icon: ClaudeWindowIcon,
    /// If true, skip the account-switch restart confirmation in Basiliskos.
    #[serde(default)]
    skip_model_switch_confirmation: bool,
    /// If true (default), launching Basiliskos re-opens the isolated Claude
    /// window when an account is already active. If false, the relay starts
    /// but the window stays closed until the user opens it.
    #[serde(default = "default_open_claude_on_launch")]
    open_claude_on_launch: bool,
}

fn default_open_claude_on_launch() -> bool {
    true
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct XaiRefreshState {
    #[serde(default)]
    relogin_required: BTreeSet<String>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct KimiRefreshState {
    #[serde(default)]
    relogin_required: BTreeSet<String>,
}

fn migrate_controller_state(mut state: ControllerState) -> ControllerState {
    if state.routes.is_empty() {
        state.routes = default_routes();
    }
    if state.codex_routes.is_empty() {
        state.codex_routes = default_routes();
    }
    state
}

fn client_routes(
    state: &ControllerState,
    client: ClientSurface,
) -> &BTreeMap<String, RouteSelection> {
    match client {
        ClientSurface::Claude => &state.routes,
        ClientSurface::Codex => &state.codex_routes,
    }
}

fn client_routes_mut(
    state: &mut ControllerState,
    client: ClientSurface,
) -> &mut BTreeMap<String, RouteSelection> {
    match client {
        ClientSurface::Claude => &mut state.routes,
        ClientSurface::Codex => &mut state.codex_routes,
    }
}

fn active_account_for(state: &ControllerState, client: ClientSurface) -> Option<&str> {
    match client {
        ClientSurface::Claude => state.active_account.as_deref(),
        ClientSurface::Codex => state.active_codex_account.as_deref(),
    }
}

fn set_active_account_for(
    state: &mut ControllerState,
    client: ClientSurface,
    file_name: Option<String>,
) {
    match client {
        ClientSurface::Claude => state.active_account = file_name,
        ClientSurface::Codex => state.active_codex_account = file_name,
    }
}

fn normalized_route_for(
    state: &ControllerState,
    client: ClientSurface,
    provider: &str,
) -> RouteSelection {
    let specs = model_specs(provider);
    let spec_is_pinned = !specs.is_empty();
    let stored = client_routes(state, client).get(provider);
    let model = stored
        .map(|route| route.model.as_str())
        .filter(|model| {
            !model.is_empty() && (!spec_is_pinned || specs.iter().any(|spec| spec.id == *model))
        })
        .unwrap_or_else(|| default_model(provider));
    if model.is_empty() {
        // Guard against a provider with no default (never happens today, but
        // keeps the caller from building an empty route).
        return RouteSelection {
            model: String::new(),
            thinking: "auto".into(),
        };
    }
    // For live-catalog providers (routers/custom) there are no pinned specs; the
    // stored or default model id is accepted as-is and validated against the
    // backend's live catalog later in `validated_route_for_request`.
    let spec = specs.iter().find(|spec| spec.id == model);
    let thinking = stored
        .map(|route| route.thinking.as_str())
        .filter(|thinking| {
            spec.map_or(*thinking == "auto", |spec| {
                *thinking == "auto" || spec.thinking_levels.contains(thinking)
            })
        })
        .unwrap_or("auto");
    RouteSelection {
        model: model.to_string(),
        thinking: thinking.to_string(),
    }
}

fn normalized_route(state: &ControllerState, provider: &str) -> RouteSelection {
    normalized_route_for(state, ClientSurface::Claude, provider)
}

fn provider_route_for(
    state: &ControllerState,
    client: ClientSurface,
    provider: &str,
) -> ProviderRoute {
    let route = normalized_route_for(state, client, provider);
    let specs = model_specs(provider);
    let selected = specs.iter().find(|spec| spec.id == route.model);
    let hidden = load_hidden_models().unwrap_or_default();
    let live_catalog = runtime_lock()
        .ok()
        .and_then(|runtime| runtime.last_known_model_catalog.get(provider).cloned());
    let mut model_options: Vec<RouteModelOption> = filter_visible_models(
        provider,
        specs,
        &route.model,
        &hidden,
        live_catalog.as_deref(),
    )
    .into_iter()
    .map(|spec| RouteModelOption {
        id: spec.id.to_string(),
        label: spec.label.to_string(),
        thinking_levels: spec
            .thinking_levels
            .iter()
            .map(|level| level.to_string())
            .collect(),
    })
    .collect();
    // Live-catalog providers (routers/custom, API-key accounts) have no pinned
    // specs, so the route panel's model picker is sourced from the live
    // `/v1/models` list instead of an empty options list.
    if model_options.is_empty() {
        for id in live_catalog.iter().flatten() {
            if hidden.contains(id) {
                continue;
            }
            model_options.push(RouteModelOption {
                id: id.clone(),
                label: id.clone(),
                thinking_levels: vec!["auto".into()],
            });
        }
        if !model_options.iter().any(|option| option.id == route.model) {
            model_options.insert(
                0,
                RouteModelOption {
                    id: route.model.clone(),
                    label: route.model.clone(),
                    thinking_levels: vec!["auto".into()],
                },
            );
        }
    }
    // Live-catalog providers (routers/custom) have no pinned label; show the
    // model id as its own label rather than an empty options list.
    let context_window = context_window_for_route(provider, &route.model);
    let selected_model_label = selected
        .map(|spec| spec.label.to_string())
        .unwrap_or_else(|| route.model.clone());
    ProviderRoute {
        provider: provider.to_string(),
        selected_model: route.model,
        selected_model_label,
        thinking: route.thinking,
        context_window,
        model_options,
    }
}

fn provider_route(state: &ControllerState, provider: &str) -> ProviderRoute {
    provider_route_for(state, ClientSurface::Claude, provider)
}

/// Returns the (model, thinking) pair the client chose in Claude's picker,
/// when it is a visible catalog model. The request carries the Anthropic
/// routing alias Claude advertised; `alias_to_picker_entry` maps it back to
/// the real upstream model and thinking level. Returns None for unknown or
/// hidden ids so callers fall back to the Basiliskos route.
fn client_picker_choice(
    request: &serde_json::Map<String, Value>,
    provider: &str,
    hidden: &BTreeSet<String>,
    selected_model: &str,
) -> Option<(String, String)> {
    let requested = request.get("model").and_then(Value::as_str)?;
    let (model, thinking) = alias_to_picker_entry(provider, requested, selected_model)?;
    if !model_specs(provider).iter().any(|spec| spec.id == model) || hidden.contains(&model) {
        return None;
    }
    Some((model, thinking))
}

/// Reads Claude's native thinking/effort control from the request
/// (`output_config.effort` or the top-level `effort` field, the shape Claude
/// Desktop sends for gateway models). Returns None when the client left
/// thinking on auto.
fn client_effort_choice(request: &serde_json::Map<String, Value>) -> Option<String> {
    let effort = request
        .get("output_config")
        .and_then(Value::as_object)
        .and_then(|config| config.get("effort"))
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            request
                .get("effort")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })?;
    match effort.as_str() {
        "auto" => None,
        "low" | "medium" | "high" | "xhigh" | "max" | "ultra" | "none" => Some(effort),
        _ => None,
    }
}

/// Applies the provider-specific model transformation (thinking suffix, Grok
/// 4.5 desktop-effort remap, DeepSeek plain id) to a base model id. The base is
/// either the Basiliskos route selection or a client-chosen picker model.
fn backend_model_identifier<'a>(provider: &str, base_model: &'a str) -> &'a str {
    if provider == "antigravity" {
        match base_model {
            "gemini-3.7-flash" | "gemini-3.6-flash-high" => "gemini-3.6-flash-high",
            "gemini-3.7-pro" | "gemini-3.1-pro-low" => "gemini-3.1-pro-low",
            "gemini-3.7-flash-lite" | "gemini-3.1-flash-lite" => "gemini-3.1-flash-lite",
            "gemini-3-flash" => "gemini-3-flash",
            other => other,
        }
    } else {
        base_model
    }
}

fn apply_route_model(
    base_model: &str,
    thinking_override: Option<&str>,
    request: &mut serde_json::Map<String, Value>,
    state: &ControllerState,
    provider: &str,
) -> String {
    let target_model = backend_model_identifier(provider, base_model);
    // A picker variant carries its own validated thinking level; otherwise use
    // the route's thinking (validated against the model actually being routed).
    let route_thinking = normalized_route(state, provider).thinking;
    if let Some(level) = thinking_override {
        if level == "auto" {
            return target_model.to_string();
        }
        if provider == "xai" && base_model == "grok-4.5" {
            let remapped =
                grok_4_5_thinking_from_desktop_effort(request).unwrap_or_else(|| level.to_string());
            return format!("{}({})", target_model, remapped);
        }
        return format!("{}({})", target_model, level);
    }
    let thinking = model_specs(provider)
        .iter()
        .find(|spec| spec.id == base_model)
        .map(|spec| {
            if route_thinking == "auto" || spec.thinking_levels.contains(&route_thinking.as_str()) {
                route_thinking.as_str()
            } else {
                "auto"
            }
        })
        .unwrap_or("auto");
    let thinking = if provider == "xai" && base_model == "grok-4.5" {
        grok_4_5_thinking_from_desktop_effort(request).unwrap_or_else(|| thinking.to_string())
    } else {
        thinking.to_string()
    };
    if thinking == "auto" {
        target_model.to_string()
    } else {
        format!("{}({})", target_model, thinking)
    }
}

/// Claude Desktop's high-context routing aliases expose effort levels that do
/// not exactly match Grok 4.5. Map the customer-facing picker to the levels
/// Grok actually accepts, and remove the Claude-only effort field before the
/// request is handed to CLIProxyAPI.
fn grok_4_5_thinking_from_desktop_effort(
    request: &mut serde_json::Map<String, Value>,
) -> Option<String> {
    let nested_effort = request
        .get_mut("output_config")
        .and_then(Value::as_object_mut)
        .and_then(|config| config.remove("effort"))
        .and_then(|value| value.as_str().map(str::to_owned));
    if request
        .get("output_config")
        .and_then(Value::as_object)
        .is_some_and(serde_json::Map::is_empty)
    {
        request.remove("output_config");
    }
    let effort = nested_effort.or_else(|| {
        request
            .remove("effort")
            .and_then(|value| value.as_str().map(str::to_owned))
    })?;
    match effort.as_str() {
        "low" | "medium" => Some("low".into()),
        "high" | "xhigh" | "max" => Some("high".into()),
        _ => None,
    }
}

// Claude Desktop's managed custom-provider config validates inferenceModels
// against Anthropic's provider catalog. Fable 5 provides the richest Claude
// Code UI capabilities, including UltraCode. This is still a routing alias;
// the gateway sends the selected route's actual upstream model, while
// `labelOverride` identifies it.
const CLAUDE_DESKTOP_ROUTING_MODEL: &str = "claude-fable-5";

fn advertised_model_name(_state: &ControllerState, _provider: Option<&str>) -> String {
    CLAUDE_DESKTOP_ROUTING_MODEL.into()
}

fn route_label(state: &ControllerState, provider: Option<&str>) -> String {
    provider
        .filter(|provider| SUPPORTED_PROVIDERS.contains(provider))
        .map(|provider| provider_route(state, provider).selected_model_label)
        .unwrap_or_else(|| "Choose a route".into())
}

fn provider_label(provider: &str) -> &'static str {
    match provider {
        "claude" => "Claude",
        "codex" => "Codex",
        "xai" => "Grok Build",
        "kimi" => "Kimi Code",
        "antigravity" => "Antigravity",
        "zai" => "Z.AI GLM",
        _ => "Unknown provider",
    }
}

fn route_identity_prompt(state: &ControllerState, provider: &str) -> String {
    let route = provider_route(state, provider);
    format!(
        "You are a routed coding assistant inside Basiliskos. Your current upstream route is {} via {}. When asked what model or assistant you are, answer with the actual route: '{} via {}'. If asked for the underlying backend, state the current upstream route truthfully. Do not claim to be Claude or Sonnet unless the current upstream route is actually that model.",
        route.selected_model_label,
        provider_label(provider),
        route.selected_model_label,
        provider_label(provider),
    )
}

fn root_dir() -> Result<PathBuf, String> {
    dirs::home_dir()
        .map(|home| home.join(".hydra-gateway"))
        .ok_or_else(|| "Unable to locate your home directory".to_string())
}

pub(crate) fn gateway_dir() -> Result<PathBuf, String> {
    Ok(root_dir()?.join("gateway"))
}

fn auth_dir() -> Result<PathBuf, String> {
    Ok(gateway_dir()?.join("auth"))
}

/// Where API-key credentials live, separate from OAuth auth files so the OAuth
/// refresh/maintenance paths never touch them.
fn keys_dir() -> Result<PathBuf, String> {
    Ok(gateway_dir()?.join("keys"))
}

/// The auth kind a credential file represents. OAuth credentials are objects
/// whose `type` is a provider map; API-key credentials carry a top-level
/// `"kind": "api_key"` discriminator (and `type` may be a plain string).
fn account_auth_kind(value: &Value) -> ProviderAuth {
    let is_api_key = value
        .get("kind")
        .and_then(Value::as_str)
        .map(|kind| kind.eq_ignore_ascii_case("api_key"))
        .unwrap_or(false);
    if is_api_key {
        ProviderAuth::ApiKey
    } else {
        ProviderAuth::Oauth
    }
}

/// The account file name that holds the active account for a client, or None
/// when no account is selected.
fn account_file_name(account: Option<&str>) -> Option<&str> {
    account.filter(|name| !name.is_empty())
}

fn controller_path() -> Result<PathBuf, String> {
    Ok(root_dir()?.join("controller.json"))
}

fn account_labels_path() -> Result<PathBuf, String> {
    Ok(root_dir()?.join("account-labels.json"))
}

pub(crate) fn hidden_models_path() -> Result<PathBuf, String> {
    Ok(root_dir()?.join("hidden-models.json"))
}

fn xai_refresh_state_path() -> Result<PathBuf, String> {
    Ok(root_dir()?.join("xai-refresh-state.json"))
}

fn kimi_refresh_state_path() -> Result<PathBuf, String> {
    Ok(root_dir()?.join("kimi-refresh-state.json"))
}

fn kimi_relogin_required(file_name: &str) -> Result<bool, String> {
    let lock = KIMI_REFRESH_STATE_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| "Basiliskos Kimi refresh state is locked".to_string())?;
    let path = kimi_refresh_state_path()?;
    if !path.exists() {
        return Ok(false);
    }
    Ok(
        load_json_with_recovery::<KimiRefreshState>(&path, "Basiliskos Kimi refresh state")?
            .relogin_required
            .contains(file_name),
    )
}

fn set_kimi_relogin_required(file_name: &str, required: bool) -> Result<(), String> {
    let lock = KIMI_REFRESH_STATE_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| "Basiliskos Kimi refresh state is locked".to_string())?;
    let path = kimi_refresh_state_path()?;
    let mut state = if path.exists() {
        load_json_with_recovery::<KimiRefreshState>(&path, "Basiliskos Kimi refresh state")?
    } else {
        KimiRefreshState::default()
    };
    if required {
        state.relogin_required.insert(file_name.to_string());
    } else {
        state.relogin_required.remove(file_name);
    }
    let bytes =
        serde_json::to_vec_pretty(&state).map_err(|_| "Could not save Kimi refresh state")?;
    durable_write(&path, &bytes)
}

fn kimi_refresh_error_requires_relogin(status: reqwest::StatusCode, code: Option<&str>) -> bool {
    status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
        || matches!(code, Some("invalid_grant") | Some("invalid_token"))
}

fn load_xai_refresh_state() -> Result<XaiRefreshState, String> {
    let path = xai_refresh_state_path()?;
    if !path.exists() {
        return Ok(XaiRefreshState::default());
    }
    load_json_with_recovery(&path, "Basiliskos Grok refresh state")
}

fn save_xai_refresh_state(state: &XaiRefreshState) -> Result<(), String> {
    let path = xai_refresh_state_path()?;
    let bytes =
        serde_json::to_vec_pretty(state).map_err(|_| "Could not save Grok refresh state")?;
    durable_write(&path, &bytes)
}

fn xai_relogin_required(file_name: &str) -> Result<bool, String> {
    let lock = XAI_REFRESH_STATE_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| "Basiliskos Grok refresh state is locked".to_string())?;
    Ok(load_xai_refresh_state()?
        .relogin_required
        .contains(file_name))
}

fn set_xai_relogin_required(file_name: &str, required: bool) -> Result<(), String> {
    let lock = XAI_REFRESH_STATE_LOCK.get_or_init(|| Mutex::new(()));
    let _guard = lock
        .lock()
        .map_err(|_| "Basiliskos Grok refresh state is locked".to_string())?;
    let mut state = load_xai_refresh_state()?;
    if required {
        state.relogin_required.insert(file_name.to_string());
    } else {
        state.relogin_required.remove(file_name);
    }
    save_xai_refresh_state(&state)
}

fn xai_refresh_error_requires_relogin(code: Option<&str>) -> bool {
    matches!(
        code,
        Some("invalid_grant") | Some("refresh_token_invalidated")
    )
}

fn config_path() -> Result<PathBuf, String> {
    Ok(gateway_dir()?.join("config.yaml"))
}

pub(crate) fn runtime_exe_path() -> Result<PathBuf, String> {
    Ok(gateway_dir()?.join("bin").join("cli-proxy-api.exe"))
}

fn secure_files_in(directory: &Path, extension: &str) -> Result<(), String> {
    if !directory.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("Could not inspect {}: {error}", directory.display()))?
    {
        let path = entry
            .map_err(|error| format!("Could not inspect a private file: {error}"))?
            .path();
        if path.is_file() && path.extension().and_then(|value| value.to_str()) == Some(extension) {
            secure_existing_path(&path)?;
        }
    }
    Ok(())
}

fn remove_private_child_directories(root: &Path) -> Result<usize, String> {
    secure_create_dir_all(root)?;
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| format!("Could not verify {}: {error}", root.display()))?;
    let mut removed = 0;
    for entry in fs::read_dir(&canonical_root)
        .map_err(|error| format!("Could not inspect {}: {error}", canonical_root.display()))?
    {
        let entry =
            entry.map_err(|error| format!("Could not inspect a stale workspace: {error}"))?;
        let file_type = entry
            .file_type()
            .map_err(|error| format!("Could not inspect a stale workspace type: {error}"))?;
        if !file_type.is_dir() || file_type.is_symlink() {
            continue;
        }
        let path = fs::canonicalize(entry.path())
            .map_err(|error| format!("Could not verify a stale workspace: {error}"))?;
        let relative = path
            .strip_prefix(&canonical_root)
            .map_err(|_| "Refusing to clean a workspace outside Basiliskos")?;
        if relative.components().count() != 1 || path.parent() != Some(canonical_root.as_path()) {
            return Err(format!(
                "Refusing to clean an unexpected workspace path: {}",
                path.display()
            ));
        }
        fs::remove_dir_all(&path)
            .map_err(|error| format!("Could not clean {}: {error}", path.display()))?;
        removed += 1;
    }
    Ok(removed)
}

fn cleanup_stale_secret_workspaces() -> Result<usize, String> {
    let login_removed = remove_private_child_directories(&login_staging_root()?)?;
    let vision_removed = remove_private_child_directories(&gateway_dir()?.join("vision-sidecars"))?;
    let mut deepseek_removed = 0;
    if let Ok(auth_dir) = auth_dir() {
        if let Ok(entries) = std::fs::read_dir(&auth_dir) {
            for entry in entries.flatten() {
                if let Ok(name) = entry.file_name().into_string() {
                    if name.starts_with("deepseek-")
                        && name.ends_with(".json")
                        && std::fs::remove_file(entry.path()).is_ok()
                    {
                        deepseek_removed += 1;
                    }
                }
            }
        }
    }
    Ok(login_removed + vision_removed + deepseek_removed)
}

pub fn initialize_controller_storage() -> Result<(), String> {
    let _mutation = mutation_lock()?;
    let root = root_dir()?;
    let gateway = gateway_dir()?;
    let auth = auth_dir()?;
    let controller_logs = gateway.join("controller-logs");
    let login_staging = login_staging_root()?;
    let vision_sidecars = gateway.join("vision-sidecars");
    let claude_profile = isolated_claude_profile_dir()?;
    let claude_logs = claude_profile.join("Basiliskos Logs");
    for directory in [
        &root,
        &gateway,
        &auth,
        &controller_logs,
        &login_staging,
        &vision_sidecars,
        &claude_profile,
        &claude_logs,
    ] {
        secure_create_dir_all(directory)?;
    }
    recover_pending_transactions(&root)?;
    cleanup_stale_secret_workspaces()?;
    let state_file = controller_path()?;
    let labels_file = account_labels_path()?;
    let config_file = config_path()?;
    let xai_refresh_state_file = xai_refresh_state_path()?;
    let kimi_refresh_state_file = kimi_refresh_state_path()?;
    for file in [
        &state_file,
        &labels_file,
        &config_file,
        &xai_refresh_state_file,
        &kimi_refresh_state_file,
    ] {
        secure_existing_path(file)?;
    }
    for json_file in [
        &state_file,
        &labels_file,
        &xai_refresh_state_file,
        &kimi_refresh_state_file,
    ] {
        if let Ok(bytes) = fs::read(json_file) {
            if serde_json::from_slice::<Value>(&bytes).is_ok() {
                durable_write(json_file, &bytes)?;
            }
        }
    }
    if let Ok(bytes) = fs::read(&config_file) {
        durable_write(&config_file, &bytes)?;
    }
    secure_files_in(&auth, "json")?;
    secure_files_in(&controller_logs, "log")?;
    secure_files_in(&claude_logs, "log")?;
    start_xai_credential_maintenance();
    start_kimi_credential_maintenance();
    start_oauth_credential_maintenance();
    Ok(())
}

fn load_state() -> Result<ControllerState, String> {
    let path = controller_path()?;
    if path.exists() || crate::persistence::backup_path(&path)?.exists() {
        let state = load_json_with_recovery(&path, "Basiliskos controller state")?;
        return Ok(migrate_controller_state(state));
    }
    let state = ControllerState {
        api_key: format!(
            "hydra-{}{}",
            Uuid::new_v4().simple(),
            Uuid::new_v4().simple()
        ),
        claude_config_id: Uuid::new_v4().to_string(),
        previous_claude_applied_id: None,
        active_account: None,
        active_codex_account: None,
        routes: default_routes(),
        codex_routes: default_routes(),
        claude_window_icon: default_claude_window_icon(),
        skip_model_switch_confirmation: false,
        open_claude_on_launch: default_open_claude_on_launch(),
    };
    save_state(&state)?;
    Ok(state)
}

fn save_state(state: &ControllerState) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("Could not serialize controller state: {error}"))?;
    durable_write(&controller_path()?, &bytes)
}

/// The relay's own API key (the `hydra-...` credential clients use to
/// authenticate against the front proxy). Shared by client integrations that
/// point third-party tools at `127.0.0.1:8317/v1`.
fn load_account_labels() -> Result<BTreeMap<String, String>, String> {
    let path = account_labels_path()?;
    if !path.exists() && !crate::persistence::backup_path(&path)?.exists() {
        return Ok(BTreeMap::new());
    }
    load_json_with_recovery(&path, "Basiliskos profile names")
}

fn save_account_labels(labels: &BTreeMap<String, String>) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(labels)
        .map_err(|error| format!("Could not serialize profile names: {error}"))?;
    durable_write(&account_labels_path()?, &bytes)
}

fn load_hidden_models() -> Result<BTreeSet<String>, String> {
    let path = hidden_models_path()?;
    if !path.exists() && !crate::persistence::backup_path(&path)?.exists() {
        return Ok(BTreeSet::new());
    }
    load_json_with_recovery(&path, "Basiliskos hidden models")
}

fn save_hidden_models(hidden: &BTreeSet<String>) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(hidden)
        .map_err(|error| format!("Could not serialize hidden models: {error}"))?;
    durable_write(&hidden_models_path()?, &bytes)
}

fn normalized_account_label(name: &str) -> Result<String, String> {
    let label = name.trim();
    if label.is_empty() {
        return Err("Profile name cannot be empty".into());
    }
    if label.chars().count() > 64 {
        return Err("Profile name must be 64 characters or fewer".into());
    }
    Ok(label.to_string())
}

pub(crate) fn sha256_file(path: &Path) -> Result<String, String> {
    let mut file = fs::File::open(path)
        .map_err(|error| format!("Could not open {}: {error}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn verified_source_exe(app: &AppHandle) -> Result<PathBuf, String> {
    let mut candidates = Vec::new();
    if let Ok(value) = std::env::var("HYDRA_GATEWAY_PROXY_EXE") {
        candidates.push(PathBuf::from(value));
    }
    if let Ok(resource) = app.path().resource_dir() {
        candidates.push(resource.join("resources/gateway/cli-proxy-api.exe"));
        candidates.push(resource.join("gateway/cli-proxy-api.exe"));
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/gateway/cli-proxy-api.exe"),
    );
    let candidate = candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            "The bundled gateway runtime is missing. Reinstall Basiliskos.".to_string()
        })?;
    let actual = sha256_file(&candidate)?;
    if actual != GATEWAY_EXE_SHA256 {
        return Err("The bundled gateway runtime failed its integrity check.".into());
    }
    Ok(candidate)
}

fn prepare_runtime(app: &AppHandle) -> Result<PathBuf, String> {
    let destination = runtime_exe_path()?;
    if destination.exists() && sha256_file(&destination)? == GATEWAY_EXE_SHA256 {
        return Ok(destination);
    }
    let source = verified_source_exe(app)?;
    let bytes = fs::read(&source)
        .map_err(|error| format!("Could not read the bundled gateway runtime: {error}"))?;
    durable_write(&destination, &bytes)
        .map_err(|error| format!("Could not install the gateway runtime: {error}"))?;
    if sha256_file(&destination)? != GATEWAY_EXE_SHA256 {
        return Err("The installed gateway runtime failed its integrity check.".into());
    }
    Ok(destination)
}

pub(crate) fn yaml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "/").replace('"', "\\\""))
}

fn runtime_plugins_dir() -> Result<PathBuf, String> {
    Ok(gateway_dir()?.join("plugins"))
}

/// Installs the bundled Codex compaction plugin next to the gateway runtime so
/// CLIProxyAPI can load it (remote-compaction-v2 for routed models).
fn prepare_codex_compaction_plugin(app: &AppHandle) -> Result<(), String> {
    let dir = runtime_plugins_dir()?;
    secure_create_dir_all(&dir)?;
    let destination = dir.join("basiliskos-codex-compaction.dll");
    if destination.exists() {
        // Simple size sanity; the plugin is not a security boundary.
        if fs::metadata(&destination).map(|m| m.len()).unwrap_or(0) > 100_000 {
            return Ok(());
        }
    }
    let mut candidates = Vec::new();
    if let Ok(resource) = app.path().resource_dir() {
        candidates.push(resource.join("resources/gateway/plugins/basiliskos-codex-compaction.dll"));
        candidates.push(resource.join("gateway/plugins/basiliskos-codex-compaction.dll"));
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/gateway/plugins/basiliskos-codex-compaction.dll"),
    );
    let source = candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "The bundled Codex compaction plugin is missing.".to_string())?;
    let bytes = fs::read(&source)
        .map_err(|error| format!("Could not read the bundled Codex compaction plugin: {error}"))?;
    durable_write(&destination, &bytes)
        .map_err(|error| format!("Could not install the Codex compaction plugin: {error}"))?;
    Ok(())
}

fn active_provider_from_auth_for(state: &ControllerState, client: ClientSurface) -> Option<String> {
    let file_name = account_file_name(active_account_for(state, client))?;
    let raw = fs::read_to_string(exact_account_path(file_name).ok()?).ok()?;
    let value = serde_json::from_str::<Value>(&raw).ok()?;
    account_provider(&value, file_name)
}

/// Emit the `openai-compatibility` provider list for every enabled API-key-only
/// account (DeepSeek, routers, custom endpoints). Verified against the pinned
/// 7.2.139 `config.example.yaml`: the block is a LIST under `openai-compatibility:`
/// with `name`, `base-url`, `api-key-entries` (an object list), and an explicit
/// `models` list. OAuth providers used by key are routed through their native
/// sections (`claude-api-key`, `xai-api-key`, `codex-api-key`, `gemini-api-key`)
/// and are not emitted here.
fn render_api_key_provider_blocks(auth: &Path) -> String {
    let _ = auth;
    let Ok(directory) = keys_dir() else {
        return String::new();
    };
    let mut providers: Vec<(String, String, String)> = Vec::new();
    collect_compat_accounts(&mut providers, &directory, true);
    if let Ok(auth_directory) = auth_dir() {
        collect_compat_accounts(&mut providers, &auth_directory, false);
    }
    if providers.is_empty() {
        return String::new();
    }
    let mut out = String::from("\nopenai-compatibility:\n");
    for (provider, base_url, key) in providers {
        let model_ids: Vec<String> = model_specs(&provider)
            .iter()
            .map(|spec| spec.id.to_string())
            .collect();
        out.push_str(&openai_compat_provider_yaml(
            &provider, &base_url, &key, &model_ids,
        ));
    }
    out
}

fn collect_compat_accounts(
    providers: &mut Vec<(String, String, String)>,
    directory: &Path,
    api_key_only: bool,
) {
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let file_name = entry.file_name().to_string_lossy().to_string();
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if value
            .get("disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        let Some(provider) = account_provider(&value, &file_name) else {
            continue;
        };
        let include = if api_key_only {
            crate::catalog::API_KEY_PROVIDERS.contains(&provider.as_str()) || provider == "zai"
        } else {
            provider == "zai"
        };
        if !include {
            continue;
        }
        let Some(key) = nested_string(&value, &["api_key"])
            .or_else(|| nested_string(&value, &["access_token"]))
        else {
            continue;
        };
        let base_url = nested_string(&value, &["base_url"])
            .or_else(|| default_api_base_url(&provider).map(str::to_string))
            .unwrap_or_default();
        if base_url.is_empty() {
            continue;
        }
        providers.push((provider, base_url, key));
    }
}

/// One `openai-compatibility` list item, matching the pinned CLIProxyAPI
/// `config.example.yaml`: `name`, `base-url`, an object list under
/// `api-key-entries`, and an explicit `models` list. Pure and unit-testable so
/// the schema can't drift silently on a CLIProxyAPI bump.
fn openai_compat_provider_yaml(
    provider: &str,
    base_url: &str,
    key: &str,
    model_ids: &[String],
) -> String {
    let mut out = String::new();
    out.push_str(&format!("  - name: {}\n", yaml_quote(provider)));
    out.push_str(&format!("    base-url: {}\n", yaml_quote(base_url)));
    out.push_str("    api-key-entries:\n");
    out.push_str(&format!("      - api-key: {}\n", yaml_quote(key)));
    if !model_ids.is_empty() {
        out.push_str("    models:\n");
        let mut seen = std::collections::BTreeSet::new();
        for id in model_ids {
            if !seen.insert(id.as_str()) {
                continue;
            }
            out.push_str(&format!("      - name: {}\n", yaml_quote(id)));
        }
    }
    out
}

fn render_config(auth: &Path, state: &ControllerState) -> String {
    let api_key_blocks = render_api_key_provider_blocks(auth);
    format!(
        r#"host: "127.0.0.1"
port: {BACKEND_PORT}
remote-management:
  allow-remote: false
  secret-key: ""
  disable-control-panel: true
auth-dir: {auth_dir}
api-keys:
  - {api_key}
debug: false
logging-to-file: true
logs-max-total-size-mb: 20
request-log: false
usage-statistics-enabled: false
passthrough-headers: false
request-retry: 0
max-retry-credentials: 1
nonstream-keepalive-interval: 0
disable-claude-cloak-mode: true
streaming:
  keepalive-seconds: 15
  bootstrap-retries: 0
oauth-model-alias:
  antigravity:
    - name: "gemini-3.6-flash-high"
      alias: "gemini-3.7-flash"
      force-mapping: true
    - name: "gemini-3.1-pro-low"
      alias: "gemini-3.7-pro"
      force-mapping: true
    - name: "gemini-3.1-flash-lite"
      alias: "gemini-3.7-flash-lite"
      force-mapping: true
plugins:
  enabled: true
  dir: "~/.hydra-gateway/gateway/plugins"
  configs:
    basiliskos-codex-compaction:
      enabled: true
# Explicit: upstream default since v7.2.128; keeps it fixed even if the
# upstream default flips. Prevents native x_search injection into Grok
# requests (issue #4339) regardless of client tool declarations.
xai:
  inject-x-search: false
# The Codex Desktop app encrypts Responses request bodies; this switch makes
# CLIProxyAPI remove the message-parameter encryption and normalize the
# requests for upstream providers (verified 7.2.128 behavior).
codex:
  optimize-multi-agent-v2: true
# API-key provider accounts (DeepSeek, routers, custom endpoints). Best-effort
# and YAML-valid; verify field names against CLIProxyAPI 7.2.139 before relying
# on live API-key routing.
{api_key_blocks}"#,
        auth_dir = yaml_quote(&auth.to_string_lossy()),
        api_key = yaml_quote(&state.api_key),
        api_key_blocks = api_key_blocks,
    )
}

fn prepare_config() -> Result<ControllerState, String> {
    let state = load_state()?;
    let auth = auth_dir()?;
    secure_create_dir_all(&auth)?;
    durable_write(&config_path()?, render_config(&auth, &state).as_bytes())?;
    Ok(state)
}

pub(crate) fn endpoint_health_check(port: u16, path: &str, api_key: &str, marker: &str) -> bool {
    let address = ("127.0.0.1", port)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| addresses.next());
    let Some(address) = address else { return false };
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(300)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nx-api-key: {api_key}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = String::new();
    stream.read_to_string(&mut response).is_ok()
        && (response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200"))
        && response.contains(marker)
}

fn backend_health_check(api_key: &str) -> bool {
    endpoint_health_check(BACKEND_PORT, "/v1/models", api_key, "\"data\"")
}

fn backend_model_ids(api_key: &str) -> Result<Vec<String>, String> {
    let address = ("127.0.0.1", BACKEND_PORT)
        .to_socket_addrs()
        .map_err(|_| "Backend address is unavailable")?
        .next()
        .ok_or("Backend address is unavailable")?;
    let mut stream = TcpStream::connect_timeout(&address, Duration::from_millis(500))
        .map_err(|_| "Backend model catalog is unavailable")?;
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .map_err(|_| "Backend model catalog timeout could not be configured")?;
    let request = format!(
        "GET /v1/models HTTP/1.1\r\nHost: 127.0.0.1\r\nx-api-key: {api_key}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|_| "Backend model catalog request failed")?;
    let mut bytes = Vec::new();
    stream
        .take(2 * 1024 * 1024)
        .read_to_end(&mut bytes)
        .map_err(|_| "Backend model catalog response failed")?;
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("Backend model catalog response is malformed")?;
    let value: Value = serde_json::from_slice(&bytes[split + 4..])
        .map_err(|_| "Backend model catalog JSON is malformed")?;
    Ok(value
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.get("id").and_then(Value::as_str).map(str::to_owned))
        .collect())
}

// Best-effort: refreshes the cached "what does the backend actually report as
// available" list for a provider. Never fails the caller Ã¢â‚¬â€ if the backend is
// unreachable or returns nothing, the previous cached catalog (or no catalog,
// meaning no live filtering) is left in place.
fn refresh_model_catalog_cache(provider: &str, api_key: &str) {
    if let Ok(models) = backend_model_ids(api_key) {
        if !models.is_empty() {
            if let Ok(mut runtime) = runtime_lock() {
                runtime
                    .last_known_model_catalog
                    .insert(provider.to_string(), models);
            }
        }
    }
}

// Keeps a model visible if it isn't manually hidden AND (when a live catalog
// is known for this provider) the backend actually reports it as available.
// The currently-selected model is always kept visible regardless of either
// filter, so an existing route selection never disappears out from under it.
fn filter_visible_models<'a>(
    provider: &str,
    specs: &'a [ModelSpec],
    selected_id: &str,
    hidden: &BTreeSet<String>,
    live_catalog: Option<&[String]>,
) -> Vec<&'a ModelSpec> {
    specs
        .iter()
        .filter(|spec| spec.id == selected_id || !hidden.contains(spec.id))
        .filter(|spec| {
            let backend_id = backend_model_identifier(provider, spec.id);
            spec.id == selected_id
                || live_catalog
                    .is_none_or(|live| live.iter().any(|id| id == spec.id || id == backend_id))
        })
        .collect()
}

fn validated_route_for_request(
    state: &ControllerState,
    provider: &str,
    correlation_id: &str,
) -> RouteSelection {
    let selected = normalized_route(state, provider);
    let Ok(models) = backend_model_ids(&state.api_key) else {
        return selected;
    };
    let target_backend_model = backend_model_identifier(provider, &selected.model);
    if models.is_empty()
        || models
            .iter()
            .any(|model| model == target_backend_model || model == &selected.model)
    {
        if let Ok(mut runtime) = runtime_lock() {
            runtime
                .last_known_good_models
                .insert(provider.to_owned(), selected.model.clone());
        }
        return selected;
    }
    let fallback = runtime_lock()
        .ok()
        .and_then(|runtime| runtime.last_known_good_models.get(provider).cloned())
        .filter(|model| {
            let target = backend_model_identifier(provider, model);
            models.iter().any(|m| m == target || m == model)
        })
        .or_else(|| {
            model_specs(provider)
                .iter()
                .find(|spec| {
                    let target = backend_model_identifier(provider, spec.id);
                    models
                        .iter()
                        .any(|model| model == target || model == spec.id)
                })
                .map(|spec| spec.id.to_owned())
        });
    let Some(model) = fallback else {
        return selected;
    };
    diagnostics::record(
        ErrorCode::ModelFallback,
        "warning",
        "The selected model is unavailable for this credential; the last known good model is being used.",
        Some(correlation_id),
        None,
        Some(provider),
    );
    let thinking = model_specs(provider)
        .iter()
        .find(|spec| spec.id == model)
        .filter(|spec| {
            selected.thinking == "auto"
                || spec.thinking_levels.contains(&selected.thinking.as_str())
        })
        .map(|_| selected.thinking)
        .unwrap_or_else(|| "auto".into());
    RouteSelection { model, thinking }
}

fn health_check(api_key: &str) -> bool {
    endpoint_health_check(GATEWAY_PORT, "/hydra/health", api_key, "\"backend\":true")
        && backend_health_check(api_key)
}

fn port_is_available(port: u16) -> bool {
    TcpListener::bind(("127.0.0.1", port)).is_ok()
}

#[cfg(target_os = "windows")]
fn same_windows_path(left: &Path, right: &Path) -> bool {
    left.to_string_lossy()
        .replace('/', "\\")
        .eq_ignore_ascii_case(&right.to_string_lossy().replace('/', "\\"))
}

#[cfg(target_os = "windows")]
fn terminate_stale_managed_backends(expected_executable: &Path) -> Result<usize, String> {
    use std::mem::size_of;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, INVALID_HANDLE_VALUE},
        System::{
            Diagnostics::ToolHelp::{
                CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
                TH32CS_SNAPPROCESS,
            },
            Threading::{
                OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
            },
        },
    };

    let snapshot = unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) };
    if snapshot == INVALID_HANDLE_VALUE {
        return Err(format!(
            "Could not inspect stale Basiliskos backends: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..Default::default()
    };
    let mut terminated = 0;
    let mut has_entry = unsafe { Process32FirstW(snapshot, &mut entry) } != 0;
    while has_entry {
        let process = unsafe {
            OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
                0,
                entry.th32ProcessID,
            )
        };
        if !process.is_null() {
            let mut buffer = vec![0_u16; 32_768];
            let mut length = buffer.len() as u32;
            let queried =
                unsafe { QueryFullProcessImageNameW(process, 0, buffer.as_mut_ptr(), &mut length) }
                    != 0;
            if queried {
                let actual = PathBuf::from(String::from_utf16_lossy(&buffer[..length as usize]));
                if same_windows_path(&actual, expected_executable) {
                    if unsafe { TerminateProcess(process, 0) } == 0 {
                        let error = std::io::Error::last_os_error();
                        unsafe { CloseHandle(process) };
                        unsafe { CloseHandle(snapshot) };
                        return Err(format!(
                            "Could not terminate stale Basiliskos backend {}: {error}",
                            entry.th32ProcessID
                        ));
                    }
                    let _ = unsafe { WaitForSingleObject(process, 3_000) };
                    terminated += 1;
                }
            }
            unsafe { CloseHandle(process) };
        }
        has_entry = unsafe { Process32NextW(snapshot, &mut entry) } != 0;
    }
    unsafe { CloseHandle(snapshot) };
    Ok(terminated)
}

#[cfg(not(target_os = "windows"))]
fn terminate_stale_managed_backends(_expected_executable: &Path) -> Result<usize, String> {
    Ok(0)
}

fn rewrite_claude_request(
    body: &mut Value,
    state: &ControllerState,
    provider: &str,
    inject_identity: bool,
) -> Result<(), String> {
    let object = body
        .as_object_mut()
        .ok_or_else(|| "Claude request body must be a JSON object".to_string())?;
    // A model the client picked in Claude's picker (advertised via
    // `inferenceModels`) wins over the Basiliskos route selection; the picker
    // carries the model only. Thinking comes from Claude's native effort
    // control in the window (`output_config.effort`), which wins over the
    // route's saved thinking.
    let hidden = load_hidden_models().unwrap_or_default();
    let route = normalized_route(state, provider);
    let chosen = client_picker_choice(object, provider, &hidden, &route.model);
    let base_model = chosen
        .as_ref()
        .map(|(model, _)| model.as_str())
        .unwrap_or(&route.model);
    let client_effort = client_effort_choice(object);
    let thinking_override = client_effort.as_deref().filter(|level| {
        *level == "auto"
            || (provider == "xai" && base_model == "grok-4.5")
            || model_specs(provider).iter().any(|spec| {
                spec.id == base_model && spec.thinking_levels.contains(&level.to_string().as_str())
            })
    });
    let routed_model = apply_route_model(base_model, thinking_override, object, state, provider);
    object.insert("model".into(), Value::String(routed_model));

    for fixup in tool_compatibility_fixups(provider) {
        fixup(object);
    }

    if !inject_identity {
        return Ok(());
    }

    let identity = serde_json::json!({
        "type": "text",
        "text": route_identity_prompt(state, provider)
    });
    match object.remove("system") {
        Some(Value::Array(mut blocks)) => {
            blocks.push(identity);
            object.insert("system".into(), Value::Array(blocks));
        }
        Some(Value::String(text)) => {
            object.insert(
                "system".into(),
                Value::Array(vec![
                    serde_json::json!({"type": "text", "text": text}),
                    identity,
                ]),
            );
        }
        Some(Value::Null) | None => {
            object.insert("system".into(), Value::Array(vec![identity]));
        }
        Some(other) => {
            return Err(format!(
                "Claude request system field has unsupported type: {other}"
            ));
        }
    }
    Ok(())
}

fn is_hop_by_hop_header(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "connection"
            | "content-length"
            | "host"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

pub(crate) fn secure_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn extract_bearer_tokens_from_file(path: &Path, tokens: &mut Vec<String>) {
    let Ok(raw) = fs::read_to_string(path) else {
        return;
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return;
    };
    extract_bearer_tokens_from_value(&value, tokens);
}

fn extract_bearer_tokens_from_value(value: &Value, tokens: &mut Vec<String>) {
    for field in [
        "access_token",
        "accessToken",
        "token",
        "api_key",
        "apiKey",
        "key",
    ] {
        if let Some(token) = value.get(field).and_then(Value::as_str) {
            let token = token.trim();
            if !token.is_empty() {
                tokens.push(token.to_string());
            }
        }
    }
    if let Some(tokens_obj) = value.get("tokens").and_then(Value::as_object) {
        for field in ["access_token", "accessToken", "token", "id_token"] {
            if let Some(token) = tokens_obj.get(field).and_then(Value::as_str) {
                let token = token.trim();
                if !token.is_empty() {
                    tokens.push(token.to_string());
                }
            }
        }
    }
}

fn collect_authorized_tokens(api_key: &str) -> Vec<String> {
    let mut valid_tokens = vec![api_key.to_string()];
    if let Ok(isolated_home) = isolated_codex_home() {
        extract_bearer_tokens_from_file(&isolated_home.join("auth.json"), &mut valid_tokens);
    }
    extract_bearer_tokens_from_file(&crate::codex_cli::real_codex_auth_path(), &mut valid_tokens);
    if let Ok(auth) = auth_dir() {
        if let Ok(entries) = fs::read_dir(auth) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|ext| ext.to_str()) == Some("json") {
                    extract_bearer_tokens_from_file(&path, &mut valid_tokens);
                }
            }
        }
    }
    valid_tokens
}

fn request_is_authorized(request: &tiny_http::Request, api_key: &str) -> bool {
    let valid_tokens = collect_authorized_tokens(api_key);
    request.headers().iter().any(|header| {
        let name = header.field.as_str().as_str();
        let value = header.value.as_str().trim();
        (name.eq_ignore_ascii_case("x-api-key")
            && valid_tokens.iter().any(|valid| secure_eq(value, valid)))
            || (name.eq_ignore_ascii_case("authorization")
                && if value.len() >= 7 && value[..7].eq_ignore_ascii_case("bearer ") {
                    let token = value[7..].trim();
                    valid_tokens.iter().any(|valid| secure_eq(token, valid))
                } else {
                    false
                })
    })
}

fn request_headers_within_budget(request: &tiny_http::Request) -> bool {
    request.headers().len() <= MAX_RELAY_HEADERS
        && request.headers().iter().fold(0_usize, |total, header| {
            total
                .saturating_add(header.field.as_str().as_str().len())
                .saturating_add(header.value.as_str().len())
        }) <= MAX_RELAY_HEADER_BYTES
}

fn proxy_error(
    code: ErrorCode,
    status: u16,
    message: &str,
    correlation_id: &str,
) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::json!({
        "type": "error",
        "error": {
            "type": code.as_str(),
            "message": message,
            "correlation_id": correlation_id
        }
    })
    .to_string()
    .into_bytes();
    let mut response = Response::from_data(body).with_status_code(StatusCode(status));
    if let Ok(header) = Header::from_bytes("content-type", "application/json") {
        response.add_header(header);
    }
    if let Ok(header) = Header::from_bytes("x-basiliskos-correlation-id", correlation_id) {
        response.add_header(header);
    }
    if let Ok(header) = Header::from_bytes("x-basiliskos-error-code", code.as_str()) {
        response.add_header(header);
    }
    response
}

fn respond_proxy_error(
    request: tiny_http::Request,
    code: ErrorCode,
    status: u16,
    message: &'static str,
    correlation_id: &str,
) {
    diagnostics::record(
        code,
        if status >= 500 { "error" } else { "warning" },
        message,
        Some(correlation_id),
        Some(status),
        None,
    );
    let _ = request.respond(proxy_error(code, status, message, correlation_id));
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum StreamFailure {
    MidstreamIdle,
    UpstreamEnded,
}

pub(crate) struct TrackedUpstream {
    pub(crate) receiver: tokio::sync::mpsc::Receiver<Result<Bytes, StreamFailure>>,
    pub(crate) current: Option<Bytes>,
    pub(crate) offset: usize,
    pub(crate) correlation_id: String,
    pub(crate) provider: Option<String>,
}

impl Read for TrackedUpstream {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            if let Some(current) = self.current.as_ref() {
                let remaining = &current[self.offset..];
                if !remaining.is_empty() {
                    let count = remaining.len().min(buffer.len());
                    buffer[..count].copy_from_slice(&remaining[..count]);
                    self.offset += count;
                    if self.offset == current.len() {
                        self.current = None;
                        self.offset = 0;
                    }
                    return Ok(count);
                }
            }
            match self.receiver.blocking_recv() {
                Some(Ok(bytes)) if !bytes.is_empty() => self.current = Some(bytes),
                Some(Ok(_)) => continue,
                Some(Err(failure)) => {
                    let (code, message, kind) = match failure {
                        StreamFailure::MidstreamIdle => (
                            ErrorCode::MidstreamIdleTimeout,
                            "The upstream stream exceeded its idle time budget.",
                            std::io::ErrorKind::TimedOut,
                        ),
                        StreamFailure::UpstreamEnded => (
                            ErrorCode::BackendConnectFailed,
                            "The upstream stream ended unexpectedly.",
                            std::io::ErrorKind::ConnectionAborted,
                        ),
                    };
                    diagnostics::record(
                        code,
                        "error",
                        message,
                        Some(&self.correlation_id),
                        None,
                        self.provider.as_deref(),
                    );
                    return Err(std::io::Error::new(kind, code.as_str()));
                }
                None => return Ok(0),
            }
        }
    }
}

pub(crate) struct UpstreamMeta {
    pub(crate) status: u16,
    headers: Vec<(String, Vec<u8>)>,
    pub(crate) body: tokio::sync::mpsc::Receiver<Result<Bytes, StreamFailure>>,
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum FirstResponseFailure {
    Timeout,
    Connect,
}

pub(crate) fn begin_upstream_request(
    runtime: &tokio::runtime::Handle,
    client: reqwest::Client,
    method: reqwest::Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> Result<UpstreamMeta, FirstResponseFailure> {
    begin_upstream_request_with_timeouts(
        runtime,
        client,
        method,
        url,
        headers,
        body,
        FIRST_RESPONSE_TIMEOUT,
        STREAM_IDLE_TIMEOUT,
    )
}

#[allow(clippy::too_many_arguments)]
fn begin_upstream_request_with_timeouts(
    runtime: &tokio::runtime::Handle,
    client: reqwest::Client,
    method: reqwest::Method,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    first_response_timeout: Duration,
    stream_idle_timeout: Duration,
) -> Result<UpstreamMeta, FirstResponseFailure> {
    let (meta_tx, meta_rx) = mpsc::sync_channel(1);
    runtime.spawn(async move {
        let mut builder = client.request(method, url);
        for (name, value) in headers {
            builder = builder.header(name, value);
        }
        let response =
            match tokio::time::timeout(first_response_timeout, builder.body(body).send()).await {
                Ok(Ok(response)) => response,
                Ok(Err(_)) => {
                    let _ = meta_tx.send(Err(FirstResponseFailure::Connect));
                    return;
                }
                Err(_) => {
                    let _ = meta_tx.send(Err(FirstResponseFailure::Timeout));
                    return;
                }
            };
        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_owned(), value.as_bytes().to_vec()))
            .collect();
        let (body_tx, body_rx) = tokio::sync::mpsc::channel(8);
        if meta_tx
            .send(Ok(UpstreamMeta {
                status,
                headers,
                body: body_rx,
            }))
            .is_err()
        {
            return;
        }
        let mut stream = response.bytes_stream();
        loop {
            match tokio::time::timeout(stream_idle_timeout, stream.next()).await {
                Ok(Some(Ok(bytes))) => {
                    if body_tx.send(Ok(bytes)).await.is_err() {
                        return;
                    }
                }
                Ok(Some(Err(_))) => {
                    let _ = body_tx.send(Err(StreamFailure::UpstreamEnded)).await;
                    return;
                }
                Ok(None) => return,
                Err(_) => {
                    let _ = body_tx.send(Err(StreamFailure::MidstreamIdle)).await;
                    return;
                }
            }
        }
    });
    meta_rx
        .recv_timeout(first_response_timeout.saturating_add(Duration::from_secs(1)))
        .unwrap_or(Err(FirstResponseFailure::Timeout))
}

fn request_surface(path: &str) -> Option<ClientSurface> {
    if path == "/v1/messages" || path == "/v1/messages/count_tokens" {
        Some(ClientSurface::Claude)
    } else if path == "/v1/chat/completions"
        || path == "/v1/responses"
        || path.starts_with("/v1/responses/")
    {
        Some(ClientSurface::Codex)
    } else {
        None
    }
}

fn classify_upstream_status(status: u16) -> Option<ErrorCode> {
    match status {
        402 => Some(ErrorCode::ProviderQuotaExhausted),
        401..=403 => Some(ErrorCode::ProviderAuthFailed),
        429 => Some(ErrorCode::ProviderRateLimited),
        500..=599 => Some(ErrorCode::UpstreamServerError),
        _ => None,
    }
}

// Accepts either delay-seconds ("120") or an HTTP-date ("Sun, 06 Nov 1994
// 08:49:37 GMT"). Returns None for a missing, malformed, or already-past value
// so callers can fall back to a fixed default cooldown.
fn parse_retry_after_seconds(value: &str) -> Option<i64> {
    let trimmed = value.trim();
    if let Ok(seconds) = trimmed.parse::<i64>() {
        return (seconds > 0).then_some(seconds);
    }
    let parsed =
        chrono::NaiveDateTime::parse_from_str(trimmed, "%a, %d %b %Y %H:%M:%S GMT").ok()?;
    let remaining = parsed
        .and_utc()
        .signed_duration_since(Utc::now())
        .num_seconds();
    (remaining > 0).then_some(remaining)
}

fn provider_auth_failure_message(provider: Option<&str>, status: u16) -> &'static str {
    match (provider, status) {
        (Some("kimi"), 402 | 403) => "This Kimi account has no active Kimi Code subscription.",
        (_, 401) => "The provider rejected the selected credential. Sign in again.",
        _ => "The provider rejected the selected credential.",
    }
}

fn health_response(api_key: &str, correlation_id: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let backend_ready = backend_health_check(api_key);
    let mut response = Response::from_string(
        serde_json::json!({
            "hydra": true,
            "backend": backend_ready,
            "version": env!("CARGO_PKG_VERSION"),
            "correlation_id": correlation_id
        })
        .to_string(),
    )
    .with_status_code(if backend_ready {
        StatusCode(200)
    } else {
        StatusCode(503)
    });
    if let Ok(header) = Header::from_bytes("content-type", "application/json") {
        response.add_header(header);
    }
    if let Ok(header) = Header::from_bytes("x-basiliskos-correlation-id", correlation_id) {
        response.add_header(header);
    }
    response
}

fn upstream_input_token_count(
    async_runtime: &tokio::runtime::Handle,
    client: &reqwest::Client,
    headers: &[(String, String)],
    body: &[u8],
    correlation_id: &str,
    provider: Option<&str>,
) -> Result<u64, String> {
    let upstream = begin_upstream_request(
        async_runtime,
        client.clone(),
        reqwest::Method::POST,
        format!("http://127.0.0.1:{BACKEND_PORT}/v1/messages/count_tokens?beta=true"),
        headers.to_vec(),
        body.to_vec(),
    )
    .map_err(|_| "The local backend could not count the request tokens.".to_string())?;
    if upstream.status != 200 {
        return Err("The local backend could not count the request tokens.".into());
    }
    let mut response = TrackedUpstream {
        receiver: upstream.body,
        current: None,
        offset: 0,
        correlation_id: correlation_id.to_owned(),
        provider: provider.map(str::to_owned),
    };
    let mut bytes = Vec::new();
    response
        .by_ref()
        .take((MAX_CONTEXT_COUNT_RESPONSE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| "The local backend token count response was incomplete.".to_string())?;
    if bytes.len() > MAX_CONTEXT_COUNT_RESPONSE_BYTES {
        return Err("The local backend token count response was too large.".into());
    }
    serde_json::from_slice::<Value>(&bytes)
        .ok()
        .and_then(|value| value.get("input_tokens").and_then(Value::as_u64))
        .ok_or_else(|| "The local backend token count response was invalid.".into())
}

fn handle_front_proxy_request(
    mut request: tiny_http::Request,
    client: &reqwest::Client,
    async_runtime: &tokio::runtime::Handle,
    api_key: &str,
    correlation_id: &str,
) {
    let request_url = request.url().to_string();
    let request_path = request_url
        .split('?')
        .next()
        .unwrap_or(request_url.as_str());
    let surface = request_surface(request_path);

    // Codex Desktop prefers the Responses WebSocket transport. The Basiliskos
    // relay is HTTP/SSE only, so a WebSocket upgrade would hang forever in
    // retries. Return 426 Upgrade Required so the client falls back to
    // HTTP/SSE Ã¢â‚¬â€ the same signal opencodex uses for the Codex app.
    let is_websocket_upgrade = request.method() == &tiny_http::Method::Get
        && request.headers().iter().any(|header| {
            header
                .field
                .as_str()
                .as_str()
                .eq_ignore_ascii_case("upgrade")
                && header.value.as_str().eq_ignore_ascii_case("websocket")
        });
    if is_websocket_upgrade && request_path.starts_with("/v1/responses") {
        diagnostics::record(
            ErrorCode::RequestInvalid,
            "info",
            "Codex WebSocket upgrade refused (426); client falls back to HTTP/SSE.",
            Some(correlation_id),
            Some(426),
            None,
        );
        let _ = request.respond(
            tiny_http::Response::from_string("Upgrade Required - use HTTP/SSE".to_string())
                .with_status_code(tiny_http::StatusCode(426)),
        );
        return;
    }
    let method = match reqwest::Method::from_bytes(request.method().as_str().as_bytes()) {
        Ok(method) => method,
        Err(error) => {
            let _ = error;
            respond_proxy_error(
                request,
                ErrorCode::RequestInvalid,
                400,
                "The request method is not supported.",
                correlation_id,
            );
            return;
        }
    };
    if request
        .body_length()
        .is_some_and(|length| length > MAX_RELAY_BODY_BYTES)
    {
        respond_proxy_error(
            request,
            ErrorCode::RequestBodyTooLarge,
            413,
            "The request body exceeds the 8 MiB Basiliskos limit.",
            correlation_id,
        );
        return;
    }
    let mut body = Vec::new();
    let read_result = request
        .as_reader()
        .take((MAX_RELAY_BODY_BYTES + 1) as u64)
        .read_to_end(&mut body);
    if read_result.is_err() {
        respond_proxy_error(
            request,
            ErrorCode::RequestInvalid,
            400,
            "The request body could not be read.",
            correlation_id,
        );
        return;
    }
    if body.len() > MAX_RELAY_BODY_BYTES {
        respond_proxy_error(
            request,
            ErrorCode::RequestBodyTooLarge,
            413,
            "The request body exceeds the 8 MiB Basiliskos limit.",
            correlation_id,
        );
        return;
    }

    let mut provider_for_event = None;
    let mut active_account_for_event = None;
    let mut context_budget = None;
    if surface == Some(ClientSurface::Claude) {
        let rewrite_result = (|| -> Result<(), String> {
            let _mutation = mutation_lock()?;
            let mut state = load_state()?;
            let provider = active_provider_from_auth_for(&state, ClientSurface::Claude)
                .ok_or_else(|| "Choose an active Basiliskos account first".to_string())?;
            provider_for_event = Some(provider.clone());
            active_account_for_event =
                active_account_for(&state, ClientSurface::Claude).map(str::to_owned);
            let validated = validated_route_for_request(&state, &provider, correlation_id);
            state.routes.insert(provider.clone(), validated);
            let mut json: Value = serde_json::from_slice(&body)
                .map_err(|_| "Claude request body is invalid JSON".to_string())?;
            // Persist a client-chosen picker (model + thinking) into the route
            // BEFORE the rewrite, so the identity prompt and the Basiliskos
            // panel are truthful on the very first request after a switch.
            let hidden = load_hidden_models().unwrap_or_default();
            let selected_model = state
                .routes
                .get(&provider)
                .map(|route| route.model.clone())
                .unwrap_or_else(|| default_model(&provider).to_string());
            if let Some((model, thinking)) = json.as_object().and_then(|object| {
                client_picker_choice(object, &provider, &hidden, &selected_model)
            }) {
                let current = state
                    .routes
                    .get(&provider)
                    .map(|route| (route.model.as_str(), route.thinking.as_str()));
                if current != Some((model.as_str(), thinking.as_str())) {
                    let mut updated = state.clone();
                    if let Some(route) = updated.routes.get_mut(&provider) {
                        route.model = model.clone();
                        route.thinking = thinking.clone();
                    }
                    save_state(&updated)?;
                    state = updated;
                    // Regenerate the Claude config so the picker follows the
                    // new selected model's thinking variants.
                    let _ = write_isolated_claude_config(&isolated_claude_profile_dir()?, &state);
                }
            }
            rewrite_claude_request(&mut json, &state, &provider, request_path == "/v1/messages")?;
            if request_path == "/v1/messages" {
                context_budget = context_budget_for_request(&provider, &json);
            }
            body = serde_json::to_vec(&json).map_err(|error| error.to_string())?;
            Ok(())
        })();
        if rewrite_result.is_err() {
            respond_proxy_error(
                request,
                ErrorCode::RequestInvalid,
                400,
                "The protected Claude request is invalid or no active account is selected.",
                correlation_id,
            );
            return;
        }
    }

    // Codex dial: the isolated Codex window talks to the built-in `openai`
    // provider redirected at this relay (`openai_base_url`). Enforce the active
    // Basiliskos route by rewriting the request model to the active route's
    // real upstream id (the renderer allowlist only offers native OpenAI slugs,
    // which cannot map to upstreams directly).
    if surface == Some(ClientSurface::Codex) {
        let content_type = request
            .headers()
            .iter()
            .find(|h| {
                h.field
                    .as_str()
                    .as_str()
                    .eq_ignore_ascii_case("content-type")
            })
            .map(|h| h.value.as_str().to_string())
            .unwrap_or_else(|| "none".into());
        let body_head: String = body
            .iter()
            .take(160)
            .map(|b| *b as char)
            .collect::<String>()
            .replace('\n', " ");
        // The Codex Desktop app ENCRYPTS Responses request bodies; CLIProxyAPI
        // decrypts them (codex.optimize-multi-agent-v2). The dial is a real
        // model switcher: the picked model id routes to its own provider via
        // CLIProxyAPI's catalog (the deepseek compat block only aliases
        // deepseek ids), so no model override happens here. The provider is
        // resolved only for diagnostics and the dial log.
        let rewrite_result = (|| -> Result<(), String> {
            let _mutation = mutation_lock()?;
            let state = load_state()?;
            let provider = active_provider_from_auth_for(&state, ClientSurface::Codex)
                .ok_or_else(|| "Choose an active Basiliskos account first".to_string())?;
            provider_for_event = Some(provider.clone());
            active_account_for_event =
                active_account_for(&state, ClientSurface::Codex).map(str::to_owned);
            Ok(())
        })();
        if let Err(message) = rewrite_result {
            let line = format!(
                "{} | method={} ctype={} body_len={} body_head={} | {message}",
                chrono::Utc::now().to_rfc3339(),
                request.method().as_str(),
                content_type,
                body.len(),
                body_head
            );
            append_codex_dial_log(&line);
            diagnostics::record(
                ErrorCode::RequestInvalid,
                "warning",
                &format!("Codex dial rewrite failed: {message}"),
                Some(correlation_id),
                Some(400),
                None,
            );
            respond_proxy_error(
                request,
                ErrorCode::RequestInvalid,
                400,
                "The Codex request could not be rewritten to the active Basiliskos route.",
                correlation_id,
            );
            return;
        }
        if let Ok(mut json_val) = serde_json::from_slice::<Value>(&body) {
            if let Some(obj) = json_val.as_object_mut() {
                if let Some(model_val) = obj.get_mut("model") {
                    if let Some(model_str) = model_val.as_str() {
                        let mapped = backend_model_identifier("antigravity", model_str);
                        if mapped != model_str {
                            *model_val = Value::String(mapped.to_string());
                            if let Ok(new_body) = serde_json::to_vec(&json_val) {
                                body = new_body;
                            }
                        }
                    }
                }
            }
        }
        append_codex_dial_log(&format!(
            "{} | method={} ctype={} body_len={} body_head={} | OK",
            chrono::Utc::now().to_rfc3339(),
            request.method().as_str(),
            content_type,
            body.len(),
            body_head
        ));
    }

    let upstream_url = format!("http://127.0.0.1:{BACKEND_PORT}{request_url}");
    let mut upstream_headers = Vec::new();
    for header in request.headers() {
        let name = header.field.as_str().as_str();
        if is_hop_by_hop_header(name) {
            continue;
        }
        if name.eq_ignore_ascii_case("authorization") {
            // Forward the normalized Basiliskos key to the backend, which validates Bearer {api_key}.
            upstream_headers.push(("authorization".to_owned(), format!("Bearer {api_key}")));
            continue;
        }
        if name.eq_ignore_ascii_case("x-api-key") {
            upstream_headers.push(("x-api-key".to_owned(), api_key.to_string()));
            continue;
        }
        upstream_headers.push((name.to_owned(), header.value.as_str().to_owned()));
    }
    if let Some(budget) = context_budget {
        let input_tokens = match upstream_input_token_count(
            async_runtime,
            client,
            &upstream_headers,
            &body,
            correlation_id,
            provider_for_event.as_deref(),
        ) {
            Ok(input_tokens) => input_tokens,
            Err(message) => {
                diagnostics::record(
                    ErrorCode::BackendConnectFailed,
                    "error",
                    &message,
                    Some(correlation_id),
                    Some(502),
                    provider_for_event.as_deref(),
                );
                respond_proxy_error(
                    request,
                    ErrorCode::BackendConnectFailed,
                    502,
                    "Basiliskos could not validate the active route's context budget.",
                    correlation_id,
                );
                return;
            }
        };
        if input_tokens.saturating_add(budget.reserved_output_tokens) > budget.window_tokens {
            respond_proxy_error(
                request,
                ErrorCode::ContextWindowExceeded,
                413,
                "This Grok request exceeds its 500K context window after reserving output tokens. Start a new session or compact the conversation.",
                correlation_id,
            );
            return;
        }
    }
    let upstream = match begin_upstream_request(
        async_runtime,
        client.clone(),
        method,
        upstream_url,
        upstream_headers,
        body,
    ) {
        Ok(response) => response,
        Err(error) => {
            let code = if matches!(error, FirstResponseFailure::Timeout) {
                ErrorCode::FirstByteTimeout
            } else {
                ErrorCode::BackendConnectFailed
            };
            diagnostics::record(
                code,
                "error",
                if matches!(error, FirstResponseFailure::Timeout) {
                    "The upstream did not produce response headers within the time budget."
                } else {
                    "The Basiliskos backend is unavailable."
                },
                Some(correlation_id),
                Some(504),
                provider_for_event.as_deref(),
            );
            let _ = request.respond(proxy_error(
                code,
                if matches!(error, FirstResponseFailure::Timeout) { 504 } else { 502 },
                if matches!(error, FirstResponseFailure::Timeout) {
                    "The upstream timed out before its first response. Retry this request."
                } else {
                    "The local backend is unavailable. Basiliskos will retry it for future requests."
                },
                correlation_id,
            ));
            return;
        }
    };
    let upstream_status = upstream.status;
    let classified = classify_upstream_status(upstream_status);
    if let Some(code) = classified {
        diagnostics::record(
            code,
            if upstream_status >= 500 {
                "error"
            } else {
                "warning"
            },
            match code {
                ErrorCode::ProviderAuthFailed => {
                    provider_auth_failure_message(provider_for_event.as_deref(), upstream_status)
                }
                ErrorCode::ProviderRateLimited => {
                    "The provider rate-limited the selected credential."
                }
                ErrorCode::ProviderQuotaExhausted => {
                    "The provider quota or billing limit was exhausted for the selected credential."
                }
                _ => "The provider returned a server error.",
            },
            Some(correlation_id),
            Some(upstream_status),
            provider_for_event.as_deref(),
        );
        let local_concurrency_fault = code == ErrorCode::ProviderRateLimited
            && upstream.headers.iter().any(|(name, value)| {
                name.eq_ignore_ascii_case("x-basiliskos-fault")
                    && String::from_utf8_lossy(value).eq_ignore_ascii_case("local-concurrency")
            });
        if matches!(
            code,
            ErrorCode::ProviderRateLimited | ErrorCode::ProviderQuotaExhausted
        ) && !local_concurrency_fault
        {
            if let Some(account_file) = active_account_for_event.clone() {
                let retry_after_seconds = upstream
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
                    .and_then(|(_, value)| {
                        parse_retry_after_seconds(&String::from_utf8_lossy(value))
                    })
                    .unwrap_or(if code == ErrorCode::ProviderQuotaExhausted {
                        DEFAULT_RATE_LIMIT_COOLDOWN_SECS.saturating_mul(4)
                    } else {
                        DEFAULT_RATE_LIMIT_COOLDOWN_SECS
                    });
                if let Ok(mut runtime) = runtime_lock() {
                    runtime.account_cooldowns.insert(
                        account_file.clone(),
                        Utc::now() + chrono::Duration::seconds(retry_after_seconds),
                    );
                }
                if let Some(provider) = provider_for_event.as_deref() {
                    attempt_same_provider_failover(
                        &account_file,
                        provider,
                        surface.unwrap_or(ClientSurface::Claude),
                    );
                }
            }
        }
        if local_concurrency_fault {
            diagnostics::record(
                ErrorCode::RelayBusy,
                "warning",
                "The local provider gate is busy; the selected account was not cooled down.",
                Some(correlation_id),
                Some(upstream_status),
                provider_for_event.as_deref(),
            );
        }
    }
    let status = StatusCode(upstream_status);
    let mut headers: Vec<Header> = upstream
        .headers
        .into_iter()
        .filter_map(|(name, value)| {
            if is_hop_by_hop_header(&name) {
                return None;
            }
            Header::from_bytes(name.as_bytes(), value).ok()
        })
        .collect();
    if let Ok(header) = Header::from_bytes("x-basiliskos-correlation-id", correlation_id) {
        headers.push(header);
    }
    if let Some(code) = classified {
        if let Ok(header) = Header::from_bytes("x-basiliskos-error-code", code.as_str()) {
            headers.push(header);
        }
    }
    let response_body: Box<dyn Read + Send> = {
        let tracked = TrackedUpstream {
            receiver: upstream.body,
            current: None,
            offset: 0,
            correlation_id: correlation_id.to_owned(),
            provider: provider_for_event.clone(),
        };
        Box::new(tracked)
    };
    let response = Response::new(status, headers, response_body, None, None);
    if request.respond(response).is_err() {
        diagnostics::record(
            ErrorCode::ClientCancelled,
            "info",
            "The client disconnected before the response completed.",
            Some(correlation_id),
            None,
            None,
        );
    }
}

/// client sends an image, the upstream rejects the whole request with a serde
fn start_front_proxy(app: AppHandle, api_key: String) -> Result<FrontProxy, String> {
    let server = Server::http(("127.0.0.1", GATEWAY_PORT))
        .map_err(|error| format!("Could not start Basiliskos compatibility proxy: {error}"))?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(5))
        .pool_max_idle_per_host(RELAY_WORKERS)
        .build()
        .map_err(|error| format!("Could not create Basiliskos proxy client: {error}"))?;
    let async_runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_io()
            .enable_time()
            .thread_name("basiliskos-relay-io")
            .build()
            .map_err(|error| format!("Could not create Basiliskos I/O runtime: {error}"))?,
    );
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let (request_tx, request_rx) =
        mpsc::sync_channel::<(tiny_http::Request, String)>(RELAY_QUEUE_CAPACITY);
    let shared_rx = Arc::new(Mutex::new(request_rx));
    let tracker = Arc::new(WorkerTracker::default());
    let mut workers = Vec::with_capacity(RELAY_WORKERS);
    for _ in 0..RELAY_WORKERS {
        let worker_rx = Arc::clone(&shared_rx);
        let worker_tracker = Arc::clone(&tracker);
        let worker_client = client.clone();
        let worker_runtime = Arc::clone(&async_runtime);
        let worker_api_key = api_key.clone();
        workers.push(thread::spawn(move || loop {
            let next = worker_rx
                .lock()
                .ok()
                .and_then(|receiver| receiver.recv().ok());
            let Some((request, correlation_id)) = next else {
                break;
            };
            if let Ok(mut active) = worker_tracker.active.lock() {
                *active += 1;
            }
            handle_front_proxy_request(
                request,
                &worker_client,
                worker_runtime.handle(),
                &worker_api_key,
                &correlation_id,
            );
            if let Ok(mut active) = worker_tracker.active.lock() {
                *active = active.saturating_sub(1);
                worker_tracker.changed.notify_all();
            }
        }));
    }
    let listener_api_key = api_key;
    let listener = thread::spawn(move || loop {
        if shutdown_rx.try_recv().is_ok() {
            break;
        }
        match server.recv_timeout(Duration::from_millis(150)) {
            Ok(Some(request)) => {
                let correlation_id = Uuid::new_v4().simple().to_string();
                if !request_headers_within_budget(&request) {
                    respond_proxy_error(
                        request,
                        ErrorCode::RequestHeadersTooLarge,
                        431,
                        "The request headers exceed the Basiliskos limit.",
                        &correlation_id,
                    );
                    continue;
                }
                // Authentication happens on the listener before the body is read,
                // parsed, rewritten, or queued to a worker.
                if !request_is_authorized(&request, &listener_api_key) {
                    respond_proxy_error(
                        request,
                        ErrorCode::RequestUnauthorized,
                        401,
                        "A valid local Basiliskos API key is required.",
                        &correlation_id,
                    );
                    continue;
                }
                if request.url().split('?').next() == Some("/hydra/health") {
                    let _ = request.respond(health_response(&listener_api_key, &correlation_id));
                    continue;
                }
                match request_tx.try_send((request, correlation_id)) {
                    Ok(()) => {}
                    Err(mpsc::TrySendError::Full((request, correlation_id))) => {
                        respond_proxy_error(
                            request,
                            ErrorCode::RelayBusy,
                            503,
                            "The Basiliskos relay is at capacity. Retry with backoff.",
                            &correlation_id,
                        );
                    }
                    Err(mpsc::TrySendError::Disconnected((request, correlation_id))) => {
                        respond_proxy_error(
                            request,
                            ErrorCode::RelayShuttingDown,
                            503,
                            "The Basiliskos relay is shutting down.",
                            &correlation_id,
                        );
                        break;
                    }
                }
            }
            Ok(None) => supervise_backend(&app),
            Err(_) => break,
        }
    });
    Ok(FrontProxy {
        shutdown: shutdown_tx,
        listener,
        workers,
        tracker,
        async_runtime,
    })
}

#[cfg(target_os = "windows")]
pub(crate) fn hidden(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn hidden(_command: &mut Command) {}

#[cfg(target_os = "windows")]
pub(crate) fn assign_gateway_to_kill_on_close_job(child: &Child) -> Result<Option<usize>, String> {
    use std::{mem::size_of, os::windows::io::AsRawHandle, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
            SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
            JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        },
    };

    let job = unsafe { CreateJobObjectW(ptr::null(), ptr::null()) };
    if job.is_null() {
        return Err(format!(
            "Could not create the Basiliskos backend job: {}",
            std::io::Error::last_os_error()
        ));
    }
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let configured = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if configured == 0 {
        let error = std::io::Error::last_os_error();
        unsafe { CloseHandle(job) };
        return Err(format!(
            "Could not configure the Basiliskos backend job: {error}"
        ));
    }
    let process_handle = child.as_raw_handle() as HANDLE;
    if unsafe { AssignProcessToJobObject(job, process_handle) } == 0 {
        let error = std::io::Error::last_os_error();
        unsafe { CloseHandle(job) };
        return Err(format!(
            "Could not secure the Basiliskos backend process: {error}"
        ));
    }
    Ok(Some(job as usize))
}

#[cfg(not(target_os = "windows"))]
fn assign_gateway_to_kill_on_close_job(_child: &Child) -> Result<Option<usize>, String> {
    Ok(None)
}

#[cfg(target_os = "windows")]
pub(crate) fn close_gateway_job(job: Option<usize>) {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    if let Some(job) = job {
        // KILL_ON_JOB_CLOSE is the crash/forced-exit backstop. During a normal
        // shutdown the child has already been asked to exit before this handle closes.
        unsafe { CloseHandle(job as HANDLE) };
    }
}

#[cfg(not(target_os = "windows"))]
fn close_gateway_job(_job: Option<usize>) {}

#[cfg(target_os = "windows")]
fn job_has_active_processes(job: usize) -> bool {
    use std::mem::size_of;
    use windows_sys::Win32::{
        Foundation::HANDLE,
        System::JobObjects::{
            JobObjectBasicAccountingInformation, QueryInformationJobObject,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        },
    };
    let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    unsafe {
        QueryInformationJobObject(
            job as HANDLE,
            JobObjectBasicAccountingInformation,
            (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
            size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
            std::ptr::null_mut(),
        ) != 0
            && info.ActiveProcesses > 0
    }
}

#[cfg(target_os = "windows")]
fn terminate_owned_job(job: usize) {
    use windows_sys::Win32::{Foundation::HANDLE, System::JobObjects::TerminateJobObject};
    unsafe {
        let _ = TerminateJobObject(job as HANDLE, 1);
    }
}

#[cfg(target_os = "windows")]
fn request_graceful_window_close(pid: u32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    for window in enum_claude_hwnds_for_pid(pid) {
        unsafe {
            let _ = PostMessageW(
                window.hwnd as windows_sys::Win32::Foundation::HWND,
                WM_CLOSE,
                0,
                0,
            );
        }
    }
}

fn spawn_backend_process(
    app: &AppHandle,
    append_logs: bool,
) -> Result<(Child, Option<usize>), String> {
    let executable = prepare_runtime(app)?;
    // The Codex compaction plugin lives next to the gateway so CPA can load it.
    if let Err(error) = prepare_codex_compaction_plugin(app) {
        diagnostics::record(
            ErrorCode::ConfigTransactionFailed,
            "warning",
            &format!("Codex compaction plugin not installed: {error}"),
            None,
            None,
            None,
        );
    }
    let log_dir = gateway_dir()?.join("controller-logs");
    secure_create_dir_all(&log_dir)?;
    let stdout_path = log_dir.join("gateway.stdout.log");
    let stderr_path = log_dir.join("gateway.stderr.log");
    let open_log = |path: &Path| {
        let mut options = fs::OpenOptions::new();
        options.create(true).write(true);
        if append_logs {
            options.append(true);
        } else {
            options.truncate(true);
        }
        options
            .open(path)
            .map_err(|error| format!("Could not open a Basiliskos backend log: {error}"))
    };
    let stdout = open_log(&stdout_path)?;
    let stderr = open_log(&stderr_path)?;
    let mut command = Command::new(executable);
    command
        .args(["-config", &config_path()?.to_string_lossy(), "-local-model"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    hidden(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start the Basiliskos backend: {error}"))?;
    let job = assign_gateway_to_kill_on_close_job(&child).inspect_err(|_| {
        let _ = child.kill();
        let _ = child.wait();
    })?;
    Ok((child, job))
}

fn supervise_backend(app: &AppHandle) {
    let Ok(_mutation) = controller().mutations.try_lock() else {
        return;
    };
    let now = Instant::now();
    let mut exited_job = None;
    let mut should_restart = false;
    {
        let Ok(mut runtime) = runtime_lock() else {
            return;
        };
        if !matches!(
            runtime.phase,
            GatewayPhase::Running | GatewayPhase::Degraded
        ) {
            return;
        }
        if let Some(child) = runtime.gateway_child.as_mut() {
            match child.try_wait() {
                Ok(None) => {
                    if runtime.phase == GatewayPhase::Degraded {
                        if let Ok(state) = load_state() {
                            if backend_health_check(&state.api_key) {
                                runtime.phase = GatewayPhase::Running;
                                runtime.backend_exit_reason = None;
                                runtime.backend_restart_attempts = 0;
                                runtime.backend_next_restart = None;
                            }
                        }
                    }
                    return;
                }
                Ok(Some(status)) => {
                    runtime.gateway_child = None;
                    #[cfg(target_os = "windows")]
                    {
                        exited_job = runtime.gateway_job.take();
                    }
                    runtime.phase = GatewayPhase::Degraded;
                    runtime.backend_restart_attempts =
                        runtime.backend_restart_attempts.saturating_add(1);
                    let delay = 2_u64
                        .saturating_pow(runtime.backend_restart_attempts.min(4))
                        .min(30);
                    runtime.backend_next_restart = Some(now + Duration::from_secs(delay));
                    runtime.backend_exit_reason = Some(format!(
                        "Backend exited with {status}; retry scheduled in {delay}s"
                    ));
                    diagnostics::record(
                        ErrorCode::BackendExited,
                        "error",
                        "The managed backend exited; a bounded restart is scheduled for future requests.",
                        None,
                        None,
                        None,
                    );
                }
                Err(_) => {
                    runtime.gateway_child = None;
                    runtime.phase = GatewayPhase::Degraded;
                    runtime.backend_restart_attempts =
                        runtime.backend_restart_attempts.saturating_add(1);
                    runtime.backend_next_restart = Some(now + Duration::from_secs(2));
                    runtime.backend_exit_reason =
                        Some("Backend process state could not be read; retry scheduled".into());
                }
            }
        }
        if runtime.gateway_child.is_none()
            && runtime
                .backend_next_restart
                .is_none_or(|restart_at| restart_at <= now)
        {
            should_restart = true;
        }
    }
    close_gateway_job(exited_job);
    if !should_restart {
        return;
    }
    let _ = prepare_config();
    match spawn_backend_process(app, true) {
        Ok((child, job)) => {
            if let Ok(mut runtime) = runtime_lock() {
                if runtime.phase == GatewayPhase::Degraded && runtime.gateway_child.is_none() {
                    runtime.gateway_child = Some(child);
                    #[cfg(target_os = "windows")]
                    {
                        runtime.gateway_job = job;
                    }
                    runtime.backend_next_restart = None;
                    runtime.backend_exit_reason = Some("Backend restart is warming up".into());
                } else {
                    let mut child = child;
                    let _ = child.kill();
                    let _ = child.wait();
                    close_gateway_job(job);
                }
            }
        }
        Err(_) => {
            diagnostics::record(
                ErrorCode::BackendRestartFailed,
                "error",
                "A managed backend restart failed; the next attempt will use bounded backoff.",
                None,
                None,
                None,
            );
            if let Ok(mut runtime) = runtime_lock() {
                runtime.backend_restart_attempts =
                    runtime.backend_restart_attempts.saturating_add(1);
                let delay = 2_u64
                    .saturating_pow(runtime.backend_restart_attempts.min(4))
                    .min(30);
                runtime.backend_next_restart = Some(Instant::now() + Duration::from_secs(delay));
                runtime.backend_exit_reason = Some(format!(
                    "Backend restart failed; retry scheduled in {delay}s"
                ));
            }
        }
    }
}

fn stop_hydra_claude_runtime() {
    let (child, job, pid, executable, profile) = match runtime_lock() {
        Ok(mut runtime) => {
            let child = runtime.claude_child.take();
            #[cfg(target_os = "windows")]
            let job = runtime.claude_job.take();
            #[cfg(not(target_os = "windows"))]
            let job = None;
            (
                child,
                job,
                runtime.claude_root_pid.take(),
                runtime.claude_executable.take(),
                runtime.claude_profile.take(),
            )
        }
        Err(_) => return,
    };
    #[cfg(target_os = "windows")]
    {
        if let (Some(pid), Some(executable), Some(profile)) = (pid, executable, profile) {
            // Only the PID created from the verified Store executable with the isolated
            // profile is asked to close. The job object below is the ownership boundary
            // for any descendants; the user's normal Claude process is never enumerated
            // by name or terminated.
            if executable.is_file() && profile == isolated_claude_profile_dir().unwrap_or_default()
            {
                request_graceful_window_close(pid);
            }
        }
        if let Some(job) = job {
            let deadline = Instant::now() + Duration::from_secs(5);
            while job_has_active_processes(job) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(100));
            }
            if job_has_active_processes(job) {
                terminate_owned_job(job);
            }
            close_gateway_job(Some(job));
        }
    }
    if let Some(mut child) = child {
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

fn request_codex_window_close(pid: u32) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    for window in enum_codex_hwnds_for_pid(pid) {
        unsafe {
            let _ = PostMessageW(
                window.hwnd as windows_sys::Win32::Foundation::HWND,
                WM_CLOSE,
                0,
                0,
            );
        }
    }
}

/// Codex hides to an off-screen owner window when the user clicks X. The
/// process stays a child of Basiliskos and keeps burning a CPU core. Reap it
/// once the last on-screen window has been gone for two seconds.
///
/// A generation counter (not the PID) identifies the watcher's own launch:
/// Windows can recycle a PID within the 500 ms poll window if the user closes
/// and immediately reopens the window, which would otherwise let a stale
/// watcher reap the new instance before its window paints. The watcher also
/// checks the live `Child` handle with `try_wait` for authoritative exit
/// detection, which is immune to PID reuse.
static CODEX_WATCHER_GENERATION: AtomicU32 = AtomicU32::new(0);

#[cfg(target_os = "windows")]
fn spawn_codex_close_watcher(pid: u32, generation: u32) {
    thread::spawn(move || {
        use crate::codex_window::{has_open_isolated_codex_window, should_reap_hidden_codex};
        let mut seen_open = false;
        let mut missing_ticks = 0_u32;
        loop {
            thread::sleep(Duration::from_millis(500));
            // The runtime must still own this exact launch: same root PID and
            // same watcher generation. A newer Codex instance (even one that
            // recycled this PID) supersedes the watcher.
            let exited = match runtime_lock() {
                Ok(mut runtime)
                    if runtime.codex_root_pid == Some(pid)
                        && runtime.codex_watcher_generation == Some(generation) =>
                {
                    match runtime.codex_child.as_mut() {
                        Some(child) => matches!(child.try_wait(), Ok(Some(_)) | Err(_)),
                        None => true,
                    }
                }
                _ => return,
            };
            if exited {
                let _ = hydra_codex_running();
                return;
            }
            let currently_open = has_open_isolated_codex_window(pid);
            if currently_open {
                seen_open = true;
                missing_ticks = 0;
                continue;
            }
            if seen_open {
                missing_ticks = missing_ticks.saturating_add(1);
            }
            if should_reap_hidden_codex(seen_open, currently_open, missing_ticks) {
                codex_log_icon_line(&format!(
                    "isolated Codex window gone; stopping leftover process pid={pid}"
                ));
                stop_hydra_codex_runtime();
                return;
            }
        }
    });
}

#[cfg(not(target_os = "windows"))]
fn spawn_codex_close_watcher(_pid: u32, _generation: u32) {}

fn stop_hydra_codex_runtime() {
    let (child, job, pid, _watcher_generation, executable, home) = match runtime_lock() {
        Ok(mut runtime) => {
            let child = runtime.codex_child.take();
            #[cfg(target_os = "windows")]
            let job = runtime.codex_job.take();
            #[cfg(not(target_os = "windows"))]
            let job = None;
            (
                child,
                job,
                runtime.codex_root_pid.take(),
                runtime.codex_watcher_generation.take(),
                runtime.codex_executable.take(),
                runtime.codex_home.take(),
            )
        }
        Err(_) => return,
    };
    #[cfg(target_os = "windows")]
    {
        if let (Some(pid), Some(executable), Some(home)) = (pid, executable, home) {
            // Only the PID created from the verified Store executable with the
            // isolated home is asked to close. The job object below is the
            // ownership boundary for any descendants; the user's normal Codex
            // app is never enumerated by name or terminated.
            if executable.is_file() && home == isolated_codex_home().unwrap_or_default() {
                request_codex_window_close(pid);
            }
        }
        if let Some(job) = job {
            let deadline = Instant::now() + Duration::from_secs(5);
            while job_has_active_processes(job) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(100));
            }
            if job_has_active_processes(job) {
                terminate_owned_job(job);
            }
            close_gateway_job(Some(job));
        }
    }
    if let Some(mut child) = child {
        if child.try_wait().ok().flatten().is_none() {
            let _ = child.kill();
        }
        let _ = child.wait();
    }
}

fn hydra_codex_running() -> bool {
    let Ok(mut runtime) = runtime_lock() else {
        return false;
    };
    #[cfg(target_os = "windows")]
    if let Some(job) = runtime.codex_job {
        if job_has_active_processes(job) {
            return true;
        }
        close_gateway_job(runtime.codex_job.take());
        runtime.codex_child.take().map(|mut child| child.wait());
        runtime.codex_root_pid = None;
        runtime.codex_watcher_generation = None;
        runtime.codex_executable = None;
        runtime.codex_home = None;
        diagnostics::record(
            ErrorCode::CodexExited,
            "info",
            "The isolated Basiliskos Codex process tree exited.",
            None,
            None,
            None,
        );
        return false;
    }
    let Some(child) = runtime.codex_child.as_mut() else {
        return false;
    };
    match child.try_wait() {
        Ok(None) => true,
        Ok(Some(_)) | Err(_) => {
            runtime.codex_child = None;
            false
        }
    }
}

fn stop_gateway_runtime() {
    stop_hydra_claude_runtime();
    stop_hydra_codex_runtime();
    let (proxy, child, job) = match runtime_lock() {
        Ok(mut runtime) => {
            runtime.phase = GatewayPhase::Stopping;
            let proxy = runtime.front_proxy.take();
            let child = runtime.gateway_child.take();
            #[cfg(target_os = "windows")]
            let job = runtime.gateway_job.take();
            #[cfg(not(target_os = "windows"))]
            let job = None;
            (proxy, child, job)
        }
        Err(_) => return,
    };
    if let Some(proxy) = proxy {
        proxy.shutdown();
    }
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
    close_gateway_job(job);
    if let Ok(mut runtime) = runtime_lock() {
        runtime.phase = GatewayPhase::Stopped;
        runtime.backend_exit_reason = None;
        runtime.backend_restart_attempts = 0;
        runtime.backend_next_restart = None;
    }
}

pub fn stop_gateway_internal() {
    if let Ok(_mutation) = mutation_lock() {
        cancel_login_runtime();
        stop_gateway_runtime();
    }
}

fn hydra_claude_running() -> bool {
    let Ok(mut runtime) = runtime_lock() else {
        return false;
    };
    #[cfg(target_os = "windows")]
    if let Some(job) = runtime.claude_job {
        if job_has_active_processes(job) {
            return true;
        }
        close_gateway_job(runtime.claude_job.take());
        runtime.claude_child.take().map(|mut child| child.wait());
        runtime.claude_root_pid = None;
        runtime.claude_executable = None;
        runtime.claude_profile = None;
        diagnostics::record(
            ErrorCode::ClaudeExited,
            "info",
            "The isolated Basiliskos Claude process tree exited.",
            None,
            None,
            None,
        );
        return false;
    }
    let Some(child) = runtime.claude_child.as_mut() else {
        return false;
    };
    match child.try_wait() {
        Ok(None) => true,
        Ok(Some(_)) | Err(_) => {
            runtime.claude_child = None;
            false
        }
    }
}

fn gateway_running() -> bool {
    let state = match load_state() {
        Ok(state) => state,
        Err(_) => return false,
    };
    health_check(&state.api_key)
}

#[tauri::command]
pub fn start_gateway(app: AppHandle) -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    start_gateway_locked(app)
}

fn start_gateway_locked(app: AppHandle) -> Result<GatewaySnapshot, String> {
    let state = prepare_config()?;
    if health_check(&state.api_key) {
        let runtime = runtime_lock()?;
        let owns_front_proxy = runtime.front_proxy.is_some()
            && matches!(
                runtime.phase,
                GatewayPhase::Starting | GatewayPhase::Running
            );
        drop(runtime);
        if owns_front_proxy {
            return gateway_snapshot_locked();
        }
        return Err(
            "Another Basiliskos instance already owns the local relay. Use that window or close it before reopening Basiliskos."
                .into(),
        );
    }
    stop_gateway_runtime();
    let executable = prepare_runtime(&app)?;
    terminate_stale_managed_backends(&executable)?;
    if !port_is_available(GATEWAY_PORT) {
        return Err(
            "Basiliskos port 8317 is occupied by another process. Close the other instance before starting the relay."
                .into(),
        );
    }
    if !port_is_available(BACKEND_PORT) {
        return Err(
            "Basiliskos backend port 8318 is occupied by a stale or unrelated process. Close it before starting the relay; Basiliskos will no longer reuse an unowned backend."
                .into(),
        );
    }
    let (mut child, job) = spawn_backend_process(&app, false).inspect_err(|_| {
        diagnostics::record(
            ErrorCode::BackendRestartFailed,
            "error",
            "The managed backend could not be started.",
            None,
            None,
            None,
        );
    })?;
    let proxy = match start_front_proxy(app.clone(), state.api_key.clone()) {
        Ok(proxy) => proxy,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            close_gateway_job(job);
            return Err(error);
        }
    };
    {
        let mut runtime = runtime_lock()?;
        runtime.phase = GatewayPhase::Starting;
        runtime.backend_exit_reason = None;
        runtime.backend_restart_attempts = 0;
        runtime.backend_next_restart = None;
        runtime.gateway_child = Some(child);
        runtime.front_proxy = Some(proxy);
        #[cfg(target_os = "windows")]
        {
            runtime.gateway_job = job;
        }
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if health_check(&state.api_key) {
            runtime_lock()?.phase = GatewayPhase::Running;
            return gateway_snapshot_locked();
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    stop_gateway_runtime();
    Err("Basiliskos did not become ready. Check ~/.hydra-gateway/gateway/controller-logs.".into())
}

#[tauri::command]
pub fn stop_gateway() -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    stop_gateway_runtime();
    gateway_snapshot_locked()
}

fn nested_string(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(text) = map.get(*key).and_then(Value::as_str) {
                    if !text.trim().is_empty() {
                        return Some(text.to_string());
                    }
                }
            }
            map.values().find_map(|child| nested_string(child, keys))
        }
        Value::Array(items) => items.iter().find_map(|child| nested_string(child, keys)),
        _ => None,
    }
}

pub(crate) fn account_provider(value: &Value, file_name: &str) -> Option<String> {
    let explicit = nested_string(value, &["type", "provider"])
        .or_else(|| nested_string(value, &["provider"]))
        .or_else(|| {
            value
                .get("type")
                .and_then(Value::as_str)
                .map(|provider| provider.to_ascii_lowercase())
        });
    let provider = explicit.or_else(|| {
        let lower = file_name.to_ascii_lowercase();
        let providers = all_providers();
        providers
            .iter()
            .find(|provider| lower.starts_with(**provider))
            .map(|provider| provider.to_string())
    })?;
    all_providers()
        .contains(&provider.as_str())
        .then_some(provider)
}

fn credential_expiry(value: &Value) -> Option<DateTime<Utc>> {
    ["expired", "expires_at", "expiresAt", "expiry"]
        .into_iter()
        .find_map(|key| value.get(key))
        .and_then(|raw| match raw {
            Value::String(value) => DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|value| value.with_timezone(&Utc)),
            Value::Number(value) => value.as_i64().and_then(|seconds| {
                // OAuth documents commonly use epoch seconds, while a few
                // local stores use milliseconds. Accept both without guessing
                // from arbitrary string fields.
                if seconds.abs() >= 100_000_000_000 {
                    Utc.timestamp_millis_opt(seconds).single()
                } else {
                    Utc.timestamp_opt(seconds, 0).single()
                }
            }),
            _ => None,
        })
}

fn credential_status(
    provider: &str,
    file_name: &str,
    expiry: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> String {
    let relogin_required = match provider {
        "xai" => xai_relogin_required(file_name).unwrap_or(false),
        "kimi" => kimi_relogin_required(file_name).unwrap_or(false),
        _ => false,
    };
    if relogin_required {
        return "relogin_required".into();
    }
    let renewal_window = match provider {
        "xai" => XAI_REFRESH_SKEW_SECS,
        "kimi" => KIMI_REFRESH_SKEW_SECS,
        "codex" | "claude" => OAUTH_REFRESH_SKEW_SECS,
        _ => 0,
    };
    match expiry {
        None => "unknown".into(),
        Some(expiry) if expiry <= now && renewal_window == 0 => "expired".into(),
        Some(expiry) if expiry <= now + ChronoDuration::seconds(renewal_window) => {
            "renewal_due".into()
        }
        Some(_) => "active".into(),
    }
}

fn auth_str(kind: ProviderAuth) -> &'static str {
    match kind {
        ProviderAuth::Oauth => "oauth",
        ProviderAuth::ApiKey => "api_key",
    }
}

fn key_credential_status(disabled: bool) -> String {
    if disabled {
        "disabled".into()
    } else {
        // An API-key account has no OAuth freshness; it is validated live by
        // the endpoint health probe when the user opens the route panel.
        "configured".into()
    }
}

fn default_account_label(provider: &str) -> String {
    match provider {
        "xai" => "Grok Build".into(),
        "codex" => "Codex account".into(),
        "kimi" => "Kimi Code".into(),
        "antigravity" => "Antigravity account".into(),
        "zai" => "Z.AI GLM".into(),
        "claude" => "Claude account".into(),
        "deepseek" => "DeepSeek API".into(),
        "opencode" => "OpenCode API".into(),
        "openrouter" => "OpenRouter API".into(),
        "litellm" => "LiteLLM API".into(),
        "custom" => "Custom endpoint".into(),
        _ => "Account".into(),
    }
}

/// A distinct default label for API-key accounts so "Grok Build" (OAuth) and
/// "Grok API" (key) never read the same. Kept separate from the OAuth default.
fn api_key_default_label(provider: &str) -> String {
    match provider {
        _ if provider == "xai" => "Grok API".into(),
        "codex" => "Codex API".into(),
        "claude" => "Claude API".into(),
        "antigravity" => "Gemini API".into(),
        "zai" => "Z.AI API".into(),
        "kimi" => "Moonshot API".into(),
        "deepseek" => "DeepSeek API".into(),
        "opencode" => "OpenCode API".into(),
        "openrouter" => "OpenRouter API".into(),
        "litellm" => "LiteLLM API".into(),
        "custom" => "Custom endpoint".into(),
        _ => "API key account".into(),
    }
}

fn list_accounts_inner(state: &ControllerState) -> Result<Vec<GatewayAccount>, String> {
    let labels = load_account_labels()?;
    let cooldowns = {
        let mut runtime = runtime_lock()?;
        let now = Utc::now();
        runtime.account_cooldowns.retain(|_, until| *until > now);
        runtime.account_cooldowns.clone()
    };
    let now = Utc::now();
    let mut accounts = Vec::new();
    for (directory, kind) in [
        (auth_dir()?, ProviderAuth::Oauth),
        (keys_dir()?, ProviderAuth::ApiKey),
    ] {
        secure_create_dir_all(&directory)?;
        for entry in fs::read_dir(&directory)
            .map_err(|error| format!("Could not read {}: {error}", directory.display()))?
        {
            let entry = entry.map_err(|error| format!("Could not read account file: {error}"))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy().to_string();
            let Ok(raw) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let kind = if account_auth_kind(&value) == ProviderAuth::ApiKey {
                ProviderAuth::ApiKey
            } else {
                kind
            };
            let Some(provider) = account_provider(&value, &file_name) else {
                continue;
            };
            let email = nested_string(&value, &["email", "preferred_username"]);
            let disabled = value
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let label = labels.get(&file_name).cloned().unwrap_or_else(|| {
                email.clone().unwrap_or_else(|| {
                    if kind == ProviderAuth::ApiKey {
                        api_key_default_label(&provider)
                    } else {
                        default_account_label(&provider)
                    }
                })
            });
            let cooldown_until_ms = cooldowns
                .get(&file_name)
                .map(|until| until.timestamp_millis());
            let base_url = nested_string(&value, &["base_url"]);
            let credential_status = match kind {
                ProviderAuth::ApiKey => key_credential_status(disabled),
                ProviderAuth::Oauth => {
                    let expiry = credential_expiry(&value);
                    credential_status(&provider, &file_name, expiry, now)
                }
            };
            let expires_at_ms = if kind == ProviderAuth::Oauth {
                credential_expiry(&value).map(|value| value.timestamp_millis())
            } else {
                None
            };
            accounts.push(GatewayAccount {
                active: state.active_account.as_deref() == Some(file_name.as_str()) && !disabled,
                active_for_codex: state.active_codex_account.as_deref() == Some(file_name.as_str())
                    && !disabled,
                file_name,
                provider,
                email,
                label,
                disabled,
                cooldown_until_ms,
                expires_at_ms,
                credential_status,
                auth: auth_str(kind).to_string(),
                base_url,
            });
        }
    }
    accounts.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then(left.auth.cmp(&right.auth))
            .then(left.label.cmp(&right.label))
    });
    Ok(accounts)
}

fn claude_3p_profile_dir() -> Result<PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("Claude-3p"))
        .ok_or_else(|| "LOCALAPPDATA is not available".to_string())
}

fn shared_claude_library_dir() -> Result<PathBuf, String> {
    Ok(claude_3p_profile_dir()?.join("configLibrary"))
}

pub(crate) fn isolated_claude_profile_dir() -> Result<PathBuf, String> {
    // Claude 1.40609 `cl()` on Windows is `%LOCALAPPDATA%\Claude-3p` unless
    // CLAUDE_USER_DATA_DIR is set, and that env now means "use Electron
    // userData" (stock %APPDATA%\Claude on MSIX) instead of the env path.
    claude_3p_profile_dir()
}

pub(crate) fn isolated_codex_home() -> Result<PathBuf, String> {
    Ok(root_dir()?.join("codex-profile"))
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestPublishedRelease {
    pub tag_name: String,
    pub name: String,
    pub body: String,
    pub published_at: String,
    pub release_url: String,
    pub installer_url: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreparedBasiliskosUpdate {
    pub token: String,
    pub tag_name: String,
    pub installer_name: String,
}

fn valid_release_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 128
        && tag
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn release_installer_name(tag: &str) -> Result<String, String> {
    if !valid_release_tag(tag) {
        return Err("The update service returned an invalid release tag.".to_owned());
    }
    let version = tag.strip_prefix('v').unwrap_or(tag);
    if version.is_empty() {
        return Err("The update service returned an invalid release tag.".to_owned());
    }
    Ok(format!("BasiliskOS_{version}_x64-setup.exe"))
}

fn release_download_url(tag: &str, asset_name: &str) -> Result<String, String> {
    if !valid_release_tag(tag)
        || !asset_name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
    {
        return Err("The update service returned an invalid release asset.".to_owned());
    }
    Ok(format!(
        "{BASILISKOS_RELEASE_DOWNLOAD_BASE}/{tag}/{asset_name}"
    ))
}

/// Resolves `asset_name` inside a SHA-256SUMS manifest case-insensitively and
/// returns `(checksum, exact_asset_name)`. The product renamed its installer
/// from `Basiliskos_` to `BasiliskOS_`, so releases can carry either casing;
/// downloading by the manifest's exact name keeps every client compatible.
fn checksum_from_manifest(manifest: &str, asset_name: &str) -> Option<(String, String)> {
    manifest.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let checksum = parts.next()?;
        let name = parts.next()?.trim_start_matches('*');
        (parts.next().is_none()
            && name.eq_ignore_ascii_case(asset_name)
            && checksum.len() == 64
            && checksum.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| (checksum.to_ascii_lowercase(), name.to_owned()))
    })
}

async fn download_verified_release_installer(tag: &str) -> Result<(PathBuf, String), String> {
    let expected_installer_name = release_installer_name(tag)?;
    let manifest_url = release_download_url(tag, "SHA256SUMS.txt")?;
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent("Basiliskos/1.1")
        .build()
        .map_err(|error| format!("Could not prepare the updater: {error}"))?;
    let manifest_response = client
        .get(manifest_url)
        .send()
        .await
        .map_err(|error| format!("Could not download the release checksum: {error}"))?;
    if !manifest_response.status().is_success() {
        return Err(format!(
            "The release checksum is unavailable ({}).",
            manifest_response.status()
        ));
    }
    if manifest_response.content_length().unwrap_or(0) > MAX_RELEASE_MANIFEST_BYTES as u64 {
        return Err("The release checksum manifest is unexpectedly large.".to_owned());
    }
    let manifest = manifest_response
        .text()
        .await
        .map_err(|error| format!("Could not read the release checksum: {error}"))?;
    if manifest.len() > MAX_RELEASE_MANIFEST_BYTES {
        return Err("The release checksum manifest is unexpectedly large.".to_owned());
    }
    let (expected_checksum, installer_name) =
        checksum_from_manifest(&manifest, &expected_installer_name).ok_or_else(|| {
            "The release checksum does not include the Windows installer.".to_owned()
        })?;
    let installer_url = release_download_url(tag, &installer_name)?;

    let installer_response = client
        .get(installer_url)
        .send()
        .await
        .map_err(|error| format!("Could not download the Windows installer: {error}"))?;
    if !installer_response.status().is_success() {
        return Err(format!(
            "The Windows installer is unavailable ({}).",
            installer_response.status()
        ));
    }
    if installer_response.content_length().unwrap_or(0) > MAX_RELEASE_INSTALLER_BYTES {
        return Err("The Windows installer is unexpectedly large.".to_owned());
    }

    let update_dir = std::env::temp_dir().join("Basiliskos").join("updates");
    fs::create_dir_all(&update_dir)
        .map_err(|error| format!("Could not create the update folder: {error}"))?;
    let destination = update_dir.join(format!("{}-{installer_name}", Uuid::new_v4()));
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&destination)
        .map_err(|error| format!("Could not prepare the installer download: {error}"))?;
    let mut bytes_written = 0_u64;
    let mut hasher = Sha256::new();
    let mut stream = installer_response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk =
            chunk.map_err(|error| format!("The installer download was interrupted: {error}"))?;
        bytes_written = bytes_written.saturating_add(chunk.len() as u64);
        if bytes_written > MAX_RELEASE_INSTALLER_BYTES {
            let _ = fs::remove_file(&destination);
            return Err("The Windows installer is unexpectedly large.".to_owned());
        }
        file.write_all(&chunk)
            .map_err(|error| format!("Could not save the installer download: {error}"))?;
        hasher.update(&chunk);
    }
    file.flush()
        .map_err(|error| format!("Could not finish the installer download: {error}"))?;
    let actual_checksum = hex::encode(hasher.finalize());
    if actual_checksum != expected_checksum {
        let _ = fs::remove_file(&destination);
        return Err(
            "The downloaded installer did not match the published SHA-256 checksum.".to_owned(),
        );
    }
    Ok((destination, installer_name))
}

// GitHub's public REST API has a small unauthenticated per-IP quota. Resolve
// the public `releases/latest` redirect natively as a quota-free fallback so a
// rate-limited client can still discover the current release without a token.
#[tauri::command]
pub async fn latest_basiliskos_release() -> Result<LatestPublishedRelease, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent("Basiliskos/1.1")
        .build()
        .map_err(|error| format!("Could not prepare the update check: {error}"))?;
    let response = client
        .get(BASILISKOS_LATEST_RELEASE_URL)
        .send()
        .await
        .map_err(|error| format!("Could not contact the update service: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "Update service returned an unexpected status ({})",
            response.status()
        ));
    }
    let value: Value = response
        .json()
        .await
        .map_err(|_| "Update service returned an invalid response".to_owned())?;
    let tag_name = value
        .get("tag_name")
        .and_then(Value::as_str)
        .ok_or_else(|| "Update service returned no release tag".to_owned())?
        .to_string();
    if !valid_release_tag(&tag_name) {
        return Err("Update service returned an unexpected release tag".into());
    }
    let release_url = value
        .get("html_url")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let installer_url = value
        .get("assets")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|asset| {
            let name = asset.get("name").and_then(Value::as_str)?;
            (name.ends_with("_x64-setup.exe"))
                .then(|| asset.get("browser_download_url").and_then(Value::as_str))
                .flatten()
        })
        .next()
        .map(str::to_owned);
    Ok(LatestPublishedRelease {
        tag_name: tag_name.clone(),
        name: value
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(&tag_name)
            .to_string(),
        body: value
            .get("body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        published_at: value
            .get("published_at")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        release_url,
        installer_url,
    })
}

#[tauri::command]
pub async fn prepare_basiliskos_update(
    tag_name: String,
) -> Result<PreparedBasiliskosUpdate, String> {
    let (installer_path, installer_name) = download_verified_release_installer(&tag_name).await?;
    let token = Uuid::new_v4().to_string();
    PREPARED_UPDATE_INSTALLERS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| "The updater is busy. Try again.".to_owned())?
        .insert(token.clone(), installer_path);
    Ok(PreparedBasiliskosUpdate {
        token,
        tag_name,
        installer_name,
    })
}

#[tauri::command]
pub fn install_basiliskos_update(app: AppHandle, token: String) -> Result<(), String> {
    let installer_path = PREPARED_UPDATE_INSTALLERS
        .get_or_init(|| Mutex::new(BTreeMap::new()))
        .lock()
        .map_err(|_| "The updater is busy. Try again.".to_owned())?
        .remove(&token)
        .ok_or_else(|| {
            "This verified update is no longer available. Download it again.".to_owned()
        })?;
    let installer_path = fs::canonicalize(&installer_path)
        .map_err(|error| format!("The verified installer is no longer available: {error}"))?;
    if !installer_path.is_file() {
        return Err("The verified installer is no longer available.".to_owned());
    }
    // perMachine NSIS installs to Program Files, so the installer needs elevation.
    // Elevate only the installer (ShellExecute "runas"), never the Basiliskos
    // process itself Ã¢â‚¬â€ the controller stays unelevated for OAuth / tray / profile work.
    launch_installer(&installer_path)?;
    app.exit(0);
    Ok(())
}

/// Launches the verified NSIS installer. On Windows this uses ShellExecuteW with
/// the `runas` verb so UAC appears; a plain CreateProcess spawn inherits the
/// unelevated app token and fails with "requires elevation".
fn launch_installer(installer_path: &Path) -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let path_wide: Vec<u16> = installer_path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let operation: Vec<u16> = "runas".encode_utf16().chain(std::iter::once(0)).collect();
        // ShellExecuteW returns an HINSTANCE cast as isize. Values > 32 mean the
        // request was accepted (UAC prompt shown / installer started). Values
        // Ã¢â€°Â¤ 32 are SE_ERR_* codes Ã¢â‚¬â€ most often SE_ERR_ACCESSDENIED when the user
        // cancels the UAC dialog.
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                path_wide.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        } as isize;
        if result <= 32 {
            return Err(match result {
                5 | 1223 => {
                    "The update installer needs administrator approval. Confirm the Windows UAC prompt, or run the downloaded installer manually."
                        .to_owned()
                }
                _ => format!(
                    "Could not launch the Windows installer (ShellExecute error {result})."
                ),
            });
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        Command::new(installer_path)
            .spawn()
            .map_err(|error| format!("Could not launch the Windows installer: {error}"))?;
        Ok(())
    }
}

#[tauri::command]
pub fn gateway_snapshot() -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    gateway_snapshot_locked()
}

/// The email of whichever relay account is currently active, if any. Used
/// only for the cross-service "currently active for" indicator Ã¢â‚¬â€ never a
/// hard dependency, so callers should treat `None` as "unknown," not "none."
pub fn active_relay_email() -> Option<String> {
    let state = load_state().ok()?;
    list_accounts_inner(&state)
        .ok()?
        .into_iter()
        .find(|account| account.active)
        .and_then(|account| account.email)
}

#[tauri::command]
pub fn open_diagnostics_folder(app: AppHandle) -> Result<(), String> {
    use tauri_plugin_opener::OpenerExt;
    let folder = gateway_dir()?.join("controller-logs");
    secure_create_dir_all(&folder)?;
    let verified_root = fs::canonicalize(gateway_dir()?)
        .map_err(|error| format!("Could not verify the Basiliskos data directory: {error}"))?;
    let verified_folder = fs::canonicalize(&folder)
        .map_err(|error| format!("Could not verify the diagnostics directory: {error}"))?;
    verified_folder
        .strip_prefix(verified_root)
        .map_err(|_| "Refusing to open a diagnostics directory outside Basiliskos")?;
    app.opener()
        .open_path(verified_folder.to_string_lossy(), None::<&str>)
        .map_err(|error| format!("Could not open the diagnostics directory: {error}"))
}

fn gateway_snapshot_locked() -> Result<GatewaySnapshot, String> {
    let mut state = load_state()?;
    restore_legacy_shared_config_if_needed(&mut state)?;
    let accounts = list_accounts_inner(&state)?;
    let routes = SUPPORTED_PROVIDERS
        .iter()
        .map(|provider| provider_route(&state, provider))
        .collect::<Vec<_>>();
    let codex_routes = SUPPORTED_PROVIDERS
        .iter()
        .map(|provider| provider_route_for(&state, ClientSurface::Codex, provider))
        .collect::<Vec<_>>();
    let running = gateway_running();
    let claude_running = hydra_claude_running();
    let codex_running = hydra_codex_running();
    let (phase, relay_present, backend_exit_reason, active_requests, login, auto_failover) = {
        let runtime = runtime_lock()?;
        let active_requests = runtime
            .front_proxy
            .as_ref()
            .and_then(|proxy| proxy.tracker.active.lock().ok().map(|active| *active))
            .unwrap_or_default();
        (
            runtime.phase,
            runtime.front_proxy.is_some(),
            runtime.backend_exit_reason.clone(),
            active_requests,
            runtime
                .login
                .as_ref()
                .map(|login| login.status.clone())
                .or_else(|| runtime.last_login.clone()),
            runtime.last_auto_failover.clone(),
        )
    };
    let phase_name = match phase {
        GatewayPhase::Stopped => "stopped",
        GatewayPhase::Starting => "starting",
        GatewayPhase::Running => "running",
        GatewayPhase::Degraded => "degraded",
        GatewayPhase::Stopping => "stopping",
    };
    let active = accounts.iter().find(|account| account.active);
    let active_label = active.map(|account| account.label.clone());
    let active_provider = active.map(|account| account.provider.clone());
    let route_detail = active_provider
        .as_deref()
        .and_then(|provider| routes.iter().find(|route| route.provider == provider))
        .map(|route| route.selected_model_label.clone())
        .unwrap_or_else(|| "No route until an account is selected".into());
    Ok(GatewaySnapshot {
        running,
        base_url: format!("http://127.0.0.1:{GATEWAY_PORT}"),
        version: GATEWAY_VERSION.into(),
        claude_running,
        codex_running,
        accounts,
        active_account: state.active_account,
        routes,
        active_codex_account: state.active_codex_account,
        codex_routes,
        auto_failover,
        controller: ComponentStatus {
            state: phase_name.into(),
            detail: format!("Controller is {phase_name}"),
        },
        relay: ComponentStatus {
            state: if relay_present { "running" } else { "stopped" }.into(),
            detail: if relay_present {
                format!("Relay online with {active_requests} active request(s)")
            } else {
                "Relay is not listening".into()
            },
        },
        backend: ComponentStatus {
            state: if running {
                "healthy"
            } else if relay_present {
                "degraded"
            } else {
                "stopped"
            }
            .into(),
            detail: if running {
                format!("CLIProxyAPI {GATEWAY_VERSION} responded to its authenticated health check")
            } else {
                backend_exit_reason
                    .clone()
                    .unwrap_or_else(|| "Backend is not ready".into())
            },
        },
        credentials: ComponentStatus {
            state: if active_label.is_some() {
                "selected"
            } else {
                "missing"
            }
            .into(),
            detail: active_label
                .map(|label| format!("{label} selected"))
                .unwrap_or_else(|| "No active credential".into()),
        },
        route: ComponentStatus {
            state: if active_provider.is_some() {
                "ready"
            } else {
                "waiting"
            }
            .into(),
            detail: route_detail.clone(),
        },
        oauth: ComponentStatus {
            state: login
                .as_ref()
                .map(|status| status.state.clone())
                .unwrap_or_else(|| "idle".into()),
            detail: login
                .as_ref()
                .map(|status| status.detail.clone())
                .unwrap_or_else(|| "No provider login has run in this session".into()),
        },
        claude: ComponentStatus {
            state: if claude_running { "running" } else { "stopped" }.into(),
            detail: if claude_running {
                "The isolated Basiliskos Claude process is running".into()
            } else {
                "The isolated Basiliskos Claude process is stopped".into()
            },
        },
        codex: ComponentStatus {
            state: if codex_running { "running" } else { "stopped" }.into(),
            detail: if codex_running {
                format!(
                    "The isolated Basiliskos Codex is running on {}",
                    route_detail.clone()
                )
            } else {
                "The isolated Basiliskos Codex process is stopped".into()
            },
        },
        backend_exit_reason,
        active_requests,
        diagnostics: diagnostics::snapshot(),
        login,
        skip_model_switch_confirmation: state.skip_model_switch_confirmation,
        open_claude_on_launch: state.open_claude_on_launch,
    })
}

fn usage_http_error_message(provider: &str, status: reqwest::StatusCode) -> String {
    match (provider, status.as_u16()) {
        ("kimi", 402 | 403) => "No active Kimi Code subscription".into(),
        (_, 401 | 403) => {
            "Usage check unavailable Ã¢â‚¬â€ saved login is active. Auto-retry in 5 min or use Refresh usage."
                .into()
        }
        (_, code) => format!(
            "Usage service returned {code}. Auto-retry in 5 min or use Refresh usage."
        ),
    }
}

fn should_refresh_after_usage_denial(provider: &str, status: reqwest::StatusCode) -> bool {
    status == reqwest::StatusCode::UNAUTHORIZED && matches!(provider, "codex" | "claude")
}

fn usage_refresh_failure_message(provider: &str, refresh_error: &str) -> String {
    if refresh_error.starts_with("Sign in again") {
        format!("{provider} refresh grant was revoked. Re-login once to restore automatic refresh.")
    } else {
        "Usage refresh failed temporarily. Auto-retry in 5 min or use Refresh usage.".into()
    }
}

struct UsageFetchError {
    status: Option<reqwest::StatusCode>,
    message: String,
}

async fn fetch_usage_json(
    provider: &str,
    token: &str,
    account_id: Option<&str>,
) -> Result<Value, UsageFetchError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("Basiliskos/1.1")
        .build()
        .map_err(|error| UsageFetchError {
            status: None,
            message: format!("Could not prepare usage request: {error}"),
        })?;
    let mut request = match provider {
        "claude" => client
            .get(CLAUDE_USAGE_URL)
            .bearer_auth(token)
            .header("anthropic-beta", "oauth-2025-04-20"),
        "codex" => {
            let request = client.get(CODEX_USAGE_URL).bearer_auth(token);
            if let Some(account_id) = account_id {
                request.header("ChatGPT-Account-Id", account_id)
            } else {
                request
            }
        }
        "xai" => client.get(XAI_USAGE_URL).bearer_auth(token),
        "kimi" => client.get(KIMI_USAGE_URL).bearer_auth(token),
        _ => {
            return Err(UsageFetchError {
                status: None,
                message: "Unsupported usage provider".into(),
            })
        }
    };
    request = request.header("Accept", "application/json");
    let response = request.send().await.map_err(|error| UsageFetchError {
        status: None,
        message: format!("Usage request failed; Basiliskos will retry automatically: {error}"),
    })?;
    if !response.status().is_success() {
        return Err(UsageFetchError {
            status: Some(response.status()),
            message: usage_http_error_message(provider, response.status()),
        });
    }
    response.json().await.map_err(|error| UsageFetchError {
        status: None,
        message: format!(
            "Usage response was invalid; Basiliskos will retry automatically: {error}"
        ),
    })
}

fn load_usage_credential(
    file_name: &str,
) -> Result<(GatewayAccount, String, Option<String>), String> {
    let _mutation = mutation_lock()?;
    let path = exact_auth_path(file_name)?;
    let state = load_state()?;
    let account = list_accounts_inner(&state)?
        .into_iter()
        .find(|account| account.file_name == file_name)
        .ok_or("Account not found")?;
    if account.provider == "antigravity" {
        return Err(
            "Antigravity quota usage is managed in the Google Cloud / Gemini developer console."
                .into(),
        );
    }
    if account.provider == "zai" {
        return Err("GLM Coding Plan quota is managed in the Z.AI console.".into());
    }
    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read account credentials: {error}"))?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("Account credentials are invalid: {error}"))?;
    let token = nested_string(&value, &["access_token"]).ok_or("Sign in again to refresh usage")?;
    let account_id = nested_string(&value, &["account_id"]);
    Ok((account, token, account_id))
}

#[tauri::command]
pub async fn get_gateway_account_usage(file_name: String) -> Result<GatewayAccountUsage, String> {
    let (mut account, token, account_id) = load_usage_credential(&file_name)?;
    let usage = match fetch_usage_json(&account.provider, &token, account_id.as_deref()).await {
        Ok(usage) => usage,
        Err(error)
            if error.status.is_some_and(|status| {
                should_refresh_after_usage_denial(&account.provider, status)
            }) =>
        {
            let refresh_result = match account.provider.as_str() {
                "codex" => refresh_codex_credential(&file_name, true).await,
                "claude" => refresh_claude_credential(&file_name, true).await,
                _ => unreachable!(),
            };
            if let Err(refresh_error) = refresh_result {
                return Err(usage_refresh_failure_message(
                    &account.provider,
                    &refresh_error,
                ));
            }
            let refreshed = load_usage_credential(&file_name)?;
            account = refreshed.0;
            fetch_usage_json(&account.provider, &refreshed.1, refreshed.2.as_deref())
                .await
                .map_err(|error| error.message)?
        }
        Err(error) => return Err(error.message),
    };
    let windows = match account.provider.as_str() {
        "claude" => parse_claude_usage(&usage),
        "codex" => parse_codex_usage(&usage),
        "xai" => parse_xai_usage(&usage),
        "kimi" => parse_kimi_usage(&usage),
        _ => Vec::new(),
    };
    if windows.is_empty() {
        return Err(
            "Provider did not report usage. Auto-retry in 5 min or use Refresh usage.".into(),
        );
    }
    Ok(GatewayAccountUsage {
        file_name,
        provider: account.provider,
        windows,
    })
}

#[tauri::command]
pub fn rename_gateway_account(file_name: String, name: String) -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    let path = exact_account_path(&file_name)?;
    if !path.is_file() {
        return Err("Account not found".into());
    }
    let state = load_state()?;
    if !list_accounts_inner(&state)?
        .iter()
        .any(|account| account.file_name == file_name)
    {
        return Err("Unsupported account file".into());
    }
    let label = normalized_account_label(&name)?;
    let mut labels = load_account_labels()?;
    labels.insert(file_name, label);
    save_account_labels(&labels)?;
    gateway_snapshot_locked()
}

pub(crate) fn exact_auth_path(file_name: &str) -> Result<PathBuf, String> {
    let supplied = Path::new(file_name);
    if supplied.file_name().and_then(|value| value.to_str()) != Some(file_name)
        || supplied.components().count() != 1
        || supplied.extension().and_then(|value| value.to_str()) != Some("json")
    {
        return Err("Invalid account filename".into());
    }
    Ok(auth_dir()?.join(file_name))
}

/// Resolve an account file name to its on-disk path, searching the OAuth auth
/// dir first and then the API-key dir. Used wherever an account may be either
/// auth flavor.
pub(crate) fn exact_account_path(file_name: &str) -> Result<PathBuf, String> {
    exact_auth_path(file_name)?; // validates the filename shape
    let auth_path = auth_dir()?.join(file_name);
    if auth_path.is_file() {
        return Ok(auth_path);
    }
    let keys_path = keys_dir()?.join(file_name);
    if keys_path.is_file() {
        return Ok(keys_path);
    }
    Ok(auth_path)
}

/// The directory for a given auth flavor (used by removal transactions).
fn account_directory_for(auth: &str) -> Result<PathBuf, String> {
    if auth == "api_key" {
        keys_dir()
    } else {
        auth_dir()
    }
}

fn account_bytes_with_disabled(path: &Path, disabled: bool) -> Result<Vec<u8>, String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let mut value: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("Account file {} is invalid: {error}", path.display()))?;
    let object = value
        .as_object_mut()
        .ok_or("Account file must contain a JSON object")?;
    object.insert("disabled".into(), Value::Bool(disabled));
    serde_json::to_vec_pretty(&value)
        .map_err(|error| format!("Could not serialize account: {error}"))
}

fn oauth_refresh_required(credential: &Value, now: DateTime<Utc>) -> bool {
    credential_expiry(credential)
        .is_none_or(|expiry| expiry <= now + ChronoDuration::seconds(OAUTH_REFRESH_SKEW_SECS))
}

fn apply_oauth_refresh(
    credential: &mut Value,
    refreshed: &Value,
    provider: &str,
) -> Result<(), String> {
    let access_token = refreshed
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or_else(|| format!("{provider} credential refresh returned no access token"))?;
    let object = credential
        .as_object_mut()
        .ok_or_else(|| format!("{provider} credential is invalid"))?;
    object.insert(
        "access_token".into(),
        Value::String(access_token.to_string()),
    );
    for key in ["refresh_token", "id_token", "token_type"] {
        if let Some(value) = refreshed
            .get(key)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
        {
            object.insert(key.into(), Value::String(value.to_string()));
        }
    }
    let expires_in = refreshed
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|seconds| *seconds > 0)
        .unwrap_or(60 * 60);
    let now = Utc::now();
    object.insert(
        "expired".into(),
        Value::String((now + ChronoDuration::seconds(expires_in)).to_rfc3339()),
    );
    object.insert("last_refresh".into(), Value::String(now.to_rfc3339()));
    Ok(())
}

fn oauth_refresh_http_error(provider: &str, status: reqwest::StatusCode) -> String {
    match status.as_u16() {
        400 | 401 | 403 => format!("Sign in again to renew this {provider} authorization"),
        429 => format!("{provider} credential refresh is temporarily rate-limited"),
        code => format!("{provider} credential refresh returned {code}"),
    }
}

async fn refresh_codex_credential(file_name: &str, force: bool) -> Result<bool, String> {
    let path = exact_auth_path(file_name)?;
    let refresh_lock = oauth_refresh_lock(&CODEX_REFRESH_LOCKS, file_name, "Codex")?;
    let _refresh = refresh_lock.lock().await;
    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read Codex credential: {error}"))?;
    let mut credential: Value =
        serde_json::from_str(&raw).map_err(|_| "Codex credential is invalid")?;
    if account_provider(&credential, file_name).as_deref() != Some("codex")
        || (!force && !oauth_refresh_required(&credential, Utc::now()))
    {
        return Ok(false);
    }
    let refresh_token = credential
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or("Sign in again to renew this Codex authorization")?;
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("client_id", CODEX_CLIENT_ID)
        .append_pair("grant_type", "refresh_token")
        .append_pair("refresh_token", refresh_token)
        .append_pair("scope", "openid profile email")
        .finish();
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("Basiliskos/2.2")
        .build()
        .map_err(|_| "Could not prepare Codex credential refresh")?
        .post(CODEX_TOKEN_ENDPOINT)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .header(reqwest::header::ACCEPT, "application/json")
        .body(form)
        .send()
        .await
        .map_err(|_| "Codex credential refresh could not reach OpenAI")?;
    if !response.status().is_success() {
        return Err(oauth_refresh_http_error("Codex", response.status()));
    }
    let refreshed: Value = response
        .json()
        .await
        .map_err(|_| "Codex credential refresh returned an invalid response")?;
    apply_oauth_refresh(&mut credential, &refreshed, "Codex")?;
    durable_write(
        &path,
        &serde_json::to_vec_pretty(&credential)
            .map_err(|_| "Could not save refreshed Codex credential")?,
    )?;
    Ok(true)
}

async fn refresh_claude_credential(file_name: &str, force: bool) -> Result<bool, String> {
    let path = exact_auth_path(file_name)?;
    let refresh_lock = oauth_refresh_lock(&CLAUDE_REFRESH_LOCKS, file_name, "Claude")?;
    let _refresh = refresh_lock.lock().await;
    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read Claude credential: {error}"))?;
    let mut credential: Value =
        serde_json::from_str(&raw).map_err(|_| "Claude credential is invalid")?;
    if account_provider(&credential, file_name).as_deref() != Some("claude")
        || (!force && !oauth_refresh_required(&credential, Utc::now()))
    {
        return Ok(false);
    }
    let refresh_token = credential
        .get("refresh_token")
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
        .ok_or("Sign in again to renew this Claude authorization")?;
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("Basiliskos/2.2")
        .build()
        .map_err(|_| "Could not prepare Claude credential refresh")?
        .post(CLAUDE_TOKEN_ENDPOINT)
        .header(reqwest::header::ACCEPT, "application/json")
        .json(&serde_json::json!({
            "client_id": CLAUDE_CLIENT_ID,
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
        }))
        .send()
        .await
        .map_err(|_| "Claude credential refresh could not reach Anthropic")?;
    if !response.status().is_success() {
        return Err(oauth_refresh_http_error("Claude", response.status()));
    }
    let refreshed: Value = response
        .json()
        .await
        .map_err(|_| "Claude credential refresh returned an invalid response")?;
    apply_oauth_refresh(&mut credential, &refreshed, "Claude")?;
    durable_write(
        &path,
        &serde_json::to_vec_pretty(&credential)
            .map_err(|_| "Could not save refreshed Claude credential")?,
    )?;
    Ok(true)
}

const XAI_REFRESH_SKEW_SECS: i64 = 5 * 60;

fn xai_refresh_required(credential: &Value, now: DateTime<Utc>) -> bool {
    let expiry = credential
        .get("expired")
        .or_else(|| credential.get("expires_at"))
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|value| value.with_timezone(&Utc));
    expiry
        .map(|value| value <= now + ChronoDuration::seconds(XAI_REFRESH_SKEW_SECS))
        .unwrap_or(true)
}

fn xai_refresh_client_id(access_token: &str) -> Option<String> {
    use base64::Engine;
    let payload = access_token.split('.').nth(1)?;
    let claims: Value = serde_json::from_slice(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(payload)
            .ok()?,
    )
    .ok()?;
    claims
        .get("client_id")
        .and_then(Value::as_str)
        .or_else(|| claims.get("aud").and_then(Value::as_str))
        .map(str::to_string)
}

fn xai_refresh_endpoint(credential: &Value) -> Result<String, String> {
    let raw = credential
        .get("token_endpoint")
        .and_then(Value::as_str)
        .ok_or("Grok credential is missing its refresh endpoint")?;
    let url =
        reqwest::Url::parse(raw).map_err(|_| "Grok credential has an invalid refresh endpoint")?;
    let trusted_host = matches!(url.host_str(), Some("auth.x.ai") | Some("accounts.x.ai"));
    if url.scheme() != "https" || !trusted_host || url.path().is_empty() {
        return Err("Grok credential has an untrusted refresh endpoint".into());
    }
    Ok(url.to_string())
}

/// Refreshes an xAI relay credential only when it is close to expiry. This is
/// intentionally owned by Basiliskos: CLIProxyAPI may also refresh active
/// credentials, but Basiliskos must be able to make a parked account usable
/// before it becomes the live relay or Grok CLI credential.
pub async fn refresh_xai_relay_credential_if_needed(file_name: &str) -> Result<bool, String> {
    let path = exact_auth_path(file_name)?;
    let refresh_lock = xai_refresh_lock(file_name)?;
    let _refresh = refresh_lock.lock().await;

    if xai_relogin_required(file_name)? {
        return Err(XAI_RELOGIN_REQUIRED.into());
    }

    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read Grok credential: {error}"))?;
    let mut credential: Value =
        serde_json::from_str(&raw).map_err(|_| "Grok credential is invalid")?;
    if account_provider(&credential, file_name).as_deref() != Some("xai") {
        return Ok(false);
    }
    if !xai_refresh_required(&credential, Utc::now()) {
        return Ok(false);
    }

    let access_token = credential
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or("Grok credential is missing an access token")?;
    let refresh_token = credential
        .get("refresh_token")
        .and_then(Value::as_str)
        .ok_or("Grok credential is missing a refresh token")?;
    let client_id = xai_refresh_client_id(access_token)
        .ok_or("Grok credential is missing its OAuth client identity")?;
    let endpoint = xai_refresh_endpoint(&credential)?;
    let refresh_form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("client_id", &client_id)
        .append_pair("refresh_token", refresh_token)
        .finish();
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("Basiliskos/1.1")
        .build()
        .map_err(|_| "Could not prepare Grok credential refresh")?
        .post(endpoint)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(refresh_form)
        .send()
        .await
        .map_err(|_| "Grok credential refresh could not reach xAI")?;
    if !response.status().is_success() {
        // Do not expose response bodies: OAuth errors can echo sensitive data.
        let status = response.status();
        let code = response.json::<Value>().await.ok().and_then(|body| {
            body.get("error")
                .or_else(|| body.get("code"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        if xai_refresh_error_requires_relogin(code.as_deref()) {
            set_xai_relogin_required(file_name, true)?;
            return Err(XAI_RELOGIN_REQUIRED.into());
        }
        return Err(if status.is_client_error() {
            "Grok credential refresh was rejected temporarily. It will retry automatically.".into()
        } else {
            "Grok credential refresh is temporarily unavailable. It will retry automatically."
                .into()
        });
    }
    let refreshed: Value = response
        .json()
        .await
        .map_err(|_| "Grok credential refresh returned an invalid response")?;
    let new_access = refreshed
        .get("access_token")
        .and_then(Value::as_str)
        .ok_or("Grok credential refresh returned no access token")?;
    let expires_in = refreshed
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|seconds| *seconds > 0)
        .unwrap_or(XAI_DEFAULT_TOKEN_LIFETIME_SECS);
    let object = credential
        .as_object_mut()
        .ok_or("Grok credential is invalid")?;
    object.insert("access_token".into(), Value::String(new_access.to_string()));
    if let Some(value) = refreshed.get("refresh_token").and_then(Value::as_str) {
        object.insert("refresh_token".into(), Value::String(value.to_string()));
    }
    if let Some(value) = refreshed.get("id_token").and_then(Value::as_str) {
        object.insert("id_token".into(), Value::String(value.to_string()));
    }
    if let Some(value) = refreshed.get("token_type").and_then(Value::as_str) {
        object.insert("token_type".into(), Value::String(value.to_string()));
    }
    let now = Utc::now();
    object.insert("expires_in".into(), Value::from(expires_in));
    object.insert(
        "expired".into(),
        Value::String((now + ChronoDuration::seconds(expires_in)).to_rfc3339()),
    );
    object.insert("last_refresh".into(), Value::String(now.to_rfc3339()));
    let bytes = serde_json::to_vec_pretty(&credential)
        .map_err(|_| "Could not save refreshed Grok credential")?;
    durable_write(&path, &bytes)?;
    set_xai_relogin_required(file_name, false)?;
    Ok(true)
}

fn saved_xai_credential_file_names() -> Result<Vec<String>, String> {
    let directory = auth_dir()?;
    let mut names = Vec::new();
    for entry in fs::read_dir(&directory)
        .map_err(|error| format!("Could not inspect saved Grok credentials: {error}"))?
    {
        let entry =
            entry.map_err(|error| format!("Could not inspect saved Grok credentials: {error}"))?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let Ok(raw) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if account_provider(&value, name).as_deref() == Some("xai") {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

async fn maintain_saved_xai_credentials_once() {
    let Ok(names) = saved_xai_credential_file_names() else {
        return;
    };
    for name in names {
        if xai_relogin_required(&name).unwrap_or(false) {
            continue;
        }
        match refresh_xai_relay_credential_if_needed(&name).await {
            Ok(true) => {
                if crate::grok_cli::sync_grok_cli_account_from_relay(&name).is_err() {
                    diagnostics::record(
                        ErrorCode::ConfigTransactionFailed,
                        "warning",
                        "Grok CLI vault could not be synchronized after a credential refresh.",
                        None,
                        None,
                        Some("xai"),
                    );
                }
            }
            Ok(false) => {}
            Err(error) => {
                diagnostics::record(
                    ErrorCode::ProviderAuthFailed,
                    "warning",
                    if error == XAI_RELOGIN_REQUIRED {
                        "A saved Grok authorization was revoked and needs one re-login."
                    } else {
                        "A saved Grok authorization refresh failed temporarily and will retry automatically."
                    },
                    None,
                    None,
                    Some("xai"),
                );
            }
        }
    }
}

fn start_xai_credential_maintenance() {
    if XAI_CREDENTIAL_MAINTENANCE_STARTED.set(()).is_err() {
        return;
    }
    tauri::async_runtime::spawn(async {
        loop {
            maintain_saved_xai_credentials_once().await;
            tokio::time::sleep(XAI_CREDENTIAL_MAINTENANCE_INTERVAL).await;
        }
    });
}

fn kimi_refresh_required(credential: &Value, now: DateTime<Utc>) -> bool {
    credential
        .get("expired")
        .and_then(Value::as_str)
        .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
        .map(|expiry| {
            expiry.with_timezone(&Utc) <= now + ChronoDuration::seconds(KIMI_REFRESH_SKEW_SECS)
        })
        .unwrap_or(true)
}

async fn refresh_kimi_relay_credential_if_needed(file_name: &str) -> Result<bool, String> {
    let path = exact_auth_path(file_name)?;
    let refresh_lock = kimi_refresh_lock(file_name)?;
    let _refresh = refresh_lock.lock().await;
    if kimi_relogin_required(file_name)? {
        return Err(KIMI_RELOGIN_REQUIRED.into());
    }
    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read Kimi credential: {error}"))?;
    let mut credential: Value =
        serde_json::from_str(&raw).map_err(|_| "Kimi credential is invalid")?;
    if account_provider(&credential, file_name).as_deref() != Some("kimi")
        || !kimi_refresh_required(&credential, Utc::now())
    {
        return Ok(false);
    }
    let refresh_token = credential
        .get("refresh_token")
        .and_then(Value::as_str)
        .ok_or("Kimi credential is missing a refresh token")?;
    let device_id = credential
        .get("device_id")
        .and_then(Value::as_str)
        .unwrap_or("");
    let form = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("grant_type", "refresh_token")
        .append_pair("client_id", KIMI_CLIENT_ID)
        .append_pair("refresh_token", refresh_token)
        .finish();
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .user_agent("Basiliskos/2.0")
        .build()
        .map_err(|_| "Could not prepare Kimi credential refresh")?
        .post(KIMI_TOKEN_ENDPOINT)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .header("X-Msh-Platform", "CLIProxyAPI")
        .header("X-Msh-Version", GATEWAY_VERSION)
        .header("X-Msh-Device-Id", device_id)
        .header(
            "X-Msh-Device-Name",
            std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Basiliskos".into()),
        )
        .header("X-Msh-Device-Model", "Windows x86_64")
        .body(form)
        .send()
        .await
        .map_err(|_| "Kimi credential refresh could not reach Kimi")?;
    let status = response.status();
    let refreshed: Value = response.json().await.unwrap_or(Value::Null);
    let code = refreshed
        .get("error")
        .or_else(|| refreshed.get("code"))
        .and_then(Value::as_str);
    if !status.is_success()
        || refreshed
            .get("access_token")
            .and_then(Value::as_str)
            .is_none()
    {
        if kimi_refresh_error_requires_relogin(status, code) {
            set_kimi_relogin_required(file_name, true)?;
            return Err(KIMI_RELOGIN_REQUIRED.into());
        }
        return Err(
            "Kimi credential refresh failed temporarily. It will retry automatically.".into(),
        );
    }
    let object = credential
        .as_object_mut()
        .ok_or("Kimi credential is invalid")?;
    let access = refreshed
        .get("access_token")
        .and_then(Value::as_str)
        .expect("checked above");
    object.insert("access_token".into(), Value::String(access.to_owned()));
    for key in ["refresh_token", "token_type", "scope"] {
        if let Some(value) = refreshed.get(key).and_then(Value::as_str) {
            object.insert(key.into(), Value::String(value.to_owned()));
        }
    }
    let expires_in = refreshed
        .get("expires_in")
        .and_then(Value::as_i64)
        .filter(|value| *value > 0)
        .unwrap_or(KIMI_DEFAULT_TOKEN_LIFETIME_SECS);
    let now = Utc::now();
    object.insert(
        "expired".into(),
        Value::String((now + ChronoDuration::seconds(expires_in)).to_rfc3339()),
    );
    object.insert("last_refresh".into(), Value::String(now.to_rfc3339()));
    durable_write(
        &path,
        &serde_json::to_vec_pretty(&credential)
            .map_err(|_| "Could not save refreshed Kimi credential")?,
    )?;
    set_kimi_relogin_required(file_name, false)?;
    Ok(true)
}

async fn maintain_saved_kimi_credentials_once() {
    let Ok(directory) = auth_dir() else {
        return;
    };
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if path.extension().and_then(|value| value.to_str()) != Some("json")
            || kimi_relogin_required(name).unwrap_or(false)
        {
            continue;
        }
        let is_kimi = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|value| account_provider(&value, name))
            .as_deref()
            == Some("kimi");
        if !is_kimi {
            continue;
        }
        if let Err(error) = refresh_kimi_relay_credential_if_needed(name).await {
            diagnostics::record(
                ErrorCode::ProviderAuthFailed,
                "warning",
                if error == KIMI_RELOGIN_REQUIRED {
                    "A saved Kimi authorization was revoked and needs one re-login."
                } else {
                    "A saved Kimi authorization refresh failed temporarily and will retry automatically."
                },
                None,
                None,
                Some("kimi"),
            );
        }
    }
}

fn start_kimi_credential_maintenance() {
    if KIMI_CREDENTIAL_MAINTENANCE_STARTED.set(()).is_err() {
        return;
    }
    tauri::async_runtime::spawn(async {
        loop {
            maintain_saved_kimi_credentials_once().await;
            tokio::time::sleep(KIMI_CREDENTIAL_MAINTENANCE_INTERVAL).await;
        }
    });
}

fn saved_provider_credential_file_names(provider: &str) -> Result<Vec<String>, String> {
    let directory = auth_dir()?;
    let mut names = Vec::new();
    for entry in fs::read_dir(&directory)
        .map_err(|error| format!("Could not inspect saved {provider} credentials: {error}"))?
    {
        let entry = entry
            .map_err(|error| format!("Could not inspect a saved {provider} credential: {error}"))?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
            continue;
        };
        let is_provider = fs::read_to_string(&path)
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
            .and_then(|value| account_provider(&value, name))
            .as_deref()
            == Some(provider);
        if is_provider {
            names.push(name.to_string());
        }
    }
    Ok(names)
}

async fn maintain_saved_oauth_credentials_once() {
    for provider in ["codex", "claude"] {
        let Ok(names) = saved_provider_credential_file_names(provider) else {
            continue;
        };
        for name in names {
            // One-refresher rule: the isolated Codex window owns the anchor
            // account's credential. The relay must not auto-refresh it, or the
            // rotation would invalidate the seeded login every maintenance cycle.
            if provider == "codex" && name == CODEX_ANCHOR_FILE_NAME {
                if let (Ok(isolated_home), Ok(auth_dir)) = (isolated_codex_home(), auth_dir()) {
                    let isolated_auth = isolated_home.join("auth.json");
                    let anchor_path = auth_dir.join(CODEX_ANCHOR_FILE_NAME);
                    if let (Ok(iso_raw), Ok(anc_raw)) = (
                        fs::read_to_string(&isolated_auth),
                        fs::read_to_string(&anchor_path),
                    ) {
                        if let (Ok(iso_val), Ok(mut anc_val)) = (
                            serde_json::from_str::<Value>(&iso_raw),
                            serde_json::from_str::<Value>(&anc_raw),
                        ) {
                            let new_token = iso_val
                                .get("tokens")
                                .and_then(|t| t.get("access_token"))
                                .or_else(|| iso_val.get("access_token"))
                                .and_then(Value::as_str);
                            if let Some(token) = new_token {
                                if anc_val.get("access_token").and_then(Value::as_str)
                                    != Some(token)
                                {
                                    if let Some(obj) = anc_val.as_object_mut() {
                                        obj.insert(
                                            "access_token".into(),
                                            Value::String(token.to_string()),
                                        );
                                        for key in ["expired", "expires_at", "expiresAt", "expiry"]
                                        {
                                            obj.remove(key);
                                        }
                                        if let Some(rt) = iso_val
                                            .get("tokens")
                                            .and_then(|t| t.get("refresh_token"))
                                            .and_then(Value::as_str)
                                        {
                                            obj.insert(
                                                "refresh_token".into(),
                                                Value::String(rt.to_string()),
                                            );
                                        }
                                        if let Some(id) = iso_val
                                            .get("tokens")
                                            .and_then(|t| t.get("id_token"))
                                            .and_then(Value::as_str)
                                        {
                                            obj.insert(
                                                "id_token".into(),
                                                Value::String(id.to_string()),
                                            );
                                        }
                                        if let Some(expiry) = iso_val
                                            .get("tokens")
                                            .and_then(|tokens| tokens.get("expires_at"))
                                            .or_else(|| iso_val.get("expires_at"))
                                        {
                                            obj.insert("expires_at".into(), expiry.clone());
                                        }
                                        if let Ok(bytes) = serde_json::to_vec_pretty(&anc_val) {
                                            let _ = durable_write(&anchor_path, &bytes);
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
                continue;
            }
            let result = match provider {
                "codex" => refresh_codex_credential(&name, false).await,
                "claude" => refresh_claude_credential(&name, false).await,
                _ => unreachable!(),
            };
            if let Err(error) = result {
                diagnostics::record(
                    ErrorCode::ProviderAuthFailed,
                    "warning",
                    if error.starts_with("Sign in again") {
                        "A saved OAuth authorization was rejected and needs one re-login."
                    } else {
                        "A saved OAuth authorization refresh failed temporarily and will retry automatically."
                    },
                    None,
                    None,
                    Some(provider),
                );
            }
        }
    }
}

fn start_oauth_credential_maintenance() {
    if OAUTH_CREDENTIAL_MAINTENANCE_STARTED.set(()).is_err() {
        return;
    }
    tauri::async_runtime::spawn(async {
        loop {
            maintain_saved_oauth_credentials_once().await;
            tokio::time::sleep(OAUTH_CREDENTIAL_MAINTENANCE_INTERVAL).await;
        }
    });
}

/// Reads the newest Claude Code session file under the isolated profile and
/// returns the window's current (model alias, effort) selection. Claude writes
/// this file on every session activity, so the newest file reflects the live
/// picker + effort controls. Returns None when no session file exists.
fn newest_claude_session_choice() -> Option<(String, String)> {
    let sessions_root = isolated_claude_profile_dir()
        .ok()?
        .join("claude-code-sessions");
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for account in fs::read_dir(&sessions_root).ok()? {
        let account = account.ok()?;
        if !account.path().is_dir() {
            continue;
        }
        for bucket in fs::read_dir(account.path()).ok()? {
            let bucket = bucket.ok()?;
            if !bucket.path().is_dir() {
                continue;
            }
            for entry in fs::read_dir(bucket.path()).ok()? {
                let entry = entry.ok()?;
                if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                    continue;
                }
                let modified = entry.metadata().ok()?.modified().ok()?;
                if newest
                    .as_ref()
                    .is_none_or(|(existing, _)| modified > *existing)
                {
                    newest = Some((modified, entry.path()));
                }
            }
        }
    }
    let (_, path) = newest?;
    let value: Value = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    let model = value.get("model").and_then(Value::as_str)?;
    let effort = value
        .get("effort")
        .and_then(Value::as_str)
        .unwrap_or("auto");
    Some((model.to_string(), effort.to_string()))
}

/// Maps a Claude effort level to a thinking level the routed model supports.
/// Grok 4.5 keeps the desktop-effort remap (mediumÃ¢â€ â€™low, high/xhigh/maxÃ¢â€ â€™high).
fn effort_to_thinking(provider: &str, model: &str, effort: &str) -> String {
    if effort == "auto" || effort == "none" {
        return "auto".to_string();
    }
    if provider == "xai" && model == "grok-4.5" {
        return match effort {
            "low" | "medium" => "low".to_string(),
            "high" | "xhigh" | "max" => "high".to_string(),
            _ => "auto".to_string(),
        };
    }
    let supported = model_specs(provider)
        .iter()
        .find(|spec| spec.id == model)
        .is_some_and(|spec| spec.thinking_levels.contains(&effort));
    if supported {
        effort.to_string()
    } else {
        "auto".to_string()
    }
}

/// Applies the isolated window's live model/effort selection to the Basiliskos
/// route so the GUI stays in real-time sync with the picker, even before any
/// request is sent. Only acts while the Claude window is running; skips when
/// the selection is the generic routing alias or matches the route already.
fn sync_route_from_claude_session() {
    let Ok(_mutation) = mutation_lock() else {
        return;
    };
    if !hydra_claude_running() {
        return;
    }
    let Some((model, effort)) = newest_claude_session_choice() else {
        return;
    };
    let Ok(mut state) = load_state() else { return };
    let Some(active) = state.active_account.clone() else {
        return;
    };
    let Ok(accounts) = list_accounts_inner(&state) else {
        return;
    };
    let Some(account) = accounts.iter().find(|account| account.file_name == active) else {
        return;
    };
    let provider = account.provider.clone();
    let default_model = default_model(&provider);
    let selected = state
        .routes
        .get(&provider)
        .map(|route| route.model.as_str())
        .unwrap_or(default_model);
    let Some((upstream, entry_thinking)) = alias_to_picker_entry(&provider, &model, selected)
    else {
        return; // generic routing alias or unknown Ã¢â‚¬â€ leave the route alone
    };
    // A variant picker entry carries its thinking; the session effort field
    // (when present) overrides it.
    let thinking = match effort_to_thinking(&provider, &upstream, &effort).as_str() {
        "auto" => entry_thinking,
        level => level.to_string(),
    };
    let current = state
        .routes
        .get(&provider)
        .map(|route| (route.model.as_str(), route.thinking.as_str()));
    if current == Some((upstream.as_str(), thinking.as_str())) {
        return;
    }
    let mut route = state
        .routes
        .get(&provider)
        .cloned()
        .unwrap_or(RouteSelection {
            model: default_model.to_string(),
            thinking: "auto".into(),
        });
    route.model = upstream.clone();
    route.thinking = thinking.clone();
    state.routes.insert(provider.clone(), route);
    let _ = save_state(&state);
    // A model change regenerates the Claude config so the picker follows.
    if let Ok(profile) = isolated_claude_profile_dir() {
        let _ = write_isolated_claude_config(&profile, &state);
    }
}

/// Parses the picker choice out of the Codex config.toml text.
fn parse_codex_config_toml(raw: &str) -> Option<(String, String)> {
    let mut model = None;
    let mut effort = "auto".to_string();
    for line in raw.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("model = ") {
            model = Some(rest.trim().trim_matches('"').to_string());
        } else if let Some(rest) = line.strip_prefix("model_reasoning_effort = ") {
            effort = rest.trim().trim_matches('"').to_string();
        }
    }
    model.map(|m| (m, effort))
}

fn codex_config_model_at(home: &Path) -> Option<(String, String)> {
    let raw = fs::read_to_string(home.join("config.toml")).ok()?;
    parse_codex_config_toml(&raw)
}

/// Reads the isolated Codex window's current picker choice from its
/// config.toml (`model = "..."`, `model_reasoning_effort = "..."`). The app
/// persists the picker selection there on change; the encrypted dial body is
/// not visible to the relay, so this file is the sync signal.
fn codex_config_model() -> Option<(String, String)> {
    let home = isolated_codex_home().ok()?;
    codex_config_model_at(&home)
}

/// Finds the Basiliskos provider that advertises the given model id.
fn model_to_provider(model: &str) -> Option<&'static str> {
    SUPPORTED_PROVIDERS
        .into_iter()
        .find(|provider| model_specs(provider).iter().any(|spec| spec.id == model))
}

/// Chooses the account to make active when the Codex window picks a model of
/// a different provider: the already-enabled account of that provider (the
/// per-provider pin), else the first non-expired account, else the first.
fn pick_codex_sync_account<'a>(
    accounts: &'a [GatewayAccount],
    provider: &str,
) -> Option<&'a GatewayAccount> {
    let candidates = accounts
        .iter()
        .filter(|account| account.provider == provider)
        .collect::<Vec<_>>();
    candidates
        .iter()
        .find(|account| account.active_for_codex && !account.disabled)
        .or_else(|| candidates.iter().find(|account| !account.disabled))
        .or_else(|| {
            candidates.iter().find(|account| {
                !matches!(
                    account.credential_status.as_str(),
                    "expired" | "relogin_required"
                )
            })
        })
        .or_else(|| candidates.first())
        .copied()
}

/// Refreshes the model catalog cache for the model currently selected in the
/// isolated Codex window without mutating Claude's active account or route.
fn sync_route_from_codex_window(_app: &AppHandle) {
    let Ok(_mutation) = mutation_lock() else {
        return;
    };
    if !hydra_codex_running() {
        return;
    }
    let Some((model, _effort)) = codex_config_model() else {
        return;
    };
    let Some(provider) = model_to_provider(&model) else {
        return;
    };
    let Ok(mut state) = load_state() else { return };
    let Ok(accounts) = list_accounts_inner(&state) else {
        return;
    };
    if let Some(account) = pick_codex_sync_account(&accounts, provider) {
        let selected = account.file_name.clone();
        if state.active_codex_account.as_deref() != Some(selected.as_str()) {
            let active_provider = state
                .active_codex_account
                .as_deref()
                .and_then(|file| accounts.iter().find(|account| account.file_name == file))
                .map(|account| account.provider.as_str());
            if active_provider.is_none() || active_provider == Some(provider) {
                state.active_codex_account = Some(selected);
            }
        }
    }
    if let Some(route) = state.codex_routes.get_mut(provider) {
        let spec = model_specs(provider).iter().find(|spec| spec.id == model);
        if let Some(spec) = spec {
            route.model = spec.id.to_string();
            if route.thinking != "auto" && !spec.thinking_levels.contains(&route.thinking.as_str())
            {
                route.thinking = "auto".to_string();
            }
        }
    }
    let _ = save_state(&state);
    if let Ok(home) = isolated_codex_home() {
        let _ = write_isolated_codex_config(&home, &state);
    }
    refresh_model_catalog_cache(provider, &state.api_key);
}

/// Runs backend crash recovery on a fixed timer instead of only on idle
/// listener ticks. The listener still calls `supervise_backend` on idle, so a
/// crash during a request burst is now detected within one tick regardless of
/// traffic. `supervise_backend` takes the mutation lock with `try_lock`, so a
/// concurrent mutation simply skips that tick.
pub fn start_backend_supervision(app: AppHandle) {
    if BACKEND_SUPERVISION_STARTED.set(()).is_err() {
        return;
    }
    tauri::async_runtime::spawn(async move {
        loop {
            tokio::time::sleep(BACKEND_SUPERVISION_INTERVAL).await;
            supervise_backend(&app);
            // Each client owns a separate route. A Codex window must not pause
            // synchronization of a live Claude session.
            sync_route_from_claude_session();
            sync_route_from_codex_window(&app);
        }
    });
}

fn validate_account_invariant(directory: &Path, state_path: &Path) -> Result<(), String> {
    let state: ControllerState = serde_json::from_slice(
        &fs::read(state_path)
            .map_err(|error| format!("Could not validate {}: {error}", state_path.display()))?,
    )
    .map_err(|error| format!("Controller state failed transaction validation: {error}"))?;
    let mut enabled = Vec::new();
    let mut supported = Vec::new();
    let mut directories = vec![directory.to_path_buf()];
    if let Ok(keys) = keys_dir() {
        if !directories.contains(&keys) {
            directories.push(keys);
        }
    }
    for directory in &directories {
        // The API-key directory may not exist yet (fresh machine). A missing
        // account directory means "no accounts of that flavor" — skip it rather
        // than failing the whole transaction.
        let Ok(entries) = fs::read_dir(directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy().to_string();
            let value: Value = serde_json::from_slice(
                &fs::read(&path)
                    .map_err(|error| format!("Could not validate {}: {error}", path.display()))?,
            )
            .map_err(|error| {
                format!(
                    "Account {} failed transaction validation: {error}",
                    path.display()
                )
            })?;
            if account_provider(&value, &file_name).is_none() {
                continue;
            }
            let disabled = value
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            supported.push(file_name.clone());
            if !disabled {
                enabled.push(file_name);
            }
        }
    }
    if let Some(active) = state.active_account.as_deref() {
        if !supported.iter().any(|file| file == active) {
            return Err("The selected account disappeared during the transaction".into());
        }
        if !enabled.iter().any(|file| file == active) {
            return Err(format!(
                "Account transaction invariant failed: the selected account {active} is not enabled (enabled: {})",
                enabled.join(", ")
            ));
        }
    }
    if let Some(active) = state.active_codex_account.as_deref() {
        if !supported.iter().any(|file| file == active) {
            return Err("The selected Codex account disappeared during the transaction".into());
        }
        if !enabled.iter().any(|file| file == active) {
            return Err(format!(
                "Account transaction invariant failed: the selected Codex account {active} is not enabled (enabled: {})",
                enabled.join(", ")
            ));
        }
    }
    // The relay keeps at most one enabled account per provider. The selected
    // (active) account drives the Claude path, while the other providers' last-
    // selected accounts stay enabled so the isolated Codex window can route a
    // picked model to its real provider. Two enabled accounts of the SAME
    // provider would make CLIProxyAPI's credential selection ambiguous.
    let mut seen_provider = std::collections::BTreeSet::new();
    let enabled_list = enabled.join(", ");
    for file_name in &enabled {
        let provider = account_provider(
            &serde_json::from_slice::<Value>(&fs::read(directory.join(file_name)).map_err(
                |error| {
                    format!(
                        "Could not re-read {}: {error}",
                        directory.join(file_name).display()
                    )
                },
            )?)
            .map_err(|error| format!("Account {} failed re-validation: {error}", file_name))?,
            file_name,
        );
        let Some(provider) = provider else { continue };
        if !seen_provider.insert(provider.to_string()) {
            return Err(format!(
                "Account transaction invariant failed: more than one enabled account for provider {provider}: {enabled_list}"
            ));
        }
    }
    Ok(())
}

fn selection_transaction(
    root: &Path,
    directory: &Path,
    state_path: &Path,
    accounts: &[GatewayAccount],
    state: &ControllerState,
    client: ClientSurface,
    file_name: &str,
) -> Result<(Vec<FileMutation>, ControllerState), String> {
    let selected_provider = accounts
        .iter()
        .find(|account| account.file_name == file_name)
        .map(|account| account.provider.as_str())
        .ok_or_else(|| "Unsupported account file".to_string())?;
    let other_client = match client {
        ClientSurface::Claude => ClientSurface::Codex,
        ClientSurface::Codex => ClientSurface::Claude,
    };
    if let Some(other_file) = active_account_for(state, other_client) {
        let other_provider = accounts
            .iter()
            .find(|account| account.file_name == other_file)
            .map(|account| account.provider.as_str());
        if other_provider == Some(selected_provider) && other_file != file_name {
            return Err(format!(
                "Claude and Codex share one enabled {selected_provider} credential. Select the current account or switch the other client to another provider first."
            ));
        }
    }
    let mut mutations = Vec::with_capacity(accounts.len() + 1);
    for account in accounts {
        let selected = account.file_name == file_name;
        // The selected account becomes enabled; other accounts of the same
        // provider are disabled so CLIProxyAPI's per-provider credential
        // selection stays unambiguous. Accounts of OTHER providers keep their
        // current state: the isolated Codex window routes picked models to any
        // provider whose credential is enabled.
        let disabled = if selected {
            false
        } else if selected_provider == account.provider.as_str() {
            true
        } else {
            account.disabled
        };
        let account_path = if account.auth == "api_key" {
            keys_dir()?.join(&account.file_name)
        } else {
            directory.join(&account.file_name)
        };
        mutations.push(FileMutation {
            path: account_path.clone(),
            after: Some(account_bytes_with_disabled(&account_path, disabled)?),
        });
    }
    let mut after_state = state.clone();
    set_active_account_for(&mut after_state, client, Some(file_name.to_string()));
    mutations.push(FileMutation {
        path: state_path.to_path_buf(),
        after: Some(
            serde_json::to_vec_pretty(&after_state)
                .map_err(|error| format!("Could not serialize controller state: {error}"))?,
        ),
    });
    for mutation in &mutations {
        mutation
            .path
            .strip_prefix(root)
            .map_err(|_| format!("Refusing to transact outside {}", root.display()))?;
    }
    Ok((mutations, after_state))
}

#[derive(Clone, Copy)]
struct AccountPaths<'a> {
    root: &'a Path,
    directory: &'a Path,
    state: &'a Path,
    labels: &'a Path,
}

fn removal_transaction(
    paths: AccountPaths<'_>,
    _accounts: &[GatewayAccount],
    state: &ControllerState,
    labels: &BTreeMap<String, String>,
    file_name: &str,
) -> Result<(Vec<FileMutation>, ControllerState), String> {
    let removing_active = state.active_account.as_deref() == Some(file_name);
    let removing_codex_active = state.active_codex_account.as_deref() == Some(file_name);
    let mut mutations = vec![FileMutation {
        path: paths.directory.join(file_name),
        after: None,
    }];
    // Other providers' accounts stay enabled: the isolated Codex window may
    // still route picked models to them. Only the removed file and the state
    // change here.
    let mut next_labels = labels.clone();
    if next_labels.remove(file_name).is_some() {
        mutations.push(FileMutation {
            path: paths.labels.to_path_buf(),
            after: Some(
                serde_json::to_vec_pretty(&next_labels)
                    .map_err(|error| format!("Could not serialize profile names: {error}"))?,
            ),
        });
    }
    let mut after_state = state.clone();
    if removing_active {
        after_state.active_account = None;
    }
    if removing_codex_active {
        after_state.active_codex_account = None;
    }
    if removing_active || removing_codex_active {
        mutations.push(FileMutation {
            path: paths.state.to_path_buf(),
            after: Some(
                serde_json::to_vec_pretty(&after_state)
                    .map_err(|error| format!("Could not serialize controller state: {error}"))?,
            ),
        });
    }
    for mutation in &mutations {
        mutation
            .path
            .strip_prefix(paths.root)
            .map_err(|_| format!("Refusing to transact outside {}", paths.root.display()))?;
    }
    Ok((mutations, after_state))
}

// Picks the first same-provider account other than the one that just got
// rate-limited, skipping any that are themselves still cooling down.
fn pick_failover_candidate<'a>(
    accounts: &'a [GatewayAccount],
    rate_limited_account: &str,
    provider: &str,
    cooling: &BTreeMap<String, chrono::DateTime<Utc>>,
    now: chrono::DateTime<Utc>,
) -> Option<&'a GatewayAccount> {
    accounts.iter().find(|account| {
        account.provider == provider
            && account.file_name != rate_limited_account
            && !matches!(
                account.credential_status.as_str(),
                "expired" | "relogin_required"
            )
            && cooling
                .get(&account.file_name)
                .is_none_or(|until| *until <= now)
    })
}

// Best-effort, same-provider only: when the active account gets rate-limited,
// look for another account of the SAME provider that isn't itself cooling
// down and switch to it automatically. This reuses exactly the manual
// select_gateway_account transaction/invariant, it just picks the candidate
// itself instead of waiting for a click. Never touches the isolated Claude
// window/process Ã¢â‚¬â€ config only varies by provider, not by account, so the
// running Claude window is left alone and its next request simply lands on
// the new credential. Silently does nothing if any step fails or no eligible
// candidate exists; the caller (the relay's 429 path) still returns the
// original rate-limit response to the client either way.
fn attempt_same_provider_failover(
    rate_limited_account: &str,
    provider: &str,
    client: ClientSurface,
) {
    let Ok(_mutation) = mutation_lock() else {
        return;
    };
    let Ok(state) = load_state() else { return };
    let Ok(accounts) = list_accounts_inner(&state) else {
        return;
    };
    let now = Utc::now();
    let cooling = runtime_lock()
        .map(|runtime| runtime.account_cooldowns.clone())
        .unwrap_or_default();
    let Some(candidate) =
        pick_failover_candidate(&accounts, rate_limited_account, provider, &cooling, now)
    else {
        return;
    };
    let candidate_file = candidate.file_name.clone();
    let candidate_label = candidate.label.clone();
    let rate_limited_label = accounts
        .iter()
        .find(|account| account.file_name == rate_limited_account)
        .map(|account| account.label.clone())
        .unwrap_or_else(|| rate_limited_account.to_string());
    let Ok(root) = root_dir() else { return };
    let Ok(directory) = auth_dir() else { return };
    let Ok(state_path) = controller_path() else {
        return;
    };
    let Ok((mutations, new_state)) = selection_transaction(
        &root,
        &directory,
        &state_path,
        &accounts,
        &state,
        client,
        &candidate_file,
    ) else {
        return;
    };
    if run_transaction(&root, &mutations, || {
        validate_account_invariant(&directory, &state_path)
    })
    .is_err()
    {
        return;
    }
    if let Ok(mut runtime) = runtime_lock() {
        runtime.last_known_good_models.clear();
        runtime.last_auto_failover = Some(AutoFailoverInfo {
            from_label: rate_limited_label,
            to_label: candidate_label,
            at_ms: Utc::now().timestamp_millis(),
        });
    }
    let _ = prepare_config();
    if let Ok(profile) = isolated_claude_profile_dir() {
        let _ = write_isolated_claude_config(&profile, &new_state);
    }
    if let Ok(home) = isolated_codex_home() {
        let _ = write_isolated_codex_config(&home, &new_state);
    }
    refresh_model_catalog_cache(provider, &new_state.api_key);
    diagnostics::record(
        ErrorCode::AccountAutoFailover,
        "warning",
        "Automatically switched to another account of the same provider after a rate limit.",
        None,
        None,
        Some(provider),
    );
}

#[tauri::command]
pub async fn select_gateway_account(
    _app: AppHandle,
    file_name: String,
    client: Option<String>,
) -> Result<AccountSelectionResult, String> {
    let refreshed = refresh_xai_relay_credential_if_needed(&file_name).await?;
    if refreshed {
        // Keep a previously served Grok CLI account current, but never alter
        // its live auth file merely because a relay credential rotated.
        crate::grok_cli::sync_grok_cli_account_from_relay(&file_name)?;
    }
    let _mutation = mutation_lock()?;
    let client = ClientSurface::parse(client.as_deref().unwrap_or("claude"))?;
    let selected = exact_account_path(&file_name)?;
    if !selected.is_file() {
        return Err("Account not found".into());
    }
    let state = load_state()?;
    let accounts = list_accounts_inner(&state)?;
    if !accounts
        .iter()
        .any(|account| account.file_name == file_name)
    {
        return Err("Unsupported account file".into());
    }
    let root = root_dir()?;
    let directory = auth_dir()?;
    let state_path = controller_path()?;
    let (mutations, state) = selection_transaction(
        &root,
        &directory,
        &state_path,
        &accounts,
        &state,
        client,
        &file_name,
    )?;
    run_transaction(&root, &mutations, || {
        validate_account_invariant(&auth_dir()?, &state_path)
    })?;
    runtime_lock()?.last_known_good_models.clear();
    prepare_config()?;
    let claude_config_changed = match client {
        ClientSurface::Claude => {
            write_isolated_claude_config(&isolated_claude_profile_dir()?, &state)?
        }
        ClientSurface::Codex => {
            write_isolated_codex_config(&isolated_codex_home()?, &state)?;
            false
        }
    };
    if let Some(newly_active_provider) = accounts
        .iter()
        .find(|account| account.file_name == file_name)
        .map(|account| account.provider.clone())
    {
        refresh_model_catalog_cache(&newly_active_provider, &state.api_key);
    }
    Ok(AccountSelectionResult {
        snapshot: gateway_snapshot_locked()?,
        claude_config_changed,
    })
}

#[tauri::command]
pub fn set_gateway_route(
    provider: String,
    model: String,
    thinking: String,
    client: Option<String>,
) -> Result<RouteUpdateResult, String> {
    let _mutation = mutation_lock()?;
    let client = ClientSurface::parse(client.as_deref().unwrap_or("claude"))?;
    if !all_providers().contains(&provider.as_str()) {
        return Err(format!("Unknown provider: {provider}"));
    }
    let specs = model_specs(&provider);
    let spec = specs.iter().find(|spec| spec.id == model);
    let mut thinking = thinking;
    if specs.is_empty() {
        // Live-catalog provider (router/custom): no pinned thinking levels, so
        // force auto; the backend validates the model id against its live list.
        thinking = "auto".into();
    } else {
        let Some(spec) = spec else {
            return Err(format!("{model} is not an available {provider} model"));
        };
        if thinking != "auto" && !spec.thinking_levels.contains(&thinking.as_str()) {
            return Err(format!(
                "{} does not support the {thinking} thinking setting",
                spec.label
            ));
        }
    }
    let model_label = spec
        .map(|spec| spec.label.to_string())
        .unwrap_or_else(|| model.clone());
    let mut state = load_state()?;
    let account_is_active = list_accounts_inner(&state)?.iter().any(|account| {
        let selected = match client {
            ClientSurface::Claude => account.active,
            ClientSurface::Codex => account.active_for_codex,
        };
        selected && account.provider == provider
    });
    // True unless an active account exists and the backend was unreachable, so
    // the saved route could not be validated against the live model catalog.
    let mut route_verified = true;
    if account_is_active {
        if let Ok(models) = backend_model_ids(&state.api_key) {
            let target_backend_model = backend_model_identifier(&provider, &model);
            if !models.is_empty()
                && !models
                    .iter()
                    .any(|m| m == target_backend_model || m == &model)
            {
                return Err(format!(
                    "{} is not available for the selected {} credential. Choose a model advertised by the backend.",
                    model_label,
                    provider_label(&provider)
                ));
            }
            if !models.is_empty() {
                if let Ok(mut runtime) = runtime_lock() {
                    runtime
                        .last_known_model_catalog
                        .insert(provider.clone(), models.clone());
                }
            }
            if models
                .iter()
                .any(|m| m == target_backend_model || m == &model)
            {
                runtime_lock()?
                    .last_known_good_models
                    .insert(provider.clone(), model.clone());
            }
        } else {
            route_verified = false;
        }
    }
    client_routes_mut(&mut state, client)
        .insert(provider.clone(), RouteSelection { model, thinking });
    save_state(&state)?;
    prepare_config()?;
    if account_is_active {
        match client {
            ClientSurface::Claude => {
                write_isolated_claude_config(&isolated_claude_profile_dir()?, &state)?;
            }
            ClientSurface::Codex => {
                write_isolated_codex_config(&isolated_codex_home()?, &state)?;
            }
        }
    }
    Ok(RouteUpdateResult {
        snapshot: gateway_snapshot_locked()?,
        route_verified,
    })
}

#[tauri::command]
pub fn set_skip_model_switch_confirmation(skip: bool) -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    let mut state = load_state()?;
    state.skip_model_switch_confirmation = skip;
    save_state(&state)?;
    gateway_snapshot_locked()
}

#[tauri::command]
pub fn set_open_claude_on_launch(open: bool) -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    let mut state = load_state()?;
    state.open_claude_on_launch = open;
    save_state(&state)?;
    gateway_snapshot_locked()
}

#[tauri::command]
pub fn get_model_catalog(provider: String) -> Result<Vec<ModelCatalogEntry>, String> {
    let _mutation = mutation_lock()?;
    if !all_providers().contains(&provider.as_str()) {
        return Err(format!("Unknown provider: {provider}"));
    }
    let hidden = load_hidden_models()?;
    let live_catalog = runtime_lock()
        .ok()
        .and_then(|runtime| runtime.last_known_model_catalog.get(&provider).cloned());
    Ok(model_specs(&provider)
        .iter()
        .map(|spec| {
            let backend_id = backend_model_identifier(&provider, spec.id);
            ModelCatalogEntry {
                id: spec.id.to_string(),
                label: spec.label.to_string(),
                hidden: hidden.contains(spec.id),
                live: live_catalog
                    .as_ref()
                    .map(|live| live.iter().any(|id| id == spec.id || id == backend_id)),
            }
        })
        .collect())
}

#[tauri::command]
pub fn set_model_hidden(model_id: String, hidden: bool) -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    if !SUPPORTED_PROVIDERS
        .iter()
        .any(|provider| model_specs(provider).iter().any(|spec| spec.id == model_id))
    {
        return Err("Unknown model id".into());
    }
    let mut hidden_models = load_hidden_models()?;
    if hidden {
        hidden_models.insert(model_id);
    } else {
        hidden_models.remove(&model_id);
    }
    save_hidden_models(&hidden_models)?;
    gateway_snapshot_locked()
}

#[tauri::command]
pub fn remove_gateway_account(file_name: String) -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    let state = load_state()?;
    let accounts = list_accounts_inner(&state)?;
    let account = accounts
        .iter()
        .find(|account| account.file_name == file_name)
        .ok_or_else(|| "Account not found".to_string())?;
    let root = root_dir()?;
    let directory = account_directory_for(&account.auth)?;
    let state_path = controller_path()?;
    let labels_path = account_labels_path()?;
    let labels = load_account_labels()?;
    let (mutations, next_state) = removal_transaction(
        AccountPaths {
            root: &root,
            directory: &directory,
            state: &state_path,
            labels: &labels_path,
        },
        &accounts,
        &state,
        &labels,
        &file_name,
    )?;
    run_transaction(&root, &mutations, || {
        validate_account_invariant(&auth_dir()?, &state_path)
    })?;
    prepare_config()?;
    if state.active_account.as_deref() == Some(file_name.as_str()) {
        stop_hydra_claude_runtime();
    }
    if state.active_codex_account.as_deref() == Some(file_name.as_str()) {
        stop_hydra_codex_runtime();
    }
    if next_state.active_account.is_some() {
        let _ = write_isolated_claude_config(&isolated_claude_profile_dir()?, &next_state);
    }
    if next_state.active_codex_account.is_some() {
        let _ = write_isolated_codex_config(&isolated_codex_home()?, &next_state);
    }
    gateway_snapshot_locked()
}

/// Build a unique, filesystem-safe account file name for an API-key account,
/// seeded from the provider and a slug of the label.
fn unique_key_account_file_name(provider: &str, label: &str) -> Result<String, String> {
    let slug: String = label
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = slug.trim_matches('-').to_string();
    let stem = if trimmed.is_empty() {
        "account".to_string()
    } else {
        trimmed
    };
    let directory = keys_dir()?;
    let mut candidate = format!("{provider}-{stem}.json");
    let mut suffix = 1;
    // The provider prefix is injected into the route; keep the file name unique
    // so two DeepSeek labels do not collide.
    while directory.join(&candidate).exists() {
        candidate = format!("{provider}-{stem}-{suffix}.json");
        suffix += 1;
    }
    Ok(candidate)
}

/// Persist a custom API-key account. This is the "API keys" half of the
/// Provider × Auth model: any provider that accepts a key can be added without
/// the browser OAuth flow.
#[tauri::command]
pub fn add_api_key_account(
    provider: String,
    label: String,
    api_key: String,
    base_url: Option<String>,
    model: Option<String>,
) -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    let provider = provider.to_ascii_lowercase();
    if !auth_kinds_for(&provider).contains(&ProviderAuth::ApiKey) {
        return Err(format!("{provider} does not accept an API key"));
    }
    let api_key = api_key.trim().to_string();
    if api_key.is_empty() {
        return Err("An API key is required.".into());
    }
    let base_url = base_url
        .map(|url| url.trim().to_string())
        .filter(|url| !url.is_empty())
        .or_else(|| default_api_base_url(&provider).map(str::to_string));
    let model = model
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty());
    let label = normalized_account_label(&label)?;
    let file_name = unique_key_account_file_name(&provider, &label)?;
    let directory = keys_dir()?;
    secure_create_dir_all(&directory)?;
    let mut object = serde_json::Map::new();
    object.insert("kind".into(), Value::String("api_key".into()));
    object.insert("provider".into(), Value::String(provider));
    object.insert("api_key".into(), Value::String(api_key));
    if let Some(base_url) = &base_url {
        object.insert("base_url".into(), Value::String(base_url.clone()));
    }
    if let Some(model) = &model {
        object.insert("model".into(), Value::String(model.clone()));
    }
    object.insert("label".into(), Value::String(label.clone()));
    object.insert("disabled".into(), Value::Bool(false));
    let bytes = serde_json::to_vec_pretty(&Value::Object(object))
        .map_err(|error| format!("Could not serialize API key account: {error}"))?;
    durable_write(&directory.join(&file_name), &bytes)?;
    let mut labels = load_account_labels()?;
    labels.insert(file_name, label);
    save_account_labels(&labels)?;
    prepare_config()?;
    gateway_snapshot_locked()
}

/// The model ids an API-key account can route: its provider's pinned models
/// plus any live catalog already fetched for that provider.
#[tauri::command]
pub fn get_api_key_account_models(file_name: String) -> Result<Vec<String>, String> {
    let path = exact_account_path(&file_name)?;
    let raw = fs::read_to_string(&path).map_err(|error| format!("Account not found: {error}"))?;
    let value: Value =
        serde_json::from_str(&raw).map_err(|error| format!("Account file is invalid: {error}"))?;
    let provider = account_provider(&value, &file_name).ok_or("Unknown provider")?;
    let mut models: Vec<String> = model_specs(&provider)
        .iter()
        .map(|spec| spec.id.to_string())
        .collect();
    if let Ok(runtime) = runtime_lock() {
        if let Some(live) = runtime.last_known_model_catalog.get(&provider) {
            for model in live {
                if !models.contains(model) {
                    models.push(model.clone());
                }
            }
        }
    }
    Ok(models)
}

enum LoginOutput {
    Line(String),
    Error(String),
    Eof,
}

fn extract_login_url(provider: &str, line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let candidate = line[start..].trim().trim_end_matches(|character: char| {
        matches!(character, ')' | ']' | '}' | '>' | '\'' | '"' | ',' | ';')
    });
    let allowed = match provider {
        "claude" => candidate.starts_with("https://claude.ai/"),
        "codex" => candidate.starts_with("https://auth.openai.com/"),
        "xai" => {
            candidate.starts_with("https://accounts.x.ai/")
                || candidate.starts_with("https://auth.x.ai/")
        }
        "kimi" => {
            candidate.starts_with("https://auth.kimi.com/")
                || candidate.starts_with("https://www.kimi.com/")
        }
        "antigravity" => candidate.starts_with("https://accounts.google.com/"),
        "zai" => zai_oauth::is_allowed_authorize_url(candidate),
        _ => false,
    };
    allowed.then(|| candidate.to_string())
}

fn extract_xai_user_code(line: &str) -> Option<String> {
    let (_, value) = line.split_once("Then enter this code:")?;
    let code = value.trim();
    (!code.is_empty()).then(|| code.to_string())
}

fn extract_kimi_user_code(line: &str) -> Option<String> {
    let (_, value) = line.split_once("User code:")?;
    let code = value.trim();
    (!code.is_empty()).then(|| code.to_string())
}

fn login_authorization_ready(
    provider: &str,
    authorization_url: &Option<String>,
    user_code: &Option<String>,
    line: &str,
) -> bool {
    authorization_url.is_some()
        && match provider {
            "xai" => line.contains("Waiting for authorization"),
            "kimi" => user_code.is_some() && line.contains("Waiting for authorization"),
            _ => true,
        }
}

fn login_stderr_failure_reason(provider: &str, line: &str) -> Option<String> {
    let normalized = line.to_ascii_lowercase();
    let provider_name = provider_label(provider);
    let expected = format!("{} authentication failed", provider.to_ascii_lowercase());
    if !normalized.contains(&expected) {
        return None;
    }
    let detail = if normalized.contains("status 401") || normalized.contains("status 403") {
        "rejected the device authorization request"
    } else if normalized.contains("status 429") {
        "is temporarily rate-limiting device authorization"
    } else if normalized.contains("timeout")
        || normalized.contains("connection")
        || normalized.contains("request failed")
    {
        "could not be reached for device authorization"
    } else {
        "failed before device authorization could begin"
    };
    Some(format!(
        "{provider_name} {detail}. Check your connection and try again."
    ))
}

fn login_staging_root() -> Result<PathBuf, String> {
    Ok(root_dir()?.join("login-staging"))
}

fn remove_login_staging(path: &Path) -> Result<(), String> {
    if !path.exists() {
        return Ok(());
    }
    let root = login_staging_root()?;
    let canonical_root = fs::canonicalize(&root)
        .map_err(|error| format!("Could not verify the login staging root: {error}"))?;
    let canonical_path = fs::canonicalize(path)
        .map_err(|error| format!("Could not verify the login staging directory: {error}"))?;
    let relative = canonical_path
        .strip_prefix(&canonical_root)
        .map_err(|_| "Refusing to remove a login staging directory outside Basiliskos")?;
    if relative.components().count() != 1 {
        return Err("Refusing to remove an unexpected login staging path".into());
    }
    fs::remove_dir_all(&canonical_path)
        .map_err(|error| format!("Could not clean the login staging directory: {error}"))
}

fn staged_login_config(state: &ControllerState, auth: &Path) -> String {
    format!(
        r#"host: "127.0.0.1"
port: {BACKEND_PORT}
remote-management:
  allow-remote: false
  secret-key: ""
  disable-control-panel: true
auth-dir: {auth_dir}
api-keys:
  - {api_key}
debug: false
logging-to-file: false
request-log: false
usage-statistics-enabled: false
request-retry: 0
plugins:
  enabled: false
"#,
        auth_dir = yaml_quote(&auth.to_string_lossy()),
        api_key = yaml_quote(&state.api_key),
    )
}

fn credential_identity(value: &Value, file_name: &str) -> String {
    nested_string(
        value,
        &[
            "email",
            "account_email",
            "user_email",
            "account_id",
            "user_id",
            "sub",
        ],
    )
    .map(|identity| identity.trim().to_ascii_lowercase())
    .filter(|identity| !identity.is_empty())
    .unwrap_or_else(|| file_name.to_ascii_lowercase())
}

fn new_credential_destination_name(directory: &Path, staged_name: &str) -> Result<String, String> {
    if staged_name.contains(['/', '\\'])
        || !staged_name.ends_with(".json")
        || staged_name.len() > 240
    {
        return Err("The provider login produced an unsafe credential filename".into());
    }
    if !directory.join(staged_name).exists() {
        return Ok(staged_name.to_owned());
    }

    let stem = staged_name
        .strip_suffix(".json")
        .unwrap_or("credential")
        .chars()
        .take(180)
        .collect::<String>();
    for _ in 0..8 {
        let candidate = format!("{stem}-{}.json", Uuid::new_v4().simple());
        if !directory.join(&candidate).exists() {
            return Ok(candidate);
        }
    }
    Err("Could not allocate a collision-free credential filename".into())
}

fn merge_staged_login(provider: &str, staging_dir: &Path) -> Result<String, String> {
    let staged_auth = staging_dir.join("auth");
    let mut candidates = Vec::new();
    for entry in fs::read_dir(&staged_auth)
        .map_err(|error| format!("Could not inspect completed login credentials: {error}"))?
    {
        let entry = entry.map_err(|error| format!("Could not inspect login output: {error}"))?;
        let path = entry.path();
        if !path.is_file() || path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let raw = fs::read(&path)
            .map_err(|error| format!("Could not validate a completed login: {error}"))?;
        let value: Value = serde_json::from_slice(&raw)
            .map_err(|_| "A completed login produced invalid credential JSON")?;
        let file_name = path
            .file_name()
            .and_then(|value| value.to_str())
            .ok_or("A completed login produced an invalid credential filename")?;
        if account_provider(&value, file_name).as_deref() == Some(provider) {
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            candidates.push((modified, file_name.to_owned(), value));
        }
    }
    candidates.sort_by_key(|candidate| candidate.0);
    let (_, staged_name, mut staged_value) = candidates
        .pop()
        .ok_or("The provider login exited without a validated credential")?;
    let identity = credential_identity(&staged_value, &staged_name);
    let state = load_state()?;
    let accounts = list_accounts_inner(&state)?;
    let directory = auth_dir()?;
    let mut destination_name = None;
    let mut disabled = true;
    for account in &accounts {
        if account.provider != provider {
            continue;
        }
        let current_path = directory.join(&account.file_name);
        let current_value: Value = serde_json::from_slice(
            &fs::read(&current_path)
                .map_err(|error| format!("Could not compare an existing credential: {error}"))?,
        )
        .map_err(|_| "An existing credential is invalid")?;
        if credential_identity(&current_value, &account.file_name) == identity {
            destination_name = Some(account.file_name.clone());
            disabled = account.disabled;
            break;
        }
    }
    let destination_name = match destination_name {
        Some(existing) => existing,
        None => new_credential_destination_name(&directory, &staged_name)?,
    };
    let object = staged_value
        .as_object_mut()
        .ok_or("The provider login credential must be a JSON object")?;
    object.insert("disabled".into(), Value::Bool(disabled));
    let after = serde_json::to_vec_pretty(&staged_value)
        .map_err(|_| "The provider login credential could not be serialized")?;
    let root = root_dir()?;
    let state_path = controller_path()?;
    run_transaction(
        &root,
        &[FileMutation {
            path: directory.join(&destination_name),
            after: Some(after),
        }],
        || validate_account_invariant(&directory, &state_path),
    )
    .inspect_err(|_| {
        diagnostics::record(
            ErrorCode::ConfigTransactionFailed,
            "error",
            "The completed credential could not be committed transactionally.",
            None,
            None,
            Some(provider),
        );
    })?;
    if provider == "xai" {
        set_xai_relogin_required(&destination_name, false)?;
    }
    if provider == "kimi" {
        set_kimi_relogin_required(&destination_name, false)?;
    }
    prepare_config()?;
    Ok(destination_name)
}

fn finish_login_session(session_id: String) {
    let Ok(_mutation) = mutation_lock() else {
        return;
    };
    let session = {
        let Ok(mut runtime) = runtime_lock() else {
            return;
        };
        if runtime
            .login
            .as_ref()
            .map(|login| login.status.session_id.as_str())
            != Some(session_id.as_str())
        {
            return;
        }
        runtime.login.take()
    };
    let Some(session) = session else { return };
    let Some(child) = session.child else {
        return;
    };
    let exit = child
        .lock()
        .map_err(|_| "Provider login process state is unavailable".to_string())
        .and_then(|mut child| {
            child
                .wait()
                .map_err(|_| "Provider login wait failed".to_string())
        });
    let result = match exit {
        Ok(status) if status.success() => {
            merge_staged_login(&session.status.provider, &session.staging_dir)
        }
        Ok(_) => Err("The provider login exited without completing authorization".into()),
        Err(error) => Err(error),
    };
    #[cfg(target_os = "windows")]
    close_gateway_job(session.job);
    let _ = remove_login_staging(&session.staging_dir);
    let status = match result {
        Ok(file_name) => ProviderLoginStatus {
            state: "completed".into(),
            result_file_name: Some(file_name),
            detail: "Provider login completed and the validated credential was committed".into(),
            ..session.status
        },
        Err(_) => {
            diagnostics::record(
                ErrorCode::LoginFailed,
                "error",
                "The provider login did not produce a validated credential.",
                None,
                None,
                Some(&session.status.provider),
            );
            ProviderLoginStatus {
                state: "failed".into(),
                result_file_name: None,
                detail: "Provider login failed without changing live credentials".into(),
                ..session.status
            }
        }
    };
    if let Ok(mut runtime) = runtime_lock() {
        runtime.last_login = Some(status);
    }
}

fn watch_login_session(session_id: String, child: Arc<Mutex<Child>>) {
    thread::spawn(move || loop {
        thread::sleep(Duration::from_millis(250));
        let active = runtime_lock()
            .ok()
            .and_then(|runtime| {
                runtime
                    .login
                    .as_ref()
                    .map(|login| login.status.session_id == session_id)
            })
            .unwrap_or(false);
        if !active {
            return;
        }
        let exited = match child.lock() {
            Ok(mut child) => !matches!(child.try_wait(), Ok(None)),
            Err(_) => true,
        };
        if exited {
            finish_login_session(session_id);
            return;
        }
    });
}

fn abort_login_start(
    session_id: &str,
    staging_dir: &Path,
    child: Option<Child>,
    job: Option<usize>,
    provider: &str,
) {
    if let Some(mut child) = child {
        let _ = child.kill();
        let _ = child.wait();
    }
    close_gateway_job(job);
    let _ = remove_login_staging(staging_dir);
    if let Ok(mut runtime) = runtime_lock() {
        if runtime.login_claim.as_deref() == Some(session_id) {
            runtime.login_claim = None;
        }
    }
    diagnostics::record(
        ErrorCode::LoginFailed,
        "error",
        "The provider login could not reach its authorization step.",
        None,
        None,
        Some(provider),
    );
}

fn provider_login_flag(provider: &str) -> Result<&'static str, String> {
    match provider {
        "claude" => Ok("-claude-login"),
        "codex" => Ok("-codex-login"),
        "xai" => Ok("-xai-login"),
        "kimi" => Ok("-kimi-login"),
        "antigravity" => Ok("-antigravity-login"),
        "zai" => Err("Z.AI login uses the official ZCode CLI flow, not CLIProxyAPI".into()),
        _ => Err("Provider must be claude, codex, xai, kimi, antigravity, or zai".into()),
    }
}

fn launch_zai_login_blocking() -> Result<ProviderLoginLaunch, String> {
    let session_id = Uuid::new_v4().simple().to_string();
    {
        let mut runtime = runtime_lock()?;
        if runtime.login.is_some() || runtime.login_claim.is_some() {
            return Err("A provider login is already running. Finish or cancel it first.".into());
        }
        runtime.login_claim = Some(session_id.clone());
        runtime.last_login = None;
    }
    let staging_dir = login_staging_root()?.join(&session_id);
    let staged_auth = staging_dir.join("auth");
    if let Err(error) = (|| -> Result<(), String> {
        let _mutation = mutation_lock()?;
        secure_create_dir_all(&staging_dir)?;
        secure_create_dir_all(&staged_auth)?;
        Ok(())
    })() {
        abort_login_start(&session_id, &staging_dir, None, None, "zai");
        return Err(error);
    }
    let client = match ZaiOAuth::production() {
        Ok(client) => client,
        Err(error) => {
            abort_login_start(&session_id, &staging_dir, None, None, "zai");
            return Err(error);
        }
    };
    let init = match tauri::async_runtime::block_on(client.start_cli_flow()) {
        Ok(init) => init,
        Err(error) => {
            abort_login_start(&session_id, &staging_dir, None, None, "zai");
            return Err(error);
        }
    };
    let authorization_url = match extract_login_url("zai", &init.authorize_url) {
        Some(url) => url,
        None => {
            abort_login_start(&session_id, &staging_dir, None, None, "zai");
            return Err("Z.AI login did not return a trusted authorization URL".into());
        }
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let status = ProviderLoginStatus {
        session_id: session_id.clone(),
        provider: "zai".into(),
        state: "waiting".into(),
        started_at: Utc::now().to_rfc3339(),
        result_file_name: None,
        detail: "Waiting for the official provider authorization to complete".into(),
    };
    {
        let mut runtime = match runtime_lock() {
            Ok(runtime) => runtime,
            Err(error) => {
                abort_login_start(&session_id, &staging_dir, None, None, "zai");
                return Err(error);
            }
        };
        if runtime.login_claim.as_deref() != Some(session_id.as_str()) {
            drop(runtime);
            abort_login_start(&session_id, &staging_dir, None, None, "zai");
            return Err("The provider login was cancelled during startup".into());
        }
        runtime.login_claim = None;
        runtime.login = Some(LoginRuntime {
            status,
            child: None,
            cancel: Arc::clone(&cancel),
            staging_dir: staging_dir.clone(),
            #[cfg(target_os = "windows")]
            job: None,
        });
    }
    let worker_session = session_id.clone();
    thread::spawn(move || {
        let outcome = tauri::async_runtime::block_on(async {
            let ready = client.wait_for_authorization(&init, &cancel).await?;
            if cancel.load(Ordering::SeqCst) {
                return Err("Z.AI login was cancelled".into());
            }
            let minted = client.mint_coding_plan_key(&ready).await?;
            Ok((ready, minted))
        });
        finish_zai_login(worker_session, staging_dir, cancel, outcome);
    });
    Ok(ProviderLoginLaunch {
        session_id,
        authorization_url,
        user_code: None,
    })
}

fn finish_zai_login(
    session_id: String,
    staging_dir: PathBuf,
    cancel: Arc<AtomicBool>,
    outcome: Result<(zai_oauth::ZaiReady, String), String>,
) {
    let commit = (|| -> Result<String, String> {
        if cancel.load(Ordering::SeqCst) {
            return Err("Z.AI login was cancelled".into());
        }
        let (ready, api_key) = outcome?;
        let _mutation = mutation_lock()?;
        if cancel.load(Ordering::SeqCst) {
            return Err("Z.AI login was cancelled".into());
        }
        let file_name = zai_oauth::credential_file_name(&ready);
        let staged_auth = staging_dir.join("auth");
        secure_create_dir_all(&staged_auth)?;
        durable_write(
            &staged_auth.join(&file_name),
            serde_json::to_vec_pretty(&zai_oauth::credential_json(&ready, &api_key))
                .map_err(|_| "The Z.AI credential could not be serialized")?
                .as_slice(),
        )?;
        merge_staged_login("zai", &staging_dir)
    })();
    let session = {
        let Ok(mut runtime) = runtime_lock() else {
            let _ = remove_login_staging(&staging_dir);
            return;
        };
        if runtime
            .login
            .as_ref()
            .map(|login| login.status.session_id.as_str())
            != Some(session_id.as_str())
        {
            let _ = remove_login_staging(&staging_dir);
            return;
        }
        runtime.login.take()
    };
    let Some(session) = session else {
        let _ = remove_login_staging(&staging_dir);
        return;
    };
    let _ = remove_login_staging(&staging_dir);
    let status = match commit {
        Ok(file_name) => ProviderLoginStatus {
            state: "completed".into(),
            result_file_name: Some(file_name),
            detail: "Provider login completed and the validated credential was committed".into(),
            ..session.status
        },
        Err(error) if error.contains("cancelled") => ProviderLoginStatus {
            state: "cancelled".into(),
            result_file_name: None,
            detail: "Provider login cancelled; live credentials were not changed".into(),
            ..session.status
        },
        Err(_) => {
            diagnostics::record(
                ErrorCode::LoginFailed,
                "error",
                "The provider login did not produce a validated credential.",
                None,
                None,
                Some("zai"),
            );
            ProviderLoginStatus {
                state: "failed".into(),
                result_file_name: None,
                detail: "Provider login failed without changing live credentials".into(),
                ..session.status
            }
        }
    };
    if let Ok(mut runtime) = runtime_lock() {
        runtime.last_login = Some(status);
    }
}

fn launch_provider_login_blocking(
    app: AppHandle,
    provider: String,
) -> Result<ProviderLoginLaunch, String> {
    if provider == "zai" {
        return launch_zai_login_blocking();
    }
    let flag = provider_login_flag(&provider)?;
    let session_id = Uuid::new_v4().simple().to_string();
    {
        let mut runtime = runtime_lock()?;
        if runtime.login.is_some() || runtime.login_claim.is_some() {
            return Err("A provider login is already running. Finish or cancel it first.".into());
        }
        runtime.login_claim = Some(session_id.clone());
        runtime.last_login = None;
    }
    let staging_dir = login_staging_root()?.join(&session_id);
    let staged_auth = staging_dir.join("auth");
    let staged_config = staging_dir.join("login-config.yaml");
    let prepared = (|| -> Result<(PathBuf, ControllerState), String> {
        let _mutation = mutation_lock()?;
        let state = prepare_config()?;
        secure_create_dir_all(&staging_dir)?;
        secure_create_dir_all(&staged_auth)?;
        durable_write(
            &staged_config,
            staged_login_config(&state, &staged_auth).as_bytes(),
        )?;
        Ok((prepare_runtime(&app)?, state))
    })();
    let (executable, _state) = match prepared {
        Ok(prepared) => prepared,
        Err(error) => {
            abort_login_start(&session_id, &staging_dir, None, None, &provider);
            return Err(error);
        }
    };
    let mut command = Command::new(executable);
    command
        .args([
            flag,
            "-no-browser",
            "-config",
            &staged_config.to_string_lossy(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hidden(&mut command);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            abort_login_start(&session_id, &staging_dir, None, None, &provider);
            return Err(format!("Could not start {provider} login: {error}"));
        }
    };
    let job = match assign_gateway_to_kill_on_close_job(&child) {
        Ok(job) => job,
        Err(error) => {
            abort_login_start(&session_id, &staging_dir, Some(child), None, &provider);
            return Err(error);
        }
    };

    let Some(stdout) = child.stdout.take() else {
        abort_login_start(&session_id, &staging_dir, Some(child), job, &provider);
        return Err(format!("Could not read {provider} login output"));
    };
    let Some(stderr) = child.stderr.take() else {
        abort_login_start(&session_id, &staging_dir, Some(child), job, &provider);
        return Err(format!("Could not read {provider} login errors"));
    };
    let (output_tx, output_rx) = mpsc::channel();
    let stderr_tx = output_tx.clone();
    let stderr_provider = provider.clone();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = output_tx.send(LoginOutput::Line(line));
        }
        let _ = output_tx.send(LoginOutput::Eof);
    });
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            // OAuth output is intentionally not persisted because it can contain short-lived
            // login data. Only a fixed, sanitized failure category reaches the UI.
            if let Some(reason) = login_stderr_failure_reason(&stderr_provider, &line) {
                let _ = stderr_tx.send(LoginOutput::Error(reason));
            }
        }
    });

    let deadline = Instant::now() + Duration::from_secs(30);
    let mut authorization_url = None;
    let mut user_code = None;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        let output = match output_rx.recv_timeout(remaining) {
            Ok(output) => output,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                abort_login_start(&session_id, &staging_dir, Some(child), job, &provider);
                return Err(format!(
                    "The {provider} login did not provide an authorization URL within 30 seconds"
                ));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => LoginOutput::Eof,
        };
        match output {
            LoginOutput::Line(line) => {
                if authorization_url.is_none() {
                    authorization_url = extract_login_url(&provider, &line);
                }
                if user_code.is_none() {
                    user_code = match provider.as_str() {
                        "xai" => extract_xai_user_code(&line),
                        "kimi" => extract_kimi_user_code(&line),
                        _ => None,
                    };
                }
                let ready =
                    login_authorization_ready(&provider, &authorization_url, &user_code, &line);
                if ready {
                    let authorization_url = authorization_url.expect("checked above");
                    let child = Arc::new(Mutex::new(child));
                    let status = ProviderLoginStatus {
                        session_id: session_id.clone(),
                        provider: provider.clone(),
                        state: "waiting".into(),
                        started_at: Utc::now().to_rfc3339(),
                        result_file_name: None,
                        detail: "Waiting for the official provider authorization to complete"
                            .into(),
                    };
                    {
                        let mut runtime = match runtime_lock() {
                            Ok(runtime) => runtime,
                            Err(error) => {
                                let child = Arc::try_unwrap(child)
                                    .ok()
                                    .and_then(|mutex| mutex.into_inner().ok());
                                abort_login_start(&session_id, &staging_dir, child, job, &provider);
                                return Err(error);
                            }
                        };
                        if runtime.login_claim.as_deref() != Some(session_id.as_str()) {
                            drop(runtime);
                            let child = Arc::try_unwrap(child)
                                .ok()
                                .and_then(|mutex| mutex.into_inner().ok());
                            abort_login_start(&session_id, &staging_dir, child, job, &provider);
                            return Err("The provider login was cancelled during startup".into());
                        }
                        runtime.login_claim = None;
                        runtime.login = Some(LoginRuntime {
                            status,
                            child: Some(Arc::clone(&child)),
                            cancel: Arc::new(AtomicBool::new(false)),
                            staging_dir: staging_dir.clone(),
                            #[cfg(target_os = "windows")]
                            job,
                        });
                    }
                    watch_login_session(session_id.clone(), child);
                    return Ok(ProviderLoginLaunch {
                        session_id,
                        authorization_url,
                        user_code,
                    });
                }
            }
            LoginOutput::Error(reason) => {
                abort_login_start(&session_id, &staging_dir, Some(child), job, &provider);
                return Err(reason);
            }
            LoginOutput::Eof => {
                let status = child
                    .try_wait()
                    .ok()
                    .flatten()
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "unknown status".into());
                abort_login_start(&session_id, &staging_dir, Some(child), job, &provider);
                return Err(format!(
                    "The {provider} login exited before providing a trusted authorization URL ({status})"
                ));
            }
        }
    }
}

#[tauri::command]
pub async fn launch_provider_login(
    app: AppHandle,
    provider: String,
) -> Result<ProviderLoginLaunch, String> {
    tauri::async_runtime::spawn_blocking(move || launch_provider_login_blocking(app, provider))
        .await
        .map_err(|error| format!("Could not run the provider login task: {error}"))?
}

fn cancel_login_runtime() {
    let session = runtime_lock().ok().and_then(|mut runtime| {
        runtime.login_claim = None;
        runtime.login.take()
    });
    let Some(session) = session else { return };
    session.cancel.store(true, Ordering::SeqCst);
    if let Some(child) = session.child {
        if let Ok(mut child) = child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    #[cfg(target_os = "windows")]
    close_gateway_job(session.job);
    let _ = remove_login_staging(&session.staging_dir);
    diagnostics::record(
        ErrorCode::LoginCancelled,
        "info",
        "The provider login was cancelled and its staging directory was discarded.",
        None,
        None,
        Some(&session.status.provider),
    );
    if let Ok(mut runtime) = runtime_lock() {
        runtime.last_login = Some(ProviderLoginStatus {
            state: "cancelled".into(),
            result_file_name: None,
            detail: "Provider login cancelled; live credentials were not changed".into(),
            ..session.status
        });
    }
}

#[tauri::command]
pub fn cancel_provider_login() -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    cancel_login_runtime();
    gateway_snapshot_locked()
}

fn read_json_object(path: &Path) -> Result<Map<String, Value>, String> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let value: Value = serde_json::from_str(&raw).map_err(|error| {
        format!(
            "Refusing to overwrite invalid JSON in {}: {error}",
            path.display()
        )
    })?;
    value.as_object().cloned().ok_or_else(|| {
        format!(
            "Refusing to overwrite non-object JSON in {}",
            path.display()
        )
    })
}

fn json_bytes(object: &Map<String, Value>) -> Result<Vec<u8>, String> {
    serde_json::to_vec_pretty(&Value::Object(object.clone())).map_err(|error| error.to_string())
}

fn backup_changed_claude_configs(
    profile: &Path,
    writes: &[(PathBuf, Vec<u8>)],
) -> Result<(), String> {
    let changed: Vec<&PathBuf> = writes
        .iter()
        .filter_map(|(path, next)| {
            let current = fs::read(path).ok()?;
            (current != *next).then_some(path)
        })
        .collect();
    if changed.is_empty() {
        return Ok(());
    }

    let backup_root = profile.join("Basiliskos Backups");
    let version = backup_root.join(format!(
        "{}-{}",
        Utc::now().format("%Y%m%dT%H%M%S%.3fZ"),
        Uuid::new_v4().simple()
    ));
    secure_create_dir_all(&backup_root)?;
    let staging = backup_root.join(format!(".tmp-{}", Uuid::new_v4().simple()));
    for path in changed {
        let relative = path
            .strip_prefix(profile)
            .map_err(|_| format!("Refusing to back up a config outside {}", profile.display()))?;
        let destination = staging.join(relative);
        if let Some(parent) = destination.parent() {
            secure_create_dir_all(parent)?;
        }
        fs::copy(path, &destination).map_err(|error| {
            format!(
                "Could not back up {} to {}: {error}",
                path.display(),
                destination.display()
            )
        })?;
    }
    match fs::rename(&staging, &version) {
        Ok(()) => Ok(()),
        Err(error) => Err(format!(
            "Could not finalize Claude config backup {}: {error}",
            version.display()
        )),
    }
}

fn validate_claude_config_set(
    meta_path: &Path,
    generated_path: &Path,
    deployment_path: &Path,
    config_id: &str,
) -> Result<(), String> {
    let meta: Value = serde_json::from_slice(
        &fs::read(meta_path).map_err(|_| "Claude metadata was not committed")?,
    )
    .map_err(|_| "Claude metadata is invalid after commit")?;
    let generated: Value = serde_json::from_slice(
        &fs::read(generated_path).map_err(|_| "Claude gateway config was not committed")?,
    )
    .map_err(|_| "Claude gateway config is invalid after commit")?;
    let deployment: Value = serde_json::from_slice(
        &fs::read(deployment_path).map_err(|_| "Claude deployment config was not committed")?,
    )
    .map_err(|_| "Claude deployment config is invalid after commit")?;
    if meta.get("appliedId").and_then(Value::as_str) != Some(config_id)
        || generated
            .get("inferenceGatewayBaseUrl")
            .and_then(Value::as_str)
            != Some("http://127.0.0.1:8317")
        || generated
            .get("inference")
            .and_then(|value| value.get("provider"))
            .and_then(Value::as_str)
            != Some("gateway")
        || generated
            .get("authentication")
            .and_then(|value| value.get("disableClaudeAiSignIn"))
            .and_then(Value::as_bool)
            != Some(true)
        || deployment.get("deploymentMode").and_then(Value::as_str) != Some("3p")
    {
        return Err("The Claude config set failed its cross-file invariant".into());
    }
    Ok(())
}

fn write_isolated_claude_config(profile: &Path, state: &ControllerState) -> Result<bool, String> {
    let library = profile.join("configLibrary");
    secure_create_dir_all(&library)?;
    let meta_path = library.join("_meta.json");
    let generated_path = library.join(format!("{}.json", state.claude_config_id));
    let deployment_path = profile.join("claude_desktop_config.json");

    // Parse every existing file before writing any of them. A malformed user
    // config therefore fails closed instead of being replaced with defaults.
    let mut meta = read_json_object(&meta_path)?;
    let mut generated = read_json_object(&generated_path)?;
    let mut deployment = read_json_object(&deployment_path)?;

    meta.entry("version").or_insert(Value::from(1));
    meta.insert(
        "appliedId".into(),
        Value::String(state.claude_config_id.clone()),
    );
    let configs = meta
        .entry("configs")
        .or_insert_with(|| Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(|| {
            format!(
                "Refusing to overwrite invalid configs metadata in {}",
                meta_path.display()
            )
        })?;
    let mut found = false;
    for entry in configs.iter_mut() {
        if entry.get("id").and_then(Value::as_str) == Some(state.claude_config_id.as_str()) {
            let object = entry.as_object_mut().ok_or_else(|| {
                format!(
                    "Refusing to overwrite an invalid Basiliskos entry in {}",
                    meta_path.display()
                )
            })?;
            object.insert("name".into(), Value::String(BASILISKOS_CONFIG_NAME.into()));
            found = true;
        }
    }
    if !found {
        configs.push(serde_json::json!({
            "id": state.claude_config_id,
            "name": BASILISKOS_CONFIG_NAME
        }));
    }

    let accounts = list_accounts_inner(state)?;
    let active_provider = accounts
        .iter()
        .find(|account| account.active)
        .map(|account| account.provider.as_str());
    let model_label = route_label(state, active_provider);
    // Advertise the active provider's picker entries in Claude's own picker:
    // the selected model plus its thinking-level variants ("Model Ã‚Â· High"),
    // then the other visible models. `name` is a real Anthropic catalog alias
    // (Claude validates it); the front proxy maps the alias back to the
    // upstream model + thinking (`client_picker_choice`).
    let inference_models = match active_provider {
        Some(provider) if SUPPORTED_PROVIDERS.contains(&provider) => {
            let hidden = load_hidden_models().unwrap_or_default();
            let route = provider_route(state, provider);
            picker_entries(provider, &hidden, &route.selected_model)
                .into_iter()
                .map(|(alias, label, _, _)| {
                    serde_json::json!({ "name": alias, "labelOverride": label })
                })
                .collect::<Vec<_>>()
        }
        _ => vec![serde_json::json!({
            "name": advertised_model_name(state, active_provider),
            "labelOverride": model_label,
        })],
    };
    generated.insert(
        "inferenceCredentialKind".into(),
        Value::String("static".into()),
    );
    generated.insert(
        "inferenceGatewayApiKey".into(),
        Value::String(state.api_key.clone()),
    );
    generated.insert(
        "inferenceGatewayAuthScheme".into(),
        Value::String("x-api-key".into()),
    );
    generated.insert(
        "inferenceGatewayBaseUrl".into(),
        Value::String(format!("http://127.0.0.1:{GATEWAY_PORT}")),
    );
    generated.insert(
        "inferenceModels".into(),
        serde_json::Value::Array(inference_models),
    );
    generated.insert("inferenceProvider".into(), Value::String("gateway".into()));
    generated.insert("modelDiscoveryEnabled".into(), Value::Bool(true));
    generated.insert("unstableDisableModelVerification".into(), Value::Bool(true));
    // Claude 1.40609 3p forcing reads nested `inference`, not the older
    // flat keys. Keep both so 1.24012 still hydrates from legacyFlatKey.
    generated.insert(
        "inference".into(),
        serde_json::json!({
            "provider": "gateway",
            "baseUrl": format!("http://127.0.0.1:{GATEWAY_PORT}"),
            "credential": {
                "kind": "static",
                "apiKey": state.api_key,
                "authScheme": "x-api-key"
            }
        }),
    );
    generated.insert(
        "authentication".into(),
        serde_json::json!({
            "disableClaudeAiSignIn": true
        }),
    );

    deployment.insert("deploymentMode".into(), Value::String("3p".into()));
    deployment.insert("awaitingSignIn".into(), Value::Bool(false));

    let writes = vec![
        (meta_path, json_bytes(&meta)?),
        (generated_path, json_bytes(&generated)?),
        (deployment_path, json_bytes(&deployment)?),
    ];
    backup_changed_claude_configs(profile, &writes)?;
    let mutations = writes
        .into_iter()
        .filter(|(path, bytes)| fs::read(path).ok().as_deref() != Some(bytes.as_slice()))
        .map(|(path, bytes)| FileMutation {
            path,
            after: Some(bytes),
        })
        .collect::<Vec<_>>();
    if mutations.is_empty() {
        return Ok(false);
    }
    run_transaction(profile, &mutations, || {
        validate_claude_config_set(
            &library.join("_meta.json"),
            &library.join(format!("{}.json", state.claude_config_id)),
            &profile.join("claude_desktop_config.json"),
            &state.claude_config_id,
        )
    })
    .inspect_err(|_| {
        diagnostics::record(
            ErrorCode::ConfigTransactionFailed,
            "error",
            "The isolated Claude config set was rolled back.",
            None,
            None,
            None,
        );
    })
    .map(|()| true)
}

/// Renders the isolated Codex `config.toml` body: only the Basiliskos relay as
/// provider, over the OpenAI Responses wire API. Pure so tests can assert the
/// golden shape without touching any real state.
fn codex_config_toml(model: &str, thinking: &str, port: u16, catalog_path: &str) -> String {
    let reasoning_line = if thinking == "auto" {
        String::new()
    } else {
        format!(
            "model_reasoning_effort = \"{}\"\n",
            thinking.replace('"', "")
        )
    };
    let compact_limit = model_to_provider(model)
        .and_then(|provider| context_window_for_route(provider, model))
        .map(|window| window.saturating_mul(80) / 100)
        .unwrap_or(160_000);
    format!(
        r#"# Generated by Basiliskos for the isolated Codex client only.
# Global ~/.codex is not modified.
model = "{model}"
{reasoning_line}# Stream the model's reasoning live instead of a post-hoc summary.
model_reasoning_summary = "detailed"
# Compact at 80% of the selected model context window so long sessions do not
# disable compaction or run into the provider limit.
model_auto_compact_token_limit = {compact_limit}
model_catalog_json = "{catalog_path}"
# Auto-injected by Basiliskos: point the built-in OpenAI provider at the relay.
openai_base_url = "http://127.0.0.1:{port}/v1"
"#,
        model = model.replace('"', ""),
        reasoning_line = reasoning_line,
        compact_limit = compact_limit,
        port = port,
        catalog_path = catalog_path.replace('\\', "/"),
    )
}

const CODEX_OWNED_CONFIG_KEYS: [&str; 7] = [
    "model",
    "model_reasoning_effort",
    "model_reasoning_summary",
    "model_auto_compact_token_limit",
    "model_catalog_json",
    "openai_base_url",
    "model_provider",
];

fn top_level_toml_key(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') || trimmed.starts_with('[') || trimmed.starts_with(']') {
        return None;
    }
    let key = trimmed.split_once('=')?.0.trim();
    (!key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_'))
    .then_some(key)
}

/// Updates only Basiliskos-owned top-level keys. Codex app settings and tables
/// outside this allowlist remain byte-for-byte intact across route refreshes.
fn merge_codex_config(existing: Option<&str>, generated: &str) -> String {
    let generated_lines = generated
        .lines()
        .filter(|line| {
            top_level_toml_key(line).is_some_and(|key| CODEX_OWNED_CONFIG_KEYS.contains(&key))
        })
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let Some(existing) = existing else {
        return generated.to_string();
    };
    let mut output = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut in_table = false;
    let mut inserted_missing = false;
    for line in existing.lines() {
        if line.trim_start().starts_with('[') {
            if !inserted_missing {
                for generated_line in &generated_lines {
                    if let Some(key) = top_level_toml_key(generated_line) {
                        if seen.insert(key.to_string()) {
                            output.push(generated_line.clone());
                        }
                    }
                }
                inserted_missing = true;
            }
            in_table = true;
        }
        if !in_table {
            if let Some(key) = top_level_toml_key(line) {
                if CODEX_OWNED_CONFIG_KEYS.contains(&key) {
                    if let Some(replacement) = generated_lines
                        .iter()
                        .find(|candidate| top_level_toml_key(candidate) == Some(key))
                    {
                        output.push(replacement.clone());
                        seen.insert(key.to_string());
                    }
                    continue;
                }
            }
        }
        output.push(line.to_string());
    }
    if !inserted_missing {
        for line in generated_lines {
            if let Some(key) = top_level_toml_key(&line) {
                if seen.insert(key.to_string()) {
                    output.push(line);
                }
            }
        }
    }
    format!("{}\n", output.join("\n"))
}

/// Builds the `ModelsResponse` catalog for the isolated Codex picker: one
/// `ModelInfo` per Basiliskos route model, slug = the real upstream model id so
/// CLIProxyAPI routes the chosen model to the right credential. The system
/// prompt is a neutral Basiliskos-authored agent prompt (the openai/codex
/// prompt made routed models falsely claim to be OpenAI's Codex).
/// Providers that currently have at least one enabled credential. The Codex
/// window's model picker only offers models whose provider is actually
/// authenticated; an un-authed provider (e.g. Claude with no credential) is
/// dropped so the picker never shows a model that cannot route.
fn enabled_providers(auth: &Path) -> std::collections::HashSet<String> {
    let mut providers = std::collections::HashSet::new();
    let mut directories = vec![auth.to_path_buf()];
    if let Ok(keys) = keys_dir() {
        if !directories.contains(&keys) {
            directories.push(keys);
        }
    }
    for directory in directories {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            let file_name = entry.file_name().to_string_lossy().to_string();
            let Ok(raw) = fs::read_to_string(&path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<Value>(&raw) else {
                continue;
            };
            let Some(provider) = account_provider(&value, &file_name) else {
                continue;
            };
            if value
                .get("disabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                continue;
            }
            providers.insert(provider);
        }
    }
    providers
}

fn codex_catalog_models(
    enabled: &std::collections::HashSet<String>,
    active_provider: Option<&str>,
) -> Vec<Value> {
    let base_instructions = include_str!("../assets/codex-system-prompt.md");
    let hidden = load_hidden_models().unwrap_or_default();
    let mut providers: Vec<&str> = Vec::new();
    if let Some(active) = active_provider {
        if enabled.contains(active) && SUPPORTED_PROVIDERS.contains(&active) {
            providers.push(active);
        }
    }
    for provider in SUPPORTED_PROVIDERS {
        if enabled.contains(provider) && !providers.contains(&provider) {
            providers.push(provider);
        }
    }

    let mut models = Vec::new();
    for provider in providers {
        for spec in model_specs(provider) {
            if hidden.contains(spec.id) {
                continue;
            }
            let context_window = context_window_for_route(provider, spec.id).unwrap_or(200_000);
            let reasoning_levels = spec
                .thinking_levels
                .iter()
                .map(|level| {
                    serde_json::json!({
                        "effort": level,
                        "description": thinking_level_label(level),
                    })
                })
                .collect::<Vec<_>>();
            let is_primary =
                spec.id == default_model(provider) || Some(provider) == active_provider;
            models.push(serde_json::json!({
                "slug": spec.id,
                "display_name": spec.label,
                "description": format!("{} via the Basiliskos relay", spec.label),
                "supported_reasoning_levels": reasoning_levels,
                "shell_type": "default",
                "visibility": "list",
                "supported_in_api": true,
                "priority": if is_primary { 1 } else { 0 },
                "default_reasoning_summary": "auto",
                "support_verbosity": false,
                "web_search_tool_type": "text",
                "truncation_policy": { "mode": "tokens", "limit": context_window },
                "supports_parallel_tool_calls": true,
                "experimental_supported_tools": [],
                "input_modalities": ["text", "image"],
                "context_window": context_window,
                "base_instructions": base_instructions,
            }));
        }
    }
    models
}

/// Write a Basiliskos-owned Codex home that points only this client at the
/// local relay. Never touches the user's global `~/.codex`. Mirrors the Claude
/// profile write; the Codex app reads `config.toml`/`auth.json` directly from
/// `CODEX_HOME`, and the generated `model-catalog.json` feeds its picker.
fn write_isolated_codex_config(home: &Path, state: &ControllerState) -> Result<(), String> {
    secure_create_dir_all(home)?;
    let provider = state
        .active_codex_account
        .as_deref()
        .and_then(|file_name| account_provider(&Value::Null, file_name))
        .ok_or_else(|| "Choose a Codex account before opening Basiliskos Codex.".to_string())?;
    let default_route = state
        .codex_routes
        .get(&provider)
        .cloned()
        .unwrap_or_else(|| RouteSelection {
            model: default_model(&provider).to_string(),
            thinking: "auto".into(),
        });
    let (model, thinking) = (default_route.model, default_route.thinking);
    // Picker catalog: only models whose provider is currently authenticated,
    // so an un-authed provider (Claude with no credential) is never offered.
    let catalog = serde_json::json!({
        "models": codex_catalog_models(&enabled_providers(&auth_dir()?), Some(&provider))
    });
    durable_write(
        &home.join("model-catalog.json"),
        serde_json::to_vec_pretty(&catalog)
            .map_err(|error| format!("Could not serialize the Codex model catalog: {error}"))?
            .as_slice(),
    )?;
    let catalog_path = home
        .join("model-catalog.json")
        .to_string_lossy()
        .replace('\\', "/");
    let generated_config = codex_config_toml(&model, &thinking, GATEWAY_PORT, &catalog_path);
    let existing_config = fs::read_to_string(home.join("config.toml")).ok();
    let merged_config = merge_codex_config(existing_config.as_deref(), &generated_config);
    durable_write(&home.join("config.toml"), merged_config.as_bytes())?;
    // Seed auth.json only if missing Ã¢â‚¬â€ the provider authenticates to the
    // Basiliskos relay via env_key; the app's own login state is anchored
    // separately (anchored-account milestone).
    let auth_path = home.join("auth.json");
    if !auth_path.exists() {
        durable_write(
            &auth_path,
            br#"{
  "OPENAI_API_KEY": null
}
"#,
        )?;
    }
    Ok(())
}

/// Seeds the isolated Codex home's `auth.json` with the ANCHOR account's real
/// ChatGPT credential, translated from the Basiliskos relay vault. The app's
/// auth manager reloads `auth.json` continuously, so a fresh seed logs the
/// isolated window in without any manual sign-in. Returns Ok(false) when the
/// anchor credential is absent (the null seed stays in place).
fn seed_isolated_codex_auth_at(
    home: &Path,
    auth: &Path,
    anchor_file_name: &str,
) -> Result<bool, String> {
    let anchor_path = auth.join(anchor_file_name);
    if !anchor_path.is_file() {
        return Ok(false);
    }
    let raw = fs::read_to_string(&anchor_path)
        .map_err(|error| format!("Could not read the anchor Codex credential: {error}"))?;
    let relay: Value =
        serde_json::from_str(&raw).map_err(|_| "The anchor Codex credential is invalid")?;
    let mut native = crate::codex_cli::translate_codex_cred(&relay)?;
    // The real native file carries auth_mode = "chatgpt"; the app's auth
    // manager expects it to recognize the credential as a ChatGPT OAuth login.
    native["auth_mode"] = Value::String("chatgpt".into());
    let bytes = serde_json::to_vec_pretty(&native)
        .map_err(|error| format!("Could not serialize the anchored Codex credential: {error}"))?;
    let destination = home.join("auth.json");
    if let Ok(existing_raw) = fs::read_to_string(&destination) {
        if let Ok(existing) = serde_json::from_str::<Value>(&existing_raw) {
            let existing_access = existing
                .get("tokens")
                .and_then(|tokens| tokens.get("access_token"))
                .or_else(|| existing.get("access_token"));
            let native_access = native
                .get("tokens")
                .and_then(|tokens| tokens.get("access_token"));
            if existing_access == native_access {
                return Ok(false);
            }
        }
    }
    durable_write(&destination, &bytes)?;
    Ok(true)
}

fn seed_isolated_codex_auth(home: &Path) -> Result<bool, String> {
    seed_isolated_codex_auth_at(home, &auth_dir()?, CODEX_ANCHOR_FILE_NAME)
}

fn restore_legacy_shared_config_if_needed(state: &mut ControllerState) -> Result<(), String> {
    let meta_path = shared_claude_library_dir()?.join("_meta.json");
    if !meta_path.exists() {
        if state.previous_claude_applied_id.take().is_some() {
            save_state(state)?;
        }
        return Ok(());
    }
    let mut meta: Value = serde_json::from_str(
        &fs::read_to_string(&meta_path)
            .map_err(|error| format!("Could not read the previous Claude config: {error}"))?,
    )
    .map_err(|error| format!("The previous Claude config metadata is invalid: {error}"))?;
    let is_hydra_applied =
        meta.get("appliedId").and_then(Value::as_str) == Some(state.claude_config_id.as_str());
    if !is_hydra_applied {
        if state.previous_claude_applied_id.take().is_some() {
            save_state(state)?;
        }
        return Ok(());
    }
    let object = meta
        .as_object_mut()
        .ok_or("Claude config metadata must be an object")?;
    object.insert(
        "appliedId".into(),
        state
            .previous_claude_applied_id
            .clone()
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    durable_write(
        &meta_path,
        &serde_json::to_vec_pretty(&meta).map_err(|error| error.to_string())?,
    )?;
    state.previous_claude_applied_id = None;
    save_state(state)
}

#[cfg(target_os = "windows")]
fn installed_claude_exe() -> Result<PathBuf, String> {
    let script = "(Get-AppxPackage -Name Claude | Sort-Object Version -Descending | Select-Object -First 1 -ExpandProperty InstallLocation)";
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    hidden(&mut command);
    let output = command
        .output()
        .map_err(|error| format!("Could not locate Claude for Windows: {error}"))?;
    if !output.status.success() {
        return Err("Claude for Windows is not installed for this user.".into());
    }
    let install_location = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if install_location.is_empty() {
        return Err(
            "Claude for Windows is not installed for this user. Install it from the Microsoft Store."
                .into(),
        );
    }
    let executable = PathBuf::from(&install_location)
        .join("app")
        .join("Claude.exe");
    let normalized = executable.to_string_lossy().to_ascii_lowercase();
    if !normalized.contains("\\windowsapps\\claude_")
        || !normalized.ends_with("\\app\\claude.exe")
        || !executable.is_file()
    {
        return Err("The installed Claude for Windows executable could not be verified.".into());
    }
    Ok(executable)
}

#[cfg(target_os = "windows")]
fn maybe_apply_claude_icons(app: &AppHandle, pid: u32, state: &ControllerState) {
    if !should_apply_claude_window_icon(state.claude_window_icon) {
        log_icon_line("icon reapply skipped (claude_window_icon=system)");
        return;
    }
    let Ok(window_ico) = claude_icon_path(app, ClaudeIconKind::WindowBlack) else {
        log_icon_line("window ico path missing");
        return;
    };
    let tray_ico = claude_icon_path(app, ClaudeIconKind::TrayInverted).unwrap_or_default();
    spawn_claude_icon_reapply(pid, window_ico, tray_ico);
}

#[cfg(not(target_os = "windows"))]
fn maybe_apply_claude_icons(_app: &AppHandle, _pid: u32, _state: &ControllerState) {}

/// For an active API-key account, Claude's `/model` picker can't list its live
/// (non-Anthropic) models, so surface the single active routing model through
/// Claude's `ANTHROPIC_CUSTOM_MODEL_OPTION` escape hatch. OAuth accounts return
/// None — they use the aliased in-app picker.
fn claude_custom_model_hook(
    state: &ControllerState,
    accounts: &[GatewayAccount],
) -> Option<String> {
    let file = state.active_account.as_deref()?;
    let account = accounts.iter().find(|account| account.file_name == file)?;
    if account.auth != "api_key" {
        return None;
    }
    let model = normalized_route(state, &account.provider).model;
    (!model.is_empty()).then_some(model)
}

#[tauri::command]
pub fn launch_hydra_claude(app: AppHandle) -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    #[cfg(target_os = "windows")]
    {
        if !gateway_running() {
            start_gateway_locked(app.clone())?;
        }
        let mut state = prepare_config()?;
        restore_legacy_shared_config_if_needed(&mut state)?;
        let accounts = list_accounts_inner(&state)?;
        if !accounts.iter().any(|account| account.active) {
            return Err("Choose an account before opening Basiliskos Claude.".into());
        }
        let profile = isolated_claude_profile_dir()?;
        write_isolated_claude_config(&profile, &state)?;
        let executable = installed_claude_exe()?;
        let log_dir = profile.join("Basiliskos Logs");
        secure_create_dir_all(&log_dir)?;
        if hydra_claude_running() {
            if let Ok(runtime) = runtime_lock() {
                if let Some(child) = runtime.claude_child.as_ref() {
                    maybe_apply_claude_icons(&app, child.id(), &state);
                }
            }
            return gateway_snapshot_locked();
        }
        let stdout_path = log_dir.join("launcher.stdout.log");
        let stderr_path = log_dir.join("launcher.stderr.log");
        durable_write(&stdout_path, b"")?;
        durable_write(&stderr_path, b"")?;
        let stdout = fs::File::create(&stdout_path)
            .map_err(|error| format!("Could not create the Basiliskos Claude log: {error}"))?;
        let stderr = fs::File::create(&stderr_path)
            .map_err(|error| format!("Could not create the Basiliskos Claude log: {error}"))?;
        let mut command = Command::new(&executable);
        // Do not set CLAUDE_USER_DATA_DIR: 1.40609 then reads stock 1p
        // userData. `--user-data-dir` must match `%LOCALAPPDATA%\Claude-3p`.
        command
            .arg(format!("--user-data-dir={}", profile.to_string_lossy()))
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        // Drill the active API-key routing model into Claude's /model picker so
        // the user sees the one model Basiliskos is actually routing.
        if let Some(model) = claude_custom_model_hook(&state, &accounts) {
            command.env("ANTHROPIC_CUSTOM_MODEL_OPTION", model);
        }
        hidden(&mut command);
        let mut child = command.spawn().map_err(|error| {
            format!("Could not open the isolated Basiliskos Claude window: {error}")
        })?;
        let job = assign_gateway_to_kill_on_close_job(&child).inspect_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
        })?;
        let pid = child.id();
        {
            let mut runtime = runtime_lock()?;
            runtime.claude_child = Some(child);
            runtime.claude_job = job;
            runtime.claude_root_pid = Some(pid);
            runtime.claude_executable = Some(executable);
            runtime.claude_profile = Some(profile.clone());
        }
        maybe_apply_claude_icons(&app, pid, &state);
        std::thread::sleep(Duration::from_millis(900));
        if !hydra_claude_running() {
            return Err(
                "Basiliskos Claude exited during startup. Check %LOCALAPPDATA%\\Claude-3p\\Basiliskos Logs."
                    .into(),
            );
        }
        gateway_snapshot_locked()
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        Err("The isolated Basiliskos Claude window is available on Windows only".into())
    }
}

#[tauri::command]
pub fn stop_hydra_claude() -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    stop_hydra_claude_runtime();
    gateway_snapshot_locked()
}

#[cfg(target_os = "windows")]
fn installed_codex_exe() -> Result<PathBuf, String> {
    let script = "(Get-AppxPackage -Name OpenAI.Codex | Sort-Object Version -Descending | Select-Object -First 1 -ExpandProperty InstallLocation)";
    let mut command = Command::new("powershell.exe");
    command.args(["-NoProfile", "-NonInteractive", "-Command", script]);
    hidden(&mut command);
    let output = command
        .output()
        .map_err(|error| format!("Could not locate the Codex desktop app: {error}"))?;
    if !output.status.success() {
        return Err("The Codex desktop app is not installed for this user.".into());
    }
    let install_location = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if install_location.is_empty() {
        return Err(
            "The Codex desktop app is not installed for this user. Install it from the Microsoft Store."
                .into(),
        );
    }
    let executable = PathBuf::from(&install_location)
        .join("app")
        .join("ChatGPT.exe");
    let normalized = executable.to_string_lossy().to_ascii_lowercase();
    if !normalized.contains("\\windowsapps\\openai.codex_")
        || !normalized.ends_with("\\app\\chatgpt.exe")
        || !executable.is_file()
    {
        return Err("The installed Codex desktop app executable could not be verified.".into());
    }
    Ok(executable)
}

#[cfg(target_os = "windows")]
fn maybe_apply_codex_icons(app: &AppHandle, pid: u32) {
    let Ok(window_ico) = codex_icon_path(app, CodexIconKind::WindowBlack) else {
        codex_log_icon_line("window ico path missing");
        return;
    };
    spawn_codex_icon_reapply(pid, window_ico);
}

#[cfg(not(target_os = "windows"))]
fn maybe_apply_codex_icons(_app: &AppHandle, _pid: u32) {}

/// Opens the isolated Basiliskos Codex window: the real installed Codex
/// desktop app (MSIX `OpenAI.Codex`, entry `app\ChatGPT.exe`) launched with an
/// isolated `--user-data-dir` + `CODEX_HOME`, pointing only at the local
/// relay. The user's normal Codex app can run at the same time (verified M0:
/// the global single-instance lock is scoped by the Chromium user-data dir).
#[tauri::command]
pub fn launch_hydra_codex_app(app: AppHandle) -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    #[cfg(target_os = "windows")]
    {
        if !gateway_running() {
            start_gateway_locked(app.clone())?;
        }
        let mut state = prepare_config()?;
        restore_legacy_shared_config_if_needed(&mut state)?;
        if !list_accounts_inner(&state)?
            .iter()
            .any(|account| account.active_for_codex)
        {
            return Err("Choose a Codex account before opening Basiliskos Codex.".into());
        }
        let home = isolated_codex_home()?;
        write_isolated_codex_config(&home, &state)?;
        // Anchor the isolated window's login: real ChatGPT credential from the
        // relay vault (one-refresher rule Ã¢â‚¬â€ the anchor is excluded from the
        // relay's auto-refresh, so the isolated app owns it).
        match seed_isolated_codex_auth(&home) {
            Ok(true) => codex_log_icon_line(&format!(
                "anchored Codex login seeded from {CODEX_ANCHOR_FILE_NAME}"
            )),
            Ok(false) => codex_log_icon_line(
                "anchor Codex credential not found; isolated login stays unseeded",
            ),
            Err(error) => codex_log_icon_line(&format!("anchor seed failed: {error}")),
        }
        let executable = installed_codex_exe()?;
        let log_dir = home.join("Basiliskos Logs");
        secure_create_dir_all(&log_dir)?;
        if hydra_codex_running() {
            if let Ok(runtime) = runtime_lock() {
                if let Some(child) = runtime.codex_child.as_ref() {
                    maybe_apply_codex_icons(&app, child.id());
                }
            }
            return gateway_snapshot_locked();
        }
        let stdout_path = log_dir.join("launcher.stdout.log");
        let stderr_path = log_dir.join("launcher.stderr.log");
        durable_write(&stdout_path, b"")?;
        durable_write(&stderr_path, b"")?;
        let stdout = fs::File::create(&stdout_path)
            .map_err(|error| format!("Could not create the Basiliskos Codex log: {error}"))?;
        let stderr = fs::File::create(&stderr_path)
            .map_err(|error| format!("Could not create the Basiliskos Codex log: {error}"))?;
        let mut command = Command::new(&executable);
        command
            .arg(format!("--user-data-dir={}", home.to_string_lossy()))
            .env("CODEX_HOME", &home)
            .env("BASILISKOS_API_KEY", &state.api_key)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        hidden(&mut command);
        let mut child = command.spawn().map_err(|error| {
            format!("Could not open the isolated Basiliskos Codex window: {error}")
        })?;
        let job = assign_gateway_to_kill_on_close_job(&child).inspect_err(|_| {
            let _ = child.kill();
            let _ = child.wait();
        })?;
        let pid = child.id();
        let watcher_generation = CODEX_WATCHER_GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
        {
            let mut runtime = runtime_lock()?;
            runtime.codex_child = Some(child);
            runtime.codex_job = job;
            runtime.codex_root_pid = Some(pid);
            runtime.codex_watcher_generation = Some(watcher_generation);
            runtime.codex_executable = Some(executable);
            runtime.codex_home = Some(home.clone());
        }
        maybe_apply_codex_icons(&app, pid);
        spawn_codex_close_watcher(pid, watcher_generation);
        std::thread::sleep(Duration::from_millis(900));
        if !hydra_codex_running() {
            return Err(
                "Basiliskos Codex exited during startup. Check ~/.hydra-gateway/codex-profile/Basiliskos Logs."
                    .into(),
            );
        }
        gateway_snapshot_locked()
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        Err("The isolated Basiliskos Codex window is available on Windows only".into())
    }
}

#[tauri::command]
pub fn stop_hydra_codex_app() -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    stop_hydra_codex_runtime();
    gateway_snapshot_locked()
}

/// Appends a Codex dial event (method, content-type, body length, body head,
/// outcome) to a file the operator can read directly, independent of the
/// in-memory diagnostics feed. Local-only; the body head is the user's own
/// prompt.
fn append_codex_dial_log(line: &str) {
    use std::io::Write;
    let Ok(dir) = gateway_dir() else { return };
    let path = dir.join("controller-logs").join("codex-dial.log");
    if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(file, "{line}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::{unrecorded_usage_window, usage_window};

    #[test]
    fn direct_update_requires_the_canonical_installer_and_checksum_entry() {
        assert_eq!(
            release_installer_name("v1.1.18").unwrap(),
            "BasiliskOS_1.1.18_x64-setup.exe"
        );
        assert!(release_installer_name("../v1.1.18").is_err());
        let checksum = "a".repeat(64);
        // The manifest's casing wins: a client expecting either the historical
        // `Basiliskos_` prefix or the current `BasiliskOS_` prefix must resolve
        // the same asset and download it under its published name.
        let new_cased = "BasiliskOS_1.1.18_x64-setup.exe";
        let old_cased = "Basiliskos_1.1.18_x64-setup.exe";
        let manifest = format!("{checksum}  {new_cased}\n{}  other.exe", "b".repeat(64));
        assert_eq!(
            checksum_from_manifest(&manifest, new_cased),
            Some((checksum.clone(), new_cased.to_owned()))
        );
        assert_eq!(
            checksum_from_manifest(&manifest, old_cased),
            Some((checksum.clone(), new_cased.to_owned()))
        );
        assert_eq!(
            checksum_from_manifest(&manifest, "other.exe"),
            Some(("b".repeat(64), "other.exe".to_owned()))
        );
        assert_eq!(
            checksum_from_manifest("bad  Basiliskos_1.1.18_x64-setup.exe", old_cased),
            None
        );
    }

    fn temp_dir(name: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!("hydra-gateway-{name}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn auth_file(auth: &Path, file_name: &str, provider: &str) {
        fs::write(
            auth.join(file_name),
            serde_json::json!({"type": provider}).to_string(),
        )
        .unwrap();
    }

    #[test]
    fn profile_names_are_trimmed_and_bounded() {
        assert_eq!(normalized_account_label("  Studio  ").unwrap(), "Studio");
        assert!(normalized_account_label("   ").is_err());
        assert!(normalized_account_label(&"x".repeat(65)).is_err());
    }

    #[test]
    fn provider_usage_payloads_report_remaining_percent() {
        let claude = parse_claude_usage(&serde_json::json!({
            "five_hour": {"utilization": 32.5},
            "seven_day": {"utilization": 71.0}
        }));
        assert_eq!(claude[0], usage_window("5h", 32.5));
        assert_eq!(claude[1], usage_window("Week", 71.0));

        let codex = parse_codex_usage(&serde_json::json!({
            "rate_limit": {
                "primary_window": {
                    "used_percent": 12.0,
                    "limit_window_seconds": 18000,
                    "reset_at": 1786642233
                },
                "secondary_window": {
                    "used_percent": 44.0,
                    "limit_window_seconds": 604800,
                    "reset_at": "1787159999"
                }
            }
        }));
        assert_eq!(codex[0].label, "5h");
        assert_eq!(codex[0].used_percent, 12.0);
        assert_eq!(codex[0].resets_at_ms, Some(1_786_642_233_000));
        assert_eq!(codex[1].label, "Week");
        assert_eq!(codex[1].used_percent, 44.0);
        assert_eq!(codex[1].resets_at_ms, Some(1_787_159_999_000));

        let xai = parse_xai_usage(&serde_json::json!({
            "config": {
                "creditUsagePercent": 23.0,
                "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY"}
            }
        }));
        assert_eq!(xai[0], usage_window("Week", 23.0));

        let xai_product_specific = parse_xai_usage(&serde_json::json!({
            "config": {
                "creditUsagePercent": 100.0,
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "end": "2026-08-07T23:36:28.124325+08:00"
                },
                "productUsage": [
                    {"product": "GrokBuild", "usagePercent": 99.0},
                    {"product": "GrokChat", "usagePercent": 1.0}
                ]
            }
        }));
        assert_eq!(xai_product_specific[0].used_percent, 99.0);
        assert_eq!(
            xai_product_specific[0].resets_at_ms,
            Some(
                DateTime::parse_from_rfc3339("2026-08-07T23:36:28.124325+08:00")
                    .unwrap()
                    .timestamp_millis()
            )
        );

        let xai_charles_3ready = parse_xai_usage(&serde_json::json!({
            "config": {
                "currentPeriod": {
                    "type": "USAGE_PERIOD_TYPE_WEEKLY",
                    "end": "2026-08-11T21:56:01.29535+08:00"
                },
                "productUsage": [
                    {"product": "GrokBuild", "usagePercent": 99.0},
                    {"product": "GrokChat", "usagePercent": 1.0}
                ]
            }
        }));
        assert_eq!(
            xai_charles_3ready[0].resets_at_ms,
            Some(
                DateTime::parse_from_rfc3339("2026-08-11T21:56:01.29535+08:00")
                    .unwrap()
                    .timestamp_millis()
            )
        );

        // A real billing config (proven by `currentPeriod`) with no usage
        // fields means "hasn't used anything yet this period", not "broken" Ã¢â‚¬â€
        // a fresh/idle account, distinct from a genuine 0%-used reading.
        let xai_idle = parse_xai_usage(&serde_json::json!({
            "config": {"currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY"}}
        }));
        assert_eq!(xai_idle, vec![unrecorded_usage_window("Week")]);
        assert!(!xai_idle[0].known);

        // No billing config at all (not even currentPeriod) is still treated
        // as genuinely unavailable.
        assert!(parse_xai_usage(&serde_json::json!({})).is_empty());

        let kimi = parse_kimi_usage(&serde_json::json!({
            "usage": {"limit": 1000, "remaining": 650},
            "limits": [
                {"name": "5h", "detail": {"limit": 200, "used": 50}},
                {"window": {"duration": 7, "timeUnit": "DAY"}, "detail": {"limit": 500, "remaining": 100}}
            ]
        }));
        assert_eq!(kimi[0], usage_window("Plan", 35.0));
        assert_eq!(kimi[1], usage_window("5h", 25.0));
        assert_eq!(kimi[2], usage_window("Week", 80.0));
    }

    #[test]
    fn usage_denial_does_not_claim_the_saved_login_expired() {
        for provider in ["codex", "claude", "xai"] {
            let message = usage_http_error_message(provider, reqwest::StatusCode::UNAUTHORIZED);
            assert!(message.contains("saved login is active"));
            assert!(message.contains("Auto-retry"));
            assert!(!message.contains("Sign in again"));
        }
        assert!(should_refresh_after_usage_denial(
            "codex",
            reqwest::StatusCode::UNAUTHORIZED
        ));
        assert!(should_refresh_after_usage_denial(
            "claude",
            reqwest::StatusCode::UNAUTHORIZED
        ));
        assert!(!should_refresh_after_usage_denial(
            "xai",
            reqwest::StatusCode::UNAUTHORIZED
        ));
        assert_eq!(
            usage_refresh_failure_message(
                "Codex",
                "Sign in again to renew this Codex authorization"
            ),
            "Codex refresh grant was revoked. Re-login once to restore automatic refresh."
        );
        assert!(!usage_refresh_failure_message(
            "Codex",
            "Codex credential refresh is temporarily rate-limited"
        )
        .contains("Re-login"));
    }

    #[test]
    fn oauth_refresh_rotates_returned_tokens_and_preserves_omitted_refresh_grant() {
        let mut credential = serde_json::json!({
            "access_token": "old-access",
            "refresh_token": "keep-refresh",
            "disabled": true
        });
        apply_oauth_refresh(
            &mut credential,
            &serde_json::json!({
                "access_token": "new-access",
                "id_token": "new-id",
                "expires_in": 3600
            }),
            "Codex",
        )
        .unwrap();
        assert_eq!(
            credential.get("access_token").and_then(Value::as_str),
            Some("new-access")
        );
        assert_eq!(
            credential.get("refresh_token").and_then(Value::as_str),
            Some("keep-refresh")
        );
        assert_eq!(
            credential.get("id_token").and_then(Value::as_str),
            Some("new-id")
        );
        assert_eq!(credential.get("disabled"), Some(&Value::Bool(true)));
        assert!(credential_expiry(&credential).is_some());
    }

    #[test]
    fn crash_cleanup_removes_only_direct_stale_workspace_directories() {
        let root = temp_dir("stale-secret-workspaces");
        for name in ["login-session", "vision-sidecar"] {
            let child = root.join(name);
            secure_create_dir_all(&child).unwrap();
            durable_write(&child.join("credential.json"), br#"{"private":true}"#).unwrap();
        }
        let sentinel = root.join("keep.txt");
        fs::write(&sentinel, "not a workspace directory").unwrap();

        assert_eq!(remove_private_child_directories(&root).unwrap(), 2);
        assert!(sentinel.is_file());
        assert!(!root.join("login-session").exists());
        assert!(!root.join("vision-sidecar").exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn codex_config_toml_points_only_at_the_local_relay_over_responses_wire() {
        let config = codex_config_toml(
            "kimi-k3",
            "high",
            8317,
            "C:/codex-profile/model-catalog.json",
        );
        assert!(config.contains("model = \"kimi-k3\""));
        assert!(config.contains("model_reasoning_effort = \"high\""));
        assert!(config.contains("model_reasoning_summary = \"detailed\""));
        assert!(config.contains("model_auto_compact_token_limit = 160000"));
        assert!(config.contains("model_catalog_json = \"C:/codex-profile/model-catalog.json\""));
        // opencodex-proven loopback form: keep the built-in openai provider and
        // redirect it with root openai_base_url (the app's renderer allowlist
        // keeps only native OpenAI slugs in the picker).
        assert!(config.contains("openai_base_url = \"http://127.0.0.1:8317/v1\""));
        assert!(!config.contains("model_provider"));
        assert!(!config.contains("[model_providers."));
        // "auto" omits the effort key so the model's native default applies.
        let auto = codex_config_toml("grok-4.5", "auto", 8317, "C:/model-catalog.json");
        assert!(!auto.contains("model_reasoning_effort"));
    }

    #[test]
    fn codex_config_merge_preserves_user_settings_and_replaces_only_owned_keys() {
        let generated = codex_config_toml("gpt-5.6-terra", "auto", 8317, "C:/catalog.json");
        let existing = "approval_policy = \"never\"\nmodel = \"old\"\nopenai_base_url = \"https://old.invalid/v1\"\n[profiles.default]\nmodel = \"user-model\"\n";
        let merged = merge_codex_config(Some(existing), &generated);
        assert!(merged.contains("approval_policy = \"never\""));
        assert!(merged.contains("[profiles.default]"));
        assert!(merged.contains("model = \"gpt-5.6-terra\""));
        assert!(!merged.contains("model = \"old\""));
        assert!(!merged.contains("https://old.invalid"));
        assert!(merged.contains("model_auto_compact_token_limit = 160000"));
        let table_first = merge_codex_config(
            Some("[profiles.default]\nmodel = \"user-model\"\n"),
            &generated,
        );
        assert!(
            table_first.find("model = \"gpt-5.6-terra\"").unwrap()
                < table_first.find("[profiles.default]").unwrap()
        );
    }

    #[test]
    fn codex_catalog_advertises_real_upstream_model_ids() {
        let all: std::collections::HashSet<String> = SUPPORTED_PROVIDERS
            .iter()
            .map(|provider| provider.to_string())
            .collect();
        let models = codex_catalog_models(&all, None);
        let ids = models
            .iter()
            .filter_map(|model| model.get("slug").and_then(Value::as_str))
            .collect::<Vec<_>>();
        // Every advertised model id is a real upstream id the relay routes by.
        assert!(ids.contains(&"gemini-3.7-flash"));
        assert!(ids.contains(&"grok-4.5"));
        assert!(ids.contains(&"kimi-k3"));
        // Every entry carries the required ModelInfo fields + a vendored prompt.
        for model in &models {
            assert!(model.get("slug").is_some());
            assert!(model.get("display_name").is_some());
            assert!(model.get("supported_reasoning_levels").is_some());
            assert!(model.get("shell_type").is_some());
            assert!(model.get("visibility").is_some());
            assert!(model.get("supported_in_api").is_some());
            assert!(model.get("truncation_policy").is_some());
            assert!(model.get("supports_parallel_tool_calls").is_some());
            let prompt = model
                .get("base_instructions")
                .and_then(Value::as_str)
                .unwrap_or("");
            assert!(
                prompt.starts_with("You are an AI coding assistant"),
                "prompt must be present"
            );
        }
    }

    #[test]
    fn codex_catalog_offers_only_authenticated_providers() {
        let auth = temp_dir("codex-catalog-auth");
        auth_file(&auth, "xai-test.json", "xai");

        let enabled = enabled_providers(&auth);
        assert!(enabled.contains("xai"));
        assert!(!enabled.contains("claude"));
        assert!(!enabled.contains("codex"));
        assert!(!enabled.contains("kimi"));
        assert!(!enabled.contains("antigravity"));
        assert!(!enabled.contains("zai"));

        let models = codex_catalog_models(&enabled, None);
        let ids = models
            .iter()
            .filter_map(|model| model.get("slug").and_then(Value::as_str))
            .collect::<Vec<_>>();
        // Authenticated providers are offered.
        assert!(ids.contains(&"grok-4.5"));
        // Un-authed providers are dropped, so the picker never shows a model
        // that cannot route.
        assert!(!ids.contains(&"gpt-5.6-terra"));
        assert!(!ids.contains(&"kimi-k3"));
        assert!(!ids.contains(&"claude-sonnet-4-5-20250929"));
        assert!(!ids.contains(&"gemini-3.7-flash"));

        let _ = fs::remove_dir_all(auth);
    }

    #[test]
    fn codex_home_is_written_only_inside_the_isolated_home() {
        let root = temp_dir("codex-home");
        let home = root.join("isolated-home");
        let untouched = root.join("normal-config.toml");
        fs::write(&untouched, "normal-config-must-not-change").unwrap();
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: Some("kimi-1.json".into()),
            active_codex_account: Some("kimi-1.json".into()),
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        write_isolated_codex_config(&home, &state).unwrap();
        assert_eq!(
            fs::read_to_string(&untouched).unwrap(),
            "normal-config-must-not-change"
        );
        let config = fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(config.contains("openai_base_url = \"http://127.0.0.1:8317/v1\""));
        assert!(!config.contains("model_provider"));
        assert!(config.contains("model_catalog_json = \""));
        // The active kimi route's default model is advertised to the Codex client.
        assert!(config.contains(&format!("model = \"{}\"", default_model("kimi"))));
        // The picker catalog is written next to the config with real model ids.
        let catalog: Value =
            serde_json::from_str(&fs::read_to_string(home.join("model-catalog.json")).unwrap())
                .unwrap();
        assert!(catalog["models"].as_array().is_some());
        // Catalog slugs follow live ~/.hydra-gateway auth files. Membership is
        // covered by codex_catalog_offers_only_authenticated_providers.
        let auth: Value =
            serde_json::from_str(&fs::read_to_string(home.join("auth.json")).unwrap()).unwrap();
        assert_eq!(auth.get("OPENAI_API_KEY"), Some(&Value::Null));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "manual: dumps the generated catalog for the installed codex.exe"]
    fn dump_codex_catalog_for_validation() {
        let home = std::env::var("BASILISKOS_CATALOG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| std::env::temp_dir().join("basiliskos-catalog-check"));
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: Some("kimi-1.json".into()),
            active_codex_account: Some("kimi-1.json".into()),
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        write_isolated_codex_config(&home, &state).unwrap();
        println!("catalog+config written to {}", home.display());
    }

    #[test]
    fn anchored_codex_auth_is_translated_with_auth_mode() {
        let root = temp_dir("codex-anchor");
        let auth = root.join("auth");
        let home = root.join("isolated-home");
        secure_create_dir_all(&auth).unwrap();
        fs::write(
            auth.join("codex-anchor@example.com.json"),
            serde_json::json!({
                "access_token": "at-1",
                "account_id": "acct-1",
                "email": "anchor@example.com",
                "id_token": "id-1",
                "last_refresh": "2026-08-12T00:00:00Z",
                "refresh_token": "rt-1",
                "type": "codex",
            })
            .to_string(),
        )
        .unwrap();
        let seeded =
            seed_isolated_codex_auth_at(&home, &auth, "codex-anchor@example.com.json").unwrap();
        assert!(seeded);
        assert!(
            !seed_isolated_codex_auth_at(&home, &auth, "codex-anchor@example.com.json").unwrap()
        );
        let native: Value =
            serde_json::from_str(&fs::read_to_string(home.join("auth.json")).unwrap()).unwrap();
        assert_eq!(native["auth_mode"], "chatgpt");
        assert_eq!(native["tokens"]["account_id"], "acct-1");
        assert_eq!(native["tokens"]["access_token"], "at-1");
        assert_eq!(native["OPENAI_API_KEY"], Value::Null);
        // A missing anchor is not an error: the null seed stays in place.
        let absent = seed_isolated_codex_auth_at(&home, &auth, "missing.json").unwrap();
        assert!(!absent);
        assert_eq!(
            serde_json::from_str::<Value>(&fs::read_to_string(home.join("auth.json")).unwrap())
                .unwrap()["auth_mode"],
            "chatgpt"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn codex_home_write_requires_a_selected_account() {
        let root = temp_dir("codex-no-account");
        let home = root.join("isolated-home");
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        assert!(write_isolated_codex_config(&home, &state).is_err());
        assert!(!home.join("config.toml").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn backend_config_is_loopback_only() {
        let auth = temp_dir("config");
        auth_file(&auth, "xai-test.json", "xai");
        let state = ControllerState {
            api_key: "test-secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: Some("xai-test.json".into()),
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        let config = render_config(&auth, &state);
        assert!(config.contains("host: \"127.0.0.1\""));
        assert!(config.contains("port: 8318"));
        assert!(config.contains("disable-control-panel: true"));
        assert!(config.contains("streaming:\n  keepalive-seconds: 15"));
        assert!(config.contains("request-retry: 0"));
        assert!(config.contains("max-retry-credentials: 1"));
        assert!(config.contains("bootstrap-retries: 0"));
        assert!(config.contains("disable-claude-cloak-mode: true"));
        assert!(config.contains("oauth-model-alias:"));
        assert!(config.contains("gemini-3.7-flash"));
        let _ = fs::remove_dir_all(auth);
    }

    #[test]
    fn claude_config_is_written_only_inside_isolated_profile() {
        let root = temp_dir("claude");
        let profile = root.join("isolated-profile");
        let untouched = root.join("normal-claude-config.json");
        fs::write(&untouched, "normal-config-must-not-change").unwrap();
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "hydra-id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        write_isolated_claude_config(&profile, &state).unwrap();
        assert_eq!(
            fs::read_to_string(&untouched).unwrap(),
            "normal-config-must-not-change"
        );
        let library = profile.join("configLibrary");
        let meta: Value =
            serde_json::from_str(&fs::read_to_string(library.join("_meta.json")).unwrap()).unwrap();
        assert_eq!(
            meta.get("appliedId").and_then(Value::as_str),
            Some("hydra-id")
        );
        let config: Value =
            serde_json::from_str(&fs::read_to_string(library.join("hydra-id.json")).unwrap())
                .unwrap();
        assert_eq!(
            config.get("inferenceGatewayApiKey").and_then(Value::as_str),
            Some("secret")
        );
        assert_eq!(
            config
                .get("inferenceModels")
                .and_then(Value::as_array)
                .and_then(|models| models.first())
                .and_then(|model| model.get("name"))
                .and_then(Value::as_str),
            Some("claude-fable-5")
        );
        let deployment: Value = serde_json::from_str(
            &fs::read_to_string(profile.join("claude_desktop_config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            deployment.get("deploymentMode").and_then(Value::as_str),
            Some("3p")
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn advertised_model_name_is_a_valid_claude_desktop_routing_alias() {
        let mut state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        assert_eq!(
            advertised_model_name(&state, Some("claude")),
            "claude-fable-5"
        );
        state.routes.insert(
            "xai".into(),
            RouteSelection {
                model: "grok-4.5".into(),
                thinking: "high".into(),
            },
        );
        assert_eq!(advertised_model_name(&state, Some("xai")), "claude-fable-5");
        state.routes.insert(
            "kimi".into(),
            RouteSelection {
                model: "kimi-k3".into(),
                thinking: "max".into(),
            },
        );
        assert_eq!(
            advertised_model_name(&state, Some("kimi")),
            "claude-fable-5"
        );
        assert_eq!(advertised_model_name(&state, None), "claude-fable-5");
    }

    #[test]
    fn claude_config_merge_preserves_user_settings_and_unrelated_configs() {
        let root = temp_dir("claude-merge");
        let profile = root.join("isolated-profile");
        let library = profile.join("configLibrary");
        fs::create_dir_all(&library).unwrap();
        let meta_path = library.join("_meta.json");
        let generated_path = library.join("hydra-id.json");
        let deployment_path = profile.join("claude_desktop_config.json");
        fs::write(
            &meta_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "version": 7,
                "appliedId": "personal-id",
                "configs": [{"id": "personal-id", "name": "Personal", "pinned": true}],
                "uiDensity": "compact"
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            &generated_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "inferenceGatewayBaseUrl": "http://old.invalid",
                "customSetting": {"keep": true}
            }))
            .unwrap(),
        )
        .unwrap();
        fs::write(
            &deployment_path,
            serde_json::to_vec_pretty(&serde_json::json!({
                "deploymentMode": "personal",
                "theme": "dark",
                "mcpServers": {"custom": {"command": "example"}}
            }))
            .unwrap(),
        )
        .unwrap();

        let mut state = ControllerState {
            api_key: "new-secret".into(),
            claude_config_id: "hydra-id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        write_isolated_claude_config(&profile, &state).unwrap();

        let meta: Value = serde_json::from_slice(&fs::read(&meta_path).unwrap()).unwrap();
        assert_eq!(meta.get("version").and_then(Value::as_i64), Some(7));
        assert_eq!(
            meta.get("uiDensity").and_then(Value::as_str),
            Some("compact")
        );
        assert_eq!(
            meta.get("appliedId").and_then(Value::as_str),
            Some("hydra-id")
        );
        let configs = meta.get("configs").and_then(Value::as_array).unwrap();
        assert!(configs.iter().any(|entry| {
            entry.get("id").and_then(Value::as_str) == Some("personal-id")
                && entry.get("pinned").and_then(Value::as_bool) == Some(true)
        }));
        assert!(configs.iter().any(|entry| {
            entry.get("id").and_then(Value::as_str) == Some("hydra-id")
                && entry.get("name").and_then(Value::as_str) == Some("Basiliskos")
        }));

        let generated: Value = serde_json::from_slice(&fs::read(&generated_path).unwrap()).unwrap();
        assert_eq!(
            generated
                .get("customSetting")
                .and_then(|value| value.get("keep"))
                .and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(
            generated
                .get("inferenceGatewayApiKey")
                .and_then(Value::as_str),
            Some("new-secret")
        );
        assert_eq!(
            generated
                .get("inference")
                .and_then(|value| value.get("provider"))
                .and_then(Value::as_str),
            Some("gateway")
        );
        assert_eq!(
            generated
                .get("inference")
                .and_then(|value| value.get("credential"))
                .and_then(|value| value.get("kind"))
                .and_then(Value::as_str),
            Some("static")
        );
        assert_eq!(
            generated
                .get("authentication")
                .and_then(|value| value.get("disableClaudeAiSignIn"))
                .and_then(Value::as_bool),
            Some(true)
        );

        let deployment: Value =
            serde_json::from_slice(&fs::read(&deployment_path).unwrap()).unwrap();
        assert_eq!(
            deployment.get("theme").and_then(Value::as_str),
            Some("dark")
        );
        assert_eq!(
            deployment
                .get("mcpServers")
                .and_then(|value| value.get("custom"))
                .and_then(|value| value.get("command"))
                .and_then(Value::as_str),
            Some("example")
        );
        assert_eq!(
            deployment.get("deploymentMode").and_then(Value::as_str),
            Some("3p")
        );

        let backup_root = profile.join("Basiliskos Backups");
        let backup_day = fs::read_dir(&backup_root)
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let backed_up_deployment: Value = serde_json::from_slice(
            &fs::read(backup_day.join("claude_desktop_config.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            backed_up_deployment
                .get("deploymentMode")
                .and_then(Value::as_str),
            Some("personal")
        );

        state.routes.insert(
            "codex".into(),
            RouteSelection {
                model: "gpt-5.5-codex".into(),
                thinking: "high".into(),
            },
        );
        write_isolated_claude_config(&profile, &state).unwrap();
        let deployment_after_repeat: Value =
            serde_json::from_slice(&fs::read(&deployment_path).unwrap()).unwrap();
        assert_eq!(
            deployment_after_repeat.get("theme").and_then(Value::as_str),
            Some("dark")
        );
        assert_eq!(fs::read_dir(&backup_root).unwrap().count(), 1);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_claude_json_fails_closed_without_overwriting_other_files() {
        let root = temp_dir("claude-invalid");
        let profile = root.join("isolated-profile");
        let library = profile.join("configLibrary");
        fs::create_dir_all(&library).unwrap();
        let meta_path = library.join("_meta.json");
        let generated_path = library.join("hydra-id.json");
        let deployment_path = profile.join("claude_desktop_config.json");
        let meta_before = br#"{"appliedId":"personal","configs":[],"custom":true}"#;
        let generated_before = br#"{"customSetting":"keep"}"#;
        let invalid_deployment = b"{ definitely not valid json";
        fs::write(&meta_path, meta_before).unwrap();
        fs::write(&generated_path, generated_before).unwrap();
        fs::write(&deployment_path, invalid_deployment).unwrap();
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "hydra-id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };

        let error = write_isolated_claude_config(&profile, &state).unwrap_err();
        assert!(error.contains("Refusing to overwrite invalid JSON"));
        assert_eq!(fs::read(&meta_path).unwrap(), meta_before);
        assert_eq!(fs::read(&generated_path).unwrap(), generated_before);
        assert_eq!(fs::read(&deployment_path).unwrap(), invalid_deployment);
        assert!(!profile.join("Basiliskos Backups").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn front_proxy_rewrites_the_model_and_appends_route_identity() {
        let mut state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "hydra-id".into(),
            previous_claude_applied_id: None,
            active_account: Some("xai-test.json".into()),
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        state.routes.insert(
            "xai".into(),
            RouteSelection {
                model: "grok-4.5".into(),
                thinking: "high".into(),
            },
        );
        let mut request = serde_json::json!({
            "model": "claude-opus-4-99",
            "system": [{"type": "text", "text": "You are powered by Sonnet."}],
            "messages": [{"role": "user", "content": "Who are you?"}]
        });
        rewrite_claude_request(&mut request, &state, "xai", true).unwrap();
        assert_eq!(
            request.get("model").and_then(Value::as_str),
            Some("grok-4.5(high)")
        );
        let system = request
            .get("system")
            .and_then(Value::as_array)
            .expect("system remains an array");
        assert_eq!(system.len(), 2);
        assert!(system[1]
            .get("text")
            .and_then(Value::as_str)
            .unwrap()
            .contains("You are a routed coding assistant"));
    }

    #[test]
    fn grok_4_5_maps_desktop_effort_to_supported_thinking_levels() {
        let mut state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: Some("xai-test.json".into()),
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        state.routes.insert(
            "xai".into(),
            RouteSelection {
                model: "grok-4.5".into(),
                thinking: "high".into(),
            },
        );

        for (effort, expected) in [
            ("low", "grok-4.5(low)"),
            ("medium", "grok-4.5(low)"),
            ("high", "grok-4.5(high)"),
            ("xhigh", "grok-4.5(high)"),
            ("max", "grok-4.5(high)"),
        ] {
            let mut request = serde_json::json!({
                "model": "claude-opus-4-99",
                "output_config": { "effort": effort }
            });
            rewrite_claude_request(&mut request, &state, "xai", false).unwrap();
            assert_eq!(request.get("model").and_then(Value::as_str), Some(expected));
            assert!(request.get("output_config").is_none());
        }
    }

    #[test]
    fn grok_4_5_uses_a_truthful_context_budget() {
        let request = serde_json::json!({
            "model": "grok-4.5(high)",
            "max_tokens": 16_384
        });
        let budget = context_budget_for_request("xai", &request).unwrap();
        assert_eq!(budget.window_tokens, 500_000);
        assert_eq!(budget.reserved_output_tokens, 16_384);
        assert!(499_999_u64.saturating_add(budget.reserved_output_tokens) > budget.window_tokens);
        assert!(483_616_u64.saturating_add(budget.reserved_output_tokens) <= budget.window_tokens);
        assert!(context_budget_for_request("codex", &request).is_none());
    }

    #[test]
    fn endpoint_health_requires_success_and_expected_body_marker() {
        fn serve_once(response: &'static str) -> u16 {
            let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
            let port = listener.local_addr().unwrap().port();
            thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut request = [0_u8; 1024];
                let read = stream.read(&mut request).unwrap();
                let request = String::from_utf8_lossy(&request[..read]);
                assert!(request.starts_with("GET /ready HTTP/1.1"));
                assert!(request.contains("x-api-key: test-key"));
                stream.write_all(response.as_bytes()).unwrap();
            });
            port
        }

        let healthy = serve_once(
            "HTTP/1.1 200 OK\r\nContent-Length: 16\r\nConnection: close\r\n\r\n{\"backend\":true}",
        );
        assert!(endpoint_health_check(
            healthy,
            "/ready",
            "test-key",
            "\"backend\":true"
        ));

        let degraded = serve_once(
            "HTTP/1.1 503 Service Unavailable\r\nContent-Length: 17\r\nConnection: close\r\n\r\n{\"backend\":false}",
        );
        assert!(!endpoint_health_check(
            degraded,
            "/ready",
            "test-key",
            "\"backend\":true"
        ));
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn gateway_job_kills_backend_when_owner_handle_closes() {
        let mut child = Command::new("cmd")
            .args(["/C", "ping 127.0.0.1 -n 30 > nul"])
            .spawn()
            .unwrap();
        let job = assign_gateway_to_kill_on_close_job(&child).unwrap();
        close_gateway_job(job);
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if child.try_wait().unwrap().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(25));
        }
        let _ = child.kill();
        let _ = child.wait();
        panic!("backend process survived the KILL_ON_JOB_CLOSE job handle");
    }

    #[test]
    fn account_selection_pins_one_account_per_provider_and_keeps_other_providers() {
        // The isolated Codex window routes picked models to their real
        // providers, so selecting an account must not disable OTHER providers'
        // credentials. Only same-provider accounts are pinned (one enabled per
        // provider) so CLIProxyAPI's credential selection stays unambiguous.
        let root = temp_dir("accounts");
        let auth = root.join("auth");
        fs::create_dir_all(&auth).unwrap();
        fs::write(
            auth.join("codex-a.json"),
            r#"{"type":"codex","disabled":false}"#,
        )
        .unwrap();
        fs::write(
            auth.join("xai-b.json"),
            r#"{"type":"xai","disabled":false}"#,
        )
        .unwrap();
        fs::write(auth.join("xai-c.json"), r#"{"type":"xai","disabled":true}"#).unwrap();
        let account = |file_name: &str, provider: &str, disabled: bool| GatewayAccount {
            file_name: file_name.into(),
            provider: provider.into(),
            email: None,
            label: provider.into(),
            disabled,
            active: false,
            active_for_codex: false,
            cooldown_until_ms: None,
            expires_at_ms: None,
            credential_status: "unknown".into(),
            auth: "oauth".into(),
            base_url: None,
        };
        let accounts = vec![
            account("codex-a.json", "codex", false),
            account("xai-b.json", "xai", false),
            account("xai-c.json", "xai", true),
        ];
        let state_path = root.join("controller.json");
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
        let (mutations, _) = selection_transaction(
            &root,
            &auth,
            &state_path,
            &accounts,
            &state,
            ClientSurface::Claude,
            "xai-b.json",
        )
        .unwrap();
        run_transaction(&root, &mutations, || {
            validate_account_invariant(&auth, &state_path)
        })
        .unwrap();
        let codex: Value =
            serde_json::from_str(&fs::read_to_string(auth.join("codex-a.json")).unwrap()).unwrap();
        let grok: Value =
            serde_json::from_str(&fs::read_to_string(auth.join("xai-b.json")).unwrap()).unwrap();
        let grok_other: Value =
            serde_json::from_str(&fs::read_to_string(auth.join("xai-c.json")).unwrap()).unwrap();
        // Same-provider sibling pinned off, selected account enabled, and the
        // other provider's credential left available for the model switcher.
        assert_eq!(grok.get("disabled").and_then(Value::as_bool), Some(false));
        assert_eq!(
            grok_other.get("disabled").and_then(Value::as_bool),
            Some(true)
        );
        assert_eq!(codex.get("disabled").and_then(Value::as_bool), Some(false));
        let selected: ControllerState =
            serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(selected.active_account.as_deref(), Some("xai-b.json"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn controller_state_migration_adds_codex_defaults_without_changing_claude_state() {
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: Some("claude-a.json".into()),
            active_codex_account: None,
            routes: BTreeMap::from([(
                "claude".into(),
                RouteSelection {
                    model: "claude-sonnet-4-5-20250929".into(),
                    thinking: "high".into(),
                },
            )]),
            codex_routes: BTreeMap::new(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        let migrated = migrate_controller_state(state);
        assert_eq!(migrated.active_account.as_deref(), Some("claude-a.json"));
        assert_eq!(
            migrated
                .routes
                .get("claude")
                .map(|route| route.model.as_str()),
            Some("claude-sonnet-4-5-20250929")
        );
        assert_eq!(migrated.codex_routes, default_routes());
    }

    #[test]
    fn codex_selection_changes_only_codex_surface_state() {
        let root = temp_dir("codex-selection-surface");
        let auth = root.join("auth");
        fs::create_dir_all(&auth).unwrap();
        for (file_name, provider) in [("claude-a.json", "claude"), ("xai-b.json", "xai")] {
            fs::write(
                auth.join(file_name),
                serde_json::json!({ "type": provider, "disabled": false }).to_string(),
            )
            .unwrap();
        }
        let accounts = vec![
            GatewayAccount {
                file_name: "claude-a.json".into(),
                provider: "claude".into(),
                email: None,
                label: "Claude".into(),
                disabled: false,
                active: true,
                active_for_codex: false,
                cooldown_until_ms: None,
                expires_at_ms: None,
                credential_status: "unknown".into(),
                auth: "oauth".into(),
                base_url: None,
            },
            GatewayAccount {
                file_name: "xai-b.json".into(),
                provider: "xai".into(),
                email: None,
                label: "Grok".into(),
                disabled: false,
                active: false,
                active_for_codex: false,
                cooldown_until_ms: None,
                expires_at_ms: None,
                credential_status: "unknown".into(),
                auth: "oauth".into(),
                base_url: None,
            },
        ];
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: Some("claude-a.json".into()),
            active_codex_account: None,
            routes: default_routes(),
            codex_routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        let state_path = root.join("controller.json");
        let (_, next) = selection_transaction(
            &root,
            &auth,
            &state_path,
            &accounts,
            &state,
            ClientSurface::Codex,
            "xai-b.json",
        )
        .unwrap();
        assert_eq!(next.active_account.as_deref(), Some("claude-a.json"));
        assert_eq!(next.active_codex_account.as_deref(), Some("xai-b.json"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cross_surface_same_provider_selection_requires_same_account() {
        let root = temp_dir("codex-selection-conflict");
        let auth = root.join("auth");
        fs::create_dir_all(&auth).unwrap();
        for file_name in ["xai-a.json", "xai-b.json"] {
            fs::write(auth.join(file_name), br#"{"type":"xai","disabled":false}"#).unwrap();
        }
        let account = |file_name: &str| GatewayAccount {
            file_name: file_name.into(),
            provider: "xai".into(),
            email: None,
            label: file_name.into(),
            disabled: false,
            active: file_name == "xai-a.json",
            active_for_codex: false,
            cooldown_until_ms: None,
            expires_at_ms: None,
            credential_status: "unknown".into(),
            auth: "oauth".into(),
            base_url: None,
        };
        let accounts = vec![account("xai-a.json"), account("xai-b.json")];
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: Some("xai-a.json".into()),
            active_codex_account: None,
            routes: default_routes(),
            codex_routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        let error = selection_transaction(
            &root,
            &auth,
            &root.join("controller.json"),
            &accounts,
            &state,
            ClientSurface::Codex,
            "xai-b.json",
        )
        .unwrap_err();
        assert!(error.contains("share one enabled xai credential"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn account_selection_rolls_back_every_write_failure() {
        for fail_after in 0..3 {
            let root = temp_dir("account-selection-failure");
            let auth = root.join("auth");
            fs::create_dir_all(&auth).unwrap();
            let codex_path = auth.join("codex-a.json");
            let xai_path = auth.join("xai-b.json");
            let state_path = root.join("controller.json");
            let codex_before = br#"{"type":"codex","disabled":false}"#.to_vec();
            let xai_before = br#"{"type":"xai","disabled":true}"#.to_vec();
            fs::write(&codex_path, &codex_before).unwrap();
            fs::write(&xai_path, &xai_before).unwrap();
            let state = ControllerState {
                api_key: "secret".into(),
                claude_config_id: "id".into(),
                previous_claude_applied_id: None,
                active_account: Some("codex-a.json".into()),
                active_codex_account: None,
                codex_routes: default_routes(),
                routes: default_routes(),
                claude_window_icon: default_claude_window_icon(),
                skip_model_switch_confirmation: false,
                open_claude_on_launch: true,
            };
            let state_before = serde_json::to_vec_pretty(&state).unwrap();
            fs::write(&state_path, &state_before).unwrap();
            let accounts = vec![
                GatewayAccount {
                    file_name: "codex-a.json".into(),
                    provider: "codex".into(),
                    email: None,
                    label: "Codex".into(),
                    disabled: false,
                    active: true,
                    active_for_codex: false,
                    cooldown_until_ms: None,
                    expires_at_ms: None,
                    credential_status: "unknown".into(),
                    auth: "oauth".into(),
                    base_url: None,
                },
                GatewayAccount {
                    file_name: "xai-b.json".into(),
                    provider: "xai".into(),
                    email: None,
                    label: "Grok".into(),
                    disabled: true,
                    active: false,
                    active_for_codex: false,
                    cooldown_until_ms: None,
                    expires_at_ms: None,
                    credential_status: "unknown".into(),
                    auth: "oauth".into(),
                    base_url: None,
                },
            ];
            let (mutations, _) = selection_transaction(
                &root,
                &auth,
                &state_path,
                &accounts,
                &state,
                ClientSurface::Claude,
                "xai-b.json",
            )
            .unwrap();
            assert!(crate::persistence::run_transaction_with_fault(
                &root,
                &mutations,
                || validate_account_invariant(&auth, &state_path),
                fail_after,
                false,
            )
            .is_err());
            assert_eq!(fs::read(&codex_path).unwrap(), codex_before);
            assert_eq!(fs::read(&xai_path).unwrap(), xai_before);
            assert_eq!(fs::read(&state_path).unwrap(), state_before);
            fs::remove_dir_all(root).unwrap();
        }
    }

    fn active_removal_fixture(
        root: &Path,
    ) -> (
        PathBuf,
        PathBuf,
        PathBuf,
        Vec<GatewayAccount>,
        ControllerState,
        BTreeMap<String, String>,
    ) {
        let auth = root.join("auth");
        fs::create_dir_all(&auth).unwrap();
        fs::write(
            auth.join("codex-a.json"),
            br#"{"type":"codex","disabled":false}"#,
        )
        .unwrap();
        fs::write(
            auth.join("xai-b.json"),
            br#"{"type":"xai","disabled":true}"#,
        )
        .unwrap();
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: Some("codex-a.json".into()),
            active_codex_account: Some("codex-a.json".into()),
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        let state_path = root.join("controller.json");
        fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
        let labels = BTreeMap::from([
            ("codex-a.json".into(), "Codex".into()),
            ("xai-b.json".into(), "Grok".into()),
        ]);
        let labels_path = root.join("account-labels.json");
        fs::write(&labels_path, serde_json::to_vec_pretty(&labels).unwrap()).unwrap();
        let accounts = vec![
            GatewayAccount {
                file_name: "codex-a.json".into(),
                provider: "codex".into(),
                email: None,
                label: "Codex".into(),
                disabled: false,
                active: true,
                active_for_codex: true,
                cooldown_until_ms: None,
                expires_at_ms: None,
                credential_status: "unknown".into(),
                auth: "oauth".into(),
                base_url: None,
            },
            GatewayAccount {
                file_name: "xai-b.json".into(),
                provider: "xai".into(),
                email: None,
                label: "Grok".into(),
                disabled: true,
                active: false,
                active_for_codex: false,
                cooldown_until_ms: None,
                expires_at_ms: None,
                credential_status: "unknown".into(),
                auth: "oauth".into(),
                base_url: None,
            },
        ];
        (auth, state_path, labels_path, accounts, state, labels)
    }

    #[test]
    fn active_account_removal_leaves_other_providers_credentials_untouched() {
        // Removing the active account clears the selection but must not disable
        // other providers' credentials Ã¢â‚¬â€ the isolated Codex window still routes
        // picked models to them.
        let root = temp_dir("active-removal");
        let (auth, state_path, labels_path, accounts, state, labels) =
            active_removal_fixture(&root);
        let xai_before = fs::read(auth.join("xai-b.json")).unwrap();
        let (mutations, _) = removal_transaction(
            AccountPaths {
                root: &root,
                directory: &auth,
                state: &state_path,
                labels: &labels_path,
            },
            &accounts,
            &state,
            &labels,
            "codex-a.json",
        )
        .unwrap();
        run_transaction(&root, &mutations, || {
            validate_account_invariant(&auth, &state_path)
        })
        .unwrap();
        assert!(!auth.join("codex-a.json").exists());
        assert!(!crate::persistence::backup_path(&auth.join("codex-a.json"))
            .unwrap()
            .exists());
        // The remaining credential is byte-for-byte unchanged.
        assert_eq!(fs::read(auth.join("xai-b.json")).unwrap(), xai_before);
        let after: ControllerState =
            serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(after.active_account, None);
        let after_labels: BTreeMap<String, String> =
            serde_json::from_slice(&fs::read(&labels_path).unwrap()).unwrap();
        assert!(!after_labels.contains_key("codex-a.json"));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn active_account_removal_rolls_back_every_write_failure() {
        for fail_after in 0..3 {
            let root = temp_dir("active-removal-failure");
            let (auth, state_path, labels_path, accounts, state, labels) =
                active_removal_fixture(&root);
            let before = [
                (
                    auth.join("codex-a.json"),
                    br#"{"type":"codex","disabled":false}"#.to_vec(),
                ),
                (
                    auth.join("xai-b.json"),
                    br#"{"type":"xai","disabled":true}"#.to_vec(),
                ),
                (
                    state_path.clone(),
                    serde_json::to_vec_pretty(&state).unwrap(),
                ),
                (
                    labels_path.clone(),
                    serde_json::to_vec_pretty(&labels).unwrap(),
                ),
            ];
            let (mutations, _) = removal_transaction(
                AccountPaths {
                    root: &root,
                    directory: &auth,
                    state: &state_path,
                    labels: &labels_path,
                },
                &accounts,
                &state,
                &labels,
                "codex-a.json",
            )
            .unwrap();
            // The removed file, the state, and the labels Ã¢â‚¬â€ the remaining
            // credential is untouched by design.
            assert_eq!(mutations.len(), 3);
            assert!(crate::persistence::run_transaction_with_fault(
                &root,
                &mutations,
                || validate_account_invariant(&auth, &state_path),
                fail_after,
                false,
            )
            .is_err());
            for (path, bytes) in before {
                assert_eq!(fs::read(path).unwrap(), bytes);
            }
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn login_url_parser_accepts_only_expected_https_hosts() {
        assert_eq!(
            extract_login_url(
                "codex",
                "https://auth.openai.com/oauth/authorize?state=test&code_challenge=abc"
            )
            .as_deref(),
            Some("https://auth.openai.com/oauth/authorize?state=test&code_challenge=abc")
        );
        assert_eq!(
            extract_login_url(
                "claude",
                "Visit https://claude.ai/oauth/authorize?state=test"
            )
            .as_deref(),
            Some("https://claude.ai/oauth/authorize?state=test")
        );
        assert_eq!(
            extract_login_url(
                "xai",
                "https://accounts.x.ai/oauth2/device?user_code=ABCD-1234"
            )
            .as_deref(),
            Some("https://accounts.x.ai/oauth2/device?user_code=ABCD-1234")
        );
        assert_eq!(
            extract_login_url(
                "kimi",
                "Verification URL: https://auth.kimi.com/oauth/device?user_code=KIMI-1234"
            )
            .as_deref(),
            Some("https://auth.kimi.com/oauth/device?user_code=KIMI-1234")
        );
        assert_eq!(
            extract_login_url(
                "kimi",
                "Verification URL: https://www.kimi.com/code/device?user_code=KIMI-1234"
            )
            .as_deref(),
            Some("https://www.kimi.com/code/device?user_code=KIMI-1234")
        );
        assert_eq!(
            extract_login_url(
                "antigravity",
                "Visit the following URL to continue authentication:\nhttps://accounts.google.com/o/oauth2/v2/auth?client_id=test"
            )
            .as_deref(),
            Some("https://accounts.google.com/o/oauth2/v2/auth?client_id=test")
        );
        assert_eq!(
            extract_login_url("zai", "https://chat.z.ai/auth?flow=1").as_deref(),
            Some("https://chat.z.ai/auth?flow=1")
        );
        assert!(extract_login_url("codex", "about:blank").is_none());
        assert!(extract_login_url(
            "codex",
            "https://auth.openai.com.evil.example/oauth/authorize"
        )
        .is_none());
        assert!(extract_login_url("codex", "http://auth.openai.com/oauth/authorize").is_none());
        assert!(extract_login_url("kimi", "https://auth.kimi.com.evil.example/oauth").is_none());
        assert!(extract_login_url("kimi", "https://www.kimi.com.evil.example/oauth").is_none());
        assert!(extract_login_url(
            "antigravity",
            "https://accounts.google.com.evil.example/auth"
        )
        .is_none());
        assert!(extract_login_url("zai", "https://chat.z.ai.evil.example/auth").is_none());
    }

    #[test]
    fn xai_device_code_parser_preserves_the_one_time_code() {
        assert_eq!(
            extract_xai_user_code("Then enter this code: ABCD-1234").as_deref(),
            Some("ABCD-1234")
        );
        assert!(extract_xai_user_code("Waiting for authorization...").is_none());
    }

    #[test]
    fn kimi_device_login_waits_for_its_one_time_code() {
        let authorization_url =
            Some("https://auth.kimi.com/oauth/device?user_code=KIMI-1234".into());
        assert_eq!(
            extract_kimi_user_code("User code: KIMI-1234").as_deref(),
            Some("KIMI-1234")
        );
        assert!(!login_authorization_ready(
            "kimi",
            &authorization_url,
            &None,
            "Waiting for authorization..."
        ));
        assert!(login_authorization_ready(
            "kimi",
            &authorization_url,
            &Some("KIMI-1234".into()),
            "Waiting for authorization..."
        ));
        assert!(!login_authorization_ready(
            "kimi",
            &authorization_url,
            &Some("KIMI-1234".into()),
            "User code: KIMI-1234"
        ));
    }

    #[test]
    fn login_stderr_failures_are_classified_without_preserving_provider_output() {
        assert_eq!(
            login_stderr_failure_reason(
                "kimi",
                "time=... level=error msg=\"Kimi authentication failed: kimi: device code request failed with status 403: response body\""
            )
            .as_deref(),
            Some("Kimi Code rejected the device authorization request. Check your connection and try again.")
        );
        assert_eq!(
            login_stderr_failure_reason(
                "kimi",
                "time=... level=error msg=\"Kimi authentication failed: kimi: device code request failed: connection timed out\""
            )
            .as_deref(),
            Some("Kimi Code could not be reached for device authorization. Check your connection and try again.")
        );
        assert!(login_stderr_failure_reason("kimi", "User code: KIMI-1234").is_none());
    }

    #[test]
    fn xai_native_web_search_is_removed_before_cliproxy_injects_x_search() {
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        let mut request = serde_json::json!({
            "model": "claude-opus-4-99",
            "tools": [
                {
                    "type": "web_search_20250305",
                    "name": "web_search",
                    "max_uses": 5
                }
            ],
            "tool_choice": {"type": "tool", "name": "web_search"}
        });
        rewrite_claude_request(&mut request, &state, "xai", true).unwrap();
        assert_eq!(request["tools"][0]["type"].as_str(), Some("x_search"));
        assert_eq!(request["tools"][0]["name"].as_str(), Some("x_search"));
        assert_eq!(request["tool_choice"]["name"].as_str(), Some("x_search"));
        let identity = request["system"][0]["text"].as_str().unwrap();
        assert!(identity.contains("Grok Build"));
    }

    #[test]
    fn xai_native_web_search_removal_keeps_other_tools() {
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        let mut request = serde_json::json!({
            "model": "claude-opus-4-99",
            "tools": [
                {"type": "web_search", "name": "web_search"},
                {"type": "function", "name": "some_other_tool", "parameters": {"type": "object"}}
            ],
            "tool_choice": {"type": "web_search"}
        });
        rewrite_claude_request(&mut request, &state, "xai", true).unwrap();
        let tools = request.get("tools").and_then(Value::as_array).unwrap();
        assert_eq!(tools.len(), 2);
        assert_eq!(tools[0]["name"].as_str(), Some("x_search"));
        assert_eq!(tools[1]["name"].as_str(), Some("some_other_tool"));
        assert_eq!(request["tool_choice"]["name"].as_str(), Some("x_search"));
    }

    #[test]
    fn non_xai_provider_keeps_tools_unchanged() {
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        let mut request = serde_json::json!({
            "model": "claude-opus-4-99",
            "tools": [
                {"type": "x_search"},
                {
                    "type": "function",
                    "function": {
                        "name": "web_search",
                        "description": "Search",
                        "parameters": {"type": "object"}
                    }
                }
            ]
        });
        rewrite_claude_request(&mut request, &state, "kimi", true).unwrap();
        let tools = request.get("tools").and_then(Value::as_array).unwrap();
        assert_eq!(tools.len(), 2);
    }

    #[test]
    fn kimi_flattens_deferred_tool_reference_blocks_that_cliproxyapi_rejects() {
        // CLIProxyAPI issue #4405 minimal repro: a tool_result whose nested
        // content contains an Anthropic deferred-tool `tool_reference` block
        // gets a 400 from Kimi's /v1/messages path. The issue's own suggested
        // fix is exactly this: replace it with a plain text block.
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        let mut request = serde_json::json!({
            "model": "kimi-k3",
            "max_tokens": 256,
            "messages": [
                {"role": "user", "content": "find tool"},
                {
                    "role": "assistant",
                    "content": [
                        {"type": "tool_use", "id": "tu_1", "name": "ToolSearch", "input": {"query": "x"}}
                    ]
                },
                {
                    "role": "user",
                    "content": [
                        {
                            "type": "tool_result",
                            "tool_use_id": "tu_1",
                            "content": [
                                {"type": "tool_reference", "tool_name": "SendMessage"}
                            ]
                        },
                        {"type": "text", "text": "ok now say hi"}
                    ]
                }
            ]
        });
        rewrite_claude_request(&mut request, &state, "kimi", false).unwrap();
        let tool_result_content = &request["messages"][2]["content"][0]["content"];
        assert_eq!(
            tool_result_content[0],
            serde_json::json!({"type": "text", "text": "SendMessage"})
        );
        // Unrelated blocks (the tool_use, the sibling text block) are untouched.
        assert_eq!(request["messages"][1]["content"][0]["type"], "tool_use");
        assert_eq!(request["messages"][2]["content"][1]["type"], "text");
        assert_eq!(
            request["messages"][2]["content"][1]["text"],
            "ok now say hi"
        );
    }

    #[test]
    fn old_controller_state_migrates_to_known_good_routes() {
        let state: ControllerState = serde_json::from_str(
            r#"{"api_key":"secret","claude_config_id":"id","active_account":null}"#,
        )
        .unwrap();
        assert_eq!(
            normalized_route(&state, "claude").model,
            "claude-sonnet-4-5-20250929"
        );
        assert_eq!(normalized_route(&state, "codex").model, "gpt-5.6-terra");
        assert_eq!(normalized_route(&state, "xai").model, "grok-4.5");
        assert_eq!(normalized_route(&state, "xai").thinking, "auto");
        assert_eq!(normalized_route(&state, "kimi").model, "kimi-k3");
        assert_eq!(normalized_route(&state, "kimi").thinking, "auto");
        assert_eq!(state.claude_window_icon, ClaudeWindowIcon::Black);
    }

    #[test]
    fn old_controller_state_defaults_claude_window_icon_to_black() {
        let state: ControllerState = serde_json::from_str(
            r#"{"api_key":"secret","claude_config_id":"id","active_account":null}"#,
        )
        .unwrap();
        assert_eq!(state.claude_window_icon, ClaudeWindowIcon::Black);
        assert!(should_apply_claude_window_icon(state.claude_window_icon));
    }

    #[test]
    fn claude_window_icon_round_trips_in_controller_state() {
        for (raw, expected) in [
            ("black", ClaudeWindowIcon::Black),
            ("system", ClaudeWindowIcon::System),
        ] {
            let json = format!(
                r#"{{"api_key":"secret","claude_config_id":"id","active_account":null,"claude_window_icon":"{raw}"}}"#
            );
            let state: ControllerState = serde_json::from_str(&json).unwrap();
            assert_eq!(state.claude_window_icon, expected);
            let encoded = serde_json::to_value(&state).unwrap();
            assert_eq!(
                encoded.get("claude_window_icon").and_then(|v| v.as_str()),
                Some(raw)
            );
        }
        assert!(!should_apply_claude_window_icon(ClaudeWindowIcon::System));
    }

    #[test]
    fn bundled_claude_icon_assets_exist_in_dev_tree() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("resources/icons");
        assert!(
            root.join("claude-window-black.ico").is_file(),
            "missing claude-window-black.ico"
        );
        assert!(
            root.join("claude-tray-inverted.ico").is_file(),
            "missing claude-tray-inverted.ico"
        );
        assert!(
            root.join("codex-window-black.ico").is_file(),
            "missing codex-window-black.ico"
        );
    }

    #[test]
    fn selected_model_and_thinking_are_encoded_in_the_proxied_request() {
        let auth = temp_dir("selected-route");
        auth_file(&auth, "xai-test.json", "xai");
        let mut state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: Some("xai-test.json".into()),
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        state.routes.insert(
            "xai".into(),
            RouteSelection {
                model: "grok-4.5".into(),
                thinking: "high".into(),
            },
        );
        let mut request = serde_json::json!({"model": "claude-opus-4-99"});
        rewrite_claude_request(&mut request, &state, "xai", true).unwrap();
        assert_eq!(
            request.get("model").and_then(Value::as_str),
            Some("grok-4.5(high)")
        );
        let identity = request["system"][0]["text"].as_str().unwrap();
        assert!(identity.contains("current upstream route is Grok 4.5 via Grok Build"));
        assert!(identity.contains("actual route: 'Grok 4.5 via Grok Build'"));
        assert_eq!(route_label(&state, Some("xai")), "Grok 4.5");
        let _ = fs::remove_dir_all(auth);
    }

    #[test]
    fn kimi_route_uses_the_selected_model_and_truthful_identity() {
        let mut state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        state.routes.insert(
            "kimi".into(),
            RouteSelection {
                model: "kimi-k2.7-code".into(),
                thinking: "high".into(),
            },
        );
        // The generic default alias is not a picker alias for kimi (kimi uses
        // pool indexes 0-6, not claude-fable-5), so the request falls back to
        // the Basiliskos route selection.
        let mut request = serde_json::json!({"model": "claude-opus-4-99"});
        rewrite_claude_request(&mut request, &state, "kimi", true).unwrap();
        assert_eq!(
            request.get("model").and_then(Value::as_str),
            Some("kimi-k2.7-code(high)")
        );
        let identity = request["system"][0]["text"].as_str().unwrap();
        assert!(identity.contains("current upstream route is Kimi K2.7 Code via Kimi Code"));
        assert_eq!(route_label(&state, Some("kimi")), "Kimi K2.7 Code");
    }

    #[test]
    fn kimi_k3_is_default_and_routes_with_max_thinking() {
        let mut state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        assert_eq!(normalized_route(&state, "kimi").model, "kimi-k3");
        assert_eq!(route_label(&state, Some("kimi")), "Kimi K3");
        state.routes.insert(
            "kimi".into(),
            RouteSelection {
                model: "kimi-k3".into(),
                thinking: "max".into(),
            },
        );
        let mut request = serde_json::json!({"model": "claude-opus-4-99"});
        rewrite_claude_request(&mut request, &state, "kimi", true).unwrap();
        assert_eq!(
            request.get("model").and_then(Value::as_str),
            Some("kimi-k3(max)")
        );
        let identity = request["system"][0]["text"].as_str().unwrap();
        assert!(identity.contains("current upstream route is Kimi K3 via Kimi Code"));
        assert_eq!(route_label(&state, Some("kimi")), "Kimi K3");
    }

    #[test]
    fn relay_faults_have_stable_upstream_classifications() {
        assert_eq!(request_surface("/v1/messages"), Some(ClientSurface::Claude));
        assert_eq!(
            request_surface("/v1/messages/count_tokens"),
            Some(ClientSurface::Claude)
        );
        assert_eq!(request_surface("/v1/responses"), Some(ClientSurface::Codex));
        assert_eq!(
            request_surface("/v1/responses/compact"),
            Some(ClientSurface::Codex)
        );
        assert_eq!(
            request_surface("/v1/chat/completions"),
            Some(ClientSurface::Codex)
        );
        assert_eq!(
            classify_upstream_status(401),
            Some(ErrorCode::ProviderAuthFailed)
        );
        assert_eq!(
            classify_upstream_status(429),
            Some(ErrorCode::ProviderRateLimited)
        );
        assert_eq!(
            classify_upstream_status(503),
            Some(ErrorCode::UpstreamServerError)
        );
        assert_eq!(classify_upstream_status(200), None);
        assert_eq!(
            classify_upstream_status(402),
            Some(ErrorCode::ProviderQuotaExhausted)
        );
        assert_eq!(
            classify_upstream_status(403),
            Some(ErrorCode::ProviderAuthFailed)
        );
        assert_eq!(
            provider_auth_failure_message(Some("kimi"), 402),
            "This Kimi account has no active Kimi Code subscription."
        );
        assert_eq!(
            provider_auth_failure_message(Some("kimi"), 403),
            "This Kimi account has no active Kimi Code subscription."
        );
        assert_eq!(
            provider_auth_failure_message(Some("codex"), 403),
            "The provider rejected the selected credential."
        );
    }

    #[test]
    fn retry_after_parses_delay_seconds_and_http_date_and_rejects_junk() {
        assert_eq!(parse_retry_after_seconds("120"), Some(120));
        assert_eq!(parse_retry_after_seconds("  45  "), Some(45));
        assert_eq!(parse_retry_after_seconds("0"), None);
        assert_eq!(parse_retry_after_seconds("-5"), None);
        assert_eq!(parse_retry_after_seconds("not-a-number"), None);
        assert_eq!(parse_retry_after_seconds(""), None);

        let future = Utc::now() + chrono::Duration::seconds(90);
        let http_date = future.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        let parsed = parse_retry_after_seconds(&http_date).expect("http-date should parse");
        assert!((85..=90).contains(&parsed), "expected ~90s, got {parsed}");

        let past = Utc::now() - chrono::Duration::seconds(30);
        let past_http_date = past.format("%a, %d %b %Y %H:%M:%S GMT").to_string();
        assert_eq!(parse_retry_after_seconds(&past_http_date), None);
    }

    #[test]
    fn visible_models_respect_hidden_list_and_live_catalog_but_never_drop_the_selected_model() {
        const SPECS: &[ModelSpec] = &[
            ModelSpec {
                id: "model-a",
                label: "Model A",
                thinking_levels: &[],
            },
            ModelSpec {
                id: "model-b",
                label: "Model B",
                thinking_levels: &[],
            },
            ModelSpec {
                id: "model-c",
                label: "Model C",
                thinking_levels: &[],
            },
        ];

        // No hidden list, no live data yet: everything shows.
        let none_hidden = BTreeSet::new();
        let ids = |visible: Vec<&ModelSpec>| visible.iter().map(|spec| spec.id).collect::<Vec<_>>();
        assert_eq!(
            ids(filter_visible_models(
                "provider",
                SPECS,
                "model-a",
                &none_hidden,
                None
            )),
            vec!["model-a", "model-b", "model-c"]
        );

        // A manually-hidden model disappears, unless it's the current selection.
        let hidden_b = BTreeSet::from(["model-b".to_string()]);
        assert_eq!(
            ids(filter_visible_models(
                "provider", SPECS, "model-a", &hidden_b, None
            )),
            vec!["model-a", "model-c"]
        );
        assert_eq!(
            ids(filter_visible_models(
                "provider", SPECS, "model-b", &hidden_b, None
            )),
            vec!["model-a", "model-b", "model-c"]
        );

        // A live catalog that omits a model hides it too, unless it's selected.
        let live = vec!["model-a".to_string(), "model-c".to_string()];
        assert_eq!(
            ids(filter_visible_models(
                "provider",
                SPECS,
                "model-a",
                &none_hidden,
                Some(&live)
            )),
            vec!["model-a", "model-c"]
        );
        assert_eq!(
            ids(filter_visible_models(
                "provider",
                SPECS,
                "model-b",
                &none_hidden,
                Some(&live)
            )),
            vec!["model-a", "model-b", "model-c"]
        );

        // Both filters combine.
        assert_eq!(
            ids(filter_visible_models(
                "provider",
                SPECS,
                "model-a",
                &hidden_b,
                Some(&live)
            )),
            vec!["model-a", "model-c"]
        );
    }

    #[test]
    fn failover_picks_a_same_provider_account_that_is_not_cooling_and_skips_others() {
        fn account(file_name: &str, provider: &str) -> GatewayAccount {
            GatewayAccount {
                file_name: file_name.into(),
                provider: provider.into(),
                email: None,
                label: file_name.into(),
                disabled: true,
                active: false,
                active_for_codex: false,
                cooldown_until_ms: None,
                expires_at_ms: None,
                credential_status: "unknown".into(),
                auth: "oauth".into(),
                base_url: None,
            }
        }
        let accounts = vec![
            account("codex-a.json", "codex"),
            account("codex-b.json", "codex"),
            account("codex-c.json", "codex"),
            account("xai-only.json", "xai"),
        ];
        let now = Utc::now();

        // No cooldowns recorded: first other same-provider account wins.
        let none_cooling = BTreeMap::new();
        assert_eq!(
            pick_failover_candidate(&accounts, "codex-a.json", "codex", &none_cooling, now)
                .map(|account| account.file_name.as_str()),
            Some("codex-b.json")
        );

        // codex-b is cooling: falls through to codex-c.
        let mut cooling = BTreeMap::new();
        cooling.insert(
            "codex-b.json".to_string(),
            now + chrono::Duration::seconds(30),
        );
        assert_eq!(
            pick_failover_candidate(&accounts, "codex-a.json", "codex", &cooling, now)
                .map(|account| account.file_name.as_str()),
            Some("codex-c.json")
        );

        // An expired cooldown entry no longer excludes the account.
        let mut expired = BTreeMap::new();
        expired.insert(
            "codex-b.json".to_string(),
            now - chrono::Duration::seconds(1),
        );
        assert_eq!(
            pick_failover_candidate(&accounts, "codex-a.json", "codex", &expired, now)
                .map(|account| account.file_name.as_str()),
            Some("codex-b.json")
        );

        // Never crosses providers: with no other codex account, the xai
        // account is not picked as a substitute even though it's available.
        let single_provider_accounts = vec![
            account("codex-a.json", "codex"),
            account("xai-only.json", "xai"),
        ];
        assert!(pick_failover_candidate(
            &single_provider_accounts,
            "codex-a.json",
            "codex",
            &none_cooling,
            now
        )
        .is_none());

        // All same-provider candidates cooling: no failover.
        let mut all_cooling = BTreeMap::new();
        all_cooling.insert(
            "codex-b.json".to_string(),
            now + chrono::Duration::seconds(30),
        );
        all_cooling.insert(
            "codex-c.json".to_string(),
            now + chrono::Duration::seconds(30),
        );
        // Expired or relogin_required candidates are skipped.
        let mut expired_b = accounts.clone();
        expired_b[1].credential_status = "expired".into();
        assert_eq!(
            pick_failover_candidate(&expired_b, "codex-a.json", "codex", &none_cooling, now)
                .map(|account| account.file_name.as_str()),
            Some("codex-c.json")
        );
        let mut relogin_b = accounts.clone();
        relogin_b[1].credential_status = "relogin_required".into();
        assert_eq!(
            pick_failover_candidate(&relogin_b, "codex-a.json", "codex", &none_cooling, now)
                .map(|account| account.file_name.as_str()),
            Some("codex-c.json")
        );
    }

    #[test]
    fn antigravity_model_mapping_keeps_models_visible_against_live_backend() {
        let none_hidden = BTreeSet::new();
        let live = vec![
            "gemini-3.6-flash-high".to_string(),
            "gemini-3.1-pro-low".to_string(),
        ];
        let visible = filter_visible_models(
            "antigravity",
            crate::catalog::ANTIGRAVITY_MODELS,
            "gemini-3.7-flash",
            &none_hidden,
            Some(&live),
        );
        let ids: Vec<&str> = visible.iter().map(|s| s.id).collect();
        assert!(ids.contains(&"gemini-3.7-flash"));
        assert!(ids.contains(&"gemini-3.7-pro"));
        assert!(!ids.contains(&"gemini-3.7-flash-lite"));
    }

    fn begin_mock_request(
        runtime: &tokio::runtime::Handle,
        scenario: crate::test_support::FaultScenario,
        first_response_timeout: Duration,
        stream_idle_timeout: Duration,
    ) -> (
        crate::test_support::MockBackend,
        Result<UpstreamMeta, FirstResponseFailure>,
    ) {
        // Some Windows endpoint filters intermittently abort a brand-new
        // loopback GET before response headers. Retrying only the disposable
        // test fixture keeps the fault harness deterministic; production
        // requests use begin_upstream_request directly and are never replayed.
        for _ in 0..3 {
            let backend = crate::test_support::MockBackend::spawn(scenario).unwrap();
            let result = begin_upstream_request_with_timeouts(
                runtime,
                reqwest::Client::builder().no_proxy().build().unwrap(),
                reqwest::Method::GET,
                format!("http://{}/fault", backend.address()),
                Vec::new(),
                Vec::new(),
                first_response_timeout,
                stream_idle_timeout,
            );
            if matches!(result, Err(FirstResponseFailure::Connect)) {
                continue;
            }
            return (backend, result);
        }
        panic!("the loopback fault fixture was aborted three consecutive times")
    }

    #[test]
    fn relay_long_sse_stream_survives_while_each_chunk_meets_idle_budget() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (_backend, meta) = begin_mock_request(
            runtime.handle(),
            crate::test_support::FaultScenario::DelayedSseChunk(Duration::from_millis(120)),
            Duration::from_millis(500),
            Duration::from_millis(500),
        );
        let meta = meta.unwrap();
        let mut reader = TrackedUpstream {
            receiver: meta.body,
            current: None,
            offset: 0,
            correlation_id: "sse-test".into(),
            provider: None,
        };
        let mut body = String::new();
        reader.read_to_string(&mut body).unwrap();
        assert!(body.contains("data: first"));
        assert!(body.contains("data: second"));
    }

    #[test]
    fn relay_distinguishes_first_response_and_midstream_idle_timeouts() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (_first, result) = begin_mock_request(
            runtime.handle(),
            crate::test_support::FaultScenario::DelayedFirstByte(Duration::from_millis(120)),
            Duration::from_millis(20),
            Duration::from_millis(500),
        );
        assert!(matches!(result, Err(FirstResponseFailure::Timeout)));

        let (_stream, meta) = begin_mock_request(
            runtime.handle(),
            crate::test_support::FaultScenario::DelayedSseChunk(Duration::from_millis(120)),
            Duration::from_millis(500),
            Duration::from_millis(20),
        );
        let meta = meta.unwrap();
        let mut reader = TrackedUpstream {
            receiver: meta.body,
            current: None,
            offset: 0,
            correlation_id: "idle-test".into(),
            provider: None,
        };
        let mut body = String::new();
        let error = reader.read_to_string(&mut body).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(body.contains("data: first"));
    }

    #[test]
    fn relay_budgets_are_fixed_and_bounded() {
        assert_eq!(RELAY_WORKERS, 8);
        assert_eq!(RELAY_QUEUE_CAPACITY, 32);
        assert_eq!(MAX_RELAY_BODY_BYTES, 8 * 1024 * 1024);
        assert_eq!(MAX_RELAY_HEADERS, 64);
        assert_eq!(MAX_RELAY_HEADER_BYTES, 64 * 1024);
    }

    #[test]
    fn kimi_usage_handles_missing_or_invalid_limits() {
        assert!(parse_kimi_usage(&serde_json::json!({
            "usage": {"limit": 0, "used": 1},
            "limits": [{"detail": {"remaining": 1}}]
        }))
        .is_empty());
        assert_eq!(
            parse_kimi_usage(&serde_json::json!({
                "limits": [{"window": {"duration": 300, "timeUnit": "MINUTE"}, "detail": {"limit": 100, "used": 10}}]
            })),
            vec![usage_window("5h", 10.0)]
        );
    }

    #[test]
    fn kimi_usage_errors_explain_missing_subscription() {
        assert_eq!(
            usage_http_error_message("kimi", reqwest::StatusCode::from_u16(402).unwrap()),
            "No active Kimi Code subscription"
        );
        assert_eq!(
            usage_http_error_message("kimi", reqwest::StatusCode::FORBIDDEN),
            "No active Kimi Code subscription"
        );
        assert_eq!(
            usage_http_error_message("kimi", reqwest::StatusCode::UNAUTHORIZED),
            "Usage check unavailable Ã¢â‚¬â€ saved login is active. Auto-retry in 5 min or use Refresh usage."
        );
        assert_eq!(
            usage_http_error_message("codex", reqwest::StatusCode::FORBIDDEN),
            "Usage check unavailable Ã¢â‚¬â€ saved login is active. Auto-retry in 5 min or use Refresh usage."
        );
        assert_eq!(
            usage_http_error_message("xai", reqwest::StatusCode::from_u16(500).unwrap()),
            "Usage service returned 500. Auto-retry in 5 min or use Refresh usage."
        );
    }

    #[test]
    fn invalid_or_unsupported_route_values_fall_back_safely() {
        let mut state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        state.routes.insert(
            "xai".into(),
            RouteSelection {
                model: "grok-composer-2.5-fast".into(),
                thinking: "high".into(),
            },
        );
        assert_eq!(normalized_route(&state, "xai").thinking, "auto");
        state.routes.insert(
            "codex".into(),
            RouteSelection {
                model: "made-up-model".into(),
                thinking: "none".into(),
            },
        );
        assert_eq!(normalized_route(&state, "codex").model, "gpt-5.6-terra");
        assert_eq!(normalized_route(&state, "codex").thinking, "auto");
    }

    #[test]
    fn provider_login_identity_is_normalized_and_has_a_safe_fallback() {
        let credential = serde_json::json!({"account": {"email": "  USER@Example.COM "}});
        assert_eq!(
            credential_identity(&credential, "Codex-Fallback.JSON"),
            "user@example.com"
        );
        assert_eq!(
            credential_identity(&serde_json::json!({}), "Codex-Fallback.JSON"),
            "codex-fallback.json"
        );
    }

    #[test]
    fn skip_model_switch_confirmation_defaults_to_false_and_round_trips() {
        let state: ControllerState = serde_json::from_str(
            r#"{"api_key":"secret","claude_config_id":"id","active_account":null}"#,
        )
        .unwrap();
        assert!(!state.skip_model_switch_confirmation);
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: true,
            open_claude_on_launch: default_open_claude_on_launch(),
        };
        let encoded = serde_json::to_value(&state).unwrap();
        assert_eq!(
            encoded
                .get("skip_model_switch_confirmation")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        let decoded: ControllerState = serde_json::from_value(encoded).unwrap();
        assert!(decoded.skip_model_switch_confirmation);
    }

    #[test]
    fn xai_refresh_is_due_only_inside_the_safety_window() {
        let now = Utc::now();
        let due = serde_json::json!({
            "expired": (now + ChronoDuration::seconds(XAI_REFRESH_SKEW_SECS - 1)).to_rfc3339()
        });
        let fresh = serde_json::json!({
            "expired": (now + ChronoDuration::seconds(XAI_REFRESH_SKEW_SECS + 60)).to_rfc3339()
        });
        assert!(xai_refresh_required(&due, now));
        assert!(!xai_refresh_required(&fresh, now));
        assert!(xai_refresh_required(&serde_json::json!({}), now));
    }

    #[test]
    fn xai_refresh_endpoint_accepts_only_trusted_https_hosts() {
        assert!(xai_refresh_endpoint(&serde_json::json!({
            "token_endpoint": "https://auth.x.ai/oauth2/token"
        }))
        .is_ok());
        assert!(xai_refresh_endpoint(&serde_json::json!({
            "token_endpoint": "http://auth.x.ai/oauth2/token"
        }))
        .is_err());
        assert!(xai_refresh_endpoint(&serde_json::json!({
            "token_endpoint": "https://auth.x.ai.attacker.invalid/oauth2/token"
        }))
        .is_err());
    }

    #[test]
    fn xai_refresh_relogin_is_only_required_for_terminal_grant_errors() {
        assert!(xai_refresh_error_requires_relogin(Some("invalid_grant")));
        assert!(xai_refresh_error_requires_relogin(Some(
            "refresh_token_invalidated"
        )));
        assert!(!xai_refresh_error_requires_relogin(Some(
            "temporarily_unavailable"
        )));
        assert!(!xai_refresh_error_requires_relogin(None));
    }

    #[test]
    fn credential_expiry_status_is_truthful_about_renewal_and_relogin() {
        let now = Utc::now();
        let credential = serde_json::json!({
            "expired": (now + ChronoDuration::minutes(15)).to_rfc3339(),
        });
        assert!(credential_expiry(&credential).is_some());
        assert_eq!(
            credential_status(
                "codex",
                "codex-test.json",
                credential_expiry(&credential),
                now
            ),
            "active"
        );
        assert_eq!(
            credential_status(
                "codex",
                "codex-test.json",
                Some(now - ChronoDuration::seconds(1)),
                now
            ),
            "renewal_due"
        );
        assert_eq!(
            credential_status("codex", "codex-test.json", None, now),
            "unknown"
        );
    }

    #[test]
    fn kimi_refresh_relogin_is_only_required_for_rejected_grants() {
        assert!(kimi_refresh_error_requires_relogin(
            reqwest::StatusCode::UNAUTHORIZED,
            None
        ));
        assert!(kimi_refresh_error_requires_relogin(
            reqwest::StatusCode::BAD_REQUEST,
            Some("invalid_grant")
        ));
        assert!(!kimi_refresh_error_requires_relogin(
            reqwest::StatusCode::BAD_REQUEST,
            Some("temporarily_unavailable")
        ));
        assert!(!kimi_refresh_error_requires_relogin(
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            None
        ));
    }

    #[test]
    fn client_picker_choice_honors_visible_catalog_models_only() {
        let hidden = BTreeSet::new();
        // Claude sends the Anthropic routing alias; the proxy maps it back to
        // the real upstream model with its thinking level.
        let request = serde_json::json!({ "model": "claude-opus-4-5" });
        assert_eq!(
            client_picker_choice(request.as_object().unwrap(), "kimi", &hidden, "kimi-k3"),
            Some(("kimi-k2.7-code".to_string(), "auto".to_string()))
        );
        // Unknown aliases fall back to the Basiliskos route.
        let unknown = serde_json::json!({ "model": "claude-sonnet-9-9" });
        assert_eq!(
            client_picker_choice(unknown.as_object().unwrap(), "kimi", &hidden, "kimi-k3"),
            None
        );
        // A model the user hid is not honored even when requested by alias.
        let hidden_set = BTreeSet::from(["kimi-k2.7-code".to_string()]);
        assert_eq!(
            client_picker_choice(request.as_object().unwrap(), "kimi", &hidden_set, "kimi-k3"),
            None
        );
    }

    #[test]
    fn picker_aliases_resolve_and_variants_carry_thinking() {
        use std::collections::BTreeSet;
        let hidden = BTreeSet::new();
        for provider in SUPPORTED_PROVIDERS {
            let mut aliases = BTreeSet::new();
            let selected = default_model(provider);
            let entries = picker_entries(provider, &hidden, selected);
            assert!(!entries.is_empty(), "{provider} picker is empty");
            assert_eq!(entries[0].2, selected, "selected model is first");
            for (alias, _, model, thinking) in &entries {
                assert!(
                    aliases.insert(alias.as_str()),
                    "{provider} alias collision: {alias}"
                );
                let resolved = alias_to_picker_entry(provider, alias, selected);
                assert!(resolved.is_some(), "{provider} alias {alias} unresolved");
                if thinking == "auto" {
                    assert_eq!(resolved, Some((model.clone(), "auto".into())));
                } else {
                    // Variant entries resolve to the selected model with the level.
                    assert_eq!(resolved, Some((selected.to_string(), thinking.clone())));
                }
            }
        }
    }

    #[test]
    fn effort_to_thinking_validates_against_model_levels() {
        // Grok 4.5 remaps desktop effort to its low/high pair.
        assert_eq!(effort_to_thinking("xai", "grok-4.5", "max"), "high");
        assert_eq!(effort_to_thinking("xai", "grok-4.5", "medium"), "low");
        // Auto passes through.
        assert_eq!(effort_to_thinking("kimi", "kimi-k3", "auto"), "auto");
    }

    #[test]
    fn codex_config_model_parses_the_picker_choice() {
        let parsed = parse_codex_config_toml(
            r#"# Generated by Basiliskos for the isolated Codex client only.
model = "grok-4.6"
model_reasoning_effort = "xhigh"
model_reasoning_summary = "detailed"
model_auto_compact_token_limit = 9000000000000
openai_base_url = "http://127.0.0.1:8317/v1"
"#,
        )
        .expect("the picker choice parses");
        assert_eq!(parsed, ("grok-4.6".to_string(), "xhigh".to_string()));
        // The `model_catalog_json` / `model_reasoning_*` lines must not be
        // mistaken for the model itself, and a missing effort defaults to auto.
        let bare = parse_codex_config_toml(
            "model = \"kimi-k2.7-code\"\nmodel_reasoning_summary = \"detailed\"\n",
        )
        .expect("bare choice parses");
        assert_eq!(bare, ("kimi-k2.7-code".to_string(), "auto".to_string()));
        assert!(parse_codex_config_toml("openai_base_url = \"x\"\n").is_none());
    }

    #[test]
    fn model_to_provider_maps_catalog_ids_only() {
        assert_eq!(model_to_provider("grok-4.6"), Some("xai"));
        assert_eq!(model_to_provider("gpt-5.6-terra"), Some("codex"));
        assert_eq!(model_to_provider("kimi-k3"), Some("kimi"));
        assert_eq!(
            model_to_provider("claude-sonnet-4-5-20250929"),
            Some("claude")
        );
        // A model Basiliskos does not advertise maps to nothing.
        assert_eq!(model_to_provider("gpt-4o"), None);
    }

    #[test]
    fn pick_codex_sync_account_prefers_the_enabled_pin_then_valid() {
        let account =
            |file_name: &str, provider: &str, disabled: bool, status: &str| GatewayAccount {
                file_name: file_name.into(),
                provider: provider.into(),
                email: None,
                label: provider.into(),
                disabled,
                active: false,
                active_for_codex: false,
                cooldown_until_ms: None,
                expires_at_ms: None,
                credential_status: status.into(),
                auth: "oauth".into(),
                base_url: None,
            };
        let accounts = vec![
            account("xai-a.json", "xai", true, "expired"),
            account("xai-b.json", "xai", false, "active"),
            account("xai-c.json", "xai", true, "active"),
        ];
        assert_eq!(
            pick_codex_sync_account(&accounts, "xai").map(|a| a.file_name.as_str()),
            Some("xai-b.json")
        );
        // No enabled account: prefer a non-expired one over an expired one.
        let no_pin = vec![
            account("xai-a.json", "xai", true, "expired"),
            account("xai-c.json", "xai", true, "active"),
        ];
        assert_eq!(
            pick_codex_sync_account(&no_pin, "xai").map(|a| a.file_name.as_str()),
            Some("xai-c.json")
        );
        // Unknown provider Ã¢â€ â€™ nothing.
        assert!(pick_codex_sync_account(&accounts, "kimi").is_none());
    }

    #[test]
    fn client_effort_choice_reads_output_config_and_top_level() {
        let request = serde_json::json!({ "output_config": { "effort": "high" } });
        assert_eq!(
            client_effort_choice(request.as_object().unwrap()),
            Some("high".to_string())
        );
        let top_level = serde_json::json!({ "effort": "max" });
        assert_eq!(
            client_effort_choice(top_level.as_object().unwrap()),
            Some("max".to_string())
        );
        let none = serde_json::json!({});
        assert_eq!(client_effort_choice(none.as_object().unwrap()), None);
        let unknown = serde_json::json!({ "output_config": { "effort": "turbo" } });
        assert_eq!(client_effort_choice(unknown.as_object().unwrap()), None);
    }

    #[test]
    fn apply_route_model_applies_thinking_suffix_per_provider() {
        let mut state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: Some("kimi-a.json".into()),
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: BTreeMap::from([(
                "kimi".to_string(),
                RouteSelection {
                    model: "kimi-k3".into(),
                    thinking: "high".into(),
                },
            )]),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        let mut request = serde_json::json!({}).as_object().unwrap().clone();
        // kimi-k3 only supports max thinking, so route thinking "high" must
        // degrade to auto (plain id) for the route model.
        assert_eq!(
            apply_route_model("kimi-k3", None, &mut request, &state, "kimi"),
            "kimi-k3"
        );
        // A client-chosen kimi-k2.7-code supports high Ã¢â€ â€™ suffix applied.
        state.routes.insert(
            "kimi".into(),
            RouteSelection {
                model: "kimi-k2.7-code".into(),
                thinking: "high".into(),
            },
        );
        assert_eq!(
            apply_route_model("kimi-k2.7-code", None, &mut request, &state, "kimi"),
            "kimi-k2.7-code(high)"
        );
    }

    #[test]
    fn route_update_result_flattens_snapshot_and_exposes_route_verified() {
        let snapshot = GatewaySnapshot {
            running: true,
            base_url: "http://127.0.0.1:8317".into(),
            version: "test".into(),
            claude_running: false,
            codex_running: false,
            accounts: Vec::new(),
            active_account: None,
            routes: Vec::new(),
            active_codex_account: None,
            codex_routes: Vec::new(),
            auto_failover: None,
            controller: ComponentStatus {
                state: "healthy".into(),
                detail: String::new(),
            },
            relay: ComponentStatus {
                state: "healthy".into(),
                detail: String::new(),
            },
            backend: ComponentStatus {
                state: "healthy".into(),
                detail: String::new(),
            },
            credentials: ComponentStatus {
                state: "healthy".into(),
                detail: String::new(),
            },
            route: ComponentStatus {
                state: "ready".into(),
                detail: String::new(),
            },
            oauth: ComponentStatus {
                state: "ready".into(),
                detail: String::new(),
            },
            claude: ComponentStatus {
                state: "stopped".into(),
                detail: String::new(),
            },
            codex: ComponentStatus {
                state: "stopped".into(),
                detail: String::new(),
            },
            backend_exit_reason: None,
            active_requests: 0,
            diagnostics: Vec::new(),
            login: None,
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        let value = serde_json::to_value(RouteUpdateResult {
            snapshot,
            route_verified: false,
        })
        .unwrap();
        let object = value.as_object().unwrap();
        // Flattened snapshot fields stay at the top level (the frontend types
        // the result as Snapshot & { routeVerified }).
        assert_eq!(object.get("running"), Some(&serde_json::json!(true)));
        assert_eq!(object.get("routeVerified"), Some(&serde_json::json!(false)));
        // Camel-cased and disjoint from snapshot fields.
        assert_eq!(
            object.keys().filter(|key| *key == "routeVerified").count(),
            1
        );
    }

    #[test]
    fn extract_bearer_tokens_finds_top_level_and_nested_codex_tokens() {
        let mut tokens = Vec::new();
        let codex_native = serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "access_token": "token-nested-access",
                "refresh_token": "token-nested-refresh",
                "id_token": "token-nested-id"
            }
        });
        extract_bearer_tokens_from_value(&codex_native, &mut tokens);
        assert!(tokens.contains(&"token-nested-access".to_string()));
        assert!(tokens.contains(&"token-nested-id".to_string()));

        let mut relay_tokens = Vec::new();
        let relay_format = serde_json::json!({
            "access_token": "token-top-access",
            "type": "codex"
        });
        extract_bearer_tokens_from_value(&relay_format, &mut relay_tokens);
        assert!(relay_tokens.contains(&"token-top-access".to_string()));
    }

    #[test]
    fn api_key_account_shape_is_detected() {
        let value = serde_json::json!({
            "kind": "api_key",
            "provider": "deepseek",
            "api_key": "sk-test",
            "base_url": "https://api.deepseek.com",
            "label": "DeepSeek",
            "disabled": false
        });
        assert_eq!(account_auth_kind(&value), ProviderAuth::ApiKey);
        assert_eq!(
            account_provider(&value, "deepseek-acct.json").as_deref(),
            Some("deepseek")
        );
    }

    #[test]
    fn oauth_account_shape_is_detected() {
        let value = serde_json::json!({
            "type": { "provider": "codex" },
            "access_token": "token"
        });
        assert_eq!(account_auth_kind(&value), ProviderAuth::Oauth);
        assert_eq!(
            account_provider(&value, "codex-charles.json").as_deref(),
            Some("codex")
        );
    }

    #[test]
    fn render_config_keeps_the_cliproxy_contract() {
        // Guards the CLIProxyAPI config surface Basiliskos depends on. If an
        // upstream version bump forces a field rename and someone edits
        // `render_config` wrong, this catches it instead of silently breaking
        // the relay at runtime.
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            active_codex_account: None,
            codex_routes: default_routes(),
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
            skip_model_switch_confirmation: false,
            open_claude_on_launch: true,
        };
        let config = render_config(Path::new("unused-auth"), &state);
        for key in [
            "auth-dir:",
            "api-keys:",
            "oauth-model-alias:",
            "force-mapping: true",
            "inject-x-search: false",
            "optimize-multi-agent-v2: true",
            "plugins:",
            "configs:",
            "basiliskos-codex-compaction:",
            "disable-control-panel: true",
            "request-retry: 0",
            "bootstrap-retries: 0",
        ] {
            assert!(
                config.contains(key),
                "CLIProxyAPI config lost a required key: {key}"
            );
        }
    }

    #[test]
    fn openai_compat_provider_block_matches_the_cliproxy_schema() {
        // Guards the api-key provider emission against the pinned CLIProxyAPI
        // config.example.yaml shape (openai-compatibility list, api-key-entries
        // as an object list, explicit models). The wrong shape registers zero
        // providers and silently disables API-key routing.
        let yaml = openai_compat_provider_yaml(
            "deepseek",
            "https://api.deepseek.com",
            "sk-test",
            &["deepseek-chat".to_string(), "deepseek-reasoner".to_string()],
        );
        assert!(yaml.contains("  - name: \"deepseek\""));
        assert!(yaml.contains("    base-url: \"https://api.deepseek.com\""));
        assert!(yaml.contains("    api-key-entries:"));
        assert!(yaml.contains("      - api-key: \"sk-test\""));
        assert!(yaml.contains("    models:"));
        assert!(yaml.contains("      - name: \"deepseek-chat\""));
        assert!(yaml.contains("      - name: \"deepseek-reasoner\""));
        // The old, non-registering shape must never come back.
        assert!(!yaml.contains("\ndeepseek:\n"));
    }
}
