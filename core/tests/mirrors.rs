//! HTTP mirror fallback. `localhost` and `127.0.0.1` are different origins for the engine.
mod server;

use dlcore::task::ConflictPolicy;
use dlcore::{Algo, Checksum, Ctx, Outcome, RateLimiter, Settings, Status, Task, engine};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn ctx(retries: u32) -> Ctx {
    Ctx {
        protocol_client: Settings::default().build_protocol_client().unwrap(),
        client: Settings::default().build_client().unwrap(),
        global: Arc::new(RateLimiter::new(0)),
        local: Arc::new(RateLimiter::new(0)),
        retries,
        cancel: CancellationToken::new(),
        verify: false,
    }
}

fn task(url: String, dir: &std::path::Path, conns: u32, mirrors: Vec<String>) -> Arc<Mutex<Task>> {
    Arc::new(Mutex::new(Task {
        url,
        dir: dir.into(),
        connections: conns,
        status: Status::Downloading,
        mirrors,
        ..Default::default()
    }))
}

/// The same path on another origin of the same server.
fn other_origin(s: &server::Server, name: &str) -> String {
    format!("http://localhost:{}/files/{name}", s.port)
}

fn log(t: &Arc<Mutex<Task>>) -> Vec<String> {
    t.lock().unwrap().log.clone()
}

/// Primary died mid-transfer: known size, a resumable `.part` with some bytes.
async fn partial(data: &[u8], dir: &std::path::Path) -> (Arc<server::Server>, Arc<Mutex<Task>>) {
    let p = server::start(data.to_vec(), true, 0).await;
    p.fail_after.store(300_000, Ordering::SeqCst);
    let t = task(p.url("f.bin"), dir, 1, vec![]);
    engine::run(t.clone(), ctx(0)).await.unwrap_err();
    {
        let t = t.lock().unwrap();
        assert_eq!(t.total, Some(data.len() as u64));
        assert!(t.downloaded() > 0, "expected a partial download");
        assert!(t.part_path().exists());
    }
    // From now on the primary is down.
    p.fail_status.store(503, Ordering::SeqCst);
    (p, t)
}

#[tokio::test]
async fn primary_503_falls_back_to_mirror_and_logs_origin_only() {
    let data = server::test_data(700_000);
    let p = server::start(data.clone(), true, 0).await;
    p.fail_status.store(503, Ordering::SeqCst);
    let m = server::start(vec![], true, 0).await;
    m.set_data(data.clone(), "\"m1\"");
    let dir = tempfile::tempdir().unwrap();
    let mirror = format!("{}?token=secret", other_origin(&m, "f.bin"));
    let t = task(p.url("f.bin"), dir.path(), 2, vec![mirror]);
    assert_eq!(
        engine::run(t.clone(), ctx(0)).await.unwrap(),
        Outcome::Completed
    );
    assert!(std::fs::read(dir.path().join("f.bin")).unwrap() == data);
    let origin = format!("http://localhost:{}", m.port);
    let log = log(&t);
    assert!(
        log.iter()
            .any(|l| l == &format!("HTTP source failed; trying mirror 1 of 1 ({origin})")),
        "{log:?}"
    );
    assert!(
        log.iter()
            .any(|l| l == &format!("HTTP download completed from mirror 1 ({origin})")),
        "{log:?}"
    );
    assert!(
        log.iter()
            .all(|l| !l.contains("token") && !l.contains("/files/")),
        "{log:?}"
    );
    assert_ne!(t.lock().unwrap().etag.as_deref(), Some("\"m1\""));
}

#[tokio::test]
async fn not_found_falls_back_but_forbidden_never_does() {
    let data = server::test_data(300_000);
    let m = server::start(data.clone(), true, 0).await;
    let dir = tempfile::tempdir().unwrap();

    let p = server::start(data.clone(), true, 0).await;
    p.fail_status.store(404, Ordering::SeqCst);
    let t = task(
        p.url("a.bin"),
        dir.path(),
        1,
        vec![other_origin(&m, "a.bin")],
    );
    assert_eq!(
        engine::run(t.clone(), ctx(0)).await.unwrap(),
        Outcome::Completed
    );
    assert!(std::fs::read(dir.path().join("a.bin")).unwrap() == data);
    assert!(m.requests.load(Ordering::SeqCst) >= 2);

    let m = server::start(data.clone(), true, 0).await;
    p.fail_status.store(403, Ordering::SeqCst);
    let t = task(
        p.url("b.bin"),
        dir.path(),
        1,
        vec![other_origin(&m, "b.bin")],
    );
    let e = engine::run(t.clone(), ctx(1)).await.unwrap_err();
    assert_eq!(e, "HTTP 403 Forbidden");
    assert_eq!(m.requests.load(Ordering::SeqCst), 0);
    assert!(!dir.path().join("b.bin").exists());
}

fn credentials(t: &Arc<Mutex<Task>>, origin: &str) {
    let mut t = t.lock().unwrap();
    t.cookie = Some("sid=secret".into());
    t.authorization = Some("Basic c2VjcmV0".into());
    t.headers = vec![("X-Custom".into(), "c1".into())];
    t.referer = Some(format!("{origin}/page"));
    t.user_agent = Some("MirrorTest/1".into());
    t.resolved_url = Some(t.url.clone());
    t.module_headers = vec![("X-Module".into(), "m1".into())];
}

const PRIVATE: [&str; 5] = [
    "cookie:",
    "authorization:",
    "x-custom:",
    "x-module:",
    "referer:",
];

#[tokio::test]
async fn mirror_isolation_by_origin() {
    let data = server::test_data(300_000);
    let p = server::start(data.clone(), true, 0).await;
    p.fail_status.store(503, Ordering::SeqCst);
    *p.fail_name.lock().unwrap() = "f.bin".into();
    let origin = format!("http://127.0.0.1:{}", p.port);
    let dir = tempfile::tempdir().unwrap();

    // Another origin gets only the User-Agent.
    let m = server::start(data.clone(), true, 0).await;
    let t = task(
        p.url("f.bin"),
        dir.path(),
        2,
        vec![other_origin(&m, "f.bin")],
    );
    credentials(&t, &origin);
    assert_eq!(
        engine::run(t.clone(), ctx(0)).await.unwrap(),
        Outcome::Completed
    );
    let heads = m.heads.lock().unwrap().clone();
    assert!(heads.len() >= 2, "{heads:?}");
    for h in &heads {
        let h = h.to_ascii_lowercase();
        for name in PRIVATE {
            assert!(!h.contains(name), "{name} leaked to another origin: {h}");
        }
        assert!(h.contains("user-agent: mirrortest/1"), "{h}");
    }

    // The same origin gets everything the primary would.
    let t = task(p.url("f.bin"), dir.path(), 2, vec![p.url("g.bin")]);
    credentials(&t, &origin);
    assert_eq!(
        engine::run(t.clone(), ctx(0)).await.unwrap(),
        Outcome::Completed
    );
    let heads: Vec<String> = p
        .heads
        .lock()
        .unwrap()
        .iter()
        .filter(|h| h.contains("/files/g.bin"))
        .map(|h| h.to_ascii_lowercase())
        .collect();
    assert!(heads.len() >= 2, "{heads:?}");
    for h in &heads {
        for name in PRIVATE {
            assert!(h.contains(name), "{name} missing on the same origin: {h}");
        }
        assert!(h.contains("user-agent: mirrortest/1"), "{h}");
    }
}

#[tokio::test]
async fn mirror_finishes_the_partial_file_of_a_dead_primary() {
    let data = server::test_data(2 << 20);
    let p = server::start(vec![], true, 0).await;
    p.set_data(data.clone(), "\"p1\"");
    p.fail_after.store(512 * 1024, Ordering::SeqCst);
    let m = server::start(vec![], true, 0).await;
    m.set_data(data.clone(), "\"m1\"");
    let dir = tempfile::tempdir().unwrap();
    let t = task(
        p.url("f.bin"),
        dir.path(),
        1,
        vec![other_origin(&m, "f.bin")],
    );
    assert_eq!(
        engine::run(t.clone(), ctx(0)).await.unwrap(),
        Outcome::Completed
    );
    assert!(std::fs::read(dir.path().join("f.bin")).unwrap() == data);
    let sent = m.bytes_sent.load(Ordering::SeqCst);
    assert!(sent < data.len() as u64, "mirror sent {sent} bytes");
    assert_eq!(
        t.lock().unwrap().etag.as_deref(),
        Some("\"p1\""),
        "the primary's validator stays"
    );
}

#[tokio::test]
async fn mirror_with_another_size_is_skipped() {
    let data = server::test_data(1 << 20);
    let dir = tempfile::tempdir().unwrap();
    let (_p, t) = partial(&data, dir.path()).await;
    let have = t.lock().unwrap().downloaded();
    let other = server::start(server::test_data((1 << 20) + 1), true, 0).await;

    // Every mirror refused: the primary's error, and the .part stays.
    t.lock().unwrap().mirrors = vec![other_origin(&other, "f.bin")];
    let e = engine::run(t.clone(), ctx(0)).await.unwrap_err();
    assert_eq!(e, "HTTP 503 Service Unavailable");
    assert!(dir.path().join("f.bin.part").exists());
    assert_eq!(t.lock().unwrap().downloaded(), have);
    assert!(
        log(&t)
            .iter()
            .any(|l| l == "HTTP mirror 1 skipped: size differs or no range support")
    );
    assert_eq!(other.data_requests.load(Ordering::SeqCst), 0);

    // The next mirror with the right size is used.
    let same = server::start(data.clone(), true, 0).await;
    t.lock().unwrap().mirrors = vec![other_origin(&other, "f.bin"), other_origin(&same, "f.bin")];
    assert_eq!(
        engine::run(t.clone(), ctx(0)).await.unwrap(),
        Outcome::Completed
    );
    assert!(std::fs::read(dir.path().join("f.bin")).unwrap() == data);
    assert!(same.bytes_sent.load(Ordering::SeqCst) < data.len() as u64);
    assert!(
        log(&t)
            .iter()
            .any(|l| l.starts_with("HTTP download completed from mirror 2 "))
    );
}

#[tokio::test]
async fn mirror_without_ranges_is_skipped_for_a_partial() {
    let data = server::test_data(1 << 20);
    let dir = tempfile::tempdir().unwrap();
    let (_p, t) = partial(&data, dir.path()).await;
    let have = t.lock().unwrap().downloaded();
    let m = server::start(data.clone(), false, 0).await;
    t.lock().unwrap().mirrors = vec![other_origin(&m, "f.bin")];
    let e = engine::run(t.clone(), ctx(0)).await.unwrap_err();
    assert_eq!(e, "HTTP 503 Service Unavailable");
    assert_eq!(m.requests.load(Ordering::SeqCst), 1, "probe only");
    assert!(dir.path().join("f.bin.part").exists());
    assert_eq!(t.lock().unwrap().downloaded(), have);
}

#[tokio::test]
async fn conflicts_checksums_and_cancel_never_contact_a_mirror() {
    let data = server::test_data(300_000);
    let p = server::start(data.clone(), true, 0).await;
    let m = server::start(data.clone(), true, 0).await;
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(dir.path().join("x.bin"), b"old").unwrap();
    let t = task(
        p.url("x.bin"),
        dir.path(),
        1,
        vec![other_origin(&m, "x.bin")],
    );
    t.lock().unwrap().conflict = ConflictPolicy::Skip;
    let e = engine::run(t.clone(), ctx(0)).await.unwrap_err();
    assert!(e.starts_with("file exists:"), "{e}");

    let t = task(
        p.url("y.bin"),
        dir.path(),
        1,
        vec![other_origin(&m, "y.bin")],
    );
    t.lock().unwrap().checksum = Some(Checksum {
        algo: Algo::Md5,
        expected: "0".repeat(32),
        source: "user".into(),
    });
    let e = engine::run(t.clone(), ctx(0)).await.unwrap_err();
    assert!(e.contains("verification failed"), "{e}");

    let slow = server::start(data.clone(), true, 100_000).await;
    let t = task(
        slow.url("z.bin"),
        dir.path(),
        1,
        vec![other_origin(&m, "z.bin")],
    );
    let c = ctx(0);
    let cancel = c.cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
    });
    assert_eq!(engine::run(t, c).await.unwrap(), Outcome::Cancelled);

    assert_eq!(m.requests.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn module_mirrors_need_a_resolved_url_and_are_never_saved() {
    let data = server::test_data(300_000);
    let p = server::start(data.clone(), true, 0).await;
    p.fail_status.store(503, Ordering::SeqCst);
    let m = server::start(data.clone(), true, 0).await;
    let dir = tempfile::tempdir().unwrap();
    let t = task(p.url("f.bin"), dir.path(), 1, vec![]);
    let module_mirror = other_origin(&m, "module.bin");
    t.lock().unwrap().module_mirrors = vec![module_mirror.clone()];
    let saved = serde_json::to_string(&*t.lock().unwrap()).unwrap();
    assert!(!saved.contains("module.bin"), "{saved}");
    let e = engine::run(t.clone(), ctx(0)).await.unwrap_err();
    assert_eq!(e, "HTTP 503 Service Unavailable");
    assert_eq!(m.requests.load(Ordering::SeqCst), 0);

    // The resolved link is on the primary's origin, so it fails the same way.
    {
        let mut t = t.lock().unwrap();
        t.resolved_url = Some(format!(
            "http://127.0.0.1:{}/files/resolved.bin?x=1",
            p.port
        ));
        t.suggested_name = Some("f.bin".into());
    }
    assert_eq!(
        engine::run(t.clone(), ctx(0)).await.unwrap(),
        Outcome::Completed
    );
    assert!(std::fs::read(dir.path().join("f.bin")).unwrap() == data);
    assert!(
        t.lock()
            .unwrap()
            .final_url
            .as_deref()
            .is_some_and(|u| u.ends_with("/files/module.bin"))
    );
    // Neither the resolved link nor the mirror (nor where the bytes came from) is saved.
    let saved = serde_json::to_string(&*t.lock().unwrap()).unwrap();
    assert!(!saved.contains("module.bin"), "{saved}");
    assert!(!saved.contains("resolved"), "{saved}");
    assert!(!saved.contains("final_url"), "{saved}");
}

#[tokio::test]
async fn unusable_mirrors_are_ignored_and_at_most_eight_tried() {
    let data = server::test_data(300_000);
    let p = server::start(data.clone(), true, 0).await;
    p.fail_status.store(500, Ordering::SeqCst);
    let m = server::start(data, true, 0).await;
    m.fail_status.store(503, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let mut mirrors = vec![
        format!("ftp://127.0.0.1:{}/files/f.bin", m.port),
        p.url("f.bin"),
        other_origin(&m, "0.bin"),
        format!(" {} ", other_origin(&m, "0.bin")),
    ];
    mirrors.extend((1..12).map(|i| other_origin(&m, &format!("{i}.bin"))));
    let t = task(p.url("f.bin"), dir.path(), 1, mirrors);
    let e = engine::run(t.clone(), ctx(0)).await.unwrap_err();
    assert_eq!(e, "HTTP 500 Internal Server Error", "the primary's error");
    assert_eq!(m.requests.load(Ordering::SeqCst), 8);
    let log = log(&t);
    assert!(
        log.iter()
            .any(|l| l.starts_with("HTTP source failed; trying mirror 8 of 8 ")),
        "{log:?}"
    );
    assert!(!dir.path().join("f.bin").exists());
}

#[tokio::test]
async fn overwrite_policy_survives_a_fallback_to_a_mirror() {
    let data = server::test_data(1 << 20);
    // The primary dies after picking the name, or before (probe answers 503).
    for (cut, status) in [(300_000, 0), (0, 503)] {
        let p = server::start(data.clone(), true, 0).await;
        p.fail_after.store(cut, Ordering::SeqCst);
        p.fail_status.store(status, Ordering::SeqCst);
        let m = server::start(data.clone(), true, 0).await;
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.bin"), b"old").unwrap();
        let t = task(
            p.url("f.bin"),
            dir.path(),
            1,
            vec![other_origin(&m, "f.bin")],
        );
        t.lock().unwrap().conflict = ConflictPolicy::Overwrite;
        assert_eq!(
            engine::run(t.clone(), ctx(0)).await.unwrap(),
            Outcome::Completed
        );
        assert!(std::fs::read(dir.path().join("f.bin")).unwrap() == data);
        assert!(!dir.path().join("f (1).bin").exists());
        assert_eq!(t.lock().unwrap().file_name, "f.bin");
    }
}

#[tokio::test]
async fn primary_digest_is_kept_when_a_mirror_finishes() {
    use base64::Engine;
    use sha2::Digest;
    let data = server::test_data(1 << 20);
    let p = server::start(data.clone(), true, 0).await;
    let b64 = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(&data));
    p.headers
        .lock()
        .unwrap()
        .push(("Repr-Digest".into(), format!("sha-256=:{b64}:")));
    p.fail_after.store(300_000, Ordering::SeqCst);
    let m = server::start(data.clone(), true, 0).await;
    let dir = tempfile::tempdir().unwrap();
    let t = task(
        p.url("f.bin"),
        dir.path(),
        1,
        vec![other_origin(&m, "f.bin")],
    );
    let mut c = ctx(0);
    c.verify = true;
    assert_eq!(engine::run(t.clone(), c).await.unwrap(), Outcome::Completed);
    let v = t.lock().unwrap().verification.clone().unwrap();
    assert_eq!(v.state, dlcore::VerifyState::Verified);
    assert_eq!(v.source, "header: repr-digest");
}
