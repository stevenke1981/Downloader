//! Download queue: concurrency limit, priorities, scheduler, persistence.
use crate::category::Category;
use crate::checksum::Checksum;
use crate::engine::{self, Ctx, Outcome};
use crate::limiter::RateLimiter;
use crate::settings::Settings;
use crate::task::{Status, Task};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

struct Entry {
    task: Arc<Mutex<Task>>,
    limiter: Arc<RateLimiter>,
    cancel: Option<CancellationToken>,
    handle: Option<JoinHandle<()>>,
    speed: f64,
    last_bytes: u64,
}

impl Entry {
    fn running(&self) -> bool {
        self.handle.as_ref().is_some_and(|h| !h.is_finished())
    }
}

struct Inner {
    entries: Mutex<Vec<Entry>>,
    settings: Mutex<Settings>,
    client: Mutex<reqwest::Client>,
    global: Arc<RateLimiter>,
    dir: PathBuf,
    next_id: AtomicU64,
    rt: tokio::runtime::Handle,
    /// Bytes of the last downloads.json written, so unchanged autosaves are skipped.
    last_saved: Mutex<Vec<u8>>,
    /// Set when downloads.json existed but could not be read: saves go to downloads.json.recovered so the
    /// user's list is never replaced with this session's.
    save_disabled: AtomicBool,
}

/// A point-in-time view of one download for the UI.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub id: u64,
    pub url: String,
    pub name: String,
    pub path: PathBuf,
    pub total: Option<u64>,
    pub downloaded: u64,
    pub speed: f64,
    pub eta: Option<u64>,
    pub status: Status,
    pub category: Category,
    pub error: Option<String>,
    /// (start, pos, end) of each segment.
    pub segments: Vec<(u64, u64, u64)>,
    pub connections: u32,
    pub active_connections: u32,
    pub speed_limit: u64,
    pub priority: i32,
    pub resumable: bool,
    pub verification: Option<crate::task::Verification>,
    /// 0..1 while hashing.
    pub verify_progress: f32,
}

#[derive(Debug, Clone, Default)]
pub struct NewDownload {
    pub url: String,
    /// None = folder from settings (per category).
    pub dir: Option<PathBuf>,
    /// None = from settings.
    pub connections: Option<u32>,
    pub speed_limit: Option<u64>,
    pub checksum: Option<Checksum>,
    pub priority: i32,
    /// false = add paused.
    pub start: bool,
    pub referer: Option<String>,
    /// File name to use instead of the server's.
    pub file_name: Option<String>,
    /// Checksum list / sidecar URLs seen on the page.
    pub checksum_urls: Vec<String>,
    /// `Cookie` header sent with every request (kept in memory only).
    pub cookie: Option<String>,
}

pub struct Manager {
    inner: Arc<Inner>,
    rt: tokio::runtime::Runtime,
}

impl Manager {
    /// `dir` holds settings.json and downloads.json.
    pub fn new(dir: &Path) -> Manager {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(4).build().expect("tokio runtime");
        let (settings, _) = load_json::<Settings>(&dir.join("settings.json"));
        let (mut tasks, tasks_readable) = load_json::<Vec<Task>>(&dir.join("downloads.json"));
        for t in &mut tasks {
            if matches!(t.status, Status::Downloading | Status::Verifying) {
                t.status = Status::Queued;
            }
        }
        let next = tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        let client = settings.build_client().or_else(|_| Settings::default().build_client()).expect("http client");
        let global = Arc::new(RateLimiter::new(settings.global_limit));
        let entries = tasks
            .into_iter()
            .map(|t| Entry {
                limiter: Arc::new(RateLimiter::new(t.speed_limit)),
                // Start counting from what's already on disk, or a resumed job shows a bogus speed spike.
                last_bytes: t.downloaded(),
                task: Arc::new(Mutex::new(t)),
                cancel: None,
                handle: None,
                speed: 0.0,
            })
            .collect();
        let inner = Arc::new(Inner {
            entries: Mutex::new(entries),
            settings: Mutex::new(settings),
            client: Mutex::new(client),
            global,
            dir: dir.to_path_buf(),
            next_id: AtomicU64::new(next),
            rt: rt.handle().clone(),
            last_saved: Mutex::new(Vec::new()),
            save_disabled: AtomicBool::new(!tasks_readable),
        });
        let tick = inner.clone();
        rt.spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(500));
            let mut last = Instant::now();
            let mut last_save = Instant::now();
            loop {
                iv.tick().await;
                let dt = last.elapsed().as_secs_f64().max(0.001);
                last = Instant::now();
                tick.tick(dt);
                if last_save.elapsed() > Duration::from_secs(3) {
                    last_save = Instant::now();
                    tick.save();
                }
            }
        });
        Manager { inner, rt }
    }

    pub fn settings(&self) -> Settings {
        self.inner.settings.lock().unwrap().clone()
    }

    pub fn set_settings(&self, s: Settings) -> Result<(), String> {
        let client = s.build_client()?;
        let _ = std::fs::create_dir_all(&self.inner.dir);
        let json = serde_json::to_vec_pretty(&s).unwrap();
        {
            // Serialize settings changes with dispatch. A timer tick must not
            // start queued jobs using a limit captured before this update.
            let _dispatch = self.inner.entries.lock().unwrap();
            *self.inner.client.lock().unwrap() = client;
            self.inner.global.set_rate(s.global_limit);
            *self.inner.settings.lock().unwrap() = s;
        }
        write_atomic(&self.inner.dir.join("settings.json"), &json).map_err(|e| e.to_string())
    }

    pub fn add(&self, d: NewDownload) -> u64 {
        let s = self.settings();
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let name = d.file_name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| crate::util::name_from_url(&d.url));
        let cat = Category::from_name(&name);
        let limit = d.speed_limit.unwrap_or(s.per_download_limit);
        let cookie = match d.cookie.filter(|c| !c.trim().is_empty()) {
            Some(c) if usable_cookie(&c) => Some(c),
            Some(_) => {
                // Never print the value: it is a session secret.
                eprintln!("a cookie was dropped: it is not a valid HTTP header value or is longer than 8 KiB");
                None
            }
            None => None,
        };
        let had_cookie = cookie.is_some();
        let t = Task {
            id,
            url: d.url.trim().to_string(),
            dir: d.dir.unwrap_or_else(|| s.folder_for(cat)),
            connections: d.connections.unwrap_or(s.max_connections).clamp(1, 32),
            speed_limit: limit,
            checksum: d.checksum,
            priority: d.priority,
            category: cat,
            status: if d.start { Status::Queued } else { Status::Paused },
            added: chrono::Utc::now().timestamp(),
            referer: d.referer.filter(|r| r.starts_with("http")),
            suggested_name: d.file_name,
            checksum_urls: d.checksum_urls,
            cookie,
            had_cookie,
            ..Default::default()
        };
        let last_bytes = t.downloaded();
        self.inner.entries.lock().unwrap().push(Entry {
            task: Arc::new(Mutex::new(t)),
            limiter: Arc::new(RateLimiter::new(limit)),
            cancel: None,
            handle: None,
            speed: 0.0,
            last_bytes,
        });
        self.save_async();
        self.inner.dispatch();
        id
    }

    /// Persist on the runtime, not the caller's thread: saving fsyncs, which can stall the GUI on a slow disk.
    /// `save_with` holds its lock across snapshot and write, so the last queued save always writes the newest state.
    fn save_async(&self) {
        let me = self.inner.clone();
        self.rt.spawn_blocking(move || me.save());
    }

    fn with<R>(&self, id: u64, f: impl FnOnce(&mut Entry) -> R) -> Option<R> {
        let mut es = self.inner.entries.lock().unwrap();
        es.iter_mut().find(|e| e.task.lock().unwrap().id == id).map(f)
    }

    /// Queue a paused/failed download (it starts when a slot is free).
    pub fn start(&self, id: u64) {
        self.with(id, |e| {
            let mut t = e.task.lock().unwrap();
            if matches!(t.status, Status::Paused | Status::Error) {
                t.status = Status::Queued;
                t.error = None;
                t.verify_retried = false;
            }
        });
        self.inner.dispatch();
    }

    pub fn pause(&self, id: u64) {
        self.with(id, |e| {
            let mut t = e.task.lock().unwrap();
            if matches!(t.status, Status::Queued | Status::Downloading) {
                t.status = Status::Paused;
                if let Some(c) = &e.cancel {
                    c.cancel();
                }
            }
        });
        self.save_async();
    }

    pub fn start_all(&self) {
        let ids: Vec<u64> = self.snapshots().iter().map(|s| s.id).collect();
        ids.into_iter().for_each(|i| self.start(i));
    }
    pub fn pause_all(&self) {
        let ids: Vec<u64> = self.snapshots().iter().map(|s| s.id).collect();
        ids.into_iter().for_each(|i| self.pause(i));
    }

    /// Remove from the list; optionally delete the downloaded / partial file.
    pub fn remove(&self, id: u64, delete_file: bool) {
        let entry = {
            let mut es = self.inner.entries.lock().unwrap();
            let Some(i) = es.iter().position(|e| e.task.lock().unwrap().id == id) else { return };
            es.remove(i)
        };
        if let Some(c) = &entry.cancel {
            c.cancel();
        }
        let t = entry.task.lock().unwrap().clone();
        let handle = entry.handle;
        self.rt.spawn(async move {
            if let Some(h) = handle {
                let _ = h.await;
            }
            if !t.file_name.is_empty() {
                let _ = std::fs::remove_file(t.part_path());
                if delete_file {
                    let _ = std::fs::remove_file(t.final_path());
                }
            }
        });
        self.save_async();
    }

    pub fn set_priority(&self, id: u64, p: i32) {
        self.with(id, |e| e.task.lock().unwrap().priority = p);
    }
    pub fn set_speed_limit(&self, id: u64, bps: u64) {
        self.with(id, |e| {
            e.task.lock().unwrap().speed_limit = bps;
            e.limiter.set_rate(bps);
        });
    }
    pub fn set_connections(&self, id: u64, n: u32) {
        self.with(id, |e| e.task.lock().unwrap().connections = n.clamp(1, 32));
    }

    pub fn snapshots(&self) -> Vec<Snapshot> {
        let es = self.inner.entries.lock().unwrap();
        es.iter()
            .map(|e| {
                let t = e.task.lock().unwrap();
                let downloaded = t.downloaded();
                let running = e.running() && t.status == Status::Downloading;
                let speed = if running { e.speed } else { 0.0 };
                let eta = match (t.total, speed > 1.0) {
                    (Some(n), true) => Some(((n.saturating_sub(downloaded)) as f64 / speed) as u64),
                    _ => None,
                };
                Snapshot {
                    id: t.id,
                    url: t.url.clone(),
                    name: t.display_name(),
                    path: if t.file_name.is_empty() { t.dir.clone() } else { t.final_path() },
                    total: t.total,
                    downloaded,
                    speed,
                    eta,
                    status: t.status,
                    category: t.category,
                    error: t.error.clone(),
                    segments: t.segments.iter().map(|s| (s.start, s.pos, s.end)).collect(),
                    connections: t.connections,
                    active_connections: if running { t.segments.iter().filter(|s| s.busy).count() as u32 } else { 0 },
                    speed_limit: t.speed_limit,
                    priority: t.priority,
                    resumable: t.resumable,
                    verification: t.verification.clone(),
                    verify_progress: match t.total { Some(n) if n > 0 => (t.verify_done.load(Ordering::Relaxed) as f64 / n as f64).min(1.0) as f32, _ => 0.0 },
                }
            })
            .collect()
    }

    pub fn save(&self) {
        self.save_async();
    }

    /// Handle of the manager's tokio runtime (for the IPC server).
    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.rt.handle().clone()
    }

    /// Cancel everything and persist (call before exit).
    pub fn shutdown(self) {
        {
            let es = self.inner.entries.lock().unwrap();
            for e in es.iter() {
                if let Some(c) = &e.cancel {
                    c.cancel();
                }
            }
        }
        let handles: Vec<_> = self.inner.entries.lock().unwrap().iter_mut().filter_map(|e| e.handle.take()).collect();
        self.rt.block_on(async {
            for h in handles {
                let _ = tokio::time::timeout(Duration::from_secs(3), h).await;
            }
        });
        // Tasks that were cancelled by shutdown should resume next launch.
        self.inner.save_with(|t| if t.status == Status::Downloading { t.status = Status::Queued });
        self.rt.shutdown_timeout(Duration::from_secs(1));
    }
}

impl Inner {
    fn save(&self) {
        self.save_with(|_| {});
    }
    fn save_with(&self, f: impl Fn(&mut Task)) {
        // Held across snapshot + write so an older snapshot can't overwrite a newer one.
        let mut last = self.last_saved.lock().unwrap();
        let tasks: Vec<Task> = self
            .entries
            .lock()
            .unwrap()
            .iter()
            .map(|e| {
                let mut t = e.task.lock().unwrap().clone();
                f(&mut t);
                t
            })
            .collect();
        if let Ok(json) = serde_json::to_vec(&tasks) {
            if *last == json {
                return;
            }
            let _ = std::fs::create_dir_all(&self.dir);
            // downloads.json couldn't be read at startup: never overwrite it, but keep this session's list
            // in a side file so it can be recovered by hand.
            let name = if self.save_disabled.load(Ordering::Relaxed) { "downloads.json.recovered" } else { "downloads.json" };
            if write_atomic(&self.dir.join(name), &json).is_ok() {
                *last = json;
            }
        }
    }

    /// Timer loop only: update speeds from the bytes moved over the real elapsed `dt`, then dispatch.
    fn tick(self: &Arc<Self>, dt: f64) {
        {
            let mut es = self.entries.lock().unwrap();
            for e in es.iter_mut() {
                let b = e.task.lock().unwrap().downloaded();
                let inst = b.saturating_sub(e.last_bytes) as f64 / dt;
                e.speed = if e.speed == 0.0 { inst } else { e.speed * 0.6 + inst * 0.4 };
                e.last_bytes = b;
            }
        }
        self.dispatch();
    }

    /// Reap finished jobs and start queued ones. Never touches speeds (no real dt here).
    fn dispatch(self: &Arc<Self>) {
        let mut es = self.entries.lock().unwrap();
        let settings = self.settings.lock().unwrap().clone();
        let allowed = settings.schedule.allows_now();
        for e in es.iter_mut() {
            if e.handle.as_ref().is_some_and(|h| h.is_finished()) {
                e.handle = None;
                e.cancel = None;
            }
        }
        if !allowed {
            // Outside the schedule window: put running downloads back in the queue.
            for e in es.iter_mut() {
                let mut t = e.task.lock().unwrap();
                if t.status == Status::Downloading {
                    t.status = Status::Queued;
                    if let Some(c) = &e.cancel {
                        c.cancel();
                    }
                }
            }
            return;
        }
        let running = es.iter().filter(|e| e.running()).count();
        let slots = (settings.max_concurrent.max(1) as usize).saturating_sub(running);
        if slots == 0 {
            return;
        }
        let mut queued: Vec<(i32, u64, usize)> = es
            .iter()
            .enumerate()
            .filter(|(_, e)| !e.running())
            .filter_map(|(i, e)| {
                let t = e.task.lock().unwrap();
                (t.status == Status::Queued).then_some((-t.priority, t.id, i))
            })
            .collect();
        queued.sort();
        let client = self.client.lock().unwrap().clone();
        for (_, _, i) in queued.into_iter().take(slots) {
            let e = &mut es[i];
            let cancel = CancellationToken::new();
            let (downloaded, speed_limit) = {
                let mut t = e.task.lock().unwrap();
                t.status = Status::Downloading;
                (t.downloaded(), t.speed_limit)
            };
            e.limiter.set_rate(speed_limit);
            // Measure this run's speed from where it resumes, not from zero.
            e.last_bytes = downloaded;
            e.speed = 0.0;
            let ctx = Ctx {
                client: client.clone(),
                global: self.global.clone(),
                local: e.limiter.clone(),
                retries: settings.retries,
                cancel: cancel.clone(),
                verify: settings.auto_verify,
            };
            let task = e.task.clone();
            let me = self.clone();
            e.cancel = Some(cancel);
            e.handle = Some(self.rt.spawn(async move {
                let r = engine::run(task.clone(), ctx).await;
                {
                    let mut t = task.lock().unwrap();
                    match r {
                        Ok(Outcome::Completed) => {
                            t.status = Status::Completed;
                            t.error = None;
                        }
                        Ok(Outcome::Cancelled) => {
                            if t.status == Status::Downloading {
                                t.status = Status::Paused;
                            }
                        }
                        Err(err) => {
                            t.status = Status::Error;
                            t.error = Some(err);
                        }
                    }
                }
                me.save();
            }));
        }
    }
}

fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::File::create(&tmp)?;
    f.write_all(data)?;
    // Flush to disk before the rename, so a power loss can't leave an empty file behind.
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp, path)
}

/// Read a state file. `Ok(None)` when it doesn't exist. Other errors (a sharing violation or an antivirus lock on
/// Windows is usually brief) are retried `retries` times, `pause` apart, before the error is returned.
fn read_retrying(mut read: impl FnMut() -> std::io::Result<Vec<u8>>, retries: u32, pause: Duration) -> std::io::Result<Option<Vec<u8>>> {
    let mut left = retries;
    loop {
        match read() {
            Ok(bytes) => return Ok(Some(bytes)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) if left == 0 => return Err(e),
            Err(_) => {
                left -= 1;
                std::thread::sleep(pause);
            }
        }
    }
}

/// Load a JSON state file. A missing file means defaults. A file that exists but doesn't parse is
/// moved to `<name>.corrupt` (replacing an older one) so the next save can't silently overwrite it.
/// A file that can't be read at all, or a corrupt one that can't be moved aside, returns `false` as the
/// second value: the caller must not write it, or the list would be replaced by defaults.
fn load_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> (T, bool) {
    let bytes = match read_retrying(|| std::fs::read(path), 5, Duration::from_millis(100)) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return (T::default(), true),
        Err(e) => {
            eprintln!("{} cannot be read ({e}); starting empty and not overwriting it this session", path.display());
            return (T::default(), false);
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(v) => (v, true),
        Err(e) => {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            let bad = path.with_file_name(format!("{name}.corrupt"));
            match std::fs::rename(path, &bad) {
                Ok(()) => {
                    eprintln!("{} is unreadable ({e}); moved it to {}", path.display(), bad.display());
                    (T::default(), true)
                }
                Err(re) => {
                    eprintln!("{} is unreadable ({e}) and could not be moved aside ({re}); not overwriting it this session", path.display());
                    (T::default(), false)
                }
            }
        }
    }
}

/// A cookie can be sent as a header value and is within a sane size. Anything else would make every request fail to build.
fn usable_cookie(c: &str) -> bool {
    reqwest::header::HeaderValue::from_str(c).is_ok() && c.len() <= 8192
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Error, ErrorKind};

    #[test]
    fn usable_cookie_rules() {
        assert!(usable_cookie("sid=abc; uid=42"));
        // http 1.x accepts UTF-8 bytes in header values, so such a cookie is kept.
        assert!(usable_cookie("name=中文"));
        assert!(!usable_cookie("sid=a\nSet-Cookie: x=y"), "CR/LF must never reach a header");
        assert!(!usable_cookie("sid=a\u{1}b"), "control characters are not header bytes");
        assert!(usable_cookie(&format!("k={}", "a".repeat(8190))), "8192 bytes is the limit");
        assert!(!usable_cookie(&format!("k={}", "a".repeat(8191))));
    }

    #[test]
    fn state_read_retries_then_gives_up() {
        let mut calls = 0;
        let r = read_retrying(
            || {
                calls += 1;
                Err(Error::new(ErrorKind::PermissionDenied, "locked"))
            },
            5,
            Duration::ZERO,
        );
        assert!(r.is_err());
        assert_eq!(calls, 6, "one try plus five retries");

        let mut calls = 0;
        let r = read_retrying(
            || {
                calls += 1;
                if calls < 3 { Err(Error::new(ErrorKind::PermissionDenied, "locked")) } else { Ok(b"[]".to_vec()) }
            },
            5,
            Duration::ZERO,
        );
        assert_eq!(r.unwrap(), Some(b"[]".to_vec()));
        assert_eq!(calls, 3);

        let mut calls = 0;
        let r = read_retrying(
            || {
                calls += 1;
                Err(Error::new(ErrorKind::NotFound, "gone"))
            },
            5,
            Duration::ZERO,
        );
        assert_eq!(r.unwrap(), None);
        assert_eq!(calls, 1, "a missing file is not retried");
    }
}
