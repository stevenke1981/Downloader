use super::*;

impl Inner {
    pub(super) fn save(&self) {
        self.save_with(|_| {});
    }
    pub(super) fn save_with(&self, f: impl Fn(&mut Task)) {
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
            let name = if self.save_disabled.load(Ordering::Relaxed) {
                "downloads.json.recovered"
            } else {
                "downloads.json"
            };
            if write_atomic(&self.dir.join(name), &json).is_ok() {
                *last = json;
            }
        }
    }

}

pub(super) fn write_atomic(path: &Path, data: &[u8]) -> std::io::Result<()> {
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
pub(super) fn read_retrying(
    mut read: impl FnMut() -> std::io::Result<Vec<u8>>,
    retries: u32,
    pause: Duration,
) -> std::io::Result<Option<Vec<u8>>> {
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
pub(super) fn load_json<T: serde::de::DeserializeOwned + Default>(path: &Path) -> (T, bool) {
    let bytes = match read_retrying(|| std::fs::read(path), 5, Duration::from_millis(100)) {
        Ok(Some(bytes)) => bytes,
        Ok(None) => return (T::default(), true),
        Err(e) => {
            eprintln!(
                "{} cannot be read ({e}); starting empty and not overwriting it this session",
                path.display()
            );
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
                    eprintln!(
                        "{} is unreadable ({e}); moved it to {}",
                        path.display(),
                        bad.display()
                    );
                    (T::default(), true)
                }
                Err(re) => {
                    eprintln!(
                        "{} is unreadable ({e}) and could not be moved aside ({re}); not overwriting it this session",
                        path.display()
                    );
                    (T::default(), false)
                }
            }
        }
    }
}
