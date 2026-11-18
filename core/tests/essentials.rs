//! Conflict policy, custom headers, duplicates, export/import and the remote routes.
use dlcore::task::ConflictPolicy;
use dlcore::{Ctx, Manager, NewDownload, Outcome, RateLimiter, Settings, Status, Task, engine};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

/// Minimal HTTP/1.1 server (Range aware) that records every request head, lowercased.
struct Srv {
    port: u16,
    heads: Arc<Mutex<Vec<String>>>,
}

fn serve(data: Vec<u8>, redirect: Option<String>) -> Srv {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let heads = Arc::new(Mutex::new(Vec::new()));
    let seen = heads.clone();
    std::thread::spawn(move || {
        for s in l.incoming() {
            let Ok(mut s) = s else { break };
            let (data, seen, redirect) = (data.clone(), seen.clone(), redirect.clone());
            std::thread::spawn(move || {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 2048];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match s.read(&mut tmp) {
                        Ok(0) | Err(_) => return,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_ascii_lowercase();
                seen.lock().unwrap().push(head.clone());
                let name = head
                    .split_whitespace()
                    .nth(1)
                    .unwrap_or("/")
                    .rsplit('/')
                    .next()
                    .unwrap()
                    .to_string();
                if let Some(base) = redirect {
                    let _ = write!(
                        s,
                        "HTTP/1.1 302 Found\r\nLocation: {base}{name}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    return;
                }
                let total = data.len();
                let (status, a, b) = match head
                    .lines()
                    .find_map(|l| l.strip_prefix("range: bytes="))
                    .and_then(|r| r.split_once('-'))
                {
                    Some((a, b)) => {
                        let a: usize = a.parse().unwrap();
                        let b = b.parse::<usize>().map_or(total - 1, |b| b.min(total - 1));
                        ("206 Partial Content", a, b + 1)
                    }
                    None => ("200 OK", 0, total),
                };
                let mut h = format!(
                    "HTTP/1.1 {status}\r\nContent-Length: {}\r\nAccept-Ranges: bytes\r\nConnection: close\r\n",
                    b - a
                );
                if status.starts_with("206") {
                    h += &format!("Content-Range: bytes {a}-{}/{total}\r\n", b - 1);
                }
                h += "\r\n";
                let _ = s.write_all(h.as_bytes());
                let _ = s.write_all(&data[a..b]);
            });
        }
    });
    Srv { port, heads }
}

impl Srv {
    fn url(&self, name: &str) -> String {
        format!("http://127.0.0.1:{}/files/{name}", self.port)
    }
}

fn ctx() -> Ctx {
    Ctx {
        protocol_client: Settings::default().build_protocol_client().unwrap(),
        client: Settings::default().build_client().unwrap(),
        global: Arc::new(RateLimiter::new(0)),
        local: Arc::new(RateLimiter::new(0)),
        retries: 1,
        cancel: CancellationToken::new(),
        verify: false,
    }
}

fn task(url: String, dir: &std::path::Path, conflict: ConflictPolicy) -> Arc<Mutex<Task>> {
    Arc::new(Mutex::new(Task {
        url,
        dir: dir.into(),
        connections: 2,
        status: Status::Downloading,
        conflict,
        ..Default::default()
    }))
}

fn data(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i * 7 % 251) as u8).collect()
}

#[tokio::test]
async fn conflict_rename_overwrite_skip() {
    let d = data(300_000);
    let s = serve(d.clone(), None);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("a.bin"), b"old").unwrap();

    let t = task(s.url("a.bin"), dir.path(), ConflictPolicy::Rename);
    assert_eq!(
        engine::run(t.clone(), ctx()).await.unwrap(),
        Outcome::Completed
    );
    assert_eq!(t.lock().unwrap().file_name, "a (1).bin");
    assert_eq!(std::fs::read(dir.path().join("a.bin")).unwrap(), b"old");

    let t = task(s.url("a.bin"), dir.path(), ConflictPolicy::Overwrite);
    assert_eq!(
        engine::run(t.clone(), ctx()).await.unwrap(),
        Outcome::Completed
    );
    assert_eq!(t.lock().unwrap().file_name, "a.bin");
    assert_eq!(std::fs::read(dir.path().join("a.bin")).unwrap(), d);

    std::fs::write(dir.path().join("a.bin"), b"keep").unwrap();
    let t = task(s.url("a.bin"), dir.path(), ConflictPolicy::Skip);
    let e = engine::run(t.clone(), ctx()).await.unwrap_err();
    assert!(e.starts_with("file exists: "), "{e}");
    assert_eq!(dlcore::util::error_code(&e), "conflict");
    assert_eq!(std::fs::read(dir.path().join("a.bin")).unwrap(), b"keep");
    assert!(!dir.path().join("a.bin.part").exists());
}

#[tokio::test]
async fn overwrite_never_reuses_a_foreign_part_file() {
    let s = serve(data(1000), None);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("b.bin.part"), b"someone else").unwrap();
    let t = task(s.url("b.bin"), dir.path(), ConflictPolicy::Overwrite);
    engine::run(t.clone(), ctx()).await.unwrap();
    assert_eq!(t.lock().unwrap().file_name, "b (1).bin");
    assert_eq!(
        std::fs::read(dir.path().join("b.bin.part")).unwrap(),
        b"someone else"
    );
}

#[tokio::test]
async fn headers_and_user_agent_reach_the_server() {
    let s = serve(data(100_000), None);
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("h.bin"), dir.path(), ConflictPolicy::Rename);
    {
        let mut t = t.lock().unwrap();
        t.headers = vec![("X-Token".into(), "abc123".into())];
        t.user_agent = Some("TestAgent/9".into());
    }
    engine::run(t, ctx()).await.unwrap();
    let heads = s.heads.lock().unwrap();
    assert!(heads.len() >= 2);
    for h in heads.iter() {
        assert!(h.contains("x-token: abc123"), "{h}");
        assert!(h.contains("user-agent: testagent/9"), "{h}");
    }
}

#[tokio::test]
async fn custom_headers_stay_on_their_origin_but_user_agent_follows() {
    let data = data(100_000);
    let target = serve(data, None);
    // "localhost" is another origin than 127.0.0.1.
    let origin = serve(
        Vec::new(),
        Some(format!("http://localhost:{}/files/", target.port)),
    );
    let dir = tempfile::tempdir().unwrap();
    let t = task(origin.url("r.bin"), dir.path(), ConflictPolicy::Rename);
    {
        let mut t = t.lock().unwrap();
        t.headers = vec![("X-Token".into(), "abc123".into())];
        t.user_agent = Some("TestAgent/9".into());
    }
    engine::run(t, ctx()).await.unwrap();
    assert!(
        origin
            .heads
            .lock()
            .unwrap()
            .iter()
            .all(|h| h.contains("x-token: abc123"))
    );
    let heads = target.heads.lock().unwrap();
    assert!(!heads.is_empty());
    for h in heads.iter() {
        assert!(!h.contains("x-token"), "header leaked: {h}");
        assert!(h.contains("user-agent: testagent/9"), "{h}");
    }
}

#[tokio::test]
async fn resolved_url_is_downloaded_without_credentials() {
    let s = serve(data(50_000), None);
    let dir = tempfile::tempdir().unwrap();
    let t = task(
        "http://localhost:1/never/r.bin".into(),
        dir.path(),
        ConflictPolicy::Rename,
    );
    {
        let mut t = t.lock().unwrap();
        t.resolved_url = Some(s.url("r.bin"));
        t.cookie = Some("sid=secret".into());
        t.headers = vec![("X-Token".into(), "abc".into())];
    }
    engine::run(t, ctx()).await.unwrap();
    for h in s.heads.lock().unwrap().iter() {
        assert!(!h.contains("cookie"), "{h}");
        assert!(h.contains("x-token: abc"), "{h}");
    }
}

fn new_manager() -> (tempfile::TempDir, Manager) {
    let dir = tempfile::tempdir().unwrap();
    let m = Manager::new(dir.path());
    (dir, m)
}

fn paused(url: &str) -> NewDownload {
    NewDownload {
        url: url.into(),
        start: false,
        ..Default::default()
    }
}

#[test]
fn old_state_files_load_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("downloads.json"),
        r#"[{"id":7,"url":"https://example.invalid/a.zip","status":"Paused","dir":"x","file_name":"a.zip"}]"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("settings.json"),
        r#"{"max_concurrent":2,"retries":3}"#,
    )
    .unwrap();
    let m = Manager::new(dir.path());
    let snaps = m.snapshots();
    assert_eq!(snaps.len(), 1);
    assert_eq!(snaps[0].id, 7);
    let s = m.settings();
    assert_eq!(s.max_concurrent, 2);
    assert_eq!(s.conflict, ConflictPolicy::Rename);
    assert!(!s.modules.enabled && s.modules.approved.is_empty());
}

#[test]
fn unique_rejects_duplicates_with_a_stable_prefix() {
    let (_d, m) = new_manager();
    let first = m.try_add(paused("https://example.invalid/a.zip")).unwrap();
    assert_eq!(
        m.find_duplicate(" https://example.invalid/a.zip "),
        Some(first)
    );
    // Without `unique` a second copy is still allowed.
    m.try_add(paused("https://example.invalid/a.zip")).unwrap();
    let e = m
        .try_add(NewDownload {
            unique: true,
            ..paused("https://example.invalid/a.zip")
        })
        .unwrap_err();
    assert_eq!(e, format!("duplicate: already queued as #{first}"));
    assert_eq!(dlcore::util::error_code(&e), "duplicate");
    assert!(
        m.find_duplicate("https://example.invalid/other.zip")
            .is_none()
    );
}

#[test]
fn new_download_validates_headers_and_inherits_conflict_policy() {
    let (_d, m) = new_manager();
    let e = m
        .try_add(NewDownload {
            headers: vec![("Cookie".into(), "x".into())],
            ..paused("https://example.invalid/a.zip")
        })
        .unwrap_err();
    assert!(e.starts_with("invalid header"), "{e}");
    let mut s = m.settings();
    s.conflict = ConflictPolicy::Skip;
    m.set_settings(s).unwrap();
    m.try_add(paused("https://example.invalid/b.zip")).unwrap();
    let json = m.export_queue();
    assert!(json.contains("\"conflict\": \"skip\""), "{json}");
}

#[test]
fn export_import_round_trip() {
    let (_d, a) = new_manager();
    a.try_add(NewDownload {
        priority: 5,
        referer: Some("https://example.invalid/page".into()),
        headers: vec![("X-Token".into(), "abc".into())],
        user_agent: Some("UA/1".into()),
        cookie: Some("sid=secret".into()),
        conflict: Some(ConflictPolicy::Overwrite),
        ..paused("https://example.invalid/a.zip")
    })
    .unwrap();
    a.try_add(paused("https://example.invalid/b.zip")).unwrap();
    let json = a.export_queue();
    assert!(!json.contains("secret") && !json.contains("sid="), "{json}");
    assert!(json.contains("\"schema\": 1"));

    let (_d2, b) = new_manager();
    let r = b.import_queue(&json, false).unwrap();
    assert_eq!(r.added.len(), 2);
    assert!(r.skipped.is_empty());
    let again = b.import_queue(&json, false).unwrap();
    assert!(again.added.is_empty());
    assert_eq!(again.skipped.len(), 2);
    assert!(again.skipped[0].1.starts_with("duplicate:"));
    assert_eq!(b.snapshots().len(), 2);
    assert_eq!(b.snapshots()[0].priority, 5);
    assert_eq!(a.export_queue(), b.export_queue());
    assert!(
        b.import_queue("{\"schema\":2,\"downloads\":[]}", false)
            .is_err()
    );
    assert!(b.import_queue("nope", false).is_err());
}

#[test]
fn module_approvals_only_change_through_approve_and_revoke() {
    let (d, m) = new_manager();
    let mdir = d.path().join("modules/demo");
    std::fs::create_dir_all(&mdir).unwrap();
    std::fs::write(mdir.join("module.json"), b"{}").unwrap();
    m.approve_module("demo").unwrap();
    let hash = m.settings().modules.approved["demo"].clone();
    assert_eq!(hash.len(), 64);
    // A settings form cannot add or drop approvals.
    let mut s = m.settings();
    s.modules.approved.clear();
    s.modules.approved.insert("evil".into(), "a".repeat(64));
    s.modules.enabled = true;
    m.set_settings(s).unwrap();
    let s = m.settings();
    assert_eq!(s.modules.approved.len(), 1);
    assert_eq!(s.modules.approved["demo"], hash);
    assert!(s.modules.enabled);
    m.revoke_module("demo").unwrap();
    assert!(m.settings().modules.approved.is_empty());
    assert!(m.approve_module("../x").is_err());
    assert!(m.approve_module("missing").is_err());
    let _ = m.modules();
}

fn call(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, serde_json::Value) {
    let mut s = TcpStream::connect(addr).unwrap();
    write!(
        s,
        "{method} {path} HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer 0123456789abcdef0123\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).unwrap();
    let status = out[9..12].parse().unwrap();
    let body = out.split_once("\r\n\r\n").unwrap().1;
    (status, serde_json::from_str(body).unwrap())
}

#[test]
fn remote_routes_and_error_codes() {
    let (_d, m) = new_manager();
    let server = dlcore::remote::serve(
        m.clone(),
        dlcore::remote::RemoteSettings {
            enabled: true,
            bind: "127.0.0.1:0".into(),
            token: "0123456789abcdef0123".into(),
        },
    )
    .unwrap();
    let a = server.address();
    let add = r#"{"url":"https://example.invalid/a.zip","start":false,"unique":true,"priority":3,"referer":"https://example.invalid/","headers":{"X-A":"1"},"user_agent":"UA/2","conflict":"skip"}"#;
    let (st, v) = call(a, "POST", "/downloads", add);
    assert_eq!(st, 201, "{v}");
    let id = v["id"].as_u64().unwrap();
    let (st, v) = call(a, "POST", "/downloads", add);
    assert_eq!(st, 409);
    assert_eq!(v["code"], "duplicate");
    assert!(v["error"].as_str().unwrap().starts_with("duplicate:"));
    let (st, v) = call(
        a,
        "POST",
        "/downloads",
        r#"{"url":"https://example.invalid/c","headers":{"Cookie":"x"}}"#,
    );
    assert_eq!(st, 400);
    assert_eq!(v["code"], "invalid");
    let (st, v) = call(a, "GET", &format!("/downloads/{id}"), "");
    assert_eq!((st, v["priority"].as_i64()), (200, Some(3)));
    assert_eq!(call(a, "GET", "/downloads/999", "").0, 404);
    let (st, v) = call(a, "GET", "/version", "");
    assert_eq!((st, v["version"].as_str().is_some()), (200, true));
    let (st, v) = call(a, "GET", "/modules", "");
    assert_eq!((st, v.is_array()), (200, true));
    let (st, queue) = call(a, "GET", "/export", "");
    assert_eq!((st, queue["schema"].as_i64()), (200, Some(1)));
    let (st, rep) = call(a, "POST", "/import", &queue.to_string());
    assert_eq!(st, 200);
    assert_eq!(rep["skipped"].as_array().unwrap().len(), 1);
}
