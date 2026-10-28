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
    if status == Status::Completed && path.is_dir() {
        return platform_open(path).map_err(RevealError::Io);
    }
    let file = completed_file(status, path)?;
    platform_reveal(&file).map_err(RevealError::Io)
}

#[derive(Debug, PartialEq)]
pub enum PreviewError {
    File(RevealError),
    Unsupported,
    Open(String),
}

/// Decode a bounded local image/text preview in-process. Other supported documents
/// and media get metadata; downloaded content is never executed.
pub struct Preview {
    pub name: String,
    pub text: String,
    pub image: Option<(u32, u32, Vec<u8>)>,
}

pub fn preview(status: Status, path: &Path) -> Result<Preview, PreviewError> {
    use std::io::Read;
    let file = preview_file(status, path)?;
    let name = file
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    if file.is_dir() {
        let mut names = std::fs::read_dir(&file)
            .map_err(|e| PreviewError::Open(e.to_string()))?
            .take(201)
            .map(|e| e.map(|e| e.file_name().to_string_lossy().into_owned()))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| PreviewError::Open(e.to_string()))?;
        names.sort();
        let truncated = names.len() > 200;
        names.truncate(200);
        let text = format!(
            "{}\n\n{}{}",
            file.display(),
            names.join("\n"),
            if truncated { "\n…" } else { "" }
        );
        return Ok(Preview {
            name,
            text,
            image: None,
        });
    }
    let meta = std::fs::metadata(&file).map_err(|e| PreviewError::Open(e.to_string()))?;
    let extension = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut text = format!("{}\n{} bytes\n", file.display(), meta.len());
    let image = if matches!(
        extension.as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp"
    ) {
        if meta.len() > 32 * 1024 * 1024 {
            return Err(PreviewError::Open(
                "Image exceeds 32 MiB preview limit".into(),
            ));
        }
        let mut reader =
            image::ImageReader::open(&file).map_err(|e| PreviewError::Open(e.to_string()))?;
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(8192);
        limits.max_image_height = Some(8192);
        limits.max_alloc = Some(64 * 1024 * 1024);
        reader.limits(limits);
        let image = reader
            .decode()
            .map_err(|e| PreviewError::Open(e.to_string()))?;
        // A grayscale decode can fit the decoder budget but expand fourfold to RGBA.
        if u64::from(image.width()) * u64::from(image.height()) > 16 * 1024 * 1024 {
            return Err(PreviewError::Open(
                "Image exceeds 16 megapixel preview limit".into(),
            ));
        }
        let image = image.into_rgba8();
        text.push_str(&format!("{} × {}", image.width(), image.height()));
        Some((image.width(), image.height(), image.into_raw()))
    } else if matches!(
        extension.as_str(),
        "txt" | "md" | "log" | "json" | "csv" | "xml" | "html" | "htm" | "srt" | "vtt"
    ) {
        let mut bytes = Vec::new();
        std::fs::File::open(&file)
            .map_err(|e| PreviewError::Open(e.to_string()))?
            .take(256 * 1024)
            .read_to_end(&mut bytes)
            .map_err(|e| PreviewError::Open(e.to_string()))?;
        text.push_str(&String::from_utf8_lossy(&bytes));
        if meta.len() > bytes.len() as u64 {
            text.push_str("\n… (256 KiB preview limit)");
        }
        None
    } else {
        text.push_str(
            "Media / document metadata preview. Playback and document rendering are not available.",
        );
        None
    };
    Ok(Preview { name, text, image })
}

fn platform_open(path: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL};
        use windows::core::{PCWSTR, w};
        let path = dlcore::browser::normal_path(path.to_path_buf());
        let wide: Vec<_> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let result = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        if result.0 as isize <= 32 {
            return Err((result.0 as isize).to_string());
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        let viewer = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(viewer)
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

fn preview_file(status: Status, path: &Path) -> Result<PathBuf, PreviewError> {
    if status == Status::Completed && path.is_dir() {
        return std::fs::canonicalize(path).map_err(|error| PreviewError::Open(error.to_string()));
    }
    let file = completed_file(status, path).map_err(PreviewError::File)?;
    let ext = file
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(
        ext.as_str(),
        "mp4"
            | "mkv"
            | "webm"
            | "mov"
            | "avi"
            | "mpeg"
            | "mpg"
            | "m4v"
            | "mp3"
            | "flac"
            | "wav"
            | "ogg"
            | "opus"
            | "m4a"
            | "aac"
            | "wma"
            | "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "bmp"
            | "webp"
            | "avif"
            | "tif"
            | "tiff"
            | "txt"
            | "md"
            | "log"
            | "json"
            | "csv"
            | "xml"
            | "html"
            | "htm"
            | "srt"
            | "vtt"
            | "h264"
            | "ts"
            | "asf"
            | "pdf"
            | "epub"
    ) {
        return Err(PreviewError::Unsupported);
    }
    Ok(file)
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
    fn preview_requires_completed_supported_content() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("fixture.txt");
        let executable = dir.path().join("fixture.exe");
        std::fs::write(&text, b"fixture").unwrap();
        std::fs::write(&executable, b"fixture").unwrap();
        assert!(preview_file(Status::Completed, &text).is_ok());
        assert!(preview_file(Status::Completed, dir.path()).is_ok());
        assert!(preview_file(Status::Paused, &text).is_err());
        assert!(preview_file(Status::Completed, &executable).is_err());
    }
    #[test]
    fn preview_decodes_images_and_bounds_text_without_opening_a_viewer() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("page.html");
        let bytes = vec![b'x'; 300 * 1024];
        std::fs::write(&file, bytes).unwrap();
        let content = preview(Status::Completed, &file).unwrap();
        assert!(content.image.is_none());
        assert!(content.text.contains("256 KiB preview limit"));
        assert!(content.text.len() < 257 * 1024);
        let file = dir.path().join("image.png");
        image::RgbaImage::from_pixel(2, 3, image::Rgba([12, 34, 56, 255]))
            .save(&file)
            .unwrap();
        let content = preview(Status::Completed, &file).unwrap();
        let (width, height, bytes) = content.image.unwrap();
        assert_eq!((width, height), (2, 3));
        assert_eq!(bytes, [12, 34, 56, 255].repeat(6));
        let invalid = dir.path().join("invalid.png");
        std::fs::write(&invalid, b"not an image").unwrap();
        assert!(preview(Status::Completed, &invalid).is_err());
        let large = dir.path().join("large-grayscale.png");
        image::GrayImage::from_pixel(4097, 4096, image::Luma([0]))
            .save(&large)
            .unwrap();
        assert!(
            matches!(preview(Status::Completed, &large), Err(PreviewError::Open(message)) if message.contains("16 megapixel"))
        );
    }
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
