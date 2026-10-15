//! Post-download integrity verification: find a checksum (user, sidecar file,
//! checksum list next to the file, response headers, MD5 ETag), hash the file
//! in a blocking thread with progress, and record the result on the task.
use crate::checksum::{self, Algo, Checksum};
use crate::engine::Ctx;
use crate::task::{Status, Task, Verification, VerifyState};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const SIDE_EXT: [&str; 9] = ["sha512", "sha256", "b2", "sha1", "md5", "sha512sum", "sha256sum", "sha1sum", "md5sum"];
const LISTS: [&str; 18] = [
    "SHA512SUMS", "sha512sums.txt", "sha512sums", "b2sums.txt", "B2SUMS", "SHA256SUMS", "sha256sums.txt", "sha256sums",
    "sha256sum.txt", "SHA1SUMS", "sha1sums.txt", "MD5SUMS", "md5sums.txt", "md5sums", "CHECKSUMS", "checksums.txt",
    "CHECKSUM", "checksums.sha256",
];

/// Directory URL (ending in '/') and the remote file name of `url`.
pub fn split_url(url: &str) -> Option<(String, String)> {
    let u = url.split(['?', '#']).next()?;
    let (dir, name) = u.rsplit_once('/')?;
    if name.is_empty() || !dir.contains("://") || dir.ends_with(':') || dir.ends_with("/") && dir.matches('/').count() < 3 {
        return None;
    }
    let name = percent_encoding::percent_decode_str(name).decode_utf8_lossy().to_string();
    Some((format!("{dir}/"), name))
}

fn encode(name: &str) -> String {
    percent_encoding::utf8_percent_encode(name, percent_encoding::NON_ALPHANUMERIC)
        .to_string()
        .replace("%2E", ".")
        .replace("%2D", "-")
        .replace("%5F", "_")
}

/// Fetch a small text file; None unless 200 and plain text.
async fn fetch_text(client: &reqwest::Client, url: &str) -> Option<String> {
    let r = tokio::time::timeout(Duration::from_secs(20), client.get(url).send()).await.ok()?.ok()?;
    if r.status().as_u16() != 200 || r.content_length().is_some_and(|n| n > 4 << 20) {
        return None;
    }
    let ct = r.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    if ct.contains("html") {
        return None;
    }
    let b = tokio::time::timeout(Duration::from_secs(30), r.bytes()).await.ok()?.ok()?;
    if b.len() > 4 << 20 {
        return None;
    }
    let s = String::from_utf8_lossy(&b).to_string();
    (!s.trim_start().starts_with('<')).then_some(s)
}

async fn exists(client: &reqwest::Client, url: &str) -> bool {
    let r = tokio::time::timeout(Duration::from_secs(15), client.get(url).header(reqwest::header::RANGE, "bytes=0-0").send()).await;
    matches!(r, Ok(Ok(r)) if r.status().is_success()
        && !r.headers().get(reqwest::header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or("").contains("html"))
}

/// Look for a checksum for `file_url`: first in `extra` (URLs the browser saw on the
/// page), then sidecar files, then common checksum lists in the same directory.
pub async fn discover(client: &reqwest::Client, file_url: &str, extra: &[String]) -> Option<Checksum> {
    let (dir, name) = split_url(file_url)?;
    let mut cands: Vec<String> = extra.iter().filter(|u| u.starts_with("http")).cloned().collect();
    for e in SIDE_EXT {
        cands.push(format!("{dir}{}.{e}", encode(&name)));
    }
    for l in LISTS {
        cands.push(format!("{dir}{l}"));
    }
    let mut seen = std::collections::HashSet::new();
    let mut best: Option<Checksum> = None;
    for url in cands {
        if !seen.insert(url.clone()) {
            continue;
        }
        let Some(text) = fetch_text(client, &url).await else { continue };
        let list_name = url.rsplit('/').next().unwrap_or("");
        let single = list_name.starts_with(&encode(&name)) || list_name.starts_with(name.as_str());
        if let Some(mut c) = checksum::find_in_list(&text, &name, Algo::from_file_name(list_name), single) {
            c.source = url.clone();
            if best.as_ref().is_none_or(|b| c.algo.strength() > b.algo.strength()) {
                best = Some(c);
            }
            // Strong enough; stop probing.
            if best.as_ref().is_some_and(|b| b.algo.strength() >= Algo::Sha256.strength()) {
                break;
            }
        }
    }
    best
}

/// A detached OpenPGP signature next to the file, if any.
pub async fn find_signature(client: &reqwest::Client, file_url: &str) -> Option<String> {
    let (dir, name) = split_url(file_url)?;
    for ext in ["sig", "asc"] {
        let u = format!("{dir}{}.{ext}", encode(&name));
        if exists(client, &u).await {
            return Some(u);
        }
    }
    None
}

fn set(task: &Arc<Mutex<Task>>, v: Verification) {
    task.lock().unwrap().verification = Some(v);
}

/// Verify a finished download. `Err` means a definite mismatch.
pub async fn verify_task(task: &Arc<Mutex<Task>>, ctx: &Ctx) -> Result<(), String> {
    let (user, headers, etag, url, extra, path, progress) = {
        let t = task.lock().unwrap();
        (
            t.checksum.clone(),
            t.header_checksums.clone(),
            t.etag.clone(),
            t.final_url.clone().unwrap_or_else(|| t.url.clone()),
            t.checksum_urls.clone(),
            t.final_path(),
            t.verify_done.clone(),
        )
    };
    if user.is_none() && !ctx.verify {
        task.lock().unwrap().verification = None;
        return Ok(());
    }
    {
        let mut t = task.lock().unwrap();
        t.status = Status::Verifying;
        t.verify_done.store(0, Ordering::Relaxed);
    }
    set(task, Verification { state: VerifyState::Verifying, ..Default::default() });

    let (found, signature) = if ctx.verify {
        let c = ctx.client.clone();
        let u2 = url.clone();
        let sig = tokio::spawn(async move { find_signature(&c, &u2).await });
        let found = if user.is_some() { None } else { discover(&ctx.client, &url, &extra).await };
        (found, sig.await.ok().flatten())
    } else {
        (None, None)
    };
    let header = headers.into_iter().max_by_key(|c| c.algo.strength());
    let advisory = etag.as_deref().and_then(checksum::etag_md5);
    let chosen = user.or(found).or(header).or(advisory);
    let sig_note = if signature.is_some() { "signature available (not verified)".to_string() } else { String::new() };

    let Some(c) = chosen else {
        set(task, Verification { state: VerifyState::NoChecksum, signature, note: sig_note, ..Default::default() });
        return Ok(());
    };
    let p2 = path.clone();
    let algo = c.algo;
    let actual = tokio::task::spawn_blocking(move || checksum::hash_file(&p2, algo, Some(&progress)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("cannot read file for verification: {e}"))?;
    let ok = actual.eq_ignore_ascii_case(&c.expected);
    let mut v = Verification {
        state: if ok { VerifyState::Verified } else { VerifyState::Failed },
        algo: Some(c.algo),
        expected: c.expected.clone(),
        actual: actual.clone(),
        source: c.source.clone(),
        signature,
        note: sig_note,
    };
    if !ok && c.source == "ETag" {
        v.state = VerifyState::Unverified;
        v.note = "ETag is not an MD5 of the file; not treated as an error".into();
        set(task, v);
        return Ok(());
    }
    set(task, v);
    if ok {
        Ok(())
    } else {
        Err(format!("verification failed ({}): expected {}, got {actual}", c.algo.name(), c.expected))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split() {
        let (d, n) = split_url("https://m.org/iso/2026.10.01/archlinux-x86_64.iso?x=1").unwrap();
        assert_eq!(d, "https://m.org/iso/2026.10.01/");
        assert_eq!(n, "archlinux-x86_64.iso");
        assert_eq!(split_url("https://m.org/a%20b.zip").unwrap().1, "a b.zip");
        assert_eq!(encode("a b+c.tar.zst"), "a%20b%2Bc.tar.zst");
    }
}
