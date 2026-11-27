# Downloader 0.9.0 validation — 2026-11-27

Base: `c62947b` (0.8.1, see [validation-0.8.1.md](validation-0.8.1.md)). Scope: YouTube video (H.264 + AAC muxed in process into MP4), YouTube audio as MP3, X (Twitter) post video, title-based file names, and the agent CLI (`dlxctl doctor`, `dlx --schema`, `dlxctl help --json`).

## Evidence (Windows 11, local)

| Check | Result | Evidence |
| --- | --- | --- |
| Format | PASS | `cargo fmt -p downloader --check`; `rustfmt --edition 2024 --check` on every changed core file |
| Strict Clippy | PASS | `cargo +stable-x86_64-pc-windows-msvc clippy --release --workspace --all-targets --locked -- -D warnings` |
| Core tests | PASS | `cargo test --release --locked -p downloader-core`: 251 passed, 0 failed (core unit 130) |
| Routing | PASS | `registry::tests::site_items_route_to_media`: YouTube and X item links go to the media adapter in Auto mode; channels, playlists and profiles do not |
| YouTube split download | PASS | `media::site::tests::youtube_split_streams_are_muxed_and_named_after_the_title`: local InnerTube player plus fragmented video and audio served in 64-byte ranges (one range fails once and is retried) produce a muxed MP4 named after the title, with the client's User-Agent on every media request |
| Not-fragmented fallback | PASS | `youtube_falls_back_to_progressive_when_streams_are_not_fragmented` |
| YouTube audio as MP3 | PASS | `youtube_audio_mode_produces_mp3` (fragmented AAC fixture converted in process) |
| X post video | PASS | `x_post_video_is_downloaded_and_named` (local syndication endpoint) |
| No external processes | PASS | `tests/external.rs` covers `media/site.rs`, `media/mp4mux.rs`, `media/mp3.rs`, `sites/*` |
| Live YouTube and X downloads | NOT_RUN | All tests are offline; the public APIs were not called |

## Limits

YouTube resolution depends on InnerTube clients (ANDROID_VR, then IOS for HLS, then ANDROID) that YouTube can change or restrict without notice; streams that need signature JavaScript are skipped, so some videos report `YouTube: no directly downloadable stream`. Video is capped at 1080p H.264; AV1, VP9, WebM and Opus are not used. Channels, playlists and profiles are not expanded. Site downloads restart from the beginning after an interruption. The other 0.8.1 limits are unchanged.
