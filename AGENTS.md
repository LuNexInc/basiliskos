# Basiliskos

Basiliskos is a fork of the committed `../grok-hydra` codebase. The original
`../grok-hydra` project must remain untouched by work in this folder.

The product goal is a small Windows controller that keeps Claude Code Desktop
as the user's working interface while switching its local gateway between
Claude, Codex, and Grok accounts the user owns or is authorized to use.

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

Follow the root workspace `AGENTS.md` and `HANDOFF.md` protocol.
