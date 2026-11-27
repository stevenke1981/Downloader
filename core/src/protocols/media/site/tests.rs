//! Offline end-to-end site downloads against a local server.
use super::super::{download_with, mp3, mp4mux};
use super::Sites;
use crate::RateLimiter;
use crate::engine::{Ctx, Outcome};
use crate::protocols::sites::youtube::CLIENTS;
use crate::task::{DownloadKind, Task};
use reqwest::Url;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

struct Route {
    body: Vec<u8>,
    /// Answer the first request with HTTP 500.
    flaky: AtomicBool,
}

/// Request line, Range header and User-Agent of one request.
type Seen = (String, Option<String>, Option<String>);

struct Server {
    base: Url,
    routes: Arc<Mutex<HashMap<String, Arc<Route>>>>,

    seen: Arc<Mutex<Vec<Seen>>>,
    handle: tokio::task::JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl Server {
    fn route(&self, path: &str, body: impl Into<Vec<u8>>, flaky: bool) {
        self.routes.lock().unwrap().insert(
            path.into(),
            Arc::new(Route {
                body: body.into(),
                flaky: AtomicBool::new(flaky),
            }),
        );
    }
    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base, path.trim_start_matches('/'))
    }
}

async fn server() -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let routes: Arc<Mutex<HashMap<String, Arc<Route>>>> = Default::default();
    let seen: Arc<Mutex<Vec<_>>> = Default::default();
    let (table, log) = (routes.clone(), seen.clone());
    let handle = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let (table, log) = (table.clone(), log.clone());
            tokio::spawn(async move {
                let mut head = Vec::new();
                let mut byte = [0];
                while !head.ends_with(b"\r\n\r\n") {
                    if socket.read_exact(&mut byte).await.is_err() {
                        return;
                    }
                    head.push(byte[0]);
                }
                let head = String::from_utf8(head).unwrap();
                let header = |name: &str| {
                    head.lines().find_map(|l| {
                        let (k, v) = l.split_once(": ")?;
                        k.eq_ignore_ascii_case(name).then(|| v.to_string())
                    })
                };
                let length: usize = header("content-length").map_or(0, |v| v.parse().unwrap());
                let mut body = vec![0; length];
                socket.read_exact(&mut body).await.unwrap();
                let line = head.lines().next().unwrap().to_string();
                let range = header("range");
                log.lock()
                    .unwrap()
                    .push((line.clone(), range.clone(), header("user-agent")));
                let path = line.split_whitespace().nth(1).unwrap();
                let path = path.split('?').next().unwrap();
                let route = table.lock().unwrap().get(path).cloned();
                let reply = match route {
                    None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_vec(),
                    Some(route) if route.flaky.swap(false, Ordering::SeqCst) => {
                        b"HTTP/1.1 500 Oops\r\nContent-Length: 0\r\n\r\n".to_vec()
                    }
                    Some(route) => {
                        let total = route.body.len();
                        let (status, extra, data) = match range {
                            Some(range) => {
                                let (a, b) = range
                                    .strip_prefix("bytes=")
                                    .unwrap()
                                    .split_once('-')
                                    .unwrap();
                                let (a, b): (usize, usize) =
                                    (a.parse().unwrap(), b.parse().unwrap());
                                (
                                    "206 Partial Content",
                                    format!("Content-Range: bytes {a}-{b}/{total}\r\n"),
                                    &route.body[a..=b],
                                )
                            }
                            None => ("200 OK", String::new(), &route.body[..]),
                        };
                        let mut reply = format!(
                            "HTTP/1.1 {status}\r\n{extra}Content-Length: {}\r\nConnection: close\r\n\r\n",
                            data.len()
                        )
                        .into_bytes();
                        reply.extend_from_slice(data);
                        reply
                    }
                };
                let _ = socket.write_all(&reply).await;
            });
        }
    });
    Server {
        base,
        routes,
        seen,
        handle,
    }
}

struct Run {
    stage: tempfile::TempDir,
    task: Arc<Mutex<Task>>,
    result: Result<Outcome, String>,
}

impl Run {
    fn payload(&self) -> Vec<(String, Vec<u8>)> {
        let mut files: Vec<_> = std::fs::read_dir(self.stage.path().join("payload"))
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                (
                    e.file_name().to_string_lossy().into_owned(),
                    std::fs::read(e.path()).unwrap(),
                )
            })
            .collect();
        files.sort();
        files
    }
}

async fn start(server: &Server, url: &str, kind: DownloadKind) -> Run {
    let stage = tempfile::tempdir().unwrap();
    let task = Arc::new(Mutex::new(Task {
        url: url.into(),
        kind,
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
    let sites = Sites {
        youtube: server.base.clone(),
        x: server.base.clone(),
        chunk: 64,
    };
    let result = download_with(task.clone(), ctx, stage.path(), &sites).await;
    Run {
        stage,
        task,
        result,
    }
}

const WATCH: &str = "https://www.youtube.com/watch?v=dQw4w9WgXcQ";

fn player(server: &Server, video: &[u8], audio: &[u8], progressive: &[u8]) -> Value {
    let format = |itag: u32, path: &str, mime: &str, len: usize| {
        json!({
            "itag": itag,
            "url": server.url(path),
            "mimeType": mime,
            "height": 720,
            "bitrate": 1000,
            "contentLength": len.to_string(),
        })
    };
    json!({
        "playabilityStatus": { "status": "OK" },
        "videoDetails": { "title": "My Clip: part/1" },
        "streamingData": {
            "formats": [format(18, "/p.mp4", "video/mp4; codecs=\"avc1.42001E, mp4a.40.2\"", progressive.len())],
            "adaptiveFormats": [
                format(136, "/v.mp4", "video/mp4; codecs=\"avc1.4d401f\"", video.len()),
                format(140, "/a.m4a", "audio/mp4; codecs=\"mp4a.40.2\"", audio.len()),
            ],
        },
    })
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn fragmented_video() -> Vec<u8> {
    mp4mux::tests::video_file(&mp4mux::tests::video_frags())
}

#[tokio::test]
async fn youtube_split_streams_are_muxed_and_named_after_the_title() {
    let server = server().await;
    let (video, audio) = (fragmented_video(), mp3::tests::fragmented_silence());
    server.route(
        "/youtubei/v1/player",
        player(&server, &video, &audio, b"").to_string(),
        false,
    );
    server.route("/v.mp4", video.clone(), false);
    server.route("/a.m4a", audio.clone(), true);
    let run = start(&server, WATCH, DownloadKind::Auto).await;
    assert_eq!(run.result.as_ref().unwrap(), &Outcome::Completed);
    let files = run.payload();
    assert_eq!(files.len(), 1);
    let (name, out) = &files[0];
    assert_eq!(name, "My Clip_ part_1.mp4");
    let top = mp4mux::boxes(out, 0, out.len()).unwrap();
    assert_eq!(&top[0].kind, b"ftyp");
    assert_eq!(&top[1].kind, b"moov");
    assert!(contains(out, b"vide") && contains(out, b"soun"));
    assert!(contains(out, b"video-1-large"));
    assert_eq!(run.task.lock().unwrap().total, Some(out.len() as u64));

    let seen = server.seen.lock().unwrap();
    let media: Vec<_> = seen
        .iter()
        .filter(|(l, _, _)| l.starts_with("GET "))
        .collect();
    let expected = video.len().div_ceil(64) + audio.len().div_ceil(64) + 1;
    assert_eq!(media.len(), expected, "one retried audio range");
    assert!(media.iter().all(|(_, range, _)| range.is_some()));
    assert!(
        media
            .iter()
            .all(|(_, _, agent)| agent.as_deref() == Some(CLIENTS[0].user_agent))
    );
}

#[tokio::test]
async fn youtube_falls_back_to_progressive_when_streams_are_not_fragmented() {
    let server = server().await;
    let plain = [
        &[0, 0, 0, 16][..],
        b"ftypisom",
        &[0, 0, 0, 0],
        &[0, 0, 0, 16],
        b"moov",
        &[0, 0, 0, 8],
        b"free",
    ]
    .concat();
    let progressive = [
        &[0, 0, 0, 12][..],
        b"ftypisom",
        &[0; 4],
        b"progressive data",
    ]
    .concat();
    let audio = mp3::tests::fragmented_silence();
    let json = player(&server, &plain, &audio, &progressive);
    server.route("/youtubei/v1/player", json.to_string(), false);
    server.route("/v.mp4", plain, false);
    server.route("/a.m4a", audio, false);
    server.route("/p.mp4", progressive.clone(), false);
    let run = start(&server, WATCH, DownloadKind::Video).await;
    run.result.as_ref().unwrap();
    assert_eq!(
        run.payload(),
        [("My Clip_ part_1.mp4".to_string(), progressive)]
    );
    assert!(
        run.task
            .lock()
            .unwrap()
            .log
            .iter()
            .any(|l| l.contains("not fragmented"))
    );
}

#[tokio::test]
async fn youtube_audio_mode_produces_mp3() {
    let server = server().await;
    let audio = mp3::tests::fragmented_silence();
    let json = player(&server, &fragmented_video(), &audio, b"");
    server.route("/youtubei/v1/player", json.to_string(), false);
    server.route("/a.m4a", audio, false);
    let run = start(&server, "https://youtu.be/dQw4w9WgXcQ", DownloadKind::Audio).await;
    run.result.as_ref().unwrap();
    let files = run.payload();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].0, "My Clip_ part_1.mp3");
    let mp3 = &files[0].1;
    assert!(mp3.starts_with(b"ID3") || (mp3[0] == 0xff && mp3[1] & 0xe0 == 0xe0));
    let seen = server.seen.lock().unwrap();
    assert!(!seen.iter().any(|(l, _, _)| l.contains("/v.mp4")));
    assert!(!run.stage.path().join("state/media.mp3.part").exists());
}

#[tokio::test]
async fn audio_mode_rejects_other_sites() {
    let server = server().await;
    let run = start(&server, "https://example.com/song.m4a", DownloadKind::Audio).await;
    assert_eq!(
        run.result.unwrap_err(),
        "audio download supports YouTube links only"
    );
    let run = start(
        &server,
        "https://x.com/someone/status/20",
        DownloadKind::Audio,
    )
    .await;
    assert_eq!(
        run.result.unwrap_err(),
        "audio download supports YouTube links only"
    );
    assert!(server.seen.lock().unwrap().is_empty());
}

#[tokio::test]
async fn x_post_video_is_downloaded_and_named() {
    let server = server().await;
    let clip = [&[0, 0, 0, 12][..], b"ftypmp42", &[0; 4], b"x video bytes"].concat();
    let tweet = json!({
        "__typename": "Tweet",
        "text": "Hello world https://t.co/abc",
        "mediaDetails": [{
            "type": "video",
            "video_info": { "variants": [
                { "content_type": "video/mp4", "bitrate": 832000, "url": server.url("/x.mp4") },
            ]},
        }],
    });
    server.route("/tweet-result", tweet.to_string(), false);
    server.route("/x.mp4", clip.clone(), false);
    let run = start(
        &server,
        "https://x.com/someone/status/1234567890123456789",
        DownloadKind::Auto,
    )
    .await;
    run.result.as_ref().unwrap();
    assert_eq!(run.payload(), [("Hello world.mp4".to_string(), clip)]);
    let seen = server.seen.lock().unwrap();
    assert!(
        seen[0]
            .0
            .starts_with("GET /tweet-result?id=1234567890123456789")
    );
}

#[test]
fn production_endpoints_are_the_public_apis() {
    let sites = Sites::default();
    assert_eq!(sites.youtube.as_str(), "https://www.youtube.com/");
    assert_eq!(sites.x.as_str(), "https://cdn.syndication.twimg.com/");
    assert_eq!(sites.chunk, 10 << 20);
}
