# Basiliskos logic repair plan

Status: Awaiting approval for M0  
Scope: Repair the 22 confirmed logic defects from the three audits dated 2026-08-20 and 2026-08-21.  
Release scope: Excluded. This plan does not change the version, publish repository, tag, installer, or installed application.

## Goal

Make Claude and Codex client behavior independent, make request attribution and recovery accurate, preserve Codex-owned data, and make every fallback truthful and testable.

## Fixed execution rules

1. Work on the canonical `main` checkout in `hydra-gateway/`.
2. Leave unrelated workspace changes untouched. Stage only Basiliskos files changed by the active milestone and that milestone's handoff file.
3. Add a failing regression test before each behavior change. Record the failing assertion in the milestone handoff.
4. Change the smallest boundary that makes the regression test pass. Do not combine later milestones into the active milestone.
5. Run the milestone's focused tests, review its diff, commit it, write the handoff, and stop for approval.
6. Do not restart Basiliskos, open an isolated client, use live credentials, change an account, or send a provider request until the applicable live-verification step receives approval.
7. Do not release or bump the version under this plan. Release work needs a separate version decision and release approval.
8. If implementation evidence requires a different design, stop. Update this plan and obtain approval before continuing.

## Product invariants

- `active_account` and `routes` remain the serialized Claude fields for backward compatibility.
- `active_codex_account` and `codex_routes` hold the isolated Codex selection.
- Claude requests use Claude state. Codex Responses and Chat Completions requests use Codex state.
- CLIProxyAPI loads one enabled credential per provider. If Claude and Codex use the same provider, both clients must use the same account for that provider. The UI and errors must state this constraint.
- A client action must not rewrite the other client's model, route, account state, or client configuration.
- The isolated Codex app owns its refreshed `auth.json` after the first seed. Basiliskos captures refreshed credentials back to the vault.
- Basiliskos modifies only Basiliskos-owned Codex configuration keys. It preserves every app-owned and unknown TOML key.
- Local overload, provider authentication, provider quota, provider rate limits, and provider server failures are distinct error classes.
- Every image description maps to one source image. A description must not represent an image that the vision sidecar did not process.

## Defect coverage matrix

| ID | Confirmed defect | Milestone | Required regression |
|---|---|---|---|
| L01 | `/v1/responses` attributes requests to Claude state. | M2 | A Codex request without a Claude selection resolves its Codex provider and account. |
| L02 | Claude live-picker synchronization stops while Codex is open. | M1 | Each live client synchronizes independently when both windows are present. |
| L03 | The main ChatGPT card displays Claude account, route, and usage. | M1 | The card renders Codex snapshot fields only. |
| L04 | Grok 4.6 shows a 500,000-token window but receives no matching request budget. | M4 | Grok 4.5 and 4.6 receive the same declared and enforced budget. |
| L05 | Account removal leaves backend and client configuration stale. | M5 | Removing a selected account rewrites state, catalog, client config, and running-backend config. |
| L06 | **Use for Basiliskos Codex** invokes the Claude selection transaction. | M1 | A Codex selection changes only Codex state and Codex-owned files. |
| L07 | Persisted state has one active account and one route map. | M1 | Legacy state migrates without losing Claude state and gains independent Codex fields. |
| L08 | Codex launch requires the Claude active account. | M1 | Codex launches from a valid Codex selection when Claude has no selection. |
| L09 | Codex rate-limit failover invokes the Claude selection transaction. | M2 | Codex failure recovery updates Codex state and preserves unrelated Claude state. |
| L10 | ChatGPT settings, tray status, and account buttons use Claude active flags. | M1 | Every Codex surface uses `active_codex_account`, `codex_routes`, and `active_for_codex`. |
| L11 | Generated Codex configuration disables automatic compaction. | M4 | The generated threshold is model-aware and permits the compaction plugin to receive a trigger. |
| L12 | The plugin's two-request gate returns an internal 429 for ordinary requests and can cause account failover. | M2 | Three concurrent ordinary requests pass; local DeepSeek overload never cools or switches an account. |
| L13 | Grok web-search declarations are removed while `x_search` injection is disabled. | M4 | A client web-search declaration becomes one valid `x_search` declaration and remains a search request. |
| L14 | Codex launch replaces the whole `config.toml`. | M3 | Desktop, plugin, feature, marketplace, MCP, Windows, comments, and unknown keys survive a Basiliskos update. |
| L15 | Codex launch can overwrite a token refreshed by the isolated app. | M3 | An existing valid isolated credential is never reseeded during ordinary launch, including when Codex is already running. |
| L16 | Capture-back updates tokens but preserves stale expiry metadata. | M3 | Capture-back recomputes or clears expiry fields and clears a stale expired flag. |
| L17 | DeepSeek **replace key** creates another account and keeps the old account. | M5 | Replacement keeps the account identity and label, removes the old key file, and rolls back on failure. |
| L18 | Vision collection consumes its limits from the oldest conversation content. | M6 | The newest user turn and newest images receive priority when the request exceeds the limits. |
| L19 | One aggregate vision description replaces every image. | M6 | Each processed image receives only its own description; an unprocessed image receives an explicit omission marker. |
| L20 | Codex converts every `auto` reasoning choice to `high`. | M4 | The rendered effort is supported by the selected model, and models with no effort levels omit the key. |
| L21 | HTTP 402 is classified as failed authentication and cannot use an alternate account. | M2 | HTTP 402 becomes quota or billing exhaustion, honors `Retry-After`, and can use an eligible same-provider account. |
| L22 | Hidden models remain in the isolated Codex catalog. | M4 | Catalog regeneration excludes hidden models unless the current selection needs a visible repair path. |

## M0: Establish the baseline

### Changes

- Do not change product behavior.
- Run `pnpm test:all` from the canonical checkout.
- Record the current Rust, UI, Go plugin, command-surface, installer, gateway, and secret-scan results.
- Add a test-only request fixture helper if repeated setup cannot stay local to each test module. Do not add production abstractions in M0.

### Stop condition

- If the baseline fails, record each pre-existing failure and stop before product edits.
- If the baseline passes, commit only a test helper when one was necessary. Otherwise, write and commit only the milestone handoff.

### Acceptance

- The baseline result and current commit are recorded.
- No runtime, client, credential, generated configuration, or provider request changes occur.

## M1: Restore independent client state and UI

### State model

- Add `ClientSurface::{Claude, Codex}` and typed helpers for active accounts, routes, configuration writes, and status labels.
- Keep `ControllerState.active_account` and `ControllerState.routes` as Claude's serialized fields.
- Add `active_codex_account: Option<String>` and `codex_routes: BTreeMap<String, RouteSelection>` with `serde(default)`.
- Migrate a state file that lacks Codex fields by reading the isolated Codex model when available. Map that model to its provider and the provider's enabled account. Leave Codex unselected when no valid mapping exists. Do not copy Claude state by default.
- Add `active_for_codex` to each account and Codex fields to `GatewaySnapshot`.

### Commands and account constraint

- Add a required `client` argument to `select_gateway_account` and `set_gateway_route` instead of creating parallel command families.
- Use one internal typed selection transaction for UI selection and recovery.
- Preserve the one-enabled-account-per-provider invariant. Permit both clients to select the same account. Reject a different same-provider account while the other client uses that provider, with an explicit shared-provider message.
- Make Codex selection regenerate only the isolated Codex catalog and merged configuration. Make Claude selection regenerate only Claude configuration. Regenerate shared backend configuration only when the provider credential pin changes.

### Synchronization and launch

- Split live-client synchronization into independent Claude and Codex branches. The state of one window must not skip the other branch.
- Make Codex launch validate `active_codex_account` and its provider. Remove the Claude active-account requirement.
- Replace the test-only `pick_codex_sync_account` path with production code that the tests call directly.

### UI

- Update `App.tsx`, `TrayDashboard.tsx`, and shared UI helpers so the ChatGPT card, settings strip, tray dashboard, account buttons, route labels, and usage request use Codex fields.
- Keep Claude surfaces on Claude fields.
- Show the shared-provider account constraint before a conflicting selection is submitted.

### Focused verification

- Run Rust state migration, selection, synchronization, launch-validation, and snapshot tests.
- Run `pnpm test:ui` with explicit dual-client fixtures.
- Run the command-surface check because command parameters and bindings change.

### Acceptance

- L02, L03, L06, L07, L08, and L10 pass their regressions.
- Selecting or launching Codex causes no Claude state or Claude configuration diff.
- Selecting Claude causes no Codex state or Codex configuration diff.

## M2: Make request attribution and recovery surface-aware

### Request context

- Build one `RequestContext` after authorization and body parsing. Store the client surface, request path, requested model, resolved model, provider, and account file.
- Classify `/v1/messages` and `/v1/messages/count_tokens` as Claude.
- Classify `/v1/responses`, its subpaths, and `/v1/chat/completions` as Codex.
- Resolve the Codex provider from the selected or requested catalog model. Return a clear unavailable-provider error when that provider has no enabled credential.
- Use the same context for rewrite, diagnostics, error messages, cooldowns, failover, and response events. Do not read global active state again later in the request.

### Recovery

- Pass `ClientSurface` into same-provider failover.
- Update only the failed client's selection when the other client uses another provider.
- If both clients use the failed provider, apply the shared provider account change to both client snapshots and state the shared change in diagnostics.
- Add `ProviderQuotaExhausted` for HTTP 402. Use `Retry-After` when present and the existing default cooldown when absent. Attempt an eligible same-provider account before returning the failure.
- Keep HTTP 401 and 403 under `ProviderAuthFailed`, HTTP 429 under `ProviderRateLimited`, and HTTP 5xx under `UpstreamServerError`.

### Plugin admission

- Determine whether the plugin must direct-hop a DeepSeek request before applying its two-slot gate.
- Do not reject ordinary requests because two other requests are active.
- Keep the two-slot limit for direct DeepSeek and vision work.
- Add a stable response header, `X-Basiliskos-Fault: local-concurrency`, to a local admission rejection.
- Map that marker to `LocalConcurrencyLimited` in the front proxy. Return a retryable response without provider cooldown or account failover.

### Focused verification

- Run Rust request-context, attribution, cooldown, HTTP 402, and failover tests.
- Run Go plugin tests with three overlapping ordinary requests and three overlapping DeepSeek requests.
- Add a relay-level test that passes the local-overload marker through the front proxy and proves account state stays unchanged.

### Acceptance

- L01, L09, L12, and L21 pass their regressions.
- Every diagnostic event identifies the provider and account selected in the immutable request context.

## M3: Preserve Codex configuration and credential ownership

### Merge-safe configuration

- Add a direct `toml_edit` dependency at the version already present in `Cargo.lock`.
- Parse the existing isolated `config.toml` as an editable document.
- Update only Basiliskos-owned top-level keys: `model`, `model_reasoning_effort` when applicable, `model_reasoning_summary`, `model_auto_compact_token_limit`, `model_catalog_json`, and `openai_base_url`.
- Preserve all other keys, tables, comments, order, and formatting. Remove only obsolete keys that Basiliskos previously generated and explicitly owns.
- Use the same parsed document to read the current model and effort. Remove the line scanner.
- Write through `durable_write` only after parsing and validation succeed.

### One-way credential ownership

- Check `hydra_codex_running()` before any seed or configuration operation that requires a stopped client.
- Seed `auth.json` only when the file is missing or invalid, or when the user explicitly changes the anchor while Codex is stopped.
- Do not reseed a valid existing isolated credential during ordinary launch.
- During capture-back, copy access, refresh, and ID tokens as one durable update.
- Derive `expires_at` from the refreshed access token's JWT `exp` claim when present. Clear stale `expires_at` when no reliable expiry exists. Set `expired` to `false` and update `last_refresh` when the token changes.
- Never write token values to logs, tests, Markdown, or diagnostics.

### Focused verification

- Test a TOML fixture that contains every known app-owned section, comments, and unknown keys.
- Test ordinary launch, already-running launch, first seed, explicit anchor change, invalid seed, token capture-back, JWT expiry, and no-expiry capture-back.
- Run the runtime and tracked-file secret scans.

### Acceptance

- L14, L15, and L16 pass their regressions.
- Repeated configuration generation is idempotent.
- A token refreshed by the isolated app remains unchanged after an ordinary Basiliskos launch.

## M4: Correct model, context, compaction, and Grok search contracts

### Model-aware configuration

- Resolve the selected `ModelSpec` before rendering Codex configuration.
- For `auto`, use `high` only when the model supports `high`. Use the only supported effort when the model exposes one level. Omit `model_reasoning_effort` when the model exposes no levels. Reject an explicit unsupported effort at the command boundary.
- Derive `model_auto_compact_token_limit` as 80 percent of the selected model's declared context window. Keep the catalog truncation limit and request budget based on the same source value.
- Extend `context_budget_for_request` to Grok 4.6 and use the resolved model rather than a `grok-4.5` prefix check.
- Pass the hidden-model set into `codex_catalog_models`. If the current model becomes hidden, select the provider default visible model and persist the repair before writing the catalog.

### Grok search translation

- Keep CLIProxyAPI `inject-x-search: false` to avoid issue #4339.
- Replace each client `web_search` or versioned `web_search_*` declaration with one normalized `x_search` declaration.
- Preserve unrelated tools. Normalize or remove only a `tool_choice` that targets the replaced web-search tool.
- Deduplicate an existing `x_search` declaration.

### Focused verification

- Test every catalog model against its generated reasoning effort.
- Test Grok 4.5 and 4.6 declared windows, input budgets, output reserves, and over-limit errors.
- Test compaction thresholds for 200,000-token and 500,000-token models and a plugin `compaction_trigger` request.
- Test hidden current and non-current models.
- Test Grok search-only, mixed-tool, forced-choice, and existing-`x_search` requests.
- Run one approved live Grok search smoke with a copied auth directory after all focused tests pass.

### Acceptance

- L04, L11, L13, L20, and L22 pass their regressions.
- The live smoke produces search-backed output without issue #4339 and without modifying real credentials.

## M5: Repair account replacement and removal

### DeepSeek replacement

- Extend `add_deepseek_account` with an optional `replace_file_name`.
- Verify the new key before acquiring the mutation lock.
- When replacing, validate that the target exists and belongs to DeepSeek. Preserve its display label and both client-selection roles.
- Commit the new credential, state update, label update, and old credential deletion in one transaction. A failed transaction must restore every original file.
- Update the relogin UI to send the selected file name. Keep the add-account path unchanged when no replacement target exists.

### Removal and regeneration

- Clear each client selection that references the removed account.
- If another eligible account exists for the same provider and does not violate the shared-provider rule, select it for the affected client. Otherwise, leave that client unselected.
- Regenerate the backend configuration, isolated Claude configuration when affected, isolated Codex merged configuration, and Codex catalog from the post-removal state.
- If the backend is running, apply the established controlled restart or reload path after the durable transaction. Report a runtime reload failure without restoring stale persistent state.
- Reject launch and route actions that refer to a removed account or unavailable model.

### Focused verification

- Test replacement while inactive, active for Claude, active for Codex, active for both, label preservation, invalid target, verification failure, and transaction rollback.
- Test removal for each client, both clients, same-provider fallback, no fallback, stale Codex model, running backend reload success, and reload failure.

### Acceptance

- L05 and L17 pass their regressions.
- No removed credential, account reference, route, catalog entry, or generated backend entry remains active.

## M6: Map vision descriptions to source images

### Collection and replacement

- Traverse request messages and Responses input from newest to oldest.
- Select at most eight newest images. Build each sidecar request from one image and up to 8,192 characters from its local user turn.
- Run at most two sidecar descriptions concurrently.
- Assign a stable traversal index to each image and replace the image only with the description returned for that index.
- Replace an older image outside the eight-image limit with `Image omitted from this text-only route because the eight-image limit was reached.` Do not attach another image's description.
- Apply the same mapping rules to Anthropic Messages and OpenAI Responses plugin paths.
- Preserve existing presentation guidance once per request.

### Focused verification

- Test long histories, more than eight images, multiple images in one turn, nested tool results, mixed text and images, sidecar partial failure, and out-of-order concurrent completion.
- Use a deterministic mock sidecar that returns each image's index in its description.
- Confirm the serialized request contains each processed index once and contains no source image data.

### Acceptance

- L18 and L19 pass their regressions.
- The current user image receives priority, each processed image receives its own description, and omitted images are identified truthfully.

## M7: Run the complete verification gate

### Automated verification

- Run `cargo fmt --check` and Go formatting checks for changed plugin files.
- Run focused Rust, UI, and Go tests once more.
- Run `pnpm test:all` without relaxing any gate or allowlist.
- Run `..\.tools\check-map-freshness.ps1 -Project hydra-gateway` when module locations change.
- Review the complete diff against this plan and the defect coverage matrix.

### Approved live verification

- Use throwaway configuration and copied credential directories.
- Verify independent Claude and Codex routing with both windows open.
- Verify Codex launch with no Claude selection.
- Verify a Codex provider rate limit affects the correct client state.
- Verify one Grok 4.6 near-budget request, one Grok search request, one compaction trigger, and one multi-image DeepSeek request.
- Confirm the canonical global Codex home and all real credential files retain their original hashes and modification times unless the test explicitly covers approved capture-back.

### Acceptance

- All 22 defect regressions pass.
- `pnpm test:all` passes.
- Live evidence matches the client, provider, account, model, and recovery behavior in this plan.
- The handoff lists residual risks and confirms that no release occurred.

## Release boundary

After M7, stop. Present the verified diff, test evidence, live-smoke evidence, and residual risks. A release requires a separate instruction that names the version. Run the release preflight only after that instruction.

## Plan change control

This document defines the implementation order. If a milestone exposes a conflict with CLIProxyAPI, Codex Desktop, or the one-enabled-account invariant, do not improvise a broader architecture. Record the evidence, propose a precise amendment, and wait for approval.
