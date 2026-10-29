//! User-level native messaging registration and bundled extension validation.
use std::path::{Path, PathBuf};

pub const HOST_NAME: &str = "io.github.stevenke1981.downloader";
/// Stable extension ID (derived from the public key in extension/manifest.json).
pub const EXTENSION_ID: &str = include_str!("../../extension/EXTENSION_ID").trim_ascii();
const BUNDLED_MANIFEST: &str = include_str!("../../extension/manifest.json");

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Browser {
    Chrome,
    Brave,
    Edge,
}

impl Browser {
    pub const ALL: [Self; 3] = [Self::Chrome, Self::Brave, Self::Edge];
    pub fn from_index(index: i32) -> Option<Self> {
        usize::try_from(index)
            .ok()
            .and_then(|i| Self::ALL.get(i).copied())
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Chrome => "Chrome",
            Self::Brave => "Brave",
            Self::Edge => "Edge",
        }
    }
    pub fn extensions_url(self) -> &'static str {
        match self {
            Self::Chrome => "chrome://extensions/",
            Self::Brave => "brave://extensions/",
            Self::Edge => "edge://extensions/",
        }
    }
    #[cfg(windows)]
    fn registry_key(self) -> &'static str {
        match self {
            Self::Chrome => r"Software\Google\Chrome\NativeMessagingHosts",
            Self::Brave => r"Software\Chromium\NativeMessagingHosts",
            Self::Edge => r"Software\Microsoft\Edge\NativeMessagingHosts",
        }
    }
    #[cfg(not(windows))]
    fn manifest_dir(self) -> Option<PathBuf> {
        #[cfg(target_os = "macos")]
        let (base, relative) = (
            dirs::home_dir()?.join("Library/Application Support"),
            match self {
                Self::Chrome => "Google/Chrome",
                Self::Brave => "BraveSoftware/Brave-Browser",
                Self::Edge => "Microsoft Edge",
            },
        );
        #[cfg(not(target_os = "macos"))]
        let (base, relative) = (
            dirs::config_dir()?,
            match self {
                Self::Chrome => "google-chrome",
                Self::Brave => "BraveSoftware/Brave-Browser",
                Self::Edge => "microsoft-edge",
            },
        );
        Some(base.join(relative).join("NativeMessagingHosts"))
    }
}

/// Connector health only; this cannot prove the extension is loaded in a profile.
#[derive(Debug)]
pub struct NativeHostStatus {
    pub registered: bool,
    pub manifest_path: Option<PathBuf>,
    pub host_path: Option<PathBuf>,
    pub issue: Option<String>,
}

pub fn host_exe_name() -> &'static str {
    if cfg!(windows) {
        "downloader-host.exe"
    } else {
        "downloader-host"
    }
}

pub fn absolute_file(path: &Path) -> Result<PathBuf, String> {
    if !path.is_absolute() {
        return Err(format!(
            "An absolute file path is required: {}",
            path.display()
        ));
    }
    if !path.is_file() {
        return Err(format!("File not found: {}", path.display()));
    }
    std::fs::canonicalize(path)
        .map(normal_path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

/// Browser/file chooser implementations do not all accept verbatim prefixes.
pub fn normal_path(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::{OsStrExt, OsStringExt};
        let wide: Vec<u16> = path.as_os_str().encode_wide().collect();
        let prefix: Vec<u16> = r"\\?\".encode_utf16().collect();
        let unc: Vec<u16> = r"\\?\UNC\".encode_utf16().collect();
        if wide.starts_with(&unc) {
            return std::ffi::OsString::from_wide(
                &[&[92u16, 92u16][..], &wide[unc.len()..]].concat(),
            )
            .into();
        }
        if wide.starts_with(&prefix) {
            return std::ffi::OsString::from_wide(&wide[prefix.len()..]).into();
        }
    }
    path
}

/// Reject unrelated, incomplete or differently keyed unpacked extension folders.
pub fn validate_extension_dir(path: &Path) -> Result<PathBuf, String> {
    let text = std::fs::read_to_string(path.join("manifest.json"))
        .map_err(|e| format!("{}: {e}", path.join("manifest.json").display()))?;
    let actual: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("Invalid extension manifest: {e}"))?;
    let bundled: serde_json::Value = serde_json::from_str(BUNDLED_MANIFEST)
        .map_err(|e| format!("Invalid bundled extension manifest: {e}"))?;
    for field in [
        "manifest_version",
        "key",
        "background",
        "content_scripts",
        "options_ui",
    ] {
        if actual[field] != bundled[field] {
            return Err(format!(
                "This is not the bundled Downloader extension ({field} differs)"
            ));
        }
    }
    if !actual["permissions"]
        .as_array()
        .is_some_and(|p| p.iter().any(|v| v == "nativeMessaging"))
    {
        return Err("The extension is missing nativeMessaging permission".into());
    }
    for file in [
        "background.js",
        "alt.js",
        "options.html",
        "options.js",
        "icons/icon16.png",
        "icons/icon32.png",
        "icons/icon48.png",
        "icons/icon128.png",
        "_locales/en/messages.json",
        "_locales/zh_TW/messages.json",
    ] {
        if !path.join(file).is_file() {
            return Err(format!("Incomplete extension folder: {file} is missing"));
        }
    }
    std::fs::canonicalize(path)
        .map(normal_path)
        .map_err(|e| e.to_string())
}

pub fn manifest_json(host_path: &Path) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "name": HOST_NAME, "description": "Downloader native messaging host",
        "path": host_path.to_string_lossy(), "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{}/", EXTENSION_ID)],
    }))
    .expect("a native host manifest contains only JSON strings")
}

mod registration;
#[cfg(windows)]
pub use registration::browser_app_path;
pub use registration::{native_host_status, register, unregister};

#[cfg(test)]
mod tests;
