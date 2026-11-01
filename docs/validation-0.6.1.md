# Downloader 0.6.1 validation — 2026-11-01

Base: `7abcac6` (0.6.0). Scope: clipboard watcher fix (`540f7d0`) and the new **Download copied links automatically** option (`cfb67ba`). Guide: [usability.zh-TW.md](usability.zh-TW.md).

## Evidence (Windows 10, local)

| Check | Result | Evidence |
| --- | --- | --- |
| GUI format | PASS | `cargo fmt -p downloader --check` |
| Strict Clippy | PASS | `cargo clippy --release --workspace --all-targets --locked -- -D warnings` |
| Rust tests | PASS | `cargo test --release --locked --no-fail-fast`: GUI 48 (+2 ignored native fixtures), core 62, download 29, other 29 — 0 failed |
| Extension tests | PASS | `node --test scripts/tests/extension-background.test.cjs`: 16 passed |
| CLI smoke | PASS | `dlx --version` / `dlxctl --version` report 0.6.1 |
| Clipboard auto-download E2E | PASS | Isolated `DOWNLOADER_STATE_DIR`, settings `watch_clipboard` + `clipboard_auto_download`, local `http.server`; copying the link downloaded `file.bin` (200000 bytes) within 6 s with no dialog interaction |
| Clipboard watcher from tray/compact | PASS | `540f7d0`: native-host message with `launch:false`, Add dialog shown on the restored main window |
| Translation files | PASS | `scripts/gen_po.py`: 252 strings × 7 languages |
| Windows installer smoke | see CI | `installer-smoke.ps1` runs in CI |
| Linux / macOS runtime | NOT_RUN | Windows host only |

## Limits

Auto-download always uses the default folder and options. That no Add dialog appears in auto mode was checked from the code path, not from a screenshot. The new strings are machine-translated.
