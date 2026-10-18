//! Local IPC between the native-messaging host (or any local tool) and the running app.
//!
//! The app listens on 127.0.0.1 (random port) and writes `{port, token}` to
//! `<state dir>/ipc.json`. Each request is one JSON line; each reply is one JSON line.
use serde::{Deserialize, Serialize};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(default)]
pub struct AddRequest {
    pub url: String,
    pub referrer: Option<String>,
    pub filename: Option<String>,
    /// Checksum list / sidecar links found on the page.
    pub checksum_urls: Vec<String>,
    /// A hash the user typed (algorithm detected by length).
    pub checksum: Option<String>,
    /// `Cookie` header value for the URL (from the browser extension).
    pub cookie: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "cmd", rename_all = "lowercase")]
pub enum Command {
    Ping,
    Add { downloads: Vec<AddRequest> },
    Show,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Envelope {
    token: String,
    #[serde(flatten)]
    command: Command,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Reply {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

impl Reply {
    pub fn ok() -> Reply {
        Reply { ok: true, error: None, version: Some(env!("CARGO_PKG_VERSION").into()) }
    }
    pub fn err(e: impl Into<String>) -> Reply {
        Reply { ok: false, error: Some(e.into()), version: None }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
}

pub fn endpoint_path(state_dir: &Path) -> PathBuf {
    state_dir.join("ipc.json")
}

pub fn random_token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut out = String::new();
    for i in 0..4u64 {
        let mut h = std::collections::hash_map::RandomState::new().build_hasher();
        h.write_u64(i ^ std::process::id() as u64);
        h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
        out += &format!("{:016x}", h.finish());
    }
    out
}

/// Start the server on `rt`; `handler` runs on a tokio worker thread.
/// Writes the endpoint file and returns it.
pub fn serve(
    rt: &tokio::runtime::Handle,
    state_dir: &Path,
    handler: impl Fn(Command) -> Reply + Send + Sync + 'static,
) -> std::io::Result<Endpoint> {
    let std_listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    std_listener.set_nonblocking(true)?;
    let port = std_listener.local_addr()?.port();
    let token = random_token();
    let ep = Endpoint { port, token: token.clone() };
    std::fs::create_dir_all(state_dir)?;
    write_endpoint(&endpoint_path(state_dir), &serde_json::to_vec(&ep).unwrap())?;
    let handler = std::sync::Arc::new(handler);
    rt.spawn(async move {
        let listener = tokio::net::TcpListener::from_std(std_listener).unwrap();
        loop {
            let Ok((sock, _)) = listener.accept().await else { continue };
            let (handler, token) = (handler.clone(), token.clone());
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                // Bound per connection. A "download all links" batch can exceed 1 MiB, and the native host accepts up to 4 MiB.
                let mut lines = tokio::io::BufReader::new(r.take(8 << 20)).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    // A wrong token closes the connection after the error reply.
                    let (reply, close) = match serde_json::from_str::<Envelope>(&line) {
                        Ok(env) if ct_eq(&env.token, &token) => (handler(env.command), false),
                        Ok(_) => (Reply::err("bad token"), true),
                        Err(e) => (Reply::err(format!("bad request: {e}")), false),
                    };
                    let mut out = serde_json::to_vec(&reply).unwrap();
                    out.push(b'\n');
                    if w.write_all(&out).await.is_err() || close {
                        break;
                    }
                }
            });
        }
    });
    Ok(ep)
}

/// Compare two secrets without an early exit on the first differing byte.
fn ct_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() != b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        diff |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    diff == 0
}

/// Write the endpoint file (it holds the token). On unix it is readable by the owner only.
fn write_endpoint(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    #[cfg(unix)]
    {
        // The mode applies only when the file is created; tighten a pre-existing file too.
        use std::os::unix::fs::PermissionsExt;
        f.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    f.write_all(bytes)
}

use tokio::io::AsyncReadExt as _;

/// Blocking client: send one command to the endpoint and wait for the reply.
pub fn send(ep: &Endpoint, cmd: Command) -> std::io::Result<Reply> {
    let addr = std::net::SocketAddr::from(([127, 0, 0, 1], ep.port));
    let mut s = TcpStream::connect_timeout(&addr, Duration::from_secs(2))?;
    s.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut line = serde_json::to_vec(&Envelope { token: ep.token.clone(), command: cmd }).unwrap();
    line.push(b'\n');
    s.write_all(&line)?;
    let mut reply = String::new();
    BufReader::new(s).read_line(&mut reply)?;
    serde_json::from_str(&reply).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Read the endpoint file and send; fails if the app isn't running.
pub fn send_to_app(state_dir: &Path, cmd: Command) -> std::io::Result<Reply> {
    let ep: Endpoint = serde_json::from_slice(&std::fs::read(endpoint_path(state_dir))?)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    send(&ep, cmd)
}

/// Default state directory shared by the app and the native host.
pub fn default_state_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("DOWNLOADER_STATE_DIR") {
        return d.into();
    }
    dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("Downloader")
}
