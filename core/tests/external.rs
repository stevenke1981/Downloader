use dlcore::task::DownloadKind;
use dlcore::{Ctx, Outcome, RateLimiter, Settings, Task, engine, external, util};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

fn ctx() -> Ctx {
    Ctx {
        client: Settings::default().build_client().unwrap(),
        global: Arc::new(RateLimiter::new(0)),
        local: Arc::new(RateLimiter::new(0)),
        retries: 0,
        cancel: CancellationToken::new(),
        verify: false,
    }
}

#[test]
fn sources_and_ephemeral_credentials_are_validated_at_the_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let descriptor = dir.path().join("中文 payload.torrent");
    std::fs::write(&descriptor, b"test descriptor").unwrap();
    assert!(util::validate_source(descriptor.to_str().unwrap()).is_ok());
    let file_url = reqwest::Url::from_file_path(&descriptor).unwrap();
    assert!(util::validate_source(file_url.as_str()).is_ok());
    assert!(util::validate_source(dir.path().join("missing.torrent").to_str().unwrap()).is_err());
    let ordinary = dir.path().join("ordinary.txt");
    std::fs::write(&ordinary, b"not a descriptor").unwrap();
    assert!(util::validate_source(ordinary.to_str().unwrap()).is_err());
    for source in [
        "ftp://user:secret@example.com/file",
        "http://user:secret@example.com/file",
        "file:///missing.txt",
        "javascript:alert(1)",
    ] {
        assert!(util::validate_source(source).is_err(), "{source}");
    }
    let mut task = Task {
        authorization: Some("Basic c2VjcmV0".into()),
        kind: DownloadKind::Video,
        ..Default::default()
    };
    for n in 0..150 {
        task.append_log(format!("event {n}"));
    }
    let json = serde_json::to_string(&task).unwrap();
    assert!(!json.contains("c2VjcmV0") && !json.contains("authorization"));
    assert_eq!(task.log.len(), 100);
    assert_eq!(task.log[0], "event 50");
    assert_eq!(
        serde_json::from_str::<Task>("{}").unwrap().kind,
        DownloadKind::Auto
    );
    assert_ne!(
        external::staging_path(&task),
        external::staging_path(&Task {
            kind: DownloadKind::Auto,
            ..task
        })
    );
}

#[test]
fn native_child_output_exit_code_and_checksums_must_agree() {
    // A native executable exercises actual spawn/argv/exit handling without third-party tools.
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("fixture.rs");
    let program = dir
        .path()
        .join(format!("fixture{}", std::env::consts::EXE_SUFFIX));
    std::fs::write(&source, r#"
        fn main() {
            let args: Vec<_> = std::env::args_os().collect();
            let index = args.iter().position(|arg| arg == "--dir").unwrap();
            let dir = std::path::PathBuf::from(&args[index + 1]);
            match std::env::var("DLX_FIXTURE_MODE").unwrap().as_str() {
                "empty" => {},
                "partial" => { std::fs::write(dir.join("payload.part"), b"unfinished").unwrap(); },
                "failed" => { std::fs::write(dir.join("payload.bin"), b"unfinished").unwrap(); std::process::exit(7); },
                "multi" | "multi-checksum" => {
                    std::fs::create_dir_all(dir.join("nested")).unwrap();
                    std::fs::write(dir.join("nested/a.bin"), b"first").unwrap();
                    std::fs::write(dir.join("nested/b.bin"), b"second").unwrap();
                },
                _ => { std::fs::write(dir.join("payload.bin"), b"fixture payload\n").unwrap(); },
            }
        }
    "#).unwrap();
    let compiler = Command::new("rustc")
        .arg("--edition=2024")
        .arg(&source)
        .arg("-o")
        .arg(&program)
        .output()
        .unwrap();
    assert!(
        compiler.status.success(),
        "{}",
        String::from_utf8_lossy(&compiler.stderr)
    );
    for mode in [
        "empty",
        "partial",
        "failed",
        "single",
        "multi",
        "mismatch",
        "multi-checksum",
    ] {
        let output_dir = dir.path().join(mode);
        let output = Command::new(env!("CARGO_BIN_EXE_dlx"))
            .args(["ftp://example.invalid/payload.bin", "--quiet", "--verify"])
            .arg(if matches!(mode, "mismatch" | "multi-checksum") {
                "00000000000000000000000000000000"
            } else {
                "off"
            })
            .arg("-o")
            .arg(&output_dir)
            .env("DOWNLOADER_ARIA2", &program)
            .env("DLX_FIXTURE_MODE", mode)
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        if matches!(mode, "single" | "multi") {
            assert!(output.status.success(), "{mode}: {stderr}");
            let published = Path::new(stdout.trim().split("  ").next().unwrap());
            assert!(
                published.exists(),
                "CLI must report the actual published path: {stdout}"
            );
            if mode == "multi" {
                assert!(published.is_dir());
                assert_eq!(
                    std::fs::read(published.join("nested/a.bin")).unwrap(),
                    b"first"
                );
                assert_eq!(
                    std::fs::read(published.join("nested/b.bin")).unwrap(),
                    b"second"
                );
            } else {
                assert_eq!(std::fs::read(published).unwrap(), b"fixture payload\n");
            }
        } else {
            assert!(!output.status.success(), "{mode} must never complete");
            assert!(!output_dir.join("payload.bin").exists());
            if mode == "empty" || mode == "partial" {
                assert!(stderr.contains("no completed output"), "{stderr}");
            }
            if mode == "mismatch" {
                assert!(stderr.contains("verification failed"), "{stderr}");
            }
            if mode == "failed" {
                assert!(stderr.contains("failed"), "{stderr}");
            }
            if mode == "multi-checksum" {
                assert!(
                    stderr.contains("whole-file checksum cannot verify a multi-file"),
                    "{stderr}"
                );
            }
        }
    }
}

async fn http(
    redirect: Option<String>,
) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/payload.bin", listener.local_addr().unwrap());
    let authorization = Arc::new(Mutex::new(Vec::new()));
    let values = authorization.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 1024];
            while !bytes.windows(4).any(|b| b == b"\r\n\r\n") {
                let n = socket.read(&mut buffer).await.unwrap();
                if n == 0 {
                    break;
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
            let request = String::from_utf8_lossy(&bytes);
            values.lock().unwrap().push(
                request
                    .lines()
                    .find_map(|line| line.strip_prefix("authorization: ").map(str::to_string))
                    .unwrap_or_default(),
            );
            if let Some(redirect) = &redirect {
                let reply = format!(
                    "HTTP/1.1 302 Found\r\nLocation: {redirect}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                socket.write_all(reply.as_bytes()).await.unwrap();
                continue;
            }
            let data = b"payload";
            let probe = request.lines().any(|line| line == "range: bytes=0-0");
            let (body, end) = if probe {
                (&data[..1], 0)
            } else {
                (&data[..], 6)
            };
            let head = format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes 0-{end}/7\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).await.unwrap();
            socket.write_all(body).await.unwrap();
        }
    });
    (url, authorization, server)
}

#[tokio::test]
async fn basic_auth_applies_to_probe_and_segments_only_at_original_origin() {
    let dir = tempfile::tempdir().unwrap();
    let (url, authorization, server) = http(None).await;
    let task = Arc::new(Mutex::new(Task {
        url,
        dir: dir.path().into(),
        authorization: Some("Basic dXNlcjpwYXNz".into()),
        ..Default::default()
    }));
    assert_eq!(engine::run(task, ctx()).await.unwrap(), Outcome::Completed);
    assert_eq!(
        *authorization.lock().unwrap(),
        ["Basic dXNlcjpwYXNz", "Basic dXNlcjpwYXNz"]
    );
    server.abort();
    let (target, target_auth, target_server) = http(None).await;
    let (origin, origin_auth, origin_server) = http(Some(target)).await;
    let task = Arc::new(Mutex::new(Task {
        url: origin,
        dir: dir.path().into(),
        authorization: Some("Basic dXNlcjpwYXNz".into()),
        ..Default::default()
    }));
    assert_eq!(engine::run(task, ctx()).await.unwrap(), Outcome::Completed);
    assert_eq!(*origin_auth.lock().unwrap(), ["Basic dXNlcjpwYXNz"]);
    assert_eq!(*target_auth.lock().unwrap(), ["", ""]);
    origin_server.abort();
    target_server.abort();
}

#[tokio::test]
#[ignore = "requires a real FFmpeg executable on PATH or DOWNLOADER_FFMPEG"]
async fn real_ffmpeg_rtsp_child_is_cancelled_and_waited() {
    assert!(
        external::capabilities()
            .iter()
            .any(|tool| tool.name == "ffmpeg" && tool.available)
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("rtsp://{}/live", listener.local_addr().unwrap());
    let dir = tempfile::tempdir().unwrap();
    let task = Arc::new(Mutex::new(Task {
        url,
        dir: dir.path().into(),
        ..Default::default()
    }));
    let context = ctx();
    let cancel = context.cancel.clone();
    let job = tokio::spawn(engine::run(task.clone(), context));
    let (mut control, _) = tokio::time::timeout(Duration::from_secs(5), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut request = [0; 4096];
    let size = tokio::time::timeout(Duration::from_secs(5), control.read(&mut request))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&request[..size]).starts_with("OPTIONS rtsp://"));
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), job)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Outcome::Cancelled
    );
    let closed = tokio::time::timeout(Duration::from_secs(5), control.read(&mut request))
        .await
        .unwrap();
    assert!(
        closed.is_err() || closed.unwrap() == 0,
        "FFmpeg control socket must close before cancellation returns"
    );
    assert!(task.lock().unwrap().file_name.is_empty());
    assert!(
        task.lock()
            .unwrap()
            .log
            .iter()
            .any(|line| line.contains("stopped"))
    );
}

#[tokio::test]
#[ignore = "requires a real FFmpeg executable on PATH or DOWNLOADER_FFMPEG"]
async fn real_ffmpeg_captures_finite_rtsp_rtp_audio() {
    assert!(
        external::capabilities()
            .iter()
            .any(|tool| tool.name == "ffmpeg" && tool.available)
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("rtsp://{}/live", listener.local_addr().unwrap());
    let fixture_url = url.clone();
    let fixture = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        loop {
            let mut request = Vec::new();
            let mut byte = [0];
            while !request.ends_with(b"\r\n\r\n") {
                if socket.read(&mut byte).await.unwrap() == 0 {
                    return;
                }
                request.push(byte[0]);
            }
            let request = String::from_utf8(request).unwrap();
            let sequence = request
                .lines()
                .find_map(|line| line.strip_prefix("CSeq: "))
                .unwrap();
            let method = request.split_whitespace().next().unwrap();
            let mut reply = format!("RTSP/1.0 200 OK\r\nCSeq: {sequence}\r\n");
            match method {
                "OPTIONS" => reply.push_str("Public: OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN\r\n\r\n"),
                "DESCRIBE" => {
                    let sdp = "v=0\r\no=- 0 0 IN IP4 127.0.0.1\r\ns=Downloader fixture\r\nt=0 0\r\na=control:*\r\nm=audio 0 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000/1\r\na=control:trackID=0\r\n";
                    reply.push_str(&format!("Content-Base: {fixture_url}/\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{sdp}", sdp.len()));
                }
                "SETUP" => reply.push_str("Session: 12345678\r\nTransport: RTP/AVP/TCP;unicast;interleaved=0-1\r\n\r\n"),
                "PLAY" => reply.push_str(&format!("Session: 12345678\r\nRange: npt=0-\r\nRTP-Info: url={fixture_url}/trackID=0;seq=0;rtptime=0\r\n\r\n")),
                _ => reply.push_str("Session: 12345678\r\n\r\n"),
            }
            socket.write_all(reply.as_bytes()).await.unwrap();
            if method == "PLAY" {
                for sequence in 0u16..400 {
                    let mut packet = vec![b'$', 0, 0, 172, 0x80, 0];
                    packet.extend_from_slice(&sequence.to_be_bytes());
                    packet.extend_from_slice(&(u32::from(sequence) * 160).to_be_bytes());
                    packet.extend_from_slice(&1u32.to_be_bytes());
                    packet.extend_from_slice(&[0x80; 160]);
                    if socket.write_all(&packet).await.is_err() {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                socket.shutdown().await.unwrap();
                return;
            }
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let task = Arc::new(Mutex::new(Task {
        url,
        dir: dir.path().into(),
        ..Default::default()
    }));
    let context = ctx();
    let cancel = context.cancel.clone();
    let mut job = tokio::spawn(engine::run(task.clone(), context));
    let result = tokio::select! {
        result = &mut job => result.unwrap(),
        _ = tokio::time::sleep(Duration::from_secs(15)) => {
            cancel.cancel();
            let _ = job.await;
            panic!("finite RTSP capture timed out");
        }
    };
    fixture.await.unwrap();
    assert_eq!(result.unwrap(), Outcome::Completed);
    let task = task.lock().unwrap();
    let bytes = std::fs::read(task.final_path()).unwrap();
    assert!(
        bytes.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]),
        "capture must contain Matroska EBML"
    );
    assert!(bytes.len() > 32000);
    assert_eq!(task.total, Some(bytes.len() as u64));
}
