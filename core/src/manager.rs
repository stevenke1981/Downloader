//! Download queue: concurrency limit, priorities, scheduler, persistence.
use crate::category::Category;
use crate::checksum::Checksum;
use crate::engine::{self, Ctx, Outcome};
use crate::limiter::RateLimiter;
use crate::settings::Settings;
use crate::task::{Status, Task};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
}

pub struct Manager {
    inner: Arc<Inner>,
    rt: tokio::runtime::Runtime,
}

impl Manager {
    /// `dir` holds settings.json and downloads.json.
    pub fn new(dir: &Path) -> Manager {
        let rt = tokio::runtime::Builder::new_multi_thread().enable_all().worker_threads(4).build().expect("tokio runtime");
        let settings: Settings = std::fs::read(dir.join("settings.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let mut tasks: Vec<Task> = std::fs::read(dir.join("downloads.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
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
                task: Arc::new(Mutex::new(t)),
                cancel: None,
                handle: None,
                speed: 0.0,
                last_bytes: 0,
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
        *self.inner.client.lock().unwrap() = client;
        self.inner.global.set_rate(s.global_limit);
        let _ = std::fs::create_dir_all(&self.inner.dir);
        let json = serde_json::to_vec_pretty(&s).unwrap();
        *self.inner.settings.lock().unwrap() = s;
        write_atomic(&self.inner.dir.join("settings.json"), &json).map_err(|e| e.to_string())
    }

    pub fn add(&self, d: NewDownload) -> u64 {
        let s = self.settings();
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let name = d.file_name.clone().filter(|n| !n.trim().is_empty()).unwrap_or_else(|| crate::util::name_from_url(&d.url));
        let cat = Category::from_name(&name);
        let limit = d.speed_limit.unwrap_or(s.per_download_limit);
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
            ..Default::default()
        };
        self.inner.entries.lock().unwrap().push(Entry {
            task: Arc::new(Mutex::new(t)),
            limiter: Arc::new(RateLimiter::new(limit)),
            cancel: None,
            handle: None,
            speed: 0.0,
            last_bytes: 0,
        });
        self.inner.save();
        self.inner.tick(0.5);
        id
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
            }
        });
        self.inner.tick(0.5);
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
        self.inner.save();
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
        self.inner.save();
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
                }
            })
            .collect()
    }

    pub fn save(&self) {
        self.inner.save();
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
        let _ = std::fs::create_dir_all(&self.dir);
        if let Ok(json) = serde_json::to_vec(&tasks) {
            let _ = write_atomic(&self.dir.join("downloads.json"), &json);
        }
    }

    fn tick(self: &Arc<Self>, dt: f64) {
        let settings = self.settings.lock().unwrap().clone();
        let allowed = settings.schedule.allows_now();
        let mut es = self.entries.lock().unwrap();
        // speeds + reap finished jobs
        for e in es.iter_mut() {
            let b = e.task.lock().unwrap().downloaded();
            let inst = b.saturating_sub(e.last_bytes) as f64 / dt;
            e.speed = if e.speed == 0.0 { inst } else { e.speed * 0.6 + inst * 0.4 };
            e.last_bytes = b;
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
            e.task.lock().unwrap().status = Status::Downloading;
            e.limiter.set_rate(e.task.lock().unwrap().speed_limit);
            let ctx = Ctx {
                client: client.clone(),
                global: self.global.clone(),
                local: e.limiter.clone(),
                retries: settings.retries,
                cancel: cancel.clone(),
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
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    std::fs::rename(&tmp, path)
}
