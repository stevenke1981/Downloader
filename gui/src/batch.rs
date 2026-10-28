//! Validate a complete paste before mutating the queue.
use dlcore::{Manager, NewDownload};
use slint::ComponentHandle;
use std::collections::HashSet;

#[derive(Debug, PartialEq)]
pub enum BatchError {
    Empty,
    TooMany,
    Line(usize, String),
    Checksum,
}

pub fn parse(input: &str) -> Result<Vec<String>, BatchError> {
    let mut seen = HashSet::new();
    let mut urls = Vec::new();
    for (line, value) in input.lines().enumerate() {
        let value = value.trim().trim_start_matches('\u{feff}');
        if value.is_empty() {
            continue;
        }
        if let Err(reason) = dlcore::util::validate_source(value) {
            return Err(BatchError::Line(line + 1, reason));
        }
        if seen.insert(value.to_owned()) {
            urls.push(value.to_owned());
            if urls.len() > 1000 {
                return Err(BatchError::TooMany);
            }
        }
    }
    if urls.is_empty() {
        return Err(BatchError::Empty);
    }
    Ok(urls)
}

pub fn enqueue(
    manager: &Manager,
    input: &str,
    template: NewDownload,
) -> Result<Vec<u64>, BatchError> {
    let urls = parse(input)?;
    if template.checksum.is_some() && urls.len() != 1 {
        return Err(BatchError::Checksum);
    }
    Ok(urls
        .into_iter()
        .map(|url| {
            manager.add(NewDownload {
                url,
                ..template.clone()
            })
        })
        .collect())
}

pub fn message(ui: &crate::AppWindow, error: BatchError) -> slint::SharedString {
    let tr = ui.global::<crate::Tr>();
    match error {
        BatchError::Empty => tr.invoke_batch_empty(),
        BatchError::TooMany => tr.invoke_batch_limit(),
        BatchError::Line(line, reason) => tr.invoke_batch_line(line as i32, reason.into()),
        BatchError::Checksum => tr.invoke_batch_checksum(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deduplicates_and_reports_original_line_numbers() {
        assert_eq!(
            parse("\nhttps://example.org/a\r\n https://example.org/a \nhttps://example.org/b")
                .unwrap(),
            ["https://example.org/a", "https://example.org/b"]
        );
        assert!(matches!(
            parse("https://example.org/a\n\nnot a URL"),
            Err(BatchError::Line(3, _))
        ));
        assert_eq!(parse(" \n\r\n"), Err(BatchError::Empty));
    }

    #[test]
    fn invalid_tail_and_multi_download_checksum_leave_queue_untouched() {
        let state = tempfile::tempdir().unwrap();
        let manager = Manager::new(state.path());
        assert!(matches!(
            enqueue(
                &manager,
                "https://example.org/a\ninvalid",
                NewDownload::default()
            ),
            Err(BatchError::Line(2, _))
        ));
        assert!(manager.snapshots().is_empty());
        assert_eq!(
            enqueue(
                &manager,
                "https://example.org/a\nhttps://example.org/b",
                NewDownload {
                    checksum: dlcore::Checksum::guess(&"a".repeat(64)),
                    ..Default::default()
                }
            ),
            Err(BatchError::Checksum)
        );
        assert!(manager.snapshots().is_empty());
        let added = enqueue(
            &manager,
            "https://example.org/a\nhttps://example.org/a\nhttps://example.org/b",
            NewDownload::default(),
        )
        .unwrap();
        assert_eq!(added.len(), 2);
        assert_eq!(manager.snapshots().len(), 2);
        assert!(
            manager
                .snapshots()
                .iter()
                .all(|s| s.status == dlcore::Status::Paused)
        );
        manager.shutdown();
    }

    #[test]
    fn accepts_local_descriptors_with_spaces_but_rejects_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let torrent = dir.path().join("fixture with spaces.torrent");
        let arbitrary = dir.path().join("fixture.txt");
        std::fs::write(&torrent, b"fixture").unwrap();
        std::fs::write(&arbitrary, b"fixture").unwrap();
        assert_eq!(parse(&torrent.display().to_string()).unwrap().len(), 1);
        assert!(matches!(
            parse(&arbitrary.display().to_string()),
            Err(BatchError::Line(1, _))
        ));
    }
}
