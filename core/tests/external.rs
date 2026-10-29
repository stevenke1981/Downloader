use dlcore::task::DownloadKind;
use dlcore::{Ctx, Outcome, RateLimiter, Settings, Task, engine, external, util};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

fn ctx() -> Ctx {
    Ctx {
        protocol_client: dlcore::Settings::default().build_protocol_client().unwrap(),
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
fn protocols_are_embedded_and_never_spawn_external_engines() {
    assert!(
        external::capabilities()
            .iter()
            .all(|c| c.available && c.path.is_none())
    );
    for source in [
        include_str!("../src/engine.rs"),
        include_str!("../src/protocols/mod.rs"),
        include_str!("../src/protocols/ftp.rs"),
        include_str!("../src/protocols/metalink.rs"),
        include_str!("../src/protocols/torrent.rs"),
        include_str!("../src/protocols/media.rs"),
        include_str!("../src/protocols/media/hls.rs"),
        include_str!("../src/protocols/media/mmsh.rs"),
        include_str!("../src/protocols/media/rtsp.rs"),
        include_str!("../src/protocols/media/parse.rs"),
        include_str!("../src/protocols/media/parse/playlist.rs"),
        include_str!("../src/protocols/media/parse/discovery.rs"),
    ] {
        assert!(!source.contains("process::Command"));
        assert!(!source.contains("Command::new"));
        assert!(!source.contains("DOWNLOADER_ARIA2"));
        assert!(!source.contains("DOWNLOADER_FFMPEG"));
        assert!(!source.contains("DOWNLOADER_YTDLP"));
        assert!(!source.contains("DOWNLOADER_CURL"));
    }
}

#[tokio::test]
async fn native_metalink_publication_requires_whole_file_checksum_and_excludes_state() {
    let dir = tempfile::tempdir().unwrap();
    let (url, _, server) = http(None).await;
    let descriptor = dir.path().join("fixture.meta4");
    std::fs::write(&descriptor, format!(
        "<metalink xmlns=\"urn:ietf:params:xml:ns:metalink\"><file name=\"nested/payload.bin\"><size>7</size><url>{url}</url></file></metalink>"
    )).unwrap();
    for (id, expected, success) in [
        (1, "00000000000000000000000000000000", false),
        (2, "321c3cf486ed509164edec1e1981fec8", true),
    ] {
        let output = dir.path().join(format!("output-{id}"));
        let task = Arc::new(Mutex::new(Task {
            id,
            url: descriptor.to_str().unwrap().into(),
            dir: output.clone(),
            suggested_name: Some("fixture.meta4".into()),
            checksum: dlcore::Checksum::guess(expected),
            ..Default::default()
        }));
        let result = engine::run(task.clone(), ctx()).await;
        if success {
            assert_eq!(result.unwrap(), Outcome::Completed);
            let task = task.lock().unwrap();
            assert_eq!(task.file_name, "payload.bin");
            assert_eq!(std::fs::read(task.final_path()).unwrap(), b"payload");
            assert_eq!(
                task.verification.as_ref().unwrap().state,
                dlcore::VerifyState::Verified
            );
            assert!(!external::staging_path(&task).exists());
            assert_eq!(std::fs::read_dir(output).unwrap().count(), 1);
        } else {
            assert!(result.unwrap_err().contains("verification failed"));
            let task = task.lock().unwrap();
            assert_eq!(
                task.verification.as_ref().unwrap().state,
                dlcore::VerifyState::Failed
            );
            assert!(task.file_name.is_empty());
            assert_eq!(
                std::fs::read_dir(output).unwrap().count(),
                1,
                "only staging is retained"
            );
        }
    }
    // A multi-file publication must move only payload, never protocol state.
    std::fs::write(&descriptor, format!(
        "<metalink xmlns=\"urn:ietf:params:xml:ns:metalink\"><file name=\"nested/a.bin\"><size>7</size><url>{url}</url></file><file name=\"b.bin\"><size>7</size><url>{url}</url></file></metalink>"
    )).unwrap();
    let task = Arc::new(Mutex::new(Task {
        id: 3,
        url: descriptor.to_str().unwrap().into(),
        dir: dir.path().join("multi"),
        ..Default::default()
    }));
    assert_eq!(
        engine::run(task.clone(), ctx()).await.unwrap(),
        Outcome::Completed
    );
    let task = task.lock().unwrap();
    assert_eq!(
        std::fs::read(task.final_path().join("nested/a.bin")).unwrap(),
        b"payload"
    );
    assert_eq!(
        std::fs::read(task.final_path().join("b.bin")).unwrap(),
        b"payload"
    );
    assert!(!task.final_path().join("state").exists());
    assert!(!external::staging_path(&task).exists());
    server.abort();
}

#[tokio::test]
async fn native_protocol_requests_retain_configured_http_proxy() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let settings = Settings {
        proxy: format!("http://{}", listener.local_addr().unwrap()),
        ..Default::default()
    };
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let fixture = tokio::spawn(async move {
        for _ in 0..2 {
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
            let request = String::from_utf8(bytes).unwrap();
            let body = if request.starts_with("GET http://native-proxy-fixture.invalid/pkg.meta4 ")
            {
                b"<metalink xmlns=\"urn:ietf:params:xml:ns:metalink\"><file name=\"payload.bin\"><size>7</size><url>http://native-proxy-fixture.invalid/payload.bin</url></file></metalink>".to_vec()
            } else {
                assert!(
                    request.starts_with("GET http://native-proxy-fixture.invalid/payload.bin ")
                );
                b"payload".to_vec()
            };
            seen.lock().unwrap().push(request);
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(header.as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let task = Arc::new(Mutex::new(Task {
        url: "http://native-proxy-fixture.invalid/pkg.meta4".into(),
        dir: dir.path().into(),
        ..Default::default()
    }));
    let mut context = ctx();
    context.client = settings.build_client().unwrap();
    context.protocol_client = settings.build_protocol_client().unwrap();
    assert_eq!(
        engine::run(task.clone(), context).await.unwrap(),
        Outcome::Completed
    );
    assert_eq!(
        std::fs::read(task.lock().unwrap().final_path()).unwrap(),
        b"payload"
    );
    fixture.await.unwrap();
    assert_eq!(requests.lock().unwrap().len(), 2);
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
            let ranged = request.lines().any(|line| line.starts_with("range: "));
            let (body, end) = if probe {
                (&data[..1], 0)
            } else {
                (&data[..], 6)
            };
            let head = if ranged {
                format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes 0-{end}/7\r\nConnection: close\r\n\r\n",
                    body.len()
                )
            } else {
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
            };
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
