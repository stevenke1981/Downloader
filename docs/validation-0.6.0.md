# Downloader 0.6.0 validation — 2026-10-31

Base: `74be006` (0.5.2 line). Scope: desktop usability pass — search, context menu, Clear completed, Open file, keyboard shortcuts, status-bar controls, notice severity, full translations. User guide: [usability.zh-TW.md](usability.zh-TW.md). Team brief: [team/usability-0.6/brief.md](team/usability-0.6/brief.md).

## Team

| Role | Model | Scope |
| --- | --- | --- |
| Lead | Opus | Contract, integration, review fixes, release |
| Slint UI | Haiku | `app.slint` layout, menu, shortcuts, icon, layout tests |
| Rust GUI | Haiku | `actions.rs`, search/clear/open/select handlers, unit tests |
| i18n / docs | Haiku | 251 strings × 7 languages, merging `gen_po.py`, user guide |
| Independent review | Sonnet | Read-only review; 12 findings, all HIGH/MEDIUM fixed |

## Evidence (Windows 10, local)

| Check | Result | Evidence |
| --- | --- | --- |
| Baseline before changes | PASS | strict Clippy and `cargo test --release --locked`, exit 0 |
| GUI format | PASS | `cargo fmt -p downloader --check` |
| Strict Clippy | PASS | `cargo clippy --release --workspace --all-targets --locked -- -D warnings` |
| Release build | PASS | `cargo build --release --locked` |
| Rust tests | PASS | `cargo test --release --locked --no-fail-fast`: GUI 45 (+2 ignored native fixtures), core 62, download 29, IPC/Drive/other 37 — 0 failed |
| Extension tests | PASS | `node --test scripts/tests/extension-background.test.cjs`: 16 passed |
| CLI smoke | PASS | `dlx --version` / `dlxctl --version` report 0.6.0; `dlx --help` exit 0 |
| Native GUI launch | PASS | Real Skia window with isolated state, zh_TW: toolbar incl. Clear completed, status-bar Compact/Tray/Exit, empty-state shortcut tip rendered |
| Translation files | PASS | Custom validator: 251/251 UI strings in every language, placeholders match, no empty msgstr |
| Windows installer smoke | NOT_RUN locally | `installer-smoke.ps1` installs machine-wide; runs in CI |
| Linux / macOS runtime | NOT_RUN | Windows host only; `open`/`xdg-open` paths are compile-checked |

## Review fixes applied

- Context-menu items act on the row selected by the right-press, not on a list index that a refresh may have shifted.
- Focus returns to the main window after the Add/Delete dialogs close; the Add dialog focuses its URL box. Mouse clicks no longer park focus on toolbar buttons (Space/Enter shortcuts keep working).
- A selection hidden by filter or search is cleared, so shortcuts never act on an invisible row.
- Open file uses an allowlist of passive types; everything else is revealed in its folder. The child process is reaped.
- Clear completed only removes visible completed rows. The clipboard watcher no longer re-prompts for links just pre-filled by Add.
- Test fix: Tools-window tests selected the language before any component existed, which Slint ignores; they passed only while the system-locale bundle lacked those strings.

## Limits

Translations are machine-authored and not reviewed by native speakers. Context-menu popups and real mouse/IME/screen-reader behaviour were not exercised by automated tests.
