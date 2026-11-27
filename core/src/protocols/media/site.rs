//! Built-in site resolvers (YouTube, X) feeding the native media transfer.
//!
//! The resolvers in `protocols::sites` return direct stream URLs; this file downloads
//! them, muxes split YouTube video and audio in process, converts YouTube audio to MP3
//! and names the payload after the title. Built-in resolvers take these hosts before
//! any external resolver module is consulted (see `protocols::handles`).
use super::{Transfer, mp3, mp4mux};
use crate::protocols::sites::x;
use crate::protocols::sites::youtube::{self, Choice, Stream};
use crate::task::DownloadKind;
use crate::util;
use reqwest::Url;
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncWriteExt;

/// Googlevideo throttles long single responses; ~10 MiB ranges keep full speed.
const CHUNK: u64 = 10 << 20;
const CHUNK_RETRIES: u32 = 3;
const TITLE_BYTES: usize = 150;
/// Size of the late-range probe at the end of a stream.
const PROBE: u64 = 64 << 10;

/// Site API endpoints and the media range size. Production uses [`Default`]; tests
/// point the endpoints at a local server.
pub(super) struct Sites {
    pub youtube: Url,
    pub x: Url,
    pub chunk: u64,
}

impl Default for Sites {
    fn default() -> Self {
        Self {
            youtube: Url::parse(youtube::DEFAULT_BASE).expect("static URL"),
            x: Url::parse(x::SYNDICATION).expect("static URL"),
            chunk: CHUNK,
        }
    }
}

async fn blocking<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|e| format!("media worker failed: {e}"))?
}

impl Transfer {
    /// Download a supported site item. `Ok(false)` means `url` is not a site item and
    /// the generic HTTP media path should take it.
    pub(super) async fn site(&self, url: &Url, sites: &Sites) -> Result<bool, String> {
        let audio = self.snapshot.kind == DownloadKind::Audio;
        if let Some(id) = youtube::video_id(url) {
            self.youtube(&id, audio, sites).await?;
            return Ok(true);
        }
        if audio {
            return Err(
                "unsupported link for audio: audio download supports YouTube links only".into(),
            );
        }
        if let Some(id) = x::status_id(url) {
            let video = x::resolve(&self.ctx.protocol_client, &sites.x, &id).await?;
            self.name(video.title.as_deref());
            self.task
                .lock()
                .unwrap()
                .append_log(format!("X post video resolved ({} bit/s)", video.bitrate));
            self.http(video.url).await?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Use a sanitized title as the payload file stem. Until publication, the task
    /// shows that name instead of the URL's last path segment (`1` for `/video/1`),
    /// unless the user chose a name.
    fn name(&self, title: Option<&str>) {
        let Some(title) = title else {
            return;
        };
        let mut stem = util::sanitize(title);
        if stem.len() > TITLE_BYTES {
            let mut end = TITLE_BYTES;
            while !stem.is_char_boundary(end) {
                end -= 1;
            }
            stem.truncate(end);
            stem = util::sanitize(&stem);
        }
        if stem.is_empty() || self.stem.set(stem.clone()).is_err() {
            return;
        }
        let mut task = self.task.lock().unwrap();
        if task
            .suggested_name
            .as_deref()
            .is_some_and(|name| !util::sanitize(name).is_empty())
        {
            return;
        }
        let extension = match self.snapshot.kind {
            DownloadKind::Audio => "mp3",
            _ => "mp4",
        };
        task.file_name = format!("{stem}.{extension}");
        task.category = crate::category::Category::from_name(&task.file_name);
    }

    async fn youtube(&self, id: &str, audio: bool, sites: &Sites) -> Result<(), String> {
        let mut resolver = youtube::Resolver::new(&sites.youtube, id)?;
        while let Some(resolved) = resolver.next(&self.ctx.protocol_client, audio).await {
            *self.user_agent.lock().unwrap() = Some(resolved.user_agent.clone());
            self.name(resolved.title.as_deref());
            if self.youtube_client(resolved, audio, sites.chunk).await? {
                return Ok(());
            }
        }
        Err(resolver.error())
    }

    /// Download one client's streams. `Ok(false)`: none is fully downloadable, so the
    /// next client should be tried.
    async fn youtube_client(
        &self,
        resolved: youtube::Resolved,
        audio: bool,
        chunk: u64,
    ) -> Result<bool, String> {
        let part = self.state.join("media.part");
        let client = resolved.client;
        let fallback = resolved.progressive;
        match resolved.choice {
            Choice::Split {
                video,
                audio: sound,
            } => {
                if !(self.probe(&video).await? && self.probe(&sound).await?) {
                    return self.limited(client, fallback, false, chunk).await;
                }
                self.log(&format!(
                    "YouTube ({client}): downloading H.264 video and AAC audio"
                ));
                self.expect(&[&video, &sound]);
                let video_file = self.state.join("video.mp4");
                let audio_file = self.state.join("audio.m4a");
                let done = self.ranged(&video, &video_file, 0, chunk).await?;
                self.ranged(&sound, &audio_file, done, chunk).await?;
                let (cancel, out) = (self.ctx.cancel.clone(), part.clone());
                let muxed =
                    blocking(move || mp4mux::mux(&video_file, &audio_file, &out, &cancel)).await;
                match muxed {
                    Ok(()) => self.complete(&part, "mp4").await?,
                    Err(e) if e == "mp4 mux: input is not fragmented" => {
                        let stream = fallback.ok_or(e)?;
                        self.log("YouTube: streams are not fragmented; using the muxed stream");
                        self.single(&stream, &part, chunk, "mp4").await?;
                    }
                    Err(e) => return Err(e),
                }
            }
            Choice::Audio(stream) => {
                if !self.probe(&stream).await? {
                    return self.limited(client, fallback, true, chunk).await;
                }
                self.log(&format!(
                    "YouTube ({client}): downloading AAC audio for MP3"
                ));
                self.mp3(&stream, chunk).await?;
            }
            Choice::Progressive(stream) => {
                if !self.probe(&stream).await? {
                    return Ok(false);
                }
                self.log(&format!("YouTube ({client}): downloading the muxed stream"));
                if audio {
                    self.mp3(&stream, chunk).await?;
                } else {
                    self.single(&stream, &part, chunk, "mp4").await?;
                }
            }
            Choice::Hls(url) => {
                self.log(&format!("YouTube ({client}): downloading the HLS stream"));
                self.http(url).await?;
            }
        }
        Ok(true)
    }

    /// Adaptive streams failed the late-range probe: use the muxed stream when it is
    /// fully downloadable, else let the next client try.
    async fn limited(
        &self,
        client: &str,
        fallback: Option<Stream>,
        audio: bool,
        chunk: u64,
    ) -> Result<bool, String> {
        let Some(stream) = fallback else {
            self.log(&format!(
                "YouTube ({client}): streams are limited; trying the next client"
            ));
            return Ok(false);
        };
        if !self.probe(&stream).await? {
            self.log(&format!(
                "YouTube ({client}): streams are limited; trying the next client"
            ));
            return Ok(false);
        }
        self.log("YouTube: HD streams are limited for this client; using the 360p muxed stream");
        if audio {
            self.mp3(&stream, chunk).await?;
        } else {
            self.single(&stream, &self.state.join("media.part"), chunk, "mp4")
                .await?;
        }
        Ok(true)
    }

    /// Download an mp4/m4a with an AAC track and publish it converted to MP3.
    async fn mp3(&self, stream: &Stream, chunk: u64) -> Result<(), String> {
        self.expect(&[stream]);
        let input = self.state.join("audio.m4a");
        self.ranged(stream, &input, 0, chunk).await?;
        self.log("Converting audio to MP3");
        let part = self.state.join("media.part");
        let (cancel, out) = (self.ctx.cancel.clone(), part.clone());
        blocking(move || mp3::to_mp3(&input, &out, &cancel, &|_| {})).await?;
        self.complete(&part, "mp3").await
    }

    /// Whether the end of `stream` is served. Googlevideo answers 403 past about the
    /// first minute of adaptive media when the client lacks a proof-of-origin token.
    async fn probe(&self, stream: &Stream) -> Result<bool, String> {
        let Some(length) = stream.length.filter(|n| *n > 0) else {
            return Ok(true);
        };
        let offset = length.saturating_sub(PROBE);
        match self
            .get(&stream.url, Some((offset, length - offset)), false)
            .await
        {
            Ok(_) => Ok(true),
            Err(e) if e.starts_with("media HTTP 4") => Ok(false),
            // A transient failure says nothing about the gate; the ranged download
            // retries and reports real errors.
            Err(_) => Ok(true),
        }
    }

    fn log(&self, line: &str) {
        self.task.lock().unwrap().append_log(line);
    }

    /// Show the summed stream sizes as the download total when all are known.
    fn expect(&self, streams: &[&Stream]) {
        let total: Option<u64> = streams.iter().map(|s| s.length).sum();
        let mut task = self.task.lock().unwrap();
        task.total = total;
        if let (Some(total), Some(segment)) = (total, task.segments.first_mut()) {
            segment.pos = 0;
            segment.end = total;
        }
    }

    async fn single(
        &self,
        stream: &Stream,
        part: &Path,
        chunk: u64,
        ext: &str,
    ) -> Result<(), String> {
        self.expect(&[stream]);
        self.ranged(stream, part, 0, chunk).await?;
        self.complete(part, ext).await
    }

    /// Publish a finished `state/media.part` through [`Transfer::finish`].
    async fn complete(&self, part: &Path, ext: &str) -> Result<(), String> {
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .open(part)
            .await
            .map_err(|e| format!("cannot open staged media: {e}"))?;
        self.finish(file, ext).await
    }

    /// Download `stream` to `path` in `chunk`-sized Range requests, each retried up to
    /// three times. Progress is reported as `done` plus the bytes written; returns the
    /// new cumulative figure.
    async fn ranged(
        &self,
        stream: &Stream,
        path: &Path,
        done: u64,
        chunk: u64,
    ) -> Result<u64, String> {
        let mut file = tokio::fs::File::create(path)
            .await
            .map_err(|e| format!("cannot create staged media: {e}"))?;
        let Some(length) = stream.length.filter(|n| *n > 0) else {
            // No advertised size: one plain request.
            let mut response = self.get(&stream.url, None, false).await?;
            let mut written = 0u64;
            while let Some(bytes) = response
                .chunk()
                .await
                .map_err(|e| format!("media body: {}", super::net(e)))?
            {
                self.limit(bytes.len()).await;
                file.write_all(&bytes).await.map_err(|e| e.to_string())?;
                written += bytes.len() as u64;
                self.progress(done + written);
            }
            return close(file, done + written).await;
        };
        let chunk = chunk.max(1);
        let mut offset = 0u64;
        while offset < length {
            let size = chunk.min(length - offset);
            let mut attempt = 0;
            let bytes = loop {
                match self.range(&stream.url, offset, size, done + offset).await {
                    Ok(bytes) => break bytes,
                    Err(e) if attempt >= CHUNK_RETRIES || e.starts_with("media HTTP 4") => {
                        return Err(e);
                    }
                    Err(_) => {
                        attempt += 1;
                        tokio::time::sleep(Duration::from_millis(500 * u64::from(attempt))).await;
                    }
                }
            };
            file.write_all(&bytes).await.map_err(|e| e.to_string())?;
            offset += size;
        }
        close(file, done + length).await
    }

    /// One validated Range response, buffered so a failed attempt leaves no bytes.
    async fn range(&self, url: &Url, offset: u64, size: u64, base: u64) -> Result<Vec<u8>, String> {
        let mut response = self.get(url, Some((offset, size)), false).await?;
        let mut bytes = Vec::with_capacity(size as usize);
        while let Some(piece) = response
            .chunk()
            .await
            .map_err(|e| format!("media body: {}", super::net(e)))?
        {
            self.limit(piece.len()).await;
            if bytes.len() as u64 + piece.len() as u64 > size {
                return Err("media range response is longer than requested".into());
            }
            bytes.extend_from_slice(&piece);
            self.progress(base + bytes.len() as u64);
        }
        if bytes.len() as u64 != size {
            return Err("media range response ended early".into());
        }
        Ok(bytes)
    }
}

async fn close(mut file: tokio::fs::File, total: u64) -> Result<u64, String> {
    file.flush().await.map_err(|e| e.to_string())?;
    file.sync_all().await.map_err(|e| e.to_string())?;
    Ok(total)
}

#[cfg(test)]
mod tests;
