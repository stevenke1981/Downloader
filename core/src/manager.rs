//! Download queue: concurrency limit, priorities, scheduler, persistence.
mod automation;
mod models;
mod persistence;
mod scheduler;

use persistence::{load_json, write_atomic};
use crate::automation::Action;
use crate::category::Category;
use crate::checksum::Checksum;
use crate::engine::{self, Ctx, Outcome};
use crate::limiter::RateLimiter;
use crate::settings::Settings;
use crate::task::{DownloadKind, Status, Task};
use std::collections::HashMap;
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
    protocol_client: Mutex<reqwest::Client>,
    global: Arc<RateLimiter>,
    dir: PathBuf,
    next_id: AtomicU64,
    rt: tokio::runtime::Handle,
    /// Bytes of the last downloads.json written, so unchanged autosaves are skipped.
    last_saved: Mutex<Vec<u8>>,
    /// Set when downloads.json existed but could not be read: saves go to downloads.json.recovered so the
    /// user's list is never replaced with this session's.
    save_disabled: AtomicBool,
    credentials: Mutex<HashMap<String, String>>,
    automation_log: Mutex<Vec<String>>,
}

pub use models::{NewDownload, Snapshot};

struct RuntimeOwner(Option<tokio::runtime::Runtime>);

impl std::ops::Deref for RuntimeOwner {
    type Target = tokio::runtime::Runtime;
    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("manager runtime")
    }
}

impl Drop for RuntimeOwner {
    fn drop(&mut self) {
        // A remote-control clone can be the last owner to leave an async task.
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

#[derive(Clone)]
pub struct Manager {
    inner: Arc<Inner>,
    rt: Arc<RuntimeOwner>,
}

impl Manager {
    /// `dir` holds settings.json and downloads.json.
    pub fn new(dir: &Path) -> Manager {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .worker_threads(4)
            .build()
            .expect("tokio runtime");
        let (settings, _) = load_json::<Settings>(&dir.join("settings.json"));
        let (mut tasks, tasks_readable) = load_json::<Vec<Task>>(&dir.join("downloads.json"));
        for t in &mut tasks {
            if matches!(t.status, Status::Downloading | Status::Verifying) {
                t.status = Status::Queued;
            }
        }
        let next = tasks.iter().map(|t| t.id).max().unwrap_or(0) + 1;
        let client = settings
            .build_client()
            .or_else(|_| Settings::default().build_client())
            .expect("http client");
        let global = Arc::new(RateLimiter::new(settings.effective_global_limit()));
        let protocol_client = settings.build_protocol_client()
            .or_else(|_| Settings::default().build_protocol_client())
            .expect("native protocol client");
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
            protocol_client: Mutex::new(protocol_client),
            global,
            dir: dir.to_path_buf(),
            next_id: AtomicU64::new(next),
            rt: rt.handle().clone(),
            last_saved: Mutex::new(Vec::new()),
            save_disabled: AtomicBool::new(!tasks_readable),
            credentials: Mutex::new(HashMap::new()),
            automation_log: Mutex::new(Vec::new()),
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
        Manager {
            inner,
            rt: Arc::new(RuntimeOwner(Some(rt))),
        }
    }

    pub fn settings(&self) -> Settings {
        self.inner.settings.lock().unwrap().clone()
    }

    pub fn set_settings(&self, mut s: Settings) -> Result<(), String> {
        s.validate()?;
        for rule in &mut s.site_rules {
            rule.host = crate::site::origin(&rule.host)?;
        }
        let client = s.build_client()?;
        let protocol_client = s.build_protocol_client()?;
        std::fs::create_dir_all(&self.inner.dir).map_err(|e| e.to_string())?;
        {
            // Serialize settings changes with dispatch. A timer tick must not
            // start queued jobs using a limit captured before this update.
            let _dispatch = self.inner.entries.lock().unwrap();
            let mut current = self.inner.settings.lock().unwrap();
            // Applying an old open settings form must not re-arm a consumed one-shot.
            for action in &mut s.scheduled_actions {
                if current
                    .scheduled_actions
                    .iter()
                    .any(|old| old.id == action.id && old.consumed)
                {
                    action.consumed = true;
                }
            }
            let json = serde_json::to_vec_pretty(&s).map_err(|e| e.to_string())?;
            write_atomic(&self.inner.dir.join("settings.json"), &json)
                .map_err(|e| e.to_string())?;
            *self.inner.client.lock().unwrap() = client;
            *self.inner.protocol_client.lock().unwrap() = protocol_client;
            self.inner.global.set_rate(s.effective_global_limit());
            *current = s;
        }
        self.inner.dispatch();
        Ok(())
    }

    pub fn add(&self, d: NewDownload) -> u64 {
        self.try_add(d).unwrap_or_else(|error| {
            eprintln!("download was not added: {error}");
            0
        })
    }

    pub fn try_add(&self, d: NewDownload) -> Result<u64, String> {
        crate::util::validate_source(d.url.trim())?;
        if d.connections
            .is_some_and(|count| !(1..=32).contains(&count))
        {
            return Err("connections must be 1–32".into());
        }
        let s = self.settings();
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        let name = d
            .file_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .unwrap_or_else(|| crate::util::name_from_url(&d.url));
        let cat = Category::from_name(&name);
        let limit = d.speed_limit.unwrap_or(s.per_download_limit);
        let cookie = match d.cookie.filter(|c| !c.trim().is_empty()) {
            Some(c) if usable_cookie(&c) => Some(c),
            Some(_) => {
                // Never print the value: it is a session secret.
                eprintln!(
                    "a cookie was dropped: it is not a valid HTTP header value or is longer than 8 KiB"
                );
                None
            }
            None => None,
        };
        let had_cookie = cookie.is_some();
        let origin = crate::site::url_origin(d.url.trim());
        let rule = origin
            .as_ref()
            .and_then(|origin| s.site_rules.iter().find(|r| &r.host == origin));
        let authorization = origin
            .as_ref()
            .and_then(|origin| self.inner.credentials.lock().unwrap().get(origin).cloned());
        let t = Task {
            id,
            url: d.url.trim().to_string(),
            kind: d.kind,
            dir: d.dir.unwrap_or_else(|| s.folder_for_name(&name, cat)),
            connections: d
                .connections
                .unwrap_or_else(|| rule.map_or(s.max_connections, |r| r.connections))
                .clamp(1, 32),
            authorization,
            speed_limit: limit,
            checksum: d.checksum,
            priority: d.priority,
            category: cat,
            status: if d.start {
                Status::Queued
            } else {
                Status::Paused
            },
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
        Ok(id)
    }

    pub fn set_traffic_mode(&self, mode: crate::settings::TrafficMode) -> Result<(), String> {
        let mut settings = self.settings();
        settings.traffic_mode = mode;
        self.set_settings(settings)
    }

    pub fn global_limit(&self) -> u64 {
        self.inner.global.rate()
    }

    /// Bare host names mean HTTPS; use a complete origin for HTTP or non-default ports.
    pub fn set_site_credentials(
        &self,
        host: &str,
        username: &str,
        password: &str,
    ) -> Result<(), String> {
        use base64::Engine;
        let origin = crate::site::origin(host)?;
        if username.contains(':')
            || username
                .chars()
                .chain(password.chars())
                .any(char::is_control)
        {
            return Err("invalid Basic-auth username or password".into());
        }
        let authorization = if username.is_empty() && password.is_empty() {
            None
        } else {
            Some(format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
            ))
        };
        {
            let mut credentials = self.inner.credentials.lock().unwrap();
            if let Some(value) = &authorization {
                credentials.insert(origin.clone(), value.clone());
            } else {
                credentials.remove(&origin);
            }
        }
        for entry in self.inner.entries.lock().unwrap().iter_mut() {
            let mut task = entry.task.lock().unwrap();
            if crate::site::url_origin(&task.url).as_ref() == Some(&origin) {
                task.authorization = authorization.clone();
            }
        }
        if !username.is_empty() {
            let mut settings = self.settings();
            if let Some(rule) = settings
                .site_rules
                .iter_mut()
                .find(|rule| rule.host == origin)
            {
                rule.username = username.into();
            } else {
                settings.site_rules.push(crate::site::SiteRule {
                    host: origin,
                    username: username.into(),
                    ..Default::default()
                });
            }
            self.set_settings(settings)?;
        }
        Ok(())
    }

    pub fn automation_log(&self) -> Vec<String> {
        self.inner.automation_log.lock().unwrap().clone()
    }

    pub fn client(&self) -> reqwest::Client {
        self.inner.client.lock().unwrap().clone()
    }

    pub fn crawl_client(&self) -> Result<crate::grabber::CrawlClient, String> {
        self.settings().build_crawl_client()
    }

    /// Persist on the runtime, not the caller's thread: saving fsyncs, which can stall the GUI on a slow disk.
    /// `save_with` holds its lock across snapshot and write, so the last queued save always writes the newest state.
    fn save_async(&self) {
        let me = self.inner.clone();
        self.rt.spawn_blocking(move || me.save());
    }

    fn with<R>(&self, id: u64, f: impl FnOnce(&mut Entry) -> R) -> Option<R> {
        let mut es = self.inner.entries.lock().unwrap();
        es.iter_mut()
            .find(|e| e.task.lock().unwrap().id == id)
            .map(f)
    }

    /// Queue a paused/failed download (it starts when a slot is free).
    pub fn start(&self, id: u64) {
        self.with(id, |e| {
            let mut t = e.task.lock().unwrap();
            if matches!(t.status, Status::Paused | Status::Error) {
                t.status = Status::Queued;
                t.error = None;
                t.verify_retried = false;
                t.append_log("Queued by user");
            }
        });
        self.inner.dispatch();
    }

    pub fn pause(&self, id: u64) {
        self.with(id, |e| {
            let mut t = e.task.lock().unwrap();
            if matches!(t.status, Status::Queued | Status::Downloading) {
                t.status = Status::Paused;
                t.append_log("Paused by user");
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
            let Some(i) = es.iter().position(|e| e.task.lock().unwrap().id == id) else {
                return;
            };
            es.remove(i)
        };
        if let Some(c) = &entry.cancel {
            c.cancel();
        }
        let task = entry.task;
        let handle = entry.handle;
        self.rt.spawn(async move {
            if let Some(h) = handle {
                let _ = h.await;
            }
            // Publication can finish while cancellation is being handled; inspect the
            // final task path only once the worker (and its child process) has exited.
            let t = task.lock().unwrap().clone();
            let Ok(root) = std::fs::canonicalize(&t.dir) else {
                return;
            };
            let owned_path = |path: PathBuf| {
                std::fs::canonicalize(path)
                    .ok()
                    .filter(|path| path != &root && path.starts_with(&root))
            };
            let external = crate::protocols::handles(&t.url, t.kind);
            if !t.file_name.is_empty() {
                if let Some(path) = owned_path(t.part_path()) {
                    let _ = std::fs::remove_file(path);
                }
                if delete_file && let Some(path) = owned_path(t.final_path()) {
                    if external && path.is_dir() {
                        let _ = std::fs::remove_dir_all(path);
                    } else {
                        let _ = std::fs::remove_file(path);
                    }
                }
            }
            if delete_file
                && external
                && let Some(path) = owned_path(crate::protocols::staging_path(&t))
            {
                if path.is_dir() {
                    let _ = std::fs::remove_dir_all(path);
                } else {
                    let _ = std::fs::remove_file(path);
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
                    path: if t.file_name.is_empty() {
                        t.dir.clone()
                    } else {
                        t.final_path()
                    },
                    total: t.total,
                    downloaded,
                    speed,
                    eta,
                    status: t.status,
                    category: t.category,
                    error: t.error.clone(),
                    segments: t.segments.iter().map(|s| (s.start, s.pos, s.end)).collect(),
                    connections: t.connections,
                    active_connections: if running {
                        t.segments.iter().filter(|s| s.busy).count() as u32
                    } else {
                        0
                    },
                    speed_limit: t.speed_limit,
                    priority: t.priority,
                    resumable: t.resumable,
                    verification: t.verification.clone(),
                    verify_progress: match t.total {
                        Some(n) if n > 0 => (t.verify_done.load(Ordering::Relaxed) as f64
                            / n as f64)
                            .min(1.0) as f32,
                        _ => 0.0,
                    },
                    kind: t.kind,
                    log: t.log.clone(),
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
        let handles: Vec<_> = self
            .inner
            .entries
            .lock()
            .unwrap()
            .iter_mut()
            .filter_map(|e| e.handle.take())
            .collect();
        self.rt.block_on(async {
            for h in handles {
                let _ = tokio::time::timeout(Duration::from_secs(3), h).await;
            }
        });
        // Tasks that were cancelled by shutdown should resume next launch.
        self.inner.save_with(|t| {
            if t.status == Status::Downloading {
                t.status = Status::Queued
            }
        });
        if let Ok(mut owner) = Arc::try_unwrap(self.rt)
            && let Some(runtime) = owner.0.take()
        {
            runtime.shutdown_timeout(Duration::from_secs(1));
        }
    }
}

/// A cookie can be sent as a header value and is within a sane size. Anything else would make every request fail to build.
fn usable_cookie(c: &str) -> bool {
    reqwest::header::HeaderValue::from_str(c).is_ok() && c.len() <= 8192
}

#[cfg(test)]
mod tests;
