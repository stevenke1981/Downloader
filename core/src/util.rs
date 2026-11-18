use std::path::{Path, PathBuf};

pub fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

pub fn human_eta(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 99 {
        "∞".into()
    } else if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// Parse "500K", "2M", "1.5MB", "1048576" into bytes (binary units).
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_uppercase();
    let s = s
        .trim_end_matches("/S")
        .trim_end_matches('B')
        .trim_end_matches('I');
    if s.is_empty() {
        return Some(0);
    }
    let (num, mul) = match s.chars().last()? {
        'K' => (&s[..s.len() - 1], 1024.0),
        'M' => (&s[..s.len() - 1], 1024.0 * 1024.0),
        'G' => (&s[..s.len() - 1], 1024.0 * 1024.0 * 1024.0),
        _ => (s, 1.0),
    };
    let v: f64 = num.trim().parse().ok()?;
    (v.is_finite() && v >= 0.0).then_some((v * mul) as u64)
}

/// File name from a URL path (percent-decoded), or "download".
pub fn name_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let path = path.split_once("://").map(|(_, r)| r).unwrap_or(path);
    let last = path
        .split_once('/')
        .map(|(_, p)| p)
        .unwrap_or("")
        .rsplit('/')
        .next()
        .unwrap_or("");
    let dec = percent_encoding::percent_decode_str(last)
        .decode_utf8_lossy()
        .to_string();
    let clean = sanitize(&dec);
    if clean.is_empty() {
        "download".into()
    } else {
        clean
    }
}

/// Parse `filename*=UTF-8''...` or `filename="..."` from Content-Disposition.
pub fn name_from_disposition(v: &str) -> Option<String> {
    for part in v.split(';') {
        let p = part.trim();
        if let Some(rest) = p.strip_prefix("filename*=") {
            let enc = rest.splitn(3, '\'').nth(2).unwrap_or(rest);
            let n = percent_encoding::percent_decode_str(enc.trim_matches('"'))
                .decode_utf8_lossy()
                .to_string();
            let n = sanitize(&n);
            if !n.is_empty() {
                return Some(n);
            }
        }
    }
    for part in v.split(';') {
        if let Some(rest) = part.trim().strip_prefix("filename=") {
            let n = sanitize(rest.trim_matches('"'));
            if !n.is_empty() {
                return Some(n);
            }
        }
    }
    None
}

/// Windows device names; they are unusable as file stems in any case and with any extension.
const WIN_RESERVED: [&str; 22] = [
    "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
    "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
];
const MAX_NAME_BYTES: usize = 200;

pub fn sanitize(name: &str) -> String {
    let n: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect();
    let mut n = n.trim().trim_matches('.').to_string();
    let stem = n
        .split('.')
        .next()
        .unwrap_or("")
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    if WIN_RESERVED.contains(&stem.as_str()) {
        n.insert(0, '_');
    }
    truncate_name(&n, MAX_NAME_BYTES)
}

/// Shorten `n` to at most `max` UTF-8 bytes: cut the stem on a char boundary and keep
/// the extension when it is at most 16 bytes long.
fn truncate_name(n: &str, max: usize) -> String {
    if n.len() <= max {
        return n.to_string();
    }
    let (stem, ext) = match n.rfind('.') {
        Some(i) if i > 0 && n.len() - i - 1 <= 16 => (&n[..i], &n[i..]),
        _ => (n, ""),
    };
    let mut end = (max - ext.len()).min(stem.len());
    while !stem.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{ext}", stem[..end].trim_end_matches([' ', '.']))
}

/// `dir/name`, or `dir/name (1).ext` ... if that (or its .part) already exists.
pub fn unique_name(dir: &Path, name: &str) -> String {
    let taken = |n: &str| dir.join(n).exists() || dir.join(format!("{n}.part")).exists();
    if !taken(name) {
        return name.to_string();
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (1..)
        .map(|i| format!("{stem} ({i}){ext}"))
        .find(|n| !taken(n))
        .unwrap()
}

/// Name to save `name` under in `dir` according to `p`.
pub fn resolve_conflict(
    dir: &Path,
    name: &str,
    p: crate::task::ConflictPolicy,
) -> Result<String, String> {
    use crate::task::ConflictPolicy;
    match p {
        ConflictPolicy::Rename => Ok(unique_name(dir, name)),
        ConflictPolicy::Overwrite => Ok(name.to_string()),
        ConflictPolicy::Skip => {
            let path = dir.join(name);
            if path.exists() {
                Err(format!("file exists: {}", path.display()))
            } else {
                Ok(name.to_string())
            }
        }
    }
}

/// Whether `s` contains `num` not adjacent to other digits.
fn has_number(s: &str, num: &str) -> bool {
    s.match_indices(num).any(|(i, _)| {
        let before = s[..i].bytes().last();
        let after = s[i + num.len()..].bytes().next();
        !before.is_some_and(|b| b.is_ascii_digit()) && !after.is_some_and(|b| b.is_ascii_digit())
    })
}

/// "http 404", "status 503", "status code 500", "server returned 403".
fn has_http_status(s: &str) -> bool {
    ["http ", "status ", "status code ", "server returned "]
        .iter()
        .any(|p| {
            s.match_indices(p).any(|(i, _)| {
                let rest = &s.as_bytes()[i + p.len()..];
                rest.len() >= 3
                    && rest[..3].iter().all(u8::is_ascii_digit)
                    && (b'1'..=b'5').contains(&rest[0])
                    && rest.get(3).is_none_or(|b| !b.is_ascii_digit())
            })
        })
}

/// Stable machine-readable class of an error message:
/// http | network | verify | conflict | duplicate | disk | module | auth | invalid | error.
pub fn error_code(msg: &str) -> &'static str {
    let m = msg.trim().to_ascii_lowercase();
    if m.starts_with("duplicate:") {
        "duplicate"
    } else if m.starts_with("file exists:") {
        "conflict"
    } else if m.starts_with("not enough disk space:") {
        "disk"
    } else if m.starts_with("module ") {
        "module"
    } else if m.contains("verification failed") || m.contains("checksum mismatch") {
        "verify"
    } else if m.contains("token") || m.contains("unauthorized") || has_number(&m, "401") {
        "auth"
    } else if has_http_status(&m) {
        "http"
    } else if [
        "connect",
        "timed out",
        "timeout",
        "dns",
        "resolve",
        "unreachable",
        "connection",
        "network",
    ]
    .iter()
    .any(|k| m.contains(k))
    {
        "network"
    } else if m.contains("invalid") || m.contains("unsupported") {
        "invalid"
    } else {
        "error"
    }
}

pub fn default_download_dir() -> PathBuf {
    dirs::download_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn looks_like_url(s: &str) -> bool {
    validate_source(s).is_ok()
}

/// Validate the public source boundary without probing the network or executing tools.
pub fn validate_source(source: &str) -> Result<(), String> {
    let source = source.trim();
    if source.is_empty() {
        return Err("download source is empty".into());
    }
    let local = descriptor_path(source);
    if let Some(path) = local {
        return if path.is_file() {
            Ok(())
        } else {
            Err("local torrent/Metalink file does not exist".into())
        };
    }
    let url = reqwest::Url::parse(source)
        .map_err(|_| "enter a supported URL or an existing .torrent/.metalink/.meta4 file")?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(
            "credentials in URLs are not saved; use a site profile or the FTP browser login".into(),
        );
    }
    if source.contains(char::is_whitespace) {
        return Err("URL contains whitespace; encode spaces as %20".into());
    }
    match url.scheme() {
        "http" | "https" | "ftp" | "rtsp" | "mms" | "mmsh" | "mmst" if url.host_str().is_some() => {
            Ok(())
        }
        "magnet"
            if url.query_pairs().any(|(key, value)| {
                if key != "xt" {
                    return false;
                }
                if let Some(hash) = value.strip_prefix("urn:btih:") {
                    return (hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                        || (hash.len() == 32
                            && hash
                                .bytes()
                                .all(|b| b.is_ascii_alphabetic() || matches!(b, b'2'..=b'7')));
                }
                value.strip_prefix("urn:btmh:1220").is_some_and(|hash| {
                    hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit())
                })
            }) =>
        {
            Ok(())
        }
        _ => Err(
            "unsupported download source (HTTP, FTP, torrent, Metalink, RTSP or MMS required)"
                .into(),
        ),
    }
}

/// Local descriptors only; arbitrary local files never become download sources.
pub fn descriptor_path(source: &str) -> Option<PathBuf> {
    let path = if source.starts_with("file:") {
        reqwest::Url::parse(source).ok()?.to_file_path().ok()?
    } else {
        if source.contains("://") || source.starts_with("magnet:") {
            return None;
        }
        PathBuf::from(source)
    };
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    matches!(ext.as_str(), "torrent" | "metalink" | "meta4").then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names() {
        assert_eq!(name_from_url("https://x.org/a/b%20c.zip?x=1"), "b c.zip");
        assert_eq!(name_from_url("https://x.org/"), "download");
        assert_eq!(
            name_from_disposition("attachment; filename=\"r.pdf\"").unwrap(),
            "r.pdf"
        );
        assert_eq!(
            name_from_disposition("attachment; filename*=UTF-8''%E4%B8%AD.txt").unwrap(),
            "中.txt"
        );
    }
    #[test]
    fn sizes() {
        assert_eq!(parse_size("2M"), Some(2 << 20));
        assert_eq!(parse_size("500k"), Some(500 * 1024));
        assert_eq!(parse_size("100"), Some(100));
        assert_eq!(parse_size("1.5MB"), Some(1572864));
        assert_eq!(parse_size(""), Some(0));
        assert_eq!(parse_size("-1"), None);
        assert_eq!(parse_size("inf"), None);
        assert_eq!(parse_size("NaN"), None);
        assert_eq!(parse_size("infK"), None);
        assert_eq!(human_eta(3725), "1:02:05");
    }
    #[test]
    fn sanitize_windows_reserved_names() {
        assert_eq!(sanitize("con"), "_con");
        assert_eq!(sanitize("CON.txt"), "_CON.txt");
        assert_eq!(sanitize("nul.tar.gz"), "_nul.tar.gz");
        assert_eq!(sanitize("NUL.tar.gz"), "_NUL.tar.gz");
        assert_eq!(sanitize("com1.txt"), "_com1.txt");
        assert_eq!(sanitize("LPT9 .log"), "_LPT9 .log");
        assert_eq!(sanitize("COM10.txt"), "COM10.txt");
        assert_eq!(sanitize("console.txt"), "console.txt");
        assert_eq!(sanitize("a/b:c"), "a_b_c");
        assert_eq!(name_from_url("https://x.org/aux"), "_aux");
    }
    #[test]
    fn sanitize_limits_length() {
        let s = sanitize(&format!("{}.zip", "a".repeat(300)));
        assert_eq!(s.len(), 200);
        assert!(s.ends_with(".zip"));
        let s = sanitize(&format!("{}.txt", "中".repeat(100)));
        assert!(s.len() <= 200 && s.len() > 190 && s.ends_with(".txt") && s.starts_with('中'));
        // Extension longer than 16 bytes is not kept specially.
        assert_eq!(
            sanitize(&format!("{}.{}", "b".repeat(250), "x".repeat(20))),
            "b".repeat(200)
        );
        assert_eq!(sanitize("short.tar.gz"), "short.tar.gz");
    }
    #[test]
    fn conflict_policies() {
        use crate::task::ConflictPolicy;
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        for p in [
            ConflictPolicy::Rename,
            ConflictPolicy::Overwrite,
            ConflictPolicy::Skip,
        ] {
            assert_eq!(resolve_conflict(d, "a.zip", p).unwrap(), "a.zip");
        }
        std::fs::write(d.join("a.zip"), b"x").unwrap();
        assert_eq!(
            resolve_conflict(d, "a.zip", ConflictPolicy::Rename).unwrap(),
            "a (1).zip"
        );
        assert_eq!(
            resolve_conflict(d, "a.zip", ConflictPolicy::Overwrite).unwrap(),
            "a.zip"
        );
        let e = resolve_conflict(d, "a.zip", ConflictPolicy::Skip).unwrap_err();
        assert!(
            e.starts_with("file exists: ") && e.ends_with("a.zip"),
            "{e}"
        );
        assert_eq!(error_code(&e), "conflict");
    }
    #[test]
    fn error_codes() {
        for (msg, code) in [
            ("duplicate: already queued as #4", "duplicate"),
            ("file exists: C:\\x\\a.zip", "conflict"),
            ("not enough disk space: need 2 GB, 1 GB free", "disk"),
            ("module example-resolver: timed out", "module"),
            ("verification failed: SHA-256 mismatch", "verify"),
            ("checksum mismatch", "verify"),
            ("HTTP 404 Not Found", "http"),
            ("server returned status 503", "http"),
            ("HTTP 401 Unauthorized", "auth"),
            ("missing or wrong token", "auth"),
            ("unauthorized", "auth"),
            ("error sending request: connection refused", "network"),
            ("operation timed out", "network"),
            ("dns error: failed to lookup address", "network"),
            ("invalid header name", "invalid"),
            ("unsupported download source", "invalid"),
            ("something odd", "error"),
            ("downloaded 4010 bytes", "error"),
        ] {
            assert_eq!(error_code(msg), code, "{msg}");
        }
    }
    #[test]
    fn url_scheme_gate() {
        assert!(looks_like_url("https://example.com/a.zip"));
        assert!(looks_like_url("  http://example.com/a.zip  "));
        assert!(looks_like_url("ftp://example.com/a.zip"));
        assert!(looks_like_url("rtsp://example.com/live"));
        assert!(looks_like_url(
            "magnet:?xt=urn:btih:0123456789012345678901234567890123456789"
        ));
        assert!(!looks_like_url("https://user:secret@example.com/file"));
        assert!(!looks_like_url("magnet:?dn=missing-hash"));
        assert!(!looks_like_url("magnet:?xt=urn:btih:"));
        assert!(!looks_like_url("magnet:?xt=urn:btih:short"));
        assert!(!looks_like_url("javascript:alert(1)"));
        assert!(!looks_like_url("https://exa mple.com/a"));
        assert!(!looks_like_url("https://"));
    }
}
