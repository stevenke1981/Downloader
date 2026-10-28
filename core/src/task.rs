use crate::category::Category;
use crate::checksum::Checksum;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DownloadKind {
    #[default]
    Auto,
    Video,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Status {
    #[default]
    Queued,
    Downloading,
    Paused,
    Verifying,
    Completed,
    Error,
}

impl Status {
    pub fn index(self) -> i32 {
        self as i32
    }
}

/// A byte range [start, end) of which [start, pos) is already on disk.
/// `end == u64::MAX` means "until the server closes" (unknown size).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seg {
    pub start: u64,
    pub pos: u64,
    pub end: u64,
    #[serde(skip)]
    pub busy: bool,
}

impl Seg {
    pub fn new(start: u64, end: u64) -> Seg {
        Seg {
            start,
            pos: start,
            end,
            busy: false,
        }
    }
    pub fn done(&self) -> bool {
        self.pos >= self.end
    }
    pub fn remaining(&self) -> u64 {
        self.end.saturating_sub(self.pos)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Task {
    pub id: u64,
    pub url: String,
    pub kind: DownloadKind,
    pub dir: PathBuf,
    /// Final file name (empty until the server has been probed).
    pub file_name: String,
    pub total: Option<u64>,
    pub etag: Option<String>,
    pub last_modified: Option<String>,
    pub resumable: bool,
    pub segments: Vec<Seg>,
    pub status: Status,
    /// Max connections for this download.
    pub connections: u32,
    /// Bytes/s, 0 = unlimited.
    pub speed_limit: u64,
    pub checksum: Option<Checksum>,
    /// Higher runs first.
    pub priority: i32,
    pub category: Category,
    pub added: i64,
    pub error: Option<String>,
    /// Referer header sent with every request (e.g. from the browser extension).
    pub referer: Option<String>,
    /// File name suggested by the browser; used instead of the server's name.
    pub suggested_name: Option<String>,
    /// URL after redirects (used to find checksum files next to it).
    pub final_url: Option<String>,
    /// Checksum list / sidecar URLs seen on the page (from the browser extension).
    pub checksum_urls: Vec<String>,
    /// Digests advertised in response headers.
    pub header_checksums: Vec<Checksum>,
    pub verification: Option<Verification>,
    pub verify_retried: bool,
    /// A login cookie was used for this download (it is never persisted, so this flag survives a restart).
    #[serde(default)]
    pub had_cookie: bool,
    /// `Cookie` header sent with every request. Never persisted.
    #[serde(skip)]
    pub cookie: Option<String>,
    /// Exact-origin HTTP Basic Authorization. Passwords are never persisted.
    #[serde(skip)]
    pub authorization: Option<String>,
    /// Bounded lifecycle messages. Request headers and credentials are excluded.
    pub log: Vec<String>,
    /// Bytes hashed so far while verifying (not persisted).
    #[serde(skip)]
    pub verify_done: std::sync::Arc<std::sync::atomic::AtomicU64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum VerifyState {
    #[default]
    Verifying,
    Verified,
    Failed,
    /// No checksum was found.
    NoChecksum,
    /// A checksum was found but couldn't be relied on (e.g. ETag not an MD5).
    Unverified,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
#[serde(default)]
pub struct Verification {
    pub state: VerifyState,
    pub algo: Option<crate::checksum::Algo>,
    pub expected: String,
    pub actual: String,
    pub source: String,
    /// URL of a detached OpenPGP signature (.sig / .asc), if one exists.
    pub signature: Option<String>,
    pub note: String,
}

impl Task {
    pub fn append_log(&mut self, message: impl Into<String>) {
        const MAX_LOG: usize = 100;
        if self.log.len() >= MAX_LOG {
            self.log.drain(..self.log.len() - MAX_LOG + 1);
        }
        self.log.push(message.into());
    }
    pub fn downloaded(&self) -> u64 {
        if self.status == Status::Completed {
            return self
                .total
                .unwrap_or_else(|| self.segments.iter().map(|s| s.pos - s.start).sum());
        }
        self.segments
            .iter()
            .map(|s| s.pos.saturating_sub(s.start))
            .sum()
    }
    pub fn display_name(&self) -> String {
        if self.file_name.is_empty() {
            crate::util::name_from_url(&self.url)
        } else {
            self.file_name.clone()
        }
    }
    pub fn final_path(&self) -> PathBuf {
        self.dir.join(&self.file_name)
    }
    pub fn part_path(&self) -> PathBuf {
        self.dir.join(format!("{}.part", self.file_name))
    }
}
