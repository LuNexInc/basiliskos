use bytes::Bytes;
use chrono::Utc;
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, OnceLock},
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use tiny_http::{Header, Response, Server, StatusCode};
use uuid::Uuid;

use crate::diagnostics::{self, DiagnosticEvent, ErrorCode};

use crate::persistence::{
    durable_write, load_json_with_recovery, recover_pending_transactions, run_transaction,
    secure_create_dir_all, secure_existing_path, FileMutation,
};

const GATEWAY_VERSION: &str = "7.2.77";
const GATEWAY_EXE_SHA256: &str = "0f2b23b5b533c92c2ce86bb37e2bb7bd7472b81b3f63bf8cc19950aca0a0cc2c";
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
const SUPPORTED_PROVIDERS: [&str; 3] = ["claude", "codex", "xai"];
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const XAI_USAGE_URL: &str = "https://cli-chat-proxy.grok.com/v1/billing?format=credits";
#[derive(Clone, Copy)]
struct ModelSpec {
    id: &'static str,
    label: &'static str,
    thinking_levels: &'static [&'static str],
}

const CLAUDE_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "claude-sonnet-4-5-20250929",
        label: "Claude Sonnet 4.5",
        thinking_levels: &["none", "low", "medium", "high", "xhigh", "max"],
    },
    ModelSpec {
        id: "claude-sonnet-4-6",
        label: "Claude Sonnet 4.6",
        thinking_levels: &["none", "low", "medium", "high", "max"],
    },
    ModelSpec {
        id: "claude-opus-4-5-20251101",
        label: "Claude Opus 4.5",
        thinking_levels: &["none", "low", "medium", "high", "xhigh", "max"],
    },
    ModelSpec {
        id: "claude-opus-4-6",
        label: "Claude Opus 4.6",
        thinking_levels: &["none", "low", "medium", "high", "max"],
    },
    ModelSpec {
        id: "claude-opus-4-7",
        label: "Claude Opus 4.7",
        thinking_levels: &["none", "low", "medium", "high", "xhigh", "max"],
    },
    ModelSpec {
        id: "claude-opus-4-8",
        label: "Claude Opus 4.8",
        thinking_levels: &["none", "low", "medium", "high", "xhigh", "max"],
    },
    ModelSpec {
        id: "claude-haiku-4-5-20251001",
        label: "Claude Haiku 4.5",
        thinking_levels: &["none", "low", "medium", "high", "xhigh", "max"],
    },
];

const CODEX_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "gpt-5.5",
        label: "GPT-5.5",
        thinking_levels: &["low", "medium", "high", "xhigh"],
    },
    ModelSpec {
        id: "gpt-5.6-sol",
        label: "GPT-5.6 Sol",
        thinking_levels: &["low", "medium", "high", "xhigh", "max", "ultra"],
    },
    ModelSpec {
        id: "gpt-5.6-terra",
        label: "GPT-5.6 Terra",
        thinking_levels: &["low", "medium", "high", "xhigh", "max", "ultra"],
    },
    ModelSpec {
        id: "gpt-5.6-luna",
        label: "GPT-5.6 Luna",
        thinking_levels: &["low", "medium", "high", "xhigh", "max"],
    },
    ModelSpec {
        id: "gpt-5.4",
        label: "GPT-5.4",
        thinking_levels: &["low", "medium", "high", "xhigh"],
    },
    ModelSpec {
        id: "gpt-5.4-mini",
        label: "GPT-5.4 Mini",
        thinking_levels: &["low", "medium", "high", "xhigh"],
    },
];

const XAI_MODELS: &[ModelSpec] = &[
    ModelSpec {
        id: "grok-build-0.1",
        label: "Grok Build 0.1",
        thinking_levels: &[],
    },
    ModelSpec {
        id: "grok-4.5",
        label: "Grok 4.5",
        thinking_levels: &["low", "medium", "high"],
    },
    ModelSpec {
        id: "grok-4.3",
        label: "Grok 4.3",
        thinking_levels: &["none", "low", "medium", "high"],
    },
    ModelSpec {
        id: "grok-4.20-0309-reasoning",
        label: "Grok 4.20 Reasoning",
        thinking_levels: &[],
    },
    ModelSpec {
        id: "grok-4.20-0309-non-reasoning",
        label: "Grok 4.20 Non-Reasoning",
        thinking_levels: &[],
    },
    ModelSpec {
        id: "grok-4.20-multi-agent-0309",
        label: "Grok 4.20 Multi-Agent",
        thinking_levels: &["low", "medium", "high"],
    },
    ModelSpec {
        id: "grok-3-mini",
        label: "Grok 3 Mini",
        thinking_levels: &["low", "medium", "high"],
    },
    ModelSpec {
        id: "grok-3-mini-fast",
        label: "Grok 3 Mini Fast",
        thinking_levels: &["low", "medium", "high"],
    },
    ModelSpec {
        id: "grok-composer-2.5-fast",
        label: "Grok Composer 2.5 Fast",
        thinking_levels: &[],
    },
];

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
    front_proxy: Option<FrontProxy>,
    backend_exit_reason: Option<String>,
    backend_restart_attempts: u32,
    backend_next_restart: Option<Instant>,
    last_known_good_models: BTreeMap<String, String>,
    login_claim: Option<String>,
    login: Option<LoginRuntime>,
    last_login: Option<ProviderLoginStatus>,
    #[cfg(target_os = "windows")]
    gateway_job: Option<usize>,
}

#[derive(Default)]
struct ControllerManager {
    runtime: Mutex<ControllerRuntime>,
    mutations: Mutex<()>,
}

static CONTROLLER: OnceLock<ControllerManager> = OnceLock::new();

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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayAccount {
    pub file_name: String,
    pub provider: String,
    pub email: Option<String>,
    pub label: String,
    pub disabled: bool,
    pub active: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySnapshot {
    pub running: bool,
    pub base_url: String,
    pub version: String,
    pub claude_running: bool,
    pub accounts: Vec<GatewayAccount>,
    pub active_account: Option<String>,
    pub routes: Vec<ProviderRoute>,
    pub controller: ComponentStatus,
    pub relay: ComponentStatus,
    pub backend: ComponentStatus,
    pub credentials: ComponentStatus,
    pub route: ComponentStatus,
    pub oauth: ComponentStatus,
    pub claude: ComponentStatus,
    pub backend_exit_reason: Option<String>,
    pub active_requests: usize,
    pub diagnostics: Vec<DiagnosticEvent>,
    pub login: Option<ProviderLoginStatus>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ComponentStatus {
    pub state: String,
    pub detail: String,
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
pub struct ProviderRoute {
    pub provider: String,
    pub selected_model: String,
    pub selected_model_label: String,
    pub thinking: String,
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
    child: Arc<Mutex<Child>>,
    staging_dir: PathBuf,
    #[cfg(target_os = "windows")]
    job: Option<usize>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct GatewayUsageWindow {
    pub label: String,
    pub used_percent: f64,
    pub remaining_percent: f64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayAccountUsage {
    pub file_name: String,
    pub provider: String,
    pub windows: Vec<GatewayUsageWindow>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct RouteSelection {
    model: String,
    thinking: String,
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
    #[serde(default = "default_routes")]
    routes: BTreeMap<String, RouteSelection>,
    /// Basiliskos-owned preference: recolor the isolated Claude window/tray icons.
    /// Never written into Claude's own profile. Default black (distinct from stock Claude).
    #[serde(default = "default_claude_window_icon")]
    claude_window_icon: ClaudeWindowIcon,
}

fn model_specs(provider: &str) -> &'static [ModelSpec] {
    match provider {
        "claude" => CLAUDE_MODELS,
        "codex" => CODEX_MODELS,
        "xai" => XAI_MODELS,
        _ => &[],
    }
}

fn default_model(provider: &str) -> &'static str {
    match provider {
        "claude" => "claude-sonnet-4-5-20250929",
        "codex" => "gpt-5.5",
        "xai" => "grok-build-0.1",
        _ => "",
    }
}

fn default_routes() -> BTreeMap<String, RouteSelection> {
    SUPPORTED_PROVIDERS
        .into_iter()
        .map(|provider| {
            (
                provider.to_string(),
                RouteSelection {
                    model: default_model(provider).to_string(),
                    thinking: "auto".into(),
                },
            )
        })
        .collect()
}

fn normalized_route(state: &ControllerState, provider: &str) -> RouteSelection {
    let specs = model_specs(provider);
    let stored = state.routes.get(provider);
    let model = stored
        .map(|route| route.model.as_str())
        .filter(|model| specs.iter().any(|spec| spec.id == *model))
        .unwrap_or_else(|| default_model(provider));
    let spec = specs
        .iter()
        .find(|spec| spec.id == model)
        .expect("every supported provider has a default model");
    let thinking = stored
        .map(|route| route.thinking.as_str())
        .filter(|thinking| *thinking == "auto" || spec.thinking_levels.contains(thinking))
        .unwrap_or("auto");
    RouteSelection {
        model: model.to_string(),
        thinking: thinking.to_string(),
    }
}

fn provider_route(state: &ControllerState, provider: &str) -> ProviderRoute {
    let route = normalized_route(state, provider);
    let specs = model_specs(provider);
    let selected = specs
        .iter()
        .find(|spec| spec.id == route.model)
        .expect("normalized routes always select a catalog model");
    ProviderRoute {
        provider: provider.to_string(),
        selected_model: route.model,
        selected_model_label: selected.label.to_string(),
        thinking: route.thinking,
        model_options: specs
            .iter()
            .map(|spec| RouteModelOption {
                id: spec.id.to_string(),
                label: spec.label.to_string(),
                thinking_levels: spec
                    .thinking_levels
                    .iter()
                    .map(|level| level.to_string())
                    .collect(),
            })
            .collect(),
    }
}

fn routed_model(state: &ControllerState, provider: &str) -> String {
    let route = normalized_route(state, provider);
    if route.thinking == "auto" {
        route.model
    } else {
        format!("{}({})", route.model, route.thinking)
    }
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

fn gateway_dir() -> Result<PathBuf, String> {
    Ok(root_dir()?.join("gateway"))
}

fn auth_dir() -> Result<PathBuf, String> {
    Ok(gateway_dir()?.join("auth"))
}

fn controller_path() -> Result<PathBuf, String> {
    Ok(root_dir()?.join("controller.json"))
}

fn account_labels_path() -> Result<PathBuf, String> {
    Ok(root_dir()?.join("account-labels.json"))
}

fn config_path() -> Result<PathBuf, String> {
    Ok(gateway_dir()?.join("config.yaml"))
}

fn runtime_exe_path() -> Result<PathBuf, String> {
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

pub fn initialize_controller_storage() -> Result<(), String> {
    let _mutation = mutation_lock()?;
    let root = root_dir()?;
    let gateway = gateway_dir()?;
    let auth = auth_dir()?;
    let controller_logs = gateway.join("controller-logs");
    let claude_profile = isolated_claude_profile_dir()?;
    let claude_logs = claude_profile.join("Basiliskos Logs");
    for directory in [
        &root,
        &gateway,
        &auth,
        &controller_logs,
        &claude_profile,
        &claude_logs,
    ] {
        secure_create_dir_all(directory)?;
    }
    recover_pending_transactions(&root)?;
    let state_file = controller_path()?;
    let labels_file = account_labels_path()?;
    let config_file = config_path()?;
    for file in [&state_file, &labels_file, &config_file] {
        secure_existing_path(file)?;
    }
    for json_file in [&state_file, &labels_file] {
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
    Ok(())
}

fn load_state() -> Result<ControllerState, String> {
    let path = controller_path()?;
    if path.exists() || crate::persistence::backup_path(&path)?.exists() {
        return load_json_with_recovery(&path, "Basiliskos controller state");
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
        routes: default_routes(),
        claude_window_icon: default_claude_window_icon(),
    };
    save_state(&state)?;
    Ok(state)
}

fn save_state(state: &ControllerState) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("Could not serialize controller state: {error}"))?;
    durable_write(&controller_path()?, &bytes)
}

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

fn sha256_file(path: &Path) -> Result<String, String> {
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

fn yaml_quote(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "/").replace('"', "\\\""))
}

fn active_provider_from_auth(auth: &Path, state: &ControllerState) -> Option<String> {
    let file_name = state.active_account.as_deref()?;
    let raw = fs::read_to_string(auth.join(file_name)).ok()?;
    let value = serde_json::from_str::<Value>(&raw).ok()?;
    account_provider(&value, file_name)
}

fn render_config(auth: &Path, state: &ControllerState) -> String {
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
plugins:
  enabled: false
"#,
        auth_dir = yaml_quote(&auth.to_string_lossy()),
        api_key = yaml_quote(&state.api_key),
    )
}

fn prepare_config() -> Result<ControllerState, String> {
    let state = load_state()?;
    let auth = auth_dir()?;
    secure_create_dir_all(&auth)?;
    durable_write(&config_path()?, render_config(&auth, &state).as_bytes())?;
    Ok(state)
}

fn endpoint_health_check(port: u16, path: &str, api_key: &str, marker: &str) -> bool {
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

fn validated_route_for_request(
    state: &ControllerState,
    provider: &str,
    correlation_id: &str,
) -> RouteSelection {
    let selected = normalized_route(state, provider);
    let Ok(models) = backend_model_ids(&state.api_key) else {
        return selected;
    };
    if models.is_empty() || models.iter().any(|model| model == &selected.model) {
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
        .filter(|model| models.contains(model))
        .or_else(|| {
            model_specs(provider)
                .iter()
                .find(|spec| models.iter().any(|model| model == spec.id))
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
    object.insert("model".into(), Value::String(routed_model(state, provider)));
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

fn secure_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.bytes()
        .zip(right.bytes())
        .fold(0_u8, |difference, (a, b)| difference | (a ^ b))
        == 0
}

fn request_is_authorized(request: &tiny_http::Request, api_key: &str) -> bool {
    request.headers().iter().any(|header| {
        let name = header.field.as_str().as_str();
        let value = header.value.as_str().trim();
        (name.eq_ignore_ascii_case("x-api-key") && secure_eq(value, api_key))
            || (name.eq_ignore_ascii_case("authorization")
                && value
                    .strip_prefix("Bearer ")
                    .is_some_and(|token| secure_eq(token, api_key)))
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
enum StreamFailure {
    MidstreamIdle,
    UpstreamEnded,
}

struct TrackedUpstream {
    receiver: tokio::sync::mpsc::Receiver<Result<Bytes, StreamFailure>>,
    current: Option<Bytes>,
    offset: usize,
    correlation_id: String,
    provider: Option<String>,
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

struct UpstreamMeta {
    status: u16,
    headers: Vec<(String, Vec<u8>)>,
    body: tokio::sync::mpsc::Receiver<Result<Bytes, StreamFailure>>,
}

#[derive(Clone, Copy, Debug)]
enum FirstResponseFailure {
    Timeout,
    Connect,
}

fn begin_upstream_request(
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

fn classify_upstream_status(status: u16) -> Option<ErrorCode> {
    match status {
        401 | 403 => Some(ErrorCode::ProviderAuthFailed),
        429 => Some(ErrorCode::ProviderRateLimited),
        500..=599 => Some(ErrorCode::UpstreamServerError),
        _ => None,
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

fn handle_front_proxy_request(
    mut request: tiny_http::Request,
    client: &reqwest::Client,
    async_runtime: &tokio::runtime::Handle,
    _api_key: &str,
    correlation_id: &str,
) {
    let request_url = request.url().to_string();
    let request_path = request_url
        .split('?')
        .next()
        .unwrap_or(request_url.as_str());
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
    if request_path == "/v1/messages" || request_path == "/v1/messages/count_tokens" {
        let rewrite_result = (|| -> Result<(), String> {
            let _mutation = mutation_lock()?;
            let mut state = load_state()?;
            let provider = active_provider_from_auth(&auth_dir()?, &state)
                .ok_or_else(|| "Choose an active Basiliskos account first".to_string())?;
            provider_for_event = Some(provider.clone());
            let validated = validated_route_for_request(&state, &provider, correlation_id);
            state.routes.insert(provider.clone(), validated);
            let mut json: Value = serde_json::from_slice(&body)
                .map_err(|_| "Claude request body is invalid JSON".to_string())?;
            rewrite_claude_request(&mut json, &state, &provider, request_path == "/v1/messages")?;
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

    let upstream_url = format!("http://127.0.0.1:{BACKEND_PORT}{request_url}");
    let mut upstream_headers = Vec::new();
    for header in request.headers() {
        let name = header.field.as_str().as_str();
        if !is_hop_by_hop_header(name) {
            upstream_headers.push((name.to_owned(), header.value.as_str().to_owned()));
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
                ErrorCode::ProviderAuthFailed => "The provider rejected the selected credential.",
                ErrorCode::ProviderRateLimited => {
                    "The provider rate-limited the selected credential."
                }
                _ => "The provider returned a server error.",
            },
            Some(correlation_id),
            Some(upstream_status),
            provider_for_event.as_deref(),
        );
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
    let response = Response::new(
        status,
        headers,
        TrackedUpstream {
            receiver: upstream.body,
            current: None,
            offset: 0,
            correlation_id: correlation_id.to_owned(),
            provider: provider_for_event,
        },
        None,
        None,
    );
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
fn hidden(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn hidden(_command: &mut Command) {}

#[cfg(target_os = "windows")]
fn assign_gateway_to_kill_on_close_job(child: &Child) -> Result<Option<usize>, String> {
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
fn close_gateway_job(job: Option<usize>) {
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

fn stop_gateway_runtime() {
    stop_hydra_claude_runtime();
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

fn account_provider(value: &Value, file_name: &str) -> Option<String> {
    let explicit =
        nested_string(value, &["type", "provider"]).map(|provider| provider.to_ascii_lowercase());
    let provider = explicit.or_else(|| {
        let lower = file_name.to_ascii_lowercase();
        SUPPORTED_PROVIDERS
            .iter()
            .find(|provider| lower.starts_with(**provider))
            .map(|provider| provider.to_string())
    })?;
    SUPPORTED_PROVIDERS
        .contains(&provider.as_str())
        .then_some(provider)
}

fn list_accounts_inner(state: &ControllerState) -> Result<Vec<GatewayAccount>, String> {
    let directory = auth_dir()?;
    let labels = load_account_labels()?;
    secure_create_dir_all(&directory)?;
    let mut accounts = Vec::new();
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
        let Some(provider) = account_provider(&value, &file_name) else {
            continue;
        };
        let email = nested_string(&value, &["email", "preferred_username"]);
        let disabled = value
            .get("disabled")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let label = labels.get(&file_name).cloned().unwrap_or_else(|| {
            email.clone().unwrap_or_else(|| match provider.as_str() {
                "xai" => "Grok account".into(),
                "codex" => "Codex account".into(),
                _ => "Claude account".into(),
            })
        });
        accounts.push(GatewayAccount {
            active: state.active_account.as_deref() == Some(file_name.as_str()) && !disabled,
            file_name,
            provider,
            email,
            label,
            disabled,
        });
    }
    accounts.sort_by(|left, right| {
        left.provider
            .cmp(&right.provider)
            .then(left.label.cmp(&right.label))
    });
    Ok(accounts)
}

fn shared_claude_library_dir() -> Result<PathBuf, String> {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .map(|path| path.join("Claude-3p").join("configLibrary"))
        .ok_or_else(|| "LOCALAPPDATA is not available".to_string())
}

fn isolated_claude_profile_dir() -> Result<PathBuf, String> {
    Ok(root_dir()?.join("claude-profile"))
}

#[tauri::command]
pub fn gateway_snapshot() -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    gateway_snapshot_locked()
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
    let running = gateway_running();
    let claude_running = hydra_claude_running();
    let (phase, relay_present, backend_exit_reason, active_requests, login) = {
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
        accounts,
        active_account: state.active_account,
        routes,
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
            detail: route_detail,
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
        backend_exit_reason,
        active_requests,
        diagnostics: diagnostics::snapshot(),
        login,
    })
}

fn number_at(value: &Value, path: &[&str]) -> Option<f64> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current
        .as_f64()
        .or_else(|| current.as_str()?.parse::<f64>().ok())
}

fn usage_window(label: &str, used_percent: f64) -> GatewayUsageWindow {
    let used_percent = used_percent.clamp(0.0, 100.0);
    GatewayUsageWindow {
        label: label.into(),
        used_percent,
        remaining_percent: 100.0 - used_percent,
    }
}

fn parse_claude_usage(value: &Value) -> Vec<GatewayUsageWindow> {
    let mut windows = Vec::new();
    if let Some(used) = number_at(value, &["five_hour", "utilization"]) {
        windows.push(usage_window("5h", used));
    }
    if let Some(used) = number_at(value, &["seven_day", "utilization"]) {
        windows.push(usage_window("Week", used));
    }
    windows
}

fn codex_window_label(window: &Value, fallback: &str) -> String {
    match number_at(window, &["limit_window_seconds"]).map(|value| value as i64) {
        Some(seconds) if (14_400..=21_600).contains(&seconds) => "5h".into(),
        Some(seconds) if (518_400..=691_200).contains(&seconds) => "Week".into(),
        _ => fallback.into(),
    }
}

fn parse_codex_usage(value: &Value) -> Vec<GatewayUsageWindow> {
    let mut windows = Vec::new();
    let Some(rate_limit) = value.get("rate_limit") else {
        return windows;
    };
    for (key, fallback) in [("primary_window", "5h"), ("secondary_window", "Week")] {
        let Some(window) = rate_limit.get(key) else {
            continue;
        };
        if let Some(used) = number_at(window, &["used_percent"]) {
            windows.push(usage_window(&codex_window_label(window, fallback), used));
        }
    }
    windows
}

fn parse_xai_usage(value: &Value) -> Vec<GatewayUsageWindow> {
    let product_usage = value
        .get("config")
        .and_then(|config| config.get("productUsage"))
        .and_then(Value::as_array)
        .and_then(|items| {
            items.iter().find_map(|item| {
                let is_grok_build = item
                    .get("product")
                    .and_then(Value::as_str)
                    .is_none_or(|product| product.eq_ignore_ascii_case("GrokBuild"));
                is_grok_build
                    .then(|| number_at(item, &["usagePercent"]))
                    .flatten()
            })
        });
    number_at(value, &["config", "creditUsagePercent"])
        .or_else(|| number_at(value, &["creditUsagePercent"]))
        .or(product_usage)
        .map(|used| vec![usage_window("Week", used)])
        .unwrap_or_default()
}

async fn fetch_usage_json(
    provider: &str,
    token: &str,
    account_id: Option<&str>,
) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .user_agent("Basiliskos/1.1")
        .build()
        .map_err(|error| format!("Could not prepare usage request: {error}"))?;
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
        _ => return Err("Unsupported usage provider".into()),
    };
    request = request.header("Accept", "application/json");
    let response = request
        .send()
        .await
        .map_err(|error| format!("Usage request failed: {error}"))?;
    if response.status() == reqwest::StatusCode::UNAUTHORIZED
        || response.status() == reqwest::StatusCode::FORBIDDEN
    {
        return Err("Sign in again to refresh usage".into());
    }
    if !response.status().is_success() {
        return Err(format!("Usage service returned {}", response.status()));
    }
    response
        .json()
        .await
        .map_err(|error| format!("Usage response was invalid: {error}"))
}

#[tauri::command]
pub async fn get_gateway_account_usage(file_name: String) -> Result<GatewayAccountUsage, String> {
    let (account, token, account_id) = {
        let _mutation = mutation_lock()?;
        let path = exact_auth_path(&file_name)?;
        let state = load_state()?;
        let account = list_accounts_inner(&state)?
            .into_iter()
            .find(|account| account.file_name == file_name)
            .ok_or("Account not found")?;
        let raw = fs::read_to_string(&path)
            .map_err(|error| format!("Could not read account credentials: {error}"))?;
        let value: Value = serde_json::from_str(&raw)
            .map_err(|error| format!("Account credentials are invalid: {error}"))?;
        let token =
            nested_string(&value, &["access_token"]).ok_or("Sign in again to refresh usage")?;
        let account_id = nested_string(&value, &["account_id"]);
        (account, token, account_id)
    };
    let usage = fetch_usage_json(&account.provider, &token, account_id.as_deref()).await?;
    let windows = match account.provider.as_str() {
        "claude" => parse_claude_usage(&usage),
        "codex" => parse_codex_usage(&usage),
        "xai" => parse_xai_usage(&usage),
        _ => Vec::new(),
    };
    if windows.is_empty() {
        return Err("Usage remaining is not available for this profile".into());
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
    let path = exact_auth_path(&file_name)?;
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

fn exact_auth_path(file_name: &str) -> Result<PathBuf, String> {
    let supplied = Path::new(file_name);
    if supplied.file_name().and_then(|value| value.to_str()) != Some(file_name)
        || supplied.components().count() != 1
        || supplied.extension().and_then(|value| value.to_str()) != Some("json")
    {
        return Err("Invalid account filename".into());
    }
    Ok(auth_dir()?.join(file_name))
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

fn validate_account_invariant(directory: &Path, state_path: &Path) -> Result<(), String> {
    let state: ControllerState = serde_json::from_slice(
        &fs::read(state_path)
            .map_err(|error| format!("Could not validate {}: {error}", state_path.display()))?,
    )
    .map_err(|error| format!("Controller state failed transaction validation: {error}"))?;
    let mut enabled = Vec::new();
    let mut supported = Vec::new();
    for entry in fs::read_dir(directory)
        .map_err(|error| format!("Could not validate {}: {error}", directory.display()))?
    {
        let entry = entry.map_err(|error| format!("Could not validate an account: {error}"))?;
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
    match state.active_account.as_deref() {
        Some(active) => {
            if !supported.iter().any(|file| file == active) {
                return Err("The selected account disappeared during the transaction".into());
            }
            if enabled.len() != 1 || enabled[0] != active {
                return Err(format!(
                    "Account transaction invariant failed: expected only {active} enabled, found {}",
                    enabled.join(", ")
                ));
            }
        }
        None if !enabled.is_empty() => {
            return Err(format!(
                "Account transaction invariant failed: no account is selected but these are enabled: {}",
                enabled.join(", ")
            ));
        }
        None => {}
    }
    Ok(())
}

fn selection_transaction(
    root: &Path,
    directory: &Path,
    state_path: &Path,
    accounts: &[GatewayAccount],
    state: &ControllerState,
    file_name: &str,
) -> Result<(Vec<FileMutation>, ControllerState), String> {
    let mut mutations = Vec::with_capacity(accounts.len() + 1);
    for account in accounts
        .iter()
        .filter(|account| account.file_name != file_name)
        .chain(
            accounts
                .iter()
                .filter(|account| account.file_name == file_name),
        )
    {
        let path = directory.join(&account.file_name);
        mutations.push(FileMutation {
            path,
            after: Some(account_bytes_with_disabled(
                &directory.join(&account.file_name),
                account.file_name != file_name,
            )?),
        });
    }
    let mut after_state = state.clone();
    after_state.active_account = Some(file_name.to_string());
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
    accounts: &[GatewayAccount],
    state: &ControllerState,
    labels: &BTreeMap<String, String>,
    file_name: &str,
) -> Result<(Vec<FileMutation>, ControllerState), String> {
    let removing_active = state.active_account.as_deref() == Some(file_name);
    let mut mutations = vec![FileMutation {
        path: paths.directory.join(file_name),
        after: None,
    }];
    if removing_active {
        for account in accounts {
            if account.file_name != file_name {
                let account_path = paths.directory.join(&account.file_name);
                mutations.push(FileMutation {
                    after: Some(account_bytes_with_disabled(&account_path, true)?),
                    path: account_path,
                });
            }
        }
    }
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

#[tauri::command]
pub fn select_gateway_account(file_name: String) -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    let selected = exact_auth_path(&file_name)?;
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
        &file_name,
    )?;
    run_transaction(&root, &mutations, || {
        validate_account_invariant(&directory, &state_path)
    })?;
    runtime_lock()?.last_known_good_models.clear();
    prepare_config()?;
    write_isolated_claude_config(&isolated_claude_profile_dir()?, &state)?;
    gateway_snapshot_locked()
}

#[tauri::command]
pub fn set_gateway_route(
    provider: String,
    model: String,
    thinking: String,
) -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    if !SUPPORTED_PROVIDERS.contains(&provider.as_str()) {
        return Err("Provider must be claude, codex, or xai".into());
    }
    let Some(spec) = model_specs(&provider).iter().find(|spec| spec.id == model) else {
        return Err(format!("{model} is not an available {provider} model"));
    };
    if thinking != "auto" && !spec.thinking_levels.contains(&thinking.as_str()) {
        return Err(format!(
            "{} does not support the {thinking} thinking setting",
            spec.label
        ));
    }
    let mut state = load_state()?;
    let account_is_active = list_accounts_inner(&state)?
        .iter()
        .any(|account| account.active && account.provider == provider);
    if account_is_active {
        if let Ok(models) = backend_model_ids(&state.api_key) {
            if !models.is_empty() && !models.contains(&model) {
                return Err(format!(
                    "{} is not available for the selected {} credential. Choose a model advertised by the backend.",
                    spec.label,
                    provider_label(&provider)
                ));
            }
            if models.contains(&model) {
                runtime_lock()?
                    .last_known_good_models
                    .insert(provider.clone(), model.clone());
            }
        }
    }
    state
        .routes
        .insert(provider.clone(), RouteSelection { model, thinking });
    save_state(&state)?;
    prepare_config()?;
    if account_is_active {
        write_isolated_claude_config(&isolated_claude_profile_dir()?, &state)?;
    }
    gateway_snapshot_locked()
}

#[tauri::command]
pub fn remove_gateway_account(file_name: String) -> Result<GatewaySnapshot, String> {
    let _mutation = mutation_lock()?;
    let path = exact_auth_path(&file_name)?;
    let state = load_state()?;
    let accounts = list_accounts_inner(&state)?;
    if !accounts
        .iter()
        .any(|account| account.file_name == file_name)
    {
        return Err("Account not found".into());
    }
    let root = root_dir()?;
    let directory = auth_dir()?;
    let state_path = controller_path()?;
    let labels_path = account_labels_path()?;
    debug_assert_eq!(path, directory.join(&file_name));
    let labels = load_account_labels()?;
    let (mutations, _state) = removal_transaction(
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
        validate_account_invariant(&directory, &state_path)
    })?;
    gateway_snapshot_locked()
}

enum LoginOutput {
    Line(String),
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
        _ => false,
    };
    allowed.then(|| candidate.to_string())
}

fn extract_xai_user_code(line: &str) -> Option<String> {
    let (_, value) = line.split_once("Then enter this code:")?;
    let code = value.trim();
    (!code.is_empty()).then(|| code.to_string())
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
    let exit = session
        .child
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

fn launch_provider_login_blocking(
    app: AppHandle,
    provider: String,
) -> Result<ProviderLoginLaunch, String> {
    let flag = match provider.as_str() {
        "claude" => "-claude-login",
        "codex" => "-codex-login",
        "xai" => "-xai-login",
        _ => return Err("Provider must be claude, codex, or xai".into()),
    };
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
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            let _ = output_tx.send(LoginOutput::Line(line));
        }
        let _ = output_tx.send(LoginOutput::Eof);
    });
    std::thread::spawn(move || {
        for _ in BufReader::new(stderr).lines() {
            // Keep the child process from blocking on a full stderr pipe. OAuth output is
            // intentionally not persisted because it can contain short-lived login data.
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
                if provider == "xai" && user_code.is_none() {
                    user_code = extract_xai_user_code(&line);
                }
                let ready = authorization_url.is_some()
                    && (provider != "xai" || line.contains("Waiting for authorization"));
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
                            child: Arc::clone(&child),
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
    if let Ok(mut child) = session.child.lock() {
        let _ = child.kill();
        let _ = child.wait();
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
        || deployment.get("deploymentMode").and_then(Value::as_str) != Some("3p")
    {
        return Err("The Claude config set failed its cross-file invariant".into());
    }
    Ok(())
}

fn write_isolated_claude_config(profile: &Path, state: &ControllerState) -> Result<(), String> {
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
        serde_json::json!([{"name": "claude-sonnet-4-5", "labelOverride": model_label}]),
    );
    generated.insert("inferenceProvider".into(), Value::String("gateway".into()));
    generated.insert("modelDiscoveryEnabled".into(), Value::Bool(true));
    generated.insert("unstableDisableModelVerification".into(), Value::Bool(true));

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
        return Ok(());
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

#[derive(Clone, Copy)]
enum ClaudeIconKind {
    WindowBlack,
    TrayInverted,
}

fn claude_icon_file_name(kind: ClaudeIconKind) -> &'static str {
    match kind {
        ClaudeIconKind::WindowBlack => "claude-window-black.ico",
        ClaudeIconKind::TrayInverted => "claude-tray-inverted.ico",
    }
}

fn claude_icon_path(app: &AppHandle, kind: ClaudeIconKind) -> Result<PathBuf, String> {
    let file_name = claude_icon_file_name(kind);
    let mut candidates = Vec::new();
    if let Ok(resource) = app.path().resource_dir() {
        candidates.push(resource.join("resources/icons").join(file_name));
        candidates.push(resource.join("icons").join(file_name));
    }
    candidates.push(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("resources/icons")
            .join(file_name),
    );
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| format!("Bundled Claude icon missing: {file_name}"))
}

#[cfg(target_os = "windows")]
const CLAUDE_BASILISKOS_AUMID: &str = "com.threereadylab.basiliskos.claude";

#[cfg(target_os = "windows")]
struct OwnedIcon(isize);

#[cfg(target_os = "windows")]
impl Drop for OwnedIcon {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::WindowsAndMessaging::DestroyIcon;
        unsafe {
            let _ = DestroyIcon(self.0 as windows_sys::Win32::UI::WindowsAndMessaging::HICON);
        }
    }
}

#[cfg(target_os = "windows")]
fn load_hicons(path: &Path) -> Result<(OwnedIcon, OwnedIcon), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::{LoadImageW, IMAGE_ICON, LR_LOADFROMFILE};

    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // Do not use LR_SHARED — Windows may cache a stale icon from a previous ICO path.
    unsafe {
        let small = LoadImageW(
            std::ptr::null_mut(),
            wide.as_ptr(),
            IMAGE_ICON,
            16,
            16,
            LR_LOADFROMFILE,
        );
        let big = LoadImageW(
            std::ptr::null_mut(),
            wide.as_ptr(),
            IMAGE_ICON,
            32,
            32,
            LR_LOADFROMFILE,
        );
        if small.is_null() {
            return Err(format!("Could not load icon {}", path.display()));
        }
        let small = OwnedIcon(small as isize);
        if big.is_null() {
            return Err(format!("Could not load icon {}", path.display()));
        }
        Ok((small, OwnedIcon(big as isize)))
    }
}

#[cfg(target_os = "windows")]
#[derive(Clone, Debug)]
struct ClaudeHwndInfo {
    hwnd: isize,
    visible: bool,
    class_name: String,
}

#[cfg(target_os = "windows")]
fn enum_claude_hwnds_for_pid(pid: u32) -> Vec<ClaudeHwndInfo> {
    use windows_sys::Win32::Foundation::{HWND, LPARAM, TRUE};
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameW, GetWindow, GetWindowThreadProcessId, IsWindowVisible, GW_OWNER,
    };

    struct EnumData {
        pid: u32,
        windows: Vec<ClaudeHwndInfo>,
    }

    unsafe extern "system" fn callback(hwnd: HWND, lparam: LPARAM) -> windows_sys::core::BOOL {
        let data = &mut *(lparam as *mut EnumData);
        let mut window_pid = 0_u32;
        GetWindowThreadProcessId(hwnd, &mut window_pid);
        if window_pid == data.pid && GetWindow(hwnd, GW_OWNER).is_null() {
            let mut class_buf = [0_u16; 256];
            let class_len = GetClassNameW(hwnd, class_buf.as_mut_ptr(), class_buf.len() as i32);
            let class_name = if class_len > 0 {
                String::from_utf16_lossy(&class_buf[..class_len as usize])
            } else {
                String::new()
            };
            data.windows.push(ClaudeHwndInfo {
                hwnd: hwnd as isize,
                visible: IsWindowVisible(hwnd) != 0,
                class_name,
            });
        }
        TRUE
    }

    let mut data = EnumData {
        pid,
        windows: Vec::new(),
    };
    unsafe {
        let _ = EnumWindows(Some(callback), &mut data as *mut EnumData as LPARAM);
    }
    data.windows
}

#[cfg(target_os = "windows")]
fn apply_icons_to_hwnd(hwnd: isize, small: isize, big: isize) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        SendMessageW, SetClassLongPtrW, SetWindowPos, GCLP_HICON, GCLP_HICONSM, ICON_BIG,
        ICON_SMALL, SWP_FRAMECHANGED, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER, WM_SETICON,
    };

    unsafe {
        let hwnd = hwnd as windows_sys::Win32::Foundation::HWND;
        let _ = SendMessageW(hwnd, WM_SETICON, ICON_SMALL as usize, small);
        let _ = SendMessageW(hwnd, WM_SETICON, ICON_BIG as usize, big);
        let _ = SetClassLongPtrW(hwnd, GCLP_HICONSM, small);
        let _ = SetClassLongPtrW(hwnd, GCLP_HICON, big);
        let _ = SetWindowPos(
            hwnd,
            std::ptr::null_mut(),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
        );
    }
}

/// Best-effort AUMID + relaunch icon via raw shell32 COM.
#[cfg(target_os = "windows")]
struct ComApartment;

#[cfg(target_os = "windows")]
impl ComApartment {
    fn initialize() -> Option<Self> {
        use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED};
        let result = unsafe { CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        (result >= 0).then_some(Self)
    }
}

#[cfg(target_os = "windows")]
impl Drop for ComApartment {
    fn drop(&mut self) {
        use windows_sys::Win32::System::Com::CoUninitialize;
        unsafe { CoUninitialize() };
    }
}

/// Best-effort AUMID + relaunch icon via raw shell32 COM.
#[cfg(target_os = "windows")]
fn apply_basiliskos_aumid(hwnd: isize, window_ico: &Path) {
    use std::os::windows::ffi::OsStrExt;

    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }
    #[repr(C)]
    struct PropertyKey {
        fmtid: Guid,
        pid: u32,
    }
    #[repr(C)]
    struct PropVariant {
        vt: u16,
        r1: u16,
        r2: u16,
        r3: u16,
        data: usize,
    }

    type Hresult = i32;
    type Hwnd = *mut core::ffi::c_void;

    #[link(name = "shell32")]
    extern "system" {
        fn SHGetPropertyStoreForWindow(
            hwnd: Hwnd,
            riid: *const Guid,
            ppv: *mut *mut core::ffi::c_void,
        ) -> Hresult;
    }
    const VT_LPWSTR: u16 = 31;
    const FMTID: Guid = Guid {
        data1: 0x9F4C2855,
        data2: 0x9F79,
        data3: 0x4B39,
        data4: [0xA8, 0xD0, 0xE1, 0xD4, 0x2D, 0xE1, 0xD5, 0xF3],
    };
    const IID_IPROPERTY_STORE: Guid = Guid {
        data1: 0x886D8EEB,
        data2: 0x8CF2,
        data3: 0x4446,
        data4: [0x8D, 0x02, 0xCD, 0xBA, 0x1D, 0xBD, 0xCF, 0x99],
    };
    const PKEY_AUMID: PropertyKey = PropertyKey {
        fmtid: FMTID,
        pid: 5,
    };
    const PKEY_RELAUNCH_NAME: PropertyKey = PropertyKey {
        fmtid: FMTID,
        pid: 4,
    };
    const PKEY_RELAUNCH_ICON: PropertyKey = PropertyKey {
        fmtid: FMTID,
        pid: 8,
    };

    unsafe {
        let Some(_com) = ComApartment::initialize() else {
            return;
        };
        let mut store: *mut core::ffi::c_void = std::ptr::null_mut();
        let hr = SHGetPropertyStoreForWindow(hwnd as Hwnd, &IID_IPROPERTY_STORE, &mut store);
        if hr < 0 || store.is_null() {
            return;
        }

        // IPropertyStore vtable: 0 QI, 1 AddRef, 2 Release, 3 GetCount, 4 GetAt, 5 GetValue, 6 SetValue, 7 Commit
        let vtbl = *(store as *const *const usize);
        type SetValueFn = unsafe extern "system" fn(
            this: *mut core::ffi::c_void,
            key: *const PropertyKey,
            value: *const PropVariant,
        ) -> Hresult;
        type CommitFn = unsafe extern "system" fn(this: *mut core::ffi::c_void) -> Hresult;
        type ReleaseFn = unsafe extern "system" fn(this: *mut core::ffi::c_void) -> u32;
        let set_value: SetValueFn = std::mem::transmute(*vtbl.add(6));
        let commit: CommitFn = std::mem::transmute(*vtbl.add(7));
        let release: ReleaseFn = std::mem::transmute(*vtbl.add(2));

        let set_string = |key: &PropertyKey, value: &str| {
            let mut wide: Vec<u16> = std::ffi::OsStr::new(value)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let pv = PropVariant {
                vt: VT_LPWSTR,
                r1: 0,
                r2: 0,
                r3: 0,
                data: wide.as_mut_ptr() as usize,
            };
            let hr = set_value(store, key, &pv);
            drop(wide);
            hr
        };

        let ico = window_ico.to_string_lossy();
        let _ = set_string(&PKEY_AUMID, CLAUDE_BASILISKOS_AUMID);
        let _ = set_string(&PKEY_RELAUNCH_NAME, "Basiliskos Claude");
        let _ = set_string(&PKEY_RELAUNCH_ICON, ico.as_ref());
        let _ = commit(store);
        let _ = release(store);
    }
}

#[cfg(target_os = "windows")]
fn log_icon_line(message: &str) {
    if let Ok(profile) = isolated_claude_profile_dir() {
        let log_dir = profile.join("Basiliskos Logs");
        let _ = secure_create_dir_all(&log_dir);
        let path = log_dir.join("icon-apply.log");
        if let Ok(mut file) = fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(file, "{}", message);
        }
    }
}

/// Reliable distinction for Store Electron: rename the window and set a taskbar
/// overlay badge. Full package-icon replacement is often ignored by MSIX/Electron.
#[cfg(target_os = "windows")]
fn apply_window_title(hwnd: isize, title: &str) {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::UI::WindowsAndMessaging::SetWindowTextW;

    let wide: Vec<u16> = std::ffi::OsStr::new(title)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        let _ = SetWindowTextW(hwnd as windows_sys::Win32::Foundation::HWND, wide.as_ptr());
    }
}

#[cfg(target_os = "windows")]
fn apply_taskbar_overlay(hwnd: isize, small_icon: isize) {
    #[repr(C)]
    struct Guid {
        data1: u32,
        data2: u16,
        data3: u16,
        data4: [u8; 8],
    }

    type Hresult = i32;
    type Hwnd = *mut core::ffi::c_void;

    #[link(name = "ole32")]
    extern "system" {
        fn CoCreateInstance(
            rclsid: *const Guid,
            punkouter: *mut core::ffi::c_void,
            dwclscontext: u32,
            riid: *const Guid,
            ppv: *mut *mut core::ffi::c_void,
        ) -> Hresult;
    }

    const CLSCTX_INPROC_SERVER: u32 = 0x1;
    // CLSID_TaskbarList
    const CLSID_TASKBAR_LIST: Guid = Guid {
        data1: 0x56FDF344,
        data2: 0xFD6D,
        data3: 0x11D0,
        data4: [0x95, 0x8A, 0x00, 0x60, 0x97, 0xC9, 0xA0, 0x90],
    };
    // IID_ITaskbarList3
    const IID_ITASKBAR_LIST3: Guid = Guid {
        data1: 0xEA1AFB91,
        data2: 0x9E28,
        data3: 0x4B86,
        data4: [0x90, 0xE9, 0x9E, 0x9F, 0x8A, 0x5E, 0xEF, 0xAF],
    };

    unsafe {
        let Some(_com) = ComApartment::initialize() else {
            return;
        };
        let mut obj: *mut core::ffi::c_void = std::ptr::null_mut();
        let hr = CoCreateInstance(
            &CLSID_TASKBAR_LIST,
            std::ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            &IID_ITASKBAR_LIST3,
            &mut obj,
        );
        if hr < 0 || obj.is_null() {
            return;
        }
        // ITaskbarList3 vtable: HrInit=3, SetOverlayIcon=18
        let vtbl = *(obj as *const *const usize);
        type HrInitFn = unsafe extern "system" fn(this: *mut core::ffi::c_void) -> Hresult;
        type SetOverlayIconFn = unsafe extern "system" fn(
            this: *mut core::ffi::c_void,
            hwnd: Hwnd,
            hicon: isize,
            description: *const u16,
        ) -> Hresult;
        type ReleaseFn = unsafe extern "system" fn(this: *mut core::ffi::c_void) -> u32;
        let hr_init: HrInitFn = std::mem::transmute(*vtbl.add(3));
        let set_overlay: SetOverlayIconFn = std::mem::transmute(*vtbl.add(18));
        let release: ReleaseFn = std::mem::transmute(*vtbl.add(2));
        let _ = hr_init(obj);
        let desc: Vec<u16> = "Basiliskos\0".encode_utf16().collect();
        let _ = set_overlay(obj, hwnd as Hwnd, small_icon, desc.as_ptr());
        let _ = release(obj);
    }
}

#[cfg(target_os = "windows")]
fn apply_claude_window_icons(
    pid: u32,
    window_ico: &Path,
    small: &OwnedIcon,
    big: &OwnedIcon,
) -> usize {
    let hwnds = enum_claude_hwnds_for_pid(pid);
    let mut applied = 0_usize;
    for info in &hwnds {
        // Keep the Electron tray host on the inverted tray icon path, not window black.
        if info.class_name.contains("NotifyIcon") {
            continue;
        }
        apply_icons_to_hwnd(info.hwnd, small.0, big.0);
        if info.visible {
            apply_basiliskos_aumid(info.hwnd, window_ico);
            apply_window_title(info.hwnd, "Basiliskos Claude");
            apply_taskbar_overlay(info.hwnd, small.0);
        }
        applied += 1;
    }
    if applied > 0 {
        log_icon_line(&format!(
            "window icons/title/overlay applied pid={pid} count={applied} ico={}",
            window_ico.display()
        ));
    }
    applied
}

/// Best-effort tray recolor: target Electron_NotifyIconHostWindow for our PID.
/// Shell_NotifyIcon is private to the registering app — class-icon overwrite is the
/// least-harmful external approach and may still leave stock tray imagery.
#[cfg(target_os = "windows")]
fn try_apply_tray_icon_for_pid(
    pid: u32,
    tray_ico: &Path,
    small: &OwnedIcon,
    big: &OwnedIcon,
) -> bool {
    let hwnds = enum_claude_hwnds_for_pid(pid);
    let mut applied = false;
    for info in hwnds {
        if info.class_name.contains("NotifyIcon")
            || (!info.visible && info.class_name.contains("Chrome_WidgetWin_0"))
        {
            apply_icons_to_hwnd(info.hwnd, small.0, big.0);
            applied = true;
        }
    }
    if applied {
        log_icon_line(&format!(
            "tray host icons applied pid={pid} ico={}",
            tray_ico.display()
        ));
    }
    applied
}

#[cfg(target_os = "windows")]
fn spawn_claude_icon_reapply(pid: u32, window_ico: PathBuf, tray_ico: PathBuf) {
    thread::spawn(move || {
        log_icon_line(&format!(
            "icon reapply start pid={pid} window={} tray={}",
            window_ico.display(),
            tray_ico.display()
        ));
        let Ok((window_small, window_big)) = load_hicons(&window_ico) else {
            log_icon_line("window icon load failed; cosmetic customization skipped");
            return;
        };
        let tray_icons = if tray_ico.is_file() {
            load_hicons(&tray_ico).ok()
        } else {
            None
        };
        let mut consecutive_hits = 0_u32;
        // Keep the owned HICON values alive for exactly the isolated process lifetime.
        // Electron can reset its class icons after paint or focus, so reassert at a low
        // cadence after the initial startup window.
        for attempt in 0_u32.. {
            if attempt > 0 {
                thread::sleep(if attempt < 60 {
                    Duration::from_millis(500)
                } else {
                    Duration::from_secs(5)
                });
            }
            // Stop if the process is gone.
            if !process_alive(pid) {
                log_icon_line(&format!("icon reapply stop pid={pid} process exited"));
                return;
            }
            let touched = apply_claude_window_icons(pid, &window_ico, &window_small, &window_big);
            if let Some((tray_small, tray_big)) = tray_icons.as_ref() {
                let _ = try_apply_tray_icon_for_pid(pid, &tray_ico, tray_small, tray_big);
            }
            if touched > 0 {
                consecutive_hits = consecutive_hits.saturating_add(1);
            }
        }
        log_icon_line(&format!(
            "icon reapply end pid={pid} hits={consecutive_hits}"
        ));
    });
}

#[cfg(target_os = "windows")]
fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
    };

    unsafe {
        let handle = OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE,
            0,
            pid,
        );
        if handle.is_null() {
            return false;
        }
        let status = WaitForSingleObject(handle, 0);
        let _ = CloseHandle(handle);
        status == WAIT_TIMEOUT
    }
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
        command
            .env("CLAUDE_USER_DATA_DIR", &profile)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
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
                "Basiliskos Claude exited during startup. Check ~/.hydra-gateway/claude-profile/Basiliskos Logs."
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

#[cfg(test)]
mod tests {
    use super::*;

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
                    "limit_window_seconds": 18000
                },
                "secondary_window": {
                    "used_percent": 44.0,
                    "limit_window_seconds": 604800
                }
            }
        }));
        assert_eq!(codex[0], usage_window("5h", 12.0));
        assert_eq!(codex[1], usage_window("Week", 44.0));

        let xai = parse_xai_usage(&serde_json::json!({
            "config": {
                "creditUsagePercent": 23.0,
                "currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY"}
            }
        }));
        assert_eq!(xai[0], usage_window("Week", 23.0));
        assert!(parse_xai_usage(&serde_json::json!({
            "config": {"currentPeriod": {"type": "USAGE_PERIOD_TYPE_WEEKLY"}}
        }))
        .is_empty());
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
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
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
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
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
            Some("claude-sonnet-4-5")
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
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
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
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
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
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
        };
        state.routes.insert(
            "xai".into(),
            RouteSelection {
                model: "grok-4.5".into(),
                thinking: "high".into(),
            },
        );
        let mut request = serde_json::json!({
            "model": "claude-sonnet-4-5",
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
    fn account_selection_enables_one_and_disables_the_rest() {
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
        let accounts = vec![
            GatewayAccount {
                file_name: "codex-a.json".into(),
                provider: "codex".into(),
                email: None,
                label: "Codex".into(),
                disabled: false,
                active: false,
            },
            GatewayAccount {
                file_name: "xai-b.json".into(),
                provider: "xai".into(),
                email: None,
                label: "Grok".into(),
                disabled: false,
                active: false,
            },
        ];
        let state_path = root.join("controller.json");
        let state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
        };
        fs::write(&state_path, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
        let (mutations, _) =
            selection_transaction(&root, &auth, &state_path, &accounts, &state, "xai-b.json")
                .unwrap();
        run_transaction(&root, &mutations, || {
            validate_account_invariant(&auth, &state_path)
        })
        .unwrap();
        let codex: Value =
            serde_json::from_str(&fs::read_to_string(auth.join("codex-a.json")).unwrap()).unwrap();
        let grok: Value =
            serde_json::from_str(&fs::read_to_string(auth.join("xai-b.json")).unwrap()).unwrap();
        assert_eq!(codex.get("disabled").and_then(Value::as_bool), Some(true));
        assert_eq!(grok.get("disabled").and_then(Value::as_bool), Some(false));
        let selected: ControllerState =
            serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(selected.active_account.as_deref(), Some("xai-b.json"));
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
                routes: default_routes(),
                claude_window_icon: default_claude_window_icon(),
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
                },
                GatewayAccount {
                    file_name: "xai-b.json".into(),
                    provider: "xai".into(),
                    email: None,
                    label: "Grok".into(),
                    disabled: true,
                    active: false,
                },
            ];
            let (mutations, _) =
                selection_transaction(&root, &auth, &state_path, &accounts, &state, "xai-b.json")
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
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
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
            },
            GatewayAccount {
                file_name: "xai-b.json".into(),
                provider: "xai".into(),
                email: None,
                label: "Grok".into(),
                disabled: true,
                active: false,
            },
        ];
        (auth, state_path, labels_path, accounts, state, labels)
    }

    #[test]
    fn active_account_removal_disables_every_remaining_account() {
        let root = temp_dir("active-removal");
        let (auth, state_path, labels_path, accounts, state, labels) =
            active_removal_fixture(&root);
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
        let remaining: Value =
            serde_json::from_slice(&fs::read(auth.join("xai-b.json")).unwrap()).unwrap();
        assert_eq!(remaining["disabled"], true);
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
        for fail_after in 0..4 {
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
            assert_eq!(mutations.len(), 4);
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
        assert!(extract_login_url("codex", "about:blank").is_none());
        assert!(extract_login_url(
            "codex",
            "https://auth.openai.com.evil.example/oauth/authorize"
        )
        .is_none());
        assert!(extract_login_url("codex", "http://auth.openai.com/oauth/authorize").is_none());
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
    fn old_controller_state_migrates_to_known_good_routes() {
        let state: ControllerState = serde_json::from_str(
            r#"{"api_key":"secret","claude_config_id":"id","active_account":null}"#,
        )
        .unwrap();
        assert_eq!(
            normalized_route(&state, "claude").model,
            "claude-sonnet-4-5-20250929"
        );
        assert_eq!(normalized_route(&state, "codex").model, "gpt-5.5");
        assert_eq!(normalized_route(&state, "xai").model, "grok-build-0.1");
        assert_eq!(normalized_route(&state, "xai").thinking, "auto");
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
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
        };
        state.routes.insert(
            "xai".into(),
            RouteSelection {
                model: "grok-4.5".into(),
                thinking: "high".into(),
            },
        );
        let mut request = serde_json::json!({"model": "claude-sonnet-4-5"});
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
    fn relay_faults_have_stable_upstream_classifications() {
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
    fn invalid_or_unsupported_route_values_fall_back_safely() {
        let mut state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            routes: default_routes(),
            claude_window_icon: default_claude_window_icon(),
        };
        state.routes.insert(
            "xai".into(),
            RouteSelection {
                model: "grok-build-0.1".into(),
                thinking: "high".into(),
            },
        );
        assert_eq!(normalized_route(&state, "xai").thinking, "auto");
        state.routes.insert(
            "codex".into(),
            RouteSelection {
                model: "made-up-model".into(),
                thinking: "ultra".into(),
            },
        );
        assert_eq!(normalized_route(&state, "codex").model, "gpt-5.5");
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
    fn new_provider_login_never_overwrites_an_unrelated_filename_collision() {
        let directory = temp_dir("login-collision");
        fs::write(directory.join("codex.json"), b"existing credential").unwrap();
        let destination = new_credential_destination_name(&directory, "codex.json").unwrap();
        assert_ne!(destination, "codex.json");
        assert!(destination.starts_with("codex-"));
        assert!(destination.ends_with(".json"));
        assert!(new_credential_destination_name(&directory, "..\\escape.json").is_err());
        assert_eq!(
            fs::read(directory.join("codex.json")).unwrap(),
            b"existing credential"
        );
        let _ = fs::remove_dir_all(directory);
    }
}
