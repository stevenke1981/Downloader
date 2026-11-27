# Dependencies and modules

Download protocols run in-process in Rust. Downloading does not require aria2, FFmpeg, yt-dlp, curl, Python or Node. Rust crates remain third-party libraries; no external download executable does not mean no third-party dependencies.

## Module boundaries

| Module | Responsibility |
|---|---|
| `core/src/engine.rs` | HTTP(S) download lifecycle and verification |
| `core/src/engine/http.rs` | HTTP probing, origin checks and range validation |
| `core/src/engine/segments.rs` | Segment planning and work allocation |
| `core/src/engine/worker.rs` | Concurrent segment transfers and retry handling |
| `core/src/manager.rs` | Download manager API and shared state |
| `core/src/manager/models.rs` | Public download requests and snapshots |
| `core/src/manager/scheduler.rs` | Queue dispatch and progress updates |
| `core/src/manager/automation.rs` | Scheduled actions |
| `core/src/manager/persistence.rs` | State loading and atomic saves |
| `core/src/protocols/mod.rs` | Protocol dispatch, staging, integrity verification and completed-file publication |
| `core/src/protocols/ftp.rs` | FTP transfer and directory listing |
| `core/src/protocols/ftp/connection.rs` | FTP control and passive connections |
| `core/src/protocols/metalink.rs` | Metalink parsing, mirrors and advertised hashes |
| `core/src/protocols/metalink/descriptor.rs` | Metalink descriptor parsing |
| `core/src/protocols/metalink/http.rs` | Origin-scoped Metalink HTTP requests |
| `core/src/protocols/torrent.rs` | BitTorrent / magnet lifecycle through the Rust librqbit library |
| `core/src/protocols/torrent/source.rs` | Torrent source retrieval and validation |
| `core/src/protocols/torrent/tests.rs`, `core/src/protocols/torrent/tests/fixture.rs` | Torrent regressions and shared swarm fixture |
| `core/src/protocols/media.rs` | Shared media transfer state, HTTP and protocol dispatch |
| `core/src/protocols/media/hls.rs` | HLS transfer |
| `core/src/protocols/media/rtsp.rs` | RTSP capture and G.711 audio conversion |
| `core/src/protocols/media/mmsh.rs` | MMS-over-HTTP capture and ASF packet handling |
| `core/src/protocols/media/mp3.rs` | In-process AAC (mp4/m4a) to MP3 conversion: `symphonia` decodes, `rusty_mp3` encodes, both pure Rust |
| `core/src/protocols/media/mp4mux.rs` | In-process fragmented MP4 muxer: joins DASH video-only and audio-only files into one MP4, streaming media data |
| `core/src/protocols/media/site.rs` | Site downloads: ranged googlevideo transfer, mux or MP3 conversion, title naming |
| `core/src/protocols/sites/youtube.rs` | YouTube item URLs and InnerTube player resolution (direct H.264/AAC streams only) |
| `core/src/protocols/sites/x.rs` | X / Twitter post URLs and syndication resolution (mp4, else HLS) |
| `core/src/protocols/media/parse.rs` | Shared media URL validation and parsing facade |
| `core/src/protocols/media/parse/playlist.rs` | HLS playlist parsing |
| `core/src/protocols/media/parse/discovery.rs` | Webpage media discovery |
| `core/src/protocols/media/tests.rs` | Media protocol regression fixtures and tests |
| `core/src/browser.rs`, `core/src/native.rs`, `core/src/bin/host.rs` | Browser registration, Native Messaging and application startup |
| `core/src/browser/registration.rs` | Browser host registration and status |
| `core/src/automation.rs` | Explicitly configured scheduled programs and shutdown |
| `gui/src/browser_setup.rs`, `gui/src/reveal.rs` | Browser launch, file location and preview |
| `gui/src/view.rs`, `gui/src/tools/ftp.rs` | Window models and FTP tool input handling |

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
| Approved external module (optional) | The module's own `command` in `<state dir>/modules/<id>/` | Settings enable modules and the module is approved by SHA-256 |

Built-in site resolvers (`protocols/sites`) take single YouTube video and X post links before any external resolver module is consulted; modules still see every other link.

These integrations do not transfer download protocol data. An approved external module only returns URLs, headers and mirrors (or receives events); the downloader still performs the transfer. Modules are described in [modules](modules.md) and the command-line tools in [cli](cli.md). They remain necessary for their existing features. OS permissions, file associations and desktop configuration affect their availability.

## Support boundaries

The media module does not replace the entire FFmpeg or yt-dlp feature set. It does not decrypt DRM, execute arbitrary website JavaScript, provide general transcoding or mux every container. The one conversion it does is MP3 output for downloaded AAC audio (the `--audio` mode), done in-process by `symphonia` (demux and AAC decode) and `rusty_mp3` (MP3 encode), both pure Rust with no C library and no subprocess. RTSP and MMS transport limits are documented in the [feature matrix](fdm-feature-matrix.zh-TW.md).

Build and validation tools (Cargo/Rust, Node for extension tests, Python for translation generation and Inno Setup for Windows installers) are separate from end-user download dependencies. GUI rendering and native OS libraries remain platform dependencies declared in the Cargo manifests.
