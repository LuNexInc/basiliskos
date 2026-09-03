# Basiliskos

Basiliskos began as a fork of the former `../grok-hydra` codebase. `grok-hydra`
was deleted 2026-07-25 (superseded by Basiliskos; recoverable from git history if
ever needed). This folder is now the sole canonical source — see the "Source of
truth" section below for the dev-vs-publish-repo rules.

The product goal is a small Windows controller that keeps Claude Code Desktop
as the user's working interface while switching its local gateway between
Claude, Codex, Grok, Kimi, Antigravity, and Z.AI GLM accounts (OAuth) and API-key
providers and model routers (DeepSeek, OpenCode Go, OpenRouter, LiteLLM, and
custom OpenAI-compatible endpoints) the user owns or is authorized to use.

## Boundaries

- Implement from documented platform behavior and this project's own specs.
- Do not patch or redistribute Claude Code Desktop binaries.
- Authentication must use official provider OAuth/login flows or an audited
  local bridge that invokes those flows. Never automate login approval pages.
- Treat CLIProxyAPI as a possible internal dependency, not source to copy into
  this repository. Pin and audit any dependency before bundling it.
- Store credentials locally and never log or commit auth contents.
- Do not describe the project as a quota or restriction bypass.
- Keep the project distributable under the MIT license.
- Stop after each plan or discrete milestone and get Charles's approval before
  beginning the next implementation phase.

## Build

```powershell
pnpm install
pnpm build
cargo test --manifest-path src-tauri/Cargo.toml
pnpm tauri build
```

Run `pnpm test:all` before shipping any feature (especially new Tauri
commands or provider integrations). Dev doesn't run the release gate, so
skipping this lets drift pile up and surface only at release time. `test:all`
= build + ui + format + clippy + rust + surface + installer + gateway +
cliproxy + secrets + log-secrets. In particular, adding/removing a `gateway::`
command means updating the `$expected` allowlist in
`scripts/check-command-surface.ps1`. When bumping the pinned CLIProxyAPI
version, update BOTH `scripts/prepare-gateway.ps1` and the `GATEWAY_VERSION` /
`GATEWAY_EXE_SHA256` constants in `src-tauri/src/gateway.rs` (they must match),
re-run `pnpm test:all`, and confirm the config contract test
(`render_config_keeps_the_cliproxy_contract`) plus `pnpm test:cliproxy` are
green.

Follow the root workspace `AGENTS.md` and `HANDOFF.md` protocol.

## Source of truth (read this before ANY Basiliskos work)

**This folder in the `ai-projects` monorepo is canonical.** The release repo
`LuNexInc/basiliskos` (local clone: `../_publish/basiliskos-repo`) is a
publish target only. They are **not** auto-synced, and on 2026-07-24 they
silently diverged (Claude released 2.0.0 from dev while Codex released
2.0.1–2.0.3 from the release repo; neither knew about the other).

Non-negotiable rules for every tool (see also the root `AGENTS.md`
"Orchestration" section — session claims in `.sessions\` and
`.tools\preflight.ps1 -Project hydra-gateway -Tool <you>` automate these checks):

- **Start of any Basiliskos session:** compare versions before touching code —
  `grep version package.json` here vs `gh release list --repo LuNexInc/basiliskos`.
  If the release repo is ahead, **backport to dev FIRST** (copy the changed
  files back, verify `pnpm test:all`, commit) before starting new work.
- **Implement in dev, publish via the release repo** — never implement new
  work directly in the release clone.
- **If you do release:** backport any release-repo-only changes to dev in the
  SAME session, and say so in your handoff (version released + "dev synced").
- **Before tagging:** check `gh run list --repo LuNexInc/basiliskos` for
  in-flight runs and `gh release list` for a version newer than yours —
  another tool may be mid-release. Never assume your version number is free.

## Releasing (public, auto-updates all users)

1. Get Charles's approval on the version number (semver: patch = fixes, minor
   = new features, major = milestone/breaking). Bump it in **all five** spots:
   `package.json`, `src-tauri/Cargo.toml`, `src-tauri/Cargo.lock`,
   `src-tauri/tauri.conf.json`, and the hardcoded `APP_VERSION` in `src/App.tsx`.
2. Run `pnpm test:all` locally until green (fix forward; don't relax security
   gates like the command-surface allowlist or clean-room forbidden list
   without Charles's explicit OK).
3. Sync the dev tree's tracked files (+ any new source files) into the release
   clone, **excluding** dev scratch (`.agents/`, `.claude/`, `outputs/`).
   Push to `LuNexInc/basiliskos` `main` — this does NOT trigger a release.
4. Push a `v<version>` tag → triggers `.github/workflows/release-gate.yml`
   (windows-latest): build → installer-lifecycle → publish-release. Publish
   `needs: [build, installer-lifecycle]`, so nothing ships unless the whole
   gate is green. Watch the run; report the published release or the failure.
   To re-run after a fix on a never-published tag, delete it
   (`git push origin :refs/tags/v<version>`) and re-create on the fixed commit
   (auto-mode blocks `git push -f`).
5. **Write real release notes.** The workflow publishes with `--generate-notes`,
   which only emits a bare changelog link. After the release publishes, replace
   the body with user-facing notes (what's new / changed / upgrade notes;
   flag "Unsigned / Unknown publisher"):
   `gh release edit v<version> --repo LuNexInc/basiliskos --notes-file <file>`.

Installers publish **Unsigned / Unknown publisher** unless
`BASILISKOS_SIGN_CERT_BASE64` / `BASILISKOS_SIGN_CERT_PASSWORD` secrets exist
in the `LuNexInc/basiliskos` repo (workflow already wired for them).

## Provider model (3.0)

Every catalog provider is reached one of two ways via two orthogonal axes:
`Provider` (target) and `Auth` (`oauth` | `api_key`).

- **OAuth providers** (browser login, refreshable token): Claude, Codex, Grok
  (`xai`), Antigravity, Kimi Code, and Z.AI GLM. Claude / Codex / Grok / Kimi /
  Antigravity use CLIProxyAPI's `-<provider>-login`. Z.AI uses the official
  ZCode CLI poll flow because the pinned CLIProxyAPI build has no `-zai-login`.
- **API-key providers** (paste a key + optional endpoint, OpenAI/Anthropic-
  compatible): DeepSeek, Kimi (Moonshot key), OpenCode Go, OpenRouter, LiteLLM,
  and custom endpoints. Any OAuth provider can also be reached with a key.
- The Anthropic picker requires real Anthropic model ids, so non-Claude models
  are advertised under a stable alias and reverse-mapped by the front proxy.
- API keys live in `~/.hydra-gateway/gateway/keys/<provider>-<label>.json` and
  are never logged or committed.

OpenCodex was dropped in 3.0. Do **not** reinstall `@bitkyc08/opencodex` or
rewrite `~/.codex/config.toml` for this product.
