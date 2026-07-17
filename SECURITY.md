# Security

## Reporting

Please report credential exposure or authentication-handling vulnerabilities
privately to the repository owner instead of opening a public issue.

## Credential handling

Basiliskos stores controller state and provider credentials under
`%USERPROFILE%\.hydra-gateway`. Never attach `auth` directory contents,
access tokens, refresh tokens, or local API keys to an issue.

Replace expired credentials through each provider's official browser OAuth
flow from the Basiliskos UI (Claude, Codex, Grok, or Kimi). Kimi uses its
official device-authorization page and one-time code; Basiliskos never
automates the approval step.

## Scope

Basiliskos is a local loopback controller. It does not patch Claude Desktop,
bypass provider limits, or automate OAuth approval pages. Use it only with
accounts you own or are authorized to access.
