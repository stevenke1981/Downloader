use super::*;

/// A point-in-time view of one download for the UI.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Snapshot {
    pub id: u64,
    pub url: String,
    pub name: String,
    pub path: PathBuf,
    pub total: Option<u64>,
    pub downloaded: u64,
    pub speed: f64,
    pub eta: Option<u64>,
    pub status: Status,
    pub category: Category,
    pub error: Option<String>,
    /// (start, pos, end) of each segment.
    pub segments: Vec<(u64, u64, u64)>,
    pub connections: u32,
    pub active_connections: u32,
    pub speed_limit: u64,
    pub priority: i32,
    pub resumable: bool,
    pub verification: Option<crate::task::Verification>,
    /// 0..1 while hashing.
    pub verify_progress: f32,
    pub kind: DownloadKind,
    pub log: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct NewDownload {
    pub url: String,
    pub kind: DownloadKind,
    /// None = folder from settings (per category).
    pub dir: Option<PathBuf>,
    /// None = from settings.
    pub connections: Option<u32>,
    pub speed_limit: Option<u64>,
    pub checksum: Option<Checksum>,
    pub priority: i32,
    /// false = add paused.
    pub start: bool,
    pub referer: Option<String>,
    /// File name to use instead of the server's.
    pub file_name: Option<String>,
    /// Checksum list / sidecar URLs seen on the page.
    pub checksum_urls: Vec<String>,
    /// `Cookie` header sent with every request (kept in memory only).
    pub cookie: Option<String>,
}
