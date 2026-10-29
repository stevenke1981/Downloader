use super::*;

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

enum FetchErr {
    Cancelled,
    Fatal(String),
    Retry(String),
}

pub(super) struct Shared {
    pub(super) task: Arc<Mutex<Task>>,
    pub(super) file: File,
    pub(super) url: String,
    pub(super) drive_download: bool,
    pub(super) range: bool,
    pub(super) etag: Option<String>,
    pub(super) referer: Option<String>,
    pub(super) cookie: Option<String>,
    pub(super) authorization: Option<String>,
    pub(super) ctx: Ctx,
}

impl Shared {
async fn fetch(&self, idx: usize) -> Result<(), FetchErr> {
    let (pos, end, total) = {
        let mut t = self.task.lock().unwrap();
        if !self.range {
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
    let mut req = self.ctx.client.get(&self.url);
    if let Some(r) = &self.referer {
        req = req.header(reqwest::header::REFERER, r);
    }
    if let Some(c) = &self.cookie {
        req = req.header(reqwest::header::COOKIE, c);
    }
    if let Some(value) = &self.authorization {
        req = req.header(reqwest::header::AUTHORIZATION, value);
    }
    if self.range {
        let r = if end == u64::MAX {
            format!("bytes={pos}-")
        } else {
            format!("bytes={pos}-{}", end - 1)
        };
        req = req.header(RANGE, r);
        if let Some(e) = &self.etag {
            req = req.header(IF_RANGE, e);
        }
    }
    let resp = tokio::select! {
        r = req.send() => r.map_err(|e| FetchErr::Retry(err_str(e)))?,
        _ = self.ctx.cancel.cancelled() => return Err(FetchErr::Cancelled),
    };
    let st = resp.status();
    google_drive::validate_file_response(&resp, self.drive_download).map_err(FetchErr::Fatal)?;
    if self.range && st.as_u16() != 206 {
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
    let mut response_remaining = if self.range {
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
            _ = self.ctx.cancel.cancelled() => return Err(FetchErr::Cancelled),
        };
        let chunk = match next {
            None => {
                let mut t = self.task.lock().unwrap();
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
        let wait = self.ctx.local.reserve(n).max(self.ctx.global.reserve(n));
        if !wait.is_zero() {
            tokio::select! {
                _ = tokio::time::sleep(wait) => {},
                _ = self.ctx.cancel.cancelled() => return Err(FetchErr::Cancelled),
            }
        }
        let mut t = self.task.lock().unwrap();
        let s = &mut t.segments[idx];
        let take = n.min(s.remaining()) as usize;
        if take > 0 {
            write_at(&self.file, &chunk[..take], s.pos)
                .map_err(|e| FetchErr::Fatal(format!("write failed: {e}")))?;
            s.pos += take as u64;
        }
        if s.done() || response_remaining == Some(0) {
            return Ok(());
        }
    }
}

pub(super) async fn worker(self: Arc<Self>) -> Result<(), String> {
    let sh = self;
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
            match sh.fetch(idx).await {
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

}
