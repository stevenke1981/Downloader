//! Segmented downloader for a single task.
//!
//! The file is split into segments, one HTTP Range request per connection. When a
//! connection finishes its segment it "steals" work by splitting the largest
//! remaining segment in half, so slow connections never hold up the download.
mod http;
mod mirrors;
mod segments;
mod worker;

#[cfg(test)]
use http::content_range;
use http::{Probe, probe, response_range, same_origin};
use mirrors::{Fail, Source};
pub use segments::{claim, plan_segments};
use worker::Shared;

use crate::category::Category;
use crate::checksum;
use crate::google_drive;
use crate::limiter::RateLimiter;
use crate::task::{ConflictPolicy, Seg, Status, Task};
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
    // The URL (query strings may carry tokens) must not skew error classification.
    let e = e.without_url();
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
    headers: &[(String, String)],
    user_agent: Option<&str>,
) -> Result<Probe, String> {
    // Custom headers need a client that does not follow redirects by itself.
    let client = if headers.is_empty() {
        &ctx.client
    } else {
        &ctx.protocol_client
    };
    let mut attempt = 0;
    loop {
        let r = tokio::select! {
            r = probe(client, url, referer, cookie, authorization, headers, user_agent) => r,
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
    // Under Overwrite the new file is published beside the old one; it only replaces it once verified.
    let mut wanted: Option<String> = None;
    loop {
        let (outcome, want) = run_once(task.clone(), ctx.clone()).await?;
        if outcome == Outcome::Cancelled {
            return Ok(Outcome::Cancelled);
        }
        wanted = want.or(wanted);
        match crate::verify::verify_task(&task, &ctx).await {
            Ok(()) => {
                if let Some(name) = wanted {
                    replace_existing(&task, &name);
                }
                return Ok(Outcome::Completed);
            }
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

/// Move the verified download onto `name`, replacing the old file (Overwrite policy).
fn replace_existing(task: &Arc<Mutex<Task>>, name: &str) {
    let mut t = task.lock().unwrap();
    let target = t.dir.join(name);
    if target.is_dir() {
        return;
    }
    if std::fs::rename(t.final_path(), &target).is_ok() {
        t.file_name = name.to_string();
        t.category = Category::from_name(name);
        t.append_log("HTTP output replaced the existing file");
    }
}

/// One transfer, from the primary URL or, when it fails, from a mirror. The second value is the
/// wanted name when an existing file is to be overwritten once verified.
async fn run_once(task: Arc<Mutex<Task>>, ctx: Ctx) -> Result<(Outcome, Option<String>), String> {
    mirrors::run_sources(&task, &ctx).await
}

/// One transfer from one source. `wanted` is the name an existing file is to be replaced at; it
/// is kept across sources, since the first source may pick the name and a mirror finish the file.
/// A mirror is refused (before the task changes) when it cannot
/// continue the task as it stands; see `mirrors::accepts`.
async fn run_source(
    task: &Arc<Mutex<Task>>,
    ctx: &Ctx,
    src: &Source,
    wanted: &mut Option<String>,
) -> Result<Outcome, Fail> {
    let url = &src.url;
    let p = match probe_retry(
        ctx,
        url,
        src.referer.as_deref(),
        src.cookie.as_deref(),
        src.authorization.as_deref(),
        &src.headers,
        src.user_agent.as_deref(),
    )
    .await
    {
        Ok(p) => p,
        Err(_) if ctx.cancel.is_cancelled() => return Ok(Outcome::Cancelled),
        Err(e) => return Err(Fail::Source(e)),
    };
    // The probe follows redirects (reqwest drops Cookie on cross-origin hops), but the workers add the cookie
    // by hand to `p.url`. Only keep it when the redirect target is the same origin as the task URL.
    let cookie = match src.cookie.clone() {
        Some(_) if !same_origin(url, &p.url) => {
            if src.mirror.is_none() {
                eprintln!("login cookie not sent: the download was redirected to another origin");
            }
            None
        }
        c => c,
    };
    let authorization = src
        .authorization
        .clone()
        .filter(|_| same_origin(url, &p.url));
    let headers = if same_origin(url, &p.url) {
        src.headers.clone()
    } else {
        Vec::new()
    };

    let (part, conns) = {
        let mut t = task.lock().unwrap();
        if let Some(k) = src.mirror {
            let partial = t.downloaded() > 0
                && t.segments.iter().all(|s| s.end != u64::MAX)
                && !t.file_name.is_empty()
                && t.part_path().exists();
            if !mirrors::accepts(t.total, partial, &p) {
                t.append_log(format!(
                    "HTTP mirror {k} skipped: size differs or no range support"
                ));
                return Err(Fail::Source(format!("mirror {k} skipped")));
            }
            if !partial {
                // R3: start again from byte 0 with a fresh segment plan.
                t.segments.clear();
            }
        }
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
            // A mirror names the file after the URL the user asked for, if that URL has a name.
            let server = match src.mirror {
                Some(_) => {
                    let effective = t.resolved_url.as_deref().unwrap_or(&t.url);
                    Some(util::name_from_url(effective))
                        .filter(|n| n != "download")
                        .or_else(|| p.name.clone())
                }
                None => p.name.clone(),
            };
            let name = suggested.or(server).unwrap_or_else(|| "download".into());
            // A leftover .part (possibly another task's) is never reused, whatever the policy.
            let policy = if t.dir.join(format!("{name}.part")).exists() {
                ConflictPolicy::Rename
            } else {
                t.conflict
            };
            t.file_name = if policy == ConflictPolicy::Overwrite && t.dir.join(&name).exists() {
                // Keep the old file until the new one is verified.
                *wanted = Some(name.clone());
                util::unique_name(&t.dir, &name)
            } else {
                util::resolve_conflict(&t.dir, &name, policy).map_err(Fail::Local)?
            };
            t.category = Category::from_name(&t.file_name);
        }
        // A mirror has its own validators; R1/R2 above replace this check for it.
        let changed = src.mirror.is_none()
            && ((t.etag.is_some() && p.etag.is_some() && t.etag != p.etag)
                || (t.last_modified.is_some()
                    && p.last_modified.is_some()
                    && t.last_modified != p.last_modified)
                || (t.total.is_some()
                    && p.total.is_some()
                    && t.total != p.total
                    && t.segments.iter().all(|s| s.end != u64::MAX))
                || !p.range
                || !t.part_path().exists());
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
        // The primary's validators stay, so a later run can resume against the primary.
        if src.mirror.is_none() {
            t.etag = p.etag.clone();
            t.last_modified = p.last_modified.clone();
        }
        t.final_url = Some(p.url.clone());
        // A mirror's (usually empty) list never hides a digest the primary advertised.
        if src.mirror.is_none() || t.header_checksums.is_empty() {
            t.header_checksums = p.checksums.clone();
        }
        t.resumable = p.range;
        t.error = None;
        let conns = if p.range { t.connections.max(1) } else { 1 };
        if src.mirror.is_none() {
            t.append_log(format!(
                "HTTP transfer started ({conns} connection(s), ranges: {})",
                p.range
            ));
        }
        (t.part_path(), conns)
    };

    std::fs::create_dir_all(part.parent().unwrap())
        .map_err(|e| Fail::Local(format!("cannot create folder: {e}")))?;
    // Space already reserved by an earlier set_len of this .part does not need to be found again.
    let fresh = task.lock().unwrap().downloaded() == 0;
    let have = if fresh {
        0
    } else {
        std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0)
    };
    let need: u64 = match task.lock().unwrap().total {
        Some(n) => n.saturating_sub(have),
        None => 0,
    };
    if need > 0
        && let Some(free) = util::free_space(part.parent().unwrap())
        && need > free
    {
        return Err(Fail::Local(format!(
            "not enough disk space: need {}, {} free",
            util::human_size(need),
            util::human_size(free)
        )));
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(fresh)
        .open(&part)
        .map_err(|e| Fail::Local(format!("cannot open {}: {e}", part.display())))?;
    if let (true, Some(n)) = (fresh, p.total) {
        let _ = file.set_len(n);
    }

    let sh = Arc::new(Shared {
        task: task.clone(),
        file,
        url: p.url.clone(),
        drive_download: p.drive_download,
        range: p.range,
        // If-Range uses this source's own validator.
        etag: p.etag.clone(),
        referer: src.referer.clone(),
        cookie,
        authorization,
        headers,
        user_agent: src.user_agent.clone(),
        ctx: ctx.clone(),
    });
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..conns {
        set.spawn(sh.clone().worker());
    }
    // A local failure (disk, write) wins over a server failure: no mirror can fix it.
    let mut last_err: Option<Fail> = None;
    while let Some(r) = set.join_next().await {
        if let Ok(Err(e)) = r
            && !matches!(last_err, Some(Fail::Local(_)))
        {
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
        return Err(last_err.unwrap_or_else(|| Fail::Source("download incomplete".into())));
    }
    // Every byte of [0, total) must be covered by a finished segment.
    {
        let t = task.lock().unwrap();
        let mut ranges: Vec<(u64, u64)> = t.segments.iter().map(|s| (s.start, s.end)).collect();
        ranges.sort();
        let mut covered = 0u64;
        for (a, b) in ranges {
            if a > covered {
                return Err(Fail::Local(format!(
                    "download incomplete: gap at byte {covered}"
                )));
            }
            covered = covered.max(b);
        }
        if covered < total {
            return Err(Fail::Local(format!(
                "download incomplete: {covered} of {total} bytes"
            )));
        }
    }
    {
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&part)
            .map_err(|e| Fail::Local(e.to_string()))?;
        f.set_len(total).map_err(|e| Fail::Local(e.to_string()))?;
    }
    let final_path = if final_path.exists() {
        let mut t = task.lock().unwrap();
        match t.conflict {
            // Publish beside the old file; it is replaced after verification.
            ConflictPolicy::Overwrite => {
                *wanted = Some(t.file_name.clone());
                let n = util::unique_name(&t.dir, &t.file_name);
                t.file_name = n;
                t.final_path()
            }
            ConflictPolicy::Skip => {
                return Err(Fail::Local(format!(
                    "file exists: {}",
                    final_path.display()
                )));
            }
            ConflictPolicy::Rename => {
                let n = util::unique_name(&t.dir, &t.file_name);
                t.file_name = n;
                t.final_path()
            }
        }
    } else {
        final_path
    };
    let len = std::fs::metadata(&part)
        .map_err(|e| Fail::Local(e.to_string()))?
        .len();
    if len != total || p.total.is_some_and(|n| n != total) {
        return Err(Fail::Local(format!(
            "size mismatch: file has {len} bytes, server reported {}",
            p.total.unwrap_or(total)
        )));
    }
    std::fs::rename(&part, &final_path).map_err(|e| Fail::Local(format!("rename failed: {e}")))?;
    if src.mirror.is_none() {
        task.lock().unwrap().append_log("HTTP output published");
    }
    Ok(Outcome::Completed)
}

#[cfg(test)]
mod tests;
