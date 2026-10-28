//! Native HLS/direct media, RTSP H.264 Annex B, and MS-WMSP-over-HTTP.
//! Interrupted streams restart. Complete files enter payload; partial/control
//! files stay in state. No subprocess, C media engine, or JS runtime is used.
use crate::engine::{Ctx, Outcome};
use crate::task::{Seg, Task};
use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use base64::Engine;
use futures_util::StreamExt;
use reqwest::{Response, Url};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncSeekExt, AsyncWriteExt};

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
    async fn rtsp(&self, url: Url) -> Result<(), String> {
        use retina::client::{
            Credentials, PlayOptions, Session, SessionOptions, SetupOptions, TcpTransportOptions,
            TeardownPolicy, Transport,
        };
        use retina::codec::{CodecItem, FrameFormat};
        let credentials = self
            .snapshot
            .authorization
            .as_deref()
            .map(|value| {
                let bytes = value
                    .strip_prefix("Basic ")
                    .and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok())
                    .and_then(|bytes| String::from_utf8(bytes).ok())
                    .ok_or("RTSP credentials must use Basic username/password")?;
                let (username, password) = bytes
                    .split_once(':')
                    .ok_or("invalid RTSP Basic credentials")?;
                Ok::<_, String>(Credentials {
                    username: username.into(),
                    password: password.into(),
                })
            })
            .transpose()?;
        // Closing TCP owns the lifecycle; do not leave detached teardown retry
        // tasks alive after the download's cancellation future has been dropped.
        let options = SessionOptions::default()
            .creds(credentials)
            .teardown(TeardownPolicy::Never)
            .user_agent(concat!("Downloader/", env!("CARGO_PKG_VERSION")).into());
        let mut session =
            tokio::time::timeout(Duration::from_secs(30), Session::describe(url, options))
                .await
                .map_err(|_| "RTSP DESCRIBE timed out")?
                .map_err(|e| format!("RTSP DESCRIBE: {e}"))?;
        let stream = session
            .streams()
            .iter()
            .position(|stream| {
                stream.media() == "video" && stream.encoding_name().eq_ignore_ascii_case("h264")
            })
            .ok_or(
                "native RTSP capture requires an H.264 video track; audio/H.265 are not recorded",
            )?;
        tokio::time::timeout(
            Duration::from_secs(30),
            session.setup(
                stream,
                SetupOptions::default()
                    .transport(Transport::Tcp(TcpTransportOptions::default()))
                    .frame_format(FrameFormat::SIMPLE),
            ),
        )
        .await
        .map_err(|_| "RTSP SETUP timed out")?
        .map_err(|e| format!("RTSP SETUP: {e}"))?;
        let session = tokio::time::timeout(
            Duration::from_secs(30),
            session.play(PlayOptions::default()),
        )
        .await
        .map_err(|_| "RTSP PLAY timed out")?
        .map_err(|e| format!("RTSP PLAY: {e}"))?;
        let mut stream = session
            .demuxed()
            .map_err(|e| format!("RTSP depacketizer: {e}"))?;
        let mut file = tokio::fs::File::create(self.state.join("media.part"))
            .await
            .map_err(|e| e.to_string())?;
        let mut written = 0u64;
        let mut ready = false;
        let mut ended = false;
        self.task.lock().unwrap().append_log("Native RTSP/TCP H.264 Annex B capture; one video track, no audio; completion requires RTCP BYE");
        while let Some(item) = tokio::time::timeout(Duration::from_secs(60), stream.next())
            .await
            .map_err(|_| "RTSP media timed out")?
        {
            match item.map_err(|e| format!("RTSP media: {e}"))? {
                CodecItem::VideoFrame(frame) => {
                    self.limit(frame.data().len().saturating_add(16)).await;
                    if frame.loss() != 0 {
                        return Err("RTSP RTP packet loss; refusing an incomplete capture".into());
                    }
                    if !ready {
                        if !frame.is_random_access_point() {
                            continue;
                        }
                        if stream.streams()[frame.stream_id()].parameters().is_none() {
                            return Err("RTSP keyframe has no SPS/PPS; cannot create a self-describing H.264 stream".into());
                        }
                        ready = true;
                    }
                    file.write_all(frame.data())
                        .await
                        .map_err(|e| e.to_string())?;
                    written += frame.data().len() as u64;
                    self.progress(written);
                }
                CodecItem::Rtcp(packet) => {
                    self.limit(packet.raw().len().saturating_add(4)).await;
                    if packet.pkts().any(|packet| packet.payload_type() == 203) {
                        ended = true;
                        break;
                    }
                }
                _ => {}
            }
        }
        drop(stream);
        if !ended {
            return Err(
                "RTSP connection ended without an RTCP BYE; capture may be incomplete".into(),
            );
        }
        if !ready {
            return Err("RTSP ended before an H.264 keyframe".into());
        }
        self.finish(file, "h264").await
    }

    async fn mmsh(&self, url: Url) -> Result<(), String> {
        let response = self.get(&url, None, true).await?;
        let mut reader = BodyReader {
            response,
            pending: Vec::new(),
            offset: 0,
            transfer: self,
        };
        let mut header = Vec::new();
        let mut header_location = 0u32;
        let mut properties = None;
        let mut packets = 0u64;
        let mut previous_location = None;
        let mut previous_flags: Option<u8> = None;
        let mut file = tokio::fs::File::create(self.state.join("media.part"))
            .await
            .map_err(|e| e.to_string())?;
        let mut written = 0u64;
        self.task
            .lock()
            .unwrap()
            .append_log("Native MS-WMSP HTTP Play and ASF packet decapsulation");
        loop {
            let framing = reader
                .take(4)
                .await?
                .ok_or("MMSH ended without an end-of-stream packet")?;
            if framing[0] & 0x7f != b'$' {
                return Err("invalid MMSH framing marker".into());
            }
            let length = u16::from_le_bytes([framing[2], framing[3]]) as usize;
            let body = reader.take(length).await?.ok_or("truncated MMSH packet")?;
            match framing[1] {
                b'H' | b'D' | b'M' => {
                    if body.len() < 8
                        || u16::from_le_bytes([body[6], body[7]]) as usize != body.len()
                    {
                        return Err("invalid MMSH data packet size".into());
                    }
                    let location = u32::from_le_bytes(body[..4].try_into().unwrap());
                    let flags = body[5];
                    let payload = &body[8..];
                    if framing[1] == b'M' {
                        continue;
                    }
                    if framing[1] == b'H' {
                        if properties.is_some() || packets > 0 || location != header_location {
                            return Err("unexpected or out-of-order MMSH ASF header".into());
                        }
                        header_location = header_location
                            .checked_add(1)
                            .ok_or("MMSH header sequence overflow")?;
                        if header.len().saturating_add(payload.len()) > DOCUMENT_LIMIT {
                            return Err("MMSH ASF header exceeds 4 MiB".into());
                        }
                        header.extend_from_slice(payload);
                        if flags & 8 != 0 || flags == 0 {
                            properties = Some(asf_header(&header)?);
                            file.write_all(&header).await.map_err(|e| e.to_string())?;
                            written = header.len() as u64;
                            self.progress(written);
                        }
                    } else {
                        let (_, _, packet_size) =
                            properties.ok_or("MMSH data arrived before a complete ASF header")?;
                        if previous_location
                            .is_some_and(|prior: u32| prior.checked_add(1) != Some(location))
                            || previous_flags.is_some_and(|prior| prior.wrapping_add(1) != flags)
                        {
                            return Err("MMSH skipped or reordered an ASF data packet".into());
                        }
                        previous_location = Some(location);
                        previous_flags = Some(flags);
                        let packet = asf_packet(payload, packet_size)?;
                        file.write_all(&packet).await.map_err(|e| e.to_string())?;
                        packets += 1;
                        written += packet.len() as u64;
                        self.progress(written);
                    }
                }
                b'E' => {
                    if body.len() != 4 {
                        return Err("invalid MMSH end packet length".into());
                    }
                    let reason = u32::from_le_bytes(body[..4].try_into().unwrap());
                    if reason == 1 {
                        return Err(
                            "MMSH server-side multi-entry playlists are not supported".into()
                        );
                    }
                    if reason != 0 {
                        return Err(format!("MMSH server ended with HRESULT 0x{reason:08x}"));
                    }
                    break;
                }
                b'C' => {
                    return Err(
                        "MMSH stream/header changes are not supported in one ASF output".into(),
                    );
                }
                _ => return Err("unsupported MMSH packet type".into()),
            }
        }
        if packets == 0 {
            return Err("MMSH server sent no ASF media packets".into());
        }
        let (header_size, file_properties, _) = properties.ok_or("MMSH has no ASF header")?;
        // Rewrite unknown streaming lengths to match the completed recording.
        header[file_properties + 40..file_properties + 48].copy_from_slice(&written.to_le_bytes());
        header[file_properties + 56..file_properties + 64].copy_from_slice(&packets.to_le_bytes());
        let flags = u32::from_le_bytes(
            header[file_properties + 88..file_properties + 92]
                .try_into()
                .unwrap(),
        ) & !3;
        header[file_properties + 88..file_properties + 92].copy_from_slice(&flags.to_le_bytes());
        header[header_size + 16..header_size + 24]
            .copy_from_slice(&(written - header_size as u64).to_le_bytes());
        header[header_size + 40..header_size + 48].copy_from_slice(&packets.to_le_bytes());
        file.seek(std::io::SeekFrom::Start(0))
            .await
            .map_err(|e| e.to_string())?;
        file.write_all(&header).await.map_err(|e| e.to_string())?;
        self.finish(file, "asf").await
    }

    async fn hls(&self, mut url: Url, mut text: String) -> Result<(), String> {
        let mut media = None;
        for _ in 0..5 {
            match parse_playlist(&text, &url)? {
                Playlist::Master(next) => {
                    let response = self.get(&next, None, false).await?;
                    url = response.url().clone();
                    text = String::from_utf8(self.bytes(response, DOCUMENT_LIMIT).await?)
                        .map_err(|_| "HLS playlist is not UTF-8")?;
                }
                Playlist::Media(playlist) => {
                    media = Some(playlist);
                    break;
                }
            }
        }
        let mut playlist = media.ok_or("HLS master playlist nesting exceeds five levels")?;
        self.task.lock().unwrap().append_log(
            "Native HLS transfer (AES-128 identity supported; interrupted captures restart)",
        );
        let mut file = tokio::fs::File::create(self.state.join("media.part"))
            .await
            .map_err(|e| e.to_string())?;
        let mut written = 0u64;
        let mut next_sequence = playlist.sequence;
        let mut count = 0usize;
        let mut map: Option<Resource> = None;
        let mut extension = None;
        let mut keys: HashMap<String, [u8; 16]> = HashMap::new();
        loop {
            tokio::fs::write(self.state.join("playlist.m3u8"), text.as_bytes())
                .await
                .map_err(|e| e.to_string())?;
            if playlist.sequence > next_sequence {
                return Err(
                    "HLS live window skipped a segment; capture would be incomplete".into(),
                );
            }
            for (index, segment) in playlist.segments.iter().enumerate() {
                let sequence = playlist
                    .sequence
                    .checked_add(index as u64)
                    .ok_or("HLS sequence overflow")?;
                if sequence < next_sequence {
                    continue;
                }
                count += 1;
                if count > SEGMENT_COUNT_LIMIT {
                    return Err("HLS exceeds 100000 segments".into());
                }
                if segment.map != map {
                    if map.is_some() || written > 0 {
                        return Err(
                            "HLS initialization map changes are not supported in one output".into(),
                        );
                    }
                    if let Some(init) = &segment.map {
                        let response = self.get(&init.url, init.range, false).await?;
                        let bytes = self.bytes(response, SEGMENT_LIMIT).await?;
                        if bytes.len() < 12 || &bytes[4..8] != b"ftyp" {
                            return Err(
                                "HLS initialization segment is not an MP4 ftyp container".into()
                            );
                        }
                        if init
                            .range
                            .is_some_and(|(_, length)| bytes.len() as u64 != length)
                        {
                            return Err("HLS MAP range body has the wrong length".into());
                        }
                        file.write_all(&bytes).await.map_err(|e| e.to_string())?;
                        written += bytes.len() as u64;
                        map = segment.map.clone();
                        extension = Some("mp4");
                    }
                }
                let response = self
                    .get(&segment.resource.url, segment.resource.range, false)
                    .await?;
                let mut bytes = self.bytes(response, SEGMENT_LIMIT).await?;
                if segment
                    .resource
                    .range
                    .is_some_and(|(_, length)| bytes.len() as u64 != length)
                {
                    return Err("HLS byte range body has the wrong length".into());
                }
                if let Some(key) = &segment.key {
                    let key_bytes = if let Some(bytes) = keys.get(key.url.as_str()) {
                        *bytes
                    } else {
                        let response = self.get(&key.url, None, false).await?;
                        let bytes: [u8; 16] = self
                            .bytes(response, 16)
                            .await?
                            .try_into()
                            .map_err(|_| "HLS AES-128 key must be exactly 16 bytes")?;
                        if keys.len() >= 256 {
                            keys.clear();
                        }
                        keys.insert(key.url.to_string(), bytes);
                        bytes
                    };
                    let iv = key.iv.unwrap_or_else(|| {
                        let mut iv = [0; 16];
                        iv[8..].copy_from_slice(&sequence.to_be_bytes());
                        iv
                    });
                    bytes = cbc::Decryptor::<aes::Aes128>::new(&key_bytes.into(), &iv.into())
                        .decrypt_padded_vec_mut::<Pkcs7>(&bytes)
                        .map_err(|_| "invalid HLS AES-128 ciphertext or padding")?;
                }
                if map.is_some() {
                    if bytes.len() < 8
                        || !matches!(&bytes[4..8], b"styp" | b"moof" | b"sidx" | b"emsg")
                    {
                        return Err("HLS MP4 segment has no fragment box".into());
                    }
                } else {
                    let actual = media_extension(&bytes).filter(|ext| matches!(*ext, "ts" | "aac" | "mp3"))
                        .ok_or("HLS segment is not MPEG-TS/ADTS/MP3; fragmented MP4 requires EXT-X-MAP")?;
                    if extension.is_some_and(|prior| prior != actual) {
                        return Err("HLS segment container changed".into());
                    }
                    extension = Some(actual);
                }
                file.write_all(&bytes).await.map_err(|e| e.to_string())?;
                written += bytes.len() as u64;
                self.progress(written);
                next_sequence = sequence.checked_add(1).ok_or("HLS sequence overflow")?;
            }
            if playlist.end {
                break;
            }
            tokio::time::sleep(Duration::from_secs_f64(
                (playlist.target_duration / 2.0).clamp(0.5, 30.0),
            ))
            .await;
            let response = self.get(&url, None, false).await?;
            url = response.url().clone();
            text = String::from_utf8(self.bytes(response, DOCUMENT_LIMIT).await?)
                .map_err(|_| "HLS playlist is not UTF-8")?;
            playlist = match parse_playlist(&text, &url)? {
                Playlist::Media(playlist) => playlist,
                Playlist::Master(_) => {
                    return Err("HLS media playlist changed into a master".into());
                }
            };
        }
        if count == 0 {
            return Err("HLS playlist contains no media segments".into());
        }
        self.finish(file, extension.ok_or("HLS has no media representation")?)
            .await
    }

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
                    .header(reqwest::header::PRAGMA, "no-cache,rate=1.000,stream-time=0,stream-offset=0:0,request-context=1,max-duration=0,xPlayStrm=1")
                    .header("Supported", "com.microsoft.wm.srvppair, com.microsoft.wm.sswitch");
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

struct BodyReader<'a> {
    response: Response,
    pending: Vec<u8>,
    offset: usize,
    transfer: &'a Transfer,
}
impl BodyReader<'_> {
    async fn take(&mut self, length: usize) -> Result<Option<Vec<u8>>, String> {
        while self.pending.len() - self.offset < length {
            if self.offset > 0 {
                self.pending.drain(..self.offset);
                self.offset = 0;
            }
            let Some(chunk) = self
                .response
                .chunk()
                .await
                .map_err(|e| format!("MMSH body: {e}"))?
            else {
                return if self.pending.is_empty() {
                    Ok(None)
                } else {
                    Err("truncated MMSH packet".into())
                };
            };
            self.transfer.limit(chunk.len()).await;
            self.pending.extend_from_slice(&chunk);
        }
        let bytes = self.pending[self.offset..self.offset + length].to_vec();
        self.offset += length;
        Ok(Some(bytes))
    }
}

const ASF_HEADER: [u8; 16] = [
    0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa, 0x00, 0x62, 0xce, 0x6c,
];
const ASF_DATA: [u8; 16] = [
    0x36, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9, 0x00, 0xaa, 0x00, 0x62, 0xce, 0x6c,
];
const ASF_FILE_PROPERTIES: [u8; 16] = [
    0xa1, 0xdc, 0xab, 0x8c, 0x47, 0xa9, 0xcf, 0x11, 0x8e, 0xe4, 0x00, 0xc0, 0x0c, 0x20, 0x53, 0x65,
];
const ASF_STREAM_PROPERTIES: [u8; 16] = [
    0x91, 0x07, 0xdc, 0xb7, 0xb7, 0xa9, 0xcf, 0x11, 0x8e, 0xe6, 0x00, 0xc0, 0x0c, 0x20, 0x53, 0x65,
];

fn asf_header(bytes: &[u8]) -> Result<(usize, usize, usize), String> {
    if bytes.len() < 80 || !bytes.starts_with(&ASF_HEADER) {
        return Err("MMSH header is not ASF".into());
    }
    let length = usize::try_from(u64::from_le_bytes(bytes[16..24].try_into().unwrap()))
        .map_err(|_| "ASF header size overflow")?;
    if length < 30
        || length.checked_add(50) != Some(bytes.len())
        || bytes[length..length + 16] != ASF_DATA
    {
        return Err("invalid ASF Header/Data Object lengths".into());
    }
    for guid in [
        "fbb3112223bdd211b4b700a0c955fc6e",
        "14e68a292226174cb935dae07ee9289c",
        "338505435f32964aaa67a6b29b3b06b4",
    ] {
        let guid = hex::decode(guid).unwrap();
        if bytes[..length].windows(16).any(|window| window == guid) {
            return Err("DRM-protected ASF is not supported".into());
        }
    }
    let count = u32::from_le_bytes(bytes[24..28].try_into().unwrap());
    let mut position = 30usize;
    let mut properties = None;
    let mut stream = false;
    for _ in 0..count {
        if position + 24 > length {
            return Err("truncated ASF subobject".into());
        }
        let size = usize::try_from(u64::from_le_bytes(
            bytes[position + 16..position + 24].try_into().unwrap(),
        ))
        .map_err(|_| "ASF object size overflow")?;
        let end = position
            .checked_add(size)
            .filter(|end| *end <= length && size >= 24)
            .ok_or("invalid ASF subobject size")?;
        if bytes[position..position + 16] == ASF_FILE_PROPERTIES {
            if size < 104 || properties.is_some() {
                return Err("invalid ASF File Properties Object".into());
            }
            let minimum =
                u32::from_le_bytes(bytes[position + 92..position + 96].try_into().unwrap());
            let maximum =
                u32::from_le_bytes(bytes[position + 96..position + 100].try_into().unwrap());
            if minimum < 16 || minimum != maximum || maximum > (1 << 20) {
                return Err("MMSH supports fixed-size ASF packets up to 1 MiB".into());
            }
            properties = Some((position, maximum as usize));
        }
        if bytes[position..position + 16] == ASF_STREAM_PROPERTIES {
            if size < 78 {
                return Err("invalid ASF Stream Properties Object".into());
            }
            if bytes[position + 73] & 0x80 != 0 {
                return Err("encrypted ASF stream is not supported".into());
            }
            stream = true;
        }
        position = end;
    }
    if position != length || !stream {
        return Err("ASF header has invalid objects or no stream".into());
    }
    let (properties, size) = properties.ok_or("ASF header has no File Properties Object")?;
    Ok((length, properties, size))
}

/// Restore padding stripped by MMSH. Packet Length may be implicit because the
/// accepted ASF header specifies equal minimum/maximum packet sizes.
fn asf_packet(bytes: &[u8], packet_size: usize) -> Result<Vec<u8>, String> {
    if bytes.len() > packet_size || bytes.len() < 8 {
        return Err("invalid ASF packet length".into());
    }
    let offset = if bytes[0] & 0x80 != 0 {
        1 + (bytes[0] & 0x0f) as usize
    } else {
        0
    };
    if offset + 2 > bytes.len() {
        return Err("truncated ASF packet header".into());
    }
    let flags = bytes[offset];
    let sizes = [0usize, 1, 2, 4];
    let length_size = sizes[((flags >> 5) & 3) as usize];
    let sequence_size = sizes[((flags >> 1) & 3) as usize];
    let padding_size = sizes[((flags >> 3) & 3) as usize];
    let padding_offset = offset + 2 + length_size + sequence_size;
    let payload_offset = padding_offset + padding_size;
    if payload_offset + 6 > bytes.len() {
        return Err("truncated ASF packet parsing information".into());
    }
    if bytes.len() == packet_size {
        return Ok(bytes.to_vec());
    }
    let mut existing_padding = [0u8; 4];
    existing_padding[..padding_size].copy_from_slice(&bytes[padding_offset..payload_offset]);
    let existing_padding = u32::from_le_bytes(existing_padding) as usize;
    let new_length = bytes.len() - length_size - padding_size + 4;
    if new_length > packet_size || existing_padding > bytes.len() - payload_offset - 6 {
        return Err("ASF padding cannot be restored within its fixed packet size".into());
    }
    let mut packet = Vec::with_capacity(packet_size);
    packet.extend_from_slice(&bytes[..offset]);
    packet.push((flags & !0x78) | 0x18); // implicit size; DWORD padding length
    packet.push(bytes[offset + 1]);
    packet.extend_from_slice(&bytes[offset + 2 + length_size..padding_offset]);
    packet.extend_from_slice(&((existing_padding + packet_size - new_length) as u32).to_le_bytes());
    packet.extend_from_slice(&bytes[payload_offset..]);
    packet.resize(packet_size, 0);
    Ok(packet)
}

fn media_extension(bytes: &[u8]) -> Option<&'static str> {
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
    } else if bytes.len() >= 7 && bytes[0] == 0xff && bytes[1] & 0xf6 == 0xf0 {
        Some("aac")
    } else if bytes.starts_with(b"ID3")
        || (bytes.len() >= 4 && bytes[0] == 0xff && bytes[1] & 0xe0 == 0xe0 && bytes[1] & 6 != 0)
    {
        Some("mp3")
    } else if bytes.starts_with(&ASF_HEADER) {
        Some("asf")
    } else {
        None
    }
}

fn http_url(base: &Url, value: &str) -> Result<Url, String> {
    let mut url = base
        .join(value.trim())
        .map_err(|_| "invalid discovered media URL")?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("discovered media URL must use HTTP(S) without URL credentials".into());
    }
    url.set_fragment(None);
    Ok(url)
}

fn attributes(value: &str) -> Result<HashMap<&str, &str>, String> {
    let mut attrs = HashMap::new();
    let mut rest = value.trim();
    while !rest.is_empty() {
        let (name, tail) = rest.split_once('=').ok_or("invalid HLS attribute")?;
        let (value, tail) = if let Some(quoted) = tail.strip_prefix('"') {
            let end = quoted
                .find('"')
                .ok_or("unterminated HLS quoted attribute")?;
            (&quoted[..end], &quoted[end + 1..])
        } else {
            tail.split_once(',').unwrap_or((tail, ""))
        };
        if name.trim().is_empty() || attrs.insert(name.trim(), value.trim()).is_some() {
            return Err("duplicate/empty HLS attribute".into());
        }
        rest = if tail.is_empty() {
            ""
        } else {
            tail.strip_prefix(',').unwrap_or(tail).trim()
        };
    }
    Ok(attrs)
}

fn byte_range(value: &str, previous: Option<(u64, u64)>) -> Result<(u64, u64), String> {
    let (length, offset) = value
        .split_once('@')
        .map(|(length, offset)| (length, Some(offset)))
        .unwrap_or((value, None));
    let length: u64 = length
        .parse()
        .map_err(|_| "invalid HLS byte range length")?;
    let offset = match offset {
        Some(offset) => offset
            .parse()
            .map_err(|_| "invalid HLS byte range offset")?,
        None => previous
            .and_then(|(offset, length)| offset.checked_add(length))
            .ok_or("implicit HLS byte range has no preceding range")?,
    };
    if length == 0 || length > SEGMENT_LIMIT as u64 || offset.checked_add(length).is_none() {
        return Err("HLS byte range exceeds supported bounds".into());
    }
    Ok((offset, length))
}

fn parse_playlist(text: &str, base: &Url) -> Result<Playlist, String> {
    if !text.trim_start_matches('\u{feff}').starts_with("#EXTM3U") {
        return Err("invalid HLS playlist signature".into());
    }
    let mut variants = Vec::new();
    let mut pending_variant = None;
    let mut playlist = MediaPlaylist {
        segments: Vec::new(),
        sequence: 0,
        end: false,
        target_duration: 6.0,
    };
    let mut key = None;
    let mut map = None;
    let mut range = None;
    let mut previous_resource: Option<Resource> = None;
    let mut duration = false;
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if let Some(value) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let attrs = attributes(value)?;
            let bandwidth: u64 = attrs
                .get("BANDWIDTH")
                .ok_or("HLS variant has no BANDWIDTH")?
                .parse()
                .map_err(|_| "invalid HLS BANDWIDTH")?;
            pending_variant = Some((
                bandwidth,
                !attrs.contains_key("AUDIO") && !attrs.contains_key("VIDEO"),
            ));
        } else if let Some(value) = line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:") {
            playlist.sequence = value.parse().map_err(|_| "invalid HLS media sequence")?;
        } else if let Some(value) = line.strip_prefix("#EXT-X-TARGETDURATION:") {
            playlist.target_duration = value
                .parse::<f64>()
                .ok()
                .filter(|v| v.is_finite() && *v > 0.0)
                .ok_or("invalid HLS target duration")?;
        } else if let Some(value) = line.strip_prefix("#EXTINF:") {
            if !value
                .split(',')
                .next()
                .unwrap_or_default()
                .parse::<f64>()
                .is_ok_and(|v| v.is_finite() && v >= 0.0)
            {
                return Err("invalid HLS segment duration".into());
            }
            duration = true;
        } else if let Some(value) = line.strip_prefix("#EXT-X-KEY:") {
            let attrs = attributes(value)?;
            key = match attrs.get("METHOD").copied() {
                Some("NONE") => None,
                Some("AES-128") => {
                    if attrs.get("KEYFORMAT").is_some_and(|value| *value != "identity") { return Err("HLS DRM/non-identity key formats are not supported".into()); }
                    let url = http_url(base, attrs.get("URI").ok_or("HLS AES-128 key has no URI")?)?;
                    let iv = attrs.get("IV").map(|value| {
                        let value = value.strip_prefix("0x").or_else(|| value.strip_prefix("0X")).ok_or("invalid HLS IV")?;
                        if value.len() > 32 || value.is_empty() || !value.bytes().all(|v| v.is_ascii_hexdigit()) { return Err("invalid HLS IV"); }
                        hex::decode(format!("{value:0>32}")).map_err(|_| "invalid HLS IV")?.try_into().map_err(|_| "invalid HLS IV")
                    }).transpose()?;
                    Some(Key { url, iv })
                }
                _ => return Err("HLS encryption supports AES-128 identity only; SAMPLE-AES/DRM are not supported".into()),
            };
        } else if let Some(value) = line.strip_prefix("#EXT-X-MAP:") {
            if key.is_some() {
                return Err("encrypted HLS initialization maps are not supported".into());
            }
            let attrs = attributes(value)?;
            let url = http_url(base, attrs.get("URI").ok_or("HLS MAP has no URI")?)?;
            let range = attrs
                .get("BYTERANGE")
                .map(|value| byte_range(value, Some((0, 0))))
                .transpose()?;
            map = Some(Resource { url, range });
        } else if let Some(value) = line.strip_prefix("#EXT-X-BYTERANGE:") {
            range = Some(value.to_owned());
        } else if line == "#EXT-X-ENDLIST" {
            playlist.end = true;
        } else if line == "#EXT-X-DISCONTINUITY"
            || line == "#EXT-X-I-FRAMES-ONLY"
            || line.starts_with("#EXT-X-PART:")
            || line.starts_with("#EXT-X-SKIP:")
            || line == "#EXT-X-GAP"
        {
            return Err("HLS discontinuities, iframe-only, gaps and low-latency partial/delta playlists are not supported".into());
        } else if !line.starts_with('#') {
            let url = http_url(base, line)?;
            if let Some((bandwidth, self_contained)) = pending_variant.take() {
                if self_contained {
                    variants.push((bandwidth, url));
                }
            } else {
                if !duration {
                    return Err("HLS media segment has no EXTINF".into());
                }
                let previous = previous_resource
                    .as_ref()
                    .filter(|resource| resource.url == url)
                    .and_then(|resource| resource.range);
                let range = range
                    .take()
                    .map(|value| byte_range(&value, previous))
                    .transpose()?;
                let resource = Resource { url, range };
                previous_resource = Some(resource.clone());
                playlist.segments.push(Segment {
                    resource,
                    key: key.clone(),
                    map: map.clone(),
                });
                if playlist.segments.len() > SEGMENT_COUNT_LIMIT {
                    return Err("HLS playlist exceeds 100000 segments".into());
                }
                duration = false;
            }
        }
    }
    if pending_variant.is_some() || duration || range.is_some() {
        return Err("HLS playlist ends with an incomplete entry".into());
    }
    if text.contains("#EXT-X-STREAM-INF:") {
        variants.sort_by_key(|(bandwidth, _)| std::cmp::Reverse(*bandwidth));
        return Ok(Playlist::Master(
            variants
                .into_iter()
                .next()
                .ok_or("HLS master requires unsupported external audio/video rendition muxing")?
                .1,
        ));
    }
    Ok(Playlist::Media(playlist))
}

fn discover(html: &str, page: &Url) -> Result<Url, String> {
    let document = scraper::Html::parse_document(html);
    let selector = scraper::Selector::parse(
        "base[href], video[src], video source[src], meta[property], meta[name]",
    )
    .unwrap();
    let mut base = page.clone();
    let mut candidates = Vec::new();
    let mut seen_base = false;
    for element in document.select(&selector) {
        let attrs = element.value();
        if attrs.name() == "base" {
            if !seen_base {
                base = http_url(page, attrs.attr("href").unwrap())?;
                seen_base = true;
            }
        } else if let Some(src) = attrs.attr("src") {
            candidates.push(src.to_owned());
        } else if let Some(property) = attrs.attr("property").or_else(|| attrs.attr("name")) {
            if matches!(
                property.to_ascii_lowercase().as_str(),
                "og:video" | "og:video:url" | "og:video:secure_url" | "twitter:player:stream"
            ) {
                if let Some(content) = attrs.attr("content") {
                    candidates.push(content.to_owned());
                }
            }
        }
        if candidates.len() >= 64 {
            break;
        }
    }
    for candidate in candidates {
        if let Ok(url) = http_url(&base, &candidate) {
            return Ok(url);
        }
    }
    // Public already-resolved muxed URLs/HLS only. No signatureCipher, JS,
    // login, DRM bypass or adaptive audio/video track muxing is attempted.
    for marker in ["ytInitialPlayerResponse", "\"playerResponse\""] {
        for (position, _) in html.match_indices(marker).take(8) {
            let tail = &html[position + marker.len()..];
            let Some(start) = tail.find('{').filter(|position| *position < 256) else {
                continue;
            };
            let mut stream =
                serde_json::Deserializer::from_str(&tail[start..]).into_iter::<serde_json::Value>();
            let Some(Ok(player)) = stream.next() else {
                continue;
            };
            if player
                .pointer("/playabilityStatus/status")
                .and_then(|v| v.as_str())
                .is_some_and(|status| status != "OK")
            {
                return Err("website player does not permit this video to play publicly".into());
            }
            if let Some(formats) = player
                .pointer("/streamingData/formats")
                .and_then(|value| value.as_array())
            {
                let mut direct: Vec<_> = formats
                    .iter()
                    .filter(|format| {
                        format.get("drmFamilies").is_none()
                            && format.get("signatureCipher").is_none()
                            && format.get("cipher").is_none()
                    })
                    .filter_map(|format| {
                        Some((
                            format.get("bitrate").and_then(|v| v.as_u64()).unwrap_or(0),
                            format.get("url")?.as_str()?,
                        ))
                    })
                    .collect();
                direct.sort_by_key(|(bitrate, _)| std::cmp::Reverse(*bitrate));
                for (_, value) in direct {
                    if let Ok(url) = http_url(page, value) {
                        return Ok(url);
                    }
                }
            }
            if let Some(value) = player
                .pointer("/streamingData/hlsManifestUrl")
                .and_then(|v| v.as_str())
            {
                return http_url(page, value);
            }
            return Err("website player requires unsupported signature decoding, adaptive-track muxing, DRM, or has no public media URL".into());
        }
    }
    Err("no native media source found: supported HTML video/source/og:video and public player JSON direct URLs/HLS; arbitrary sites, JavaScript signatures, DRM and adaptive muxing are unsupported".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::RateLimiter;
    use aes::cipher::BlockEncryptMut;
    use sha2::{Digest, Sha256};
    use tokio::io::AsyncReadExt;
    use tokio::net::{TcpListener, TcpStream};
    use tokio_util::sync::CancellationToken;

    struct Reply {
        status: &'static str,
        headers: String,
        body: Vec<u8>,
        stall: bool,
    }
    fn reply(content_type: &str, body: impl Into<Vec<u8>>) -> Reply {
        Reply {
            status: "200 OK",
            headers: format!("Content-Type: {content_type}\r\n"),
            body: body.into(),
            stall: false,
        }
    }
    struct HttpFixture {
        base: String,
        requests: Arc<Mutex<Vec<String>>>,
        cancel: CancellationToken,
        handle: tokio::task::JoinHandle<()>,
    }
    impl Drop for HttpFixture {
        fn drop(&mut self) {
            self.cancel.cancel();
            self.handle.abort();
        }
    }

    async fn request_header(socket: &mut TcpStream) -> String {
        let mut header = Vec::new();
        while !header.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            if socket.read_exact(&mut byte).await.is_err() {
                break;
            }
            header.push(byte[0]);
            assert!(header.len() <= 16 << 10);
        }
        String::from_utf8(header).unwrap()
    }

    async fn http_fixture(routes: HashMap<String, Reply>) -> HttpFixture {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let routes = Arc::new(routes);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let observed = requests.clone();
        let cancel = CancellationToken::new();
        let stop = cancel.clone();
        let handle = tokio::spawn(async move {
            loop {
                let (mut socket, _) = tokio::select! { result = listener.accept() => result.unwrap(), _ = stop.cancelled() => break };
                let routes = routes.clone();
                let observed = observed.clone();
                let stop = stop.clone();
                tokio::spawn(async move {
                    let header = tokio::select! { header = request_header(&mut socket) => header, _ = stop.cancelled() => return };
                    let path = header
                        .lines()
                        .next()
                        .unwrap()
                        .split_whitespace()
                        .nth(1)
                        .unwrap()
                        .split('?')
                        .next()
                        .unwrap()
                        .to_owned();
                    observed.lock().unwrap().push(header.clone());
                    let Some(reply) = routes.get(&path) else {
                        let _ = socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
                        return;
                    };
                    let mut status = reply.status;
                    let mut extra = reply.headers.clone();
                    let mut body = reply.body.as_slice();
                    if path != "/ignore-range" {
                        if let Some(value) = header.lines().find_map(|line| {
                            line.to_ascii_lowercase()
                                .strip_prefix("range: ")
                                .map(str::to_owned)
                        }) {
                            let (start, end) = value
                                .strip_prefix("bytes=")
                                .unwrap()
                                .split_once('-')
                                .unwrap();
                            let start: usize = start.parse().unwrap();
                            let end: usize = end.parse().unwrap();
                            extra.push_str(&format!(
                                "Content-Range: bytes {start}-{end}/{}\r\n",
                                body.len()
                            ));
                            body = &body[start..=end];
                            status = "206 Partial Content";
                        }
                    }
                    let length = if reply.stall {
                        String::new()
                    } else {
                        format!("Content-Length: {}\r\n", body.len())
                    };
                    let response =
                        format!("HTTP/1.1 {status}\r\n{extra}{length}Connection: close\r\n\r\n");
                    if socket.write_all(response.as_bytes()).await.is_err() {
                        return;
                    }
                    for chunk in body.chunks(257) {
                        if socket.write_all(chunk).await.is_err() {
                            return;
                        }
                    }
                    if reply.stall {
                        stop.cancelled().await;
                    }
                });
            }
        });
        HttpFixture {
            base,
            requests,
            cancel,
            handle,
        }
    }

    fn environment(url: String) -> (tempfile::TempDir, Arc<Mutex<Task>>, Ctx) {
        let stage = tempfile::tempdir().unwrap();
        let task = Arc::new(Mutex::new(Task {
            url,
            ..Default::default()
        }));
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let ctx = Ctx {
            client: client.clone(),
            protocol_client: client,
            global: Arc::new(RateLimiter::new(0)),
            local: Arc::new(RateLimiter::new(0)),
            retries: 0,
            cancel: CancellationToken::new(),
            verify: false,
        };
        (stage, task, ctx)
    }

    fn wav(samples: usize) -> Vec<u8> {
        let mut bytes = b"RIFF".to_vec();
        bytes.extend_from_slice(&(36 + samples as u32 * 2).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&8000u32.to_le_bytes());
        bytes.extend_from_slice(&16000u32.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&(samples as u32 * 2).to_le_bytes());
        bytes.resize(44 + samples * 2, 0);
        bytes
    }

    fn adts() -> Vec<u8> {
        // AAC-LC 44.1 kHz stereo silent frames, with correct 13-byte ADTS length.
        [
            0xff, 0xf1, 0x50, 0x80, 0x01, 0xbf, 0xfc, 0x21, 0x10, 0x04, 0x60, 0x8c, 0x1c,
        ]
        .repeat(20)
    }

    async fn rtsp_fixture(lost: bool, end: bool) -> (String, Vec<u8>, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("rtsp://{}/live/", listener.local_addr().unwrap());
        let base = url.clone();
        let idr = [0x65, 0x88, 0x84, 0x3a, 0x26, 0x28, 0, 0x09, 0x02, 0xe0];
        let mut sps = base64::engine::general_purpose::STANDARD
            .decode("Z2QAHqwsaoLA9puCgIKgAAADACAAAAMD0IAA")
            .unwrap();
        let mut pps = base64::engine::general_purpose::STANDARD
            .decode("aO4xshsA")
            .unwrap();
        // SDP parameters can contain trailing_zero_8bits; Retina normalizes
        // those separators before inserting the Annex B parameter-set NALs.
        while sps.last() == Some(&0) {
            sps.pop();
        }
        while pps.last() == Some(&0) {
            pps.pop();
        }
        let mut expected = Vec::new();
        for _ in 0..2 {
            for nal in [sps.as_slice(), pps.as_slice(), &idr] {
                expected.extend_from_slice(&[0, 0, 0, 1]);
                expected.extend_from_slice(nal);
            }
        }
        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            for method in ["DESCRIBE", "SETUP", "PLAY"] {
                let request = request_header(&mut socket).await;
                assert!(request.starts_with(method), "{request}");
                let cseq = request
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("cseq: ")
                            .map(str::to_owned)
                    })
                    .unwrap();
                let mut extra = String::new();
                let mut body = String::new();
                if method == "DESCRIBE" {
                    extra = format!("Content-Type: application/sdp\r\nContent-Base: {base}\r\n");
                    body = "v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=Native fixture\r\nt=0 0\r\na=control:*\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\na=fmtp:96 packetization-mode=1;profile-level-id=64001E;sprop-parameter-sets=Z2QAHqwsaoLA9puCgIKgAAADACAAAAMD0IAA,aO4xshsA\r\na=control:track1\r\n".into();
                } else if method == "SETUP" {
                    assert!(request.to_ascii_lowercase().contains("interleaved=0-1"));
                    extra = "Session: fixture;timeout=60\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1;ssrc=12345678\r\n".into();
                } else if method == "PLAY" {
                    extra = format!(
                        "Session: fixture\r\nRange: npt=0-\r\nRTP-Info: url={base}track1;seq=1000;rtptime=0\r\n"
                    );
                }
                socket.write_all(format!("RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\n{extra}Content-Length: {}\r\n\r\n{body}", body.len()).as_bytes()).await.unwrap();
            }
            // A single NAL packet, then the same IDR fragmented over two FU-A RTP
            // packets, exercises real interleaved framing and depacketization.
            for (sequence, timestamp, mark, payload) in [
                (1000u16, 0u32, true, idr.to_vec()),
                (
                    if lost { 1002 } else { 1001 },
                    3600,
                    false,
                    [&[0x7c, 0x85][..], &idr[1..5]].concat(),
                ),
                (
                    if lost { 1003 } else { 1002 },
                    3600,
                    true,
                    [&[0x7c, 0x45][..], &idr[5..]].concat(),
                ),
            ] {
                let mut packet = vec![0x80, 96 | if mark { 0x80 } else { 0 }];
                packet.extend_from_slice(&sequence.to_be_bytes());
                packet.extend_from_slice(&timestamp.to_be_bytes());
                packet.extend_from_slice(&0x12345678u32.to_be_bytes());
                packet.extend_from_slice(&payload);
                let mut frame = vec![b'$', 0];
                frame.extend_from_slice(&(packet.len() as u16).to_be_bytes());
                frame.extend_from_slice(&packet);
                if socket.write_all(&frame).await.is_err() {
                    return;
                }
            }
            if end {
                socket
                    .write_all(&[b'$', 1, 0, 8, 0x81, 203, 0, 1, 0x12, 0x34, 0x56, 0x78])
                    .await
                    .unwrap();
            }
            let mut buffer = [0u8; 128];
            let _ = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buffer)).await;
        });
        (url, expected, handle)
    }

    #[tokio::test]
    async fn rtsp_describe_setup_play_rtp_fua_and_bye_record_annex_b() {
        let (url, expected, server) = rtsp_fixture(false, true).await;
        let (stage, task, ctx) = environment(url);
        let result = tokio::time::timeout(
            Duration::from_secs(5),
            download(task.clone(), ctx, stage.path()),
        )
        .await
        .unwrap();
        assert_eq!(result.unwrap(), Outcome::Completed);
        let output = std::fs::read(stage.path().join("payload/media.h264")).unwrap();
        assert_eq!(output, expected);
        assert_eq!(Sha256::digest(&output), Sha256::digest(&expected));
        assert_eq!(task.lock().unwrap().total, Some(expected.len() as u64));
        server.await.unwrap();
    }

    #[tokio::test]
    async fn rtsp_loss_and_cancel_never_publish_or_reuse_partial_stream() {
        let (url, _, server) = rtsp_fixture(true, true).await;
        let (stage, task, ctx) = environment(url);
        assert!(
            tokio::time::timeout(Duration::from_secs(5), download(task, ctx, stage.path()))
                .await
                .unwrap()
                .is_err()
        );
        assert_eq!(
            std::fs::read_dir(stage.path().join("payload"))
                .unwrap()
                .count(),
            0
        );
        server.await.unwrap();
        let (url, _, server) = rtsp_fixture(false, false).await;
        let (stage, task, ctx) = environment(url);
        let cancel = ctx.cancel.clone();
        let result = tokio::join!(download(task, ctx, stage.path()), async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            cancel.cancel();
        });
        assert_eq!(result.0.unwrap(), Outcome::Cancelled);
        assert_eq!(
            std::fs::read_dir(stage.path().join("payload"))
                .unwrap()
                .count(),
            0
        );
        assert!(stage.path().join("state/media.part").is_file());
        server.await.unwrap();
        // Reuse the same stage with a new session; stale bytes must be truncated.
        let (url, expected, server) = rtsp_fixture(false, true).await;
        let (_, task, ctx) = environment(url);
        assert_eq!(
            download(task, ctx, stage.path()).await.unwrap(),
            Outcome::Completed
        );
        assert_eq!(
            std::fs::read(stage.path().join("payload/media.h264")).unwrap(),
            expected
        );
        server.await.unwrap();
    }

    fn asf_fixture() -> (Vec<u8>, Vec<u8>) {
        let mut properties = vec![0u8; 104];
        properties[..16].copy_from_slice(&ASF_FILE_PROPERTIES);
        properties[16..24].copy_from_slice(&104u64.to_le_bytes());
        properties[64..72].copy_from_slice(&400_000u64.to_le_bytes());
        properties[88..92].copy_from_slice(&1u32.to_le_bytes());
        properties[92..96].copy_from_slice(&64u32.to_le_bytes());
        properties[96..100].copy_from_slice(&64u32.to_le_bytes());
        let mut stream = vec![0u8; 96];
        stream[..16].copy_from_slice(&ASF_STREAM_PROPERTIES);
        stream[16..24].copy_from_slice(&96u64.to_le_bytes());
        stream[24..40].copy_from_slice(&hex::decode("409e69f84d5bcf11a8fd00805f5c442b").unwrap());
        stream[40..56].copy_from_slice(&hex::decode("0057fb20555bcf11a8fd00805f5c442b").unwrap());
        stream[64..68].copy_from_slice(&18u32.to_le_bytes());
        stream[72..74].copy_from_slice(&1u16.to_le_bytes());
        stream[78..80].copy_from_slice(&1u16.to_le_bytes());
        stream[80..82].copy_from_slice(&1u16.to_le_bytes());
        stream[82..86].copy_from_slice(&8000u32.to_le_bytes());
        stream[86..90].copy_from_slice(&16000u32.to_le_bytes());
        stream[90..92].copy_from_slice(&2u16.to_le_bytes());
        stream[92..94].copy_from_slice(&16u16.to_le_bytes());
        let mut extension = vec![0u8; 46];
        extension[..16].copy_from_slice(&hex::decode("b503bf5f2ea9cf118ee300c00c205365").unwrap());
        extension[16..24].copy_from_slice(&46u64.to_le_bytes());
        extension[24..40]
            .copy_from_slice(&hex::decode("11d2d3abbaa9cf118ee600c00c205365").unwrap());
        extension[40..42].copy_from_slice(&6u16.to_le_bytes());
        let mut header = ASF_HEADER.to_vec();
        let length = 30 + properties.len() + stream.len() + extension.len();
        header.extend_from_slice(&(length as u64).to_le_bytes());
        header.extend_from_slice(&3u32.to_le_bytes());
        header.extend_from_slice(&[1, 2]);
        header.extend_from_slice(&properties);
        header.extend_from_slice(&stream);
        header.extend_from_slice(&extension);
        header.extend_from_slice(&ASF_DATA);
        header.extend_from_slice(&50u64.to_le_bytes());
        header.extend_from_slice(&[0; 16]);
        header.extend_from_slice(&0u64.to_le_bytes());
        header.extend_from_slice(&[1, 1]);
        // Single PCM payload: 20 bytes, one-byte object id, four-byte offset,
        // eight-byte replicated data. Padding has been removed on the wire.
        let mut packet = vec![0x08, 0x5d, 0, 0, 0, 0, 0, 20, 0, 0x81, 0, 0, 0, 0, 0, 8];
        packet.extend_from_slice(&20u32.to_le_bytes());
        packet.extend_from_slice(&0u32.to_le_bytes());
        packet.extend_from_slice(&[0; 20]);
        (header, packet)
    }

    fn mms_frame(kind: u8, location: u32, flags: u8, payload: &[u8]) -> Vec<u8> {
        let mut body = Vec::new();
        if kind == b'H' || kind == b'D' {
            body.extend_from_slice(&location.to_le_bytes());
            body.extend_from_slice(&[0, flags]);
            body.extend_from_slice(&((payload.len() + 8) as u16).to_le_bytes());
        }
        body.extend_from_slice(payload);
        let mut frame = vec![0xa4, kind];
        frame.extend_from_slice(&(body.len() as u16).to_le_bytes());
        frame.extend_from_slice(&body);
        frame
    }

    #[tokio::test]
    async fn mmsh_play_header_fragments_data_padding_and_end_produce_asf() {
        let (header, packet) = asf_fixture();
        let body = [
            mms_frame(b'H', 0, 4, &header[..100]),
            mms_frame(b'H', 1, 8, &header[100..]),
            mms_frame(b'D', 0, 0, &packet),
            mms_frame(b'D', 1, 1, &packet),
            mms_frame(b'E', 0, 0, &[0; 4]),
        ]
        .concat();
        let fixture = http_fixture(HashMap::from([(
            "/stream".into(),
            reply("application/x-mms-framed", body),
        )]))
        .await;
        let (stage, task, ctx) =
            environment(fixture.base.replacen("http:", "mmsh:", 1) + "/stream");
        assert_eq!(
            download(task.clone(), ctx, stage.path()).await.unwrap(),
            Outcome::Completed
        );
        let output = std::fs::read(stage.path().join("payload/media.asf")).unwrap();
        let (length, properties, packet_size) = asf_header(&output[..header.len()]).unwrap();
        assert_eq!(output.len(), header.len() + 128);
        assert_eq!(packet_size, 64);
        assert_eq!(
            u64::from_le_bytes(output[properties + 40..properties + 48].try_into().unwrap()),
            output.len() as u64
        );
        assert_eq!(
            u64::from_le_bytes(output[length + 40..length + 48].try_into().unwrap()),
            2
        );
        let expected_packet = asf_packet(&packet, 64).unwrap();
        assert_eq!(&output[header.len()..], expected_packet.repeat(2));
        assert_eq!(
            u32::from_le_bytes(expected_packet[2..6].try_into().unwrap()),
            17
        );
        assert_eq!(task.lock().unwrap().downloaded(), output.len() as u64);
        assert!(fixture.requests.lock().unwrap()[0].contains("xPlayStrm=1"));
    }

    #[tokio::test]
    async fn malformed_or_drm_mmsh_never_publishes() {
        let (header, packet) = asf_fixture();
        let mut encrypted = header.clone();
        encrypted[30 + 104 + 73] |= 0x80;
        let mut invalid_size = mms_frame(b'H', 0, 0x0c, &header);
        invalid_size[10] = 0;
        let cases = [
            mms_frame(b'D', 0, 0, &packet),
            [
                mms_frame(b'H', 0, 0x0c, &header),
                mms_frame(b'D', 0, 0, &packet),
            ]
            .concat(),
            [
                mms_frame(b'H', 0, 0x0c, &header),
                mms_frame(b'D', 0, 0, &packet),
                mms_frame(b'E', 0, 0, &0x80004005u32.to_le_bytes()),
            ]
            .concat(),
            mms_frame(b'H', 0, 0x0c, &encrypted),
            invalid_size,
            vec![b'$', b'H', 30, 0, 1, 2, 3],
        ];
        for body in cases {
            let fixture = http_fixture(HashMap::from([(
                "/stream".into(),
                reply("application/x-mms-framed", body),
            )]))
            .await;
            let (stage, task, ctx) =
                environment(fixture.base.replacen("http:", "mms:", 1) + "/stream");
            assert!(download(task, ctx, stage.path()).await.is_err());
            assert_eq!(
                std::fs::read_dir(stage.path().join("payload"))
                    .unwrap()
                    .count(),
                0
            );
        }
    }

    #[tokio::test]
    async fn hls_master_ranges_aes128_and_hash() {
        let segment = adts();
        let expected = segment.repeat(3);
        let mut iv = [0u8; 16];
        iv[15] = 9;
        let encrypted = cbc::Encryptor::<aes::Aes128>::new(&[0x42; 16].into(), &iv.into())
            .encrypt_padded_vec_mut::<Pkcs7>(&segment);
        let playlist = format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXT-X-MEDIA-SEQUENCE:7\n#EXTINF:2,\n#EXT-X-BYTERANGE:{}@0\nall.aac\n#EXTINF:2,\n#EXT-X-BYTERANGE:{}\nall.aac\n#EXT-X-KEY:METHOD=AES-128,URI=\"key\"\n#EXTINF:2,\nlast.aac\n#EXT-X-ENDLIST\n",
            segment.len(),
            segment.len()
        );
        let fixture = http_fixture(HashMap::from([
            ("/master.m3u8".into(), reply("application/vnd.apple.mpegurl", b"#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nmissing.m3u8\n#EXT-X-STREAM-INF:BANDWIDTH=2,CODECS=\"mp4a.40.2\"\nmedia.m3u8\n".to_vec())),
            ("/media.m3u8".into(), reply("application/vnd.apple.mpegurl", playlist)),
            ("/all.aac".into(), reply("audio/aac", segment.repeat(2))),
            ("/last.aac".into(), reply("application/octet-stream", encrypted)),
            ("/key".into(), reply("application/octet-stream", vec![0x42;16])),
        ])).await;
        let (stage, task, ctx) = environment(format!("{}/master.m3u8", fixture.base));
        assert_eq!(
            download(task.clone(), ctx, stage.path()).await.unwrap(),
            Outcome::Completed
        );
        let output = std::fs::read(stage.path().join("payload/media.aac")).unwrap();
        assert_eq!(output, expected);
        assert_eq!(Sha256::digest(&output), Sha256::digest(&expected));
        assert_eq!(task.lock().unwrap().downloaded(), output.len() as u64);
        assert!(!task.lock().unwrap().resumable);
        let requests = fixture.requests.lock().unwrap();
        assert!(
            requests
                .iter()
                .any(|header| header.to_ascii_lowercase().contains("range: bytes=260-519"))
        );
        assert!(
            !requests
                .iter()
                .any(|header| header.contains("missing.m3u8"))
        );
    }

    #[tokio::test]
    async fn html_video_source_og_and_public_player_json_download_media() {
        let media = wav(512);
        let fixture = http_fixture(HashMap::from([
            ("/source".into(), reply("text/html", b"<base href='/assets/'><video><source src='sound.wav?a=1&amp;b=2'></video>".to_vec())),
            ("/og".into(), reply("text/html", b"<meta property='og:video' content='/assets/sound.wav'>".to_vec())),
            ("/player".into(), reply("text/html", b"<script>var ytInitialPlayerResponse = {\"playabilityStatus\":{\"status\":\"OK\"},\"streamingData\":{\"formats\":[{\"url\":\"/assets/sound.wav\",\"bitrate\":123}]}};</script>".to_vec())),
            ("/assets/sound.wav".into(), reply("audio/wav", media.clone())),
        ])).await;
        for page in ["source", "og", "player"] {
            let (stage, task, ctx) = environment(format!("{}/{page}", fixture.base));
            assert_eq!(
                download(task, ctx, stage.path()).await.unwrap(),
                Outcome::Completed
            );
            let output = std::fs::read(stage.path().join("payload/media.wav")).unwrap();
            assert_eq!(Sha256::digest(&output), Sha256::digest(&media));
        }
        assert!(
            fixture
                .requests
                .lock()
                .unwrap()
                .iter()
                .any(|header| header.contains("/assets/sound.wav?a=1&b=2"))
        );
    }

    #[tokio::test]
    async fn credentials_stay_at_original_origin_in_redirects_and_discovery() {
        let destination = http_fixture(HashMap::from([(
            "/sound.wav".into(),
            reply("audio/wav", wav(32)),
        )]))
        .await;
        let origin = http_fixture(HashMap::from([
            (
                "/redirect".into(),
                Reply {
                    status: "302 Found",
                    headers: format!("Location: {}/sound.wav\r\n", destination.base),
                    body: Vec::new(),
                    stall: false,
                },
            ),
            (
                "/page".into(),
                reply(
                    "text/html",
                    format!("<video src='{}/sound.wav'></video>", destination.base),
                ),
            ),
        ]))
        .await;
        for page in ["redirect", "page"] {
            let (stage, task, ctx) = environment(format!("{}/{page}", origin.base));
            task.lock().unwrap().cookie = Some("private=secret".into());
            task.lock().unwrap().authorization = Some("Basic dXNlcjpwYXNz".into());
            assert_eq!(
                download(task, ctx, stage.path()).await.unwrap(),
                Outcome::Completed
            );
        }
        assert!(origin.requests.lock().unwrap().iter().all(|header| {
            header
                .to_ascii_lowercase()
                .contains("cookie: private=secret")
                && header.contains("Basic dXNlcjpwYXNz")
        }));
        assert!(
            destination
                .requests
                .lock()
                .unwrap()
                .iter()
                .all(|header| !header.to_ascii_lowercase().contains("cookie:")
                    && !header.to_ascii_lowercase().contains("authorization:"))
        );
    }

    #[tokio::test]
    async fn limits_actual_http_bytes_and_cancels_body_or_limiter_waits() {
        let media = wav(16_384);
        let fixture = http_fixture(HashMap::from([
            ("/media".into(), reply("audio/wav", media.clone())),
            (
                "/stall".into(),
                Reply {
                    status: "200 OK",
                    headers: "Content-Type: video/mp4\r\n".into(),
                    body: Vec::new(),
                    stall: true,
                },
            ),
        ]))
        .await;
        let (stage, task, ctx) = environment(format!("{}/media", fixture.base));
        ctx.global.set_rate(65_536);
        ctx.local.set_rate(131_072);
        let started = std::time::Instant::now();
        assert_eq!(
            download(task, ctx, stage.path()).await.unwrap(),
            Outcome::Completed
        );
        assert!(
            started.elapsed() >= Duration::from_millis(350),
            "limiter took {:?}",
            started.elapsed()
        );
        for endpoint in ["stall", "media"] {
            let (stage, task, ctx) = environment(format!("{}/{endpoint}", fixture.base));
            ctx.local.set_rate(1);
            let cancel = ctx.cancel.clone();
            let future = download(task, ctx, stage.path());
            let result = tokio::join!(future, async {
                tokio::time::sleep(Duration::from_millis(80)).await;
                cancel.cancel();
            });
            assert_eq!(result.0.unwrap(), Outcome::Cancelled);
            assert_eq!(
                std::fs::read_dir(stage.path().join("payload"))
                    .unwrap()
                    .count(),
                0
            );
        }
    }

    #[tokio::test]
    async fn malformed_html_hls_and_unhonored_ranges_never_publish() {
        let fixture = http_fixture(HashMap::from([
            ("/login".into(), reply("text/html", b"<html>Please sign in</html>".to_vec())),
            ("/drm.m3u8".into(), reply("application/vnd.apple.mpegurl", b"#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"key\"\n#EXTINF:2,\nx\n#EXT-X-ENDLIST\n".to_vec())),
            ("/range.m3u8".into(), reply("application/vnd.apple.mpegurl", b"#EXTM3U\n#EXTINF:2,\n#EXT-X-BYTERANGE:260@0\nignore-range\n#EXT-X-ENDLIST\n".to_vec())),
            ("/ignore-range".into(), reply("audio/aac", adts())),
            ("/cipher".into(), reply("text/html", b"<script>var ytInitialPlayerResponse={\"streamingData\":{\"formats\":[{\"signatureCipher\":\"secret\"}]}};</script>".to_vec())),
            ("/oversized".into(), reply("text/html", vec![b' '; DOCUMENT_LIMIT + 1])),
        ])).await;
        for page in ["login", "drm.m3u8", "range.m3u8", "cipher", "oversized"] {
            let (stage, task, ctx) = environment(format!("{}/{page}", fixture.base));
            assert!(download(task, ctx, stage.path()).await.is_err(), "{page}");
            assert_eq!(
                std::fs::read_dir(stage.path().join("payload"))
                    .unwrap()
                    .count(),
                0
            );
        }
        let (stage, task, ctx) = environment("mmst://127.0.0.1/not-http".into());
        assert!(
            download(task, ctx, stage.path())
                .await
                .unwrap_err()
                .contains("MMST")
        );
    }
}
