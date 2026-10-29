//! Embedded BitTorrent v1 engine. Payload and control data have separate roots.
mod source;

use source::TorrentSource;
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
    let Some(TorrentSource { descriptor, mut initial_peers, local_discovery }) =
        TorrentSource::prepare(&snapshot, &ctx, &state).await?
    else {
        return Ok(Outcome::Cancelled);
    };
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
mod tests;
