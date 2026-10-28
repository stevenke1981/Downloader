use crate::category::Category;
use crate::{automation::ScheduledAction, remote::RemoteSettings, site::SiteRule};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub enum TrafficMode {
    #[default]
    Custom,
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct FolderRule {
    pub extensions: Vec<String>,
    pub folder: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Schedule {
    pub enabled: bool,
    /// "HH:MM" local time; downloads only run inside [start, stop). Wraps past midnight.
    pub start: String,
    pub stop: String,
}

impl Default for Schedule {
    fn default() -> Self {
        Schedule {
            enabled: false,
            start: "02:00".into(),
            stop: "07:00".into(),
        }
    }
}

fn hm(s: &str) -> Option<u32> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m): (u32, u32) = (h.parse().ok()?, m.parse().ok()?);
    (h < 24 && m < 60).then_some(h * 60 + m)
}

impl Schedule {
    pub fn valid(&self) -> bool {
        hm(&self.start).is_some() && hm(&self.stop).is_some()
    }
    /// Whether downloads may run at `minute_of_day`.
    pub fn allows(&self, minute_of_day: u32) -> bool {
        if !self.enabled {
            return true;
        }
        let (Some(a), Some(b)) = (hm(&self.start), hm(&self.stop)) else {
            return true;
        };
        if a == b {
            true
        } else if a < b {
            minute_of_day >= a && minute_of_day < b
        } else {
            minute_of_day >= a || minute_of_day < b
        }
    }
    pub fn allows_now(&self) -> bool {
        use chrono::Timelike;
        let now = chrono::Local::now();
        self.allows(now.hour() * 60 + now.minute())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// UI language code ("" = follow the system).
    pub language: String,
    pub download_dir: PathBuf,
    /// Save into per-category sub-folders (Video, Music, ...).
    pub category_folders: bool,
    pub max_concurrent: u32,
    pub max_connections: u32,
    /// Bytes per second, 0 = unlimited.
    pub global_limit: u64,
    /// Default per-download limit applied to new downloads (bytes/s, 0 = unlimited).
    pub per_download_limit: u64,
    pub retries: u32,
    /// e.g. "http://127.0.0.1:8080" or "socks5://host:1080". Empty = system/env proxy.
    pub proxy: String,
    pub schedule: Schedule,
    pub watch_clipboard: bool,
    /// Look for checksum files / digest headers and verify finished downloads.
    pub auto_verify: bool,
    pub traffic_mode: TrafficMode,
    /// Low / Medium / High, bytes per second; zero means unlimited.
    pub traffic_presets: [u64; 3],
    pub site_rules: Vec<SiteRule>,
    pub folder_rules: Vec<FolderRule>,
    pub scheduled_actions: Vec<ScheduledAction>,
    pub remote: RemoteSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            language: String::new(),
            download_dir: crate::util::default_download_dir(),
            category_folders: false,
            max_concurrent: 3,
            max_connections: 8,
            global_limit: 0,
            per_download_limit: 0,
            retries: 5,
            proxy: String::new(),
            schedule: Schedule::default(),
            watch_clipboard: false,
            auto_verify: true,
            traffic_mode: TrafficMode::Custom,
            traffic_presets: [64 * 1024, 512 * 1024, 0],
            site_rules: Vec::new(),
            folder_rules: Vec::new(),
            scheduled_actions: Vec::new(),
            remote: RemoteSettings::default(),
        }
    }
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=64).contains(&self.max_concurrent) || !(1..=32).contains(&self.max_connections) {
            return Err("concurrency must be 1–64 and connections must be 1–32".into());
        }
        if !self.schedule.valid() {
            return Err("schedule times must be HH:MM".into());
        }
        let mut origins = std::collections::HashSet::new();
        for rule in &self.site_rules {
            let origin = crate::site::origin(&rule.host)?;
            if !origins.insert(origin)
                || !(1..=64).contains(&rule.max_concurrent)
                || !(1..=32).contains(&rule.connections)
            {
                return Err(
                    "site origins must be unique, concurrency 1–64 and connections 1–32".into(),
                );
            }
        }
        for rule in &self.folder_rules {
            if rule.folder.as_os_str().is_empty()
                || rule.extensions.is_empty()
                || rule.extensions.iter().any(|ext| {
                    let ext = ext.trim().trim_start_matches('.');
                    ext.is_empty()
                        || !ext
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
                })
            {
                return Err(
                    "folder rules need a folder and extensions such as zip or tar.gz".into(),
                );
            }
        }
        let mut ids = std::collections::HashSet::new();
        for action in &self.scheduled_actions {
            action.validate()?;
            if !ids.insert(action.id) {
                return Err("scheduled action IDs must be unique".into());
            }
        }
        self.remote.validate()
    }

    pub fn effective_global_limit(&self) -> u64 {
        match self.traffic_mode {
            TrafficMode::Custom => self.global_limit,
            TrafficMode::Low => self.traffic_presets[0],
            TrafficMode::Medium => self.traffic_presets[1],
            TrafficMode::High => self.traffic_presets[2],
        }
    }

    pub fn folder_for_name(&self, name: &str, cat: Category) -> PathBuf {
        let name = name.to_ascii_lowercase();
        self.folder_rules
            .iter()
            .find(|rule| {
                rule.extensions.iter().any(|extension| {
                    name.ends_with(&format!(
                        ".{}",
                        extension
                            .trim()
                            .trim_start_matches('.')
                            .to_ascii_lowercase()
                    ))
                })
            })
            .map(|rule| rule.folder.clone())
            .unwrap_or_else(|| self.folder_for(cat))
    }

    pub fn folder_for(&self, cat: Category) -> PathBuf {
        if self.category_folders && !cat.folder().is_empty() {
            self.download_dir.join(cat.folder())
        } else {
            self.download_dir.clone()
        }
    }

    pub fn build_client(&self) -> Result<reqwest::Client, String> {
        let mut b = reqwest::Client::builder()
            .user_agent(concat!("Downloader/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(20))
            .read_timeout(std::time::Duration::from_secs(30))
            .http1_only()
            // reqwest removes Authorization for host/port changes, but not a scheme-only
            // origin change on the same explicit port. Do not follow that downgrade.
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 10 {
                    attempt.error("too many redirects")
                } else if attempt.previous().last().is_some_and(|from| {
                    from.scheme() != attempt.url().scheme()
                        && from.host_str() == attempt.url().host_str()
                        && from.port_or_known_default() == attempt.url().port_or_known_default()
                }) {
                    attempt.stop()
                } else {
                    attempt.follow()
                }
            }))
            .pool_max_idle_per_host(32);
        if !self.proxy.trim().is_empty() {
            b = b.proxy(reqwest::Proxy::all(self.proxy.trim()).map_err(|e| format!("proxy: {e}"))?);
        }
        b.build().map_err(|e| e.to_string())
    }

    /// Crawler requests inspect each Location header before following it.
    pub fn build_crawl_client(&self) -> Result<crate::grabber::CrawlClient, String> {
        self.build_protocol_client().map(crate::grabber::CrawlClient::new)
    }

    /// Protocol adapters follow redirects explicitly and retain the configured proxy.
    pub fn build_protocol_client(&self) -> Result<reqwest::Client, String> {
        let mut builder = reqwest::Client::builder()
            .user_agent(concat!("Downloader/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(std::time::Duration::from_secs(20))
            .read_timeout(std::time::Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none());
        if !self.proxy.trim().is_empty() {
            builder = builder
                .proxy(reqwest::Proxy::all(self.proxy.trim()).map_err(|e| format!("proxy: {e}"))?);
        }
        builder
            .build()
            .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schedule_window() {
        let s = Schedule {
            enabled: true,
            start: "23:00".into(),
            stop: "06:30".into(),
        };
        assert!(s.allows(23 * 60 + 5));
        assert!(s.allows(60));
        assert!(!s.allows(12 * 60));
        let d = Schedule {
            enabled: true,
            start: "09:00".into(),
            stop: "17:00".into(),
        };
        assert!(d.allows(10 * 60) && !d.allows(18 * 60));
        assert!(Schedule::default().allows(720));
    }
}
