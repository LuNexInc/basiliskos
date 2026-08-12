# Serving the Codex App from Basiliskos — Deep Research (2026-08-12)

> Research only. No implementation was done in this session. This document answers:
> how the ChatGPT/Codex desktop app and the Grok apps do multi-model routing, what
> a local gateway must provide to serve the Codex app, what Basiliskos already has,
> and what must be built. Sources are listed at the end. Every claim is backed by
> code on this PC, official docs, or the openai/codex source on GitHub.

## 1. Executive summary

- **The "Codex app" on this PC is the ChatGPT desktop app** (MSIX package
  `OpenAI.Codex_2p2nqsd0c76g0`, version 26.803.10989.0). It is a closed Chromium
  shell (`ChatGPT.exe` + `Codex.exe`) that embeds the real `codex` CLI binary and
  runs a local `app-server` daemon. It reads the SAME `~/.codex/config.toml` as
  the CLI (proven by LiteLLM's official tutorial and by openai/codex issues).
- **Codex officially supports custom model providers** via
  `model_providers.<id>` in `~/.codex/config.toml` (`base_url`, `wire_api`,
  `env_key`/`experimental_bearer_token`/command auth, headers, retries). The
  **only** wire protocol is the OpenAI **Responses API** (`wire_api = "responses"`
  — `chat/completions` was removed in early February 2026).
- **The desktop app has NO UI for custom providers.** The model is whatever
  `model = "..."` says in config.toml at session creation; it is persisted per
  session in `~/.codex/state_5.sqlite` (`threads.model`) and cannot be changed
  from the app UI (openai/codex#15364, closed "not enough upvotes"). To change
  the route model you rewrite config.toml and start a new session.
- **Basiliskos can serve the Codex app today's-style with a config.toml
  injection**, the same shape cc-switch (126k stars) and LiteLLM document and
  ship: `model_provider = "basiliskos"` +
  `[model_providers.basiliskos] base_url = "http://127.0.0.1:8317/v1"`,
  `wire_api = "responses"`, auth via bearer/env key. Basiliskos's front proxy
  already accepts `Authorization: Bearer <hydra key>` and already relays all
  paths (including `/v1/responses`) to CLIProxyAPI, which advertises an OpenAI
  (incl. Responses) client surface with WebSocket support.
- **Two gaps must be closed before it works well:** (1) the Codex model picker
  needs the internal ModelInfo catalog — either serve the codex-internal
  `/models` schema or generate a `model_catalog_json` file; (2) the front proxy
  currently rewrites only `/v1/messages` (the Claude dial); a Codex dial
  (model → active route rewrite on `/v1/responses`) was built in July 2026
  (commit `5ccd39b6b`) and later removed — it is prior art in this repo's git
  history.
- **Grok side:** the Grok **CLI** (Grok Build 1.0.0) has first-class custom
  model endpoints (`[model.<name>]`, `GROK_MODELS_BASE_URL`,
  `[endpoints] models_base_url`, three wire backends incl. Anthropic
  `messages`), so it can be pointed at Basiliskos with config only. The Grok
  **Bot** desktop app is closed (no custom endpoint surface; its "BYOK" strings
  are an enterprise model-allowlist protobuf). Charles's own `grokulator`
  hosts the Grok CLI via ACP and inherits whatever the CLI supports.
- **Blocking rule:** Basiliskos `AGENTS.md` says "Do NOT rewrite
  `~/.codex/config.toml` for this product" (a gate for the OpenCodex scaffold).
  Serving the Codex app requires writing that file (or the app's CODEX_HOME),
  so this needs Charles's explicit approval before any implementation.

## 2. What the Codex app actually is (analysis of this PC)

### 2.1 Installed components

| Component | Location | Role |
|---|---|---|
| ChatGPT desktop app (MSIX) | `C:\Program Files\WindowsApps\OpenAI.Codex_26.803.10989.0_x64__2p2nqsd0c76g0` | Chromium 151 shell: `ChatGPT.exe`, `Codex.exe`, `chrome.dll`, `resources/app.asar` (226 MB), bundled `codex.exe` (293 MB) |
| Updater | `%LOCALAPPDATA%\openai-codex-electron-updater` | Electron updater |
| App data | `%LOCALAPPDATA%\Packages\OpenAI.Codex_2p2nqsd0c76g0\LocalCache\Local\Codex\Logs` | `codex-desktop-*.log` — shows Electron main + app-server protocol calls |
| Shared config | `~/.codex/config.toml`, `~/.codex/auth.json`, `~/.codex/.codex-global-state.json` | Shared with the CLI |
| CLI | `%LOCALAPPDATA%\OpenAI\Codex\bin\8e8bf206e63ac436\codex.exe` | `codex-cli 0.147.0-alpha.6.6` (2026-08) |

### 2.2 How the app talks to the engine

From the desktop logs and the asar (extracted and grepped):

- The app is an Electron shell whose renderer (the ChatGPT webview) talks to a
  local **app-server** daemon over a JSON-RPC protocol. Observed methods:
  `config/read`, `configRequirements/read`, `getAuthStatus`, `account/read`,
  `remoteControl/enable`, `experimentalFeature/list`, `model/list`,
  `modelProvider/capabilities/read`, `thread/list`, `permissionProfile/list`,
  `plugin/installed`, `plugin/read`.
- `codex app-server` runs the same `codex-config` crate as the CLI
  (`ConfigLayerSource::User { file, profile }`), so **user-level
  `~/.codex/config.toml` is honored by the app** — including
  `model_provider` / `model_providers`. This is confirmed by LiteLLM's official
  tutorial ("The Codex desktop app reads the same `~/.codex/config.toml` as the
  CLI... works for the app with no extra setup") and by real user configs in
  openai/codex issues (e.g. #32417 runs the app against `127.0.0.1:15721/v1`).
- The renderer builds per-thread config that can include a
  `model_providers.<id>` block with `base_url`, `experimental_bearer_token`,
  `wire_api: "responses"` — currently used only for the built-in GitHub
  Copilot provider (`codex_vscode_copilot`), which proves the app-server honors
  the mechanism end to end.
- The app's model picker (built-in OpenAI models only) is fed by `model/list`
  → app-server `models` → the codex model catalog. For a custom provider the
  catalog fetch hits `<base_url>/models` (codex-rs `models_endpoint.rs`,
  `MODELS_ENDPOINT = "/models"`), which expects the **internal ModelInfo
  schema** — not the standard OpenAI list (see §4.3).

## 3. How Codex does multi-model routing today

### 3.1 Built-in providers

The ChatGPT account path (default) routes through OpenAI's backend
(`chatgpt.com` / `api.openai.com` / Responses API). The model catalog comes
from OpenAI's servers. The app's model picker only shows these.

### 3.2 Custom model providers (the official contract)

From `developers.openai.com/codex/config-advanced` and the config reference
(`learn.chatgpt.com/docs/config-file/config-reference`), plus the exact Rust
types in `codex-rs/model-provider-info/src/lib.rs` (fetched from
openai/codex@main):

```toml
model = "gpt-5.6-terra"
model_provider = "proxy"
model_reasoning_effort = "high"

[model_providers.proxy]
name = "OpenAI using LLM proxy"
base_url = "http://proxy.example.com/v1"     # or without /v1; codex appends /responses
wire_api = "responses"                        # ONLY "responses" (chat removed Feb 2026)
env_key = "OPENAI_API_KEY"                    # env var for Authorization: Bearer
# experimental_bearer_token = "sk-..."        # direct token (discouraged, works)
# [model_providers.proxy.auth]                # command-backed token fetch
#   command = "/usr/local/bin/fetch-token"
#   refresh_interval_ms = 300000
# requires_openai_auth = true                 # use auth.json OPENAI_API_KEY instead of env_key
# query_params = { api-version = "..." }      # Azure-style
# http_headers = { "X-Tenant" = "..." }
# env_http_headers = { "X-Tenant" = "TENANT" }
# request_max_retries = 4
# stream_max_retries = 5
# stream_idle_timeout_ms = 300000
# supports_websockets = true                  # Responses WS transport (HTTP/SSE works without it)
# supports_standalone_web_search = true
```

Reserved IDs that cannot be overridden: `openai`, `ollama`, `lmstudio`
(plus built-in `amazon-bedrock`). `wire_api` enum in current code has exactly
one variant: `Responses` — and deserializing `"chat"` raises
`CHAT_WIRE_API_REMOVED_ERROR` pointing at discussion 7782.

Alternative simple hook: `openai_base_url = "http://127.0.0.1:8317/v1"` in
config.toml redirects the built-in `openai` provider (the CLI honors it; the
env var `OPENAI_BASE_URL` is ignored by Codex — LiteLLM's docs say so
explicitly). Do NOT touch `chatgpt_base_url`: in codex-rs it is "Base URL for
requests to ChatGPT (as opposed to the OpenAI API)" — the app's cloud/backend
traffic — not the model inference path.

### 3.3 Desktop-app quirks (all confirmed in openai/codex issues)

| Quirk | Issue | Consequence for Basiliskos |
|---|---|---|
| No UI to change model for a custom provider | #15364 (closed, "not enough upvotes") | Route model = `model` in config.toml at session creation |
| Session model persisted in `state_5.sqlite threads.model`; old sessions keep old model/creds | #15364, #29160 | After a config change, start a NEW session; consider prompting the user |
| `env_key` resolved from the app's environment | LiteLLM tutorial | On Windows MSIX, user-level env vars are visible; `experimental_bearer_token` avoids env entirely |
| `/models` discovery expects internal ModelInfo schema; standard OpenAI list silently fails | #37122 | Serve internal schema or ship `model_catalog_json` |
| Desktop GUI thread context-window/profile quirks | #14133 | Prefer a dedicated provider + explicit `model_context_window` |
| Custom provider + app UI prefs bug | #32417 | Cosmetic; config.toml still works |
| Multi-agent v2 needs Responses + provider capability | #37858, #37859 | Some app features gated for custom providers |
| Project-local config cannot set provider keys | config-advanced | Must write USER-level `~/.codex/config.toml` |

### 3.4 Model picker / catalog

- `model_catalog_json = "path.json"` loads a local catalog that **replaces** the
  bundled catalog for the process (codex-rs `core/src/config/mod.rs`:
  `model_catalog: Option<ModelsResponse>`). The file must deserialize as a
  `ModelsResponse` = `{ "object": "list", "data": [ ModelInfo, ... ] }` with
  the internal `ModelInfo` schema (openai_models.rs, ~50 fields incl.
  `slug`, `display_name`, `supported_reasoning_levels`, `shell_type`,
  `base_instructions`, `truncation_policy`, `supports_parallel_tool_calls`,
  `apply_patch_tool_type`, `web_search_tool_type`, ...). Every field without a
  serde default is load-bearing (#37122).
- Practical approach used by cc-switch users: vendor a known-good catalog and
  override slugs/display names for the route models.

## 4. The gateway wire requirements (what Basiliskos must speak)

Codex (CLI and app) with a custom provider sends:

1. `POST {base_url}/responses` — OpenAI Responses API, streaming SSE, with
   `model`, `instructions`, `input`, `tools`, `reasoning`, `store: false`
   (typical), `stream: true`. This is the ONLY inference path.
2. `GET {base_url}/models` — optional model discovery (internal schema).
3. `Authorization: Bearer <token>` — from `env_key` / `experimental_bearer_token`
   / auth.json (`requires_openai_auth`).
4. Optional WebSocket transport if `supports_websockets = true` (Codex Desktop
   0.130+ upgrades one WS per conversation — VallierDev/codex-switcher notes;
   HTTP/SSE works when the flag is false, per LiteLLM).

Basiliskos today:

- Front proxy `127.0.0.1:8317` accepts **both** `x-api-key` and
  `Authorization: Bearer` (`request_is_authorized` in gateway.rs) — the Codex
  auth path already works.
- It relays every path to CLIProxyAPI on `127.0.0.1:8318` and rewrites only
  `/v1/messages` + `/v1/messages/count_tokens` (the Claude dial:
  `rewrite_claude_request`, vision lane, context budget).
- `/v1/models` currently returns the standard OpenAI list of the ACTIVE
  account (probed live: Kimi catalog) — fine for OpenAI-compatible clients,
  wrong for the Codex picker (needs ModelInfo or `model_catalog_json`).
- CLIProxyAPI 7.2.128 (pinned, audited): "OpenAI (including Responses) ...
  or Claude-compatible client or SDK" surface; README_CN lists "streaming,
  non-streaming, and WebSocket responses in supported scenarios", OpenAI Codex
  (GPT series) OAuth support, Codex multi-account polling, and a `codex:`
  config section (`disable-codex-cloaking`, `optimize-multi-agent-v2` for
  "Codex Desktop, codex-tui, and codex_cli_rs", live-media relay). So the
  inference translation layer already exists in the pinned dependency.

## 5. Basiliskos today + prior art

### 5.1 Current state

- One dial: the **Claude** client is served via `claude_desktop_config.json`
  (`deploymentMode: "3p"`, `inferenceGatewayBaseUrl:
  http://127.0.0.1:8317`, `inferenceGatewayApiKey`, x-api-key auth,
  `inferenceModels` = active route's real upstream model ids, model
  discovery on). This is the official Claude Desktop 3rd-party gateway
  deployment mode.
- Codex CLI integration today = credential-file switcher only (`codex_cli.rs`:
  swap `~/.codex/auth.json`; never touches config.toml). Grok CLI = credential
  switcher (`grok_cli.rs`: swap `~/.grok/auth.json`).
- OpenCodex scaffold: docs/spec only (`docs/OPENCODEX-SCAFFOLD.md`); explicit
  non-goals: "No Responses-API ↔ Anthropic/Gemini protocol translation yet",
  "No Codex App injection". AGENTS.md gate: never rewrite `~/.codex/config.toml`.

### 5.2 Prior art in git history (July 2026)

Commit `5ccd39b6b` "Add independent Basiliskos Claude and Codex client dials"
(2026-07-23) built exactly the Codex-serving path, then a later auto-handoff
commit reverted it. From the commit + handoff
`handoff/2026-07-23-0054-grok-basiliskos-dual-client.md`:

- `ClientSurface { Claude, Codex }`; `active_claude_account` /
  `active_codex_account`; `claude_routes` / `codex_routes`; legacy migration.
- Front proxy rewrote `/v1/messages` with the **Claude dial** and
  `/v1/responses` + `/v1/chat/completions` with the **Codex dial**.
- Isolated Codex: `CODEX_HOME=~/.hydra-gateway/codex-profile`,
  `launch_hydra_codex` / `stop_hydra_codex` — never touches global `~/.codex`.
- UI: Claude / Codex client tabs with independent "Use-for-client" buttons and
  model pickers.
- Known at the time: "Full desktop end-to-end open of Codex App not run this
  session"; next step was "tighten Codex config (wire_api/provider mapping) if
  Codex App rejects `model_providers.basiliskos`".

The on-disk `~/.hydra-gateway/codex-profile/config.toml` (still present,
dated 2026-07-23) is exactly the provider block this research validates:

```toml
model = "gpt-5.5"
model_provider = "basiliskos"
model_reasoning_effort = "high"

[model_providers.basiliskos]
name = "Basiliskos"
base_url = "http://127.0.0.1:8317/v1"
env_key = "BASILISKOS_API_KEY"
wire_api = "responses"
```

### 5.3 Why an isolated CODEX_HOME does not reach the app

The MSIX desktop app reads the real `~/.codex` (its app-server is spawned with
the normal user environment; `CODEX_HOME` is not set for the app). An isolated
`CODEX_HOME` profile only affects CLI processes Basiliskos itself spawns. To
serve the **app**, Basiliskos must write the real `~/.codex/config.toml`
(and coordinate with the account-switcher's ownership of `~/.codex/auth.json`
if `requires_openai_auth` is used).

## 6. Community precedent — proven, shipped implementations

### 6.1 LiteLLM (official tutorial, docs.litellm.ai/docs/tutorials/openai_codex)

```toml
model = "gpt-5.6-terra"
model_provider = "litellm"
[model_providers.litellm]
name = "litellm"
base_url = "http://localhost:4000/v1"
env_key = "LITELLM_API_KEY"
wire_api = "responses"
stream_idle_timeout_ms = 7200000
stream_max_retries = 5
request_max_retries = 4
```

States the desktop app works with the same config. Caveats: env_key must exist
in the app's environment; no UI model switch; new session after config change.
LiteLLM's `lite` CLI launches Codex with `-c` provider overrides (HTTP/SSE
Responses; LiteLLM does not speak the Responses WebSocket).

### 6.2 cc-switch (farion1231/cc-switch, 126k stars — the industry standard)

"All-in-One for Claude Code, Claude Desktop, Codex, Gemini CLI, Grok Build,
OpenCode, OpenClaw, Hermes." Features directly relevant to Basiliskos:

- **Codex Chat Completions routing**: routes Chat-only providers (DeepSeek,
  Kimi, GLM, MiniMax) into Codex — the proxy converts Chat Completions to the
  Responses protocol automatically ("Needs Local Routing" toggle + model
  mapping table).
- **App-level takeover**: independent local proxy per app, down to individual
  providers; hot-switching, failover, circuit breaker, health checks.
- **Codex OAuth reverse proxy**: reuse a ChatGPT account's Codex service inside
  Claude Code.
- Codex config format it writes (from the user manual):

```toml
model_provider = "custom"
model = "gpt-5.2"
model_reasoning_effort = "high"
disable_response_storage = true
[model_providers.custom]
name = "custom"
base_url = "https://api.example.com/v1"
wire_api = "responses"
requires_openai_auth = true
```
plus `~/.codex/auth.json` = `{"OPENAI_API_KEY": "..."}`.
- Switching notes: Claude takes effect immediately; **Codex requires a
  terminal/app restart**; per-app isolation.
- `/goal` mode with custom providers needs `[features] goals = true`.

### 6.3 VallierDev/codex-switcher (Codex App multi-account + local proxy)

Tauri desktop tool (like Basiliskos) that:
- runs a local HTTP/WebSocket proxy the Codex CLI/App points at;
- does **lossless account switching** at the proxy layer (401/429/quota →
  switch account + replay, client unaware);
- translates Codex `/v1/responses` into Chat Completions for GLM/MiMo/DeepSeek
  relay accounts (model mapping `gpt-* → deepseek-chat`, etc.);
- normalizes upstream errors to `context_length_exceeded` so Codex auto-compacts;
- per-conversation session routing (kicks WS with `ws_disconnect` to rebind);
- "phone anchor" for Codex.app 26.513+ mobile remote: the mobile bridge binds
  `auth.json`'s `chatgpt_account_id`, so the disk auth is pinned while the
  proxy exit switches accounts;
- notes Codex Desktop 0.130+ upgrades one WebSocket per conversation and the
  route check runs at WS-upgrade time only.

### 6.4 Others

- Lampese/codex-switcher (619⭐): auth.json account switching + quota/usage.
- Loongphy/codex-auth (2463⭐): CLI account switch/manage.
- Bashar94/codex-cli-account-switcher (83⭐), hloolx/codex-proxy-switcher-win
  (70⭐).
- CLIProxyAPI ecosystem: CPA-Manager, quota dashboard tools — the relay product
  Basiliskos already pins.

### 6.5 Claude Code reference (what Basiliskos already implements)

Official "Run Claude Code through a gateway" (code.claude.com/docs/en/gateways):
developer credential to the gateway (`ANTHROPIC_AUTH_TOKEN`), provider
credential held by the gateway; gateway decides where requests go, developer
picks the model; `ANTHROPIC_BASE_URL` alone keeps the claude.ai subscription
login. Basiliskos's Claude Desktop integration (`deploymentMode: "3p"`) is the
first-party desktop equivalent. Codex's contract is the same shape but the
model choice is config.toml-driven (no UI).

## 7. The Grok side

### 7.1 Grok CLI (Grok Build 1.0.0 — installed on this PC)

Full custom-endpoint support, documented in the installed
`~/.grok/docs/user-guide/11-custom-models.md`:

```toml
[model.basiliskos]
model = "grok-4.5"
base_url = "http://127.0.0.1:8317/v1"     # any OpenAI-compatible endpoint
api_key = "..."                            # or env_key / extra_headers
api_backend = "chat_completions"           # chat_completions | responses | messages (Anthropic!)
context_window = 500000
```

- Global override: `[endpoints] models_base_url = "..."` or env
  `GROK_MODELS_BASE_URL` (+ `GROK_MODELS_LIST_URL`) — points the WHOLE CLI at a
  custom `/v1/models` endpoint. Setting it switches auth from session to
  `Authorization: Bearer <api key>`.
- Other env vars (docs.x.ai/build/settings/reference): `GROK_DEFAULT_MODEL`,
  `GROK_WEB_SEARCH_MODEL`, `GROK_XAI_API_BASE_URL` (default
  `https://api.x.ai/v1`), `GROK_HOME` (config home override — note: Basiliskos's
  `grok_cli.rs` comment claims no GROK_HOME exists; xAI docs say
  `~/.grok/config.toml` **or** `$GROK_HOME/config.toml`).
- Model catalog is fetched per model with its own `base_url` (verified in
  `~/.grok/models_cache.json`: `grok-4.5 → https://cli-chat-proxy.grok.com/v1`,
  `api_backend: responses`, `auth_scheme: bearer`).
- `grok models` (probed live) shows only `grok-4.5` — the catalog is
  server-fetched; custom models appear once configured.
- Priority: `[model.*]` config > prefetched `/v1/models` > hardcoded defaults.

**Consequence:** Basiliskos can serve the Grok CLI with a config-file write
only (no code changes to the CLI). The gateway must speak chat_completions or
responses and serve `/models`; the Grok CLI's own per-model `base_url` makes
the wiring explicit.

### 7.2 Grok Bot desktop app (xAI's "Grok Bot", installed)

Closed consumer Electron app (191 MB app.asar). Extracted bundles
(`host-main.cjs`, `electron-main/main.cjs`, renderer) show:
- `GROK_*` strings are internal enums (`BackgroundComposerSource.GROK_BOT`);
- `base_url` / `openai_api_base_url` hits are protobuf field schemas;
- "BYOK" is an enterprise model-allowlist message (`_ModelAllowlistByok`);
- localhost/127.0.0.1 are internal services (VNC, exec daemon, agent store,
  MCP OAuth loopback).
**No user-facing custom-endpoint surface.** The consumer Grok chat app cannot
be pointed at a gateway.

### 7.3 grokulator (Charles's own Grok Build host)

Tauri desktop host for the Grok CLI via ACP (`grok agent stdio`), Grok-only
scope, models "Grok 4.5 and Composer 2.5 Fast". It inherits whatever the CLI
supports — so any Basiliskos↔Grok CLI wiring automatically benefits
grokulator (e.g., `/model` switching via ACP `session/set_model`).

## 8. Recommended architecture (options, for later approval)

### Option A — Config.toml injection + Responses relay via the existing stack (recommended)

1. Basiliskos writes (with Charles's approval, replacing the AGENTS.md gate).
   **Merge, never replace:** the app itself writes `[desktop]` (theme,
   followUpQueueMode, avatar...), `[plugins.*]`, `[mcp_servers.*]`, `[windows]`,
   `[features]`, `[marketplaces.*]` into the same file — a blind overwrite
   breaks app settings. Preserve unknown keys (TOML round-trip or
   append-only provider block). Also verify against the APP'S bundled
   `codex.exe` (26.803.10989) as well as the installed CLI (0.147.0-alpha.6.6):
   the app runs its own binary, and version drift changes what config it
   accepts.
   - `~/.codex/config.toml`: `model = "<route model>"`,
     `model_provider = "basiliskos"`,
     `[model_providers.basiliskos] name = "Basiliskos"`,
     `base_url = "http://127.0.0.1:8317/v1"`, `wire_api = "responses"`,
     auth via `experimental_bearer_token = "<hydra key>"` (no env-var
     dependency in the MSIX app) — or `env_key = "BASILISKOS_API_KEY"` with a
     user-level env var; plus a generated `model_catalog_json` (ModelsResponse
     with one ModelInfo per route model) so the picker and /models resolve.
   - Optionally `[features] goals = true` for /goal mode parity.
2. Re-add the Codex dial to the front proxy (prior art in `5ccd39b6b`):
   rewrite `model` on `/v1/responses` to the active route's model, honor the
   client-chosen model like the Claude path does today, apply the same
   fixups (kimi flatten, deepseek placeholder, xai strip) on the Responses
   shape, and keep `/v1/messages` (Claude dial) untouched.
3. Verify CLIProxyAPI's Responses→upstream translation live for each provider
   (claude/grok/kimi/deepseek), including streaming SSE and error
   normalization (`context_length_exceeded`).
4. Switching model = rewrite `model` in config.toml + tell the user to start a
   new session (or restart the app). Note existing sessions pin their model in
   `state_5.sqlite` (#29160).
5. Keep the account-switcher (`codex_cli.rs` owns `~/.codex/auth.json`) as the
   account layer; use bearer-token auth (not `requires_openai_auth`) to avoid
   two writers on auth.json.

### Option B — CLI-only (isolated CODEX_HOME) — does NOT reach the app

Restores the July `launch_hydra_codex` dial for CLI windows. Cheapest, but the
desktop app keeps its own config. Good as a stepping stone / smoke harness.

### Option C — app-server integration (research-grade)

The app-server protocol has `config/read` but no public `config/write`-for-
providers surface; the renderer writes provider blocks only for Copilot.
Intercepting the app-server socket or patching the MSIX app is against the
project's clean-room boundaries. Not recommended.

### Option D — Grok CLI serving (independent, low effort)

Write `~/.grok/config.toml` `[model.*]` entries (or `[endpoints]
models_base_url` + env) pointing at the Basiliskos gateway, with the CLI's
own per-model `base_url`. Reuse the same relay; the CLI speaks
chat_completions/responses to the gateway. No code changes to the CLI.

## 9. Verification plan (do NOT run until Charles approves implementation)

1. Probe `GET 127.0.0.1:8317/v1/models` shape vs codex `ModelsResponse`
   expectations (already probed: standard OpenAI list today).
2. Send a minimal `/v1/responses` request to the front proxy with
   `Authorization: Bearer <hydra key>` per route provider and confirm
   CLIProxyAPI translates + streams (throwaway temp auth dir, mirroring the
   existing `outputs/smoke-7.2.128.py` harness).
3. With a throwaway CODEX_HOME, launch `codex exec -c model=...` against
   `base_url = http://127.0.0.1:8317/v1` per provider; then repeat through the
   real app with a copied config.toml and a new session.
4. Validate a generated `model_catalog_json` against `codex doctor` /
   `codex --config model_catalog_json=...` parsing.
5. Check WebSocket: keep `supports_websockets` unset (HTTP/SSE) — matches
   LiteLLM; only enable after the WS path is verified.
6. Watch usage/quota truthfulness rules (DECISIONS.md): relayed Codex traffic
   must not corrupt per-provider usage reporting.

## 10. Decision points for Charles

1. **Gate change:** approve Basiliskos writing `~/.codex/config.toml`
   (replaces the OpenCodex-scaffold "do not rewrite" rule) — required for the
   app; isolated CODEX_HOME only reaches the CLI.
2. **Auth style:** `experimental_bearer_token` (zero env setup, works in MSIX)
   vs `env_key` + user env var (cleaner but requires env setup) vs
   `requires_openai_auth` + auth.json (conflicts with the account switcher).
3. **Scope:** Codex app first (Option A), Grok CLI (Option D) in the same or a
   later milestone, or a shared "external client injection" surface for both.
4. **Model switching UX:** rewrite config + require new session (status quo in
   the whole ecosystem) — or add an in-app "restart Codex" prompt.
5. **Catalog:** generate `model_catalog_json` (recommended; works today) vs
   serve the codex-internal `/models` schema from the front proxy (cleaner
   long-term, tracks openai/codex#37122).
6. **Mobile/Remote:** Codex.app mobile remote binds `chatgpt_account_id`
   (VallierDev "phone anchor" wrinkle) — decide whether account switching in
   the app is in scope.

## 11. Constraint checklist (Basiliskos AGENTS.md / DECISIONS.md)

- Official OAuth / audited bridge only — the relay path is unchanged
  (CLIProxyAPI pinned 7.2.128).
- Never rewrite `~/.codex/config.toml` — **currently a hard rule; needs
  Charles's explicit OK for this feature** (gate exists for the OpenCodex
  scaffold; this is a different product mode).
- Command-surface allowlist: new `gateway::` commands must be added to
  `scripts/check-command-surface.ps1`.
- `pnpm test:all` before shipping; milestone approval gate per AGENTS.md.
- Secrets: the hydra front-proxy key is local-only; if `experimental_bearer_token`
  is written into config.toml it must not be logged (check
  `check-runtime-log-secrets.ps1`).
- Don't describe the feature as a quota/restriction bypass (it is a local
  router over accounts the user owns, same as today's Claude dial).

## 12. Sources

Official / OpenAI:
- https://developers.openai.com/codex/config-advanced (openai_base_url,
  model_providers, project-config limits, Bedrock/Azure/OSS)
- https://learn.chatgpt.com/docs/config-file/config-reference (model_providers
  keys, model_catalog_json, [desktop])
- https://learn.chatgpt.com/docs/changelog (Codex joins ChatGPT desktop app
  26.707; custom-provider entries #34846, #36906)
- https://github.com/openai/codex — codex-rs/model-provider-info/src/lib.rs
  (WireApi responses-only), codex-rs/protocol/src/openai_models.rs (ModelInfo,
  ModelsResponse), codex-rs/model-provider/src/models_endpoint.rs (/models),
  codex-rs/app-server/src/config_layer.rs (config layers)
- openai/codex issues: #15364, #29160, #32417, #37122, #14133, #37858, #37859,
  #32349, #21769; discussion #7782 (chat/completions removal)

Community / gateway products:
- https://docs.litellm.ai/docs/tutorials/openai_codex (official Codex-through-
  LiteLLM guide, desktop-app confirmation)
- https://github.com/farion1231/cc-switch (126k★; Codex Chat Completions
  routing, app-level takeover, config format)
- https://github.com/VallierDev/codex-switcher (Codex App local proxy,
  lossless account switch, WS-per-conversation, phone anchor)
- https://github.com/Lampese/codex-switcher · Loongphy/codex-auth ·
  bashar94/codex-cli-account-switcher
- https://github.com/router-for-me/CLIProxyAPI (README/README_CN: OpenAI
  incl. Responses + Claude client surfaces, WebSocket, codex section)
- https://code.claude.com/docs/en/gateways (Claude gateway reference)

Local analysis:
- This PC: MSIX `OpenAI.Codex_26.803.10989.0`, `codex.exe` 0.147.0-alpha.6.6,
  `~/.codex/config.toml`, `~/.codex/.codex-global-state.json`, desktop logs,
  extracted `resources/app.asar` bundles, `~/.grok/config.toml`,
  `~/.grok/docs/user-guide/11-custom-models.md`, `~/.grok/models_cache.json`,
  `~/.grok/bin/grok` 1.0.0, Grok Bot app.asar bundles,
  `~/.hydra-gateway/codex-profile/config.toml`, `controller.json`,
  Basiliskos `gateway.rs` (front proxy auth + relay + /v1/messages rewrite),
  git commit `5ccd39b6b` + handoff 2026-07-23-0054.
