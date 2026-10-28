//! Segmented downloader for a single task.
//!
//! The file is split into segments, one HTTP Range request per connection. When a
//! connection finishes its segment it "steals" work by splitting the largest
//! remaining segment in half, so slow connections never hold up the download.
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

#[derive(Debug, Default)]
struct Probe {
    url: String,
    drive_download: bool,
    range: bool,
    total: Option<u64>,
    etag: Option<String>,
    last_modified: Option<String>,
    name: Option<String>,
    checksums: Vec<checksum::Checksum>,
}

fn hdr(r: &reqwest::Response, h: reqwest::header::HeaderName) -> Option<String> {
    r.headers()
        .get(h)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

/// A single satisfied byte range. Keep the inclusive end so u64::MAX cannot wrap.
fn content_range(value: &str) -> Option<(u64, u64, Option<u64>)> {
    fn number(s: &str) -> Option<u64> {
        if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse().ok()
    }
    let (unit, value) = value.trim().split_once(' ')?;
    if !unit.eq_ignore_ascii_case("bytes") {
        return None;
    }
    let (range, total) = value.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end) = (number(start)?, number(end)?);
    let total = if total == "*" {
        None
    } else {
        Some(number(total)?)
    };
    if start > end || total.is_some_and(|n| end >= n) {
        return None;
    }
    Some((start, end, total))
}

fn response_range(resp: &reqwest::Response) -> Result<(u64, u64, Option<u64>), String> {
    let mut values = resp.headers().get_all(CONTENT_RANGE).iter();
    let value = values.next().and_then(|v| v.to_str().ok());
    if values.next().is_some() {
        return Err("invalid Content-Range: multiple fields".into());
    }
    value
        .and_then(content_range)
        .ok_or_else(|| "invalid or missing Content-Range".into())
}

/// HTTP status code from an error string such as "HTTP 404 Not Found".
fn http_code(e: &str) -> Option<u16> {
    e.strip_prefix("HTTP ")?.get(..3)?.parse().ok()
}

/// 4xx responses won't fix themselves on retry, except 408 (timeout) and 429 (rate limit).
fn permanent(code: u16) -> bool {
    (400..500).contains(&code) && code != 408 && code != 429
}

/// True when `a` and `b` share scheme, host and port (default ports count as the same). Anything unparseable is not.
fn same_origin(a: &str, b: &str) -> bool {
    match (reqwest::Url::parse(a), reqwest::Url::parse(b)) {
        (Ok(x), Ok(y)) => {
            x.scheme() == y.scheme()
                && x.host_str().is_some()
                && x.host_str() == y.host_str()
                && x.port_or_known_default() == y.port_or_known_default()
        }
        _ => false,
    }
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

async fn probe(
    client: &reqwest::Client,
    url: &str,
    referer: Option<&str>,
    cookie: Option<&str>,
    authorization: Option<&str>,
) -> Result<Probe, String> {
    let mut target = google_drive::download_url(url)?;
    let mut drive_download = google_drive::is_drive(&target);
    let mut confirmations = 0;
    let resp = loop {
        let mut req = client.get(target.clone()).header(RANGE, "bytes=0-0");
        if let Some(r) = referer {
            req = req.header(reqwest::header::REFERER, r);
        }
        // The browser header applies only to its original origin, including when
        // following a confirmation form. Never copy it to another Google host.
        if let Some(c) = cookie.filter(|_| same_origin(url, target.as_str())) {
            req = req.header(reqwest::header::COOKIE, c);
        }
        if let Some(value) = authorization.filter(|_| same_origin(url, target.as_str())) {
            req = req.header(reqwest::header::AUTHORIZATION, value);
        }
        let resp = req.send().await.map_err(err_str)?;
        if google_drive::is_sign_in(resp.url()) {
            return Err(google_drive::SIGN_IN.into());
        }
        if !resp.status().is_success() {
            return Err(format!("HTTP {}", resp.status()));
        }
        drive_download |= google_drive::is_drive(resp.url());
        if drive_download && google_drive::is_html_page(&resp) {
            if !google_drive::is_drive(resp.url()) || confirmations >= 3 {
                return Err(google_drive::UNAVAILABLE.into());
            }
            let next = google_drive::confirmation_url(resp).await?;
            if next == target {
                return Err(google_drive::UNAVAILABLE.into());
            }
            target = next;
            confirmations += 1;
            continue;
        }
        break resp;
    };
    let st = resp.status();
    if !st.is_success() {
        return Err(format!("HTTP {st}"));
    }
    let mut p = Probe {
        url: resp.url().to_string(),
        drive_download,
        etag: hdr(&resp, ETAG).filter(|e| !e.starts_with("W/")),
        last_modified: hdr(&resp, LAST_MODIFIED),
        name: hdr(&resp, CONTENT_DISPOSITION).and_then(|d| util::name_from_disposition(&d)),
        ..Default::default()
    };
    p.checksums = checksum::from_headers(resp.headers(), st.as_u16() == 200);
    if st.as_u16() == 206 {
        let (start, end, total) = response_range(&resp)?;
        if start != 0 || end != 0 || resp.content_length().is_some_and(|n| n != 1) {
            return Err("Content-Range does not match the probe (bytes=0-0)".into());
        }
        p.total = total;
        p.range = p.total.is_some();
    } else {
        p.total = resp.content_length();
    }
    if p.name.is_none() {
        let n = util::name_from_url(&p.url);
        if n != "download" {
            p.name = Some(n);
        }
    }
    Ok(p)
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

#[cfg(unix)]
fn write_at(f: &File, buf: &[u8], off: u64) -> std::io::Result<()> {
    use std::os::unix::fs::FileExt;
    f.write_all_at(buf, off)
}
#[cfg(windows)]
fn write_at(f: &File, mut buf: &[u8], mut off: u64) -> std::io::Result<()> {
    use std::os::windows::fs::FileExt;
    while !buf.is_empty() {
        let n = f.seek_write(buf, off)?;
        buf = &buf[n..];
        off += n as u64;
    }
    Ok(())
}

/// Initial equal-size split.
pub fn plan_segments(total: u64, conns: u32) -> Vec<Seg> {
    let n = (total / MIN_SPLIT).clamp(1, conns.max(1) as u64);
    let step = total / n;
    (0..n)
        .map(|i| Seg::new(i * step, if i == n - 1 { total } else { (i + 1) * step }))
        .collect()
}

/// Pick work for a connection: an idle unfinished segment, or else split the busy
/// segment with the most bytes left. Returns the segment index (now marked busy).
pub fn claim(segs: &mut Vec<Seg>, can_split: bool) -> Option<usize> {
    if let Some(i) = segs.iter().position(|s| !s.busy && !s.done()) {
        segs[i].busy = true;
        return Some(i);
    }
    if !can_split {
        return None;
    }
    let (i, rem) = segs
        .iter()
        .enumerate()
        .filter(|(_, s)| s.busy && s.end != u64::MAX)
        .map(|(i, s)| (i, s.remaining()))
        .max_by_key(|x| x.1)?;
    if rem < 2 * MIN_SPLIT {
        return None;
    }
    let mid = segs[i].pos + rem / 2;
    let end = segs[i].end;
    segs[i].end = mid;
    segs.push(Seg {
        start: mid,
        pos: mid,
        end,
        busy: true,
    });
    Some(segs.len() - 1)
}

enum FetchErr {
    Cancelled,
    Fatal(String),
    Retry(String),
}

struct Shared {
    task: Arc<Mutex<Task>>,
    file: File,
    url: String,
    drive_download: bool,
    range: bool,
    etag: Option<String>,
    referer: Option<String>,
    cookie: Option<String>,
    authorization: Option<String>,
    ctx: Ctx,
}

async fn fetch(sh: &Shared, idx: usize) -> Result<(), FetchErr> {
    let (pos, end, total) = {
        let mut t = sh.task.lock().unwrap();
        if !sh.range {
            // No range support: every attempt restarts the file from scratch.
            let s = &mut t.segments[idx];
            s.pos = s.start;
        }
        let s = t.segments[idx];
        (s.pos, s.end, t.total)
    };
    if pos >= end {
        return Ok(());
    }
    let mut req = sh.ctx.client.get(&sh.url);
    if let Some(r) = &sh.referer {
        req = req.header(reqwest::header::REFERER, r);
    }
    if let Some(c) = &sh.cookie {
        req = req.header(reqwest::header::COOKIE, c);
    }
    if let Some(value) = &sh.authorization {
        req = req.header(reqwest::header::AUTHORIZATION, value);
    }
    if sh.range {
        let r = if end == u64::MAX {
            format!("bytes={pos}-")
        } else {
            format!("bytes={pos}-{}", end - 1)
        };
        req = req.header(RANGE, r);
        if let Some(e) = &sh.etag {
            req = req.header(IF_RANGE, e);
        }
    }
    let resp = tokio::select! {
        r = req.send() => r.map_err(|e| FetchErr::Retry(err_str(e)))?,
        _ = sh.ctx.cancel.cancelled() => return Err(FetchErr::Cancelled),
    };
    let st = resp.status();
    google_drive::validate_file_response(&resp, sh.drive_download).map_err(FetchErr::Fatal)?;
    if sh.range && st.as_u16() != 206 {
        return Err(if st.as_u16() == 200 {
            FetchErr::Fatal(
                "the file changed on the server (range request answered with the whole file)"
                    .into(),
            )
        } else if permanent(st.as_u16()) {
            FetchErr::Fatal(format!("HTTP {st}"))
        } else {
            FetchErr::Retry(format!("HTTP {st}"))
        });
    }
    if !st.is_success() {
        return Err(if permanent(st.as_u16()) {
            FetchErr::Fatal(format!("HTTP {st}"))
        } else {
            FetchErr::Retry(format!("HTTP {st}"))
        });
    }
    let mut response_remaining = if sh.range {
        let (actual_start, actual_end, actual_total) =
            response_range(&resp).map_err(FetchErr::Fatal)?;
        if actual_start != pos
            || actual_end >= end
            || actual_total.is_some_and(|n| Some(n) != total)
            || resp
                .content_length()
                .is_some_and(|n| n != actual_end - actual_start + 1)
        {
            return Err(FetchErr::Fatal(
                "Content-Range does not match the requested segment or file size".into(),
            ));
        }
        // RFC 9110 permits a shorter prefix and an unknown complete length (*).
        // A valid prefix advances this segment; the worker requests its remainder.
        Some(actual_end - actual_start + 1)
    } else if st.as_u16() == 206 {
        return Err(FetchErr::Fatal(
            "unexpected partial response to a full-file request".into(),
        ));
    } else {
        None
    };
    let mut stream = resp.bytes_stream();
    loop {
        let next = tokio::select! {
            c = stream.next() => c,
            _ = sh.ctx.cancel.cancelled() => return Err(FetchErr::Cancelled),
        };
        let chunk = match next {
            None => {
                let mut t = sh.task.lock().unwrap();
                let s = &mut t.segments[idx];
                if s.end == u64::MAX {
                    s.end = s.pos;
                    let p = s.pos;
                    t.total = Some(p);
                    return Ok(());
                }
                return if s.done() {
                    Ok(())
                } else {
                    Err(FetchErr::Retry("connection closed early".into()))
                };
            }
            Some(Err(e)) => return Err(FetchErr::Retry(err_str(e))),
            Some(Ok(c)) => c,
        };
        let n = chunk.len() as u64;
        if let Some(remaining) = &mut response_remaining {
            if n > *remaining {
                return Err(FetchErr::Fatal(
                    "response body exceeds Content-Range".into(),
                ));
            }
            *remaining -= n;
        }
        let wait = sh.ctx.local.reserve(n).max(sh.ctx.global.reserve(n));
        if !wait.is_zero() {
            tokio::select! {
                _ = tokio::time::sleep(wait) => {},
                _ = sh.ctx.cancel.cancelled() => return Err(FetchErr::Cancelled),
            }
        }
        let mut t = sh.task.lock().unwrap();
        let s = &mut t.segments[idx];
        let take = n.min(s.remaining()) as usize;
        if take > 0 {
            write_at(&sh.file, &chunk[..take], s.pos)
                .map_err(|e| FetchErr::Fatal(format!("write failed: {e}")))?;
            s.pos += take as u64;
        }
        if s.done() || response_remaining == Some(0) {
            return Ok(());
        }
    }
}

async fn worker(sh: Arc<Shared>) -> Result<(), String> {
    loop {
        let idx = {
            let mut t = sh.task.lock().unwrap();
            match claim(&mut t.segments, sh.range) {
                Some(i) => i,
                None => return Ok(()),
            }
        };
        let mut attempt = 0u32;
        let res = loop {
            let before = sh.task.lock().unwrap().segments[idx].pos;
            match fetch(&sh, idx).await {
                Ok(()) => break Ok(()),
                Err(FetchErr::Cancelled) => break Err(None),
                Err(FetchErr::Fatal(e)) => break Err(Some(e)),
                Err(FetchErr::Retry(e)) => {
                    // A connection that moved the segment forward was healthy, so only count consecutive
                    // failures without progress. Non-range fetches restart from the segment start every time,
                    // so their progress never carries over and they keep counting.
                    if sh.range && sh.task.lock().unwrap().segments[idx].pos > before {
                        attempt = 0;
                    }
                    attempt += 1;
                    if attempt > sh.ctx.retries {
                        break Err(Some(e));
                    }
                    let backoff = Duration::from_millis(500u64 << attempt.min(6));
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {},
                        _ = sh.ctx.cancel.cancelled() => break Err(None),
                    }
                }
            }
        };
        sh.task.lock().unwrap().segments[idx].busy = false;
        match res {
            Ok(()) => {}
            Err(None) => return Ok(()),
            Err(Some(e)) => {
                // Let other connections finish; then report.
                return Err(e);
            }
        }
    }
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
        crate::external::handles(&t.url, t.kind)
    };
    if external {
        return crate::external::run(task, ctx).await;
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
        set.spawn(worker(sh.clone()));
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
mod tests {
    use super::*;
    #[test]
    fn validates_content_range_syntax_and_bounds() {
        assert_eq!(content_range("bytes 10-19/100"), Some((10, 19, Some(100))));
        assert_eq!(content_range("BYTES 0-0/*"), Some((0, 0, None)));
        for bad in [
            "",
            "bytes */100",
            "items 0-0/1",
            "bytes 1-0/2",
            "bytes 0-2/2",
            "bytes 0-0/0",
            "bytes +0-0/1",
            "bytes 0-0/1/2",
            "bytes 0-18446744073709551616/*",
        ] {
            assert_eq!(content_range(bad), None, "{bad}");
        }
    }

    #[test]
    fn same_origin_rules() {
        assert!(same_origin(
            "https://a.example/x.zip",
            "https://a.example/y/z.zip?q=1"
        ));
        assert!(
            same_origin("https://a.example/x", "https://A.example:443/x"),
            "default port and host case"
        );
        assert!(
            !same_origin("https://a.example/x", "https://cdn.example/x"),
            "different host"
        );
        assert!(
            !same_origin("https://a.example/x", "http://a.example/x"),
            "scheme downgrade"
        );
        assert!(
            !same_origin("https://a.example/x", "https://a.example:8443/x"),
            "different port"
        );
        assert!(!same_origin("https://a.example/x", "not a url"));
        assert!(!same_origin("", ""));
    }

    #[test]
    fn cookie_hint_only_for_auth_errors_after_restart() {
        let h = with_cookie_hint("HTTP 403 Forbidden".into(), true);
        assert!(
            h.starts_with("HTTP 403 Forbidden — login cookies are not kept after a restart"),
            "{h}"
        );
        assert!(
            h.ends_with("add the download again from the browser"),
            "{h}"
        );
        assert!(
            with_cookie_hint("HTTP 401 Unauthorized".into(), true)
                .contains("not kept after a restart")
        );
        assert_eq!(
            with_cookie_hint("HTTP 404 Not Found".into(), true),
            "HTTP 404 Not Found"
        );
        assert_eq!(
            with_cookie_hint("HTTP 403 Forbidden".into(), false),
            "HTTP 403 Forbidden"
        );
        assert_eq!(
            with_cookie_hint("connection reset".into(), true),
            "connection reset"
        );
    }

    #[test]
    fn plan_and_steal() {
        let mut s = plan_segments(10 << 20, 4);
        assert_eq!(s.len(), 4);
        assert_eq!(s.last().unwrap().end, 10 << 20);
        for _ in 0..4 {
            claim(&mut s, true).unwrap();
        }
        // first segment finishes; its connection should split the largest remaining one
        s[0].pos = s[0].end;
        s[0].busy = false;
        s[1].pos += 100;
        let i = claim(&mut s, true).unwrap();
        assert_eq!(i, 4);
        assert_eq!(s[4].end, 10 << 20);
        assert_eq!(s[3].end, s[4].start);
        assert!(s.iter().all(|x| x.start <= x.end));
        let small = &mut vec![Seg {
            start: 0,
            pos: 0,
            end: 1000,
            busy: true,
        }];
        assert!(claim(small, true).is_none());
    }
}
