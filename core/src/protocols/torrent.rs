//! Embedded BitTorrent v1 engine. Payload and control data have separate roots.
use crate::engine::{Ctx, Outcome};
use crate::task::{Seg, Task};
use futures_util::StreamExt;
use librqbit::{
    AddTorrent, AddTorrentOptions, AddTorrentResponse, DhtSessionConfig, Session, SessionOptions,
};
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::path::{Component, Path};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::time::Instant;

const MAX_DESCRIPTOR: usize = 8 * 1024 * 1024;
const METADATA_TIMEOUT: Duration = Duration::from_secs(120);
const STALL_TIMEOUT: Duration = Duration::from_secs(300);
const POLL: Duration = Duration::from_millis(250);

/// Bound each active sampling window; the shared buckets below account fetched
/// bytes and pause the torrent while paying its bandwidth debt.
fn session_rate(ctx: &Ctx) -> Option<NonZeroU32> {
    let rate = match (ctx.global.rate(), ctx.local.rate()) {
        (0, rate) | (rate, 0) => rate,
        (global, local) => global.min(local),
    };
    // librqbit requests 16 KiB blocks. Its governor quota rejects a block larger
    // than a one-second burst instead of splitting the reservation.
    NonZeroU32::new(if rate == 0 {
        0
    } else {
        rate.clamp(16 * 1024, u32::MAX as u64) as u32
    })
}

pub(crate) async fn download(
    task: Arc<Mutex<Task>>,
    ctx: Ctx,
    stage: &Path,
) -> Result<Outcome, String> {
    if ctx.cancel.is_cancelled() {
        return Ok(Outcome::Cancelled);
    }
    let snapshot = task.lock().unwrap().clone();
    let payload = stage.join("payload");
    let state = stage.join("state");
    tokio::fs::create_dir_all(&payload)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::create_dir_all(&state)
        .await
        .map_err(|e| e.to_string())?;
    let initial_rate = session_rate(&ctx);
    let source = snapshot.url.trim();
    let mut initial_peers = Vec::<SocketAddr>::new();
    let mut local_discovery = false;
    let descriptor = if source.starts_with("magnet:") {
        let magnet =
            librqbit::Magnet::parse(source).map_err(|e| format!("invalid magnet: {e:#}"))?;
        let hash = magnet
            .as_id20()
            .ok_or("only BitTorrent v1/hybrid magnets are supported")?;
        let url = reqwest::Url::parse(source).map_err(|e| e.to_string())?;
        for (_, value) in url.query_pairs().filter(|(key, _)| key == "x.pe") {
            initial_peers.push(value.parse().map_err(|_| "invalid magnet x.pe address")?);
        }
        // An explicitly loopback-only swarm is useful for offline peers and
        // tests. All ordinary public magnets retain public DHT discovery.
        local_discovery = (!initial_peers.is_empty() || !magnet.trackers.is_empty())
            && initial_peers.iter().all(|p| p.ip().is_loopback())
            && magnet.trackers.iter().all(|tracker| {
                reqwest::Url::parse(tracker)
                    .ok()
                    .and_then(|u| u.host_str()?.parse::<std::net::IpAddr>().ok())
                    .is_some_and(|ip| ip.is_loopback())
            });
        match tokio::fs::read(state.join("metadata.torrent")).await {
            Ok(bytes)
                if bytes.len() <= MAX_DESCRIPTOR
                    && librqbit::torrent_from_bytes(&bytes).is_ok_and(|t| t.info_hash == hash) =>
            {
                Some(bytes)
            }
            _ => None,
        }
    } else {
        let read = async {
            if let Some(path) = crate::util::descriptor_path(source) {
                let size = tokio::fs::metadata(&path)
                    .await
                    .map_err(|e| format!("cannot open torrent: {e}"))?
                    .len();
                if size > MAX_DESCRIPTOR as u64 {
                    return Err("torrent descriptor exceeds 8 MiB".to_owned());
                }
                return tokio::fs::read(path)
                    .await
                    .map_err(|e| format!("cannot read torrent: {e}"));
            }
            let original = reqwest::Url::parse(source).map_err(|e| e.to_string())?;
            let mut url = original.clone();
            if !matches!(url.scheme(), "http" | "https") {
                return Err(
                    "torrent source requires a .torrent file, HTTP(S), or magnet URL".into(),
                );
            }
            let mut redirects = 0;
            let response = loop {
                let mut request = ctx.protocol_client.get(url.clone());
                let same_origin = url.origin() == original.origin();
                if same_origin {
                    if let Some(value) = &snapshot.referer {
                        request = request.header(reqwest::header::REFERER, value);
                    }
                    if let Some(value) = &snapshot.cookie {
                        request = request.header(reqwest::header::COOKIE, value);
                    }
                    if let Some(value) = &snapshot.authorization {
                        request = request.header(reqwest::header::AUTHORIZATION, value);
                    }
                }
                let response = request
                    .send()
                    .await
                    .map_err(|e| format!("torrent descriptor request failed: {e}"))?;
                if response.status().is_redirection() {
                    if redirects >= 5 {
                        return Err("torrent descriptor exceeded five redirects".into());
                    }
                    let location = response
                        .headers()
                        .get(reqwest::header::LOCATION)
                        .and_then(|h| h.to_str().ok())
                        .ok_or("torrent descriptor redirect has no valid Location")?;
                    let next = url
                        .join(location)
                        .map_err(|e| format!("invalid torrent descriptor redirect: {e}"))?;
                    if !matches!(next.scheme(), "http" | "https")
                        || next.host_str().is_none()
                        || !next.username().is_empty()
                        || next.password().is_some()
                    {
                        return Err(
                            "torrent descriptor redirect must use HTTP(S) without URL credentials"
                                .into(),
                        );
                    }
                    if url.scheme() == "https" && next.scheme() != "https" {
                        return Err(
                            "torrent descriptor redirect attempted an HTTPS downgrade".into()
                        );
                    }
                    url = next;
                    redirects += 1;
                    continue;
                }
                break response
                    .error_for_status()
                    .map_err(|e| format!("torrent descriptor HTTP error: {e}"))?;
            };
            if response
                .content_length()
                .is_some_and(|n| n > MAX_DESCRIPTOR as u64)
            {
                return Err("torrent descriptor exceeds 8 MiB".into());
            }
            let mut bytes = Vec::new();
            let mut stream = response.bytes_stream();
            while let Some(chunk) = stream.next().await {
                let chunk = chunk.map_err(|e| format!("torrent descriptor read failed: {e}"))?;
                if bytes.len().saturating_add(chunk.len()) > MAX_DESCRIPTOR {
                    return Err("torrent descriptor exceeds 8 MiB".into());
                }
                let wait = ctx
                    .global
                    .reserve(chunk.len() as u64)
                    .max(ctx.local.reserve(chunk.len() as u64));
                tokio::time::sleep(wait).await;
                bytes.extend_from_slice(&chunk);
            }
            Ok(bytes)
        };
        Some(tokio::select! {
            _ = ctx.cancel.cancelled() => return Ok(Outcome::Cancelled),
            result = tokio::time::timeout(METADATA_TIMEOUT, read) => result.map_err(|_| "torrent descriptor timed out")??,
        })
    };
    if let Some(bytes) = &descriptor {
        let torrent = librqbit::torrent_from_bytes(bytes)
            .map_err(|e| format!("invalid torrent descriptor: {e}"))?;
        if torrent.info.data.private {
            local_discovery = true;
        }
        let trackers = torrent.iter_announce().collect::<Vec<_>>();
        if !trackers.is_empty()
            && trackers.iter().all(|tracker| {
                std::str::from_utf8(tracker.as_ref())
                    .ok()
                    .and_then(|t| reqwest::Url::parse(t).ok())
                    .and_then(|u| u.host_str()?.parse::<std::net::IpAddr>().ok())
                    .is_some_and(|ip| ip.is_loopback())
            })
        {
            local_discovery = true;
        }
    }
    let session_cancel = ctx.cancel.child_token();
    let _session_cancel_guard = session_cancel.clone().drop_guard();
    let session = tokio::select! {
        _ = ctx.cancel.cancelled() => return Ok(Outcome::Cancelled),
        result = tokio::time::timeout(METADATA_TIMEOUT, Session::new_with_opts(payload.clone(), SessionOptions {
            dht: (!local_discovery).then(|| DhtSessionConfig { persistence: None, ..Default::default() }),
            // Never trust persisted completion bits: every restart hashes pieces.
            fastresume: false,
            persistence: None,
            disable_local_service_discovery: true,
            peer_limit: Some(snapshot.connections.clamp(1, 128) as usize),
            cancellation_token: Some(session_cancel),
            ratelimits: librqbit::limits::LimitsConfig { download_bps: initial_rate, ..Default::default() },
            ..Default::default()
        })) => result.map_err(|_| "torrent session initialization timed out")?
            .map_err(|e| format!("cannot start embedded torrent session: {e:#}"))?,
    };
    {
        let mut task = task.lock().unwrap();
        task.resumable = true;
        task.error = None;
        task.total = None;
        task.segments = vec![Seg::new(0, u64::MAX)];
        task.append_log("Embedded Rust BitTorrent engine started; resume rechecks piece hashes");
        task.append_log("Shared bandwidth buckets pace fetched bytes every 250 ms by pausing peers; in-flight blocks may briefly exceed the cap");
    }
    // Keep all returns after session creation inside this block so every path
    // cancels peers and closes storage before handing payload to the publisher.
    let result = async {
        let add = match descriptor {
            Some(bytes) => AddTorrent::from_bytes(bytes),
            None => AddTorrent::from_url(source),
        };
        let mut resolve = session.add_torrent(add, Some(AddTorrentOptions {
            list_only: true,
            initial_peers: Some(initial_peers.clone()),
            ..Default::default()
        }));
        let deadline = Instant::now() + METADATA_TIMEOUT;
        let mut tick = tokio::time::interval(POLL);
        let metadata = loop {
            tokio::select! {
                _ = ctx.cancel.cancelled() => return Ok(Outcome::Cancelled),
                _ = tokio::time::sleep_until(deadline) => return Err("torrent metadata exchange timed out after 120 seconds".into()),
                result = &mut resolve => break match result.map_err(|e| format!("torrent metadata failed: {e:#}"))? {
                    AddTorrentResponse::ListOnly(metadata) => metadata,
                    _ => return Err("unexpected torrent metadata response".into()),
                },
                _ = tick.tick() => {
                    let rate = session_rate(&ctx);
                    if session.ratelimits.get_download_bps() != rate { session.ratelimits.set_download_bps(rate); }
                },
            }
        };
        for file in metadata.info.iter_file_details() {
            let path = file.filename.to_pathbuf();
            if file.attrs().symlink || file.symlink_path.is_some() || path.as_os_str().is_empty()
                || path.components().any(|component| !matches!(component, Component::Normal(_)))
                || path.to_string_lossy().contains(':') {
                return Err("torrent contains an unsafe file path or symbolic link".into());
            }
        }
        let same_torrent = match tokio::fs::read(state.join("metadata.torrent")).await {
            Ok(previous) => previous.len() <= MAX_DESCRIPTOR
                && librqbit::torrent_from_bytes(&previous)
                    .is_ok_and(|torrent| torrent.info_hash == metadata.info_hash),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(error) => return Err(format!("cannot read saved torrent metadata: {error}")),
        };
        if !same_torrent {
            // Metadata resolution is list-only: no active storage owns payload yet.
            // A URL/local descriptor can change identity between retries; old files
            // must not be published alongside the new torrent's verified pieces.
            tokio::fs::remove_dir_all(&payload).await
                .map_err(|e| format!("cannot discard previous torrent payload: {e}"))?;
            tokio::fs::create_dir_all(&payload).await.map_err(|e| e.to_string())?;
        }
        tokio::fs::write(state.join("metadata.torrent"), &metadata.torrent_bytes).await
            .map_err(|e| format!("cannot save torrent metadata: {e}"))?;
        initial_peers.extend(metadata.seen_peers);
        let handle = session.add_torrent(AddTorrent::from_bytes(metadata.torrent_bytes), Some(AddTorrentOptions {
            overwrite: true,
            output_folder: Some(payload.to_str().ok_or("torrent staging path is not UTF-8")?.to_owned()),
            initial_peers: Some(initial_peers),
            ..Default::default()
        })).await.map_err(|e| format!("cannot add embedded torrent: {e:#}"))?
            .into_handle().ok_or("embedded torrent did not return a handle")?;
        let mut last_progress = 0;
        let mut last_change = Instant::now();
        let mut accounted = 0;
        loop {
            if ctx.cancel.is_cancelled() { return Ok(Outcome::Cancelled); }
            let rate = session_rate(&ctx);
            if session.ratelimits.get_download_bps() != rate { session.ratelimits.set_download_bps(rate); }
            let mut stats = handle.stats();
            // During initialization librqbit reports bytes *checked*, including
            // missing/bad pieces. Only live/paused progress counts as downloaded.
            if matches!(stats.state, librqbit::TorrentStatsState::Initializing { .. }) { stats.progress_bytes = 0; }
            if let Some(error) = stats.error { return Err(format!("torrent failed: {error}")); }
            {
                let mut task = task.lock().unwrap();
                task.total = Some(stats.total_bytes);
                task.segments = vec![Seg { start: 0, pos: stats.progress_bytes, end: stats.total_bytes, busy: !stats.finished }];
            }
            if let Some(live) = handle.live() {
                let fetched = live.stats_snapshot().fetched_bytes;
                if fetched > accounted {
                    // pause() synchronously cancels peers and takes their file/
                    // piece state. Re-read after pausing to include last blocks.
                    let delta = fetched.saturating_sub(accounted);
                    let mut limits = (ctx.global.rate(), ctx.local.rate());
                    let mut global_until = Instant::now() + ctx.global.reserve(delta);
                    let mut local_until = Instant::now() + ctx.local.reserve(delta);
                    // No debt means keep the existing peer connection alive.
                    // Reconnecting every sample can trigger peer backoff.
                    let paused = global_until.max(local_until) > Instant::now();
                    let fetched = if paused {
                        session.pause(&handle).await.map_err(|e| format!("cannot pace torrent: {e:#}"))?;
                        let final_fetched = live.stats_snapshot().fetched_bytes;
                        let extra = final_fetched.saturating_sub(fetched);
                        global_until = Instant::now() + ctx.global.reserve(extra);
                        local_until = Instant::now() + ctx.local.reserve(extra);
                        final_fetched
                    } else { fetched };
                    loop {
                        tokio::select! {
                            _ = ctx.cancel.cancelled() => return Ok(Outcome::Cancelled),
                            _ = tokio::time::sleep_until(global_until.max(local_until)) => break,
                            _ = tick.tick() => {
                                // set_rate resets the corresponding token
                                // bucket, so only release that bucket's old debt.
                                let current = (ctx.global.rate(), ctx.local.rate());
                                if limits.0 != current.0 { global_until = Instant::now(); }
                                if limits.1 != current.1 { local_until = Instant::now(); }
                                limits = current;
                            },
                        }
                    }
                    if paused {
                        if !stats.finished {
                            session.unpause(&handle).await.map_err(|e| format!("cannot resume paced torrent: {e:#}"))?;
                        }
                        accounted = 0; // A new live state has fresh counters.
                    } else { accounted = fetched; }
                    last_change = Instant::now(); // Bandwidth debt is not a peer stall.
                }
            }
            if stats.progress_bytes != last_progress {
                last_progress = stats.progress_bytes;
                last_change = Instant::now();
            }
            if stats.finished {
                task.lock().unwrap().append_log("All torrent pieces hash-verified; stopping session before publication");
                return Ok(Outcome::Completed);
            }
            if last_change.elapsed() >= STALL_TIMEOUT {
                return Err("torrent made no transfer or verified-piece progress for 300 seconds; staged data retained".into());
            }
            tokio::select! {
                _ = ctx.cancel.cancelled() => return Ok(Outcome::Cancelled),
                _ = tick.tick() => {},
            }
        }
    }.await;
    session.stop().await;
    // stop() pauses, but retains handles in the session database. delete(false)
    // drops their storage without deleting partial/complete payload files.
    let ids = session.with_torrents(|torrents| torrents.map(|(id, _)| id).collect::<Vec<_>>());
    let mut close_error = None;
    for id in ids {
        if let Err(error) = session.delete(id.into(), false).await {
            close_error = Some(format!("cannot close torrent storage: {error:#}"));
        }
    }
    drop(session);
    for segment in &mut task.lock().unwrap().segments {
        segment.busy = false;
    }
    if let Some(error) = close_error {
        return Err(error);
    }
    if ctx.cancel.is_cancelled() {
        return Ok(Outcome::Cancelled);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limiter::RateLimiter;
    use librqbit::{CreateTorrentOptions, ListenerOptions};
    use sha2::{Digest, Sha256};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_util::sync::CancellationToken;

    struct Swarm {
        _root: tempfile::TempDir,
        session: Arc<Session>,
        server: tokio::task::JoinHandle<()>,
        torrent: Vec<u8>,
        magnet: String,
        http: String,
        files: Vec<(String, Vec<u8>)>,
    }

    impl Swarm {
        async fn new(multifile: bool, size: usize) -> Self {
            let root = tempfile::tempdir().unwrap();
            let input = if multifile {
                root.path().join("bundle")
            } else {
                root.path().join("fixture.bin")
            };
            let data = (0..size)
                .map(|i| ((i.wrapping_mul(37) + i / 97) % 251) as u8)
                .collect::<Vec<_>>();
            let files = if multifile {
                tokio::fs::create_dir_all(input.join("nested"))
                    .await
                    .unwrap();
                let split = size / 2 + 123;
                vec![
                    ("a.bin".into(), data[..split].to_vec()),
                    ("nested/b.bin".into(), data[split..].to_vec()),
                ]
            } else {
                vec![("fixture.bin".into(), data)]
            };
            for (name, bytes) in &files {
                tokio::fs::write(
                    if multifile {
                        input.join(name)
                    } else {
                        input.clone()
                    },
                    bytes,
                )
                .await
                .unwrap();
            }
            let session = Session::new_with_opts(
                root.path().to_owned(),
                SessionOptions {
                    dht: None,
                    disable_local_service_discovery: true,
                    listen: Some(ListenerOptions {
                        listen_addr: "127.0.0.1:0".parse().unwrap(),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let http = format!("http://{}", listener.local_addr().unwrap());
            let (torrent, seed) = session
                .create_and_serve_torrent(
                    &input,
                    CreateTorrentOptions {
                        trackers: vec![format!("{http}/announce")],
                        piece_length: Some(16 * 1024),
                        ..Default::default()
                    },
                )
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(15), seed.wait_until_completed())
                .await
                .unwrap()
                .unwrap();
            let magnet = format!(
                "{}&x.pe={}",
                torrent.as_magnet(),
                session.listen_addr().unwrap()
            );
            let torrent = torrent.as_bytes().unwrap().to_vec();
            let peer = session.listen_addr().unwrap();
            let descriptor = torrent.clone();
            let server = tokio::spawn(async move {
                while let Ok((mut socket, _)) = listener.accept().await {
                    let descriptor = descriptor.clone();
                    tokio::spawn(async move {
                        let mut request = vec![0; 16 * 1024];
                        let mut len = 0;
                        loop {
                            let n = socket.read(&mut request[len..]).await.unwrap_or(0);
                            len += n;
                            if n == 0
                                || len == request.len()
                                || request[..len].windows(4).any(|w| w == b"\r\n\r\n")
                            {
                                break;
                            }
                        }
                        let body = if request[..len].starts_with(b"GET /file.torrent") {
                            descriptor
                        } else {
                            let mut body = b"d8:intervali1e5:peers6:".to_vec();
                            body.extend_from_slice(&[127, 0, 0, 1]);
                            body.extend_from_slice(&peer.port().to_be_bytes());
                            body.push(b'e');
                            body
                        };
                        let headers = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/x-bittorrent\r\nConnection: close\r\n\r\n",
                            body.len()
                        );
                        let _ = socket.write_all(headers.as_bytes()).await;
                        let _ = socket.write_all(&body).await;
                    });
                }
            });
            Self {
                _root: root,
                session,
                server,
                torrent,
                magnet,
                http,
                files,
            }
        }

        async fn check(&self, stage: &Path) {
            for (name, expected) in &self.files {
                let actual = tokio::fs::read(stage.join("payload").join(name))
                    .await
                    .unwrap();
                assert_eq!(actual, *expected);
                assert_eq!(Sha256::digest(&actual), Sha256::digest(expected));
            }
            assert!(stage.join("state/metadata.torrent").is_file());
            assert!(!stage.join("payload/metadata.torrent").exists());
        }

        async fn stop(self) {
            self.server.abort();
            self.session.stop().await;
        }
    }

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
}
