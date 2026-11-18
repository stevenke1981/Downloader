//! External module manifests (`<modules dir>/<id>/module.json`).
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

/// Largest accepted module.json.
pub(super) const MAX_MANIFEST_BYTES: u64 = 64 * 1024;
pub(super) const DEFAULT_TIMEOUT_MS: u64 = 10_000;
pub(super) const MAX_TIMEOUT_MS: u64 = 60_000;
const KINDS: [&str; 2] = ["resolver", "hook"];
pub(super) const EVENTS: [&str; 2] = ["completed", "failed"];
const SCHEMES: [&str; 2] = ["http", "https"];

#[derive(Debug, Clone, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub(super) struct Match {
    pub schemes: Vec<String>,
    pub hosts: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(super) struct Manifest {
    pub schema: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub kinds: Vec<String>,
    #[serde(rename = "match", default)]
    pub matcher: Match,
    pub command: Vec<String>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
    #[serde(default)]
    pub events: Vec<String>,
}

/// A manifest that passed every check, with its program resolved to a canonical path.
#[derive(Debug, Clone)]
pub(super) struct Checked {
    pub manifest: Manifest,
    pub program: PathBuf,
    pub dir: PathBuf,
    pub sha256: String,
}

impl Checked {
    pub fn timeout(&self) -> Duration {
        Duration::from_millis(self.manifest.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS))
    }
    pub fn has_kind(&self, kind: &str) -> bool {
        self.manifest.kinds.iter().any(|k| k == kind)
    }
    /// Whether a resolver claims this URL (scheme and host pattern both match).
    pub fn claims(&self, url: &reqwest::Url) -> bool {
        let Some(host) = url.host_str() else {
            return false;
        };
        let host = host.to_ascii_lowercase();
        self.manifest
            .matcher
            .schemes
            .iter()
            .any(|s| s == url.scheme())
            && self
                .manifest
                .matcher
                .hosts
                .iter()
                .any(|p| host_matches(p, &host))
    }
}

/// `example.com` matches exactly; `*.example.org` matches any subdomain, not the apex.
pub(super) fn host_matches(pattern: &str, host: &str) -> bool {
    match pattern.strip_prefix("*.") {
        Some(suffix) => host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}")),
        None => pattern == host,
    }
}

fn valid_host_pattern(p: &str) -> bool {
    let host = p.strip_prefix("*.").unwrap_or(p);
    (1..=253).contains(&host.len())
        && !host.starts_with('.')
        && !host.ends_with('.')
        && !host.contains("..")
        && host
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
}

/// Lowercase hex SHA-256 of the exact manifest bytes.
pub(super) fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Read `<dir>/module.json` (size-capped) and return its bytes.
pub(super) fn read(dir: &Path) -> Result<Vec<u8>, String> {
    let path = dir.join("module.json");
    let meta = std::fs::metadata(&path).map_err(|e| format!("cannot read module.json: {e}"))?;
    if !meta.is_file() {
        return Err("module.json is not a file".into());
    }
    if meta.len() > MAX_MANIFEST_BYTES {
        return Err("module.json is larger than 64 KiB".into());
    }
    std::fs::read(&path).map_err(|e| format!("cannot read module.json: {e}"))
}

fn check_text(field: &str, value: &str, max: usize) -> Result<(), String> {
    if value.trim().is_empty() || value.len() > max || value.chars().any(char::is_control) {
        return Err(format!("{field} must be 1–{max} printable characters"));
    }
    Ok(())
}

/// Parse and validate manifest bytes found in `dir`, whose last component must equal the id.
pub(super) fn check(dir: &Path, bytes: &[u8]) -> Result<Checked, String> {
    let manifest: Manifest =
        serde_json::from_slice(bytes).map_err(|e| format!("invalid module.json: {e}"))?;
    if manifest.schema != 1 {
        return Err(format!(
            "unsupported schema {}; expected 1",
            manifest.schema
        ));
    }
    if !super::valid_id(&manifest.id) {
        return Err("id must be 1–40 characters a-z, 0-9 or -".into());
    }
    let folder = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
    if folder != manifest.id {
        return Err(format!(
            "id {:?} does not match its folder {folder:?}",
            manifest.id
        ));
    }
    check_text("name", &manifest.name, 80)?;
    check_text("version", &manifest.version, 40)?;
    if manifest.kinds.is_empty() {
        return Err("kinds must list \"resolver\" and/or \"hook\"".into());
    }
    for kind in &manifest.kinds {
        if !KINDS.contains(&kind.as_str()) {
            return Err(format!(
                "unknown kind {kind:?}; use \"resolver\" or \"hook\""
            ));
        }
    }
    let has = |k: &str| manifest.kinds.iter().any(|x| x == k);
    for scheme in &manifest.matcher.schemes {
        if !SCHEMES.contains(&scheme.as_str()) {
            return Err(format!(
                "match.schemes: {scheme:?} is not \"http\" or \"https\""
            ));
        }
    }
    if manifest.matcher.hosts.len() > 64 {
        return Err("match.hosts: at most 64 patterns".into());
    }
    for host in &manifest.matcher.hosts {
        if !valid_host_pattern(host) {
            return Err(format!(
                "match.hosts: {host:?} must be a lowercase host name, optionally starting with *."
            ));
        }
    }
    if has("resolver") && (manifest.matcher.schemes.is_empty() || manifest.matcher.hosts.is_empty())
    {
        return Err("a resolver needs match.schemes and match.hosts".into());
    }
    for event in &manifest.events {
        if !EVENTS.contains(&event.as_str()) {
            return Err(format!(
                "unknown event {event:?}; use \"completed\" or \"failed\""
            ));
        }
    }
    if has("hook") && manifest.events.is_empty() {
        return Err("a hook needs events".into());
    }
    if let Some(ms) = manifest.timeout_ms
        && !(1..=MAX_TIMEOUT_MS).contains(&ms)
    {
        return Err(format!("timeout_ms must be 1–{MAX_TIMEOUT_MS}"));
    }
    let program = check_command(dir, &manifest.command)?;
    Ok(Checked {
        manifest,
        program,
        dir: dir.to_path_buf(),
        sha256: sha256_hex(bytes),
    })
}

/// `command[0]` is a bare file name inside the module folder or an absolute path.
/// Returns the canonical program path.
fn check_command(dir: &Path, command: &[String]) -> Result<PathBuf, String> {
    let Some(first) = command.first() else {
        return Err("command must not be empty".into());
    };
    if command.len() > 32 {
        return Err("command: at most 32 arguments".into());
    }
    for arg in command {
        if arg.len() > 1024 || arg.contains('\0') {
            return Err("command: arguments must be under 1 KiB and contain no NUL".into());
        }
    }
    let path = Path::new(first);
    if first.is_empty() || path.components().any(|c| c == Component::ParentDir) {
        return Err("command[0] must not contain \"..\"".into());
    }
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let bare = path.components().count() == 1
            && matches!(path.components().next(), Some(Component::Normal(_)))
            && !first.contains(['/', '\\', ':']);
        if !bare {
            return Err(
                "command[0] must be a file name inside the module folder or an absolute path"
                    .into(),
            );
        }
        let inside = dir.join(path);
        if !inside.is_file() && cfg!(windows) && path.extension().is_none() {
            dir.join(format!("{first}.exe"))
        } else {
            inside
        }
    };
    let program = std::fs::canonicalize(&candidate)
        .map_err(|e| format!("command[0] {first:?} not found: {e}"))?;
    if !program.is_file() {
        return Err(format!("command[0] {first:?} is not a file"));
    }
    if !path.is_absolute() {
        let root = std::fs::canonicalize(dir).map_err(|e| e.to_string())?;
        if !program.starts_with(&root) {
            return Err(format!(
                "command[0] {first:?} points outside the module folder"
            ));
        }
    }
    Ok(program)
}
