use chrono::Utc;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpStream, ToSocketAddrs},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{mpsc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};
use tauri::{AppHandle, Manager};
use tiny_http::{Header, Response, Server, StatusCode};
use uuid::Uuid;

const GATEWAY_VERSION: &str = "7.2.72";
const GATEWAY_EXE_SHA256: &str = "4ab5e372f8cea947af9a07820f962a07e42aeafb56508f73fd9ab129533e88bc";
const GATEWAY_PORT: u16 = 8317;
const BACKEND_PORT: u16 = 8318;
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

static GATEWAY_CHILD: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
static CLAUDE_CHILD: OnceLock<Mutex<Option<Child>>> = OnceLock::new();
static FRONT_PROXY: OnceLock<Mutex<Option<FrontProxy>>> = OnceLock::new();

struct FrontProxy {
    shutdown: mpsc::Sender<()>,
    thread: thread::JoinHandle<()>,
}

fn gateway_child() -> &'static Mutex<Option<Child>> {
    GATEWAY_CHILD.get_or_init(|| Mutex::new(None))
}

fn claude_child() -> &'static Mutex<Option<Child>> {
    CLAUDE_CHILD.get_or_init(|| Mutex::new(None))
}

fn front_proxy() -> &'static Mutex<Option<FrontProxy>> {
    FRONT_PROXY.get_or_init(|| Mutex::new(None))
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
    pub authorization_url: String,
    pub user_code: Option<String>,
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
    let model = provider
        .filter(|provider| SUPPORTED_PROVIDERS.contains(provider))
        .map(|provider| provider_route(state, provider).selected_model_label)
        .unwrap_or_else(|| "Choose a route".into());
    model
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

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }
    let temp = path.with_extension(format!("tmp-{}", Uuid::new_v4()));
    fs::write(&temp, bytes)
        .map_err(|error| format!("Could not write {}: {error}", temp.display()))?;
    if path.exists() {
        fs::remove_file(path)
            .map_err(|error| format!("Could not replace {}: {error}", path.display()))?;
    }
    fs::rename(&temp, path)
        .map_err(|error| format!("Could not finalize {}: {error}", path.display()))
}

fn load_state() -> Result<ControllerState, String> {
    let path = controller_path()?;
    if path.exists() {
        let raw = fs::read_to_string(&path)
            .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
        return serde_json::from_str(&raw)
            .map_err(|error| format!("Basiliskos controller state is invalid: {error}"));
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
    };
    save_state(&state)?;
    Ok(state)
}

fn save_state(state: &ControllerState) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(state)
        .map_err(|error| format!("Could not serialize controller state: {error}"))?;
    atomic_write(&controller_path()?, &bytes)
}

fn load_account_labels() -> Result<BTreeMap<String, String>, String> {
    let path = account_labels_path()?;
    if !path.exists() {
        return Ok(BTreeMap::new());
    }
    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    serde_json::from_str(&raw)
        .map_err(|error| format!("Basiliskos profile names are invalid: {error}"))
}

fn save_account_labels(labels: &BTreeMap<String, String>) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(labels)
        .map_err(|error| format!("Could not serialize profile names: {error}"))?;
    atomic_write(&account_labels_path()?, &bytes)
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
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }
    fs::copy(&source, &destination)
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
    fs::create_dir_all(&auth)
        .map_err(|error| format!("Could not create {}: {error}", auth.display()))?;
    atomic_write(&config_path()?, render_config(&auth, &state).as_bytes())?;
    Ok(state)
}

fn health_check(api_key: &str) -> bool {
    let address = ("127.0.0.1", GATEWAY_PORT)
        .to_socket_addrs()
        .ok()
        .and_then(|mut addresses| addresses.next());
    let Some(address) = address else { return false };
    let Ok(mut stream) = TcpStream::connect_timeout(&address, Duration::from_millis(300)) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
    let request = format!(
        "GET /hydra/health HTTP/1.1\r\nHost: 127.0.0.1\r\nx-api-key: {api_key}\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }
    let mut response = String::new();
    stream.read_to_string(&mut response).is_ok()
        && (response.starts_with("HTTP/1.1 200") || response.starts_with("HTTP/1.0 200"))
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

fn proxy_error(message: &str) -> Response<std::io::Cursor<Vec<u8>>> {
    let body = serde_json::json!({
        "type": "error",
        "error": {"type": "api_error", "message": message}
    })
    .to_string()
    .into_bytes();
    let mut response = Response::from_data(body).with_status_code(StatusCode(502));
    if let Ok(header) = Header::from_bytes("content-type", "application/json") {
        response.add_header(header);
    }
    response
}

fn handle_front_proxy_request(mut request: tiny_http::Request, client: &reqwest::blocking::Client) {
    let request_url = request.url().to_string();
    let request_path = request_url
        .split('?')
        .next()
        .unwrap_or(request_url.as_str());
    let method = match reqwest::Method::from_bytes(request.method().as_str().as_bytes()) {
        Ok(method) => method,
        Err(error) => {
            let _ = request.respond(proxy_error(&format!("Unsupported request method: {error}")));
            return;
        }
    };
    let mut body = Vec::new();
    if let Err(error) = request.as_reader().read_to_end(&mut body) {
        let _ = request.respond(proxy_error(&format!(
            "Could not read request body: {error}"
        )));
        return;
    }

    if request_path == "/hydra/health" {
        let mut response = Response::from_string(
            serde_json::json!({"hydra": true, "version": env!("CARGO_PKG_VERSION")}).to_string(),
        );
        if let Ok(header) = Header::from_bytes("content-type", "application/json") {
            response.add_header(header);
        }
        let _ = request.respond(response);
        return;
    }

    if request_path == "/v1/messages" || request_path == "/v1/messages/count_tokens" {
        let rewrite_result = (|| -> Result<(), String> {
            let state = load_state()?;
            let provider = active_provider_from_auth(&auth_dir()?, &state)
                .ok_or_else(|| "Choose an active Basiliskos account first".to_string())?;
            let mut json: Value = serde_json::from_slice(&body)
                .map_err(|error| format!("Claude request body is invalid JSON: {error}"))?;
            rewrite_claude_request(&mut json, &state, &provider, request_path == "/v1/messages")?;
            body = serde_json::to_vec(&json).map_err(|error| error.to_string())?;
            Ok(())
        })();
        if let Err(error) = rewrite_result {
            let _ = request.respond(proxy_error(&error));
            return;
        }
    }

    let upstream_url = format!("http://127.0.0.1:{BACKEND_PORT}{request_url}");
    let mut builder = client.request(method, upstream_url);
    for header in request.headers() {
        let name = header.field.as_str().as_str();
        if !is_hop_by_hop_header(name) {
            builder = builder.header(name, header.value.as_str());
        }
    }
    let upstream = match builder.body(body).send() {
        Ok(response) => response,
        Err(error) => {
            let _ = request.respond(proxy_error(&format!(
                "Gateway backend unavailable: {error}"
            )));
            return;
        }
    };
    let status = StatusCode(upstream.status().as_u16());
    let headers = upstream
        .headers()
        .iter()
        .filter_map(|(name, value)| {
            if is_hop_by_hop_header(name.as_str()) {
                return None;
            }
            Header::from_bytes(name.as_str(), value.as_bytes()).ok()
        })
        .collect();
    let response = Response::new(status, headers, upstream, None, None);
    let _ = request.respond(response);
}

fn start_front_proxy() -> Result<(), String> {
    let server = Server::http(("127.0.0.1", GATEWAY_PORT))
        .map_err(|error| format!("Could not start Basiliskos compatibility proxy: {error}"))?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| format!("Could not create Basiliskos proxy client: {error}"))?;
    let (shutdown_tx, shutdown_rx) = mpsc::channel();
    let proxy_thread = thread::spawn(move || loop {
        if shutdown_rx.try_recv().is_ok() {
            break;
        }
        match server.recv_timeout(Duration::from_millis(150)) {
            Ok(Some(request)) => handle_front_proxy_request(request, &client),
            Ok(None) => {}
            Err(_) => break,
        }
    });
    *front_proxy()
        .lock()
        .map_err(|_| "Basiliskos proxy state is locked")? = Some(FrontProxy {
        shutdown: shutdown_tx,
        thread: proxy_thread,
    });
    Ok(())
}

#[cfg(target_os = "windows")]
fn hidden(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
fn hidden(_command: &mut Command) {}

pub fn stop_gateway_internal() {
    stop_hydra_claude_internal();
    if let Ok(mut guard) = front_proxy().lock() {
        if let Some(proxy) = guard.take() {
            let _ = proxy.shutdown.send(());
            let _ = proxy.thread.join();
        }
    }
    if let Ok(mut guard) = gateway_child().lock() {
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn stop_hydra_claude_internal() {
    if let Ok(mut guard) = claude_child().lock() {
        if let Some(mut child) = guard.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn hydra_claude_running() -> bool {
    let Ok(mut guard) = claude_child().lock() else {
        return false;
    };
    let Some(child) = guard.as_mut() else {
        return false;
    };
    match child.try_wait() {
        Ok(None) => true,
        Ok(Some(_)) | Err(_) => {
            *guard = None;
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
    let state = prepare_config()?;
    if health_check(&state.api_key) {
        return gateway_snapshot();
    }
    stop_gateway_internal();
    let executable = prepare_runtime(&app)?;
    let log_dir = gateway_dir()?.join("controller-logs");
    fs::create_dir_all(&log_dir)
        .map_err(|error| format!("Could not create {}: {error}", log_dir.display()))?;
    let stdout = fs::File::create(log_dir.join("gateway.stdout.log"))
        .map_err(|error| format!("Could not create gateway log: {error}"))?;
    let stderr = fs::File::create(log_dir.join("gateway.stderr.log"))
        .map_err(|error| format!("Could not create gateway log: {error}"))?;
    let mut command = Command::new(executable);
    command
        .args(["-config", &config_path()?.to_string_lossy(), "-local-model"])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    hidden(&mut command);
    let child = command
        .spawn()
        .map_err(|error| format!("Could not start Basiliskos: {error}"))?;
    *gateway_child()
        .lock()
        .map_err(|_| "Gateway state is locked")? = Some(child);
    if let Err(error) = start_front_proxy() {
        stop_gateway_internal();
        return Err(error);
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if health_check(&state.api_key) {
            return gateway_snapshot();
        }
        std::thread::sleep(Duration::from_millis(150));
    }
    stop_gateway_internal();
    Err("Basiliskos did not become ready. Check ~/.hydra-gateway/gateway/controller-logs.".into())
}

#[tauri::command]
pub fn stop_gateway() -> Result<GatewaySnapshot, String> {
    stop_gateway_internal();
    gateway_snapshot()
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
    fs::create_dir_all(&directory)
        .map_err(|error| format!("Could not create {}: {error}", directory.display()))?;
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
    let mut state = load_state()?;
    restore_legacy_shared_config_if_needed(&mut state)?;
    let routes = SUPPORTED_PROVIDERS
        .iter()
        .map(|provider| provider_route(&state, provider))
        .collect();
    Ok(GatewaySnapshot {
        running: gateway_running(),
        base_url: format!("http://127.0.0.1:{GATEWAY_PORT}"),
        version: GATEWAY_VERSION.into(),
        claude_running: hydra_claude_running(),
        accounts: list_accounts_inner(&state)?,
        active_account: state.active_account,
        routes,
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
    let token = nested_string(&value, &["access_token"]).ok_or("Sign in again to refresh usage")?;
    let account_id = nested_string(&value, &["account_id"]);
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
    gateway_snapshot()
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

fn set_disabled(path: &Path, disabled: bool) -> Result<(), String> {
    let raw = fs::read_to_string(path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let mut value: Value = serde_json::from_str(&raw)
        .map_err(|error| format!("Account file {} is invalid: {error}", path.display()))?;
    let object = value
        .as_object_mut()
        .ok_or("Account file must contain a JSON object")?;
    object.insert("disabled".into(), Value::Bool(disabled));
    let bytes = serde_json::to_vec_pretty(&value)
        .map_err(|error| format!("Could not serialize account: {error}"))?;
    atomic_write(path, &bytes)
}

fn select_account_files(
    directory: &Path,
    accounts: &[GatewayAccount],
    file_name: &str,
) -> Result<(), String> {
    for account in accounts {
        set_disabled(
            &directory.join(&account.file_name),
            account.file_name != file_name,
        )?;
    }
    Ok(())
}

#[tauri::command]
pub fn select_gateway_account(file_name: String) -> Result<GatewaySnapshot, String> {
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
    select_account_files(&auth_dir()?, &accounts, &file_name)?;
    let mut state = state;
    state.active_account = Some(file_name);
    save_state(&state)?;
    prepare_config()?;
    write_isolated_claude_config(&isolated_claude_profile_dir()?, &state)?;
    gateway_snapshot()
}

#[tauri::command]
pub fn set_gateway_route(
    provider: String,
    model: String,
    thinking: String,
) -> Result<GatewaySnapshot, String> {
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
    state
        .routes
        .insert(provider.clone(), RouteSelection { model, thinking });
    save_state(&state)?;
    prepare_config()?;
    if list_accounts_inner(&state)?
        .iter()
        .any(|account| account.active && account.provider == provider)
    {
        write_isolated_claude_config(&isolated_claude_profile_dir()?, &state)?;
    }
    gateway_snapshot()
}

#[tauri::command]
pub fn remove_gateway_account(file_name: String) -> Result<GatewaySnapshot, String> {
    let path = exact_auth_path(&file_name)?;
    let state = load_state()?;
    if !list_accounts_inner(&state)?
        .iter()
        .any(|account| account.file_name == file_name)
    {
        return Err("Account not found".into());
    }
    fs::remove_file(&path)
        .map_err(|error| format!("Could not remove {}: {error}", path.display()))?;
    let mut labels = load_account_labels()?;
    if labels.remove(&file_name).is_some() {
        save_account_labels(&labels)?;
    }
    let mut state = state;
    if state.active_account.as_deref() == Some(file_name.as_str()) {
        state.active_account = None;
        save_state(&state)?;
    }
    gateway_snapshot()
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
    prepare_config()?;
    let executable = prepare_runtime(&app)?;
    let mut command = Command::new(executable);
    command
        .args([
            flag,
            "-no-browser",
            "-config",
            &config_path()?.to_string_lossy(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    hidden(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not start {provider} login: {error}"))?;

    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| format!("Could not read {provider} login output"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| format!("Could not read {provider} login errors"))?;
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
                let _ = child.kill();
                let _ = child.wait();
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
                    return Ok(ProviderLoginLaunch {
                        authorization_url: authorization_url.expect("checked above"),
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
                let _ = child.kill();
                let _ = child.wait();
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
    let daily = backup_root.join(Utc::now().format("%Y-%m-%d").to_string());
    if daily.exists() {
        return Ok(());
    }
    fs::create_dir_all(&backup_root)
        .map_err(|error| format!("Could not create {}: {error}", backup_root.display()))?;
    let staging = backup_root.join(format!(".tmp-{}", Uuid::new_v4().simple()));
    for path in changed {
        let relative = path
            .strip_prefix(profile)
            .map_err(|_| format!("Refusing to back up a config outside {}", profile.display()))?;
        let destination = staging.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        fs::copy(path, &destination).map_err(|error| {
            format!(
                "Could not back up {} to {}: {error}",
                path.display(),
                destination.display()
            )
        })?;
    }
    match fs::rename(&staging, &daily) {
        Ok(()) => Ok(()),
        Err(_) if daily.exists() => Ok(()),
        Err(error) => Err(format!(
            "Could not finalize Claude config backup {}: {error}",
            daily.display()
        )),
    }
}

fn write_if_changed(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if fs::read(path).ok().as_deref() == Some(bytes) {
        return Ok(());
    }
    atomic_write(path, bytes)
}

fn write_isolated_claude_config(profile: &Path, state: &ControllerState) -> Result<(), String> {
    let library = profile.join("configLibrary");
    fs::create_dir_all(&library)
        .map_err(|error| format!("Could not create {}: {error}", library.display()))?;
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
    for (path, bytes) in writes {
        write_if_changed(&path, &bytes)?;
    }
    Ok(())
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
    atomic_write(
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

#[tauri::command]
pub fn launch_hydra_claude(app: AppHandle) -> Result<GatewaySnapshot, String> {
    #[cfg(target_os = "windows")]
    {
        if !gateway_running() {
            start_gateway(app)?;
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
        fs::create_dir_all(&log_dir)
            .map_err(|error| format!("Could not create {}: {error}", log_dir.display()))?;
        if hydra_claude_running() {
            return gateway_snapshot();
        }
        let stdout = fs::File::create(log_dir.join("launcher.stdout.log"))
            .map_err(|error| format!("Could not create the Basiliskos Claude log: {error}"))?;
        let stderr = fs::File::create(log_dir.join("launcher.stderr.log"))
            .map_err(|error| format!("Could not create the Basiliskos Claude log: {error}"))?;
        let mut command = Command::new(executable);
        command
            .env("CLAUDE_USER_DATA_DIR", &profile)
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        hidden(&mut command);
        let child = command.spawn().map_err(|error| {
            format!("Could not open the isolated Basiliskos Claude window: {error}")
        })?;
        *claude_child()
            .lock()
            .map_err(|_| "Basiliskos Claude process state is locked")? = Some(child);
        std::thread::sleep(Duration::from_millis(900));
        if !hydra_claude_running() {
            return Err(
                "Basiliskos Claude exited during startup. Check ~/.hydra-gateway/claude-profile/Basiliskos Logs."
                    .into(),
            );
        }
        return gateway_snapshot();
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = app;
        Err("The isolated Basiliskos Claude window is available on Windows only".into())
    }
}

#[tauri::command]
pub fn stop_hydra_claude() -> Result<GatewaySnapshot, String> {
    stop_hydra_claude_internal();
    gateway_snapshot()
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
        };
        let config = render_config(&auth, &state);
        assert!(config.contains("host: \"127.0.0.1\""));
        assert!(config.contains("port: 8318"));
        assert!(config.contains("disable-control-panel: true"));
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
    fn account_selection_enables_one_and_disables_the_rest() {
        let root = temp_dir("accounts");
        fs::write(
            root.join("codex-a.json"),
            r#"{"type":"codex","disabled":false}"#,
        )
        .unwrap();
        fs::write(
            root.join("xai-b.json"),
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
        select_account_files(&root, &accounts, "xai-b.json").unwrap();
        let codex: Value =
            serde_json::from_str(&fs::read_to_string(root.join("codex-a.json")).unwrap()).unwrap();
        let grok: Value =
            serde_json::from_str(&fs::read_to_string(root.join("xai-b.json")).unwrap()).unwrap();
        assert_eq!(codex.get("disabled").and_then(Value::as_bool), Some(true));
        assert_eq!(grok.get("disabled").and_then(Value::as_bool), Some(false));
        fs::remove_dir_all(root).unwrap();
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
    fn invalid_or_unsupported_route_values_fall_back_safely() {
        let mut state = ControllerState {
            api_key: "secret".into(),
            claude_config_id: "id".into(),
            previous_claude_applied_id: None,
            active_account: None,
            routes: default_routes(),
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
}
