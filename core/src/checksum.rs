//! Hash algorithms, checksum-file parsing (GNU coreutils and BSD formats) and
//! HTTP digest headers.
use base64::Engine as _;
use blake2::Blake2b512;
use md5::Md5;
use serde::{Deserialize, Serialize};
use sha1::Sha1;
use sha2::{Digest, Sha256, Sha512};
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, PartialOrd, Ord)]
pub enum Algo {
    Md5,
    Sha1,
    Sha256,
    Sha512,
    Blake2b,
}

impl Algo {
    pub fn name(self) -> &'static str {
        match self {
            Algo::Md5 => "MD5",
            Algo::Sha1 => "SHA-1",
            Algo::Sha256 => "SHA-256",
            Algo::Sha512 => "SHA-512",
            Algo::Blake2b => "BLAKE2b-512",
        }
    }
    pub fn hex_len(self) -> usize {
        match self {
            Algo::Md5 => 32,
            Algo::Sha1 => 40,
            Algo::Sha256 => 64,
            Algo::Sha512 | Algo::Blake2b => 128,
        }
    }
    /// Strength rank for choosing among several available checksums.
    pub fn strength(self) -> u8 {
        match self {
            Algo::Md5 => 1,
            Algo::Sha1 => 2,
            Algo::Sha256 => 3,
            Algo::Blake2b => 4,
            Algo::Sha512 => 5,
        }
    }
    /// Parse names like "SHA256", "sha-256", "b2", "BLAKE2b", "md5".
    pub fn parse(s: &str) -> Option<Algo> {
        let n: String = s.to_ascii_lowercase().chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        Some(match n.as_str() {
            "md5" => Algo::Md5,
            "sha1" | "sha" => Algo::Sha1,
            "sha256" => Algo::Sha256,
            "sha512" => Algo::Sha512,
            "b2" | "blake2b" | "blake2b512" | "blake2" => Algo::Blake2b,
            _ => return None,
        })
    }
    /// Algorithm suggested by a checksum file name ("sha256sums.txt", "x.iso.b2", ...).
    pub fn from_file_name(name: &str) -> Option<Algo> {
        let l = name.to_ascii_lowercase();
        if l.contains("sha512") {
            Some(Algo::Sha512)
        } else if l.contains("sha256") {
            Some(Algo::Sha256)
        } else if l.contains("sha1") {
            Some(Algo::Sha1)
        } else if l.contains("md5") {
            Some(Algo::Md5)
        } else if l.contains("b2") || l.contains("blake2") {
            Some(Algo::Blake2b)
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checksum {
    pub algo: Algo,
    /// Lower-case hex.
    pub expected: String,
    /// Where it came from: "user", a URL, or "header: Digest" etc.
    #[serde(default)]
    pub source: String,
}

fn is_hex(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit())
}

impl Checksum {
    /// Parse a pasted hex digest (optionally "algo:hex"), guessing the algorithm
    /// from its length: 32 MD5, 40 SHA-1, 64 SHA-256, 128 SHA-512.
    pub fn guess(s: &str) -> Option<Checksum> {
        let s = s.trim();
        let (hint, h) = match s.split_once([':', '=']) {
            Some((a, h)) if Algo::parse(a).is_some() => (Algo::parse(a), h.trim()),
            _ => (None, s),
        };
        let h = h.to_ascii_lowercase();
        if !is_hex(&h) {
            return None;
        }
        let algo = match (hint, h.len()) {
            (Some(a), n) if a.hex_len() == n => a,
            (Some(_), _) => return None,
            (None, 32) => Algo::Md5,
            (None, 40) => Algo::Sha1,
            (None, 64) => Algo::Sha256,
            (None, 128) => Algo::Sha512,
            _ => return None,
        };
        Some(Checksum { algo, expected: h, source: "user".into() })
    }
}

/// Hash a file; `progress` receives bytes hashed so far.
pub fn hash_file(path: &Path, algo: Algo, progress: Option<&AtomicU64>) -> std::io::Result<String> {
    let mut f = std::fs::File::open(path)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut done = 0u64;
    macro_rules! run {
        ($h:expr) => {{
            let mut h = $h;
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                h.update(&buf[..n]);
                done += n as u64;
                if let Some(p) = progress {
                    p.store(done, Ordering::Relaxed);
                }
            }
            hex::encode(h.finalize())
        }};
    }
    Ok(match algo {
        Algo::Md5 => run!(Md5::new()),
        Algo::Sha1 => run!(Sha1::new()),
        Algo::Sha256 => run!(Sha256::new()),
        Algo::Sha512 => run!(Sha512::new()),
        Algo::Blake2b => run!(Blake2b512::new()),
    })
}

pub fn verify(path: &Path, c: &Checksum) -> Result<(), String> {
    let got = hash_file(path, c.algo, None).map_err(|e| e.to_string())?;
    if got.eq_ignore_ascii_case(c.expected.trim()) {
        Ok(())
    } else {
        Err(format!("checksum mismatch: expected {}, got {}", c.expected, got))
    }
}

fn base_name(s: &str) -> &str {
    let s = s.trim();
    let s = s.strip_prefix('*').unwrap_or(s);
    s.rsplit(['/', '\\']).next().unwrap_or(s)
}

/// Find `file_name` in a checksum list. Supports
/// GNU: `<hex>  name`, `<hex> *name` (binary flag), and BSD: `SHA256 (name) = <hex>`.
/// `hint` is the algorithm implied by the list's file name (e.g. b2sums.txt).
/// A single bare hash (sidecar file like `x.iso.sha256` with just the digest) is accepted
/// when `single_file` is true.
pub fn find_in_list(text: &str, file_name: &str, hint: Option<Algo>, single_file: bool) -> Option<Checksum> {
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#'));
    let all: Vec<&str> = lines.by_ref().collect();
    for l in &all {
        // BSD style
        if let Some((left, hexv)) = l.rsplit_once(" = ").or_else(|| l.rsplit_once(")="))
            && let (Some(open), true) = (left.find(" ("), left.ends_with(')') || l.contains(")="))
        {
            let algo = Algo::parse(&left[..open]);
            let name = left[open + 2..].trim_end_matches(')');
            let h = hexv.trim().to_ascii_lowercase();
            if let Some(a) = algo
                && base_name(name) == file_name
                && is_hex(&h)
                && h.len() == a.hex_len()
            {
                return Some(Checksum { algo: a, expected: h, source: String::new() });
            }
            continue;
        }
        // GNU style
        let mut it = l.splitn(2, char::is_whitespace);
        let (Some(h), Some(name)) = (it.next(), it.next()) else { continue };
        let h = h.to_ascii_lowercase();
        if !is_hex(&h) || base_name(name) != file_name {
            continue;
        }
        if let Some(a) = by_len(&h, hint) {
            return Some(Checksum { algo: a, expected: h, source: String::new() });
        }
    }
    if single_file && all.len() == 1 {
        let h = all[0].split_whitespace().next().unwrap_or("").to_ascii_lowercase();
        if is_hex(&h)
            && let Some(a) = by_len(&h, hint)
        {
            return Some(Checksum { algo: a, expected: h, source: String::new() });
        }
    }
    None
}

fn by_len(h: &str, hint: Option<Algo>) -> Option<Algo> {
    match hint {
        Some(a) if a.hex_len() == h.len() => Some(a),
        Some(_) => None,
        None => match h.len() {
            32 => Some(Algo::Md5),
            40 => Some(Algo::Sha1),
            64 => Some(Algo::Sha256),
            128 => Some(Algo::Sha512),
            _ => None,
        },
    }
}

fn b64_to_hex(v: &str) -> Option<String> {
    let v = v.trim().trim_matches(':');
    base64::engine::general_purpose::STANDARD.decode(v).ok().map(hex::encode)
}

/// Checksums from response headers. `full_body` = the response carried the whole
/// file (Content-MD5 describes the body, so it's only usable then).
pub fn from_headers(h: &reqwest::header::HeaderMap, full_body: bool) -> Vec<Checksum> {
    let mut out = vec![];
    let get = |n: &str| h.get_all(n).iter().filter_map(|v| v.to_str().ok()).collect::<Vec<_>>().join(",");
    for name in ["repr-digest", "digest"] {
        for part in get(name).split(',') {
            let Some((a, v)) = part.split_once('=') else { continue };
            let Some(algo) = Algo::parse(a) else { continue };
            if let Some(hex) = b64_to_hex(v).filter(|x| x.len() == algo.hex_len()) {
                out.push(Checksum { algo, expected: hex, source: format!("header: {name}") });
            }
        }
    }
    for part in get("x-goog-hash").split(',') {
        if let Some(("md5", v)) = part.trim().split_once('=')
            && let Some(hex) = b64_to_hex(v).filter(|x| x.len() == 32)
        {
            out.push(Checksum { algo: Algo::Md5, expected: hex, source: "header: x-goog-hash".into() });
        }
    }
    if full_body
        && let Some(hex) = b64_to_hex(&get("content-md5")).filter(|x| x.len() == 32)
    {
        out.push(Checksum { algo: Algo::Md5, expected: hex, source: "header: Content-MD5".into() });
    }
    out
}

/// An ETag that is exactly a 32-hex MD5 (single-part S3 style). Advisory only:
/// many servers use other ETag schemes, so a mismatch is not treated as failure.
pub fn etag_md5(etag: &str) -> Option<Checksum> {
    let e = etag.trim().trim_start_matches("W/").trim_matches('"').to_ascii_lowercase();
    (e.len() == 32 && is_hex(&e)).then(|| Checksum { algo: Algo::Md5, expected: e, source: "ETag".into() })
}

#[cfg(test)]
mod tests {
    use super::*;
    const H: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    #[test]
    fn gnu_and_bsd() {
        let gnu = format!("{H}  other.iso\n{H} *archlinux-x86_64.iso\n");
        let c = find_in_list(&gnu, "archlinux-x86_64.iso", Some(Algo::Sha256), false).unwrap();
        assert_eq!(c.algo, Algo::Sha256);
        let bsd = format!("SHA256 (archlinux-x86_64.iso) = {H}\n");
        assert_eq!(find_in_list(&bsd, "archlinux-x86_64.iso", None, false).unwrap().expected, H);
        assert!(find_in_list(&bsd, "nope.iso", None, false).is_none());
        assert!(find_in_list(H, "x", None, true).is_some());
        assert!(find_in_list(H, "x", None, false).is_none());
    }
    #[test]
    fn guess_lengths() {
        assert_eq!(Checksum::guess(&"a".repeat(40)).unwrap().algo, Algo::Sha1);
        assert_eq!(Checksum::guess(&"a".repeat(128)).unwrap().algo, Algo::Sha512);
        assert_eq!(Checksum::guess(&format!("b2:{}", "a".repeat(128))).unwrap().algo, Algo::Blake2b);
        assert!(Checksum::guess("xyz").is_none());
    }
    #[test]
    fn headers() {
        let mut h = reqwest::header::HeaderMap::new();
        h.insert("digest", "SHA-256=X48E9qOokqqrvdts8nOJRJN3OWDUoyWxBf7kbu9DBPE=".parse().unwrap());
        let v = from_headers(&h, false);
        assert_eq!(v[0].expected, "5f8f04f6a3a892aaabbddb6cf273894493773960d4a325b105fee46eef4304f1");
        assert!(etag_md5("\"d41d8cd98f00b204e9800998ecf8427e\"").is_some());
        assert!(etag_md5("\"5f3c-61a2b\"").is_none());
    }
}
