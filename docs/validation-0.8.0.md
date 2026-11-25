# Downloader 0.8.0 validation — 2026-11-26

Base: `d107c81` (0.7.0). Scope: HTTP mirror fallback, `dlx` hook delivery, `--mirror` for `dlx` / `dlxctl` / the remote API, resolver mirrors kept out of saved state, `final_url` no longer saved, `not enough disk space:` on a full-disk write, Tools > Modules state fix. Plan and review: [team/v0.8/plan.md](team/v0.8/plan.md), [team/v0.8/review.md](team/v0.8/review.md). Guides: [cli.md](cli.md#mirrors), [modules.md](modules.md).

## Evidence (Windows 11, local)

| Check | Result | Evidence |
| --- | --- | --- |
| Format | PASS | `cargo +1.96.1-x86_64-pc-windows-msvc fmt --all --check` |
| Strict Clippy | PASS | `cargo +1.96.1-x86_64-pc-windows-msvc clippy -j 4 --release --workspace --all-targets --locked -- -D warnings` |
| Rust tests | PASS | `cargo +1.96.1-x86_64-pc-windows-msvc test -j 4 --release --locked --workspace`: 250 passed, 0 failed, 2 ignored (GUI 49 + 2 ignored, core unit 87, `cli` 17, `download` 29, `essentials` 16, `external` 6, `google_drive` 8, `ipc` 8, `management_services` 8, `mirrors` 11, `modules` 11) |
| Release build | PASS | `cargo +1.96.1-x86_64-pc-windows-msvc build -j 4 --release --locked`; `dlx --version` prints 0.8.0 |
| Mirror fallback, real binary | PASS | `dlx --retries 0 --mirror http://127.0.0.1:8765/f.bin http://127.0.0.1:9/f.bin`: main URL refused, mirror served by a local HTTP server, 3,000,000 bytes identical (`cmp`), exit 0; again with `--on-conflict overwrite --json`: `done` then `summary` with exit 0, one `f.bin` left |
| Hook wait and Ctrl+C in `dlx` | PASS | Approved `dlx-module-example --sleep` hook, `timeout_ms` 60000; real CTRL_C during the wait: exit 130 after 1.8 s, no hook process left, last NDJSON line is `summary` |
| GUI: add and download | PASS | Release app with `DOWNLOADER_STATE_DIR` set to a scratch folder, driven by desktop automation: Add dialog, URL, Ctrl+Enter; download completed (2.9 MB, 100%); queue reloaded after restart |
| GUI: Tools > Modules | PASS | Tab lists four built-ins at 0.8.0 and the example module. Program missing: red error, no button (fixed in this release; 0.7.0 showed Revoke). Program present + Enable: "Not approved" with Approve; Approve: "Approved" with Revoke, hash written to `settings.json` |
| GUI: hook from the app | PASS | With the module approved, a download added in the app wrote one `completed` event (path, size 3000000, id 2) to the module's `requests.log` |
| Disk-space precheck on a nearly full disk | NOT_RUN | Not run against a nearly full volume; the StorageFull mapping is covered by a unit test |
| Installer build (Inno Setup) | NOT_RUN | `scripts/build-installer.ps1` not run in this pass |
| Non-Windows systems | NOT_RUN | Linux and macOS were not built or run |

## Limits

The disk-space precheck before a download starts is Windows-only; elsewhere a full disk is reported when a write fails (`not enough disk space: write failed: ...`). A same-size resume on a mirror without a checksum mixes bytes from two sources. The Tools window opens taller than a 1080-pixel screen when Windows cascades it low on the screen; its bottom status line can sit under the taskbar (unchanged from 0.7).
