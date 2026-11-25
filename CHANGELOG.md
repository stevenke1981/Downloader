# Changelog

## 0.8.0 — 2026-11-26

### Added
- **HTTP mirror fallback** ([docs/cli.md](docs/cli.md#mirrors)): when a download's link fails (network error, timeout, 408, 429, 5xx, 404 or 410), its mirrors are tried in order, at most 8, each with the usual retries. A mirror must report the same size; a partly downloaded file continues on a mirror only if it also supports range requests, otherwise the mirror is skipped and the partial file is kept. 401, 403, 407, file conflicts, a full disk, verification failures and cancels never switch to a mirror. When every source fails you get the main link's error, so exit codes are unchanged.
- `dlx --mirror URL` and `dlxctl add --mirror URL` (repeatable, http(s) only); `mirrors` in the remote API's add body.
- `dlx --modules-dir` delivers `completed` and `failed` events to hook modules and waits for them (bounded by each module's timeout) before exiting. Output and exit codes are unchanged; Ctrl+C still exits at once.

### Changed
- Mirrors returned by a resolver module are tried after your own and are no longer saved with the download or exported.
- A write that fails because the disk is full now reports `not enough disk space:` (exit class `disk`) on every system, not only through the Windows precheck.
- Mirrors that are not http(s) are refused when a download is added or imported.

### Fixed
- **Tools > Modules** no longer offers **Revoke** for a module that fails to load and was never approved; it shows only the error.

### Isolation
- Cookies and Basic credentials go to a mirror only on the main link's origin, custom headers and the referer only on the downloaded link's origin, and a resolver's headers only on its own origin. The User-Agent goes everywhere. Logs name a mirror by origin only.

### Known limits
- The disk-space precheck before a download starts is still Windows-only; elsewhere a full disk is reported when a write fails.
- With no checksum, a same-size resume on a mirror mixes bytes from two sources. Give `--verify` / a checksum when a mirror is not fully trusted.

## 0.7.0 — 2026-11-18

### Added
- **Modules** ([docs/modules.md](docs/modules.md)): built-in protocols now sit behind one registry, unchanged in behaviour. Optional external modules live in `<state dir>/modules/<id>/module.json`. A *resolver* can turn a link into the real download link and add headers, a file name or mirrors. A *hook* is told in the background when a download completes or fails. Modules are off by default: enable them in **Tools > Modules** or with `dlxctl modules enable`, then approve each module. An approval is the SHA-256 of that module's `module.json`; editing the file disables the module. Programs start from an argument list, never a shell, with a timeout and a bounded output. An example resolver is in `modules/examples/example-resolver`.
- **File-exists policy**: when the name is taken, save as `name (1).ext` (default), **overwrite** (the old file is replaced only after the new one verifies), or **skip** (fails before transferring when the name is known in advance). Set it per download (`--on-conflict`, the `conflict` field) or as the default in **Tools > Sites / folders**.
- **Custom headers and User-Agent** for downloads (`dlx --header`, `--user-agent`, and the same fields in the remote API). Cookie, Authorization, Host, Range, If-Range and Content-Length are refused. Custom headers go only to the download's own origin and are dropped on a cross-origin redirect.
- **Duplicate detection**: `dlxctl add --unique` or `unique: true` in the remote API refuses a URL that is already queued or active.
- **Disk-space precheck** (Windows): a download stops before writing when the space it still needs is larger than the free space.
- **Queue export and import**: `dlxctl export` / `import`, `GET /export` and `POST /import`. Only unfinished downloads are exported, with no cookies, credentials or logs. Imports are added paused unless started.
- **Remote API**: `GET /downloads/{id}`, `GET /export`, `POST /import`, `GET /modules`, `GET /version`. Adding a download accepts `priority`, `checksum`, `referer`, `headers`, `user_agent`, `unique` and `conflict`. Errors are `{"error", "code"}`; duplicates and conflicts return 409.
- **Tools > Modules** tab (enable switch, module list, Approve / Revoke, Open modules folder) and the file-exists policy combo.

### Changed
- `settings.json` gains `conflict` and `modules`. Existing 0.6.x `settings.json` and `downloads.json` files load unchanged.
- Modules run only through `dlx --modules-dir <state dir>/modules` or the app; approvals are never accepted from the remote API, an import file or a settings form.

### Fixed
- A module's resolved link and extra headers are never saved, so a revoked or disabled module cannot leave stale state behind.

### For agents & scripts
- `dlx --json` writes one NDJSON event per line on stdout (`start`, `progress`, `done`, `error`, `summary`); human text stays on stderr. `dlx --tools --json` lists the embedded engines.
- `dlxctl --json` prints `{"ok":true,"data":...}` or `{"ok":false,"error":{"code","message"}}`.
- Stable exit codes: 0 success, 1 failed, 2 usage or invalid input, 3 network, 4 auth, 5 verification, 6 conflict or duplicate, 7 module, 130 interrupted. Error codes match the `code` field.
- New `dlxctl` commands: `status`, `wait` (with `--timeout`), `export`, `import`, `modules list | approve | revoke | enable | disable`.
- [AGENTS.md](AGENTS.md) for coding agents and [docs/cli.md](docs/cli.md) for the CLI reference.

### Known limits
- HTTP mirror fallback is not implemented. Mirrors a resolver returns are stored but never tried.
- The disk-space precheck is Windows-only; other systems start the download and fail on write.
- `dlx` does not deliver completion events to hook modules. A running app or `dlxctl serve` does.
- The development-only `dlx-module-example` binary is not included in the Windows packages or the installer.

## 0.6.2 — 2026-11-01

### Added
- Settings → General → **Start at sign-in (minimized to the tray)**: registers a per-user login entry (Windows `HKCU\…\Run`, XDG autostart on Linux, a LaunchAgent on macOS) that starts the app with `--tray`. The window stays hidden unless no tray icon appears within about 5 s. A `--tray` launch while the app already runs does not pop it up. The uninstaller removes the entry. Off by default.
- **Clear completed** is also in each download's right-click menu.

### Changed
- The delete dialog asks explicitly: **Remove record only** (default) or **Delete record and file**, replacing the "Also delete the file from disk" checkbox.

## 0.6.1 — 2026-11-01

### Added
- Settings → General → **Download copied links automatically**: with clipboard watching on, copied download links are queued and started at once with the default folder and options, without opening the Add dialog or restoring the window; a notice reports how many were added. Off by default.

### Fixed
- Clipboard watching no longer loses a copied link while the app sits in the tray or the compact window: the main window is restored and the Add dialog shown. Previously the dialog opened on the hidden window and watching stalled until it was closed.
- The watcher pre-fills only the new download links from the copied text, not surrounding text or links already in the list.

## 0.6.0 — 2026-10-31

### Usability
- Live search above the download list filters by file name or URL; sidebar counts stay unfiltered.
- Right-click a download for Start/Resume, Pause, Open file, Open folder, Copy URL and Delete.
- New toolbar action **Clear completed** removes the completed downloads currently shown (filter and search apply) from the list; files stay on disk.
- **Open file** opens common media, document, image, archive and text files with the default app. Programs, scripts, shortcuts, macro documents and unknown types are shown in their folder instead of being run.
- Keyboard shortcuts: Ctrl+N add, Ctrl+V add with clipboard links, Ctrl+F search, Ctrl+, settings, Delete, Space start/pause, ↑/↓ move selection (scrolls into view), Enter open completed file, Esc clears search or closes dialogs, Ctrl+Enter submits the add dialog.
- The Add dialog pre-fills download links found on the clipboard and focuses the URL box.
- Compact window, Minimize to tray and Exit moved from a separate top row into the status bar; notices are neutral for information and red only for errors; the delete dialog names the download.
- All 251 UI strings are now translated in every language (previously ~100 newer strings were English outside 繁體中文). `scripts/gen_po.py` scans every `.slint` file and merges instead of overwriting.
- Guide: [docs/usability.zh-TW.md](docs/usability.zh-TW.md).

### Changed
- `gui/ui/app.slint` split into `tr.slint` (strings, row types), `widgets.slint` (shared controls) and `compact.slint`.

### Added
- Align the FDM feature checklist with in-process Rust FTP/listing, BitTorrent/magnet, Metalink, HLS/webpage-media, RTSP H.264/G.711 and MMS-over-HTTP engines. External aria2, FFmpeg, yt-dlp and curl launchers have been removed.
- Batch URL paste/import, main and compact window descriptor drops, completed-file preview, copy-source sharing, task connection/status logs, and a separate desktop Tools window.
- Configurable low/medium/high traffic presets, origin-specific site concurrency/connection/authentication profiles with session-only passwords, and custom extension-to-folder rules.
- Bounded same-origin recursive HTML link discovery, persisted one-shot start/pause/program/shutdown actions, and an opt-in token-authenticated remote queue API with the `dlxctl` command-line tool.
- Add bounded in-app image/text/directory previews and media/document metadata, without launching a registered viewer. Audio/video playback and PDF/EPUB rendering are not implemented.
- Document the feature matrix and layered local/runtime/GUI acceptance, including protocol-specific resume, proxy, media-format and shared torrent pacing limits. Legacy Google Video/Flash services are not recreated.

### Fixed
- Discard stale torrent payload when a descriptor changes info hash; publish actual payload names and media extensions instead of browser descriptor filenames.
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
