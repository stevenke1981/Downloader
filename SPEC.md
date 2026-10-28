# FDM alignment implementation contract

Read alongside PLAN.md. Boundary validation belongs to the public input API; internal typed values are not repeatedly revalidated. Keep existing HTTP engine behavior and ephemeral Cookie handling.

## Protocol contract (protocol worker owns implementations)

- `task::DownloadKind`: serde/default enum `Auto` (default), `Video`.
- `Task.kind: DownloadKind`, `Task.authorization: Option<String>` (`serde(skip)`, HTTP Basic Authorization value prepared by management), `Task.log: Vec<String>` (bounded, no secrets).
- `engine::run` routes explicit Video and non-HTTP sources through `external`, retaining native HTTP(S). HTTP Basic auth is sent only to the matching original origin, including probe and segment fetch; never forward across origins.
- Accept FTP, RTSP/MMS, magnet, HTTP(S) torrent/metalink descriptors and local `.torrent/.metalink/.meta4` files. Do not treat arbitrary local files as download URLs.
- `external::capabilities() -> Vec<Capability>`; `Capability` exposes `name: String`, `available: bool`, `path: Option<PathBuf>`.
- `external::list_ftp(url: &str, username: &str, password: &str) -> impl Future<Output=Result<Vec<String>,String>>` lists directory entries via an available external tool. Do not persist credentials embedded in URLs.
- Tool lookup supports PATH and explicit `DOWNLOADER_ARIA2`, `DOWNLOADER_FFMPEG`, `DOWNLOADER_YTDLP`, `DOWNLOADER_CURL`. No bundled third-party binaries in Git.
- External output discovery must be scoped to a per-task staging directory; a successful exit alone does not mean a download exists. Multi-file torrent/Metalink must have a truthful output path. Explicit checksums must not be ignored.

## Management contract (management worker owns implementations)

- `NewDownload.kind: DownloadKind` (default Auto). Manager maps kind to Task.
- Settings adds `traffic_mode: TrafficMode` (`Custom/Low/Medium/High`), `traffic_presets: [u64;3]`, `site_rules: Vec<SiteRule>`, `folder_rules: Vec<FolderRule>`, `scheduled_actions: Vec<ScheduledAction>`, `remote: RemoteSettings`; serde(default) retains old files.
- `SiteRule { host: String, max_concurrent: u32, connections: u32, username: String }`. `Manager::set_site_credentials(host: &str, username: &str, password: &str) -> Result<(),String>` holds passwords only in memory; origin isolation is mandatory. Persist profile identity, not passwords.
- `FolderRule { extensions: Vec<String>, folder: PathBuf }`. Settings validates rules on save, first matching custom extension rule overrides category folder.
- `TrafficMode` and the above settings types live in public `settings` or `site` modules. `Manager::set_traffic_mode(mode: TrafficMode) -> Result<(),String>` updates actual global limiter and persists.
- `grabber::GrabOptions { max_depth: usize, max_pages: usize }` (Default), `grabber::crawl(client: &CrawlClient, url: &str, options: GrabOptions) -> impl Future<Output=Result<Vec<String>,String>>`. `CrawlClient` owns a private no-auto-redirect reqwest client; `Settings::build_crawl_client()` preserves proxy/timeouts and creates the typed boundary. Same-origin, bounded HTML reads/pages/depth; resolve relative links and dedup; no script execution or Google Drive folder enumeration.
- `automation::ScheduledAction { id: u64, at: i64, action: Action, consumed: bool }`; `Action` enum `StartAll`, `PauseAll`, `Run { program: String, args: Vec<String> }`, `Shutdown`. Epoch seconds, consumed persisted before execution; no implicit repeat after restart. Manager tick applies actions. Expose status/error through an automation log or task logs.
- `remote::RemoteSettings { enabled: bool, bind: String, token: String }` (default disabled + loopback), `remote::serve(manager: Manager, settings: RemoteSettings) -> Result<RemoteServer,String>`; RemoteServer RAII stop/address getter. GUI starts/restarts/stops server on settings apply. Authenticated JSON HTTP: GET /downloads; POST /downloads; POST /downloads/{id}/start or /pause; DELETE /downloads/{id}; POST /start-all or /pause-all. Do not expose settings/credentials or allow remote shell/shutdown.
- `dlxctl` provides remote control/crawl/site/automation operations as practical. Full APIs must be usable from GUI without raw JSON editing for ordinary operations.

## GUI contract

Desktop worker implements against these interfaces, coordinates exact discrepancies before coding, owns all GUI changes. Preserve existing layout and settings tabs and create a separate tools window/dialog if useful. Long work uses worker threads/runtime + weak UI handle; no blocking subprocess/network on GUI thread. Batch input is atomically validated before queueing, deduped, line-specific error. Local torrent/Metalink import and drop is supported; floating compact window remains available as drop target if backend permits. Preview opens completed output with platform default viewer; copy URL is explicit sharing, connection/status transitions form useful logs. No destructive program/OS action is executed during automated GUI tests.

## Source references

- https://zh.wikipedia.org/zh-tw/Free_Download_Manager (functional checklist)
- https://aria2.github.io/manual/en/html/aria2c.html (FTP, BitTorrent, magnet, Metalink argv)
- https://ffmpeg.org/ffmpeg.html (stream capture argv)
- https://github.com/yt-dlp/yt-dlp (explicit website-video mode)
- https://curl.se/docs/manpage.html (FTP listing argv)

Public API changes must be coordinated between authors; only protocol worker changes Cargo.lock. Root owns module exports and shared dispatch documentation.
