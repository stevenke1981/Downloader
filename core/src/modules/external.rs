//! The only place external module processes are started: argv only, no shell,
//! a clean environment, one JSON line in and at most one line out.
use super::manifest::{self, Checked};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// Largest accepted response line.
pub(super) const MAX_STDOUT: usize = 64 * 1024;
/// Processes running at once across all modules.
pub(super) const MAX_CONCURRENT: usize = 4;
/// Variables passed through to modules; everything else is cleared.
const ENV_KEEP: [&str; 4] = ["PATH", "SystemRoot", "TEMP", "TMP"];

/// Start the module with a clean environment and piped stdin/stdout.
fn spawn(module: &Checked) -> Result<tokio::process::Child, String> {
    // The manifest may have changed since it was approved and loaded: re-check the bytes.
    let bytes = manifest::read(&module.dir)?;
    if manifest::sha256_hex(&bytes) != module.sha256 {
        return Err("module.json changed since it was approved; approve it again".into());
    }
    let mut command = tokio::process::Command::new(&module.program);
    command
        .args(&module.manifest.command[1..])
        .current_dir(&module.dir)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    for key in ENV_KEEP {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    command.spawn().map_err(|e| format!("cannot start: {e}"))
}

/// Run one request. Returns the first stdout line (without the newline), or `None` when
/// `cancel` fired first. The process is killed when this returns.
pub(super) async fn call(
    module: &Checked,
    request: &str,
    permits: &Semaphore,
    cancel: &CancellationToken,
) -> Result<Option<String>, String> {
    let _permit = tokio::select! {
        p = permits.acquire() => p.map_err(|_| "module host is shutting down".to_string())?,
        _ = cancel.cancelled() => return Ok(None),
    };
    let mut child = spawn(module)?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let line = format!("{request}\n");
    let exchange = async {
        // A module that never reads stdin must not stall us: ignore write errors.
        let _ = stdin.write_all(line.as_bytes()).await;
        let _ = stdin.shutdown().await;
        drop(stdin);
        read_line(&mut stdout).await
    };
    let timeout = module.timeout();
    let result = tokio::select! {
        r = exchange => r.map(Some),
        _ = tokio::time::sleep(timeout) => Err(format!("timed out after {} ms", timeout.as_millis())),
        _ = cancel.cancelled() => Ok(None),
    };
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    result
}

/// Deliver one hook event and let the module finish: stdout is drained and discarded, and
/// the process is killed only when it outlives its timeout.
pub(super) async fn deliver(
    module: &Checked,
    request: &str,
    permits: &Semaphore,
) -> Result<(), String> {
    let _permit = permits
        .acquire()
        .await
        .map_err(|_| "module host is shutting down".to_string())?;
    let mut child = spawn(module)?;
    let mut stdin = child.stdin.take().ok_or("no stdin")?;
    let mut stdout = child.stdout.take().ok_or("no stdout")?;
    let line = format!("{request}\n");
    let run = async {
        let _ = stdin.write_all(line.as_bytes()).await;
        let _ = stdin.shutdown().await;
        drop(stdin);
        let _ = tokio::io::copy(&mut stdout, &mut tokio::io::sink()).await;
        child.wait().await
    };
    let timeout = module.timeout();
    let result = match tokio::time::timeout(timeout, run).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(e)) => Err(format!("cannot wait for the module: {e}")),
        Err(_) => Err(format!("timed out after {} ms", timeout.as_millis())),
    };
    let _ = child.start_kill();
    let _ = tokio::time::timeout(Duration::from_secs(5), child.wait()).await;
    result
}

/// The first line of `out`, capped at [`MAX_STDOUT`] bytes.
async fn read_line(out: &mut tokio::process::ChildStdout) -> Result<String, String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        if let Some(end) = buf.iter().position(|&b| b == b'\n') {
            buf.truncate(end);
            break;
        }
        if buf.len() > MAX_STDOUT {
            return Err("response is larger than 64 KiB".into());
        }
        let n = out
            .read(&mut chunk)
            .await
            .map_err(|e| format!("cannot read response: {e}"))?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    if buf.len() > MAX_STDOUT {
        return Err("response is larger than 64 KiB".into());
    }
    if buf.last() == Some(&b'\r') {
        buf.pop();
    }
    String::from_utf8(buf).map_err(|_| "response is not UTF-8".into())
}
