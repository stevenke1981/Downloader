//! Native HLS/direct media, RTSP H.264 Annex B or G.711 WAV, and MS-WMSP-over-HTTP.
//! Interrupted streams restart. Complete files enter payload; partial/control
//! files stay in state. No subprocess, C media engine, or JS runtime is used.
mod hls;
mod mmsh;
mod parse;
mod rtsp;

use crate::engine::{Ctx, Outcome};
use crate::task::{Seg, Task};
use futures_util::StreamExt;
use parse::{discover, http_url, parse_playlist};
use reqwest::{Response, Url};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;

#[derive(Clone, PartialEq, Eq)]
struct Resource {
    url: Url,
    range: Option<(u64, u64)>,
}
#[derive(Clone)]
struct Key {
    url: Url,
    iv: Option<[u8; 16]>,
}
struct Segment {
    resource: Resource,
    key: Option<Key>,
    map: Option<Resource>,
}
struct MediaPlaylist {
    segments: Vec<Segment>,
    sequence: u64,
    end: bool,
    target_duration: f64,
}
enum Playlist {
    Master(Url),
    Media(MediaPlaylist),
}

const DOCUMENT_LIMIT: usize = 4 << 20;
const SEGMENT_LIMIT: usize = 64 << 20;
const SEGMENT_COUNT_LIMIT: usize = 100_000;

struct Transfer {
    task: Arc<Mutex<Task>>,
    snapshot: Task,
    ctx: Ctx,
    state: PathBuf,
    payload: PathBuf,
    origin: Url,
}

impl Transfer {
    async fn limit(&self, bytes: usize) {
        let wait = self
            .ctx
            .local
            .reserve(bytes as u64)
            .max(self.ctx.global.reserve(bytes as u64));
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    fn progress(&self, bytes: u64) {
        self.task.lock().unwrap().segments[0].pos = bytes;
    }

    async fn get(
        &self,
        url: &Url,
        range: Option<(u64, u64)>,
        mms: bool,
    ) -> Result<Response, String> {
        let mut target = url.clone();
        for _ in 0..=10 {
            let mut request = self.ctx.protocol_client.get(target.clone());
            if self.origin.origin() == target.origin() {
                if let Some(cookie) = &self.snapshot.cookie {
                    request = request.header(reqwest::header::COOKIE, cookie);
                }
                if let Some(auth) = &self.snapshot.authorization {
                    request = request.header(reqwest::header::AUTHORIZATION, auth);
                }
            }
            if let Some(referer) = &self.snapshot.referer {
                request = request.header(reqwest::header::REFERER, referer);
            }
            if let Some((offset, length)) = range {
                request = request.header(
                    reqwest::header::RANGE,
                    format!("bytes={offset}-{}", offset + length - 1),
                );
            }
            if mms {
                request = request.header(reqwest::header::USER_AGENT, "NSPlayer/9.0.0.2980")
                    .header(reqwest::header::ACCEPT, "*/*")
                    .header(reqwest::header::PRAGMA, "no-cache,rate=1.000,stream-time=0,stream-offset=0:0,request-context=1,max-duration=0,xPlayStrm=1");
            }
            let response = request
                .send()
                .await
                .map_err(|e| format!("media request failed: {e}"))?;
            if response.status().is_redirection() {
                let location = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or("media redirect has no valid Location")?;
                target = http_url(&target, location)?;
                continue;
            }
            if !response.status().is_success() {
                return Err(format!("media HTTP {}", response.status()));
            }
            if let Some((offset, length)) = range {
                let expected = format!("bytes {offset}-{}/", offset + length - 1);
                let value = response
                    .headers()
                    .get(reqwest::header::CONTENT_RANGE)
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default();
                let total = value.strip_prefix(&expected).and_then(|value| {
                    if value == "*" {
                        Some(None)
                    } else {
                        value.parse::<u64>().ok().map(Some)
                    }
                });
                if response.status() != reqwest::StatusCode::PARTIAL_CONTENT
                    || total.is_none()
                    || total.flatten().is_some_and(|total| total < offset + length)
                    || response.content_length().is_some_and(|n| n != length)
                {
                    return Err("media byte range was not honored by the server".into());
                }
            } else if response.status() == reqwest::StatusCode::PARTIAL_CONTENT {
                return Err("unexpected partial media response".into());
            }
            return Ok(response);
        }
        Err("media exceeds ten redirects".into())
    }

    async fn bytes(&self, mut response: Response, limit: usize) -> Result<Vec<u8>, String> {
        if response.content_length().is_some_and(|n| n > limit as u64) {
            return Err(format!("media resource exceeds the {limit}-byte limit"));
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|e| format!("media body: {e}"))?
        {
            self.limit(chunk.len()).await;
            if bytes.len().saturating_add(chunk.len()) > limit {
                return Err(format!("media resource exceeds the {limit}-byte limit"));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(bytes)
    }

    async fn finish(&self, mut file: tokio::fs::File, extension: &str) -> Result<(), String> {
        file.flush().await.map_err(|e| e.to_string())?;
        file.sync_all().await.map_err(|e| e.to_string())?;
        let length = file.metadata().await.map_err(|e| e.to_string())?.len();
        drop(file);
        if length == 0 {
            return Err("media server produced no complete media data".into());
        }
        if self.ctx.cancel.is_cancelled() {
            return Err("media completion cancelled".into());
        }
        // A synchronous atomic rename cannot continue as a detached filesystem
        // job after cancellation drops the async transfer future.
        std::fs::rename(
            self.state.join("media.part"),
            self.payload.join(format!("media.{extension}")),
        )
        .map_err(|e| format!("cannot complete staged media: {e}"))?;
        let mut task = self.task.lock().unwrap();
        task.total = Some(length);
        task.segments = vec![Seg {
            start: 0,
            pos: length,
            end: length,
            busy: false,
        }];
        Ok(())
    }

    async fn http(&self, mut target: Url) -> Result<(), String> {
        for _ in 0..5 {
            let mut response = self.get(&target, None, false).await?;
            let final_url = response.url().clone();
            if crate::google_drive::is_sign_in(&final_url) {
                return Err(crate::google_drive::SIGN_IN.into());
            }
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let total = response.content_length();
            let mut prefix = Vec::new();
            while prefix.len() < 512 {
                let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? else {
                    break;
                };
                self.limit(chunk.len()).await;
                prefix.extend_from_slice(&chunk);
                if prefix.len() > DOCUMENT_LIMIT {
                    return Err("initial media response chunk exceeds 4 MiB".into());
                }
            }
            let text_prefix = String::from_utf8_lossy(&prefix);
            let text_prefix = text_prefix.trim_start_matches('\u{feff}').trim_start();
            let html = content_type.contains("text/html")
                || content_type.contains("application/xhtml")
                || text_prefix.starts_with('<');
            let playlist = content_type.contains("mpegurl") || text_prefix.starts_with("#EXTM3U");
            if html || playlist {
                if total.is_some_and(|n| n > DOCUMENT_LIMIT as u64) {
                    return Err("media document exceeds 4 MiB".into());
                }
                while let Some(chunk) = response.chunk().await.map_err(|e| e.to_string())? {
                    self.limit(chunk.len()).await;
                    if prefix.len().saturating_add(chunk.len()) > DOCUMENT_LIMIT {
                        return Err("media document exceeds 4 MiB".into());
                    }
                    prefix.extend_from_slice(&chunk);
                }
                let text =
                    std::str::from_utf8(&prefix).map_err(|_| "media document is not UTF-8")?;
                if playlist {
                    return self.hls(final_url, text.to_owned()).await;
                }
                if crate::google_drive::is_drive(&final_url) {
                    return Err(crate::google_drive::UNAVAILABLE.into());
                }
                target = discover(text, &final_url)?;
                self.task
                    .lock()
                    .unwrap()
                    .append_log("Native webpage media URL discovered");
                continue;
            }
            let extension =
                media_extension(&prefix).ok_or("response is not a recognized media container")?;
            {
                let mut task = self.task.lock().unwrap();
                task.total = total;
                task.final_url = Some(final_url.to_string());
            }
            let mut file = tokio::fs::File::create(self.state.join("media.part"))
                .await
                .map_err(|e| e.to_string())?;
            file.write_all(&prefix).await.map_err(|e| e.to_string())?;
            let mut written = prefix.len() as u64;
            self.progress(written);
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|e| format!("media body: {e}"))?
            {
                self.limit(chunk.len()).await;
                file.write_all(&chunk).await.map_err(|e| e.to_string())?;
                written += chunk.len() as u64;
                self.progress(written);
            }
            if total.is_some_and(|total| total != written) {
                return Err("media response ended before its Content-Length".into());
            }
            return self.finish(file, extension).await;
        }
        Err("media discovery exceeds five page/playlist hops".into())
    }
}

pub(crate) async fn download(
    task: Arc<Mutex<Task>>,
    ctx: Ctx,
    stage: &Path,
) -> Result<Outcome, String> {
    let snapshot = task.lock().unwrap().clone();
    let mut url = Url::parse(&snapshot.url).map_err(|_| "invalid media URL")?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err("media credentials must use task site credentials, not URL userinfo".into());
    }
    let mms = matches!(url.scheme(), "mms" | "mmsh");
    if url.scheme() == "mmst" {
        return Err(
            "MMST (MMS over TCP) is not supported; use an MMS-over-HTTP mms:// or mmsh:// endpoint"
                .into(),
        );
    }
    if mms {
        url = Url::parse(&format!(
            "http://{}",
            url.as_str().split_once("://").ok_or("invalid MMSH URL")?.1
        ))
        .map_err(|_| "invalid MMSH URL")?;
    }
    if !matches!(url.scheme(), "http" | "https" | "rtsp") {
        return Err("unsupported native media URL scheme".into());
    }
    let transfer = Transfer {
        task: task.clone(),
        snapshot,
        ctx: ctx.clone(),
        state: stage.join("state"),
        payload: stage.join("payload"),
        origin: url.clone(),
    };
    {
        let mut task = task.lock().unwrap();
        task.resumable = false;
        task.total = None;
        task.segments = vec![Seg::new(0, u64::MAX)];
        task.error = None;
    }
    let future = async {
        tokio::fs::create_dir_all(&transfer.state)
            .await
            .map_err(|e| e.to_string())?;
        tokio::fs::create_dir_all(&transfer.payload)
            .await
            .map_err(|e| e.to_string())?;
        if mms {
            transfer.mmsh(url).await?;
        } else if url.scheme() == "rtsp" {
            transfer.rtsp(url).await?;
        } else {
            transfer.http(url).await?;
        }
        Ok(Outcome::Completed)
    };
    // Dropping the future interrupts DNS/connect, body, polling and limiter waits
    // and closes all owned responses/sockets. No detached download task survives.
    tokio::select! { biased;
        _ = ctx.cancel.cancelled() => { task.lock().unwrap().append_log("Native media stopped; next capture restarts safely"); Ok(Outcome::Cancelled) }
        result = future => result,
    }
}

fn media_extension(bytes: &[u8]) -> Option<&'static str> {
    // Packed-audio HLS carries an ID3 timestamp before its ADTS frames.
    let audio = if bytes.starts_with(b"ID3")
        && bytes.len() >= 10
        && bytes[6..10].iter().all(|byte| byte & 0x80 == 0)
    {
        let size = bytes[6..10]
            .iter()
            .fold(0usize, |size, byte| (size << 7) | *byte as usize);
        let end = 10 + size + if bytes[5] & 0x10 != 0 { 10 } else { 0 };
        bytes
            .get(end..)
            .filter(|bytes| !bytes.is_empty())
            .unwrap_or(bytes)
    } else {
        bytes
    };
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        Some("mp4")
    } else if bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        Some("mkv")
    } else if bytes.starts_with(b"OggS") {
        Some("ogg")
    } else if bytes.starts_with(b"fLaC") {
        Some("flac")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WAVE" {
        Some("wav")
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"AVI " {
        Some("avi")
    } else if bytes.len() >= 188 && bytes[0] == 0x47 && (bytes.len() < 376 || bytes[188] == 0x47) {
        Some("ts")
    } else if audio.len() >= 7 && audio[0] == 0xff && audio[1] & 0xf6 == 0xf0 {
        Some("aac")
    } else if bytes.starts_with(b"ID3")
        || (audio.len() >= 4 && audio[0] == 0xff && audio[1] & 0xe0 == 0xe0 && audio[1] & 6 != 0)
    {
        Some("mp3")
    } else if bytes.starts_with(&mmsh::ASF_HEADER) {
        Some("asf")
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
