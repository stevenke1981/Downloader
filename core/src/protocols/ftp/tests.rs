
use super::*;
use crate::limiter::RateLimiter;
use tokio::io::{AsyncBufReadExt, BufReader};
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

pub(crate) async fn spawn_server(
    contents: &'static [u8],
    reject_rest: bool,
    stall: bool,
) -> (String, Arc<Mutex<Vec<String>>>, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("ftp://{}/a%2520b.bin", listener.local_addr().unwrap());
    let commands = Arc::new(Mutex::new(Vec::new()));
    let observed = commands.clone();
    let handle = tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                return;
            };
            let observed = observed.clone();
            tokio::spawn(async move {
                socket
                    .write_all(b"220-Welcome\r\ntext without a status code\r\n220 Ready\r\n")
                    .await
                    .unwrap();
                let mut socket = BufReader::new(socket);
                let mut passive = None;
                let mut offset = 0usize;
                loop {
                    let mut line = String::new();
                    if socket.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let line = line.trim_end().to_owned();
                    observed.lock().unwrap().push(line.clone());
                    let reply = if line.starts_with("USER ") {
                        "331 Password\r\n".to_owned()
                    } else if line.starts_with("PASS ") {
                        "230 Logged in\r\n".into()
                    } else if line == "TYPE I" {
                        "200 Binary\r\n".into()
                    } else if line.starts_with("SIZE ") {
                        format!("213 {}\r\n", contents.len())
                    } else if line.starts_with("MDTM ") {
                        "213 20261028120000\r\n".into()
                    } else if line == "EPSV" {
                        "500 EPSV unavailable\r\n".into()
                    } else if line == "PASV" {
                        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                        let port = listener.local_addr().unwrap().port();
                        passive = Some(listener);
                        format!("227 Passive (192,0,2,1,{},{})\r\n", port / 256, port % 256)
                    } else if let Some(value) = line.strip_prefix("REST ") {
                        if reject_rest {
                            "502 REST unavailable\r\n".into()
                        } else {
                            offset = value.parse().unwrap();
                            "350 Restart accepted\r\n".into()
                        }
                    } else if line.starts_with("CWD ") {
                        "250 Directory\r\n".into()
                    } else if line.starts_with("RETR ") || line == "NLST" {
                        socket
                            .get_mut()
                            .write_all(b"150 Data follows\r\n")
                            .await
                            .unwrap();
                        let (mut data, _) = passive.take().unwrap().accept().await.unwrap();
                        if stall {
                            std::future::pending::<()>().await;
                        }
                        if line == "NLST" {
                            data.write_all(
                                "one space.txt\r\n%20.txt\r\n\u{4e2d}\u{6587}.bin\r\n".as_bytes(),
                            )
                            .await
                            .unwrap();
                        } else {
                            data.write_all(&contents[offset..]).await.unwrap();
                        }
                        drop(data);
                        "226 Complete\r\n".into()
                    } else {
                        "500 Unknown\r\n".into()
                    };
                    if socket.get_mut().write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    (url, commands, handle)
}

#[tokio::test]
async fn native_ftp_binary_basic_credentials_and_raw_names() {
    let (url, commands, server) = spawn_server(b"native ftp data", false, false).await;
    let dir = tempfile::tempdir().unwrap();
    let task = Arc::new(Mutex::new(Task {
        url: url.clone(),
        authorization: Some(format!(
            "Basic {}",
            base64::engine::general_purpose::STANDARD.encode("user%20:p%3Ass:word")
        )),
        ..Task::default()
    }));
    assert_eq!(
        download(task.clone(), context(), dir.path()).await.unwrap(),
        Outcome::Completed
    );
    assert_eq!(
        tokio::fs::read(dir.path().join("payload/a%20b.bin"))
            .await
            .unwrap(),
        b"native ftp data"
    );
    assert!(task.lock().unwrap().resumable);
    let names = list(&url, "user", "password").await.unwrap();
    assert_eq!(names, ["one space.txt", "%20.txt", "\u{4e2d}\u{6587}.bin"]);
    let commands = commands.lock().unwrap();
    assert!(commands.contains(&"USER user%20".into()));
    assert!(commands.contains(&"PASS p%3Ass:word".into()));
    assert!(commands.contains(&"RETR /a%20b.bin".into()));
    server.abort();
}

#[tokio::test]
async fn native_ftp_resumes_only_matching_identity_and_accepted_rest() {
    for reject in [false, true] {
        let (url, commands, server) = spawn_server(b"abcdefghij", reject, false).await;
        let dir = tempfile::tempdir().unwrap();
        tokio::fs::create_dir_all(dir.path().join("payload"))
            .await
            .unwrap();
        tokio::fs::create_dir_all(dir.path().join("state"))
            .await
            .unwrap();
        tokio::fs::write(dir.path().join("payload/a%20b.bin"), b"abcd")
            .await
            .unwrap();
        let identity = Identity {
            source: url.clone(),
            size: Some(10),
            modified: Some("20261028120000".into()),
        };
        tokio::fs::write(
            dir.path().join("state/ftp.json"),
            serde_json::to_vec(&identity).unwrap(),
        )
        .await
        .unwrap();
        let task = Arc::new(Mutex::new(Task {
            url,
            ..Task::default()
        }));
        assert_eq!(
            download(task, context(), dir.path()).await.unwrap(),
            Outcome::Completed
        );
        assert_eq!(
            tokio::fs::read(dir.path().join("payload/a%20b.bin"))
                .await
                .unwrap(),
            b"abcdefghij"
        );
        assert!(commands.lock().unwrap().contains(&"REST 4".into()));
        server.abort();
    }
}

#[tokio::test]
async fn native_ftp_restarts_untrusted_partial_and_cancels_blocked_data() {
    let (url, commands, server) = spawn_server(b"abcdefghij", false, false).await;
    let dir = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(dir.path().join("payload"))
        .await
        .unwrap();
    tokio::fs::write(dir.path().join("payload/a%20b.bin"), b"wrong")
        .await
        .unwrap();
    assert_eq!(
        download(
            Arc::new(Mutex::new(Task {
                url,
                ..Task::default()
            })),
            context(),
            dir.path()
        )
        .await
        .unwrap(),
        Outcome::Completed
    );
    assert!(commands.lock().unwrap().contains(&"REST 0".into()));
    assert_eq!(
        tokio::fs::read(dir.path().join("payload/a%20b.bin"))
            .await
            .unwrap(),
        b"abcdefghij"
    );
    server.abort();
    let (url, _, server) = spawn_server(b"abcdefghij", false, true).await;
    let ctx = context();
    let cancel = ctx.cancel.clone();
    let task = Arc::new(Mutex::new(Task {
        url,
        ..Task::default()
    }));
    let future = download(task, ctx, dir.path());
    tokio::pin!(future);
    tokio::select! { result = &mut future => panic!("unexpected {result:?}"), _ = tokio::time::sleep(Duration::from_millis(100)) => cancel.cancel() }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), future)
            .await
            .unwrap()
            .unwrap(),
        Outcome::Cancelled
    );
    server.abort();
}
#[tokio::test]
async fn native_ftp_cancels_global_limiter_wait() {
    let (url, _, server) = spawn_server(b"abcdefghij", false, false).await;
    let dir = tempfile::tempdir().unwrap();
    let mut ctx = context();
    ctx.global = Arc::new(RateLimiter::new(1));
    let cancel = ctx.cancel.clone();
    let task = Arc::new(Mutex::new(Task {
        url,
        ..Task::default()
    }));
    let transfer = download(task, ctx, dir.path());
    tokio::pin!(transfer);
    tokio::select! { result = &mut transfer => panic!("unexpected {result:?}"), _ = tokio::time::sleep(Duration::from_millis(100)) => cancel.cancel() }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), transfer)
            .await
            .unwrap()
            .unwrap(),
        Outcome::Cancelled
    );
    server.abort();
}
