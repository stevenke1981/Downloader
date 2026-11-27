//! YouTube item resolver through the InnerTube player API.
//!
//! Only single-video URLs resolve. [`Resolver`] asks a short list of InnerTube clients,
//! one at a time, for the player response and yields each one that returns directly
//! downloadable streams (no signature cipher, no JavaScript). The caller probes them and
//! moves to the next client when googlevideo limits them: without a proof-of-origin
//! token, adaptive streams often answer only the first minute of media. Media requests
//! must reuse [`Resolved::user_agent`]; googlevideo serves large files best in ~10 MiB
//! `Range` chunks with per-chunk retry, sized from [`Stream::length`].

use reqwest::Url;
use reqwest::header::{CONTENT_TYPE, USER_AGENT};
use serde_json::{Value, json};
use std::time::Duration;

/// Production InnerTube origin. Tests inject a local base instead.
pub(crate) const DEFAULT_BASE: &str = "https://www.youtube.com/";
const BODY_LIMIT: usize = 4 << 20;
const MAX_HEIGHT: u64 = 1080;

/// One InnerTube client identity, tried in [`CLIENTS`] order.
pub(crate) struct Client {
    pub name: &'static str,
    /// `X-YouTube-Client-Name` header value.
    pub id: u32,
    pub version: &'static str,
    pub user_agent: &'static str,
    pub device_make: &'static str,
    pub device_model: &'static str,
    pub os_name: &'static str,
    pub os_version: &'static str,
    pub android_sdk: Option<u32>,
}

pub(crate) const CLIENTS: &[Client] = &[
    Client {
        name: "ANDROID_VR",
        id: 28,
        version: "1.62.27",
        user_agent: "com.google.android.apps.youtube.vr.oculus/1.62.27 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip",
        device_make: "Oculus",
        device_model: "Quest 3",
        os_name: "Android",
        os_version: "12L",
        android_sdk: Some(32),
    },
    Client {
        name: "ANDROID",
        id: 3,
        version: "20.10.38",
        user_agent: "com.google.android.youtube/20.10.38 (Linux; U; Android 14) gzip",
        device_make: "Google",
        device_model: "Pixel 8",
        os_name: "Android",
        os_version: "14",
        android_sdk: Some(34),
    },
    Client {
        name: "IOS",
        id: 5,
        version: "20.10.4",
        user_agent: "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)",
        device_make: "Apple",
        device_model: "iPhone16,2",
        os_name: "iPhone",
        os_version: "18.3.2.22D82",
        android_sdk: None,
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Stream {
    pub url: Url,
    pub length: Option<u64>,
    pub mime: String,
    pub itag: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Choice {
    /// Separate DASH video (H.264) and audio (AAC) to download and mux.
    Split {
        video: Stream,
        audio: Stream,
    },
    Audio(Stream),
    Progressive(Stream),
    Hls(Url),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Resolved {
    /// Name of the InnerTube client, for logs.
    pub client: &'static str,
    pub title: Option<String>,
    /// User agent of the client that produced the streams; media requests must reuse it.
    pub user_agent: String,
    pub choice: Choice,
    /// Progressive stream from the same client, for when a split pair cannot be muxed.
    pub progressive: Option<Stream>,
}

/// The 11-character video id of a single-video YouTube URL. Channels, playlists,
/// profiles and other pages return `None`.
pub(crate) fn video_id(url: &Url) -> Option<String> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let host = url.host_str()?.to_ascii_lowercase();
    let host = host.strip_suffix('.').unwrap_or(&host);
    let segments: Vec<&str> = url.path_segments()?.filter(|s| !s.is_empty()).collect();
    let id = match host {
        "youtu.be" => match segments.as_slice() {
            [id] => Some((*id).to_string()),
            _ => None,
        },
        "youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com" => {
            match segments.as_slice() {
                ["watch"] => url
                    .query_pairs()
                    .find(|(k, _)| k == "v")
                    .map(|(_, v)| v.into_owned()),
                ["shorts" | "embed" | "live", id] => Some((*id).to_string()),
                _ => None,
            }
        }
        "youtube-nocookie.com" | "www.youtube-nocookie.com" => match segments.as_slice() {
            ["embed", id] => Some((*id).to_string()),
            _ => None,
        },
        _ => None,
    }?;
    (valid_id(&id) && id != "videoseries").then_some(id)
}

fn valid_id(id: &str) -> bool {
    id.len() == 11
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn no_stream() -> String {
    let names: Vec<&str> = CLIENTS.iter().map(|c| c.name).collect();
    format!(
        "YouTube: no fully downloadable stream (clients tried: {})",
        names.join(", ")
    )
}

/// Walks [`CLIENTS`] in order for one video. `base` is the InnerTube origin
/// ([`DEFAULT_BASE`] in production); its host is also accepted for stream URLs so a
/// local fixture can serve both.
pub(crate) struct Resolver {
    endpoint: Url,
    base: Url,
    id: String,
    next: usize,
    reason: Option<String>,
    failure: Option<String>,
    playable: bool,
}

impl Resolver {
    pub(crate) fn new(base: &Url, id: &str) -> Result<Self, String> {
        if !valid_id(id) {
            return Err("YouTube: invalid video id".into());
        }
        let endpoint = base
            .join("youtubei/v1/player?prettyPrint=false")
            .map_err(|e| format!("YouTube: invalid API base: {e}"))?;
        Ok(Self {
            endpoint,
            base: base.clone(),
            id: id.to_string(),
            next: 0,
            reason: None,
            failure: None,
            playable: false,
        })
    }

    /// The next client whose player response is playable and has usable streams.
    pub(crate) async fn next(
        &mut self,
        client: &reqwest::Client,
        audio_only: bool,
    ) -> Option<Resolved> {
        while let Some(spec) = CLIENTS.get(self.next) {
            self.next += 1;
            let player = match player(client, &self.endpoint, spec, &self.id).await {
                Ok(player) => player,
                Err(e) => {
                    self.failure.get_or_insert(e);
                    continue;
                }
            };
            let status = player
                .pointer("/playabilityStatus/status")
                .and_then(Value::as_str)
                .unwrap_or("");
            if status != "OK" {
                let text = player
                    .pointer("/playabilityStatus/reason")
                    .and_then(Value::as_str)
                    .map(str::trim)
                    .filter(|r| !r.is_empty())
                    .map(str::to_string)
                    .unwrap_or_else(|| format!("video is not playable ({status})"));
                self.reason.get_or_insert(text);
                continue;
            }
            self.playable = true;
            let Ok(choice) = choose_in(&player, audio_only, Some(&self.base)) else {
                continue;
            };
            let title = player
                .pointer("/videoDetails/title")
                .and_then(Value::as_str)
                .map(str::to_string);
            return Some(Resolved {
                client: spec.name,
                title,
                user_agent: spec.user_agent.to_string(),
                progressive: progressive(&player, Some(&self.base)),
                choice,
            });
        }
        None
    }

    /// Why no client produced a fully downloadable stream.
    pub(crate) fn error(self) -> String {
        match (self.playable, self.reason, self.failure) {
            (false, Some(reason), _) => format!("YouTube: {reason}"),
            (false, None, Some(failure)) => failure,
            _ => no_stream(),
        }
    }
}

/// The first client's streams, without probing them (tests).
#[cfg(test)]
pub(crate) async fn resolve(
    client: &reqwest::Client,
    base: &Url,
    id: &str,
    audio_only: bool,
) -> Result<Resolved, String> {
    let mut resolver = Resolver::new(base, id)?;
    match resolver.next(client, audio_only).await {
        Some(resolved) => Ok(resolved),
        None => Err(resolver.error()),
    }
}

fn request_body(spec: &Client, id: &str) -> Value {
    let mut context = json!({
        "clientName": spec.name,
        "clientVersion": spec.version,
        "deviceMake": spec.device_make,
        "deviceModel": spec.device_model,
        "osName": spec.os_name,
        "osVersion": spec.os_version,
        "hl": "en",
        "gl": "US",
    });
    if let Some(sdk) = spec.android_sdk {
        context["androidSdkVersion"] = json!(sdk);
    }
    json!({
        "context": { "client": context },
        "videoId": id,
        "contentCheckOk": true,
        "racyCheckOk": true,
    })
}

async fn player(
    client: &reqwest::Client,
    endpoint: &Url,
    spec: &Client,
    id: &str,
) -> Result<Value, String> {
    let mut response = client
        .post(endpoint.clone())
        .timeout(Duration::from_secs(30))
        .header(CONTENT_TYPE, "application/json")
        .header(USER_AGENT, spec.user_agent)
        .header("X-YouTube-Client-Name", spec.id.to_string())
        .header("X-YouTube-Client-Version", spec.version)
        .body(request_body(spec, id).to_string())
        .send()
        .await
        .map_err(|e| {
            format!(
                "YouTube: player request failed: {}",
                super::request_error(e)
            )
        })?;
    if !response.status().is_success() {
        return Err(format!(
            "YouTube: player request returned HTTP {}",
            response.status().as_u16()
        ));
    }
    let too_large = || "YouTube: player response exceeds the 4 MiB limit".to_string();
    if response
        .content_length()
        .is_some_and(|n| n > BODY_LIMIT as u64)
    {
        return Err(too_large());
    }
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|e| {
        format!(
            "YouTube: player response failed: {}",
            super::request_error(e)
        )
    })? {
        if body.len().saturating_add(chunk.len()) > BODY_LIMIT {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|e| format!("YouTube: player response is not JSON: {e}"))
}

/// Pick streams from a player response with the production host policy.
#[cfg(test)]
pub(crate) fn choose(player: &Value, audio_only: bool) -> Result<Choice, String> {
    choose_in(player, audio_only, None)
}

/// Stream URLs must be HTTPS on googlevideo.com (HLS manifests may also be on
/// youtube.com), or on the trusted API base origin when one is given.
fn allowed(url: &Url, base: Option<&Url>, hls: bool) -> bool {
    if let Some(base) = base
        && url.scheme() == base.scheme()
        && url.host_str().is_some()
        && url.host_str() == base.host_str()
    {
        return true;
    }
    let host = url.host_str().unwrap_or("").to_ascii_lowercase();
    let under = |domain: &str| host == domain || host.ends_with(&format!(".{domain}"));
    url.scheme() == "https" && (under("googlevideo.com") || (hls && under("youtube.com")))
}

struct Format<'a> {
    stream: Stream,
    codecs: &'a str,
    height: u64,
    bitrate: u64,
}

fn formats<'a>(player: &'a Value, key: &str, base: Option<&Url>) -> Vec<Format<'a>> {
    let Some(list) = player
        .pointer(&format!("/streamingData/{key}"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|f| {
            // Formats behind signatureCipher/cipher need player JavaScript: skip them.
            let url = Url::parse(f.get("url")?.as_str()?).ok()?;
            if !allowed(&url, base, false) {
                return None;
            }
            let mime_type = f.get("mimeType")?.as_str()?;
            let (mime, codecs) = mime_type.split_once(';').unwrap_or((mime_type, ""));
            let number = |name: &str| {
                f.get(name).and_then(|v| {
                    v.as_u64()
                        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
                })
            };
            Some(Format {
                stream: Stream {
                    url,
                    length: number("contentLength"),
                    mime: mime.trim().to_ascii_lowercase(),
                    itag: u32::try_from(number("itag")?).ok()?,
                },
                codecs,
                height: number("height").unwrap_or(0),
                bitrate: number("bitrate").unwrap_or(0),
            })
        })
        .collect()
}

fn best_audio(adaptive: &[Format]) -> Option<Stream> {
    adaptive
        .iter()
        .filter(|f| f.stream.mime == "audio/mp4" && f.codecs.contains("mp4a"))
        .max_by_key(|f| {
            (
                f.stream.itag == 140,
                f.codecs.contains("mp4a.40.2"),
                f.bitrate,
            )
        })
        .map(|f| f.stream.clone())
}

fn hls(player: &Value, base: Option<&Url>) -> Result<Choice, String> {
    player
        .pointer("/streamingData/hlsManifestUrl")
        .and_then(Value::as_str)
        .and_then(|u| Url::parse(u).ok())
        .filter(|u| allowed(u, base, true))
        .map(Choice::Hls)
        .ok_or_else(no_stream)
}

/// Best progressive (muxed) mp4, preferring itag 18.
fn progressive(player: &Value, base: Option<&Url>) -> Option<Stream> {
    formats(player, "formats", base)
        .into_iter()
        .filter(|f| f.stream.mime == "video/mp4" && !f.codecs.contains("av01"))
        .max_by_key(|f| (f.stream.itag == 18, f.height, f.bitrate))
        .map(|f| f.stream)
}

fn choose_in(player: &Value, audio_only: bool, base: Option<&Url>) -> Result<Choice, String> {
    let adaptive = formats(player, "adaptiveFormats", base);
    let audio = best_audio(&adaptive);
    if audio_only {
        // Without adaptive AAC the muxed stream's audio track is converted instead.
        return audio
            .map(Choice::Audio)
            .or_else(|| progressive(player, base).map(Choice::Progressive))
            .ok_or_else(no_stream);
    }
    let video = adaptive
        .iter()
        .filter(|f| {
            f.stream.mime == "video/mp4" && f.codecs.contains("avc1") && f.height <= MAX_HEIGHT
        })
        .max_by_key(|f| (f.height, f.bitrate))
        .map(|f| f.stream.clone());
    if let (Some(video), Some(audio)) = (video, audio) {
        return Ok(Choice::Split { video, audio });
    }
    if let Some(stream) = progressive(player, base) {
        return Ok(Choice::Progressive(stream));
    }
    hls(player, base)
}

#[cfg(test)]
mod tests;
