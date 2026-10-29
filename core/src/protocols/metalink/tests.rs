
use super::*;
use crate::limiter::RateLimiter;
use sha2::{Digest, Sha256, Sha512};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

fn context() -> Ctx {
    Ctx {
        protocol_client: crate::settings::Settings::default()
            .build_protocol_client()
            .unwrap(),
        client: reqwest::Client::new(),
        global: Arc::new(RateLimiter::new(0)),
        local: Arc::new(RateLimiter::new(0)),
        retries: 0,
        cancel: CancellationToken::new(),
        verify: false,
    }
}

async fn http_server() -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let observed = Arc::new(Mutex::new(Vec::new()));
    let requests = observed.clone();
    let server = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let requests = requests.clone();
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                loop {
                    let mut b = [0u8; 1];
                    if socket.read(&mut b).await.unwrap_or(0) == 0 {
                        return;
                    }
                    bytes.push(b[0]);
                    if bytes.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                let request = String::from_utf8(bytes).unwrap();
                let path = request.split_whitespace().nth(1).unwrap().to_owned();
                requests.lock().unwrap().push(request);
                if path == "/remote.meta4" {
                    let xml = r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="remote.bin"><size>5</size><url>/one</url></file></metalink>"#;
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{xml}",
                        xml.len()
                    );
                    socket.write_all(reply.as_bytes()).await.unwrap();
                    return;
                }
                let (status, body) = match path.as_str() {
                    "/dead" => ("404 Not Found", "missing"),
                    "/short" => ("200 OK", "x"),
                    "/bad" => ("200 OK", "xxxxx"),
                    "/one" => ("200 OK", "hello"),
                    "/two" => ("200 OK", "world!"),
                    "/stall" => {
                        std::future::pending::<()>().await;
                        unreachable!()
                    }
                    _ => ("404 Not Found", "unknown"),
                };
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = socket.write_all(reply.as_bytes()).await;
            });
        }
    });
    (base, observed, server)
}

#[tokio::test]
async fn native_metalink_v3_v4_multiple_files_mirror_fallback_and_all_hashes() {
    let (base, requests, server) = http_server().await;
    for namespace in [
        "urn:ietf:params:xml:ns:metalink",
        "http://www.metalinker.org/",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let descriptor = dir.path().join("test.meta4");
        let stage = dir.path().join("stage");
        let xml = format!(
            r#"<metalink xmlns="{namespace}"><files><file name="sub/one.bin"><size>5</size><verification><hash type="sha-256">{}</hash><hash type="sha-512">{}</hash></verification><resources><url>{base}/dead</url><url>{base}/short</url><url>{base}/bad</url><url>{base}/one</url></resources></file><file name="two.bin"><size>6</size><url>{base}/two</url></file></files></metalink>"#,
            hex::encode(Sha256::digest(b"hello")),
            hex::encode(Sha512::digest(b"hello"))
        );
        tokio::fs::write(&descriptor, xml).await.unwrap();
        let task = Arc::new(Mutex::new(Task {
            url: descriptor.to_string_lossy().into_owned(),
            ..Task::default()
        }));
        assert_eq!(
            download(task.clone(), context(), &stage).await.unwrap(),
            Outcome::Completed
        );
        assert_eq!(
            tokio::fs::read(stage.join("payload/sub/one.bin"))
                .await
                .unwrap(),
            b"hello"
        );
        assert_eq!(
            tokio::fs::read(stage.join("payload/two.bin"))
                .await
                .unwrap(),
            b"world!"
        );
        assert_eq!(task.lock().unwrap().total, Some(11));
        let count = requests.lock().unwrap().len();
        assert_eq!(
            download(task, context(), &stage).await.unwrap(),
            Outcome::Completed
        );
        assert_eq!(
            requests.lock().unwrap().len(),
            count,
            "verified outputs resume without network"
        );
    }
    assert!(
        requests
            .lock()
            .unwrap()
            .iter()
            .any(|r| r.starts_with("GET /bad "))
    );
    server.abort();
}

#[tokio::test]
async fn native_metalink_rejects_each_advertised_hash_and_unsafe_paths() {
    let (base, _, server) = http_server().await;
    let dir = tempfile::tempdir().unwrap();
    let descriptor = dir.path().join("bad.meta4");
    let xml = format!(
        r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="one.bin"><size>5</size><hash type="sha-256">{}</hash><hash type="sha-512">{}</hash><url>{base}/one</url></file></metalink>"#,
        hex::encode(Sha256::digest(b"hello")),
        "0".repeat(128)
    );
    tokio::fs::write(&descriptor, xml).await.unwrap();
    let task = Arc::new(Mutex::new(Task {
        url: descriptor.to_string_lossy().into_owned(),
        ..Task::default()
    }));
    assert!(
        download(task, context(), &dir.path().join("stage"))
            .await
            .unwrap_err()
            .contains("digest mismatch")
    );
    assert!(!dir.path().join("stage/payload/one.bin").exists());
    for name in [
        "../outside",
        "/absolute",
        "C:escape",
        "a\\b",
        "NUL",
        "a/./b",
        "a//b",
    ] {
        assert!(parse(&format!(r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="{name}"><url>{base}/one</url></file></metalink>"#), None).is_err(), "{name}");
    }
    server.abort();
}

#[tokio::test]
async fn native_metalink_credentials_follow_exact_origin_and_cancel_headers() {
    let (base, requests, server) = http_server().await;
    let source = reqwest::Url::parse(&base).unwrap();
    let target = source.join("/one").unwrap();
    let snapshot = Task {
        cookie: Some("private=value".into()),
        authorization: Some("Basic c2VjcmV0".into()),
        ..Task::default()
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    request(&client, Some(&source), &target, &snapshot)
        .await
        .unwrap();
    let other = reqwest::Url::parse("http://127.0.0.1:1/").unwrap();
    request(&client, Some(&other), &target, &snapshot)
        .await
        .unwrap();
    let seen = requests.lock().unwrap().clone();
    assert!(
        seen[0]
            .to_ascii_lowercase()
            .contains("cookie: private=value")
    );
    assert!(
        seen[0]
            .to_ascii_lowercase()
            .contains("authorization: basic c2vjcmv0")
    );
    assert!(!seen[1].to_ascii_lowercase().contains("cookie:"));
    assert!(!seen[1].to_ascii_lowercase().contains("authorization:"));
    let task = Arc::new(Mutex::new(Task {
        url: format!("{base}/stall.meta4"),
        ..Task::default()
    }));
    // Use a local descriptor so the stalled operation is a mirror response.
    let dir = tempfile::tempdir().unwrap();
    let descriptor = dir.path().join("cancel.meta4");
    tokio::fs::write(&descriptor, format!(r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="one"><url>{base}/stall</url></file></metalink>"#)).await.unwrap();
    task.lock().unwrap().url = descriptor.to_string_lossy().into_owned();
    let ctx = context();
    let cancel = ctx.cancel.clone();
    let stage = dir.path().join("stage");
    let download = download(task, ctx, &stage);
    tokio::pin!(download);
    tokio::select! { result = &mut download => panic!("unexpected {result:?}"), _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => cancel.cancel() }
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(1), download)
            .await
            .unwrap()
            .unwrap(),
        Outcome::Cancelled
    );
    server.abort();
}
#[tokio::test]
async fn native_metalink_ftp_mirror_and_changed_descriptor_invalidation() {
    let (url, _, server) = crate::protocols::ftp::tests::spawn_server(b"hello", false, false).await;
    let dir = tempfile::tempdir().unwrap();
    let descriptor = dir.path().join("ftp.meta4");
    let stage = dir.path().join("stage");
    let xml = format!(
        r#"<metalink xmlns="urn:ietf:params:xml:ns:metalink"><file name="old.bin"><size>5</size><hash type="sha-256">{}</hash><url>{url}</url></file></metalink>"#,
        hex::encode(Sha256::digest(b"hello"))
    );
    tokio::fs::write(&descriptor, &xml).await.unwrap();
    let task = Arc::new(Mutex::new(Task {
        url: descriptor.to_string_lossy().into_owned(),
        ..Task::default()
    }));
    assert_eq!(
        download(task.clone(), context(), &stage).await.unwrap(),
        Outcome::Completed
    );
    assert_eq!(
        tokio::fs::read(stage.join("payload/old.bin"))
            .await
            .unwrap(),
        b"hello"
    );
    tokio::fs::write(&descriptor, xml.replace("old.bin", "new.bin"))
        .await
        .unwrap();
    assert_eq!(
        download(task, context(), &stage).await.unwrap(),
        Outcome::Completed
    );
    assert!(!stage.join("payload/old.bin").exists());
    assert_eq!(
        tokio::fs::read(stage.join("payload/new.bin"))
            .await
            .unwrap(),
        b"hello"
    );
    server.abort();
}

#[tokio::test]
async fn native_metalink_redirect_strips_cross_origin_credentials() {
    let (base, requests, server) = http_server().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let origin =
        reqwest::Url::parse(&format!("http://{}/", listener.local_addr().unwrap())).unwrap();
    let redirect = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        loop {
            let mut b = [0];
            socket.read_exact(&mut b).await.unwrap();
            bytes.push(b[0]);
            if bytes.ends_with(b"\r\n\r\n") {
                break;
            }
        }
        assert!(
            String::from_utf8(bytes)
                .unwrap()
                .to_ascii_lowercase()
                .contains("cookie: private=value")
        );
        socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: {base}/one\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    });
    let snapshot = Task {
        cookie: Some("private=value".into()),
        authorization: Some("Basic c2VjcmV0".into()),
        ..Task::default()
    };
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    request(&client, Some(&origin), &origin, &snapshot)
        .await
        .unwrap();
    redirect.await.unwrap();
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].to_ascii_lowercase().contains("cookie:"));
    assert!(!requests[0].to_ascii_lowercase().contains("authorization:"));
    server.abort();
}
#[tokio::test]
async fn native_metalink_remote_descriptor_relative_mirror() {
    let (base, requests, server) = http_server().await;
    let dir = tempfile::tempdir().unwrap();
    let task = Arc::new(Mutex::new(Task {
        url: format!("{base}/remote.meta4"),
        cookie: Some("session=yes".into()),
        ..Task::default()
    }));
    assert_eq!(
        download(task, context(), dir.path()).await.unwrap(),
        Outcome::Completed
    );
    assert_eq!(
        tokio::fs::read(dir.path().join("payload/remote.bin"))
            .await
            .unwrap(),
        b"hello"
    );
    let requests = requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(
        requests
            .iter()
            .all(|request| request.contains("cookie: session=yes"))
    );
    server.abort();
}
