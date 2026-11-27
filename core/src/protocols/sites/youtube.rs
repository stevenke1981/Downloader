//! YouTube item resolver through the InnerTube player API.
//!
//! Only single-video URLs resolve. The resolver asks a short list of InnerTube clients
//! for the player response and keeps the first one that returns directly downloadable
//! streams (no signature cipher, no JavaScript). Media requests must reuse
//! [`Resolved::user_agent`]; googlevideo serves large files best in ~10 MiB `Range`
//! chunks with per-chunk retry, sized from [`Stream::length`].
#![allow(dead_code)] // Wired into the media module in a later change.

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
    /// Use only `streamingData.hlsManifestUrl` from this client.
    pub hls_only: bool,
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
        hls_only: false,
    },
    Client {
        name: "IOS",
        id: 5,
        version: "19.45.4",
        user_agent: "com.google.ios.youtube/19.45.4 (iPhone16,2; U; CPU iOS 18_1_0 like Mac OS X;)",
        device_make: "Apple",
        device_model: "iPhone16,2",
        os_name: "iPhone",
        os_version: "18.1.0.22B83",
        android_sdk: None,
        hls_only: true,
    },
    Client {
        name: "ANDROID",
        id: 3,
        version: "19.44.38",
        user_agent: "com.google.android.youtube/19.44.38 (Linux; U; Android 11) gzip",
        device_make: "Google",
        device_model: "Pixel 5",
        os_name: "Android",
        os_version: "11",
        android_sdk: Some(30),
        hls_only: false,
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
    pub title: Option<String>,
    /// User agent of the client that produced the streams; media requests must reuse it.
    pub user_agent: String,
    pub choice: Choice,
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
        "YouTube: no directly downloadable stream (clients tried: {})",
        names.join(", ")
    )
}

/// Ask each client in turn for the player response of `id` and pick its streams.
/// `base` is the InnerTube origin ([`DEFAULT_BASE`] in production); its host is
/// also accepted for stream URLs so a local fixture can serve both.
pub(crate) async fn resolve(
    client: &reqwest::Client,
    base: &Url,
    id: &str,
    audio_only: bool,
) -> Result<Resolved, String> {
    if !valid_id(id) {
        return Err("YouTube: invalid video id".into());
    }
    let endpoint = base
        .join("youtubei/v1/player?prettyPrint=false")
        .map_err(|e| format!("YouTube: invalid API base: {e}"))?;
    let mut reason = None;
    let mut failure = None;
    let mut playable = false;
    for spec in CLIENTS {
        let player = match player(client, &endpoint, spec, id).await {
            Ok(player) => player,
            Err(e) => {
                failure.get_or_insert(e);
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
            reason.get_or_insert(text);
            continue;
        }
        playable = true;
        let choice = if spec.hls_only {
            if audio_only {
                continue;
            }
            hls(&player, Some(base))
        } else {
            choose_in(&player, audio_only, Some(base))
        };
        if let Ok(choice) = choice {
            let title = player
                .pointer("/videoDetails/title")
                .and_then(Value::as_str)
                .map(str::to_string);
            return Ok(Resolved {
                title,
                user_agent: spec.user_agent.to_string(),
                choice,
            });
        }
    }
    Err(match (playable, reason, failure) {
        (false, Some(reason), _) => format!("YouTube: {reason}"),
        (false, None, Some(failure)) => failure,
        _ => no_stream(),
    })
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
        .map_err(|e| format!("YouTube: player request failed: {e}"))?;
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
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| format!("YouTube: player response failed: {e}"))?
    {
        if body.len().saturating_add(chunk.len()) > BODY_LIMIT {
            return Err(too_large());
        }
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).map_err(|e| format!("YouTube: player response is not JSON: {e}"))
}

/// Pick streams from a player response with the production host policy.
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

fn choose_in(player: &Value, audio_only: bool, base: Option<&Url>) -> Result<Choice, String> {
    let adaptive = formats(player, "adaptiveFormats", base);
    let audio = best_audio(&adaptive);
    if audio_only {
        return audio.map(Choice::Audio).ok_or_else(no_stream);
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
    let progressive = formats(player, "formats", base);
    if let Some(stream) = progressive
        .iter()
        .filter(|f| f.stream.mime == "video/mp4" && !f.codecs.contains("av01"))
        .max_by_key(|f| (f.stream.itag == 18, f.height, f.bitrate))
    {
        return Ok(Choice::Progressive(stream.stream.clone()));
    }
    hls(player, base)
}

#[cfg(test)]
mod tests;
