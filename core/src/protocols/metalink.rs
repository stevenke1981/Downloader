//! Metalink v3/v4 multi-file downloads, with independently verified mirror fallback.
use crate::engine::{Ctx, Outcome};
use crate::task::{Seg, Task};
use futures_util::StreamExt;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::io::AsyncWriteExt;

const MAX_DESCRIPTOR: usize = 4 << 20;

mod descriptor;
mod http;
use descriptor::{File, parse};
use http::request;

async fn verified(path: &Path, file: &File) -> Result<bool, String> {
    let metadata = match tokio::fs::metadata(path).await {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.to_string()),
    };
    if !metadata.is_file() || file.size.is_some_and(|size| size != metadata.len()) {
        return Ok(false);
    }
    for hash in &file.hashes {
        let path = path.to_owned();
        let algo = hash.algo;
        let actual =
            tokio::task::spawn_blocking(move || crate::checksum::hash_file(&path, algo, None))
                .await
                .map_err(|e| e.to_string())?
                .map_err(|e| e.to_string())?;
        if !actual.eq_ignore_ascii_case(&hash.expected) {
            return Ok(false);
        }
    }
    Ok(true)
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
    // Redirects are followed manually so Cookie as well as Authorization obey exact origin.
    let client = ctx.protocol_client.clone();
    let source = reqwest::Url::parse(&snapshot.url)
        .ok()
        .filter(|u| matches!(u.scheme(), "http" | "https"));
    let (text, base) = if let Some(source) = &source {
        let response = request(&client, Some(source), source, &snapshot).await?;
        let base = response.url().clone();
        let mut stream = response.bytes_stream();
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|_| "cannot read Metalink descriptor")?;
            if bytes.len() + chunk.len() > MAX_DESCRIPTOR {
                return Err("Metalink descriptor exceeds 4 MiB".into());
            }
            tokio::time::sleep(
                ctx.global
                    .reserve(chunk.len() as u64)
                    .max(ctx.local.reserve(chunk.len() as u64)),
            )
            .await;
            bytes.extend_from_slice(&chunk);
        }
        (
            String::from_utf8(bytes).map_err(|_| "Metalink descriptor is not UTF-8")?,
            Some(base),
        )
    } else {
        let path =
            crate::util::descriptor_path(&snapshot.url).ok_or("invalid local Metalink source")?;
        let metadata = tokio::fs::metadata(&path)
            .await
            .map_err(|e| e.to_string())?;
        if metadata.len() > MAX_DESCRIPTOR as u64 {
            return Err("Metalink descriptor exceeds 4 MiB".into());
        }
        (
            tokio::fs::read_to_string(path)
                .await
                .map_err(|e| e.to_string())?,
            None,
        )
    };
    let files = parse(&text, base.as_ref())?;
    let payload = stage.join("payload");
    let state = stage.join("state");
    tokio::fs::create_dir_all(&state)
        .await
        .map_err(|e| e.to_string())?;
    let previous = tokio::fs::read_to_string(state.join("metalink.xml"))
        .await
        .ok();
    if previous.as_deref() != Some(&text)
        && tokio::fs::try_exists(&payload)
            .await
            .map_err(|e| e.to_string())?
    {
        // The payload belongs only to this private task stage. A changed descriptor
        // must not reuse old unchecksummed files or publish files it no longer names.
        tokio::fs::remove_dir_all(&payload)
            .await
            .map_err(|e| e.to_string())?;
    }
    tokio::fs::create_dir_all(&payload)
        .await
        .map_err(|e| e.to_string())?;
    tokio::fs::write(state.join("metalink.xml"), &text)
        .await
        .map_err(|e| e.to_string())?;
    {
        let mut task = task.lock().unwrap();
        task.total = files.iter().try_fold(0u64, |n, f| n.checked_add(f.size?));
        task.resumable = true; // Completed, verified files are retained; HTTP partials restart.
        task.segments = vec![Seg::new(0, task.total.unwrap_or(u64::MAX))];
        task.append_log(format!(
            "Native Metalink started with {} file(s)",
            files.len()
        ));
    }
    let mut completed = 0u64;
    for (index, file) in files.iter().enumerate() {
        let destination = payload.join(&file.name);
        if !verified(&destination, file).await? {
            if let Some(parent) = destination.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|e| e.to_string())?;
            }
            let mut accepted = false;
            let mut last_error = String::new();
            for (mirror_index, mirror) in file.mirrors.iter().enumerate() {
                let temporary = state.join(format!("file-{index}-{mirror_index}.part"));
                let result = if mirror.scheme() == "ftp" {
                    let ftp_stage = state.join(format!("ftp-{index}-{mirror_index}"));
                    let child = Arc::new(Mutex::new(Task {
                        url: mirror.to_string(),
                        suggested_name: Some("data".into()),
                        ..Task::default()
                    }));
                    let transfer =
                        crate::protocols::ftp::download(child.clone(), ctx.clone(), &ftp_stage);
                    tokio::pin!(transfer);
                    let outcome = loop {
                        tokio::select! {
                            result = &mut transfer => break result,
                            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                                task.lock().unwrap().segments[0].pos = completed + child.lock().unwrap().downloaded();
                            }
                        }
                    };
                    match outcome {
                        Ok(Outcome::Completed) => {
                            tokio::fs::rename(ftp_stage.join("payload/data"), &temporary)
                                .await
                                .map_err(|e| e.to_string())
                        }
                        Ok(Outcome::Cancelled) => return Err("Metalink cancelled".into()),
                        Err(e) => Err(e),
                    }
                } else {
                    async {
                        let response = request(&client, source.as_ref(), mirror, &snapshot).await?;
                        if file
                            .size
                            .zip(response.content_length())
                            .is_some_and(|(expected, actual)| expected != actual)
                        {
                            return Err("Metalink mirror length mismatch".into());
                        }
                        let mut output = tokio::fs::File::create(&temporary)
                            .await
                            .map_err(|e| e.to_string())?;
                        let mut stream = response.bytes_stream();
                        let mut received = 0u64;
                        while let Some(chunk) = stream.next().await {
                            let chunk = chunk.map_err(|_| "Metalink mirror body failed")?;
                            tokio::time::sleep(
                                ctx.global
                                    .reserve(chunk.len() as u64)
                                    .max(ctx.local.reserve(chunk.len() as u64)),
                            )
                            .await;
                            received = received
                                .checked_add(chunk.len() as u64)
                                .ok_or("Metalink length overflow")?;
                            if file.size.is_some_and(|size| received > size) {
                                return Err("Metalink mirror exceeds advertised size".into());
                            }
                            output.write_all(&chunk).await.map_err(|e| e.to_string())?;
                            task.lock().unwrap().segments[0].pos = completed + received;
                        }
                        output.flush().await.map_err(|e| e.to_string())?;
                        Ok(())
                    }
                    .await
                };
                match result {
                    Ok(()) if verified(&temporary, file).await? => {
                        if tokio::fs::try_exists(&destination)
                            .await
                            .map_err(|e| e.to_string())?
                        {
                            tokio::fs::remove_file(&destination)
                                .await
                                .map_err(|e| e.to_string())?;
                        }
                        tokio::fs::rename(&temporary, &destination)
                            .await
                            .map_err(|e| e.to_string())?;
                        accepted = true;
                        break;
                    }
                    Ok(()) => last_error = "Metalink mirror size or digest mismatch".into(),
                    Err(error) => last_error = error,
                }
                task.lock().unwrap().append_log(format!(
                    "Metalink mirror {} rejected: {last_error}",
                    mirror_index + 1
                ));
                task.lock().unwrap().segments[0].pos = completed;
            }
            if !accepted {
                return Err(format!("all Metalink mirrors failed: {last_error}"));
            }
        }
        completed = completed
            .checked_add(
                tokio::fs::metadata(&destination)
                    .await
                    .map_err(|e| e.to_string())?
                    .len(),
            )
            .ok_or("Metalink total size overflow")?;
        task.lock().unwrap().segments[0].pos = completed;
    }
    let mut task = task.lock().unwrap();
    task.total = Some(completed);
    task.segments[0].end = completed;
    task.append_log("All Metalink outputs verified");
    Ok(())
}

#[cfg(test)]
mod tests;
