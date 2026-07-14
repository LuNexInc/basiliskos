# Security

## Reporting

Please report credential exposure or authentication-handling vulnerabilities
privately to the repository owner instead of opening a public issue.

## Credential handling

Hydra stores profile credentials under the current user's home directory.
Never attach `auth.json`, `profiles.json`, exported credentials, access tokens,
or refresh tokens to an issue.

Revoked or expired credentials must be replaced through the official
`grok login` flow.
