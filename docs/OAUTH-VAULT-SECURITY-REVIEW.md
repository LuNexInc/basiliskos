# OAuth Vault Security Review

Date: 2026-08-07
Scope: Basiliskos credential storage, login staging, credential refresh, account switching, deletion, and temporary vision sidecars.

> Scope clarification (2026-08-07): Charles requested the crash-cleanup and
> usage/refresh accuracy work, not the DPAPI storage migration proposed below.
> Treat the encryption section as a future option requiring separate approval.

## Executive summary

Basiliskos already has a meaningful first layer: the live auth directory and its
credential files have protected Windows ACLs containing only the current user,
writes are atomic, transactions are recoverable, OAuth error bodies are not
logged, and the Tauri frontend uses a restrictive CSP.

The remaining material gap is encryption at rest. The canonical OAuth and API-key
records are plaintext JSON, and every record normally has a plaintext `.backup`
sibling. On the inspected machine that means 11 live credential records plus 11
backup copies. The machine has no associated BitLocker volume, so offline access
to the disk can bypass the account ACL and recover refresh tokens and API keys.

Recommended direction: make a versioned, current-user DPAPI-encrypted store the
canonical vault; project only the active credential into a tightly ACLed runtime
directory for CLIProxyAPI; protect runtime and staging directories with NTFS EFS
when available; and clean crash remnants before any provider process starts.

## High priority

### OAUTH-001 — Canonical credentials and backups are plaintext

- Severity: High
- Location:
  - `src-tauri/src/persistence.rs:99-121` (`durable_write`)
  - `src-tauri/src/gateway.rs:3677-3703` (`list_accounts_inner`)
  - `src-tauri/src/gateway.rs:5132-5167` (`selection_transaction`)
- Evidence: account records are parsed directly from JSON files. `durable_write`
  creates and rotates a sibling `.backup`, while account selection rewrites every
  credential to toggle `disabled`.
- Impact: an offline disk reader or backup/image reader can recover OAuth access
  and refresh tokens and DeepSeek API keys. The existing ACL is effective against
  ordinary other-user access while Windows is running, but is not encryption.
- Fix: introduce a versioned DPAPI envelope using current-user scope (do not set
  `CRYPTPROTECT_LOCAL_MACHINE`) and `CRYPTPROTECT_UI_FORBIDDEN`. Store encrypted
  blobs and encrypted transaction/backup material in a separate canonical vault.
  Keep plaintext out of the general persistence path.
- Compatibility constraint: CLIProxyAPI consumes JSON files and may rotate tokens.
  Therefore, expose only the active credential in a dedicated runtime auth
  directory, sync a validated refresh back into the encrypted vault before switch
  or shutdown, and recover/scrub a crash remnant at the next startup.
- Mitigation: the current protected ACL is worth preserving. NTFS EFS can add
  transparent encryption to the runtime directory on supported editions, but it
  should be layered under the DPAPI vault rather than treated as the portable
  canonical format.

## Medium priority

### OAUTH-002 — Login staging has an inherited, broader parent ACL

- Severity: Medium
- Location:
  - `src-tauri/src/gateway.rs:5706-5726` (`login_staging_root`, cleanup)
  - `src-tauri/src/gateway.rs:5729-5750` (`staged_login_config`)
  - `src-tauri/src/gateway.rs:6040-6049` (`launch_provider_login_blocking`)
- Evidence: `secure_create_dir_all` is called for the nested `auth` directory, not
  explicitly for `login-staging` and the per-session parent containing
  `login-config.yaml`. The inspected `login-staging` root inherits a read/execute
  rule for `CodexSandboxUsers`; the live auth directory does not. The temporary
  config contains the local gateway API key.
- Impact: another process running under the inherited sandbox group may read the
  temporary login configuration during a login and use its local API credential.
  The nested OAuth output directory itself is currently protected, which limits
  the blast radius.
- Fix: explicitly create and protect the staging root, session directory, auth
  directory, and config file before writing. Add a Windows ACL test for every
  sensitive level, not only the final file and auth directory.

### OAUTH-003 — Hard termination can leave plaintext staging and sidecar copies

- Severity: Medium
- Location:
  - `src-tauri/src/gateway.rs:1049-1102` (`initialize_controller_storage`)
  - `src-tauri/src/gateway.rs:3014-3041` (`VisionSidecar::stop`)
  - `src-tauri/src/gateway.rs:3146-3185` (`spawn_vision_sidecar`)
  - `src-tauri/src/gateway.rs:5924-5926` and `6221-6237` (best-effort login cleanup)
- Evidence: normal cancellation/drop removes temporary directories, but startup
  initialization does not sweep stale `login-staging/<session>` or
  `gateway/vision-sidecars/<uuid>` directories. A power loss or process kill can
  bypass destructors and leave copied credentials on disk.
- Impact: plaintext credential copies can outlive their intended session. ACLs
  reduce live-system exposure but do not address offline recovery.
- Fix: before starting maintenance or provider processes, enumerate only validated
  one-component child directories beneath the two exact roots, terminate any
  associated orphan process if applicable, and remove them. Keep the same strict
  path guards used by normal cleanup. Add a hard-kill/startup-recovery test.

## Low priority / defense in depth

### OAUTH-004 — Credential filenames disclose account identities

- Severity: Low
- Location: `src-tauri/src/gateway.rs:3677-3736` and provider-generated auth names
- Evidence: several provider auth filenames embed account email addresses.
- Impact: directory metadata can disclose which identities are configured even
  when the file contents are later encrypted.
- Fix: give vault entries random IDs and retain provider/email/label metadata
  inside the encrypted envelope. Runtime filenames may remain provider-compatible
  but should exist only while needed.

## Existing controls to preserve

- Protected, current-user-only ACLs for the main auth directory and files
  (`src-tauri/src/persistence.rs:529-635`), verified live on 2026-08-07.
- Atomic writes, write-through replacement, rollback transactions, and path
  traversal guards (`src-tauri/src/persistence.rs:60-97`, `186-221`, `393-466`).
- OAuth response-body suppression and sanitized UI errors
  (`src-tauri/src/gateway.rs:4760-4778`, `6104-6111`).
- Trusted HTTPS authorization-host validation and tests in `gateway.rs`.
- Restrictive Tauri CSP with no remote scripts and no dangerous React HTML sink
  found in the scoped frontend review.

## Proposed implementation checkpoints

### Milestone 1 — Containment patch

1. Protect every login-staging path level and temporary config file.
2. Sweep stale login and vision-sidecar directories at startup with strict path
   validation.
3. Add ACL and crash-remnant tests plus secret/log scan coverage.
4. Run focused Rust tests, then `pnpm test:all`.

This milestone is low-risk and independently releasable.

### Milestone 2 — Encrypted canonical vault

1. Add `vault.rs` with a versioned DPAPI current-user envelope, integrity failure
   handling, and zero plaintext in error messages.
2. Add a transactional, fail-safe migration: encrypt, decrypt-and-validate JSON
   and provider identity, commit the encrypted entry, then remove the plaintext
   canonical record and its backup. Never delete the only validated copy.
3. Refactor list, refresh, add, rename, remove, failover, and vision-sidecar paths
   through a vault abstraction. Keep parked credentials decrypted only in memory.
4. Materialize only the active account to a dedicated runtime auth directory;
   reconcile token rotation back to the vault on switch, stop, and next startup.
5. Apply EFS to runtime/staging directories when supported and report a truthful
   fallback state when unavailable.
6. Add migration rollback, corrupted/tampered blob, refresh, failover, hard-kill,
   deletion, and no-plaintext-when-stopped tests. Run `pnpm test:all`.

This milestone changes credential storage and should not begin until Milestone 1
is reviewed and approved.

## Residual risk

DPAPI current-user encryption does not protect against malware already executing
as the same logged-in user, and CLIProxyAPI necessarily needs the active
credential in usable form while serving requests. The design materially improves
offline, backup, idle-account, and crash-remnant exposure; it is not a substitute
for endpoint security or full-disk encryption.

## References

- Microsoft: CryptProtectData uses current-user/current-machine protection by
  default; `CRYPTPROTECT_LOCAL_MACHINE` broadens access to every machine user.
  https://learn.microsoft.com/en-us/windows/win32/api/dpapi/nf-dpapi-cryptprotectdata
- Tauri: keep CSP as restrictive as practical and avoid remote code.
  https://v2.tauri.app/security/csp/
