# Downloader 0.5.2 validation — 2026-10-22

Base: `f3f83664ec1ebc80337c72ff5c4dba1de17062b8`. Scope and author ownership: [dispatch plan](dispatch-plan.md).

## Current evidence

| Check | Result | Evidence |
| --- | --- | --- |
| Clean baseline, Windows release tests | PASS | `cargo test --release --workspace --locked`, exit 0: 61 passed, 1 ignored native fixture |
| Odin 0.2.0 / rust-analyzer handshake | PASS | Dedicated read-only Downloader configuration; `odin doctor --probe-lsp`, exit 0 |
| Odin semantic symbols | PASS | Real LSP symbols for `core/src/engine.rs` and `core/src/manager.rs`, exit 0 |
| Odin engine diagnostics | PASS | Current version, zero findings, exit 0; this is document diagnostics, not build proof |
| Extension regression tests | PASS | Actual `background.js` executed in Node VM with Chrome mocks: 6 passed, exit 0 |
| Final release format/lint/build/test | PASS | `scripts/verify-local.ps1 -BuildInstaller`, exit 0 on final source: GUI 16, core 24, download 29, IPC 8; 77 Rust tests plus 6 Node tests; strict release Clippy and GUI formatting pass |
| Native rendered UI snapshots | PASS | Four ignored fixtures run explicitly with process exit 0: General, Browser, Downloads, Empty; real Windows renderer snapshots at 760×480 logical / 950×600 pixels (125% scale) |
| Windows installer build | PASS | Inno Setup 7 built `dist/downloader-setup-windows-x64.exe`; compile only, installation tracked separately |
| Local app upgrade | NOT_RUN | Existing all-user installation is 0.4.1 at `C:\Program Files\Downloader`; not running at intake |
| Browser profile extension installation | BLOCKED | Browser-use policy refuses internal extension-management URLs and prohibits alternate routes; manual Load unpacked is required |
| GitHub Actions | NOT_RUN | User reports exhausted quota; pushed commits use `[skip ci]`; release workflow not dispatched |
| Linux/macOS runtime | NOT_RUN | Windows host only; macOS packaging correction is source-reviewed |

Transient raw logs, Odin results and snapshot fixtures are stored under ignored `target/review-evidence/`.

Native-rendered examples use synthetic data: [General settings](ui-0.5.2-settings-general.png), [Browser setup](ui-0.5.2-settings-browser.png). Snapshot capture proves rendered pixels; keyboard behavior is tested with Slint's testing backend. Real OS mouse input, IME and screen-reader checks were not performed.

## Review and limits

Independent review identified missing Content-Range validation and browser handoff after cancellation failure. Both receive focused fixes and regression coverage. A second review caught valid shorter/unknown-total range compatibility; that case is explicitly retained and tested.

The final UI review also caught focus restoration after cancelling Exit with Settings still open. The selected Settings tab regains focus, and a keyboard regression verifies that subsequent Tab/Enter stays in Settings. Initial strict Clippy runs found style warnings in new browser helpers; these were corrected without disabling lint checks.

Native-host readiness does not detect extension installation in browser profiles. Chrome and Edge were found locally; Brave was not detected in standard locations or App Paths. The connected browser profiles are Chrome `你的 Chrome` and Edge `設定檔 1`. Browser installation requires the user's browser action because the automation tool blocks `chrome://extensions` and equivalent internal pages.

Follow-up candidates outside this change: cancellation during checksum/signature discovery, and a byte budget for extremely large “download all links” native messages. The existing bounded-concurrency metadata discovery and native-message limits remain in place.
