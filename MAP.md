# MAP — hydra-gateway (Basiliskos)

> Navigation index: where things live / what to open first. NOT behavior docs (that's AGENTS.md).
> Size cap ~45 lines. Update in the same commit that moves/renames a module.
> Last verified: 2026-08-16

## Open first
- `AGENTS.md` — Basiliskos contract + the **canonical-vs-publish-repo** rules. Read before ANY work here.
- `src-tauri/src/gateway.rs` — the `gateway::` Tauri command surface (the app's core). Its allowlist is `scripts/check-command-surface.ps1`.
- `src/App.tsx` — the whole React UI (single page) **and** the hardcoded `APP_VERSION`.
- Run: `pnpm install` → `pnpm tauri dev`. Release gate: `pnpm test:all` (must be green before shipping).

## UI (React + Vite, `src/`)
- `App.tsx` (UI + APP_VERSION) · `TrayDashboard.tsx` (tray popup) · `ui.ts` (shared helpers for both) · `main.tsx` (entry) · `App.css` · `App.test.tsx` · `LoginFlow.test.tsx` (login no-switch flow) · `TrayDashboard.test.tsx` (tray preview render) · `test-setup.ts`.

## Rust backend (Tauri, `src-tauri/src/`)
- `gateway.rs` — command surface / provider routing (OAuth + API-key, `:8317` front proxy, model rewrite + identity injection, credential refresh, update). Still the biggest file.
- `catalog.rs` — provider catalog (ModelSpec + per-provider lists), `Provider` × `Auth` kinds, route defaults, model→Anthropic alias map, live model fetch.
- `usage.rs` — provider quota-window parsing (Claude/Codex/xAI/Kimi) + `GatewayUsageWindow`/`GatewayAccountUsage` types.
- `vision.rs` — per-provider tool-compatibility fixups (Kimi tool_reference flattening, xAI web_search strip).
- `claude_window.rs`, `codex_window.rs` — Win32 window icon, title, and close-watcher management.
- `codex_cli.rs`, `grok_cli.rs` — provider CLI integration · `codex_switcher_import.rs` — import accounts from the codex switcher.
- `persistence.rs` — local state/credential store · `diagnostics.rs` — diagnostics · `test_support.rs` — test helpers.
- `lib.rs` / `main.rs` — Tauri app entry + command registration.
- API-key accounts live in `~/.hydra-gateway/gateway/keys/` (`kind: "api_key"`); OAuth accounts live in `~/.hydra-gateway/gateway/auth/`. OpenCodex was dropped in 3.0.

## Release & security scripts (`scripts/`)
- `check-command-surface.ps1` (gateway command allowlist — update when adding/removing a `gateway::` command) · `check-cliproxy.ps1` (pinned CLIProxyAPI version/SHA consistency + upstream-release notice) · `check-installer-contract.ps1` · `check-runtime-log-secrets.ps1` · `check-sensitive-data.ps1` · `generate-release-evidence.ps1` · `sign-command.ps1` · `test-gateway.ps1` · `test-installer-lifecycle.ps1` · `tauri-build.ps1`.
- CI: `.github/workflows/release-gate.yml` — pushing a `v<version>` tag runs build → installer-lifecycle → publish.

## Version lives in 5 spots (bump ALL on release)
`package.json` · `src-tauri/Cargo.toml` · `src-tauri/Cargo.lock` · `src-tauri/tauri.conf.json` · `src/App.tsx` (`APP_VERSION`).

## Specs / docs
`PLAN.md` · `CLEAN_ROOM_SPEC.md` · `docs/PROVIDER-BACKEND-ADAPTERS.md` · `docs/RELEASE-CHECKLIST.md` · `docs/OAUTH-VAULT-SECURITY-REVIEW.md`.

## Don't touch
`outputs/` (dated session scratch) · `node_modules/`, `dist/` (build) · `.agents/`, `.claude/` (dev-only — excluded from the release-repo sync).
