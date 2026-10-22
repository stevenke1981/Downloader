# Changelog

## Unreleased

### Fixed
- Google Drive downloads no longer save Google sign-in HTML as a completed file named `identifier`. Public file share links resolve download confirmation forms and preserve response filenames; folder links explain how to download from the browser.
- Browser interception leaves Google sign-in/error pages in the browser, preserves resolved Drive media URLs with their matching cookie scope, and avoids intercepting a redirected browser fallback twice.

## 0.5.2 — 2026-10-22

### Improved
- Settings now separates General, Downloads and Browser, with a pinned Save/Cancel footer, keyboard navigation and localized guidance for small windows.
- Chrome, Brave and Edge extension setup is available inside Settings: native-host health checks and repair, extension-folder discovery/copy, and opening the selected browser's extensions page. Loading the unpacked extension remains a browser-profile action.
- Empty and filtered download lists offer relevant next actions, and toolbar/sidebar controls support keyboard focus.
- Native-host checks validate the actual registry view, manifest identity/origin, executable and current installation. Windows installer registration runs as the original user; macOS app bundles include the extension.
- `scripts/verify-local.ps1` provides local Windows checks without GitHub Actions.

### Fixed
- Reject invalid or misaligned HTTP partial responses before writing downloaded bytes, while allowing valid shorter prefixes and unknown complete lengths.
- Browser download cancellation failures no longer erase history or start a duplicate transfer in Downloader.

## 0.5.1 — 2026-10-18

### Fixed
- A corrupt `downloads.json` that can't be moved aside to `downloads.json.corrupt` is no longer overwritten by the next autosave; the session is saved to `downloads.json.recovered` instead, as for an unreadable file.
- "Download all links" now sends path-scoped login cookies (e.g. `Path=/members`): cookies are looked up per origin + path instead of once per origin at `/`.

## 0.5.0 — 2026-10-18

Includes the 0.4.x desktop-controls preview (PR #1: Windows tray, compact window, file reveal, small-screen layout) plus the fixes below.

### Fixed
- Downloads no longer fail after a few dropped connections when each attempt made progress: the per-connection retry counter resets after progress (range downloads).
- HTTP 408 and 429 are retried instead of failing the download immediately.
- The speed shown for a resumed download no longer starts with a huge bogus spike; speeds are only measured by the timer with the real elapsed time.
- A corrupt `downloads.json` / `settings.json` is moved aside to `*.corrupt` instead of being silently replaced by an empty list on the next save. State files are flushed to disk (`sync_all`) before the atomic rename. If `downloads.json` exists but can't be read at startup (e.g. locked by antivirus), it is left untouched and the session is saved to `downloads.json.recovered`.
- Cookies are only sent to the redirect target when it has the same origin (scheme, host, port) as the original URL; unusable cookie values are dropped instead of failing the download. After a restart a login-protected download that fails with 401/403 explains that cookies are not kept.
- File names that are Windows device names (`CON`, `NUL`, `COM1`, … also with an extension) are prefixed with `_`; over-long names are truncated to 200 bytes keeping the extension.
- `ftp://` links are no longer offered from the clipboard / command line (FTP is not supported).
- `--limit inf` / `nan` is rejected; `dlx` no longer underflows its final speed when the size is unknown and handles a crashed download task without panicking.
- Single click on a completed download only selects it; double-click (or "Open folder") opens the file location. Previously every click opened a file manager window.
- An empty download folder is rejected in Settings; an invalid speed limit in the details pane shows an error instead of being ignored.
- Slint layout binding loop around the details pane (deprecated, could panic in future Slint versions) and the `viewport-*` deprecation warnings are gone.
- The 0.4 desktop-control strings were only translated to Traditional Chinese; all 8 languages are now complete, and `scripts/gen_po.py` is the single source again.

### Improved
- Browser extension forwards the site's cookies (option "send cookies", on by default) so downloads that need a login work. Cookies travel only over the local IPC connection, are kept in memory and never written to disk.
- When the app can't be reached the browser fallback keeps the original file name.
- Checksum discovery probes candidate files concurrently (6 at a time, in priority order) instead of one by one.
- The GUI keeps persistent list models and updates only changed rows instead of rebuilding every list twice a second; one snapshot per tick; the compact list updates only while visible.
- Autosave skips writing `downloads.json` when nothing changed, and saves triggered from the GUI run off the UI thread.
- The compact window shows current rows as soon as it opens.
- IPC: per-connection limit raised to 8 MiB (large "download all links" batches), constant-time token comparison, a connection with a bad token is closed, and on Unix `ipc.json` is created with mode 0600.
- CI runs clippy with `-D warnings`; all clippy warnings fixed.
