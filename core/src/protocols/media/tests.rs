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
                if path != "/ignore-range"
                    && let Some(value) = header.lines().find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("range: ")
                            .map(str::to_owned)
                    })
                {
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

fn transport_segment(sequence: u8) -> Vec<u8> {
    // A real MPEG-TS program: PAT -> PMT -> AAC PES, including MPEG-2
    // section CRCs, continuity counters, PCR and PTS (no external encoder).
    let mut packets = Vec::new();
    for (pid, mut section) in [
        (0u16, vec![0, 0xb0, 13, 0, 1, 0xc1, 0, 0, 0, 1, 0xe1, 0]),
        (
            0x100,
            vec![
                2, 0xb0, 18, 0, 1, 0xc1, 0, 0, 0xe1, 1, 0xf0, 0, 0x0f, 0xe1, 1, 0xf0, 0,
            ],
        ),
    ] {
        let mut crc = 0xffff_ffffu32;
        for byte in &section {
            crc ^= (*byte as u32) << 24;
            for _ in 0..8 {
                crc = (crc << 1)
                    ^ if crc & 0x8000_0000 != 0 {
                        0x04c1_1db7
                    } else {
                        0
                    };
            }
        }
        section.extend_from_slice(&crc.to_be_bytes());
        let mut packet = vec![0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x10 | sequence, 0];
        packet.extend_from_slice(&section);
        packet.resize(188, 0xff);
        packets.extend_from_slice(&packet);
    }
    let pts = sequence as u64 * 4096 * 90_000 / 44_100;
    let audio = &adts()[..52];
    let mut pes = vec![0, 0, 1, 0xc0];
    pes.extend_from_slice(&((audio.len() + 8) as u16).to_be_bytes());
    pes.extend_from_slice(&[0x80, 0x80, 5]);
    pes.extend_from_slice(&[
        0x21 | (((pts >> 30) as u8 & 7) << 1),
        (pts >> 22) as u8,
        ((pts >> 14) as u8 & 0xfe) | 1,
        (pts >> 7) as u8,
        ((pts << 1) as u8) | 1,
    ]);
    pes.extend_from_slice(audio);
    let adaptation = 183 - pes.len();
    let mut packet = vec![0x47, 0x41, 1, 0x30 | sequence, adaptation as u8, 0x10];
    packet.extend_from_slice(&[
        (pts >> 25) as u8,
        (pts >> 17) as u8,
        (pts >> 9) as u8,
        (pts >> 1) as u8,
        ((pts & 1) << 7) as u8 | 0x7e,
        0,
    ]);
    packet.resize(5 + adaptation, 0xff);
    packet.extend_from_slice(&pes);
    assert_eq!(packet.len(), 188);
    packets.extend_from_slice(&packet);
    packets
}

#[tokio::test]
async fn hls_transport_stream_live_cancel_and_safe_restart() {
    let first = transport_segment(0);
    let second = transport_segment(1);
    let fixture = http_fixture(HashMap::from([
        ("/live.m3u8".into(), reply("application/vnd.apple.mpegurl", b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:0.093,\nfirst.ts\n".to_vec())),
        ("/vod.m3u8".into(), reply("application/vnd.apple.mpegurl", b"#EXTM3U\n#EXT-X-TARGETDURATION:1\n#EXTINF:0.093,\nfirst.ts\n#EXTINF:0.093,\nsecond.ts\n#EXT-X-ENDLIST\n".to_vec())),
        ("/first.ts".into(), reply("video/mp2t", first.clone())),
        ("/second.ts".into(), reply("video/mp2t", second.clone())),
    ])).await;
    let (stage, task, ctx) = environment(format!("{}/live.m3u8", fixture.base));
    let cancel = ctx.cancel.clone();
    let result = tokio::join!(download(task.clone(), ctx, stage.path()), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        cancel.cancel();
    });
    assert_eq!(result.0.unwrap(), Outcome::Cancelled);
    assert_eq!(task.lock().unwrap().downloaded(), first.len() as u64);
    assert_eq!(
        std::fs::read_dir(stage.path().join("payload"))
            .unwrap()
            .count(),
        0
    );
    let (_, task, ctx) = environment(format!("{}/vod.m3u8", fixture.base));
    assert_eq!(
        download(task, ctx, stage.path()).await.unwrap(),
        Outcome::Completed
    );
    let output = std::fs::read(stage.path().join("payload/media.ts")).unwrap();
    assert_eq!(output, [first, second].concat());
    let (packets, remainder) = output.as_chunks::<188>();
    assert!(remainder.is_empty());
    assert!(packets.iter().all(|packet| packet[0] == 0x47));
    let digest = hex::encode(Sha256::digest(&output));
    println!("native HLS MPEG-TS fixture SHA-256 {digest}");
}

#[tokio::test]
async fn browser_playlist_name_preserves_actual_output_format() {
    let expected = transport_segment(0);
    let fixture = http_fixture(HashMap::from([
        (
            "/stream.m3u8".into(),
            reply(
                "application/vnd.apple.mpegurl",
                b"#EXTM3U\n#EXTINF:0.093,\nfirst.ts\n#EXT-X-ENDLIST\n".to_vec(),
            ),
        ),
        ("/first.ts".into(), reply("video/mp2t", expected.clone())),
    ]))
    .await;
    let directory = tempfile::tempdir().unwrap();
    let (_, task, ctx) = environment(format!("{}/stream.m3u8", fixture.base));
    {
        let mut task = task.lock().unwrap();
        task.dir = directory.path().to_owned();
        task.suggested_name = Some("browser-title.m3u8".into());
    }
    assert_eq!(
        crate::protocols::run(task.clone(), ctx).await.unwrap(),
        Outcome::Completed
    );
    let task = task.lock().unwrap();
    assert_eq!(task.file_name, "browser-title.ts");
    assert_eq!(std::fs::read(task.final_path()).unwrap(), expected);
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
}

#[tokio::test]
async fn hls_packed_id3_aac_is_not_mislabeled_mp3() {
    let mut private = b"com.apple.streaming.transportStreamTimestamp\0".to_vec();
    private.extend_from_slice(&0u64.to_be_bytes());
    let mut tag = b"PRIV".to_vec();
    tag.extend_from_slice(&[0, 0, 0, private.len() as u8, 0, 0]);
    tag.extend_from_slice(&private);
    let mut audio = b"ID3\x04\x00\x00".to_vec();
    audio.extend_from_slice(&[0, 0, 0, tag.len() as u8]);
    audio.extend_from_slice(&tag);
    audio.extend_from_slice(&adts());
    let fixture = http_fixture(HashMap::from([
        (
            "/packed.m3u8".into(),
            reply(
                "application/vnd.apple.mpegurl",
                b"#EXTM3U\n#EXTINF:0.465,\naudio.aac\n#EXT-X-ENDLIST\n".to_vec(),
            ),
        ),
        ("/audio.aac".into(), reply("audio/aac", audio.clone())),
    ]))
    .await;
    let (stage, task, ctx) = environment(format!("{}/packed.m3u8", fixture.base));
    assert_eq!(
        download(task, ctx, stage.path()).await.unwrap(),
        Outcome::Completed
    );
    assert_eq!(
        std::fs::read(stage.path().join("payload/media.aac")).unwrap(),
        audio
    );
}

#[tokio::test]
async fn rtsp_handshake_and_mmsh_body_waits_cancel_promptly() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (stage, task, ctx) =
        environment(format!("rtsp://{}/stall", listener.local_addr().unwrap()));
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        assert!(request_header(&mut socket).await.starts_with("DESCRIBE"));
        std::future::pending::<()>().await;
    });
    let cancel = ctx.cancel.clone();
    let started = std::time::Instant::now();
    let result = tokio::join!(download(task, ctx, stage.path()), async {
        tokio::time::sleep(Duration::from_millis(80)).await;
        cancel.cancel();
    });
    assert_eq!(result.0.unwrap(), Outcome::Cancelled);
    assert!(started.elapsed() < Duration::from_millis(500));
    server.abort();
    let fixture = http_fixture(HashMap::from([(
        "/stall".into(),
        Reply {
            status: "200 OK",
            headers: "Content-Type: application/x-mms-framed\r\n".into(),
            body: Vec::new(),
            stall: true,
        },
    )]))
    .await;
    let (stage, task, ctx) = environment(fixture.base.replacen("http:", "mmsh:", 1) + "/stall");
    let cancel = ctx.cancel.clone();
    let started = std::time::Instant::now();
    let result = tokio::join!(download(task, ctx, stage.path()), async {
        tokio::time::sleep(Duration::from_millis(80)).await;
        cancel.cancel();
    });
    assert_eq!(result.0.unwrap(), Outcome::Cancelled);
    assert!(started.elapsed() < Duration::from_millis(500));
    assert_eq!(
        std::fs::read_dir(stage.path().join("payload"))
            .unwrap()
            .count(),
        0
    );
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
async fn rtsp_pcmu_and_pcma_rtp_decode_to_valid_pcm_wav() {
    for (codec, payload_type, encoded, decoded) in [
        (
            "PCMU",
            0u8,
            [0xff, 0x7f, 0x00, 0x80, 0xfe, 0x7e, 0x55, 0xd5],
            [0i16, 0, -32124, 32124, 8, -8, -716, 716],
        ),
        (
            "PCMA",
            8u8,
            [0xd5, 0x55, 0xaa, 0x2a, 0x80, 0x00, 0xd4, 0x54],
            [8i16, -8, 32256, -32256, 5504, -5504, 24, -24],
        ),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("rtsp://{}/audio/", listener.local_addr().unwrap());
        let base = url.clone();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            for method in ["DESCRIBE", "SETUP", "PLAY"] {
                let request = request_header(&mut socket).await;
                assert!(request.starts_with(method));
                let cseq = request
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("cseq: ")
                            .map(str::to_owned)
                    })
                    .unwrap();
                let (extra,body)=match method {
                    "DESCRIBE"=>(format!("Content-Type: application/sdp\r\nContent-Base: {base}\r\n"),format!("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=G711 fixture\r\nt=0 0\r\na=control:*\r\nm=audio 0 RTP/AVP {payload_type}\r\na=rtpmap:{payload_type} {codec}/8000/1\r\na=control:track1\r\n")),
                    "SETUP"=>("Session: audio;timeout=60\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1;ssrc=12345678\r\n".into(),String::new()),
                    _=>(format!("Session: audio\r\nRange: npt=0-\r\nRTP-Info: url={base}track1;seq=500;rtptime=0\r\n"),String::new()),
                };
                socket.write_all(format!("RTSP/1.0 200 OK\r\nCSeq: {cseq}\r\n{extra}Content-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
            }
            for index in 0..2u16 {
                let mut packet = vec![0x80, payload_type];
                packet.extend_from_slice(&(500 + index).to_be_bytes());
                packet.extend_from_slice(&(index as u32 * 8).to_be_bytes());
                packet.extend_from_slice(&0x12345678u32.to_be_bytes());
                packet.extend_from_slice(&encoded);
                let mut frame = vec![b'$', 0];
                frame.extend_from_slice(&(packet.len() as u16).to_be_bytes());
                frame.extend_from_slice(&packet);
                socket.write_all(&frame).await.unwrap();
            }
            socket
                .write_all(&[b'$', 1, 0, 8, 0x81, 203, 0, 1, 0x12, 0x34, 0x56, 0x78])
                .await
                .unwrap();
            let mut buffer = [0; 8];
            let _ = socket.read(&mut buffer).await;
        });
        let (stage, task, ctx) = environment(url);
        assert_eq!(
            tokio::time::timeout(
                Duration::from_secs(5),
                download(task.clone(), ctx, stage.path())
            )
            .await
            .unwrap()
            .unwrap(),
            Outcome::Completed
        );
        let output = std::fs::read(stage.path().join("payload/media.wav")).unwrap();
        let mut expected = wav(16);
        let samples: Vec<u8> = decoded
            .repeat(2)
            .into_iter()
            .flat_map(i16::to_le_bytes)
            .collect();
        expected[44..].copy_from_slice(&samples);
        assert_eq!(output, expected, "{codec}");
        assert_eq!(task.lock().unwrap().total, Some(76));
        println!(
            "native RTSP {codec} WAV fixture SHA-256 {}",
            hex::encode(Sha256::digest(&output))
        );
        server.await.unwrap();
    }
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
    extension[24..40].copy_from_slice(&hex::decode("11d2d3abbaa9cf118ee600c00c205365").unwrap());
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
    let (stage, task, ctx) = environment(fixture.base.replacen("http:", "mmsh:", 1) + "/stream");
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
        let (stage, task, ctx) = environment(fixture.base.replacen("http:", "mms:", 1) + "/stream");
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
