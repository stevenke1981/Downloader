//! Optional protocol engines. Every invocation uses argv and task-scoped staging.
use crate::engine::{Ctx, Outcome};
use crate::task::{DownloadKind, Seg, Status, Task, Verification, VerifyState};
use crate::{checksum, util};
use base64::Engine as _;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::process::Command;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Capability {
    pub name: String,
    pub available: bool,
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    Aria2,
    Ffmpeg,
    Ytdlp,
    Curl,
}

impl Tool {
    fn names(self) -> (&'static str, &'static str) {
        match self {
            Self::Aria2 => ("aria2c", "DOWNLOADER_ARIA2"),
            Self::Ffmpeg => ("ffmpeg", "DOWNLOADER_FFMPEG"),
            Self::Ytdlp => ("yt-dlp", "DOWNLOADER_YTDLP"),
            Self::Curl => ("curl", "DOWNLOADER_CURL"),
        }
    }

    fn path(self) -> Option<PathBuf> {
        let (name, variable) = self.names();
        let executable = |path: &Path| {
            path.is_file()
                && (!cfg!(windows)
                    || path.extension().is_some_and(|ext| {
                        ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("com")
                    }))
        };
        if let Some(path) = std::env::var_os(variable).filter(|s| !s.is_empty()) {
            let path = PathBuf::from(path);
            return executable(&path).then_some(path);
        }
        let path = std::env::var_os("PATH")?;
        for directory in std::env::split_paths(&path) {
            let candidate = directory.join(name);
            if executable(&candidate) {
                return Some(candidate);
            }
            if cfg!(windows) {
                let candidate = directory.join(format!("{name}.exe"));
                if executable(&candidate) {
                    return Some(candidate);
                }
            }
        }
        None
    }
}

pub fn capabilities() -> Vec<Capability> {
    [Tool::Aria2, Tool::Ffmpeg, Tool::Ytdlp, Tool::Curl]
        .into_iter()
        .map(|tool| {
            let path = tool.path();
            Capability {
                name: tool.names().0.into(),
                available: path.is_some(),
                path,
            }
        })
        .collect()
}

fn source_tool(source: &str, kind: DownloadKind) -> Option<Tool> {
    if kind == DownloadKind::Video {
        return Some(Tool::Ytdlp);
    }
    if util::descriptor_path(source).is_some() {
        return Some(Tool::Aria2);
    }
    let url = reqwest::Url::parse(source).ok()?;
    match url.scheme() {
        "ftp" | "magnet" => Some(Tool::Aria2),
        "rtsp" | "mms" | "mmsh" | "mmst" => Some(Tool::Ffmpeg),
        "http" | "https" => {
            let name = url.path().to_ascii_lowercase();
            [".torrent", ".metalink", ".meta4"]
                .iter()
                .any(|ext| name.ends_with(ext))
                .then_some(Tool::Aria2)
        }
        _ => None,
    }
}

pub(crate) fn handles(source: &str, kind: DownloadKind) -> bool {
    source_tool(source, kind).is_some()
}

/// Stable across pause/restart. Kept separate from final filenames and unrelated tasks.
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
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if [".aria2", ".part", ".ytdl", ".tmp"]
                    .iter()
                    .any(|ext| name.ends_with(ext))
                {
                    continue;
                }
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

fn command(path: &Path) -> Command {
    let mut command = Command::new(path);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW
    command
}

/// Stop the process tree on Windows, then always reap the direct child.
async fn stop(child: &mut tokio::process::Child) -> Result<(), String> {
    #[cfg(windows)]
    if let (Some(pid), Some(root)) = (child.id(), std::env::var_os("SystemRoot")) {
        let _ = command(&PathBuf::from(root).join("System32/taskkill.exe"))
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status()
            .await;
    }
    let _ = child.start_kill();
    child
        .wait()
        .await
        .map_err(|e| format!("cannot wait for stopped protocol engine: {e}"))?;
    Ok(())
}

pub(crate) async fn run(task: Arc<Mutex<Task>>, ctx: Ctx) -> Result<Outcome, String> {
    let snapshot = task.lock().unwrap().clone();
    util::validate_source(&snapshot.url)?;
    let tool = source_tool(&snapshot.url, snapshot.kind).ok_or("unsupported protocol engine")?;
    if snapshot.kind == DownloadKind::Video
        && !reqwest::Url::parse(&snapshot.url).is_ok_and(|u| matches!(u.scheme(), "http" | "https"))
    {
        return Err("website video mode requires an HTTP(S) page URL".into());
    }
    let ftp = reqwest::Url::parse(&snapshot.url).is_ok_and(|url| url.scheme() == "ftp");
    // HTTP authentication must never become a header forwarded by a third-party redirect handler.
    if (snapshot.authorization.is_some() && !ftp)
        || snapshot.cookie.as_ref().is_some_and(|c| !c.is_empty())
    {
        return Err(
            "external protocol engines do not accept browser cookies or HTTP site credentials"
                .into(),
        );
    }
    let path = tool.path().ok_or_else(|| {
        format!(
            "{} is unavailable; install it or set {} to its executable path",
            tool.names().0,
            tool.names().1
        )
    })?;
    if ctx.cancel.is_cancelled() {
        return Ok(Outcome::Cancelled);
    }
    let stage = staging_path(&snapshot);
    std::fs::create_dir_all(&stage)
        .map_err(|e| format!("cannot create protocol staging folder: {e}"))?;
    let stage = std::fs::canonicalize(stage).map_err(|e| e.to_string())?;
    let mut cmd = command(&path);
    let rate = match (ctx.local.rate(), ctx.global.rate()) {
        (0, rate) | (rate, 0) => rate,
        (local, global) => local.min(global),
    };
    match tool {
        Tool::Aria2 => {
            cmd.args([
                "--no-conf",
                "--enable-rpc=false",
                "--continue=true",
                "--allow-overwrite=false",
                "--auto-file-renaming=false",
                "--check-integrity=true",
                "--seed-time=0",
                "--bt-save-metadata=false",
                "--follow-torrent=mem",
                "--follow-metalink=mem",
                "--summary-interval=0",
                "--console-log-level=error",
                "--download-result=hide",
            ])
            .arg(format!(
                "--max-connection-per-server={}",
                snapshot.connections.clamp(1, 16)
            ))
            .arg(format!("--split={}", snapshot.connections.clamp(1, 16)))
            .arg(format!("--max-tries={}", ctx.retries.saturating_add(1)))
            .arg(format!("--max-overall-download-limit={rate}"))
            .arg("--dir")
            .arg(&stage);
            if let Some(value) = snapshot.authorization.as_deref() {
                let credentials = value
                    .strip_prefix("Basic ")
                    .and_then(|value| base64::engine::general_purpose::STANDARD.decode(value).ok())
                    .and_then(|value| String::from_utf8(value).ok())
                    .ok_or("invalid FTP site credentials")?;
                let (username, password) = credentials
                    .split_once(':')
                    .ok_or("invalid FTP site credentials")?;
                cmd.arg(format!("--ftp-user={username}"))
                    .arg(format!("--ftp-passwd={password}"));
            }
            let source = if let Some(local) = util::descriptor_path(&snapshot.url) {
                std::fs::canonicalize(local)
                    .map_err(|e| format!("cannot open descriptor: {e}"))?
                    .into_os_string()
            } else {
                OsString::from(&snapshot.url)
            };
            cmd.arg("--").arg(source);
        }
        Tool::Ffmpeg => {
            let mut url = reqwest::Url::parse(&snapshot.url).map_err(|e| e.to_string())?;
            if url.scheme() == "mms" {
                url.set_scheme("mmsh").map_err(|()| "invalid MMS URL")?;
            }
            cmd.args(["-hide_banner", "-nostdin", "-loglevel", "error", "-y"]);
            if url.scheme() == "rtsp" {
                cmd.args(["-rtsp_transport", "tcp"]);
            }
            cmd.arg("-i")
                .arg(url.as_str())
                .args(["-map", "0", "-c", "copy", "-f", "matroska"])
                .arg(stage.join("capture.mkv"));
        }
        Tool::Ytdlp => {
            cmd.args([
                "--ignore-config",
                "--no-playlist",
                "--no-warnings",
                "--no-progress",
                "--restrict-filenames",
                "--format",
                "best",
                "--paths",
            ])
            .arg(&stage)
            .args(["--output", "%(title).150B [%(id)s].%(ext)s"]);
            if rate > 0 {
                cmd.arg("--limit-rate").arg(rate.to_string());
            }
            if let Some(ffmpeg) = Tool::Ffmpeg.path() {
                cmd.arg("--ffmpeg-location").arg(ffmpeg);
            }
            cmd.arg("--").arg(&snapshot.url);
        }
        Tool::Curl => unreachable!(),
    }
    {
        let mut task = task.lock().unwrap();
        task.resumable = tool != Tool::Ffmpeg;
        task.error = None;
        task.segments = vec![Seg::new(0, u64::MAX)];
        task.total = None;
        task.append_log(format!("{} protocol engine started", tool.names().0));
        if rate > 0 && tool == Tool::Ffmpeg {
            task.append_log("FFmpeg live capture does not support a byte-per-second limit");
        }
        if ctx.global.rate() > 0 && tool != Tool::Ffmpeg {
            task.append_log("External engine limit is applied per process at launch");
        }
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", tool.names().0))?;
    let status = loop {
        tokio::select! {
            result = child.wait() => break result.map_err(|e| format!("protocol engine wait failed: {e}"))?,
            _ = ctx.cancel.cancelled() => {
                stop(&mut child).await?;
                task.lock().unwrap().append_log("Protocol engine stopped; staged data retained");
                return Ok(Outcome::Cancelled);
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                if let Ok(files) = outputs(&stage) {
                    task.lock().unwrap().segments[0].pos = files.iter().map(|(_, size)| size).sum();
                }
            }
        }
    };
    task.lock()
        .unwrap()
        .append_log(format!("{} exited with {status}", tool.names().0));
    if ctx.cancel.is_cancelled() {
        return Ok(Outcome::Cancelled);
    }
    if !status.success() {
        return Err(format!(
            "{} failed ({status}); staged data retained",
            tool.names().0
        ));
    }
    let files = outputs(&stage)?;
    if files.is_empty() {
        return Err(format!(
            "{} exited successfully but produced no completed output",
            tool.names().0
        ));
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
        task.lock().unwrap().verification = ctx.verify.then(|| Verification { state: VerifyState::NoChecksum, note: "No explicit whole-file checksum; protocol metadata checks, when present, are handled by the external engine".into(), ..Default::default() });
    }
    if ctx.cancel.is_cancelled() {
        return Ok(Outcome::Cancelled);
    }
    let name = if files.len() == 1 {
        snapshot
            .suggested_name
            .as_deref()
            .map(util::sanitize)
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                util::sanitize(&files[0].0.file_name().unwrap_or_default().to_string_lossy())
            })
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
        &stage
    };
    std::fs::rename(source, &destination)
        .map_err(|e| format!("cannot publish protocol output: {e}"))?;
    if files.len() == 1 {
        let _ = std::fs::remove_dir_all(&stage);
    }
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

/// List one FTP directory. Credentials remain in memory and never enter URL/history/logs.
pub async fn list_ftp(url: &str, username: &str, password: &str) -> Result<Vec<String>, String> {
    util::validate_source(url)?;
    let url = reqwest::Url::parse(url.trim()).map_err(|e| e.to_string())?;
    if url.scheme() != "ftp" {
        return Err("FTP browser requires an ftp:// URL".into());
    }
    let path = Tool::Curl
        .path()
        .ok_or("curl is unavailable; set DOWNLOADER_CURL to its executable path")?;
    let mut cmd = command(&path);
    cmd.args([
        "--disable",
        "--silent",
        "--fail",
        "--list-only",
        "--max-time",
        "30",
        "--proto",
        "=ftp",
    ]);
    if !username.is_empty() || !password.is_empty() {
        cmd.arg("--user").arg(format!("{username}:{password}"));
    }
    cmd.arg("--url").arg(url.as_str()).stdout(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("cannot start curl: {e}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or("cannot read FTP directory output")?;
    let read = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stdout
            .take((1 << 20) + 1)
            .read_to_end(&mut bytes)
            .await
            .map(|_| bytes)
    });
    let status = tokio::select! {
        result = child.wait() => result.map_err(|e| e.to_string())?,
        _ = tokio::time::sleep(Duration::from_secs(35)) => {
            stop(&mut child).await?;
            read.abort();
            return Err("FTP directory listing timed out".into());
        }
    };
    let bytes = read
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    if bytes.len() > 1 << 20 {
        return Err("FTP directory listing exceeds 1 MiB".into());
    }
    if !status.success() {
        return Err(format!("FTP directory listing failed ({status})"));
    }
    let text = String::from_utf8(bytes).map_err(|_| "FTP directory listing is not UTF-8")?;
    Ok(text
        .lines()
        .filter(|line| !line.is_empty() && *line != "." && *line != "..")
        .map(str::to_owned)
        .collect())
}
