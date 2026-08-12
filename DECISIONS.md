# DECISIONS — hydra-gateway (Basiliskos)

> What is intentionally settled + why + reverse-if. HANDOFF = history; this = standing state.
> Newest on top. Extracted from AGENTS.md 2026-07-25 — keep in sync when a call changes.

## 2026-08-13 — The Codex window model switcher is real, not a facade
- **Decision:** the isolated Codex window routes the picked model to its own provider. `grok-4.6` runs real Grok, `gpt-5.6-*` runs real Codex, `kimi-k3` runs Kimi — regardless of the active account. The DeepSeek `openai-compatibility` block aliases DeepSeek ids only; it no longer captures other providers' models. The dial rewrite no longer forces the active route's model. Accounts keep ONE enabled account per provider (not one globally), so CLIProxyAPI's per-provider credential selection stays unambiguous and the Claude path keeps using the user's selected account.
- **Why:** Charles picked Grok 4.6 in the Codex window's switcher and was silently routed to DeepSeek (the active route) when a screenshot 400'd with DeepSeek's text-only schema error. The switcher displayed one model and ran another — a facade. Verified against the pinned 7.2.128 runtime: with the aliases removed and the xAI/codex credentials enabled, `grok-4.5` and `gpt-5.6-terra` both route to their real providers (200).
- **Known limitation:** DeepSeek models in the codex switcher route only while DeepSeek is the active account (the compat block embeds the active DeepSeek key). Claude models need a Claude OAuth credential (none present). DeepSeek remains text-only; images in the codex window still fail upstream on a DeepSeek pick (now with a clear BAS error).
- **Reverse if:** Charles wants the codex window to follow the active route again, or CPA gains per-request credential pinning that makes global multi-enable safe.

## 2026-08-07 — Usage renewal and login-token expiry are different clocks
- **Decision:** quota renewal timestamps must come from each provider's usage/billing window. Basiliskos reads xAI's `currentPeriod.end` and Codex's per-window `reset_at`. OAuth access-token expiry is labelled as a login-token timestamp and must never be presented or implied as usage renewal.
- **Why:** xAI reported marcjanin's weekly period ending 2026-08-07 23:36 Asia/Manila and charles.3ready's ending 2026-08-11 21:56, while the nearby OAuth token expiry made Basiliskos appear to claim 2026-08-07 07:00 as renewal. Codex likewise provides exact quota-window resets independently of token expiry.
- **Reverse if:** never conflate the clocks; if a provider stops reporting its window end, show renewal as unavailable instead of substituting credential expiry.

## 2026-08-07 — Every patched installer gets a new version
- **Decision:** never finalize or distribute changed Basiliskos code under an already-published version number. The crash cleanup and refresh corrections are version 2.2.5, not another 2.2.4 build. Rebuilding an unreleased patch during its verification loop does not consume another version.
- **Why:** reusing 2.2.4 made the installed build indistinguishable from the published GitHub release and broke update/debug provenance.
- **Reverse if:** never; version identity must remain unique.

## 2026-08-07 — OAuth hardening scope is crash recovery and truthful status
- **Decision:** harden the existing local credential store by cleaning stale login/vision workspaces after crashes, refreshing saved OAuth grants before they become unusable, and reporting provider-specific usage truthfully. Do not start a DPAPI vault migration from the earlier audit without a separate explicit request.
- **Why:** Charles clarified that “strengthen my OAuth vault” meant crash cleanup plus inaccurate usage/refresh reporting, not a storage-format redesign.
- **Accuracy rules:** prefer GrokBuild usage over combined Grok billing; label Kimi's unscoped total as `Plan`, not `Week`; say `Not reported` when a provider omits a percentage; never claim “Renewing now” unless a refresh is actually in progress; refresh all OAuth usage automatically and expose only one universal manual refresh control. A usage-endpoint denial must never be presented as proof that the saved login expired.
- **Reverse if:** Charles explicitly asks for encrypted-at-rest credential migration and approves its compatibility/recovery tradeoffs.

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
- **Verified against the pinned 7.2.128 runtime:** the key must sit under `api-key-entries`; `api-keys` parses without error but loads **zero** clients. An unknown-`type` auth file is inert (loads as a client advertising no models), which is why DeepSeek can reuse the existing account machinery safely.
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
