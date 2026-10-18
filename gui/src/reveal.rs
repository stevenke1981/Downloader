//! Locate downloads without executing them or passing paths through a shell.
use dlcore::Status;
use std::path::{Path, PathBuf};

#[derive(Debug, PartialEq)]
pub enum RevealError {
    NotCompleted,
    Missing,
    NotFile,
    Io(String),
}

pub fn completed_file(status: Status, path: &Path) -> Result<PathBuf, RevealError> {
    if status != Status::Completed {
        return Err(RevealError::NotCompleted);
    }
    let meta = std::fs::metadata(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            RevealError::Missing
        } else {
            RevealError::Io(e.to_string())
        }
    })?;
    if !meta.is_file() {
        return Err(RevealError::NotFile);
    }
    std::fs::canonicalize(path).map_err(|e| RevealError::Io(e.to_string()))
}

pub fn reveal(status: Status, path: &Path) -> Result<(), RevealError> {
    let file = completed_file(status, path)?;
    platform_reveal(&file).map_err(RevealError::Io)
}

#[cfg(windows)]
fn platform_reveal(path: &Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::{
            System::Com::{
                COINIT_APARTMENTTHREADED, CoInitializeEx, CoTaskMemFree, CoUninitialize,
            },
            UI::Shell::{SHOpenFolderAndSelectItems, SHParseDisplayName},
        },
        core::PCWSTR,
    };
    // Shell APIs take a normal DOS/UNC path, not Rust's verbatim canonical prefix.
    let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
    let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
    let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
    let mut wide = if raw.starts_with(&unc) {
        [vec![92, 92], raw[unc.len()..].to_vec()].concat()
    } else if raw.starts_with(&prefix) {
        raw[prefix.len()..].to_vec()
    } else {
        raw
    };
    wide.push(0);
    // A PIDL identifies the file; no shell command string or downloaded executable is run.
    unsafe {
        let initialized = CoInitializeEx(None, COINIT_APARTMENTTHREADED).is_ok();
        let mut pidl = std::ptr::null_mut();
        let result = SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None)
            .and_then(|_| SHOpenFolderAndSelectItems(pidl, None, 0));
        if !pidl.is_null() {
            CoTaskMemFree(Some(pidl.cast()));
        }
        if initialized {
            CoUninitialize();
        }
        result.map_err(|e| e.to_string())
    }
}

#[cfg(not(windows))]
fn platform_reveal(path: &Path) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .status();
    #[cfg(not(target_os = "macos"))]
    let status = std::process::Command::new("xdg-open")
        .arg(path.parent().ok_or("Missing parent directory")?)
        .status();
    match status {
        Ok(s) if s.success() => Ok(()),
        Ok(s) => Err(format!("File manager exited with {s}")),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_every_unfinished_state_even_when_file_exists() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("fixture.txt");
        std::fs::write(&file, b"safe test data").unwrap();
        for status in [
            Status::Queued,
            Status::Downloading,
            Status::Paused,
            Status::Verifying,
            Status::Error,
        ] {
            assert_eq!(
                completed_file(status, &file),
                Err(RevealError::NotCompleted)
            );
        }
    }
    #[test]
    fn unicode_spaces_and_shell_metacharacters_are_only_path_data() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("測試 空格 & $file, (1).txt");
        std::fs::write(&file, b"fixture").unwrap();
        assert_eq!(
            completed_file(Status::Completed, &file).unwrap(),
            file.canonicalize().unwrap()
        );
        let moved = dir.path().join("moved.txt");
        std::fs::rename(&file, moved).unwrap();
        assert_eq!(
            completed_file(Status::Completed, &file),
            Err(RevealError::Missing)
        );
        assert_eq!(
            completed_file(Status::Completed, dir.path()),
            Err(RevealError::NotFile)
        );
    }
}
