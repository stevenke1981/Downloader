mod server;

use dlcore::{Algo, Checksum, Ctx, Outcome, RateLimiter, Settings, Status, Task, engine};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

fn ctx(limit: u64) -> Ctx {
    Ctx {
        client: Settings::default().build_client().unwrap(),
        global: Arc::new(RateLimiter::new(0)),
        local: Arc::new(RateLimiter::new(limit)),
        retries: 2,
        cancel: CancellationToken::new(),
        verify: false,
    }
}

fn task(url: String, dir: &std::path::Path, conns: u32) -> Arc<Mutex<Task>> {
    Arc::new(Mutex::new(Task { url, dir: dir.into(), connections: conns, status: Status::Downloading, ..Default::default() }))
}

#[tokio::test]
async fn multi_connection_range_download() {
    let data = server::test_data(8 << 20);
    let s = server::start(data.clone(), true, 0).await;
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("big.bin"), dir.path(), 8);
    assert_eq!(engine::run(t.clone(), ctx(0)).await.unwrap(), Outcome::Completed);
    let got = std::fs::read(dir.path().join("big.bin")).unwrap();
    assert!(got == data, "content mismatch");
    assert!(!dir.path().join("big.bin.part").exists());
    assert!(s.range_requests.lock().unwrap().len() >= 8, "expected >=8 range requests");
}

#[tokio::test]
async fn invalid_partial_responses_never_publish_a_completed_file() {
    for header in ["", "garbage", "bytes 1-1000/1001", "bytes 0-999/1001", "bytes 1-1000/*", "bytes 0-998/1000"] {
        let s = server::start(server::test_data(1000), true, 0).await;
        *s.data_content_range.lock().unwrap() = Some(header.into());
        let dir = tempfile::tempdir().unwrap();
        let t = task(s.url("bad-range.bin"), dir.path(), 1);
        let error = engine::run(t.clone(), ctx(0)).await.unwrap_err();
        assert!(error.contains("Content-Range"), "{header}: {error}");
        assert_eq!(t.lock().unwrap().downloaded(), 0, "invalid data must not be written");
        assert!(!dir.path().join("bad-range.bin").exists());
    }
}

#[tokio::test]
async fn partial_response_may_omit_known_total() {
    let data = server::test_data(1000);
    let s = server::start(data.clone(), true, 0).await;
    *s.data_content_range.lock().unwrap() = Some("bytes 0-999/*".into());
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("unknown-total.bin"), dir.path(), 1);
    assert_eq!(engine::run(t, ctx(0)).await.unwrap(), Outcome::Completed);
    assert_eq!(std::fs::read(dir.path().join("unknown-total.bin")).unwrap(), data);
}

#[tokio::test]
async fn shorter_valid_range_prefixes_continue_without_retry_backoff() {
    let data = server::test_data(1000);
    let s = server::start(data.clone(), true, 0).await;
    s.max_range_bytes.store(256, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("prefix.bin"), dir.path(), 1);
    let mut c = ctx(0);
    c.retries = 0; // A valid prefix is progress, not a retry failure.
    assert_eq!(engine::run(t, c).await.unwrap(), Outcome::Completed);
    assert_eq!(std::fs::read(dir.path().join("prefix.bin")).unwrap(), data);
    let ranges = s.range_requests.lock().unwrap();
    assert!(ranges.iter().any(|r| r == "bytes=256-999"));
    assert!(ranges.iter().any(|r| r == "bytes=768-999"));
}

#[tokio::test]
async fn wrong_range_on_resume_preserves_existing_partial_bytes() {
    let data = server::test_data(1000);
    let s = server::start(data.clone(), true, 0).await;
    // Correct response length but cached bytes from a different offset.
    *s.data_content_range.lock().unwrap() = Some("bytes 0-899/1000".into());
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("resume-range.bin"), dir.path(), 1);
    {
        let mut g = t.lock().unwrap();
        g.file_name = "resume-range.bin".into();
        g.total = Some(1000);
        g.resumable = true;
        g.etag = Some("\"v1\"".into());
        g.segments = vec![dlcore::Seg { start: 0, pos: 100, end: 1000, busy: false }];
        std::fs::write(g.part_path(), &data[..100]).unwrap();
    }
    let error = engine::run(t.clone(), ctx(0)).await.unwrap_err();
    assert!(error.contains("Content-Range"), "{error}");
    assert_eq!(std::fs::read(t.lock().unwrap().part_path()).unwrap(), data[..100]);
    assert_eq!(t.lock().unwrap().downloaded(), 100);
    assert!(!dir.path().join("resume-range.bin").exists());
}

#[tokio::test]
async fn work_stealing_splits_slow_segments() {
    // Throttled server: connections finishing early must split remaining segments.
    let data = server::test_data(3 << 20);
    let s = server::start(data.clone(), true, 2 << 20).await;
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("s.bin"), dir.path(), 4);
    {
        // Start from an uneven plan: one huge segment and one tiny segment.
        let mut g = t.lock().unwrap();
        g.file_name = "s.bin".into();
        g.total = Some(data.len() as u64);
        g.segments = vec![dlcore::Seg::new(0, 300_000), dlcore::Seg::new(300_000, data.len() as u64)];
        g.resumable = true;
        g.etag = Some("\"v1\"".into());
        std::fs::write(g.part_path(), b"").unwrap();
        std::fs::File::options().write(true).open(g.part_path()).unwrap().set_len(data.len() as u64).unwrap();
        // mark one byte done so the part is treated as resumable
        g.segments[0].pos = 0;
    }
    assert_eq!(engine::run(t.clone(), ctx(0)).await.unwrap(), Outcome::Completed);
    assert!(std::fs::read(dir.path().join("s.bin")).unwrap() == data);
    assert!(t.lock().unwrap().segments.len() > 2, "no dynamic splitting happened");
    assert!(s.max_parallel.load(Ordering::SeqCst) >= 3);
}

#[tokio::test]
async fn server_without_range_uses_single_connection() {
    let data = server::test_data(3 << 20);
    let s = server::start(data.clone(), false, 0).await;
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("plain.dat"), dir.path(), 8);
    assert_eq!(engine::run(t.clone(), ctx(0)).await.unwrap(), Outcome::Completed);
    assert!(std::fs::read(dir.path().join("plain.dat")).unwrap() == data);
    assert!(!t.lock().unwrap().resumable);
    assert_eq!(s.max_parallel.load(Ordering::SeqCst), 1);
    // probe + one full GET
    assert_eq!(s.requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn pause_and_resume_continues_from_saved_state() {
    let data = server::test_data(4 << 20);
    let s = server::start(data.clone(), true, 1 << 20).await; // 1 MB/s per connection
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("r.bin"), dir.path(), 2);
    let c = ctx(0);
    let cancel = c.cancel.clone();
    let job = tokio::spawn(engine::run(t.clone(), c));
    tokio::time::sleep(Duration::from_millis(900)).await;
    cancel.cancel();
    assert_eq!(job.await.unwrap().unwrap(), Outcome::Cancelled);
    // Persist and reload, as the app does across restarts.
    let saved = serde_json::to_string(&*t.lock().unwrap()).unwrap();
    let reloaded: Task = serde_json::from_str(&saved).unwrap();
    let done = reloaded.downloaded();
    assert!(done > 0 && done < data.len() as u64, "downloaded {done}");
    assert!(dir.path().join("r.bin.part").exists());
    let sent_before = s.bytes_sent.load(Ordering::SeqCst);
    let t2 = Arc::new(Mutex::new(reloaded));
    assert_eq!(engine::run(t2, ctx(0)).await.unwrap(), Outcome::Completed);
    assert!(std::fs::read(dir.path().join("r.bin")).unwrap() == data);
    let resent = s.bytes_sent.load(Ordering::SeqCst) - sent_before;
    assert!(resent < data.len() as u64 - done / 2, "resume re-downloaded too much: {resent}");
    assert!(s.range_requests.lock().unwrap().iter().any(|r| !r.starts_with("bytes=0-")));
}

#[tokio::test]
async fn resume_restarts_when_file_changed_on_server() {
    let data = server::test_data(4 << 20);
    let s = server::start(data.clone(), true, 1 << 20).await;
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("c.bin"), dir.path(), 2);
    let c = ctx(0);
    let cancel = c.cancel.clone();
    let job = tokio::spawn(engine::run(t.clone(), c));
    tokio::time::sleep(Duration::from_millis(600)).await;
    cancel.cancel();
    job.await.unwrap().unwrap();
    let new_data: Vec<u8> = data.iter().map(|b| b.wrapping_add(1)).collect();
    s.set_data(new_data.clone(), "\"v2\"");
    assert_eq!(engine::run(t.clone(), ctx(0)).await.unwrap(), Outcome::Completed);
    assert!(std::fs::read(dir.path().join("c.bin")).unwrap() == new_data);
}

#[tokio::test]
async fn speed_limit_is_respected() {
    let data = server::test_data(1 << 20);
    let s = server::start(data.clone(), true, 0).await;
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("slow.bin"), dir.path(), 4);
    let start = Instant::now();
    assert_eq!(engine::run(t, ctx(512 * 1024)).await.unwrap(), Outcome::Completed);
    let secs = start.elapsed().as_secs_f64();
    // 1 MiB at 512 KiB/s should take ~2s
    assert!(secs > 1.6 && secs < 4.0, "took {secs:.2}s");
    assert!(std::fs::read(dir.path().join("slow.bin")).unwrap() == data);
}

#[tokio::test]
async fn checksum_verification() {
    let data = server::test_data(600_000);
    let s = server::start(data.clone(), true, 0).await;
    let dir = tempfile::tempdir().unwrap();
    use sha2::Digest;
    let good = hex::encode(sha2::Sha256::digest(&data));
    let t = task(s.url("ok.bin"), dir.path(), 2);
    t.lock().unwrap().checksum = Some(Checksum { algo: Algo::Sha256, expected: good, source: "user".into() });
    assert_eq!(engine::run(t, ctx(0)).await.unwrap(), Outcome::Completed);

    let md5 = hex::encode(md5::Md5::digest(&data));
    let t = task(s.url("ok2.bin"), dir.path(), 2);
    t.lock().unwrap().checksum = Checksum::guess(&md5);
    assert_eq!(engine::run(t, ctx(0)).await.unwrap(), Outcome::Completed);

    let t = task(s.url("bad.bin"), dir.path(), 2);
    t.lock().unwrap().checksum = Some(Checksum { algo: Algo::Md5, expected: "0".repeat(32), source: "user".into() });
    let e = engine::run(t, ctx(0)).await.unwrap_err();
    assert!(e.contains("verification failed"), "{e}");
}

#[tokio::test]
async fn unique_name_when_file_exists() {
    let data = server::test_data(1000);
    let s = server::start(data.clone(), true, 0).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("dup.txt"), b"old").unwrap();
    let t = task(s.url("dup.txt"), dir.path(), 4);
    engine::run(t.clone(), ctx(0)).await.unwrap();
    assert_eq!(t.lock().unwrap().file_name, "dup (1).txt");
    assert!(std::fs::read(dir.path().join("dup (1).txt")).unwrap() == data);
}

#[tokio::test]
async fn cookie_is_sent_with_every_request() {
    let data = server::test_data(2 << 20);
    let s = server::start(data.clone(), true, 0).await;
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("cookie.bin"), dir.path(), 4);
    t.lock().unwrap().cookie = Some("sid=abc; uid=42".into());
    assert_eq!(engine::run(t.clone(), ctx(0)).await.unwrap(), Outcome::Completed);
    assert!(std::fs::read(dir.path().join("cookie.bin")).unwrap() == data);
    let seen = s.cookies.lock().unwrap().clone();
    assert_eq!(seen.len(), s.requests.load(Ordering::SeqCst), "every request must be recorded");
    assert!(seen.len() >= 5, "expected probe + segment requests, got {}", seen.len());
    assert!(seen.iter().all(|c| c == "sid=abc; uid=42"), "requests without the cookie: {seen:?}");
}

#[tokio::test]
async fn cookie_not_sent_across_redirect_to_other_origin() {
    let data = server::test_data(2 << 20);
    let origin = server::start(data.clone(), true, 0).await;
    let mirror = server::start(data.clone(), true, 0).await;
    // The origin hands the file to a mirror on another origin ("localhost" is a different host string than 127.0.0.1).
    *origin.redirect.lock().unwrap() = Some(format!("http://localhost:{}/files/", mirror.port));
    let dir = tempfile::tempdir().unwrap();
    let t = task(origin.url("moved.bin"), dir.path(), 4);
    t.lock().unwrap().cookie = Some("sid=secret".into());
    assert_eq!(engine::run(t.clone(), ctx(0)).await.unwrap(), Outcome::Completed);
    assert!(std::fs::read(dir.path().join("moved.bin")).unwrap() == data);
    assert!(origin.cookies.lock().unwrap().iter().any(|c| c == "sid=secret"), "the probe to the origin should carry the cookie");
    let leaked: Vec<String> = mirror.cookies.lock().unwrap().iter().filter(|c| !c.is_empty()).cloned().collect();
    assert!(leaked.is_empty(), "cookie leaked to the redirect target: {} requests", leaked.len());
    assert!(mirror.requests.load(Ordering::SeqCst) >= 2, "segments should have been fetched from the mirror");
}

#[tokio::test]
async fn auth_failure_after_restart_explains_lost_cookie() {
    let data = server::test_data(300_000);
    let s = server::start(data, true, 0).await;
    s.require_cookie.store(true, Ordering::SeqCst);
    let dir = tempfile::tempdir().unwrap();
    // As after a restart: the task remembers a cookie was used, but the value is gone.
    let t = task(s.url("login.bin"), dir.path(), 2);
    t.lock().unwrap().had_cookie = true;
    let e = engine::run(t, ctx(0)).await.unwrap_err();
    assert!(e.starts_with("HTTP 403") && e.contains("login cookies are not kept after a restart"), "{e}");
    // Without a previous cookie the plain error stays as it was.
    let t = task(s.url("login2.bin"), dir.path(), 2);
    let e = engine::run(t, ctx(0)).await.unwrap_err();
    assert_eq!(e, "HTTP 403 Forbidden");
}

#[test]
fn unusable_cookie_is_dropped_and_download_still_succeeds() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let s = rt.block_on(server::start(server::test_data(1 << 20), true, 0));
    let state = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let m = dlcore::Manager::new(state.path());
    let mut st = m.settings();
    st.download_dir = out.path().into();
    m.set_settings(st).unwrap();
    // A control character can't be sent in a header; before validation every request would fail to build.
    let id = m.add(dlcore::NewDownload { url: s.url("ctl.bin"), start: true, cookie: Some("sid=a\u{1}b".into()), ..Default::default() });
    let status = |m: &dlcore::Manager| m.snapshots().iter().find(|x| x.id == id).map(|x| (x.status, x.error.clone())).unwrap();
    let t0 = Instant::now();
    while status(&m).0 != Status::Completed && status(&m).0 != Status::Error && t0.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let (st, err) = status(&m);
    assert_eq!(st, Status::Completed, "error: {err:?}");
    assert_eq!(std::fs::read(out.path().join("ctl.bin")).unwrap().len(), 1 << 20);
    assert!(s.cookies.lock().unwrap().iter().all(|c| c.is_empty()), "the dropped cookie must not be sent");
    m.shutdown();
}

#[test]
fn cookie_value_is_not_persisted_but_had_cookie_is() {
    let state = tempfile::tempdir().unwrap();
    let m = dlcore::Manager::new(state.path());
    // Added paused, so nothing is fetched.
    m.add(dlcore::NewDownload { url: "http://127.0.0.1:9/never.bin".into(), start: false, cookie: Some("sid=secret123".into()), ..Default::default() });
    m.shutdown();
    let saved = std::fs::read_to_string(state.path().join("downloads.json")).unwrap();
    assert!(!saved.contains("secret123"), "cookie value written to disk");
    assert!(saved.contains("\"had_cookie\":true"), "{saved}");
    let m = dlcore::Manager::new(state.path());
    assert_eq!(m.snapshots().len(), 1);
    m.shutdown();
}

#[test]
fn manager_moves_corrupt_downloads_json_aside() {
    let state = tempfile::tempdir().unwrap();
    std::fs::write(state.path().join("downloads.json"), b"{not json").unwrap();
    let m = dlcore::Manager::new(state.path());
    assert!(m.snapshots().is_empty());
    assert!(!state.path().join("downloads.json").exists(), "corrupt file should have been moved");
    assert_eq!(std::fs::read(state.path().join("downloads.json.corrupt")).unwrap(), b"{not json");
    m.shutdown();
}

#[test]
fn manager_keeps_corrupt_downloads_json_when_it_cannot_be_moved() {
    let state = tempfile::tempdir().unwrap();
    std::fs::write(state.path().join("downloads.json"), b"{not json").unwrap();
    // A non-empty directory in the way makes the rename to .corrupt fail on every platform.
    let blocker = state.path().join("downloads.json.corrupt");
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("keep"), b"x").unwrap();
    let m = dlcore::Manager::new(state.path());
    m.add(dlcore::NewDownload { url: "http://127.0.0.1:9/a.bin".into(), start: false, ..Default::default() });
    m.shutdown();
    assert_eq!(std::fs::read(state.path().join("downloads.json")).unwrap(), b"{not json", "original was overwritten");
    let recovered = std::fs::read_to_string(state.path().join("downloads.json.recovered")).unwrap();
    assert!(recovered.contains("a.bin"), "{recovered}");
}

#[test]
fn manager_queue_and_persistence() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let s = rt.block_on(server::start(server::test_data(2 << 20), true, 256 << 10));
    let state = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let m = dlcore::Manager::new(state.path());
    let mut st = m.settings();
    st.download_dir = out.path().into();
    st.max_concurrent = 1;
    m.set_settings(st).unwrap();
    let a = m.add(dlcore::NewDownload { url: s.url("a.bin"), start: true, ..Default::default() });
    let b = m.add(dlcore::NewDownload { url: s.url("b.bin"), start: true, priority: 5, ..Default::default() });
    std::thread::sleep(Duration::from_millis(300));
    let snaps = m.snapshots();
    let st = |id| snaps.iter().find(|x| x.id == id).unwrap().status;
    // only one at a time; `a` was added first and started immediately
    assert_eq!(st(a), Status::Downloading);
    assert_eq!(st(b), Status::Queued);
    m.pause(a);
    std::thread::sleep(Duration::from_millis(1500));
    let done = |m: &dlcore::Manager, id| m.snapshots().iter().find(|x| x.id == id).unwrap().status;
    assert_eq!(done(&m, a), Status::Paused);
    let t0 = Instant::now();
    while done(&m, b) != Status::Completed && t0.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(done(&m, b), Status::Completed);
    m.shutdown();
    // Reload from disk: `a` keeps its partial progress and resumes to completion.
    let m = dlcore::Manager::new(state.path());
    let snap = m.snapshots();
    assert_eq!(snap.len(), 2);
    let pa = snap.iter().find(|x| x.id == a).unwrap();
    assert_eq!(pa.status, Status::Paused);
    assert!(pa.downloaded > 0);
    m.start(a);
    let t0 = Instant::now();
    while done(&m, a) != Status::Completed && t0.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(done(&m, a), Status::Completed);
    assert_eq!(std::fs::read(out.path().join("a.bin")).unwrap().len(), 2 << 20);
    m.shutdown();
}

#[tokio::test]
async fn multi_connection_beats_single_connection_on_capped_server() {
    // Server caps each connection at 1 MiB/s, like many real mirrors do.
    let data = server::test_data(4 << 20);
    let s = server::start(data.clone(), true, 1 << 20).await;
    let dir = tempfile::tempdir().unwrap();
    let mut times = vec![];
    for (i, conns) in [1u32, 8].iter().enumerate() {
        let t = task(s.url(&format!("cap{i}.bin")), dir.path(), *conns);
        let start = Instant::now();
        assert_eq!(engine::run(t, ctx(0)).await.unwrap(), Outcome::Completed);
        times.push(start.elapsed().as_secs_f64());
        assert!(std::fs::read(dir.path().join(format!("cap{i}.bin"))).unwrap() == data);
    }
    let speedup = times[0] / times[1];
    println!("1 connection: {:.2}s, 8 connections: {:.2}s, speedup {speedup:.1}x", times[0], times[1]);
    assert!(speedup > 3.0, "speedup only {speedup:.1}x");
}

// ---------------- verification ----------------
fn vctx() -> Ctx {
    Ctx { verify: true, ..ctx(0) }
}
fn sha256(d: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(d))
}
fn state(t: &Arc<Mutex<Task>>) -> dlcore::Verification {
    t.lock().unwrap().verification.clone().expect("verification recorded")
}

#[tokio::test]
async fn verify_with_sidecar_file() {
    let data = server::test_data(700_000);
    let s = server::start(data.clone(), true, 0).await;
    s.extra.lock().unwrap().insert("tool.tar.zst.sha256".into(), format!("{}\n", sha256(&data)).into_bytes());
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("tool.tar.zst"), dir.path(), 4);
    assert_eq!(engine::run(t.clone(), vctx()).await.unwrap(), Outcome::Completed);
    let v = state(&t);
    assert_eq!(v.state, dlcore::VerifyState::Verified);
    assert_eq!(v.algo, Some(Algo::Sha256));
    assert!(v.source.ends_with("tool.tar.zst.sha256"), "{}", v.source);
}

#[tokio::test]
async fn verify_with_gnu_sums_list() {
    let data = server::test_data(500_000);
    let s = server::start(data.clone(), true, 0).await;
    use blake2::Digest;
    let b2 = hex::encode(blake2::Blake2b512::digest(&data));
    s.extra.lock().unwrap().insert(
        "b2sums.txt".into(),
        format!("{}  other.iso\n{b2} *arch-x86_64.iso\n", "0".repeat(128)).into_bytes(),
    );
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("arch-x86_64.iso"), dir.path(), 4);
    assert_eq!(engine::run(t.clone(), vctx()).await.unwrap(), Outcome::Completed);
    let v = state(&t);
    assert_eq!(v.state, dlcore::VerifyState::Verified);
    assert_eq!(v.algo, Some(Algo::Blake2b));
}

#[tokio::test]
async fn verify_with_bsd_sums_list_and_signature() {
    let data = server::test_data(400_000);
    let s = server::start(data.clone(), true, 0).await;
    use sha2::Digest;
    let h = hex::encode(sha2::Sha512::digest(&data));
    s.extra.lock().unwrap().insert("SHA512SUMS".into(), format!("SHA512 (pkg.zip) = {h}\n").into_bytes());
    s.extra.lock().unwrap().insert("pkg.zip.sig".into(), b"\x89\x02\x33fake".to_vec());
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("pkg.zip"), dir.path(), 2);
    assert_eq!(engine::run(t.clone(), vctx()).await.unwrap(), Outcome::Completed);
    let v = state(&t);
    assert_eq!(v.state, dlcore::VerifyState::Verified);
    assert_eq!(v.algo, Some(Algo::Sha512));
    assert!(v.signature.unwrap().ends_with("pkg.zip.sig"));
}

#[tokio::test]
async fn verify_with_digest_header() {
    let data = server::test_data(300_000);
    let s = server::start(data.clone(), true, 0).await;
    use base64::Engine;
    use sha2::Digest;
    let b64 = base64::engine::general_purpose::STANDARD.encode(sha2::Sha256::digest(&data));
    s.headers.lock().unwrap().push(("Repr-Digest".into(), format!("sha-256=:{b64}:")));
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("blob.bin"), dir.path(), 2);
    assert_eq!(engine::run(t.clone(), vctx()).await.unwrap(), Outcome::Completed);
    let v = state(&t);
    assert_eq!(v.state, dlcore::VerifyState::Verified);
    assert_eq!(v.source, "header: repr-digest");
}

#[tokio::test]
async fn mismatch_triggers_one_full_redownload_then_succeeds() {
    let data = server::test_data(600_000);
    let s = server::start(data.clone(), true, 0).await;
    s.extra.lock().unwrap().insert("f.bin.sha256".into(), sha256(&data).into_bytes());
    s.corrupt.store(1, Ordering::SeqCst); // first data response is corrupted
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("f.bin"), dir.path(), 1);
    assert_eq!(engine::run(t.clone(), vctx()).await.unwrap(), Outcome::Completed);
    assert_eq!(state(&t).state, dlcore::VerifyState::Verified);
    assert!(t.lock().unwrap().verify_retried);
    assert_eq!(s.data_requests.load(Ordering::SeqCst), 2, "downloaded twice");
    assert!(std::fs::read(dir.path().join("f.bin")).unwrap() == data);
}

#[tokio::test]
async fn persistent_mismatch_fails_after_one_retry() {
    let data = server::test_data(300_000);
    let s = server::start(data.clone(), true, 0).await;
    s.extra.lock().unwrap().insert("SHA256SUMS".into(), format!("{}  bad.bin\n", "1".repeat(64)).into_bytes());
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("bad.bin"), dir.path(), 1);
    let e = engine::run(t.clone(), vctx()).await.unwrap_err();
    assert!(e.contains("verification failed"), "{e}");
    assert_eq!(state(&t).state, dlcore::VerifyState::Failed);
    assert_eq!(s.data_requests.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn no_checksum_available() {
    let data = server::test_data(200_000);
    let s = server::start(data.clone(), true, 0).await;
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("plain.dat"), dir.path(), 2);
    assert_eq!(engine::run(t.clone(), vctx()).await.unwrap(), Outcome::Completed);
    let v = state(&t);
    assert_eq!(v.state, dlcore::VerifyState::NoChecksum);
    assert!(v.signature.is_none());
}

#[tokio::test]
async fn md5_etag_is_advisory() {
    let data = server::test_data(100_000);
    let s = server::start(data.clone(), true, 0).await;
    *s.etag.lock().unwrap() = format!("\"{}\"", "a".repeat(32)); // not the real MD5
    let dir = tempfile::tempdir().unwrap();
    let t = task(s.url("e.bin"), dir.path(), 2);
    assert_eq!(engine::run(t.clone(), vctx()).await.unwrap(), Outcome::Completed);
    assert_eq!(state(&t).state, dlcore::VerifyState::Unverified);
}
