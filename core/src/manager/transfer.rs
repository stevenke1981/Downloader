//! Queue export / import. Exports hold what is needed to download again: never cookies,
//! passwords, logs or progress.
use super::*;
use crate::task::ConflictPolicy;
use serde::{Deserialize, Serialize};

const MAX_IMPORT: usize = 10_000;

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Item {
    url: String,
    kind: DownloadKind,
    dir: Option<PathBuf>,
    file_name: Option<String>,
    connections: Option<u32>,
    speed_limit: Option<u64>,
    checksum: Option<Checksum>,
    priority: i32,
    referer: Option<String>,
    headers: Vec<(String, String)>,
    user_agent: Option<String>,
    mirrors: Vec<String>,
    conflict: Option<ConflictPolicy>,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
struct Document {
    schema: u32,
    downloads: Vec<Item>,
}

/// Result of `Manager::import_queue`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ImportReport {
    /// Ids of the downloads that were added.
    pub added: Vec<u64>,
    /// (url, reason) of every entry that was not added.
    pub skipped: Vec<(String, String)>,
}

impl Manager {
    /// Every unfinished download as `{"schema":1,"downloads":[..]}`.
    pub fn export_queue(&self) -> String {
        let downloads = self
            .inner
            .entries
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| {
                let t = e.task.lock().unwrap();
                (t.status != Status::Completed).then(|| Item {
                    url: t.url.clone(),
                    kind: t.kind,
                    dir: Some(t.dir.clone()),
                    file_name: t
                        .suggested_name
                        .clone()
                        .or_else(|| Some(t.file_name.clone()).filter(|n| !n.is_empty())),
                    connections: Some(t.connections),
                    speed_limit: Some(t.speed_limit),
                    checksum: t.checksum.clone(),
                    priority: t.priority,
                    referer: t.referer.clone(),
                    headers: t.headers.clone(),
                    user_agent: t.user_agent.clone(),
                    mirrors: t.mirrors.clone(),
                    conflict: Some(t.conflict),
                })
            })
            .collect();
        serde_json::to_string_pretty(&Document {
            schema: 1,
            downloads,
        })
        .unwrap_or_default()
    }

    /// Add every entry of an export. Duplicates and invalid entries are reported, not fatal.
    pub fn import_queue(&self, json: &str, start: bool) -> Result<ImportReport, String> {
        let doc: Document =
            serde_json::from_str(json).map_err(|e| format!("invalid queue file: {e}"))?;
        if doc.schema != 1 {
            return Err(format!(
                "invalid queue file: unsupported schema {}",
                doc.schema
            ));
        }
        if doc.downloads.len() > MAX_IMPORT {
            return Err(format!(
                "invalid queue file: more than {MAX_IMPORT} downloads"
            ));
        }
        let mut report = ImportReport::default();
        for i in doc.downloads {
            let url = i.url.clone();
            let added = self.try_add(NewDownload {
                url: i.url,
                kind: i.kind,
                dir: i.dir,
                connections: i.connections,
                speed_limit: i.speed_limit,
                checksum: i.checksum,
                priority: i.priority,
                start,
                referer: i.referer,
                file_name: i.file_name,
                headers: i.headers,
                user_agent: i.user_agent,
                mirrors: i.mirrors,
                conflict: i.conflict,
                unique: true,
                ..Default::default()
            });
            match added {
                Ok(id) => report.added.push(id),
                Err(e) => report.skipped.push((url, e)),
            }
        }
        Ok(report)
    }
}
