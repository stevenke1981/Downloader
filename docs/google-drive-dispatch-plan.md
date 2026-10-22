# Google Drive download fix

- Base commit: `77f4d7821ea6540bf6ab618005d12086cff674e7` (clean worktree at intake).
- Request: use Agent Team Workflow and Odin IDE to fix Drive downloads saved as `identifier` instead of the intended file.
- Root owner: `/root`, implementation and integration in `E:\Downloader`; source scope `core/src/`, supporting manifest only if required, task evidence in this document and `google-drive-handoff.json`.
- Test owner: `drive-tests`, isolated worktree `target/worktrees/google-drive-tests`; owns new `core/tests/google_drive.rs` and associated fixture only. Reproduce wrong success on login/HTML responses, preserve normal downloads, record exact commands and exits.
- Extension owner: `drive-extension`, isolated worktree `target/worktrees/google-drive-extension`; owns `extension/background.js` and `scripts/tests/extension-background.test.cjs`. Diagnose Drive interception, implement only evidenced related fixes and tests.
- Dependencies: test and extension findings feed core owner; integration is serialized. No other author writes the main worktree.
- Odin project: `ws_be6d445980b6717b0e09d633291ddc8c25c589e4d6785fca`, confirmed root `E:\Downloader`; read-only grants. Used for live project and LSP analysis, with authorized local edits/builds through workspace tools.

## Acceptance

1. A Google login/error/confirmation page is never published as a completed Drive file named `identifier`.
2. Supported public Drive links resolve to file content and use its response filename; private links explain that browser authentication is required.
3. Extension preserves usable download URLs/metadata and avoids intercepting Google login HTML as a file.
4. Targeted regression tests fail on baseline and pass after the fix; required affected build/lint/test checks pass. Real Drive verification requires a suitable accessible link and is reported separately.
5. One final diff check; no push, release, account changes, or broadened Odin grants.

## Decisions and evidence

- Intake: `git status --short` empty. Odin live analysis and Rust LSP symbol overview succeeded; Odin write/shell grants are false.
- User confirmed `identifier` is Google login HTML and supplied a folder link. Connected Drive listing found three ZIP members; no account/sharing setting was changed.
- Baseline regressions reproduced the defect: five of eight core Drive integration cases failed; extension's final fixture also had five baseline failures.
- Implemented core resolution/page rejection and extension URL/cookie handling. Candidate Drive integration 8/8 and extension 16/16 passed. Single independent scoped check found no evidenced blocking issue.
- `scripts/verify-local.ps1` exited 0: GUI format, strict release Clippy, release build, 87 Rust tests (one existing native fixture ignored), 16 extension tests, JS checks and CLI version smoke.
- Release CLI live checks: public single-file share downloaded correct `spam.txt` bytes; private member returned explicit sign-in failure with no output; folder URL returned browser download guidance with no output.
- Inno Setup 7 built the local installer; extension ZIP integrity and source hash checks passed. No installed-app upgrade, browser-profile modification, remote CI, push or release was performed.
- A shared Cargo target initially reused baseline worktree artifacts. Cleaning only `downloader-core` dev artifacts and rebuilding removed the stale result; all final release verification used the main worktree candidate.
- Final delivery: `docs/google-drive-fix.md`, `docs/google-drive-handoff.json`, `dist/downloader-setup-google-drive-fix-windows-x64.exe`, and `dist/downloader-browser-extension-google-drive-fix.zip`.
