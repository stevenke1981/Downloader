use crate::category::Category;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
        Schedule { enabled: false, start: "02:00".into(), stop: "07:00".into() }
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
        let (Some(a), Some(b)) = (hm(&self.start), hm(&self.stop)) else { return true };
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
        }
    }
}

impl Settings {
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
            .pool_max_idle_per_host(32);
        if !self.proxy.trim().is_empty() {
            b = b.proxy(reqwest::Proxy::all(self.proxy.trim()).map_err(|e| format!("proxy: {e}"))?);
        }
        b.build().map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schedule_window() {
        let s = Schedule { enabled: true, start: "23:00".into(), stop: "06:30".into() };
        assert!(s.allows(23 * 60 + 5));
        assert!(s.allows(60));
        assert!(!s.allows(12 * 60));
        let d = Schedule { enabled: true, start: "09:00".into(), stop: "17:00".into() };
        assert!(d.allows(10 * 60) && !d.allows(18 * 60));
        assert!(Schedule::default().allows(720));
    }
}
