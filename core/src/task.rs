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

/// What to do when the target file already exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ConflictPolicy {
    /// Save as `name (1).ext`.
    #[default]
    Rename,
    /// Replace the existing file once the download completes.
    Overwrite,
    /// Fail with "file exists: <path>".
    Skip,
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
    /// Extra request headers (validated with `validate_headers`).
    pub headers: Vec<(String, String)>,
    /// User-Agent override.
    pub user_agent: Option<String>,
    /// Alternative URLs for the same file.
    pub mirrors: Vec<String>,
    /// What to do when the target file already exists.
    pub conflict: ConflictPolicy,
    /// URL a resolver module produced. Never persisted.
    #[serde(skip)]
    pub resolved_url: Option<String>,
    /// Headers a resolver module asked for. Never persisted, and meant only for requests to
    /// the origin of `resolved_url` (they override `headers` of the same name there).
    #[serde(skip)]
    pub module_headers: Vec<(String, String)>,
    /// Id of the module that resolved this download.
    pub module: Option<String>,
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

const MAX_HEADERS: usize = 16;
const MAX_HEADER_BYTES: usize = 4096;
const FORBIDDEN_HEADERS: [&str; 6] = [
    "cookie",
    "authorization",
    "host",
    "range",
    "if-range",
    "content-length",
];

/// Check user-supplied request headers: at most 16, 4 KiB in total, valid names, no CR/LF,
/// and none the downloader manages itself (Cookie, Authorization, Host, Range, If-Range,
/// Content-Length).
pub fn validate_headers(h: &[(String, String)]) -> Result<(), String> {
    if h.len() > MAX_HEADERS {
        return Err(format!(
            "invalid headers: at most {MAX_HEADERS} are allowed"
        ));
    }
    let total: usize = h.iter().map(|(k, v)| k.len() + v.len()).sum();
    if total > MAX_HEADER_BYTES {
        return Err("invalid headers: more than 4 KiB in total".into());
    }
    for (name, value) in h {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        {
            return Err(format!("invalid header name {name:?}"));
        }
        if value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
            return Err(format!(
                "invalid header value for {name}: line breaks are not allowed"
            ));
        }
        if FORBIDDEN_HEADERS.contains(&name.to_ascii_lowercase().as_str()) {
            return Err(format!(
                "invalid header {name}: set by the downloader itself"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(k: &str, v: &str) -> (String, String) {
        (k.into(), v.into())
    }

    #[test]
    fn headers_accepted() {
        assert!(validate_headers(&[]).is_ok());
        assert!(validate_headers(&[h("Accept", "*/*"), h("X-Api-Key", "abc")]).is_ok());
        assert!(validate_headers(&vec![h("X-A", "1"); 16]).is_ok());
    }

    #[test]
    fn headers_rejected() {
        for name in [
            "Cookie",
            "authorization",
            "HOST",
            "Range",
            "If-Range",
            "Content-Length",
        ] {
            assert!(validate_headers(&[h(name, "x")]).is_err(), "{name}");
        }
        assert!(validate_headers(&[h("X-A", "a\r\nX-B: b")]).is_err());
        assert!(validate_headers(&[h("X-A", "a\nb")]).is_err());
        assert!(validate_headers(&[h("X-A\r\n", "a")]).is_err());
        assert!(validate_headers(&[h("Bad Name", "a")]).is_err());
        assert!(validate_headers(&[h("", "a")]).is_err());
        assert!(validate_headers(&vec![h("X-A", "1"); 17]).is_err());
        assert!(validate_headers(&[h("X-A", &"a".repeat(4096))]).is_err());
        assert!(validate_headers(&[h("X-A", &"a".repeat(4093))]).is_ok());
    }

    #[test]
    fn old_task_json_loads() {
        let t: Task =
            serde_json::from_str(r#"{"id":3,"url":"https://x.org/a.zip","status":"Paused"}"#)
                .unwrap();
        assert_eq!(t.conflict, ConflictPolicy::Rename);
        assert!(t.headers.is_empty() && t.user_agent.is_none() && t.mirrors.is_empty());
        assert!(t.module.is_none() && t.resolved_url.is_none());
        let p: ConflictPolicy = serde_json::from_str("\"overwrite\"").unwrap();
        assert_eq!(p, ConflictPolicy::Overwrite);
        assert_eq!(
            serde_json::to_string(&ConflictPolicy::Skip).unwrap(),
            "\"skip\""
        );
    }
}
