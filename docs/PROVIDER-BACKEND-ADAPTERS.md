# Provider backend adapters

Basiliskos routes every live request through its own front proxy on
`127.0.0.1:8317`. That proxy owns model rewriting, the truthful route-identity
prompt, and the tool-compatibility fixups, and is the stable seam between the
clients (Claude Code Desktop, the isolated Codex window) and whatever gateway
actually reaches the upstream provider.

This document records the contract a backend adapter must satisfy so a future
gateway runtime — most obviously an OpenCode-Go style router — can be dropped in
behind that seam without changing the clients.

## The two axes

Every account is identified by two orthogonal axes:

- **Provider** — the target catalog identity (`claude`, `codex`, `xai`, `kimi`,
  `antigravity`, `zai`, `deepseek`, `opencode`, `openrouter`, `litellm`, `custom`).
- **Auth** — `oauth` (browser login, refreshable token) or `api_key` (static
  key + optional endpoint). Any OAuth provider can also be reached by key.

## Backend enum

`crate::catalog` reserves a `Backend` notion: the default is `cliproxy` (the
pinned CLIProxyAPI runtime), with a reserved `custom(path)` variant for
an alternate gateway binary. Nothing currently constructs `custom`; it is
documented so the design does not paint the product into a corner.

## Front-proxy contract

A custom backend adapter must present a loopback-only, key-gated HTTP
compatibility endpoint that:

1. Accepts Anthropic-shaped `/v1/messages` (Claude Code Desktop) and
   OpenAI-shaped `/v1/chat/completions` and `/v1/responses` (Codex) on a single
   unpublished loopback port.
2. Lets Basiliskos's front proxy own the model rewrite and the identity prompt
   — that is, the adapter must not re-assert a false model identity.
3. Validates requests with the generated local `api_key` (Bearer / `x-api-key`).
4. Never binds a public address and never opens a management web panel.
5. Is pinned and SHA-256-verified like CLIProxyAPI before bundling.

## What ships in 3.0

- The `Provider × Auth` model and the API-key credential store
  (`~/.hydra-gateway/gateway/keys/<provider>-<label>.json`).
- API-key account commands (`add_api_key_account`, `get_api_key_account_models`)
  and the grouped OAuth / API-keys UI.
- Curated API-key presets: **OpenCode Go**, **OpenRouter**, **LiteLLM**, plus a
  generic `custom` OpenAI-compatible slot (covers Portkey, Higress, new-api,
  Bifrost, vLLM, Bedrock, OpenCode Zen).

## Known gaps (pending verification)

- **CLIProxyAPI config emission** for an API-key provider now uses the verified
  `openai-compatibility:` list shape (`name` / `base-url` /
  `api-key-entries` as an object list / `models`), matching the pinned
  `config.example.yaml`. Confirmed end-to-end against the pinned binary in
  `scripts/test-cliproxy-api-key.ps1` (2 models registered + routed to a
  loopback upstream). The schema is protected in CI by
  `openai_compat_provider_block_matches_the_cliproxy_schema`. The live-binary
  script is a manual tool (timing-sensitive against a real upstream); it is not
  part of `test:all`.
- **Router model catalog** is not yet mapped into the Claude picker's Anthropic
  alias scheme; routers/custom expose a live model list but advertise only the
  selected model today. Wiring the live list into the picker needs the alias-map
  refactor noted in `catalog.rs`.
- **OpenCode-Go credential shape** (opencode.ai workspace id + key) is reserved;
  confirm the exact wire form when building the OpenCode preset.
