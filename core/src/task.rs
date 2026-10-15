use crate::category::Category;
use crate::checksum::Checksum;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
        Seg { start, pos: start, end, busy: false }
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
}

impl Task {
    pub fn downloaded(&self) -> u64 {
        if self.status == Status::Completed {
            return self.total.unwrap_or_else(|| self.segments.iter().map(|s| s.pos - s.start).sum());
        }
        self.segments.iter().map(|s| s.pos.saturating_sub(s.start)).sum()
    }
    pub fn display_name(&self) -> String {
        if self.file_name.is_empty() { crate::util::name_from_url(&self.url) } else { self.file_name.clone() }
    }
    pub fn final_path(&self) -> PathBuf {
        self.dir.join(&self.file_name)
    }
    pub fn part_path(&self) -> PathBuf {
        self.dir.join(format!("{}.part", self.file_name))
    }
}
