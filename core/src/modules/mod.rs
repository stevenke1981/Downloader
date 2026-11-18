//! Add-on module system: built-in protocol modules and approved external modules.
mod external;
mod manifest;
#[cfg(test)]
mod tests;

use crate::task::{DownloadKind, Task};
use manifest::Checked;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;

/// Module folders scanned at most.
const MAX_MODULES: usize = 64;
/// Hook deliveries waiting or running at once; later events are dropped.
const MAX_PENDING_HOOKS: usize = 32;
/// Mirrors a resolver may add.
const MAX_MIRRORS: usize = 16;

/// User choices about external modules.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct ModuleSettings {
    /// External modules run only when this is on.
    pub enabled: bool,
    /// Module id -> lowercase hex SHA-256 of the approved module.json.
    pub approved: BTreeMap<String, String>,
}

/// Module ids are 1–40 characters of `a-z`, `0-9` and `-`.
pub fn valid_id(id: &str) -> bool {
    (1..=40).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn valid_sha256(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

impl ModuleSettings {
    pub fn validate(&self) -> Result<(), String> {
        for (id, hash) in &self.approved {
            if !valid_id(id) {
                return Err(format!(
                    "invalid module id {id:?}: use 1–40 characters a-z, 0-9 or -"
                ));
            }
            if !valid_sha256(hash) {
                return Err(format!(
                    "invalid approval for module {id}: expected 64 lowercase hex characters"
                ));
            }
        }
        Ok(())
    }
}

/// What the UI and CLIs show about one module.
#[derive(Debug, Clone, Serialize)]
pub struct ModuleInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub builtin: bool,
    pub kinds: Vec<String>,
    pub approved: bool,
    pub error: Option<String>,
    pub manifest_sha256: String,
}

/// A download lifecycle event delivered to hook modules.
#[derive(Debug, Clone, Serialize)]
pub struct HookEvent {
    /// "completed" or "failed".
    pub event: &'static str,
    pub id: u64,
    pub url: String,
    pub path: PathBuf,
    pub size: Option<u64>,
    pub error: Option<String>,
}

/// Loaded modules. Cheap to share; reload by building a new one.
#[derive(Debug)]
pub struct Registry {
    modules: Vec<ModuleInfo>,
    /// Valid, approved external modules; empty unless modules are enabled.
    runnable: Vec<Checked>,
    permits: Arc<Semaphore>,
    hooks_pending: Arc<AtomicUsize>,
}

impl Default for Registry {
    fn default() -> Self {
        Registry {
            modules: builtin_infos(),
            runnable: Vec::new(),
            permits: Arc::new(Semaphore::new(external::MAX_CONCURRENT)),
            hooks_pending: Arc::new(AtomicUsize::new(0)),
        }
    }
}

fn builtin_infos() -> Vec<ModuleInfo> {
    crate::protocols::registry::builtin()
        .iter()
        .map(|m| ModuleInfo {
            id: m.id().into(),
            name: m.name().into(),
            version: env!("CARGO_PKG_VERSION").into(),
            builtin: true,
            kinds: vec!["protocol".into()],
            approved: true,
            error: None,
            manifest_sha256: String::new(),
        })
        .collect()
}

/// Hash of `<dir>/<id>/module.json` for approval. Fails when the manifest is missing or invalid,
/// so only a module that could run can be approved.
pub fn manifest_hash(dir: &Path, id: &str) -> Result<String, String> {
    if !valid_id(id) {
        return Err(format!("module {id}: invalid id"));
    }
    let folder = dir.join(id);
    let bytes = manifest::read(&folder).map_err(|e| format!("module {id}: {e}"))?;
    let checked = manifest::check(&folder, &bytes).map_err(|e| format!("module {id}: {e}"))?;
    Ok(checked.sha256)
}

const CHANGED: &str = "module.json changed since it was approved; approve it again";

/// A resolver's answer.
#[derive(Debug, Deserialize)]
struct Response {
    ok: bool,
    #[serde(default)]
    skip: bool,
    url: Option<String>,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    filename: Option<String>,
    #[serde(default)]
    mirrors: Vec<String>,
    error: Option<String>,
}

/// A resolver output URL must be plain HTTP(S) that the HTTP engine (not a built-in protocol) takes.
fn check_output_url(url: &str) -> Result<(), String> {
    let parsed =
        reqwest::Url::parse(url).map_err(|_| format!("returned an invalid URL {url:?}"))?;
    if !matches!(parsed.scheme(), "http" | "https") {
        return Err(format!(
            "returned a {} URL; only http(s) is allowed",
            parsed.scheme()
        ));
    }
    crate::util::validate_source(url).map_err(|e| format!("returned an unusable URL: {e}"))?;
    if crate::protocols::handles(url, DownloadKind::Auto) {
        return Err("returned a URL that needs a protocol module".into());
    }
    Ok(())
}

/// Module-provided text shown to users: single line, bounded.
fn clean(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(300)
        .collect::<String>()
        .trim()
        .to_string()
}

impl Registry {
    /// Scan `dir` for modules. Never fails: problems are reported in `ModuleInfo::error`.
    pub fn load(settings: &ModuleSettings, dir: &Path) -> Arc<Registry> {
        let mut registry = Registry::default();
        let mut folders: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
            .map(|e| e.path())
            .collect();
        folders.sort();
        folders.truncate(MAX_MODULES);
        for folder in folders {
            let id = folder
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            let bytes = manifest::read(&folder);
            let sha = bytes
                .as_deref()
                .map(manifest::sha256_hex)
                .unwrap_or_default();
            let checked = bytes.and_then(|b| manifest::check(&folder, &b));
            let stored = settings.approved.get(&id);
            let approved = !sha.is_empty() && stored == Some(&sha);
            let error = match &checked {
                Err(e) => Some(e.clone()),
                Ok(_) if stored.is_some() && !approved => Some(CHANGED.into()),
                Ok(_) => None,
            };
            let (name, version, kinds) = match &checked {
                Ok(c) => (
                    c.manifest.name.clone(),
                    c.manifest.version.clone(),
                    c.manifest.kinds.clone(),
                ),
                Err(_) => (id.clone(), String::new(), Vec::new()),
            };
            registry.modules.push(ModuleInfo {
                id,
                name,
                version,
                builtin: false,
                kinds,
                approved,
                error,
                manifest_sha256: sha,
            });
            if settings.enabled
                && approved
                && let Ok(checked) = checked
            {
                registry.runnable.push(checked);
            }
        }
        Arc::new(registry)
    }

    pub fn empty() -> Arc<Registry> {
        Arc::new(Registry::default())
    }

    /// Built-in protocol modules first, then external modules by folder name.
    pub fn list(&self) -> Vec<ModuleInfo> {
        self.modules.clone()
    }

    /// Let a matching resolver module rewrite the task before it is downloaded.
    /// A no-op when a built-in protocol takes the task or no enabled, approved resolver claims
    /// it. Returns `Ok` without changes when `cancel` fires; the caller checks `cancel`.
    pub async fn prepare(
        &self,
        task: &Arc<Mutex<Task>>,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        if self.runnable.is_empty() {
            return Ok(());
        }
        let (url, kind, referer) = {
            let t = task.lock().unwrap();
            (t.url.trim().to_string(), t.kind, t.referer.clone())
        };
        if crate::protocols::handles(&url, kind) {
            return Ok(());
        }
        let Ok(parsed) = reqwest::Url::parse(&url) else {
            return Ok(());
        };
        let Some(module) = self
            .runnable
            .iter()
            .find(|m| m.has_kind("resolver") && m.claims(&parsed))
        else {
            return Ok(());
        };
        let id = &module.manifest.id;
        let fail = |e: String| format!("module {id}: {e}");
        let request = serde_json::json!({"v": 1, "op": "resolve", "url": url, "referer": referer});
        let Some(line) = external::call(module, &request.to_string(), &self.permits, Some(cancel))
            .await
            .map_err(fail)?
        else {
            return Ok(());
        };
        let response: Response = serde_json::from_str(&line)
            .map_err(|_| fail("answered with something that is not a JSON response".into()))?;
        if !response.ok {
            let why = response.error.as_deref().map(clean).unwrap_or_default();
            return Err(fail(if why.is_empty() { "failed".into() } else { why }));
        }
        if response.skip {
            task.lock()
                .unwrap()
                .append_log(format!("Module {id} left the link unchanged"));
            return Ok(());
        }
        let resolved = response
            .url
            .map(|u| u.trim().to_string())
            .ok_or_else(|| fail("answered ok without a url".into()))?;
        check_output_url(&resolved).map_err(fail)?;
        let added: Vec<(String, String)> = response.headers.into_iter().collect();
        crate::task::validate_headers(&added).map_err(fail)?;
        if response.mirrors.len() > MAX_MIRRORS {
            return Err(fail(format!("returned more than {MAX_MIRRORS} mirrors")));
        }
        for mirror in &response.mirrors {
            check_output_url(mirror.trim()).map_err(fail)?;
        }
        let filename = response
            .filename
            .map(|f| crate::util::sanitize(&f))
            .filter(|f| !f.is_empty());
        let mut t = task.lock().unwrap();
        let mut headers = t.headers.clone();
        for (name, value) in added {
            headers.retain(|(n, _)| !n.eq_ignore_ascii_case(&name));
            headers.push((name, value));
        }
        crate::task::validate_headers(&headers).map_err(fail)?;
        t.headers = headers;
        for mirror in response.mirrors {
            let mirror = mirror.trim().to_string();
            if !t.mirrors.contains(&mirror) {
                t.mirrors.push(mirror);
            }
        }
        t.resolved_url = Some(resolved);
        t.module = Some(id.clone());
        if t.suggested_name.is_none() {
            t.suggested_name = filename;
        }
        t.append_log(format!(
            "Link resolved by module {id} {}",
            module.manifest.version
        ));
        Ok(())
    }

    /// Deliver `event` to hook modules in the background (bounded).
    pub fn notify(self: &Arc<Self>, rt: &tokio::runtime::Handle, event: HookEvent) {
        let targets: Vec<&Checked> = self
            .runnable
            .iter()
            .filter(|m| m.has_kind("hook") && m.manifest.events.iter().any(|e| e == event.event))
            .collect();
        if targets.is_empty() {
            return;
        }
        let mut message = serde_json::to_value(&event).unwrap_or_default();
        if let Some(object) = message.as_object_mut() {
            object.insert("v".into(), 1.into());
            object.insert("op".into(), "event".into());
        }
        let line = message.to_string();
        for module in targets {
            if self.hooks_pending.fetch_add(1, Ordering::SeqCst) >= MAX_PENDING_HOOKS {
                self.hooks_pending.fetch_sub(1, Ordering::SeqCst);
                continue;
            }
            let module = module.clone();
            let line = line.clone();
            let permits = self.permits.clone();
            let pending = self.hooks_pending.clone();
            rt.spawn(async move {
                // Hook output and failures are ignored by design.
                let _ = external::call(&module, &line, &permits, None).await;
                pending.fetch_sub(1, Ordering::SeqCst);
            });
        }
    }
}

/// `<state dir>/modules`.
pub fn modules_dir() -> PathBuf {
    crate::ipc::default_state_dir().join("modules")
}
