//! Segmented downloader for a single task.
//!
//! The file is split into segments, one HTTP Range request per connection. When a
//! connection finishes its segment it "steals" work by splitting the largest
//! remaining segment in half, so slow connections never hold up the download.
use crate::category::Category;
use crate::checksum;
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
    pub global: Arc<RateLimiter>,
    pub local: Arc<RateLimiter>,
    pub retries: u32,
    pub cancel: CancellationToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    Cancelled,
}

#[derive(Debug, Default)]
struct Probe {
    url: String,
    range: bool,
    total: Option<u64>,
    etag: Option<String>,
    last_modified: Option<String>,
    name: Option<String>,
}

fn hdr(r: &reqwest::Response, h: reqwest::header::HeaderName) -> Option<String> {
    r.headers().get(h).and_then(|v| v.to_str().ok()).map(str::to_string)
}

async fn probe(client: &reqwest::Client, url: &str) -> Result<Probe, String> {
    let resp = client.get(url).header(RANGE, "bytes=0-0").send().await.map_err(err_str)?;
    let st = resp.status();
    if !st.is_success() {
        return Err(format!("HTTP {st}"));
    }
    let mut p = Probe {
        url: resp.url().to_string(),
        etag: hdr(&resp, ETAG).filter(|e| !e.starts_with("W/")),
        last_modified: hdr(&resp, LAST_MODIFIED),
        name: hdr(&resp, CONTENT_DISPOSITION).and_then(|d| util::name_from_disposition(&d)),
        ..Default::default()
    };
    if st.as_u16() == 206 {
        p.total = hdr(&resp, CONTENT_RANGE).and_then(|c| c.rsplit('/').next().and_then(|t| t.trim().parse().ok()));
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
    (0..n).map(|i| Seg::new(i * step, if i == n - 1 { total } else { (i + 1) * step })).collect()
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
    segs.push(Seg { start: mid, pos: mid, end, busy: true });
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
    range: bool,
    etag: Option<String>,
    ctx: Ctx,
}

async fn fetch(sh: &Shared, idx: usize) -> Result<(), FetchErr> {
    let (pos, end) = {
        let mut t = sh.task.lock().unwrap();
        if !sh.range {
            // No range support: every attempt restarts the file from scratch.
            let s = &mut t.segments[idx];
            s.pos = s.start;
        }
        let s = t.segments[idx];
        (s.pos, s.end)
    };
    if pos >= end {
        return Ok(());
    }
    let mut req = sh.ctx.client.get(&sh.url);
    if sh.range {
        let r = if end == u64::MAX { format!("bytes={pos}-") } else { format!("bytes={pos}-{}", end - 1) };
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
    if sh.range && st.as_u16() != 206 {
        return Err(if st.as_u16() == 200 {
            FetchErr::Fatal("the file changed on the server (range request answered with the whole file)".into())
        } else if st.is_client_error() && st.as_u16() != 429 {
            FetchErr::Fatal(format!("HTTP {st}"))
        } else {
            FetchErr::Retry(format!("HTTP {st}"))
        });
    }
    if !st.is_success() {
        return Err(if st.is_client_error() && st.as_u16() != 429 {
            FetchErr::Fatal(format!("HTTP {st}"))
        } else {
            FetchErr::Retry(format!("HTTP {st}"))
        });
    }
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
                return if s.done() { Ok(()) } else { Err(FetchErr::Retry("connection closed early".into())) };
            }
            Some(Err(e)) => return Err(FetchErr::Retry(err_str(e))),
            Some(Ok(c)) => c,
        };
        let n = chunk.len() as u64;
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
            write_at(&sh.file, &chunk[..take], s.pos).map_err(|e| FetchErr::Fatal(format!("write failed: {e}")))?;
            s.pos += take as u64;
        }
        if s.done() {
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
            match fetch(&sh, idx).await {
                Ok(()) => break Ok(()),
                Err(FetchErr::Cancelled) => break Err(None),
                Err(FetchErr::Fatal(e)) => break Err(Some(e)),
                Err(FetchErr::Retry(e)) => {
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

async fn probe_retry(ctx: &Ctx, url: &str) -> Result<Probe, String> {
    let mut attempt = 0;
    loop {
        let r = tokio::select! {
            r = probe(&ctx.client, url) => r,
            _ = ctx.cancel.cancelled() => return Err(String::new()),
        };
        match r {
            Ok(p) => return Ok(p),
            Err(e) if attempt >= ctx.retries || e.starts_with("HTTP 4") => return Err(e),
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

/// Download (or resume) `task`. Segment progress is updated in place so the caller
/// can persist it at any time. Does not change `task.status` except to `Verifying`.
pub async fn run(task: Arc<Mutex<Task>>, ctx: Ctx) -> Result<Outcome, String> {
    let url = task.lock().unwrap().url.clone();
    let p = match probe_retry(&ctx, &url).await {
        Ok(p) => p,
        Err(_) if ctx.cancel.is_cancelled() => return Ok(Outcome::Cancelled),
        Err(e) => return Err(e),
    };

    let (part, conns) = {
        let mut t = task.lock().unwrap();
        if t.file_name.is_empty() {
            let name = p.name.clone().unwrap_or_else(|| "download".into());
            t.file_name = util::unique_name(&t.dir, &name);
            t.category = Category::from_name(&t.file_name);
        }
        let changed = (t.etag.is_some() && p.etag.is_some() && t.etag != p.etag)
            || (t.last_modified.is_some() && p.last_modified.is_some() && t.last_modified != p.last_modified)
            || (t.total.is_some() && p.total.is_some() && t.total != p.total && t.segments.iter().all(|s| s.end != u64::MAX))
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
        t.last_modified = p.last_modified.clone();
        t.resumable = p.range;
        t.error = None;
        let conns = if p.range { t.connections.max(1) } else { 1 };
        (t.part_path(), conns)
    };

    std::fs::create_dir_all(part.parent().unwrap()).map_err(|e| format!("cannot create folder: {e}"))?;
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

    let sh = Arc::new(Shared { task: task.clone(), file, url: p.url.clone(), range: p.range, etag: p.etag.clone(), ctx: ctx.clone() });
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
        return Ok(Outcome::Cancelled);
    }
    let (complete, total, final_path, sum) = {
        let t = task.lock().unwrap();
        let complete = t.segments.iter().all(Seg::done);
        let total = t.total.unwrap_or_else(|| t.downloaded());
        (complete, total, t.final_path(), t.checksum.clone())
    };
    if !complete {
        return Err(last_err.unwrap_or_else(|| "download incomplete".into()));
    }
    {
        let f = std::fs::OpenOptions::new().write(true).open(&part).map_err(|e| e.to_string())?;
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
    std::fs::rename(&part, &final_path).map_err(|e| format!("rename failed: {e}"))?;
    if let Some(c) = sum {
        task.lock().unwrap().status = Status::Verifying;
        let fp = final_path.clone();
        tokio::task::spawn_blocking(move || checksum::verify(&fp, &c)).await.map_err(|e| e.to_string())??;
    }
    Ok(Outcome::Completed)
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let small = &mut vec![Seg { start: 0, pos: 0, end: 1000, busy: true }];
        assert!(claim(small, true).is_none());
    }
}
