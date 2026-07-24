# OpenCodex inside Basiliskos (scaffold)

## Why this exists

Charles asked to scaffold **OpenCodex** capability **inside Basiliskos**
(`hydra-gateway`), not as a second global npm install of
`@bitkyc08/opencodex`.

Context:

- Community OpenCodex (`lidge-jun/opencodex`) is a local multi-provider proxy
  for Codex CLI / Claude Code with many adapters and a dashboard.
- On 2026-07-21 it was installed globally for Nexus; on 2026-07-22 it was
  **removed** because the dead proxy on `:10100` broke Discord Nexus freeform
  turns and even a healthy heavy route was too slow.
- Basiliskos already multi-homes Claude / Codex / Grok / Kimi through a pinned
  **CLIProxyAPI** child + Basiliskos front proxy. Re-introducing a second
  system-wide Codex base-URL rewrite would fight that design.

So the product decision for this milestone:

> Embed an **OpenCodex-shaped catalog + preference surface** in Basiliskos, with
> live multi-provider routing **hard-disabled** until an explicit later
> milestone. Do not reinstall the npm package. Do not rewrite
> `~/.codex/config.toml`.

## What shipped (scaffold 0.1.0)

| Piece | Location | Behavior |
|-------|----------|----------|
| Design-time catalog | `src-tauri/src/opencodex.rs` | OpenRouter, Ollama, DeepSeek, Gemini, Azure, custom OpenAI-compatible entries; all `live: false` |
| Prefs | `~/.hydra-gateway/opencodex-prefs.json` | `experimentalEnabled`, `selectedRoute`, non-secret notes |
| Commands | `opencodex_status`, `set_opencodex_preferences` | Read/write scaffold prefs; never enable live routing |
| Snapshot field | `GatewaySnapshot.opencodex` | UI can render catalog from normal refresh |
| UI tab | App **OpenCodex** | Catalog browser + experimental toggle + route picker |
| Live request path | unchanged | Still Claude/Codex/Grok/Kimi via CLIProxyAPI |

Hooks reserved for later:

- `should_route_via_opencodex(provider, model) -> bool` — always `false` now
- `resolve_catalog_route(id)` — maps catalog ids to provider/model metadata

## Non-goals (this milestone)

- No `@bitkyc08/opencodex` / `ocx` dependency
- No Responses-API ↔ Anthropic/Gemini protocol translation yet
- No account pooling, failover, or Codex App injection
- No secrets in the catalog prefs file (API keys stay out until a real auth path)
- No claim of quota bypass

## Next milestones (need Charles approval before each)

1. **Credential vault for key/endpoint providers** under
   `~/.hydra-gateway/opencodex-auth/` (same durable-write + secure-dir pattern as
   gateway auth).
2. **Optional adapter path** off the Basiliskos front proxy for catalog routes,
   only when experimental + live flags are both on.
3. **Usage / health** for catalog providers where the upstream exposes it.
4. **Optional** Codex CLI injection as a separate product mode — default remains
   Basiliskos-owned Claude profile only.

## Inspiration / references

- Community project: https://github.com/lidge-jun/opencodex
- Package (not installed): `@bitkyc08/opencodex`
- Prior workspace history:
  - `handoff/2026-07-21-1520-grok-opencodex-install.md`
  - `handoff/2026-07-22-1627-claude-nexus-opencodex-removal.md`
