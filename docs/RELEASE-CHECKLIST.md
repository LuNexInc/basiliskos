# Basiliskos release gate

A release is blocked unless every applicable item below has current evidence from
the same commit and the Windows release workflow is green.

- [ ] `pnpm install --frozen-lockfile` and `pnpm test:all` pass.
- [ ] CLIProxyAPI archive and executable hashes match `prepare-gateway.ps1`.
- [ ] Cargo deny, OSV, secret, runtime-log, command-surface, and actionlint gates pass.
- [ ] The NSIS clean install, 1.1.5 migration, repair, rejected rollback, shortcut,
      profile-preservation, and uninstall lane passes on a clean Windows runner.
- [ ] Artifact SHA-256 manifest, SPDX SBOM, JavaScript license inventory, and build
      provenance are attached to the workflow run.
- [ ] `artifacts.json` records the exact Authenticode state for the app and
      installer. If certificate secrets are configured, both must be `Valid`;
      otherwise release notes clearly say `Unsigned / Unknown publisher`.
      Signing secrets, when used, exist only in the CI secret store.
- [ ] The install location is `C:\Program Files\3ReadyLab\Basiliskos` and no legacy
      `%LOCALAPPDATA%\Basiliskos` or `C:\Program Files\Basiliskos` binary remains.
- [ ] Upgrade preserves `~/.hydra-gateway` credentials and the isolated Claude profile.
- [ ] Release notes list stable diagnostic codes and any known limitations.
- [ ] Updater publication remains disabled until a separate updater lifecycle and
      integrity policy is approved for the exact updater payload.
