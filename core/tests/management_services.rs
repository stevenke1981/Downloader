use dlcore::{
    Manager, NewDownload, Settings, Status,
    automation::{Action, ScheduledAction},
    grabber::{self, GrabOptions},
    remote::{self, RemoteSettings},
    settings::{FolderRule, TrafficMode},
    site::SiteRule,
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[derive(Clone)]
struct Reply {
    body: Vec<u8>,
    mime: &'static str,
    location: Option<String>,
    delay: Duration,
}

impl Reply {
    fn html(body: &str) -> Self {
        Self {
            body: body.as_bytes().to_vec(),
            mime: "text/html",
            location: None,
            delay: Duration::ZERO,
        }
    }
    fn file(delay: Duration) -> Self {
        Self {
            body: vec![42; 128 * 1024],
            mime: "application/octet-stream",
            location: None,
            delay,
        }
    }
    fn redirect(location: String) -> Self {
        Self {
            location: Some(location),
            ..Self::html("")
        }
    }
}

struct Fixture {
    rt: tokio::runtime::Runtime,
    origin: String,
    requests: Arc<Mutex<Vec<(String, String)>>>,
    routes: Arc<Mutex<HashMap<String, Reply>>>,
}

impl Fixture {
    fn new() -> Self {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let listener = rt
            .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
            .unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let routes = Arc::new(Mutex::new(HashMap::<String, Reply>::new()));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let responses = routes.clone();
        rt.spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else { return };
                let routes = responses.clone();
                let requests = recorded.clone();
                tokio::spawn(async move {
                    let mut bytes = Vec::new();
                    while !bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                        let mut buffer = [0; 4096];
                        let Ok(count) = socket.read(&mut buffer).await else { return };
                        if count == 0 { return; }
                        bytes.extend_from_slice(&buffer[..count]);
                    }
                    let request = String::from_utf8_lossy(&bytes);
                    let path = request.split_whitespace().nth(1).unwrap().to_owned();
                    let header = |name: &str| request.lines().filter_map(|line| line.split_once(':')).find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.trim().to_owned());
                    requests.lock().unwrap().push((path.clone(), header("authorization").unwrap_or_default()));
                    let reply = routes.lock().unwrap().get(&path).cloned();
                    let Some(reply) = reply else { let _ = socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await; return };
                    if let Some(location) = reply.location {
                        let _ = socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await;
                        return;
                    }
                    let range = header("range").and_then(|range| {
                        let (start, end) = range.strip_prefix("bytes=")?.split_once('-')?;
                        Some((start.parse::<usize>().ok()?, if end.is_empty() { reply.body.len() - 1 } else { end.parse::<usize>().ok()? }))
                    });
                    let (start, end) = range.unwrap_or((0, reply.body.len().saturating_sub(1)));
                    let end = (end + 1).min(reply.body.len());
                    let body = &reply.body[start.min(end)..end];
                    let mut headers = format!("HTTP/1.1 {}\r\nContent-Length: {}\r\nContent-Type: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n", if range.is_some() { "206 Partial Content" } else { "200 OK" }, body.len(), reply.mime);
                    if range.is_some() { headers.push_str(&format!("Content-Range: bytes {start}-{}/{total}\r\n", end - 1, total = reply.body.len())); }
                    headers.push_str("\r\n");
                    if socket.write_all(headers.as_bytes()).await.is_err() { return; }
                    if body.len() > 1 { tokio::time::sleep(reply.delay).await; }
                    let _ = socket.write_all(body).await;
                });
            }
        });
        Self {
            rt,
            origin,
            requests,
            routes,
        }
    }
    fn url(&self, path: &str) -> String {
        format!("{}{path}", self.origin)
    }
    fn put(&self, path: &str, reply: Reply) {
        self.routes.lock().unwrap().insert(path.into(), reply);
    }
}

fn wait_until(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(12);
    while !condition() {
        assert!(Instant::now() < deadline, "condition timed out");
        std::thread::sleep(Duration::from_millis(30));
    }
}

#[test]
fn old_settings_folder_rules_and_live_traffic_modes() {
    let mut settings: Settings =
        serde_json::from_str(r#"{"max_concurrent":2,"global_limit":1234}"#).unwrap();
    assert_eq!(settings.traffic_mode, TrafficMode::Custom);
    assert!(!settings.remote.enabled);
    settings.folder_rules.push(FolderRule {
        extensions: vec![".ZIP".into()],
        folder: "custom-archives".into(),
    });
    assert_eq!(
        settings.folder_for_name("EXAMPLE.zip", dlcore::Category::Other),
        std::path::PathBuf::from("custom-archives")
    );
    settings.validate().unwrap();
    settings.folder_rules.push(FolderRule {
        extensions: vec!["../exe".into()],
        folder: "invalid".into(),
    });
    assert!(settings.validate().is_err());
    settings.folder_rules.pop();
    settings.traffic_presets = [1024, 2048, 0];
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path());
    manager.set_settings(settings).unwrap();
    for (mode, expected) in [
        (TrafficMode::Low, 1024),
        (TrafficMode::Medium, 2048),
        (TrafficMode::High, 0),
        (TrafficMode::Custom, 1234),
    ] {
        manager.set_traffic_mode(mode).unwrap();
        assert_eq!(manager.global_limit(), expected);
        let persisted: Settings =
            serde_json::from_slice(&std::fs::read(dir.path().join("settings.json")).unwrap())
                .unwrap();
        assert_eq!(persisted.traffic_mode, mode);
    }
    assert!(
        manager
            .try_add(NewDownload {
                url: "http://user:secret@example.com/a".into(),
                ..Default::default()
            })
            .is_err()
    );
    manager.shutdown();
}

#[test]
fn traffic_preset_throttles_a_real_download() {
    let fixture = Fixture::new();
    fixture.put("/limited.bin", Reply::file(Duration::ZERO));
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path());
    manager
        .set_settings(Settings {
            download_dir: dir.path().join("out"),
            auto_verify: false,
            max_connections: 1,
            traffic_mode: TrafficMode::Low,
            traffic_presets: [64 * 1024, 512 * 1024, 0],
            ..Default::default()
        })
        .unwrap();
    let started = Instant::now();
    manager
        .try_add(NewDownload {
            url: fixture.url("/limited.bin"),
            start: true,
            ..Default::default()
        })
        .unwrap();
    wait_until(|| manager.snapshots()[0].status == Status::Completed);
    assert!(
        started.elapsed() >= Duration::from_millis(1400),
        "128 KiB must be throttled by the actual 64 KiB/s global limiter"
    );
    assert_eq!(
        std::fs::read(dir.path().join("out/limited.bin")).unwrap(),
        vec![42; 128 * 1024]
    );
    manager.shutdown();
}

#[test]
fn site_credentials_stay_ephemeral_and_do_not_cross_redirect_origins() {
    let first = Fixture::new();
    let second = Fixture::new();
    first.put("/file.bin", Reply::file(Duration::ZERO));
    first.put(
        "/redirect.bin",
        Reply::redirect(second.url("/redirected.bin")),
    );
    second.put("/redirected.bin", Reply::file(Duration::ZERO));
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path());
    manager
        .set_settings(Settings {
            download_dir: dir.path().join("out"),
            auto_verify: false,
            site_rules: vec![SiteRule {
                host: first.origin.clone(),
                username: "account".into(),
                connections: 1,
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
    manager
        .set_site_credentials(&first.origin, "account", "session-only-secret")
        .unwrap();
    manager
        .try_add(NewDownload {
            url: first.url("/file.bin"),
            start: true,
            ..Default::default()
        })
        .unwrap();
    manager
        .try_add(NewDownload {
            url: first.url("/redirect.bin"),
            start: true,
            ..Default::default()
        })
        .unwrap();
    wait_until(|| {
        manager
            .snapshots()
            .iter()
            .all(|snapshot| snapshot.status == Status::Completed)
    });
    assert!(
        first
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(_, auth)| auth.starts_with("Basic "))
    );
    assert!(!second.requests.lock().unwrap().is_empty());
    assert!(
        second
            .requests
            .lock()
            .unwrap()
            .iter()
            .all(|(_, auth)| auth.is_empty())
    );
    manager.shutdown();
    for filename in ["settings.json", "downloads.json"] {
        let data = std::fs::read_to_string(dir.path().join(filename)).unwrap();
        assert!(!data.contains("session-only-secret"));
        assert!(!data.contains("Basic "));
        assert!(!data.contains("authorization"));
    }
}

#[test]
fn per_origin_concurrency_leaves_other_sites_a_slot() {
    let first = Fixture::new();
    let second = Fixture::new();
    first.put("/a.bin", Reply::file(Duration::from_secs(3)));
    first.put("/b.bin", Reply::file(Duration::from_secs(3)));
    second.put("/c.bin", Reply::file(Duration::from_secs(3)));
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path());
    manager
        .set_settings(Settings {
            download_dir: dir.path().join("out"),
            max_concurrent: 4,
            auto_verify: false,
            site_rules: vec![SiteRule {
                host: first.origin.clone(),
                max_concurrent: 1,
                connections: 1,
                ..Default::default()
            }],
            ..Default::default()
        })
        .unwrap();
    for url in [
        first.url("/a.bin"),
        first.url("/b.bin"),
        second.url("/c.bin"),
    ] {
        manager.add(NewDownload {
            url,
            start: true,
            ..Default::default()
        });
    }
    wait_until(|| {
        manager
            .snapshots()
            .iter()
            .filter(|snapshot| snapshot.status == Status::Downloading)
            .count()
            == 2
    });
    let snapshots = manager.snapshots();
    assert_eq!(
        snapshots
            .iter()
            .filter(|snapshot| snapshot.url.starts_with(&first.origin)
                && snapshot.status == Status::Downloading)
            .count(),
        1
    );
    assert_eq!(
        snapshots
            .iter()
            .filter(|snapshot| snapshot.status == Status::Queued)
            .count(),
        1
    );
    assert!(
        snapshots
            .iter()
            .any(|snapshot| snapshot.url.starts_with(&second.origin)
                && snapshot.status == Status::Downloading)
    );
    assert!(
        snapshots
            .iter()
            .filter(|snapshot| snapshot.url.starts_with(&first.origin))
            .all(|snapshot| snapshot.connections == 1)
    );
    manager.pause_all();
    manager.shutdown();
}

#[test]
fn crawl_bounds_resolves_deduplicates_and_blocks_cross_origin_redirects() {
    let site = Fixture::new();
    let external = Fixture::new();
    site.put("/index", Reply::html(&format!(r#"<!-- <a href="/comment"> --><script>"<a href='/script'>"</script><a href="child">child</a><a href="child#top">duplicate</a><img src='/image.png?x=1&amp;y=2'><a href="{}">external</a><a href="javascript:void(0)">script</a>"#, external.url("/outside"))));
    site.put("/child", Reply::html("<a href='/deep.bin'>file</a>"));
    site.put("/image.png?x=1&y=2", Reply::file(Duration::ZERO));
    site.put("/hop", Reply::redirect(site.url("/index")));
    site.put("/escape", Reply::redirect(external.url("/outside")));
    let client = Settings::default().build_crawl_client().unwrap();
    let links = site
        .rt
        .block_on(grabber::crawl(
            &client,
            &site.url("/index"),
            GrabOptions {
                max_depth: 1,
                max_pages: 3,
            },
        ))
        .unwrap();
    assert_eq!(links.len(), 3, "{links:?}");
    assert!(links.contains(&site.url("/deep.bin")));
    assert!(links.contains(&site.url("/image.png?x=1&y=2")));
    assert_eq!(site.requests.lock().unwrap().len(), 3);
    assert!(external.requests.lock().unwrap().is_empty());
    let shallow = site
        .rt
        .block_on(grabber::crawl(
            &client,
            &site.url("/hop"),
            GrabOptions {
                max_depth: 0,
                max_pages: 1,
            },
        ))
        .unwrap();
    assert_eq!(shallow.len(), 2);
    assert!(
        site.rt
            .block_on(grabber::crawl(
                &client,
                &site.url("/escape"),
                GrabOptions::default()
            ))
            .is_err()
    );
    assert!(external.requests.lock().unwrap().is_empty());
    assert!(
        site.rt
            .block_on(grabber::crawl(
                &client,
                &site.url("/index"),
                GrabOptions {
                    max_depth: 9,
                    max_pages: 1
                }
            ))
            .is_err()
    );
}

#[test]
fn one_shot_executes_argv_once_and_consumes_before_restart() {
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("child settings");
    let manager = Manager::new(dir.path());
    let username = "literal ; & spaced \" argument";
    let action = ScheduledAction {
        id: 1,
        at: chrono::Utc::now().timestamp(),
        action: Action::Run {
            program: env!("CARGO_BIN_EXE_dlxctl").into(),
            args: vec![
                "--data-dir".into(),
                marker.to_string_lossy().into_owned(),
                "site".into(),
                "set".into(),
                "example.com".into(),
                "--username".into(),
                username.into(),
            ],
        },
        consumed: false,
    };
    manager
        .set_settings(Settings {
            scheduled_actions: vec![action],
            ..Default::default()
        })
        .unwrap();
    wait_until(|| {
        manager
            .automation_log()
            .iter()
            .any(|line| line == "Action 1: completed")
    });
    let child_path = marker.join("settings.json");
    let child: Settings = serde_json::from_slice(&std::fs::read(&child_path).unwrap()).unwrap();
    assert_eq!(
        child.site_rules[0].username, username,
        "argv must remain one literal argument"
    );
    let stored: Settings =
        serde_json::from_slice(&std::fs::read(dir.path().join("settings.json")).unwrap()).unwrap();
    assert!(stored.scheduled_actions[0].consumed);
    // An open settings form holding the original unconsumed value cannot re-arm this ID.
    let mut stale = stored.clone();
    stale.scheduled_actions[0].consumed = false;
    manager.set_settings(stale).unwrap();
    assert!(manager.settings().scheduled_actions[0].consumed);
    let modified = std::fs::metadata(&child_path).unwrap().modified().unwrap();
    manager.shutdown();
    let restarted = Manager::new(dir.path());
    std::thread::sleep(Duration::from_millis(800));
    assert!(restarted.automation_log().is_empty());
    assert_eq!(
        std::fs::metadata(&child_path).unwrap().modified().unwrap(),
        modified
    );
    restarted.shutdown();
}

#[test]
fn authenticated_remote_routes_and_raii_stop() {
    let fixture = Fixture::new();
    fixture.put("/file.bin", Reply::file(Duration::from_secs(1)));
    let dir = tempfile::tempdir().unwrap();
    let manager = Manager::new(dir.path());
    assert!(remote::serve(manager.clone(), RemoteSettings::default()).is_err());
    let token = remote::generate_token().unwrap();
    assert_eq!(token.len(), 64);
    assert_ne!(token, remote::generate_token().unwrap());
    let server = remote::serve(
        manager.clone(),
        RemoteSettings {
            enabled: true,
            bind: "127.0.0.1:0".into(),
            token: token.clone(),
        },
    )
    .unwrap();
    let address = server.address();
    let base = format!("http://{address}");
    fixture.rt.block_on(async {
        let client = reqwest::Client::new();
        assert_eq!(client.get(format!("{base}/downloads")).send().await.unwrap().status(), 401);
        assert_eq!(client.get(format!("{base}/downloads")).bearer_auth("wrong").send().await.unwrap().status(), 401);
        for path in ["/settings", "/shutdown", "/automation"] {
            assert_eq!(client.get(format!("{base}{path}")).bearer_auth(&token).send().await.unwrap().status(), 404);
        }
        let reply = client.post(format!("{base}/downloads")).bearer_auth(&token).body(serde_json::json!({"url":fixture.url("/file.bin"),"start":false,"dir":dir.path().join("out")}).to_string()).send().await.unwrap();
        assert_eq!(reply.status(), 201);
        let reply: serde_json::Value = serde_json::from_str(&reply.text().await.unwrap()).unwrap();
        let id = reply["id"].as_u64().unwrap();
        let response = client.get(format!("{base}/downloads")).bearer_auth(&token).send().await.unwrap().text().await.unwrap();
        assert!(response.contains("Paused"));
        assert!(!response.contains(&token));
        assert!(!response.contains("authorization"));
        for path in [format!("/downloads/{id}/start"), format!("/downloads/{id}/pause"), "/start-all".into(), "/pause-all".into()] {
            assert_eq!(client.post(format!("{base}{path}")).bearer_auth(&token).send().await.unwrap().status(), 200);
        }
        assert_eq!(client.post(format!("{base}/downloads")).bearer_auth(&token).body(r#"{"url":"http://user:password@example.com/a"}"#).send().await.unwrap().status(), 400);
        assert_eq!(client.delete(format!("{base}/downloads/{id}")).bearer_auth(&token).send().await.unwrap().status(), 200);
        assert_eq!(client.post(format!("{base}/downloads/{id}/start")).bearer_auth(&token).send().await.unwrap().status(), 404);
    });
    {
        use std::io::{Read, Write};
        for request in [
            format!(
                "POST /downloads HTTP/1.1\r\nAuthorization: Bearer {token}\r\nContent-Length: 65537\r\n\r\n"
            ),
            format!(
                "GET /downloads HTTP/1.1\r\nAuthorization: Bearer {token}\r\nAuthorization: Bearer {token}\r\n\r\n"
            ),
        ] {
            let mut socket = std::net::TcpStream::connect(address).unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            socket.write_all(request.as_bytes()).unwrap();
            let mut response = String::new();
            socket.read_to_string(&mut response).unwrap();
            assert!(response.starts_with("HTTP/1.1 400"));
        }
    }
    drop(server);
    wait_until(|| std::net::TcpStream::connect(address).is_err());
    manager.shutdown();
}
