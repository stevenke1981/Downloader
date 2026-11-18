//! Add-on module system: built-in protocol modules and approved external modules.
use crate::task::Task;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

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
#[derive(Debug, Default)]
pub struct Registry {
    modules: Vec<ModuleInfo>,
}

impl Registry {
    /// Scan `dir` for modules. Never fails: problems are reported in `ModuleInfo::error`.
    pub fn load(_settings: &ModuleSettings, _dir: &Path) -> Arc<Registry> {
        Self::empty()
    }

    pub fn empty() -> Arc<Registry> {
        Arc::new(Registry::default())
    }

    pub fn list(&self) -> Vec<ModuleInfo> {
        self.modules.clone()
    }

    /// Let a matching resolver module rewrite the task before it is downloaded.
    pub async fn prepare(
        &self,
        _task: &Arc<Mutex<Task>>,
        _cancel: &CancellationToken,
    ) -> Result<(), String> {
        Ok(())
    }

    /// Deliver `event` to hook modules in the background (bounded).
    pub fn notify(self: &Arc<Self>, _rt: &tokio::runtime::Handle, _event: HookEvent) {}
}

/// `<state dir>/modules`.
pub fn modules_dir() -> PathBuf {
    crate::ipc::default_state_dir().join("modules")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_validation() {
        let mut s = ModuleSettings::default();
        assert!(s.validate().is_ok());
        s.approved.insert("example-resolver".into(), "a".repeat(64));
        assert!(s.validate().is_ok());
        s.approved.insert("Bad".into(), "a".repeat(64));
        assert!(s.validate().is_err());
        s.approved.remove("Bad");
        s.approved.insert("x".repeat(41), "a".repeat(64));
        assert!(s.validate().is_err());
        s.approved.remove(&"x".repeat(41));
        s.approved.insert("ok".into(), "A".repeat(64));
        assert!(s.validate().is_err());
        s.approved.insert("ok".into(), "a".repeat(63));
        assert!(s.validate().is_err());
    }

    #[test]
    fn empty_registry() {
        let r = Registry::load(&ModuleSettings::default(), Path::new("does-not-exist"));
        assert!(r.list().is_empty());
        assert!(Registry::empty().list().is_empty());
        assert!(modules_dir().ends_with("modules"));
    }
}
