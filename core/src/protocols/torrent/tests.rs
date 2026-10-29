mod fixture;

use fixture::Swarm;
use super::*;
use crate::limiter::RateLimiter;
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

fn context(global: Arc<RateLimiter>, local: Arc<RateLimiter>) -> Ctx {
    Ctx {
        client: reqwest::Client::new(),
        protocol_client: reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap(),
        global,
        local,
        retries: 1,
        cancel: CancellationToken::new(),
        verify: true,
    }
}

fn task(source: String) -> Arc<Mutex<Task>> {
    Arc::new(Mutex::new(Task {
        url: source,
        connections: 4,
        ..Default::default()
    }))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn magnet_metadata_and_multifile_via_embedded_peer() {
    let swarm = Swarm::new(true, 256 * 1024 + 311).await;
    let stage = tempfile::tempdir().unwrap();
    let task = task(swarm.magnet.clone());
    let ctx = context(Arc::new(RateLimiter::new(0)), Arc::new(RateLimiter::new(0)));
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(30),
            download(task.clone(), ctx, stage.path())
        )
        .await
        .unwrap()
        .unwrap(),
        Outcome::Completed
    );
    assert_eq!(task.lock().unwrap().downloaded(), 256 * 1024 + 311);
    swarm.check(stage.path()).await;
    // A storage handle retained by the completed session would block this
    // exclusive Windows open and prevent facade publication.
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(stage.path().join("payload/a.bin"))
            .unwrap();
    }
    swarm.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_descriptor_and_single_file_via_tracker() {
    let swarm = Swarm::new(false, 96 * 1024 + 173).await;
    let stage = tempfile::tempdir().unwrap();
    let ctx = context(Arc::new(RateLimiter::new(0)), Arc::new(RateLimiter::new(0)));
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(30),
            download(
                task(format!("{}/file.torrent", swarm.http)),
                ctx,
                stage.path()
            )
        )
        .await
        .unwrap()
        .unwrap(),
        Outcome::Completed
    );
    swarm.check(stage.path()).await;
    swarm.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn changed_descriptor_discards_stale_payload_before_publication() {
    let previous = Swarm::new(true, 32 * 1024 + 11).await;
    let swarm = Swarm::new(false, 48 * 1024 + 13).await;
    let directory = tempfile::tempdir().unwrap();
    let task = Arc::new(Mutex::new(Task {
        url: format!("{}/file.torrent", swarm.http),
        dir: directory.path().to_owned(),
        suggested_name: Some("file.torrent".into()),
        connections: 4,
        ..Default::default()
    }));
    let stage = crate::protocols::staging_path(&task.lock().unwrap());
    tokio::fs::create_dir_all(stage.join("state"))
        .await
        .unwrap();
    tokio::fs::create_dir_all(stage.join("payload/nested"))
        .await
        .unwrap();
    tokio::fs::write(stage.join("state/metadata.torrent"), &previous.torrent)
        .await
        .unwrap();
    for (name, bytes) in &previous.files {
        tokio::fs::write(stage.join("payload").join(name), bytes)
            .await
            .unwrap();
    }
    previous.stop().await;
    let ctx = context(Arc::new(RateLimiter::new(0)), Arc::new(RateLimiter::new(0)));
    assert_eq!(
        tokio::time::timeout(
            Duration::from_secs(30),
            crate::protocols::run(task.clone(), ctx)
        )
        .await
        .unwrap()
        .unwrap(),
        Outcome::Completed
    );
    let output = {
        let task = task.lock().unwrap();
        assert_eq!(task.file_name, "fixture.bin");
        task.final_path()
    };
    assert_eq!(tokio::fs::read(output).await.unwrap(), swarm.files[0].1);
    assert!(!stage.exists());
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    swarm.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancel_then_resume_rehashes_corrupt_existing_piece() {
    let swarm = Swarm::new(false, 512 * 1024 + 43).await;
    let stage = tempfile::tempdir().unwrap();
    let descriptor = stage.path().join("fixture.torrent");
    tokio::fs::write(&descriptor, &swarm.torrent).await.unwrap();
    let task = task(descriptor.to_string_lossy().into_owned());
    let ctx = context(
        Arc::new(RateLimiter::new(0)),
        Arc::new(RateLimiter::new(64 * 1024)),
    );
    let cancel = ctx.cancel.clone();
    let path = stage.path().to_owned();
    let running_task = task.clone();
    let running = tokio::spawn(async move { download(running_task, ctx, &path).await });
    tokio::time::timeout(Duration::from_secs(15), async {
        while task.lock().unwrap().downloaded() < 16 * 1024 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Outcome::Cancelled
    );
    let output = stage.path().join("payload/fixture.bin");
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(&output)
            .unwrap();
    }
    let mut bytes = tokio::fs::read(&output).await.unwrap();
    let before = Sha256::digest(&bytes);
    tokio::time::sleep(Duration::from_millis(350)).await;
    assert_eq!(
        before,
        Sha256::digest(tokio::fs::read(&output).await.unwrap())
    );
    bytes[0] ^= 0xff;
    tokio::fs::write(&output, bytes).await.unwrap();
    let ctx = context(Arc::new(RateLimiter::new(0)), Arc::new(RateLimiter::new(0)));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(30), download(task, ctx, stage.path()))
            .await
            .unwrap()
            .unwrap(),
        Outcome::Completed
    );
    swarm.check(stage.path()).await;
    swarm.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_torrents_share_global_bandwidth_debt() {
    let swarm = Swarm::new(false, 128 * 1024 + 43).await;
    let first = tempfile::tempdir().unwrap();
    let second = tempfile::tempdir().unwrap();
    let global = Arc::new(RateLimiter::new(64 * 1024));
    let start = Instant::now();
    let ctx1 = context(global.clone(), Arc::new(RateLimiter::new(0)));
    let ctx2 = context(global, Arc::new(RateLimiter::new(0)));
    let (a, b) = tokio::time::timeout(Duration::from_secs(40), async {
        tokio::join!(
            download(task(swarm.magnet.clone()), ctx1, first.path()),
            download(task(swarm.magnet.clone()), ctx2, second.path())
        )
    })
    .await
    .unwrap();
    assert_eq!(a.unwrap(), Outcome::Completed);
    assert_eq!(b.unwrap(), Outcome::Completed);
    assert!(
        // 256 KiB / 64 KiB/s minus the shared 16 KiB burst, followed
        // by session.stop()'s one-second close. Independent per-session
        // caps could finish around three seconds and must fail this gate.
        start.elapsed() >= Duration::from_millis(4400),
        "two downloads bypassed the shared bandwidth bucket: {:?}",
        start.elapsed()
    );
    swarm.check(first.path()).await;
    swarm.check(second.path()).await;
    swarm.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn live_rate_changes_keep_independent_global_and_local_debts() {
    let swarm = Swarm::new(false, 64 * 1024 + 17).await;
    let stage = tempfile::tempdir().unwrap();
    // Below the upstream 16 KiB block size: shared debt pacing must work
    // without asking governor for an impossible undersized burst.
    let global = Arc::new(RateLimiter::new(4096));
    let local = Arc::new(RateLimiter::new(2048));
    let ctx = context(global.clone(), local.clone());
    let task = task(swarm.magnet.clone());
    let running_task = task.clone();
    let path = stage.path().to_owned();
    let running = tokio::spawn(async move { download(running_task, ctx, &path).await });
    tokio::time::timeout(Duration::from_secs(10), async {
        while task.lock().unwrap().downloaded() == 0 {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    global.set_rate(0);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        !running.is_finished(),
        "changing the global limit discarded local bandwidth debt"
    );
    local.set_rate(0);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Outcome::Completed
    );
    swarm.check(stage.path()).await;
    swarm.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn corrupt_network_pieces_are_rejected() {
    let swarm = Swarm::new(false, 32 * 1024 + 17).await;
    // The seeded torrent was hash-checked above. Change the backing bytes
    // afterwards so this real peer transmits data inconsistent with its
    // metainfo; the receiver must reject it rather than report completion.
    let corrupt = swarm.files[0]
        .1
        .iter()
        .map(|byte| byte ^ 0xff)
        .collect::<Vec<_>>();
    tokio::fs::write(swarm._root.path().join("fixture.bin"), corrupt)
        .await
        .unwrap();
    let stage = tempfile::tempdir().unwrap();
    let task = task(swarm.magnet.clone());
    let ctx = context(Arc::new(RateLimiter::new(0)), Arc::new(RateLimiter::new(0)));
    let cancel = ctx.cancel.clone();
    let path = stage.path().to_owned();
    let running_task = task.clone();
    let running = tokio::spawn(async move { download(running_task, ctx, &path).await });
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let sent = swarm.session.with_torrents(|torrents| {
                torrents
                    .map(|(_, handle)| handle.stats().uploaded_bytes)
                    .sum::<u64>()
            });
            if sent >= 16 * 1024 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(
        !running.is_finished(),
        "corrupt peer content was accepted as complete"
    );
    assert_eq!(
        task.lock().unwrap().downloaded(),
        0,
        "corrupt pieces contributed verified progress"
    );
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Outcome::Cancelled
    );
    swarm.stop().await;
}

#[tokio::test]
async fn already_cancelled_creates_no_session_or_payload() {
    let stage = tempfile::tempdir().unwrap();
    let ctx = context(Arc::new(RateLimiter::new(0)), Arc::new(RateLimiter::new(0)));
    ctx.cancel.cancel();
    assert_eq!(
        download(
            task("magnet:?xt=urn:btih:0000000000000000000000000000000000000000".into()),
            ctx,
            stage.path()
        )
        .await
        .unwrap(),
        Outcome::Cancelled
    );
    assert_eq!(std::fs::read_dir(stage.path()).unwrap().count(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancellation_interrupts_stalled_magnet_metadata_exchange() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (accepted, receiver) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        accepted.send(()).unwrap();
        std::future::pending::<()>().await;
        drop(socket);
    });
    let stage = tempfile::tempdir().unwrap();
    let ctx = context(Arc::new(RateLimiter::new(0)), Arc::new(RateLimiter::new(0)));
    let cancel = ctx.cancel.clone();
    let path = stage.path().to_owned();
    let task = task(format!(
        "magnet:?xt=urn:btih:0000000000000000000000000000000000000001&x.pe={address}"
    ));
    let running = tokio::spawn(async move { download(task, ctx, &path).await });
    tokio::time::timeout(Duration::from_secs(5), receiver)
        .await
        .unwrap()
        .unwrap();
    cancel.cancel();
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), running)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        Outcome::Cancelled
    );
    assert_eq!(
        std::fs::read_dir(stage.path().join("payload"))
            .unwrap()
            .count(),
        0
    );
    peer.abort();
}
