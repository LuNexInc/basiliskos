# DECISIONS — hydra-gateway (Basiliskos)

> What is intentionally settled + why + reverse-if. HANDOFF = history; this = standing state.
> Newest on top. Extracted from AGENTS.md 2026-07-25 — keep in sync when a call changes.

## 2026-08-02 — DeepSeek V4 effort uses adaptive thinking, not numeric budgets
- **Decision:** route DeepSeek V4 through Anthropic adaptive thinking (`thinking.type=adaptive` + `output_config.effort`) so CLIProxyAPI emits the OpenAI-compatible `reasoning_effort`. Flash exposes `low`, `high`, and `max`; Pro exposes `high` and `max` because its upstream maps `low` to `high`.
- **Why:** the previous `thinking.budget_tokens` bridge saturated at `high`, making V4 Flash `max` impossible despite DeepSeek supporting it. DeepSeek sampling settings have no effect while thinking is enabled, so Basiliskos removes `temperature`, `top_p`, and frequency/presence penalties for an explicit thinking route, while leaving them unchanged in auto mode.
- **Provider isolation:** generate the compatibility provider as `basiliskos-deepseek`, not `deepseek`. The selected account remains a `deepseek-*.json` file for Basiliskos' account UI, but CLIProxyAPI otherwise selects that stored file (which has no `base_url`) before the generated compatibility client and rejects requests with `missing provider baseURL`.
- **Thinking off:** advertise `Off` for both V4 models. It sends `thinking.type=disabled` and removes the selected effort, so DeepSeek receives a true non-thinking request and honours caller-provided temperature and sampling settings.
- **Reverse if:** a future CLIProxyAPI release directly supports the DeepSeek provider contract and makes the adaptive bridge unnecessary; re-verify every advertised effort level against the pinned runtime before changing this path.

## 2026-07-24 — This monorepo folder is canonical; `LuNexInc/basiliskos` is publish-only
- **Decision:** implement here; the release repo is a one-way publish target, NOT auto-synced. Before any session, compare versions (`grep version package.json` vs `gh release list --repo LuNexInc/basiliskos`); if the release repo is ahead, backport to dev FIRST.
- **Why:** on 2026-07-24 the two silently diverged — Claude released 2.0.0 from dev while Codex released 2.0.1–2.0.3 from the release repo, neither aware of the other. Cost ~a day.
- **Reverse if:** a real two-way CI sync is built between dev and the release repo (until then, manual backport-first stands). Enforced by `.tools/preflight.ps1 -Project hydra-gateway`.

## 2026-08-01 — DeepSeek ships as a live 5th provider, authorized by API key
- **Decision:** `deepseek` is in `SUPPORTED_PROVIDERS` and routes live. It has no OAuth flow, so it is added via `add_deepseek_account` (paste key → verified against `api.deepseek.com/user/balance` → saved) instead of `launch_provider_login`. The key is stored as a normal `deepseek-*.json` auth file, and is rendered into the backend config's `openai-compatibility` block **only while that account is the selected one**.
- **Why:** Charles approved storing an API key for this (the standing gate below required explicit sign-off). Only the active account's key is rendered because CLIProxyAPI load-balances across every `api-key-entries` entry — emitting all saved keys would silently route through an account the user did not pick.
- **Verified against the pinned 7.2.83 runtime:** the key must sit under `api-key-entries`; `api-keys` parses without error but loads **zero** clients. An unknown-`type` auth file is inert (loads as a client advertising no models), which is why DeepSeek can reuse the existing account machinery safely.
- **Known limitation:** DeepSeek bills a prepaid balance, not a quota window, so `get_gateway_account_usage` returns an explanatory error rather than a fabricated percentage.
- **Reverse if:** CLIProxyAPI gains a first-class DeepSeek provider with its own auth (then drop the compat block), or Charles wants keys kept out of the generated config entirely.

## OpenCodex is scaffold/UI only — live routing stays Claude/Codex/Grok/Kimi
- **Decision:** ship an OpenCodex-shaped catalog + UI tab, but live request routing uses only the four real providers via the pinned CLIProxyAPI. Do NOT reinstall `@bitkyc08/opencodex` or rewrite `~/.codex/config.toml`.
- **Why:** the catalog is a forward-looking surface; enabling live multi-provider routing / storing API keys needs explicit sign-off. *(Superseded for DeepSeek only on 2026-08-01 — see the entry above. The gate still stands for every other catalog entry.)*
- **Reverse if:** Charles approves live catalog routing (see `docs/OPENCODEX-SCAFFOLD.md`). Note: `opencodex.rs` isn't actually in the tree yet — this decision currently describes intent, not shipped code.

## Standing product & legal boundaries (stable)
- **Official OAuth / audited local bridge only** for provider auth. Never automate provider login-approval pages. Never describe Basiliskos as a quota or restriction bypass. *Reverse if:* never — this is the product's legal footing.
- **Don't patch or redistribute Claude Code Desktop binaries.** CLIProxyAPI is a pinned, audited dependency — not source to copy in. *Reverse if:* only after audit + Charles's explicit OK to bundle.
- **MIT license**, kept distributable. *Reverse if:* Charles changes the licensing model deliberately.
- **Milestone approval gate:** stop after each plan/discrete milestone and get Charles's approval before the next implementation phase. *Why:* keeps a public auto-updating installer from shipping unreviewed. *Reverse if:* Charles lifts the gate for a specific stretch.

## Release discipline (stable)
- **Version bumps in all 5 spots** (see MAP) and release via a `v<version>` tag → `release-gate.yml`. Never relax security gates (command-surface allowlist, clean-room forbidden list) to make a release pass — fix forward. *Reverse if:* never without Charles's explicit OK.
