# MAP — hydra-gateway (Basiliskos)

> Navigation index: where things live / what to open first. NOT behavior docs (that's AGENTS.md).
> Size cap ~45 lines. Update in the same commit that moves/renames a module.
> Last verified: 2026-08-11

## Open first
- `AGENTS.md` — Basiliskos contract + the **canonical-vs-publish-repo** rules. Read before ANY work here.
- `src-tauri/src/gateway.rs` — the `gateway::` Tauri command surface (the app's core). Its allowlist is `scripts/check-command-surface.ps1`.
- `src/App.tsx` — the whole React UI (single page) **and** the hardcoded `APP_VERSION`.
- Run: `pnpm install` → `pnpm tauri dev`. Release gate: `pnpm test:all` (must be green before shipping).

## UI (React + Vite, `src/`)
- `App.tsx` (UI + APP_VERSION) · `main.tsx` (entry) · `App.css` · `App.test.tsx` (UI test) · `test-setup.ts`.

## Rust backend (Tauri, `src-tauri/src/`)
- `gateway.rs` — command surface / provider routing (Claude/Codex/Grok/Kimi OAuth + DeepSeek API key).
- `codex_cli.rs`, `grok_cli.rs` — provider CLI integration · `codex_switcher_import.rs` — import accounts from the codex switcher.
- `persistence.rs` — local state/credential store · `diagnostics.rs` — diagnostics · `test_support.rs` — test helpers.
- `lib.rs` / `main.rs` — Tauri app entry + command registration.
- ⚠️ `opencodex.rs` is referenced in AGENTS.md ("OpenCodex catalog") but **is NOT present** — scaffold not landed. Verify before relying on it; see `docs/OPENCODEX-SCAFFOLD.md`.

## Release & security scripts (`scripts/`)
- `check-command-surface.ps1` (gateway command allowlist — update when adding/removing a `gateway::` command) · `check-installer-contract.ps1` · `check-runtime-log-secrets.ps1` · `check-sensitive-data.ps1` · `generate-release-evidence.ps1` · `sign-command.ps1` · `test-gateway.ps1` · `test-installer-lifecycle.ps1` · `tauri-build.ps1`.
- CI: `.github/workflows/release-gate.yml` — pushing a `v<version>` tag runs build → installer-lifecycle → publish.

## Version lives in 5 spots (bump ALL on release)
`package.json` · `src-tauri/Cargo.toml` · `src-tauri/Cargo.lock` · `src-tauri/tauri.conf.json` · `src/App.tsx` (`APP_VERSION`).

## Specs / docs
`PLAN.md` · `CLEAN_ROOM_SPEC.md` · `docs/OPENCODEX-SCAFFOLD.md` · `docs/DEEPSEEK-VISION.md` · `docs/RELEASE-CHECKLIST.md` · `docs/OAUTH-VAULT-SECURITY-REVIEW.md`.

## Don't touch
`outputs/` (dated session scratch) · `node_modules/`, `dist/` (build) · `.agents/`, `.claude/` (dev-only — excluded from the release-repo sync).
