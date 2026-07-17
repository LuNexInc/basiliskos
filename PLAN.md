# Basiliskos plan

## Product contract

Charles's normal Claude Code Desktop remains untouched. Basiliskos is a small
local Windows controller that launches a second, isolated Claude Desktop process
and changes which authorized backend and account serves only that client:

```text
Basiliskos-owned Claude Code Desktop profile
        |
        v
local Anthropic-compatible gateway
        |
        +-- Claude (existing Claude account)
        +-- Codex (selected OAuth account)
        +-- Grok Build (selected OAuth account)
        `-- Kimi Code (selected OAuth account)
```

The switching UI should feel like Basiliskos: provider tabs, account cards, usage or
health status when available, and one explicit **Use this account** action.

## Decisions already made

- Project folder: `hydra-gateway`
- Windows product name: `Basiliskos`
- Application identifier: `com.threereadylab.hydragateway`
- Local state folder: `~/.hydra-gateway`
- The existing `grok-hydra` project stays independent and unchanged.
- Claude Code Desktop is not patched or replaced.
- The normal Claude Desktop process and `%LOCALAPPDATA%\Claude-3p` profile are
  never used as Basiliskos's client.
- Provider credentials stay local and are never committed or logged.

## Implemented architecture

1. **Desktop controller** — the existing Tauri shell owns provider/account UI,
   gateway lifecycle, configuration, and diagnostics.
2. **Gateway engine** — CLIProxyAPI v7.2.83 runs as a verified child process.
   Its release archive and executable hashes are pinned; its repository is not
   vendored. Pinned for Kimi K3 registry support. Upstream issue #4339
   (v7.2.73+ xAI `x_search` injection vs client `web_search`) remains open —
   re-test Grok web_search after upgrades.
3. **Provider accounts** — Claude, Codex, xAI, and Kimi use the engine's official
   provider login flags and a common local account-file model.
4. **Claude Code isolation** — launch the installed app executable with its own
   `CLAUDE_USER_DATA_DIR` under `~/.hydra-gateway/claude-profile`. The installed
   app code explicitly honors this boundary and Electron allows both profiles
   to run simultaneously.
5. **Model aliases** — stable Claude-facing names map to Claude Sonnet 4.5,
   Codex `gpt-5.5`, or `grok-build-0.1`, depending on the active account.

## Milestones and stop points

### 1. Gateway proof of concept — implemented

- [x] Run a pinned gateway binary locally without installing it system-wide.
- [x] Generate the gateway config only inside Basiliskos's isolated Claude profile.
- [x] Exercise the real gateway's authenticated and unauthenticated model API.
- [ ] Complete interactive Codex, Grok, and Claude OAuth approvals and prompts.
- Record request/response compatibility gaps without recording credentials or
  prompt content.

**Stop for approval and a live Claude Code check.**

### 2. Provider and account model — implemented

- [x] Discover provider-neutral account records from private local auth files.
- [x] Add Claude, Codex, Grok, and Kimi OAuth login launchers.
- [x] Enable one account and disable the others atomically on switch.
- [x] Track the active provider/account locally.

**Stop for approval and account-switch tests.**

### 3. Basiliskos-style controller UI — implemented

- [x] Add provider tabs and account cards.
- [x] Add gateway start/stop and connection health.
- [x] Add isolated Basiliskos Claude open/close actions without targeting the normal
  Claude app or Claude Code CLI.
- [x] Keep keys and advanced gateway details out of the main UI.

**Stop for UI review before packaging.**

### 4. Windows packaging — implemented locally

- [x] Acquire and bundle the pinned gateway dependency with checksum checks.
- [x] Build one canonical per-machine NSIS installer with a `3ReadyLab` Start
  menu and a fixed `C:\Program Files\3ReadyLab\Basiliskos` default.
- [x] Keep user profiles under `~/.hydra-gateway`, outside program files.

**Stop before publishing or installing a release build.**

### 5. Basiliskos route and thinking controls — implemented locally in 1.0.3

- [x] Brand the isolated controller as **Basiliskos** while keeping the real route
  visible, for example `Basiliskos · Grok 4.5` or `Basiliskos · GPT-5.6 Terra`; the
  assistant itself reports the actual route directly.
- [x] Add a model selector to the Basiliskos controller from the pinned runtime's
  curated safe text/tool catalog; exclude image/video models and
  Claude-facing aliases. The catalog is pinned because several OAuth routes
  work even when the runtime's model-list endpoint omits them.
- [x] Add a thinking selector whose options come from the selected model's
  declared capabilities. Use **Auto** for provider/app defaults, expose only
  supported levels such as Low / Medium / High / XHigh / Max, and disable the
  control for models such as `grok-build-0.1` that advertise no thinking
  setting.
- [x] Persist the last model and thinking choice per provider. Validate stored
  values on load and fall back to the existing known-good routes: Claude
  Sonnet 4.5, Codex GPT-5.5, Grok Build 0.1, and Kimi K3 with Auto
  thinking.
- [x] Regenerate the hot-reloadable OAuth alias so every
  Claude-facing model ID routes to the selected upstream model. A route change
  affects the next request and must not terminate either Claude process.
- [x] Show the authoritative route in Basiliskos as `Basiliskos -> provider/model ->
  account`, including the effective thinking level. Update Claude's custom
  model label when its isolated profile reloads; if Claude does not hot-reload
  that label, expose an explicit **Reopen Basiliskos** action instead of silently
  restarting it.
- [x] Add state-migration, config-generation, capability-filtering, and
  unsupported-thinking regression tests.
- [x] Append a route-aware Basiliskos identity instruction after protocol
  translation so Claude, Codex, Grok, and Kimi backends do not mistake the
  Claude-compatible client alias for their actual identity. Keep the real
  upstream model visible in both the instruction and model chip. The Basiliskos
  loopback front proxy now rewrites the valid Claude-shaped request to the
  selected provider's actual upstream model ID and appends the Basiliskos identity
  before CLIProxyAPI forwards it (1.0.9).
- [ ] After installation approval, run one minimal live request for each
  configured provider/model combination Charles has authenticated.

### 6. Kimi Code OAuth — implemented locally

- [x] Add Kimi as a provider using the bundled CLIProxyAPI 7.2.83
  `-kimi-login` device-authorization flow.
- [x] Validate only `https://auth.kimi.com/` authorization URLs, preserve the
  one-time device code, and wait until the runtime reports it is ready before
  opening the browser.
- [x] Use the same staged, cancellable, transactionally committed credential
  path as the existing providers.
- [x] Add Kimi's pinned coding-model catalog, route identity, account UI, and
  availability fallback. Catalog now defaults to `kimi-k3` (1.1.13) and keeps
  the prior K2.x coding models selectable.
- [x] Allow the Tauri opener to open only validated Kimi device-login hosts
  (`https://auth.kimi.com/*`, `https://www.kimi.com/*`) so Add account can open
  the browser; if open fails, surface the trusted URL for manual copy (1.1.13).
- [x] Poll official Kimi Code usage (`GET /coding/v1/usages`) and surface
  weekly/rolling windows when the account is entitled.
- [x] Map Kimi usage and upstream model `402`/`403` responses to a clear
  "no active Kimi Code subscription" message instead of a re-auth prompt.
- [x] Live Kimi OAuth device authorization was completed successfully; accounts
  without Kimi Code entitlement still cannot list models or usage until the
  subscription is activated.
- [ ] With an entitled Kimi Code account, send one minimal live request and
  confirm usage windows render. Do not automate approval pages or record
  credential values.

Implementation note: CLIProxyAPI v7.2.83 supports hot-reloaded
`oauth-model-alias` mappings and payload overrides such as
`reasoning.effort`. Its pinned catalog currently advertises Codex reasoning
levels per model; Grok 4.5, Grok 4.3, Grok 3 Mini/Fast, and Grok 4.20 Multi
Agent support selectable effort, while Grok Build 0.1 and Composer 2.5 Fast do
not. Claude budget-only models require the gateway's level-to-budget mapping;
adaptive Claude models accept named effort levels.

**Stop for approval before installing 1.0.3 or changing the live route.**

## Explicit non-goals

- No modification or redistribution of Claude Code Desktop.
- No automated interaction with OAuth approval pages.
- No sharing, pooling, or rotation of accounts the user does not own or control.
- No claim that provider limits are bypassed; switching only changes the
  selected authorized backend/account.
- No public release until provider terms, licenses, and credential storage have
  been reviewed.
