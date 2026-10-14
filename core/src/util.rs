use std::path::{Path, PathBuf};

pub fn human_size(n: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 { format!("{n} B") } else { format!("{v:.1} {}", U[i]) }
}

pub fn human_eta(secs: u64) -> String {
    let (h, m, s) = (secs / 3600, secs / 60 % 60, secs % 60);
    if h > 99 { "∞".into() } else if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m}:{s:02}") }
}

/// Parse "500K", "2M", "1.5MB", "1048576" into bytes (binary units).
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim().to_ascii_uppercase();
    let s = s.trim_end_matches("/S").trim_end_matches('B').trim_end_matches('I');
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
    (v >= 0.0).then(|| (v * mul) as u64)
}

/// File name from a URL path (percent-decoded), or "download".
pub fn name_from_url(url: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or("");
    let path = path.split_once("://").map(|(_, r)| r).unwrap_or(path);
    let last = path.split_once('/').map(|(_, p)| p).unwrap_or("").rsplit('/').next().unwrap_or("");
    let dec = percent_encoding::percent_decode_str(last).decode_utf8_lossy().to_string();
    let clean = sanitize(&dec);
    if clean.is_empty() { "download".into() } else { clean }
}

/// Parse `filename*=UTF-8''...` or `filename="..."` from Content-Disposition.
pub fn name_from_disposition(v: &str) -> Option<String> {
    for part in v.split(';') {
        let p = part.trim();
        if let Some(rest) = p.strip_prefix("filename*=") {
            let enc = rest.splitn(3, '\'').nth(2).unwrap_or(rest);
            let n = percent_encoding::percent_decode_str(enc.trim_matches('"')).decode_utf8_lossy().to_string();
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

pub fn sanitize(name: &str) -> String {
    let n: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
        .collect();
    n.trim().trim_matches('.').to_string()
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
    (1..).map(|i| format!("{stem} ({i}){ext}")).find(|n| !taken(n)).unwrap()
}

pub fn default_download_dir() -> PathBuf {
    dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(|| PathBuf::from("."))
}

pub fn looks_like_url(s: &str) -> bool {
    let s = s.trim();
    (s.starts_with("http://") || s.starts_with("https://") || s.starts_with("ftp://"))
        && !s.contains(char::is_whitespace)
        && s.len() > 10
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names() {
        assert_eq!(name_from_url("https://x.org/a/b%20c.zip?x=1"), "b c.zip");
        assert_eq!(name_from_url("https://x.org/"), "download");
        assert_eq!(name_from_disposition("attachment; filename=\"r.pdf\"").unwrap(), "r.pdf");
        assert_eq!(name_from_disposition("attachment; filename*=UTF-8''%E4%B8%AD.txt").unwrap(), "中.txt");
    }
    #[test]
    fn sizes() {
        assert_eq!(parse_size("2M"), Some(2 << 20));
        assert_eq!(parse_size("500k"), Some(500 * 1024));
        assert_eq!(parse_size("100"), Some(100));
        assert_eq!(parse_size(""), Some(0));
        assert_eq!(human_eta(3725), "1:02:05");
    }
}
