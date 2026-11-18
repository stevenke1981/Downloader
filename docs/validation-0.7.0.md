# Downloader 0.7.0 validation — 2026-11-18

Base: `8d28623` (0.7 feature commits `fe3e0c6` and `8d28623`). Scope: modules (`docs/modules.md`), file-exists policy, custom headers and User-Agent, duplicate detection, disk-space precheck (Windows), queue export/import, remote API additions, agent CLI (`--json`, exit codes, `dlxctl` commands), `AGENTS.md`, GUI Tools > Modules tab and file-exists policy. Guides: [cli.md](cli.md), [modules.md](modules.md), [AGENTS.md](../AGENTS.md).

## Evidence (Windows 11, local)

| Check | Result | Evidence |
| --- | --- | --- |
| Release build | PASS | `cargo +1.96.1-x86_64-pc-windows-msvc build --release --locked --workspace` |
| Rust tests | PASS | `cargo +1.96.1-x86_64-pc-windows-msvc test --release --locked --workspace`: 223 passed, 0 failed, 2 ignored (GUI 49 + 2 ignored, core unit 80, `cli` 12, `download` 29, `essentials` 16, `external` 6, `google_drive` 8, `ipc` 8, `management_services` 8, `modules` 7) |
| Strict Clippy | PASS | `cargo +1.96.1-x86_64-pc-windows-msvc clippy --release --workspace --all-targets --locked -- -D warnings` |
| Format | PASS | `cargo +1.96.1-x86_64-pc-windows-msvc fmt --all --check` |
| Package contents | PASS (by inspection) | `scripts/package-windows.ps1` and `installer/downloader.iss` name four executables (`downloader`, `dlx`, `dlxctl`, `downloader-host`); `dlx-module-example` is not copied |
| Version | PASS | `core/Cargo.toml`, `gui/Cargo.toml` and the two `Cargo.lock` package entries are 0.7.0; the installer and packaging script point at this file |
| Tools > Modules tab, visual | NOT_RUN | Compiled; not exercised in a running window |
| Disk-space precheck on a nearly full disk | NOT_RUN | Not run against a nearly full volume |
| Installer build (Inno Setup) | NOT_RUN | `scripts/build-installer.ps1` not run in this pass |

## Limits

HTTP mirror fallback is not implemented: mirrors returned by a resolver are stored but never tried. The disk-space precheck is Windows-only. `dlx` does not deliver completion events to hook modules; the running app or `dlxctl serve` does. The development-only `dlx-module-example` binary is not packaged.
