//! Native HLS/direct media, RTSP H.264 Annex B or G.711 WAV, and MS-WMSP-over-HTTP.
//! Interrupted streams restart. Complete files enter payload; partial/control
//! files stay in state. No subprocess, C media engine, or JS runtime is used.
mod parse;

use crate::engine::{Ctx, Outcome};
use crate::task::{Seg, Task};
use aes::cipher::{BlockDecryptMut, KeyIvInit, block_padding::Pkcs7};
use base64::Engine;
use futures_util::StreamExt;
use parse::{discover, http_url, parse_playlist};
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
        use retina::codec::{CodecItem, FrameFormat, ParametersRef};
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
            .or_else(|| session.streams().iter().position(|stream| {
                stream.media() == "audio" && matches!(stream.encoding_name().to_ascii_lowercase().as_str(), "pcmu" | "pcma")
            }))
            .ok_or(
                "native RTSP requires H.264 video or G.711 PCMU/PCMA audio; other codecs and simultaneous A/V muxing are unsupported",
            )?;
        let audio = if session.streams()[stream].media() == "audio" {
            let Some(ParametersRef::Audio(parameters)) = session.streams()[stream].parameters()
            else {
                return Err("RTSP G.711 has no audio parameters".into());
            };
            let sample_rate = parameters.clock_rate();
            let channels = parameters.channels().get();
            if sample_rate != 8000 || !(1..=2).contains(&channels) {
                return Err("native G.711 capture supports 8000 Hz mono/stereo only".into());
            }
            Some((
                session.streams()[stream]
                    .encoding_name()
                    .eq_ignore_ascii_case("pcma"),
                sample_rate,
                channels,
            ))
        } else {
            None
        };
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
        if let Some((_, sample_rate, channels)) = audio {
            file.write_all(&pcm_wav_header(0, sample_rate, channels))
                .await
                .map_err(|e| e.to_string())?;
            written = 44;
        }
        let mut ready = false;
        let mut ended = false;
        self.task.lock().unwrap().append_log(if audio.is_some() {
            "Native RTSP/TCP G.711 -> PCM16 WAV capture; one audio track; completion requires RTCP BYE"
        } else {
            "Native RTSP/TCP H.264 Annex B capture; one video track, no audio; completion requires RTCP BYE"
        });
        while let Some(item) = tokio::time::timeout(Duration::from_secs(60), stream.next())
            .await
            .map_err(|_| "RTSP media timed out")?
        {
            match item.map_err(|e| format!("RTSP media: {e}"))? {
                CodecItem::VideoFrame(frame) if audio.is_none() => {
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
                CodecItem::AudioFrame(frame) => {
                    let Some((alaw, _, channels)) = audio else {
                        continue;
                    };
                    self.limit(frame.data().len().saturating_add(16)).await;
                    if frame.loss() != 0 {
                        return Err("RTSP G.711 RTP packet loss; refusing incomplete audio".into());
                    }
                    if !frame.data().len().is_multiple_of(channels as usize) {
                        return Err(
                            "RTSP G.711 payload does not contain whole audio sample frames".into(),
                        );
                    }
                    let data_length = written - 44 + frame.data().len() as u64 * 2;
                    if data_length > (u32::MAX - 36) as u64 {
                        return Err("RTSP WAV recording exceeds RIFF's 4 GiB size limit".into());
                    }
                    let pcm: Vec<u8> = frame
                        .data()
                        .iter()
                        .flat_map(|sample| g711_sample(*sample, alaw).to_le_bytes())
                        .collect();
                    file.write_all(&pcm).await.map_err(|e| e.to_string())?;
                    written += pcm.len() as u64;
                    ready = true;
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
            return Err("RTSP ended before a complete H.264 keyframe or G.711 audio frame".into());
        }
        if let Some((_, sample_rate, channels)) = audio {
            file.seek(std::io::SeekFrom::Start(0))
                .await
                .map_err(|e| e.to_string())?;
            file.write_all(&pcm_wav_header(
                (written - 44) as u32,
                sample_rate,
                channels,
            ))
            .await
            .map_err(|e| e.to_string())?;
        }
        self.finish(file, if audio.is_some() { "wav" } else { "h264" })
            .await
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
    let mut existing_padding = [0u8; 4];
    existing_padding[..padding_size].copy_from_slice(&bytes[padding_offset..payload_offset]);
    let existing_padding = u32::from_le_bytes(existing_padding) as usize;
    if existing_padding > bytes.len() - payload_offset - 6 {
        return Err("invalid ASF packet padding length".into());
    }
    if bytes.len() == packet_size {
        return Ok(bytes.to_vec());
    }
    let new_length = bytes.len() - length_size - padding_size + 4;
    if new_length > packet_size {
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

fn pcm_wav_header(data_length: u32, sample_rate: u32, channels: u16) -> [u8; 44] {
    let mut header = [0u8; 44];
    header[..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(data_length + 36).to_le_bytes());
    header[8..16].copy_from_slice(b"WAVEfmt ");
    header[16..20].copy_from_slice(&16u32.to_le_bytes());
    header[20..22].copy_from_slice(&1u16.to_le_bytes());
    header[22..24].copy_from_slice(&channels.to_le_bytes());
    header[24..28].copy_from_slice(&sample_rate.to_le_bytes());
    header[28..32].copy_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes());
    header[32..34].copy_from_slice(&(channels * 2).to_le_bytes());
    header[34..36].copy_from_slice(&16u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&data_length.to_le_bytes());
    header
}

fn g711_sample(code: u8, alaw: bool) -> i16 {
    // ITU-T G.711 segment expansion, calculated in i32 before the signed PCM16
    // conversion. A-law has a half-quantum offset; mu-law has a 132-unit bias.
    let code = if alaw { code ^ 0x55 } else { !code };
    let segment = (code >> 4) & 7;
    let magnitude = if alaw {
        let quantized = (code as i32 & 15) << 4;
        if segment == 0 {
            quantized + 8
        } else {
            (quantized + 264) << (segment - 1)
        }
    } else {
        (((code as i32 & 15) << 3) + 132) << segment
    };
    if alaw {
        if code & 0x80 != 0 {
            magnitude as i16
        } else {
            -magnitude as i16
        }
    } else if code & 0x80 != 0 {
        (132 - magnitude) as i16
    } else {
        (magnitude - 132) as i16
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
    } else if bytes.starts_with(&ASF_HEADER) {
        Some("asf")
    } else {
        None
    }
}

#[cfg(test)]
mod tests;
