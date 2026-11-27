//! Built-in protocol modules: in-process Rust adapters behind one small trait.
//! Order in [`builtin`] is the dispatch precedence.
use crate::engine::{Ctx, Outcome};
use crate::task::{DownloadKind, Task};
use crate::util;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

/// The future an adapter returns.
pub type DownloadFuture = Pin<Box<dyn Future<Output = Result<Outcome, String>> + Send>>;

/// An in-process protocol adapter. Adapters write into `stage/payload` (and may keep
/// private state in `stage/state`); the facade verifies and publishes.
pub trait ProtocolModule: Sync {
    /// Stable id, e.g. "ftp".
    fn id(&self) -> &'static str;
    /// Human name, e.g. "FTP".
    fn name(&self) -> &'static str;
    /// Whether this adapter takes `source`. Checked in [`builtin`] order.
    fn matches(&self, source: &str, kind: DownloadKind) -> bool;
    /// Download into the canonical staging folder `stage`.
    fn download(&self, task: Arc<Mutex<Task>>, ctx: Ctx, stage: PathBuf) -> DownloadFuture;
    /// Name for a single published file. `actual` and `suggested` are already sanitized.
    fn publish_name(&self, actual: String, suggested: Option<String>, _file: &Path) -> String {
        suggested.unwrap_or(actual)
    }
}

/// Scheme of a URL source; `None` for local descriptors and unparsable input.
fn scheme(source: &str) -> Option<String> {
    if util::descriptor_path(source).is_some() {
        return None;
    }
    reqwest::Url::parse(source)
        .ok()
        .map(|u| u.scheme().to_string())
}

/// A single YouTube video or X post URL, which the built-in site resolvers take.
fn site_item(source: &str) -> bool {
    use super::sites::{x, youtube};
    reqwest::Url::parse(source)
        .is_ok_and(|u| youtube::video_id(&u).is_some() || x::status_id(&u).is_some())
}

/// Lowercase extension of a local descriptor or an HTTP(S) URL path.
fn extension(source: &str) -> Option<String> {
    let ext = |p: &Path| Some(p.extension()?.to_str()?.to_ascii_lowercase());
    if let Some(path) = util::descriptor_path(source) {
        return ext(&path);
    }
    let url = reqwest::Url::parse(source).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    ext(Path::new(url.path()))
}

struct Media;
struct Ftp;
struct Torrent;
struct Metalink;

impl ProtocolModule for Media {
    fn id(&self) -> &'static str {
        "media"
    }
    fn name(&self) -> &'static str {
        "HLS / RTSP / MMS / web media"
    }
    fn matches(&self, source: &str, kind: DownloadKind) -> bool {
        matches!(kind, DownloadKind::Video | DownloadKind::Audio)
            || site_item(source)
            || scheme(source)
                .is_some_and(|s| matches!(s.as_str(), "rtsp" | "mms" | "mmsh" | "mmst"))
            || extension(source).as_deref() == Some("m3u8")
    }
    fn download(&self, task: Arc<Mutex<Task>>, ctx: Ctx, stage: PathBuf) -> DownloadFuture {
        Box::pin(async move { super::media::download(task, ctx, &stage).await })
    }
    fn publish_name(&self, actual: String, suggested: Option<String>, file: &Path) -> String {
        suggested.map_or(actual, |name| {
            let mut name = PathBuf::from(name);
            name.set_extension(file.extension().unwrap_or_default());
            name.to_string_lossy().into_owned()
        })
    }
}

impl ProtocolModule for Ftp {
    fn id(&self) -> &'static str {
        "ftp"
    }
    fn name(&self) -> &'static str {
        "FTP"
    }
    fn matches(&self, source: &str, _kind: DownloadKind) -> bool {
        scheme(source).as_deref() == Some("ftp")
    }
    fn download(&self, task: Arc<Mutex<Task>>, ctx: Ctx, stage: PathBuf) -> DownloadFuture {
        Box::pin(async move { super::ftp::download(task, ctx, &stage).await })
    }
}

impl ProtocolModule for Torrent {
    fn id(&self) -> &'static str {
        "torrent"
    }
    fn name(&self) -> &'static str {
        "BitTorrent / magnet"
    }
    fn matches(&self, source: &str, _kind: DownloadKind) -> bool {
        scheme(source).as_deref() == Some("magnet")
            || extension(source).as_deref() == Some("torrent")
    }
    fn download(&self, task: Arc<Mutex<Task>>, ctx: Ctx, stage: PathBuf) -> DownloadFuture {
        Box::pin(async move { super::torrent::download(task, ctx, &stage).await })
    }
    // Descriptor names describe the request, not the downloaded payload.
    fn publish_name(&self, actual: String, _suggested: Option<String>, _file: &Path) -> String {
        actual
    }
}

impl ProtocolModule for Metalink {
    fn id(&self) -> &'static str {
        "metalink"
    }
    fn name(&self) -> &'static str {
        "Metalink"
    }
    fn matches(&self, source: &str, _kind: DownloadKind) -> bool {
        extension(source).is_some_and(|e| e == "metalink" || e == "meta4")
    }
    fn download(&self, task: Arc<Mutex<Task>>, ctx: Ctx, stage: PathBuf) -> DownloadFuture {
        Box::pin(async move { super::metalink::download(task, ctx, &stage).await })
    }
    fn publish_name(&self, actual: String, _suggested: Option<String>, _file: &Path) -> String {
        actual
    }
}

static BUILTIN: [&'static dyn ProtocolModule; 4] = [&Media, &Ftp, &Torrent, &Metalink];

/// All built-in protocol modules in precedence order: website video mode, then scheme, then extension.
pub fn builtin() -> &'static [&'static dyn ProtocolModule] {
    &BUILTIN
}

/// The first built-in module that takes `source`.
pub fn find(source: &str, kind: DownloadKind) -> Option<&'static dyn ProtocolModule> {
    builtin().iter().copied().find(|m| m.matches(source, kind))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The 0.6 dispatch function, kept verbatim to prove precedence is unchanged.
    fn legacy(source: &str, kind: DownloadKind) -> Option<&'static str> {
        if kind == DownloadKind::Video {
            return Some("media");
        }
        let extension = if let Some(path) = util::descriptor_path(source) {
            path.extension()?.to_str()?.to_ascii_lowercase()
        } else {
            let url = reqwest::Url::parse(source).ok()?;
            match url.scheme() {
                "ftp" => return Some("ftp"),
                "magnet" => return Some("torrent"),
                "rtsp" | "mms" | "mmsh" | "mmst" => return Some("media"),
                "http" | "https" => {}
                _ => return None,
            }
            Path::new(url.path())
                .extension()?
                .to_str()?
                .to_ascii_lowercase()
        };
        match extension.as_str() {
            "torrent" => Some("torrent"),
            "metalink" | "meta4" => Some("metalink"),
            "m3u8" => Some("media"),
            _ => None,
        }
    }

    #[test]
    fn precedence_matches_legacy_dispatch() {
        let dir = tempfile::tempdir().unwrap();
        let mut sources: Vec<String> = [
            "https://example.com/a.zip",
            "https://example.com/a.torrent",
            "https://example.com/A.TORRENT?x=1",
            "http://example.com/a.metalink",
            "https://example.com/a.meta4",
            "https://example.com/live.m3u8",
            "https://example.com/page",
            "https://example.com/",
            "ftp://example.com/a.torrent",
            "ftp://example.com/live.m3u8",
            "ftp://example.com/",
            "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567",
            "rtsp://example.com/a.torrent",
            "mms://example.com/x",
            "mmsh://example.com/x.meta4",
            "mmst://example.com/x",
            "gopher://example.com/a.torrent",
            "javascript:alert(1)",
            "not a url",
            "",
        ]
        .map(String::from)
        .into();
        for name in ["a.torrent", "b.meta4", "c.metalink", "d.m3u8", "e.txt"] {
            let path = dir.path().join(name);
            std::fs::write(&path, b"x").unwrap();
            sources.push(path.to_string_lossy().into_owned());
            sources.push(reqwest::Url::from_file_path(&path).unwrap().to_string());
        }
        for source in &sources {
            for kind in [DownloadKind::Auto, DownloadKind::Video] {
                assert_eq!(
                    find(source, kind).map(|m| m.id()),
                    legacy(source, kind),
                    "{source} {kind:?}"
                );
                assert_eq!(
                    crate::protocols::handles(source, kind),
                    legacy(source, kind).is_some()
                );
            }
        }
    }

    #[test]
    fn site_items_route_to_media() {
        let media = |source: &str, kind| find(source, kind).map(|m| m.id());
        for source in [
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ",
            "https://www.youtube.com/shorts/dQw4w9WgXcQ",
            "https://x.com/someone/status/1234567890123456789",
            "https://twitter.com/i/status/1234567890123456789/video/1",
        ] {
            assert_eq!(media(source, DownloadKind::Auto), Some("media"), "{source}");
            assert!(crate::protocols::handles(source, DownloadKind::Auto));
        }
        for source in [
            "https://www.youtube.com/@channel",
            "https://www.youtube.com/channel/UC1234567890",
            "https://www.youtube.com/playlist?list=PL123",
            "https://x.com/someone",
            "https://x.com/someone/media",
            "https://example.com/watch?v=dQw4w9WgXcQ",
        ] {
            assert_eq!(media(source, DownloadKind::Auto), None, "{source}");
        }
        for source in ["https://youtu.be/dQw4w9WgXcQ", "https://example.com/page"] {
            assert_eq!(media(source, DownloadKind::Audio), Some("media"));
            assert_eq!(media(source, DownloadKind::Video), Some("media"));
        }
    }

    #[test]
    fn ids_are_unique_and_valid() {
        let ids: std::collections::BTreeSet<_> = builtin().iter().map(|m| m.id()).collect();
        assert_eq!(ids.len(), builtin().len());
        assert!(ids.iter().all(|id| crate::modules::valid_id(id)));
    }

    #[test]
    fn publish_names() {
        let file = Path::new("stage/out.ts");
        let pick = |id: &str, suggested: Option<&str>| {
            let m = builtin().iter().find(|m| m.id() == id).unwrap();
            m.publish_name("out.ts".into(), suggested.map(String::from), file)
        };
        assert_eq!(pick("ftp", Some("s.bin")), "s.bin");
        assert_eq!(pick("ftp", None), "out.ts");
        assert_eq!(pick("torrent", Some("s.torrent")), "out.ts");
        assert_eq!(pick("metalink", Some("s.meta4")), "out.ts");
        assert_eq!(pick("media", Some("clip.html")), "clip.ts");
        assert_eq!(pick("media", None), "out.ts");
    }
}
