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
    t.lock().unwrap().checksum = Some(Checksum { algo: Algo::Sha256, expected: good });
    assert_eq!(engine::run(t, ctx(0)).await.unwrap(), Outcome::Completed);

    let md5 = hex::encode(md5::Md5::digest(&data));
    let t = task(s.url("ok2.bin"), dir.path(), 2);
    t.lock().unwrap().checksum = Checksum::guess(&md5);
    assert_eq!(engine::run(t, ctx(0)).await.unwrap(), Outcome::Completed);

    let t = task(s.url("bad.bin"), dir.path(), 2);
    t.lock().unwrap().checksum = Some(Checksum { algo: Algo::Md5, expected: "0".repeat(32) });
    let e = engine::run(t, ctx(0)).await.unwrap_err();
    assert!(e.contains("checksum mismatch"), "{e}");
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
