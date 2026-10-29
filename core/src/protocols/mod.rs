//! In-process Rust protocol adapters and verified publication.
//! Adapters own protocol I/O; this facade owns dispatch, staging and publication.
mod ftp;
mod metalink;
mod torrent;
mod media;
use crate::engine::{Ctx, Outcome};
use crate::task::{DownloadKind, Seg, Status, Task, Verification, VerifyState};
use crate::{checksum, util};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Capability {
    pub name: String,
    pub available: bool,
    pub path: Option<PathBuf>,
}

pub fn capabilities() -> Vec<Capability> {
    [
        "Rust FTP / directory browser",
        "Rust Metalink",
        "Rust BitTorrent / magnet",
        "Rust HLS / webpage media",
        "Rust RTSP H.264 / G.711",
        "Rust MMS-over-HTTP",
    ]
    .into_iter()
    .map(|name| Capability {
        name: name.into(),
        available: true,
        path: None,
    })
    .collect()
}

#[derive(Debug, Clone, Copy)]
enum Protocol {
    Ftp,
    Metalink,
    Torrent,
    Media,
}

fn protocol(source: &str, kind: DownloadKind) -> Option<Protocol> {
    if kind == DownloadKind::Video {
        return Some(Protocol::Media);
    }
    let extension = if let Some(path) = util::descriptor_path(source) {
        path.extension()?.to_str()?.to_ascii_lowercase()
    } else {
        let url = reqwest::Url::parse(source).ok()?;
        match url.scheme() {
            "ftp" => return Some(Protocol::Ftp),
            "magnet" => return Some(Protocol::Torrent),
            "rtsp" | "mms" | "mmsh" | "mmst" => return Some(Protocol::Media),
            "http" | "https" => {}
            _ => return None,
        }
        Path::new(url.path())
            .extension()?
            .to_str()?
            .to_ascii_lowercase()
    };
    match extension.as_str() {
        "torrent" => Some(Protocol::Torrent),
        "metalink" | "meta4" => Some(Protocol::Metalink),
        "m3u8" => Some(Protocol::Media),
        _ => None,
    }
}

pub(crate) fn handles(source: &str, kind: DownloadKind) -> bool {
    protocol(source, kind).is_some()
}

/// Stable across pause/restart. Legacy prefix keeps existing task cleanup compatible.
pub fn staging_path(task: &Task) -> PathBuf {
    let hash = task
        .url
        .bytes()
        .chain([task.kind as u8])
        .fold(0xcbf29ce484222325u64, |h, b| {
            (h ^ b as u64).wrapping_mul(0x100000001b3)
        });
    task.dir
        .join(format!(".dlx-external-{:016x}-{hash:016x}", task.id))
}

fn outputs(directory: &Path) -> Result<Vec<(PathBuf, u64)>, String> {
    let mut files = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in
            std::fs::read_dir(directory).map_err(|e| format!("cannot read staged output: {e}"))?
        {
            let entry = entry.map_err(|e| e.to_string())?;
            let kind = entry.file_type().map_err(|e| e.to_string())?;
            if kind.is_symlink() {
                return Err("staged output contains a symbolic link".into());
            }
            if kind.is_dir() {
                pending.push(entry.path());
            }
            if kind.is_file() {
                files.push((
                    entry.path(),
                    entry.metadata().map_err(|e| e.to_string())?.len(),
                ));
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

pub(crate) async fn run(task: Arc<Mutex<Task>>, ctx: Ctx) -> Result<Outcome, String> {
    let snapshot = task.lock().unwrap().clone();
    util::validate_source(&snapshot.url)?;
    let protocol = protocol(&snapshot.url, snapshot.kind).ok_or("unsupported native protocol")?;
    if snapshot.kind == DownloadKind::Video
        && !reqwest::Url::parse(&snapshot.url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
    {
        return Err("website video mode requires an HTTP(S) page URL".into());
    }
    if ctx.cancel.is_cancelled() {
        return Ok(Outcome::Cancelled);
    }
    let stage = staging_path(&snapshot);
    std::fs::create_dir_all(stage.join("payload"))
        .map_err(|e| format!("cannot create protocol staging folder: {e}"))?;
    std::fs::create_dir_all(stage.join("state")).map_err(|e| e.to_string())?;
    let stage = std::fs::canonicalize(stage).map_err(|e| e.to_string())?;
    task.lock()
        .unwrap()
        .append_log(format!("Starting native Rust {protocol:?} adapter"));
    // Adapters own network cancellation and protocol integrity; state never enters publication.
    let verify = ctx.verify;
    let cancel = ctx.cancel.clone();
    let result = match protocol {
        Protocol::Ftp => crate::protocols::ftp::download(task.clone(), ctx, &stage).await,
        Protocol::Metalink => crate::protocols::metalink::download(task.clone(), ctx, &stage).await,
        Protocol::Torrent => crate::protocols::torrent::download(task.clone(), ctx, &stage).await,
        Protocol::Media => crate::protocols::media::download(task.clone(), ctx, &stage).await,
    }?;
    if result == Outcome::Cancelled || cancel.is_cancelled() {
        return Ok(Outcome::Cancelled);
    }
    let payload = stage.join("payload");
    let files = outputs(&payload)?;
    if files.is_empty() {
        return Err("native protocol produced no completed output".into());
    }
    let total = files.iter().map(|(_, size)| size).sum();
    if let Some(expected) = snapshot.checksum {
        if files.len() != 1 {
            task.lock().unwrap().verification = Some(Verification {
                state: VerifyState::Failed,
                algo: Some(expected.algo),
                expected: expected.expected,
                source: expected.source,
                note: "A whole-file checksum requires a single output file".into(),
                ..Default::default()
            });
            return Err("a whole-file checksum cannot verify a multi-file download; no output was published".into());
        }
        {
            let mut task = task.lock().unwrap();
            task.status = Status::Verifying;
            task.verify_done
                .store(0, std::sync::atomic::Ordering::Relaxed);
        }
        let output = files[0].0.clone();
        let progress = snapshot.verify_done;
        let algo = expected.algo;
        let actual = tokio::task::spawn_blocking(move || {
            checksum::hash_file(&output, algo, Some(&progress))
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| format!("cannot verify staged output: {e}"))?;
        let matched = actual.eq_ignore_ascii_case(&expected.expected);
        task.lock().unwrap().verification = Some(Verification {
            state: if matched {
                VerifyState::Verified
            } else {
                VerifyState::Failed
            },
            algo: Some(algo),
            expected: expected.expected,
            actual,
            source: expected.source,
            ..Default::default()
        });
        if !matched {
            return Err("verification failed; staged output was not published".into());
        }
    } else {
        task.lock().unwrap().verification = verify.then(|| Verification { state: VerifyState::NoChecksum, note: "No explicit whole-file checksum; protocol metadata checks, when present, are validated by the native Rust adapter".into(), ..Default::default() });
    }
    if cancel.is_cancelled() {
        return Ok(Outcome::Cancelled);
    }
    let name = if files.len() == 1 {
        let actual = util::sanitize(&files[0].0.file_name().unwrap_or_default().to_string_lossy());
        let suggested = snapshot
            .suggested_name
            .as_deref()
            .map(util::sanitize)
            .filter(|s| !s.is_empty());
        match protocol {
            Protocol::Ftp => suggested.unwrap_or(actual),
            // Descriptor names describe the request, not the downloaded payload.
            Protocol::Torrent | Protocol::Metalink => actual,
            Protocol::Media => suggested.map_or(actual, |name| {
                let mut name = PathBuf::from(name);
                name.set_extension(files[0].0.extension().unwrap_or_default());
                name.to_string_lossy().into_owned()
            }),
        }
    } else {
        let name = util::name_from_url(&snapshot.url);
        let stem = Path::new(&name)
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy();
        if stem.is_empty() || snapshot.url.starts_with("magnet:") {
            "download-files".into()
        } else {
            format!("{stem}-files")
        }
    };
    let name = util::unique_name(&snapshot.dir, &name);
    let destination = snapshot.dir.join(&name);
    let source = if files.len() == 1 {
        &files[0].0
    } else {
        &payload
    };
    std::fs::rename(source, &destination)
        .map_err(|e| format!("cannot publish protocol output: {e}"))?;
    let _ = std::fs::remove_dir_all(&stage);
    let mut task = task.lock().unwrap();
    task.file_name = name;
    task.category = crate::category::Category::from_name(&task.file_name);
    task.total = Some(total);
    task.segments = vec![Seg {
        start: 0,
        pos: total,
        end: total,
        busy: false,
    }];
    task.append_log(format!("Published {} output file(s)", files.len()));
    Ok(Outcome::Completed)
}

/// List one FTP directory in process. Credentials are never persisted.
pub async fn list_ftp(url: &str, username: &str, password: &str) -> Result<Vec<String>, String> {
    util::validate_source(url)?;
    crate::protocols::ftp::list(url, username, password).await
}
