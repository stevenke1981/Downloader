//! Opt-in token-authenticated HTTP queue control. No settings or automation routes.
use crate::{Manager, NewDownload};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{net::SocketAddr, path::PathBuf, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct RemoteSettings {
    pub enabled: bool,
    pub bind: String,
    pub token: String,
}

impl Default for RemoteSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            bind: "127.0.0.1:8765".into(),
            token: String::new(),
        }
    }
}

impl RemoteSettings {
    pub fn validate(&self) -> Result<(), String> {
        if !self.enabled {
            return Ok(());
        }
        self.bind
            .parse::<SocketAddr>()
            .map_err(|_| "remote bind must be an IP address and port".to_string())?;
        if !(16..=512).contains(&self.token.len())
            || !self.token.bytes().all(|byte| byte.is_ascii_graphic())
        {
            return Err("remote control needs a token of 16–512 visible ASCII characters".into());
        }
        Ok(())
    }
}

/// A 256-bit token from the operating system random generator.
pub fn generate_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        unsafe extern "system" {
            fn BCryptGenRandom(
                algorithm: *mut std::ffi::c_void,
                buffer: *mut u8,
                length: u32,
                flags: u32,
            ) -> i32;
        }
        // BCRYPT_USE_SYSTEM_PREFERRED_RNG permits a null algorithm handle.
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                bytes.as_mut_ptr(),
                bytes.len() as u32,
                2,
            )
        };
        if status < 0 {
            return Err("system token generation failed".into());
        }
    }
    #[cfg(not(windows))]
    {
        use std::io::Read;
        std::fs::File::open("/dev/urandom")
            .and_then(|mut file| file.read_exact(&mut bytes))
            .map_err(|e| e.to_string())?;
    }
    Ok(hex::encode(bytes))
}

pub struct RemoteServer {
    address: SocketAddr,
    cancel: CancellationToken,
    handle: Option<tokio::task::JoinHandle<()>>,
}

impl RemoteServer {
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn stop(&mut self) {
        self.cancel.cancel();
        if let Some(handle) = self.handle.take() {
            handle.abort();
        }
    }
}

impl Drop for RemoteServer {
    fn drop(&mut self) {
        self.stop();
    }
}

pub fn serve(manager: Manager, settings: RemoteSettings) -> Result<RemoteServer, String> {
    settings.validate()?;
    if !settings.enabled {
        return Err("remote control is disabled".into());
    }
    let listener =
        std::net::TcpListener::bind(&settings.bind).map_err(|e| format!("remote bind: {e}"))?;
    listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let address = listener.local_addr().map_err(|e| e.to_string())?;
    let runtime = manager.runtime();
    let _guard = runtime.enter();
    let listener = tokio::net::TcpListener::from_std(listener).map_err(|e| e.to_string())?;
    let cancel = CancellationToken::new();
    let stopped = cancel.clone();
    let handle = runtime.spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                _ = stopped.cancelled() => break,
                _ = connections.join_next(), if !connections.is_empty() => {},
                accepted = listener.accept() => {
                    let Ok((mut stream, _)) = accepted else { break };
                    if connections.len() >= 32 { continue; }
                    let manager = manager.clone();
                    let token = settings.token.clone();
                    connections.spawn(async move {
                        let response = tokio::time::timeout(Duration::from_secs(5), read_request(&mut stream)).await;
                        let (status, body) = match response {
                            Ok(Ok(request)) => {
                                let expected = format!("Bearer {token}");
                                if request.authorization.as_deref() != Some(expected.as_str()) {
                                    (401, json!({"error":"authentication required","code":"auth"}))
                                } else { route(&manager, request) }
                            }
                            Ok(Err(error)) => (400, json!({"error":error,"code":"invalid"})),
                            Err(_) => (408, json!({"error":"request timed out","code":"network"})),
                        };
                        let body = serde_json::to_vec(&body).unwrap_or_default();
                        let reason = match status { 200 => "OK", 201 => "Created", 400 => "Bad Request", 401 => "Unauthorized", 404 => "Not Found", 409 => "Conflict", 408 => "Request Timeout", _ => "Error" };
                        let headers = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\nCache-Control: no-store\r\n\r\n", body.len());
                        let _ = tokio::time::timeout(Duration::from_secs(5), async {
                            stream.write_all(headers.as_bytes()).await?;
                            stream.write_all(&body).await
                        }).await;
                    });
                }
            }
        }
    });
    Ok(RemoteServer {
        address,
        cancel,
        handle: Some(handle),
    })
}

struct Request {
    method: String,
    path: String,
    authorization: Option<String>,
    body: Vec<u8>,
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Result<Request, &'static str> {
    const HEADER_LIMIT: usize = 16 * 1024;
    const BODY_LIMIT: usize = 64 * 1024;
    // A queue export can be large.
    const IMPORT_LIMIT: usize = 1024 * 1024;
    let mut bytes = Vec::new();
    let header_end = loop {
        let mut chunk = [0; 4096];
        let count = stream
            .read(&mut chunk)
            .await
            .map_err(|_| "request read failed")?;
        if count == 0 {
            return Err("incomplete request");
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            if index + 4 > HEADER_LIMIT {
                return Err("headers too large");
            }
            break index + 4;
        }
        if bytes.len() > HEADER_LIMIT {
            return Err("headers too large");
        }
    };
    let header = std::str::from_utf8(&bytes[..header_end]).map_err(|_| "headers must be UTF-8")?;
    let mut lines = header.split("\r\n");
    let request_line: Vec<_> = lines
        .next()
        .ok_or("missing request line")?
        .split_whitespace()
        .collect();
    if request_line.len() != 3
        || !matches!(request_line[2], "HTTP/1.0" | "HTTP/1.1")
        || !request_line[1].starts_with('/')
    {
        return Err("invalid request line");
    }
    let method = request_line[0].to_owned();
    let path = request_line[1].to_owned();
    let mut length = None;
    let mut authorization = None;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line.split_once(':').ok_or("invalid header")?;
        match name.to_ascii_lowercase().as_str() {
            "content-length" => {
                if length.is_some() {
                    return Err("duplicate Content-Length");
                }
                length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .map_err(|_| "invalid Content-Length")?,
                );
            }
            "authorization" => {
                if authorization.is_some() {
                    return Err("duplicate Authorization");
                }
                authorization = Some(value.trim().to_owned());
            }
            "transfer-encoding" => return Err("Transfer-Encoding is not supported"),
            _ => {}
        }
    }
    let length = length.unwrap_or(0);
    if length
        > if path.split('?').next() == Some("/import") {
            IMPORT_LIMIT
        } else {
            BODY_LIMIT
        }
    {
        return Err("request body too large");
    }
    while bytes.len() < header_end + length {
        let mut chunk = [0; 4096];
        let count = stream
            .read(&mut chunk)
            .await
            .map_err(|_| "request read failed")?;
        if count == 0 {
            return Err("incomplete request body");
        }
        bytes.extend_from_slice(&chunk[..count]);
    }
    Ok(Request {
        method,
        path,
        authorization,
        body: bytes[header_end..header_end + length].to_vec(),
    })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AddDownload {
    url: String,
    dir: Option<PathBuf>,
    connections: Option<u32>,
    speed_limit: Option<u64>,
    #[serde(default)]
    kind: crate::task::DownloadKind,
    #[serde(default = "start_by_default")]
    start: bool,
    file_name: Option<String>,
    #[serde(default)]
    priority: i32,
    /// "sha256:<hex>" or a bare digest.
    checksum: Option<String>,
    referer: Option<String>,
    /// An object `{"Name":"value"}` or a list of `["Name","value"]` pairs.
    #[serde(default)]
    headers: Headers,
    user_agent: Option<String>,
    #[serde(default)]
    unique: bool,
    conflict: Option<crate::task::ConflictPolicy>,
    /// Alternative http(s) sources for the same file.
    #[serde(default)]
    mirrors: Vec<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Headers {
    Map(std::collections::BTreeMap<String, String>),
    List(Vec<(String, String)>),
}

impl Default for Headers {
    fn default() -> Self {
        Headers::List(Vec::new())
    }
}

impl Headers {
    fn into_vec(self) -> Vec<(String, String)> {
        match self {
            Headers::Map(m) => m.into_iter().collect(),
            Headers::List(l) => l,
        }
    }
}

fn error_body(error: &str) -> Value {
    json!({"error":error,"code":crate::util::error_code(error)})
}

/// Duplicates and file conflicts are 409; everything else the caller can fix is 400.
fn error_status(error: &str) -> u16 {
    match crate::util::error_code(error) {
        "duplicate" | "conflict" => 409,
        _ => 400,
    }
}

fn start_by_default() -> bool {
    true
}

fn route(manager: &Manager, request: Request) -> (u16, Value) {
    let (path, query) = request
        .path
        .split_once('?')
        .unwrap_or((request.path.as_str(), ""));
    let start = !query
        .split('&')
        .any(|q| q == "start=0" || q == "start=false");
    match (request.method.as_str(), path) {
        ("GET", "/downloads") => (200, json!(manager.snapshots())),
        ("POST", "/downloads") => {
            let download: AddDownload = match serde_json::from_slice(&request.body) {
                Ok(download) => download,
                Err(_) => {
                    return (
                        400,
                        json!({"error":"invalid download fields","code":"invalid"}),
                    );
                }
            };
            if download
                .connections
                .is_some_and(|count| !(1..=32).contains(&count))
            {
                return (
                    400,
                    json!({"error":"connections must be 1–32","code":"invalid"}),
                );
            }
            let mirrors: Vec<String> = download
                .mirrors
                .iter()
                .map(|mirror| mirror.trim().to_string())
                .collect();
            let mirrors_ok = mirrors.len() <= 8
                && mirrors.iter().all(|mirror| {
                    reqwest::Url::parse(mirror)
                        .is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
                        && crate::util::validate_source(mirror).is_ok()
                });
            if !mirrors_ok {
                return (
                    400,
                    json!({"error":"mirrors must be at most 8 valid http(s) URLs","code":"invalid"}),
                );
            }
            let checksum = match download.checksum.as_deref() {
                None | Some("") => None,
                Some(text) => match crate::checksum::Checksum::guess(text) {
                    Some(c) => Some(c),
                    None => {
                        return (400, json!({"error":"invalid checksum","code":"invalid"}));
                    }
                },
            };
            match manager.try_add(NewDownload {
                priority: download.priority,
                checksum,
                referer: download.referer,
                headers: download.headers.into_vec(),
                user_agent: download.user_agent,
                unique: download.unique,
                conflict: download.conflict,
                url: download.url,
                dir: download.dir,
                connections: download.connections,
                speed_limit: download.speed_limit,
                kind: download.kind,
                start: download.start,
                file_name: download.file_name,
                mirrors,
                ..Default::default()
            }) {
                Ok(id) => (201, json!({"id":id})),
                Err(error) => (error_status(&error), error_body(&error)),
            }
        }
        ("GET", "/export") => match serde_json::from_str::<Value>(&manager.export_queue()) {
            Ok(queue) => (200, queue),
            Err(e) => (500, error_body(&e.to_string())),
        },
        ("POST", "/import") => {
            let Ok(text) = std::str::from_utf8(&request.body) else {
                return (
                    400,
                    json!({"error":"invalid queue file: not UTF-8","code":"invalid"}),
                );
            };
            match manager.import_queue(text, start) {
                Ok(report) => (200, json!(report)),
                Err(error) => (400, error_body(&error)),
            }
        }
        ("GET", "/modules") => (200, json!(manager.modules())),
        ("GET", "/version") => (200, json!({"version":env!("CARGO_PKG_VERSION")})),
        ("POST", "/start-all") => {
            manager.start_all();
            (200, json!({"ok":true}))
        }
        ("POST", "/pause-all") => {
            manager.pause_all();
            (200, json!({"ok":true}))
        }
        _ => {
            let parts: Vec<_> = path.trim_start_matches('/').split('/').collect();
            let id = parts.get(1).and_then(|id| id.parse::<u64>().ok());
            if parts.first() != Some(&"downloads")
                || !id
                    .is_some_and(|id| manager.snapshots().iter().any(|download| download.id == id))
            {
                return (
                    404,
                    json!({"error":"unknown route or download","code":"invalid"}),
                );
            }
            let id = id.unwrap();
            match (request.method.as_str(), parts.as_slice()) {
                ("POST", [_, _, "start"]) => manager.start(id),
                ("POST", [_, _, "pause"]) => manager.pause(id),
                ("GET", [_, _]) => {
                    return match manager.snapshots().into_iter().find(|d| d.id == id) {
                        Some(d) => (200, json!(d)),
                        None => (404, json!({"error":"unknown download","code":"invalid"})),
                    };
                }
                ("DELETE", [_, _]) => manager.remove(id, false),
                _ => return (404, json!({"error":"unknown route","code":"invalid"})),
            }
            (200, json!({"ok":true}))
        }
    }
}
