# Dependencies and modules

Download protocols run in-process in Rust. Downloading does not require aria2, FFmpeg, yt-dlp, curl, Python or Node. Rust crates remain third-party libraries; no external download executable does not mean no third-party dependencies.

## Module boundaries

| Module | Responsibility |
|---|---|
| `core/src/engine.rs` | Segmented HTTP(S), resume and download lifecycle |
| `core/src/protocols/mod.rs` | Protocol dispatch, staging, integrity verification and completed-file publication |
| `core/src/protocols/ftp.rs` | FTP transfer and directory listing |
| `core/src/protocols/metalink.rs` | Metalink parsing, mirrors and advertised hashes |
| `core/src/protocols/torrent.rs` | BitTorrent / magnet through the Rust librqbit library |
| `core/src/protocols/media.rs` | HLS, static-page media, RTSP and MMS-over-HTTP |
| `core/src/protocols/media/parse.rs` | Shared media URL validation and parsing facade |
| `core/src/protocols/media/parse/playlist.rs` | HLS playlist parsing |
| `core/src/protocols/media/parse/discovery.rs` | Webpage media discovery |
| `core/src/protocols/media/tests.rs` | Media protocol regression fixtures and tests |
| `core/src/browser.rs`, `core/src/native.rs`, `core/src/bin/host.rs` | Browser registration, Native Messaging and application startup |
| `core/src/automation.rs` | Explicitly configured scheduled programs and shutdown |
| `gui/src/browser_setup.rs`, `gui/src/reveal.rs` | Browser launch, file location and preview |

The public `dlcore::external` API remains a compatibility alias for `dlcore::protocols`. Existing `.dlx-external-*` staging names remain unchanged to preserve paused downloads. Task and settings formats are unchanged.

## Remaining executable dependencies

| Feature | Executable or OS service | Trigger |
|---|---|---|
| Browser integration | Installed Chrome, Brave or Edge; this product's `downloader-host` | User installs extension or opens browser settings |
| Windows browser registration | `reg.exe` | Register or unregister Native Messaging host |
| Open extension folder | Windows `explorer.exe`, macOS `open`, Linux `xdg-open` | User opens folder |
| Open or preview completed files | Windows Shell / file associations; macOS `open`, Linux `xdg-open` | User opens completed file |
| Scheduled program | User-selected executable and arguments | Explicitly configured schedule |
| Scheduled shutdown | Windows `shutdown.exe` or Unix `shutdown` | Explicit shutdown schedule |

These integrations do not transfer download protocol data. They remain necessary for their existing features. OS permissions, file associations and desktop configuration affect their availability.

## Support boundaries

The media module does not replace the entire FFmpeg or yt-dlp feature set. It does not decrypt DRM, execute arbitrary website JavaScript, provide general transcoding or mux every container. RTSP and MMS transport limits are documented in the [feature matrix](fdm-feature-matrix.zh-TW.md).

Build and validation tools (Cargo/Rust, Node for extension tests, Python for translation generation and Inno Setup for Windows installers) are separate from end-user download dependencies. GUI rendering and native OS libraries remain platform dependencies declared in the Cargo manifests.
