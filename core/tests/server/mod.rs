//! Tiny HTTP/1.1 test server with optional Range support and per-connection throttling.
#![allow(dead_code)]
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
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
    pub bytes_sent: AtomicU64,
    pub max_parallel: AtomicUsize,
    active: AtomicUsize,
    pub port: u16,
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
        bytes_sent: AtomicU64::new(0),
        max_parallel: AtomicUsize::new(0),
        active: AtomicUsize::new(0),
        port,
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
    let data = s.data.lock().unwrap().clone();
    let etag = s.etag.lock().unwrap().clone();
    let total = data.len() as u64;
    let mut range = None;
    if s.range {
        if let Some(r) = header("Range") {
            let if_range_ok = header("If-Range").is_none_or(|v| v == etag);
            if if_range_ok {
                s.range_requests.lock().unwrap().push(r.clone());
                let spec = r.trim_start_matches("bytes=");
                let (a, b) = spec.split_once('-').unwrap();
                let a: u64 = a.parse().unwrap();
                let b: u64 = if b.is_empty() { total - 1 } else { b.parse::<u64>().unwrap().min(total - 1) };
                range = Some((a, b));
            }
        }
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
    head += "\r\n";
    sock.write_all(head.as_bytes()).await?;
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
