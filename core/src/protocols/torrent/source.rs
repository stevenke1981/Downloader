use super::*;

pub(super) struct TorrentSource {
    pub(super) descriptor: Option<Vec<u8>>,
    pub(super) initial_peers: Vec<SocketAddr>,
    pub(super) local_discovery: bool,
}

impl TorrentSource {
    pub(super) async fn prepare(snapshot: &Task, ctx: &Ctx, state: &Path) -> Result<Option<Self>, String> {
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
                _ = ctx.cancel.cancelled() => return Ok(None),
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
        Ok(Some(Self { descriptor, initial_peers, local_discovery }))
    }
}
