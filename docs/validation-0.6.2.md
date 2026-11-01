# Downloader 0.6.2 validation — 2026-11-01

Base: `c77ef78` (0.6.1). Scope: Start at sign-in (minimized to the tray), Clear completed in the row menu, explicit delete choice. Guide: [usability.zh-TW.md](usability.zh-TW.md).

## Evidence (Windows 10, local)

| Check | Result | Evidence |
| --- | --- | --- |
| GUI format | PASS | `cargo fmt -p downloader --check` |
| Strict Clippy | PASS | `cargo clippy --release --workspace --all-targets --locked -- -D warnings` |
| Rust tests | PASS | `cargo test --release --locked --no-fail-fast`: GUI 48 (+2 ignored), core 65 (+3 autostart), download 29, other 29 — 0 failed |
| Sign-in entry written | PASS | Isolated state with `launch_at_login:true`: `HKCU\…\Run\Downloader` = `"…\downloader.exe" --tray`; value removed afterwards |
| `--tray` start hidden | PASS | Only the 14×14 winit event-target window is visible; no main window after 8 s |
| Second `--tray` launch | PASS | Exits at once, main window stays hidden |
| Plain launch while running | PASS | Existing main window (974×678) is shown |
| Translation files | PASS | `scripts/gen_po.py`: 254 strings × 7 languages |
| Row menu / delete dialog | NOT_RUN visually | Compiled and covered by Slint build; buttons call the existing `delete-confirmed` path with `del-file` set |
| Linux / macOS login entry | NOT_RUN | File content unit-tested; not exercised on a real session |

## Limits

The Windows uninstaller removes the Run value of the user who runs the uninstaller. Translations are machine-authored.
