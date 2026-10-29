//! Native, cancellable passive FTP transfers. No external protocol engine is used.
use crate::engine::{Ctx, Outcome};
use crate::task::{Seg, Task};
use base64::Engine as _;
use percent_encoding::percent_decode_str;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

mod connection;
use connection::Ftp;

fn remote_path(url: &reqwest::Url) -> Result<String, String> {
    let path = percent_decode_str(url.path())
        .decode_utf8()
        .map_err(|_| "FTP path is not UTF-8")?
        .into_owned();
    if path.contains(['\r', '\n', '\0']) {
        return Err("invalid FTP path".into());
    }
    Ok(path)
}

fn credentials(task: &Task, url: &reqwest::Url) -> Result<(String, String), String> {
    if let Some(value) = &task.authorization {
        let decoded = value
            .strip_prefix("Basic ")
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
            .and_then(|b| String::from_utf8(b).ok())
            .ok_or("invalid FTP site credentials")?;
        let (user, password) = decoded
            .split_once(':')
            .ok_or("invalid FTP site credentials")?;
        return Ok((user.into(), password.into()));
    }
    if !url.username().is_empty() {
        return Ok((
            percent_decode_str(url.username())
                .decode_utf8()
                .map_err(|_| "invalid FTP username")?
                .into_owned(),
            percent_decode_str(url.password().unwrap_or(""))
                .decode_utf8()
                .map_err(|_| "invalid FTP password")?
                .into_owned(),
        ));
    }
    Ok(("anonymous".into(), "anonymous@".into()))
}

#[derive(serde::Serialize, serde::Deserialize, PartialEq)]
struct Identity {
    source: String,
    size: Option<u64>,
    modified: Option<String>,
}

pub(crate) async fn download(
    task: Arc<Mutex<Task>>,
    ctx: Ctx,
    stage: &Path,
) -> Result<Outcome, String> {
    tokio::select! {
        biased;
        _ = ctx.cancel.cancelled() => Ok(Outcome::Cancelled),
        result = transfer(task, &ctx, stage) => result.map(|()| Outcome::Completed),
    }
}

async fn transfer(task: Arc<Mutex<Task>>, ctx: &Ctx, stage: &Path) -> Result<(), String> {
    let snapshot = task.lock().unwrap().clone();
    let url = reqwest::Url::parse(&snapshot.url).map_err(|_| "invalid FTP URL")?;
    let path = remote_path(&url)?;
    let (username, password) = credentials(&snapshot, &url)?;
    let mut ftp = tokio::time::timeout(
        Duration::from_secs(30),
        Ftp::connect(&url, &username, &password),
    )
    .await
    .map_err(|_| "FTP connection timed out")??;
    let reply = ftp.command(&format!("SIZE {path}")).await?;
    let size = (reply.0 == 213)
        .then(|| reply.1.get(4..).unwrap_or("").trim().parse::<u64>().ok())
        .flatten();
    let reply = ftp.command(&format!("MDTM {path}")).await?;
    let modified = (reply.0 == 213)
        .then(|| reply.1.get(4..).unwrap_or("").trim().to_owned())
        .filter(|s| !s.is_empty());
    let mut source = url.clone();
    let _ = source.set_username("");
    let _ = source.set_password(None);
    let identity = Identity {
        source: source.to_string(),
        size,
        modified,
    };
    let name = snapshot
        .suggested_name
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(crate::util::sanitize)
        .unwrap_or_else(|| crate::util::sanitize(path.rsplit('/').next().unwrap_or("download")));
    if name.is_empty() {
        return Err("FTP URL does not name a file".into());
    }
    let payload = stage.join("payload");
    let state = stage.join("state");
    tokio::fs::create_dir_all(&payload)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::create_dir_all(&state)
        .await
        .map_err(|e| e.to_string())?;
    let output = payload.join(name);
    let record = state.join("ftp.json");
    let previous = tokio::fs::read(&record)
        .await
        .ok()
        .and_then(|b| serde_json::from_slice::<Identity>(&b).ok());
    let mut offset = tokio::fs::metadata(&output)
        .await
        .map(|m| m.len())
        .unwrap_or(0);
    let safe = size.is_some()
        && identity.modified.is_some()
        && previous.as_ref() == Some(&identity)
        && size.is_some_and(|n| offset <= n);
    if !safe {
        offset = 0;
    }
    let mut data = ftp.passive().await?;
    let rest = ftp.command(&format!("REST {offset}")).await?.0 == 350;
    if offset > 0 && !rest {
        offset = 0;
    }
    // Rejected REST means a fresh RETR, never append bytes from the beginning.
    let mut output = tokio::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(offset == 0)
        .append(offset > 0)
        .open(&output)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::write(
        &record,
        serde_json::to_vec(&identity).map_err(|e| e.to_string())?,
    )
    .await
    .map_err(|e| e.to_string())?;
    {
        let mut task = task.lock().unwrap();
        task.total = size;
        task.resumable = rest && size.is_some() && identity.modified.is_some();
        task.segments = vec![Seg {
            start: 0,
            pos: offset,
            end: size.unwrap_or(u64::MAX),
            busy: false,
        }];
        task.append_log(format!("Native FTP transfer started at byte {offset}"));
    }
    if !matches!(ftp.command(&format!("RETR {path}")).await?.0, 125 | 150) {
        return Err("FTP file retrieval rejected".into());
    }
    let mut buffer = [0u8; 32768];
    loop {
        let n = tokio::time::timeout(Duration::from_secs(60), data.read(&mut buffer))
            .await
            .map_err(|_| "FTP data connection timed out")?
            .map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        let delay = ctx
            .global
            .reserve(n as u64)
            .max(ctx.local.reserve(n as u64));
        tokio::time::sleep(delay).await;
        if size.is_some_and(|size| offset.saturating_add(n as u64) > size) {
            return Err("FTP transfer exceeds advertised size".into());
        }
        output
            .write_all(&buffer[..n])
            .await
            .map_err(|e| e.to_string())?;
        offset += n as u64;
        task.lock().unwrap().segments[0].pos = offset;
    }
    output.flush().await.map_err(|e| e.to_string())?;
    if !matches!(ftp.reply().await?.0, 226 | 250) {
        return Err("FTP transfer did not complete successfully".into());
    }
    if size.is_some_and(|size| size != offset) {
        return Err("FTP transfer is shorter than advertised size".into());
    }
    let mut task = task.lock().unwrap();
    task.total = Some(offset);
    task.segments[0].end = offset;
    task.append_log("Native FTP transfer completed");
    Ok(())
}

/// Return NLST entry names unchanged; callers own URL encoding.
pub async fn list(url: &str, username: &str, password: &str) -> Result<Vec<String>, String> {
    tokio::time::timeout(Duration::from_secs(30), async {
        let url = reqwest::Url::parse(url).map_err(|_| "invalid FTP URL")?;
        let path = remote_path(&url)?;
        let (username, password) = if username.is_empty() && password.is_empty() {
            credentials(&Task::default(), &url)?
        } else {
            (username.into(), password.into())
        };
        let mut ftp = Ftp::connect(&url, &username, &password).await?;
        if ftp.command(&format!("CWD {path}")).await?.0 != 250 {
            return Err("FTP directory rejected".into());
        }
        let mut data = ftp.passive().await?;
        if !matches!(ftp.command("NLST").await?.0, 125 | 150) {
            return Err("FTP directory listing rejected".into());
        }
        let mut bytes = Vec::new();
        (&mut data)
            .take((1 << 20) + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|e| e.to_string())?;
        if bytes.len() > 1 << 20 {
            return Err("FTP directory listing exceeds 1 MiB".into());
        }
        if !matches!(ftp.reply().await?.0, 226 | 250) {
            return Err("FTP directory listing failed".into());
        }
        let text = String::from_utf8(bytes).map_err(|_| "FTP directory listing is not UTF-8")?;
        Ok(text
            .lines()
            .filter(|line| !line.is_empty() && *line != "." && *line != "..")
            .map(str::to_owned)
            .collect())
    })
    .await
    .map_err(|_| "FTP directory listing timed out")?
}

#[cfg(test)]
pub(crate) mod tests;
