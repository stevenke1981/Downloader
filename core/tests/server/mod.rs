//! Tiny HTTP/1.1 test server with optional Range support and per-connection throttling.
#![allow(dead_code)]
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

pub struct Server {
    pub data: Mutex<Arc<Vec<u8>>>,
    pub range: bool,
    /// Bytes/s per connection (0 = unlimited).
    pub per_conn_rate: u64,
    pub etag: Mutex<String>,
    pub requests: AtomicUsize,
    pub range_requests: Mutex<Vec<String>>,
    /// `Cookie` header of every request ("" when absent), in arrival order.
    pub cookies: Mutex<Vec<String>>,
    pub bytes_sent: AtomicU64,
    pub max_parallel: AtomicUsize,
    active: AtomicUsize,
    pub port: u16,
    /// Extra files served by exact name (checksum lists, sidecars...).
    pub extra: Mutex<std::collections::HashMap<String, Vec<u8>>>,
    /// Extra response headers for the main file (e.g. Digest).
    pub headers: Mutex<Vec<(String, String)>>,
    /// Flip one byte in the next N data responses of the main file.
    pub corrupt: AtomicUsize,
    /// Number of full/partial data requests for the main file.
    pub data_requests: AtomicUsize,
    /// Answer every main-file request with 302 to this base URL plus the file name.
    pub redirect: Mutex<Option<String>>,
    /// Answer the main file with 403 when the request has no Cookie header.
    pub require_cookie: AtomicBool,
}

fn is_meta(name: &str) -> bool {
    let l = name.to_ascii_lowercase();
    ["sha256", "sha512", "sha1", "md5", "b2", "sig", "asc", "sum", "sums", "sum.txt", "sums.txt", "checksums", "checksum", "checksums.txt"]
        .iter()
        .any(|e| l.ends_with(&format!(".{e}")) || l == *e || l.ends_with(e))
}

pub fn test_data(n: usize) -> Vec<u8> {
    let mut x = 0x12345678u32;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x as u8
        })
        .collect()
}

impl Server {
    pub fn url(&self, name: &str) -> String {
        format!("http://127.0.0.1:{}/files/{name}", self.port)
    }
    pub fn set_data(&self, d: Vec<u8>, etag: &str) {
        *self.data.lock().unwrap() = Arc::new(d);
        *self.etag.lock().unwrap() = etag.into();
    }
}

pub async fn start(data: Vec<u8>, range: bool, per_conn_rate: u64) -> Arc<Server> {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let s = Arc::new(Server {
        data: Mutex::new(Arc::new(data)),
        range,
        per_conn_rate,
        etag: Mutex::new("\"v1\"".into()),
        requests: AtomicUsize::new(0),
        range_requests: Mutex::new(vec![]),
        cookies: Mutex::new(vec![]),
        bytes_sent: AtomicU64::new(0),
        max_parallel: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        port,
        extra: Default::default(),
        headers: Default::default(),
        corrupt: AtomicUsize::new(0),
        data_requests: AtomicUsize::new(0),
        redirect: Mutex::new(None),
        require_cookie: AtomicBool::new(false),
    });
    let srv = s.clone();
    tokio::spawn(async move {
        loop {
            let (sock, _) = l.accept().await.unwrap();
            let srv = srv.clone();
            tokio::spawn(async move {
                let _ = handle(srv, sock).await;
            });
        }
    });
    s
}

async fn handle(s: Arc<Server>, mut sock: tokio::net::TcpStream) -> std::io::Result<()> {
    let mut buf = Vec::new();
    let mut tmp = [0u8; 4096];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    s.requests.fetch_add(1, Ordering::SeqCst);
    let req = String::from_utf8_lossy(&buf).to_string();
    let header = |name: &str| {
        req.lines()
            .find(|l| l.to_ascii_lowercase().starts_with(&format!("{}:", name.to_ascii_lowercase())))
            .map(|l| l[name.len() + 1..].trim().to_string())
    };
    s.cookies.lock().unwrap().push(header("Cookie").unwrap_or_default());
    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
    let name = path.rsplit('/').next().unwrap_or("").to_string();
    let extra = s.extra.lock().unwrap().get(&name).cloned();
    if let Some(body) = extra {
        let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: text/plain\r\nConnection: close\r\n\r\n", body.len());
        sock.write_all(head.as_bytes()).await?;
        return sock.write_all(&body).await;
    }
    if is_meta(&name) {
        return sock.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    }
    // Bind the guard first: a guard alive across an await would make this future non-Send.
    let redirect = s.redirect.lock().unwrap().clone();
    if let Some(base) = redirect {
        let head = format!("HTTP/1.1 302 Found\r\nLocation: {base}{name}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
        return sock.write_all(head.as_bytes()).await;
    }
    if s.require_cookie.load(Ordering::SeqCst) && header("Cookie").is_none() {
        return sock.write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await;
    }
    let mut data = s.data.lock().unwrap().clone();
    let etag = s.etag.lock().unwrap().clone();
    let total = data.len() as u64;
    let mut range = None;
    if s.range
        && let Some(r) = header("Range")
        && header("If-Range").is_none_or(|v| v == etag)
    {
        s.range_requests.lock().unwrap().push(r.clone());
        let spec = r.trim_start_matches("bytes=");
        let (a, b) = spec.split_once('-').unwrap();
        let a: u64 = a.parse().unwrap();
        let b: u64 = if b.is_empty() { total - 1 } else { b.parse::<u64>().unwrap().min(total - 1) };
        range = Some((a, b));
    }
    let (status, start, end) = match range {
        Some((a, b)) => ("206 Partial Content", a, b + 1),
        None => ("200 OK", 0, total),
    };
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\nContent-Type: application/octet-stream\r\nConnection: close\r\n",
        end - start
    );
    if s.range {
        head += &format!("Accept-Ranges: bytes\r\nETag: {etag}\r\n");
    }
    if range.is_some() {
        head += &format!("Content-Range: bytes {start}-{}/{total}\r\n", end - 1);
    }
    for (k, v) in s.headers.lock().unwrap().iter() {
        head += &format!("{k}: {v}\r\n");
    }
    head += "\r\n";
    sock.write_all(head.as_bytes()).await?;
    if end - start > 1 {
        s.data_requests.fetch_add(1, Ordering::SeqCst);
        if s.corrupt.load(Ordering::SeqCst) > 0 {
            s.corrupt.fetch_sub(1, Ordering::SeqCst);
            let mut d = (*data).clone();
            d[start as usize] ^= 0xff;
            data = Arc::new(d);
        }
    }
    let now = s.active.fetch_add(1, Ordering::SeqCst) + 1;
    s.max_parallel.fetch_max(now, Ordering::SeqCst);
    let t0 = std::time::Instant::now();
    let mut sent = 0u64;
    let mut res = Ok(());
    for chunk in data[start as usize..end as usize].chunks(16 * 1024) {
        if s.per_conn_rate > 0 {
            let due = std::time::Duration::from_secs_f64(sent as f64 / s.per_conn_rate as f64);
            if let Some(w) = due.checked_sub(t0.elapsed()) {
                tokio::time::sleep(w).await;
            }
        }
        if let Err(e) = sock.write_all(chunk).await {
            res = Err(e);
            break;
        }
        sent += chunk.len() as u64;
        s.bytes_sent.fetch_add(chunk.len() as u64, Ordering::SeqCst);
    }
    s.active.fetch_sub(1, Ordering::SeqCst);
    res
}
