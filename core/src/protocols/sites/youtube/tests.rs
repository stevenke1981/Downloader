use super::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

fn id(url: &str) -> Option<String> {
    video_id(&Url::parse(url).unwrap())
}

#[test]
fn video_ids_come_only_from_item_urls() {
    let want = Some("dQw4w9WgXcQ".to_string());
    for url in [
        "https://www.youtube.com/watch?v=dQw4w9WgXcQ",
        "https://youtube.com/watch?v=dQw4w9WgXcQ&list=PL123&t=42",
        "https://m.youtube.com/watch?feature=share&v=dQw4w9WgXcQ",
        "https://music.youtube.com/watch?v=dQw4w9WgXcQ",
        "https://youtu.be/dQw4w9WgXcQ?si=abc",
        "https://www.youtube.com/shorts/dQw4w9WgXcQ",
        "https://www.youtube.com/embed/dQw4w9WgXcQ",
        "https://www.youtube.com/live/dQw4w9WgXcQ?feature=share",
        "https://www.youtube-nocookie.com/embed/dQw4w9WgXcQ",
        "http://WWW.YouTube.com./watch?v=dQw4w9WgXcQ",
    ] {
        assert_eq!(id(url), want, "{url}");
    }
    for url in [
        "https://www.youtube.com/",
        "https://www.youtube.com/@channel",
        "https://www.youtube.com/channel/UC1234567890",
        "https://www.youtube.com/c/Name",
        "https://www.youtube.com/user/name",
        "https://www.youtube.com/playlist?list=PL123",
        "https://www.youtube.com/embed/videoseries?list=PL123",
        "https://www.youtube.com/watch?v=short",
        "https://www.youtube.com/watch?v=dQw4w9WgXcQx",
        "https://www.youtube.com/watch?v=dQw4w9WgX%2BQ",
        "https://www.youtube.com/watch",
        "https://www.youtube.com/shorts/dQw4w9WgXcQ/extra",
        "https://youtu.be/",
        "https://youtu.be/dQw4w9WgXcQ/more",
        "https://www.youtube-nocookie.com/watch?v=dQw4w9WgXcQ",
        "https://evil.example/watch?v=dQw4w9WgXcQ",
        "https://youtube.com.evil.example/watch?v=dQw4w9WgXcQ",
        "ftp://www.youtube.com/watch?v=dQw4w9WgXcQ",
    ] {
        assert_eq!(id(url), None, "{url}");
    }
}

const G: &str = "https://rr1---sn-abc.googlevideo.com/videoplayback";

fn format(itag: u32, mime: &str, height: u64, bitrate: u64) -> Value {
    json!({
        "itag": itag,
        "url": format!("{G}?itag={itag}"),
        "mimeType": mime,
        "height": height,
        "bitrate": bitrate,
        "contentLength": (itag as u64 * 1000).to_string(),
    })
}

fn player(formats: Vec<Value>, adaptive: Vec<Value>) -> Value {
    json!({
        "playabilityStatus": { "status": "OK" },
        "videoDetails": { "title": "Clip" },
        "streamingData": { "formats": formats, "adaptiveFormats": adaptive },
    })
}

fn typical() -> Value {
    player(
        vec![format(
            18,
            "video/mp4; codecs=\"avc1.42001E, mp4a.40.2\"",
            360,
            500,
        )],
        vec![
            format(137, "video/mp4; codecs=\"avc1.640028\"", 1080, 4_000_000),
            format(136, "video/mp4; codecs=\"avc1.4d401f\"", 720, 2_000_000),
            format(299, "video/mp4; codecs=\"avc1.64002a\"", 1080, 6_000_000),
            format(264, "video/mp4; codecs=\"avc1.640032\"", 1440, 9_000_000),
            format(399, "video/mp4; codecs=\"av01.0.08M.08\"", 1080, 9_000_000),
            format(248, "video/webm; codecs=\"vp9\"", 1080, 9_000_000),
            format(139, "audio/mp4; codecs=\"mp4a.40.5\"", 0, 48_000),
            format(140, "audio/mp4; codecs=\"mp4a.40.2\"", 0, 128_000),
            format(251, "audio/webm; codecs=\"opus\"", 0, 160_000),
        ],
    )
}

#[test]
fn picks_best_avc1_up_to_1080p_and_aac_audio() {
    let Choice::Split { video, audio } = choose(&typical(), false).unwrap() else {
        panic!("expected split");
    };
    assert_eq!(video.itag, 299);
    assert_eq!(video.mime, "video/mp4");
    assert_eq!(video.length, Some(299_000));
    assert_eq!(audio.itag, 140);
    assert_eq!(audio.url.as_str(), format!("{G}?itag=140"));
    assert_eq!(
        choose(&typical(), true).unwrap(),
        Choice::Audio(audio.clone())
    );
    // Without itag 140, the best AAC-LC wins over HE-AAC even at a lower bitrate.
    let p = player(
        vec![],
        vec![
            format(139, "audio/mp4; codecs=\"mp4a.40.5\"", 0, 90_000),
            format(141, "audio/mp4; codecs=\"mp4a.40.2\"", 0, 64_000),
        ],
    );
    let Choice::Audio(a) = choose(&p, true).unwrap() else {
        panic!()
    };
    assert_eq!(a.itag, 141);
}

#[test]
fn ciphered_and_foreign_formats_are_skipped() {
    let mut p = typical();
    for f in p["streamingData"]["adaptiveFormats"]
        .as_array_mut()
        .unwrap()
    {
        if f["mimeType"].as_str().unwrap().starts_with("audio") {
            let url = f["url"].take();
            f["signatureCipher"] = json!(format!("s=abc&url={url}"));
        }
    }
    // No audio: falls back to progressive itag 18.
    let Choice::Progressive(s) = choose(&p, false).unwrap() else {
        panic!()
    };
    assert_eq!(s.itag, 18);
    assert_eq!(
        choose(&p, true).unwrap_err(),
        "YouTube: no directly downloadable stream (clients tried: ANDROID_VR, IOS, ANDROID)"
    );
    p["streamingData"]["formats"][0]["url"] = json!("https://evil.example/videoplayback");
    p["streamingData"]["hlsManifestUrl"] = json!("https://manifest.googlevideo.com/hls.m3u8");
    assert_eq!(
        choose(&p, false).unwrap(),
        Choice::Hls(Url::parse("https://manifest.googlevideo.com/hls.m3u8").unwrap())
    );
    p["streamingData"]["hlsManifestUrl"] = json!("http://manifest.googlevideo.com/hls.m3u8");
    assert!(choose(&p, false).is_err());
    let mut q = typical();
    q["streamingData"]["adaptiveFormats"][0]["url"] =
        json!("https://googlevideo.com.evil.example/videoplayback");
    let Choice::Split { video, .. } = choose(&q, false).unwrap() else {
        panic!()
    };
    assert_eq!(video.itag, 299);
    assert!(allowed(
        &Url::parse("https://www.youtube.com/x.m3u8").unwrap(),
        None,
        true
    ));
    assert!(!allowed(
        &Url::parse("https://www.youtube.com/videoplayback").unwrap(),
        None,
        false
    ));
}

struct Fixture {
    base: Url,
    seen: Arc<Mutex<Vec<(String, Value)>>>,
    replies: Arc<Mutex<HashMap<u32, (u16, String)>>>,
    handle: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

/// Answers POST /youtubei/v1/player with the reply registered for the client id header.
async fn fixture(replies: HashMap<u32, (u16, String)>) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let replies = Arc::new(Mutex::new(replies));
    let table = replies.clone();
    let handle = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut head = Vec::new();
            let mut byte = [0];
            while !head.ends_with(b"\r\n\r\n") {
                socket.read_exact(&mut byte).await.unwrap();
                head.push(byte[0]);
            }
            let head = String::from_utf8(head).unwrap().to_ascii_lowercase();
            let header = |name: &str| {
                head.lines()
                    .find_map(|l| l.strip_prefix(&format!("{name}: ")))
                    .map(str::to_string)
            };
            let length: usize = header("content-length").unwrap().parse().unwrap();
            let mut body = vec![0; length];
            socket.read_exact(&mut body).await.unwrap();
            let client: u32 = header("x-youtube-client-name").unwrap().parse().unwrap();
            log.lock()
                .unwrap()
                .push((head.clone(), serde_json::from_slice(&body).unwrap()));
            let (status, text) = table
                .lock()
                .unwrap()
                .get(&client)
                .cloned()
                .unwrap_or((500, String::new()));
            let reply = format!(
                "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{text}",
                text.len()
            );
            // The client may hang up early (size limit); keep serving.
            let _ = socket.write_all(reply.as_bytes()).await;
        }
    });
    Fixture {
        base,
        seen,
        replies,
        handle,
    }
}

fn unplayable(reason: &str) -> String {
    json!({ "playabilityStatus": { "status": "LOGIN_REQUIRED", "reason": reason } }).to_string()
}

#[tokio::test]
async fn resolve_uses_android_vr_and_accepts_the_base_host() {
    let fx = fixture(HashMap::new()).await;
    let mut p = typical();
    // Streams on the injected base host are accepted next to googlevideo.com.
    p["streamingData"]["adaptiveFormats"][0]["url"] =
        json!(format!("{}videoplayback?itag=137", fx.base));
    p["streamingData"]["adaptiveFormats"][2]["mimeType"] = json!("video/webm; codecs=\"vp9\"");
    fx.replies.lock().unwrap().insert(28, (200, p.to_string()));
    let resolved = resolve(&reqwest::Client::new(), &fx.base, "dQw4w9WgXcQ", false)
        .await
        .unwrap();
    assert_eq!(resolved.title.as_deref(), Some("Clip"));
    assert_eq!(resolved.user_agent, CLIENTS[0].user_agent);
    let Choice::Split { video, audio } = resolved.choice else {
        panic!()
    };
    assert_eq!(video.itag, 137);
    assert_eq!(video.url.host_str(), fx.base.host_str());
    assert_eq!(audio.itag, 140);
    // The production policy rejects the fixture host.
    let Choice::Split { video, .. } = choose(&p, false).unwrap() else {
        panic!()
    };
    assert_eq!(video.itag, 136);
    let seen = fx.seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    let (head, body) = &seen[0];
    assert!(head.starts_with("post /youtubei/v1/player?prettyprint=false http/1.1"));
    assert!(head.contains("x-youtube-client-version: 1.62.27"));
    assert!(head.contains("user-agent: com.google.android.apps.youtube.vr.oculus/1.62.27"));
    assert_eq!(body["videoId"], "dQw4w9WgXcQ");
    assert_eq!(body["contentCheckOk"], true);
    assert_eq!(body["racyCheckOk"], true);
    let c = &body["context"]["client"];
    assert_eq!(c["clientName"], "ANDROID_VR");
    assert_eq!(c["deviceMake"], "Oculus");
    assert_eq!(c["deviceModel"], "Quest 3");
    assert_eq!(c["androidSdkVersion"], 32);
    assert_eq!(c["osName"], "Android");
    assert_eq!(c["osVersion"], "12L");
    assert_eq!(c["hl"], "en");
    assert_eq!(c["gl"], "US");
}

#[tokio::test]
async fn resolve_falls_back_through_clients() {
    let hls = json!({
        "playabilityStatus": { "status": "OK" },
        "streamingData": { "hlsManifestUrl": "https://manifest.googlevideo.com/api/hls.m3u8" },
    })
    .to_string();
    let mut replies = HashMap::new();
    replies.insert(
        28,
        (200, unplayable("Sign in to confirm you are not a bot")),
    );
    replies.insert(5, (200, hls));
    let fx = fixture(replies).await;
    let client = reqwest::Client::new();
    let resolved = resolve(&client, &fx.base, "dQw4w9WgXcQ", false)
        .await
        .unwrap();
    assert_eq!(
        resolved.choice,
        Choice::Hls(Url::parse("https://manifest.googlevideo.com/api/hls.m3u8").unwrap())
    );
    assert_eq!(resolved.user_agent, CLIENTS[1].user_agent);
    // Audio-only ignores the IOS HLS answer and reaches ANDROID, which fails here.
    let err = resolve(&client, &fx.base, "dQw4w9WgXcQ", true)
        .await
        .unwrap_err();
    assert_eq!(err, no_stream());
    let names: Vec<String> = fx.seen.lock().unwrap()[2..]
        .iter()
        .map(|(_, b)| b["context"]["client"]["clientName"].to_string())
        .collect();
    assert_eq!(names, ["\"ANDROID_VR\"", "\"IOS\"", "\"ANDROID\""]);
    assert!(
        fx.seen.lock().unwrap()[3].1["context"]["client"]
            .get("androidSdkVersion")
            .is_none()
    );
}

#[tokio::test]
async fn resolve_reports_the_playability_reason() {
    let mut replies = HashMap::new();
    for id in [28, 5, 3] {
        replies.insert(id, (200, unplayable("This video is private")));
    }
    let fx = fixture(replies).await;
    let client = reqwest::Client::new();
    let err = resolve(&client, &fx.base, "dQw4w9WgXcQ", false)
        .await
        .unwrap_err();
    assert_eq!(err, "YouTube: This video is private");
    assert_eq!(crate::util::error_code(&err), "error");
    assert!(
        resolve(&client, &fx.base, "bad", false)
            .await
            .unwrap_err()
            .contains("invalid")
    );
}

#[tokio::test]
async fn resolve_rejects_oversized_and_failed_responses() {
    let mut replies = HashMap::new();
    replies.insert(28, (200, " ".repeat(BODY_LIMIT + 1)));
    replies.insert(5, (429, String::new()));
    let fx = fixture(replies).await;
    let err = resolve(&reqwest::Client::new(), &fx.base, "dQw4w9WgXcQ", false)
        .await
        .unwrap_err();
    assert_eq!(err, "YouTube: player response exceeds the 4 MiB limit");
    assert_eq!(fx.seen.lock().unwrap().len(), 3);
}
