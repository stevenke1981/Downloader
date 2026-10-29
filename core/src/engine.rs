//! Segmented downloader for a single task.
//!
//! The file is split into segments, one HTTP Range request per connection. When a
//! connection finishes its segment it "steals" work by splitting the largest
//! remaining segment in half, so slow connections never hold up the download.
mod http;
mod segments;
mod worker;

use http::{probe, response_range, same_origin, Probe};
#[cfg(test)]
use http::content_range;
pub use segments::{claim, plan_segments};
use worker::Shared;

use crate::category::Category;
use crate::checksum;
use crate::google_drive;
use crate::limiter::RateLimiter;
use crate::task::{Seg, Status, Task};
use crate::util;
use futures_util::StreamExt;
use reqwest::header::{CONTENT_DISPOSITION, CONTENT_RANGE, ETAG, IF_RANGE, LAST_MODIFIED, RANGE};
use std::fs::File;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Don't create segments smaller than this.
pub const MIN_SPLIT: u64 = 256 * 1024;

#[derive(Clone)]
pub struct Ctx {
    pub client: reqwest::Client,
    /// Native adapters inspect redirects before forwarding origin-scoped credentials.
    pub protocol_client: reqwest::Client,
    pub global: Arc<RateLimiter>,
    pub local: Arc<RateLimiter>,
    pub retries: u32,
    pub cancel: CancellationToken,
    /// Look for checksum files / headers automatically after downloading.
    pub verify: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Cancelled,
}

/// HTTP status code from an error string such as "HTTP 404 Not Found".
fn http_code(e: &str) -> Option<u16> {
    e.strip_prefix("HTTP ")?.get(..3)?.parse().ok()
}

/// 4xx responses won't fix themselves on retry, except 408 (timeout) and 429 (rate limit).
fn permanent(code: u16) -> bool {
    (400..500).contains(&code) && code != 408 && code != 429
}

const COOKIE_HINT: &str =
    " — login cookies are not kept after a restart; add the download again from the browser";

/// Adds a hint to a 401/403 when the login cookie used before a restart is gone (cookies are never persisted).
fn with_cookie_hint(err: String, lost_cookie: bool) -> String {
    if lost_cookie && matches!(http_code(&err), Some(401 | 403)) {
        format!("{err}{COOKIE_HINT}")
    } else {
        err
    }
}

fn err_str(e: reqwest::Error) -> String {
    let mut s = e.to_string();
    let mut src = std::error::Error::source(&e);
    while let Some(c) = src {
        s = format!("{s}: {c}");
        src = c.source();
    }
    s
}

async fn probe_retry(
    ctx: &Ctx,
    url: &str,
    referer: Option<&str>,
    cookie: Option<&str>,
    authorization: Option<&str>,
) -> Result<Probe, String> {
    let mut attempt = 0;
    loop {
        let r = tokio::select! {
            r = probe(&ctx.client, url, referer, cookie, authorization) => r,
            _ = ctx.cancel.cancelled() => return Err(String::new()),
        };
        match r {
            Ok(p) => return Ok(p),
            Err(e)
                if attempt >= ctx.retries
                    || http_code(&e).is_some_and(permanent)
                    || e.starts_with("Google Drive:") =>
            {
                return Err(e);
            }
            Err(_) => {
                attempt += 1;
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_millis(500u64 << attempt.min(6))) => {},
                    _ = ctx.cancel.cancelled() => return Err(String::new()),
                }
            }
        }
    }
}

/// Download (or resume) `task`, then verify it. Segment progress is updated in place
/// so the caller can persist it at any time. Does not change `task.status` except to
/// `Verifying`. On a checksum mismatch the file is downloaded again once.
pub async fn run(task: Arc<Mutex<Task>>, ctx: Ctx) -> Result<Outcome, String> {
    {
        let t = task.lock().unwrap();
        util::validate_source(&t.url)?;
    }
    let external = {
        let t = task.lock().unwrap();
        crate::protocols::handles(&t.url, t.kind)
    };
    if external {
        return crate::protocols::run(task, ctx).await;
    }
    loop {
        if run_once(task.clone(), ctx.clone()).await? == Outcome::Cancelled {
            return Ok(Outcome::Cancelled);
        }
        match crate::verify::verify_task(&task, &ctx).await {
            Ok(()) => return Ok(Outcome::Completed),
            Err(e) => {
                let mut t = task.lock().unwrap();
                if t.verify_retried {
                    return Err(e);
                }
                // We only have a whole-file hash, so we can't tell which part is bad:
                // download everything again once.
                t.verify_retried = true;
                let _ = std::fs::remove_file(t.final_path());
                t.segments.clear();
                t.total = None;
                t.status = Status::Downloading;
            }
        }
    }
}

async fn run_once(task: Arc<Mutex<Task>>, ctx: Ctx) -> Result<Outcome, String> {
    let (url, referer, cookie, had_cookie, authorization) = {
        let mut t = task.lock().unwrap();
        t.append_log("HTTP probe started");
        (
            t.url.clone(),
            t.referer.clone(),
            t.cookie.clone().filter(|c| !c.trim().is_empty()),
            t.had_cookie,
            t.authorization.clone(),
        )
    };
    let lost_cookie = had_cookie && cookie.is_none();
    let p = match probe_retry(
        &ctx,
        &url,
        referer.as_deref(),
        cookie.as_deref(),
        authorization.as_deref(),
    )
    .await
    {
        Ok(p) => p,
        Err(_) if ctx.cancel.is_cancelled() => return Ok(Outcome::Cancelled),
        Err(e) => return Err(with_cookie_hint(e, lost_cookie)),
    };
    // The probe follows redirects (reqwest drops Cookie on cross-origin hops), but the workers add the cookie
    // by hand to `p.url`. Only keep it when the redirect target is the same origin as the task URL.
    let cookie = match cookie {
        Some(_) if !same_origin(&url, &p.url) => {
            eprintln!("login cookie not sent: the download was redirected to another origin");
            None
        }
        c => c,
    };
    let authorization = authorization.filter(|_| same_origin(&url, &p.url));

    let (part, conns) = {
        let mut t = task.lock().unwrap();
        if t.file_name.is_empty() {
            let suggested = t
                .suggested_name
                .as_deref()
                .map(util::sanitize)
                .filter(|n| !n.is_empty())
                .filter(|n| {
                    !p.drive_download
                        || !matches!(
                            n.to_ascii_lowercase().as_str(),
                            "identifier" | "uc" | "download" | "view" | "open"
                        )
                });
            let name = suggested
                .or_else(|| p.name.clone())
                .unwrap_or_else(|| "download".into());
            t.file_name = util::unique_name(&t.dir, &name);
            t.category = Category::from_name(&t.file_name);
        }
        let changed = (t.etag.is_some() && p.etag.is_some() && t.etag != p.etag)
            || (t.last_modified.is_some()
                && p.last_modified.is_some()
                && t.last_modified != p.last_modified)
            || (t.total.is_some()
                && p.total.is_some()
                && t.total != p.total
                && t.segments.iter().all(|s| s.end != u64::MAX))
            || !p.range
            || !t.part_path().exists();
        if changed && t.downloaded() > 0 {
            t.segments.clear();
        }
        if t.segments.is_empty() {
            t.total = p.total;
            t.segments = match (p.range, p.total) {
                (true, Some(n)) => plan_segments(n, t.connections),
                (_, Some(n)) => vec![Seg::new(0, n)],
                _ => vec![Seg::new(0, u64::MAX)],
            };
        }
        for s in &mut t.segments {
            s.busy = false;
        }
        t.etag = p.etag.clone();
        t.final_url = Some(p.url.clone());
        t.header_checksums = p.checksums.clone();
        t.last_modified = p.last_modified.clone();
        t.resumable = p.range;
        t.error = None;
        let conns = if p.range { t.connections.max(1) } else { 1 };
        t.append_log(format!(
            "HTTP transfer started ({conns} connection(s), ranges: {})",
            p.range
        ));
        (t.part_path(), conns)
    };

    std::fs::create_dir_all(part.parent().unwrap())
        .map_err(|e| format!("cannot create folder: {e}"))?;
    let fresh = task.lock().unwrap().downloaded() == 0;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(fresh)
        .open(&part)
        .map_err(|e| format!("cannot open {}: {e}", part.display()))?;
    if let (true, Some(n)) = (fresh, p.total) {
        let _ = file.set_len(n);
    }

    let sh = Arc::new(Shared {
        task: task.clone(),
        file,
        url: p.url.clone(),
        drive_download: p.drive_download,
        range: p.range,
        etag: p.etag.clone(),
        referer,
        cookie,
        authorization,
        ctx: ctx.clone(),
    });
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..conns {
        set.spawn(sh.clone().worker());
    }
    let mut last_err = None;
    while let Some(r) = set.join_next().await {
        if let Ok(Err(e)) = r {
            last_err = Some(e);
        }
    }
    drop(sh);
    if ctx.cancel.is_cancelled() {
        task.lock()
            .unwrap()
            .append_log("HTTP transfer stopped; partial data retained");
        return Ok(Outcome::Cancelled);
    }
    let (complete, total, final_path) = {
        let t = task.lock().unwrap();
        let complete = t.segments.iter().all(Seg::done);
        let total = t.total.unwrap_or_else(|| t.downloaded());
        (complete, total, t.final_path())
    };
    if !complete {
        return Err(with_cookie_hint(
            last_err.unwrap_or_else(|| "download incomplete".into()),
            lost_cookie,
        ));
    }
    // Every byte of [0, total) must be covered by a finished segment.
    {
        let t = task.lock().unwrap();
        let mut ranges: Vec<(u64, u64)> = t.segments.iter().map(|s| (s.start, s.end)).collect();
        ranges.sort();
        let mut covered = 0u64;
        for (a, b) in ranges {
            if a > covered {
                return Err(format!("download incomplete: gap at byte {covered}"));
            }
            covered = covered.max(b);
        }
        if covered < total {
            return Err(format!("download incomplete: {covered} of {total} bytes"));
        }
    }
    {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&part)
            .map_err(|e| e.to_string())?;
        f.set_len(total).map_err(|e| e.to_string())?;
    }
    let final_path = if final_path.exists() {
        let mut t = task.lock().unwrap();
        let n = util::unique_name(&t.dir, &t.file_name);
        t.file_name = n;
        t.final_path()
    } else {
        final_path
    };
    let len = std::fs::metadata(&part).map_err(|e| e.to_string())?.len();
    if len != total || p.total.is_some_and(|n| n != total) {
        return Err(format!(
            "size mismatch: file has {len} bytes, server reported {}",
            p.total.unwrap_or(total)
        ));
    }
    std::fs::rename(&part, &final_path).map_err(|e| format!("rename failed: {e}"))?;
    task.lock().unwrap().append_log("HTTP output published");
    Ok(Outcome::Completed)
}

#[cfg(test)]
mod tests;
