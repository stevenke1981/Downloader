//! Register / unregister the native messaging host for Chrome, Chromium and Edge.
use std::path::{Path, PathBuf};

pub const HOST_NAME: &str = "io.github.stevenke1981.downloader";
/// Stable extension ID (derived from the public key in extension/manifest.json).
pub const EXTENSION_ID: &str = include_str!("../../extension/EXTENSION_ID").trim_ascii();

pub fn host_exe_name() -> &'static str {
    if cfg!(windows) { "downloader-host.exe" } else { "downloader-host" }
}

pub fn manifest_json(host_path: &Path) -> String {
    serde_json::to_string_pretty(&serde_json::json!({
        "name": HOST_NAME,
        "description": "Downloader native messaging host",
        "path": host_path.to_string_lossy(),
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{}/", EXTENSION_ID)],
    }))
    .unwrap()
}

#[cfg(not(windows))]
fn manifest_dirs() -> Vec<PathBuf> {
    let mut v = vec![];
    #[cfg(target_os = "macos")]
    if let Some(h) = dirs::home_dir() {
        let base = h.join("Library/Application Support");
        for b in ["Google/Chrome", "Chromium", "Microsoft Edge", "BraveSoftware/Brave-Browser"] {
            v.push(base.join(b).join("NativeMessagingHosts"));
        }
    }
    #[cfg(not(target_os = "macos"))]
    if let Some(c) = dirs::config_dir() {
        for b in ["google-chrome", "chromium", "microsoft-edge", "BraveSoftware/Brave-Browser"] {
            v.push(c.join(b).join("NativeMessagingHosts"));
        }
    }
    v
}

#[cfg(windows)]
const REG_KEYS: [&str; 3] = [
    r"HKCU\Software\Google\Chrome\NativeMessagingHosts",
    r"HKCU\Software\Microsoft\Edge\NativeMessagingHosts",
    r"HKCU\Software\Chromium\NativeMessagingHosts",
];

/// Register for the current user. `host_path` is the absolute path of downloader-host.
pub fn register(host_path: &Path) -> Result<Vec<PathBuf>, String> {
    let json = manifest_json(host_path);
    #[cfg(windows)]
    {
        let dir = crate::ipc::default_state_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join(format!("{HOST_NAME}.json"));
        std::fs::write(&file, json).map_err(|e| e.to_string())?;
        for k in REG_KEYS {
            let st = std::process::Command::new("reg")
                .args(["add", &format!(r"{k}\{HOST_NAME}"), "/ve", "/t", "REG_SZ", "/d"])
                .arg(&file)
                .arg("/f")
                .output()
                .map_err(|e| e.to_string())?;
            if !st.status.success() {
                return Err(String::from_utf8_lossy(&st.stderr).into());
            }
        }
        Ok(vec![file])
    }
    #[cfg(not(windows))]
    {
        let mut out = vec![];
        for d in manifest_dirs() {
            std::fs::create_dir_all(&d).map_err(|e| e.to_string())?;
            let f = d.join(format!("{HOST_NAME}.json"));
            std::fs::write(&f, &json).map_err(|e| e.to_string())?;
            out.push(f);
        }
        Ok(out)
    }
}

pub fn unregister() {
    #[cfg(windows)]
    {
        for k in REG_KEYS {
            let _ = std::process::Command::new("reg").args(["delete", &format!(r"{k}\{HOST_NAME}"), "/f"]).output();
        }
        let _ = std::fs::remove_file(crate::ipc::default_state_dir().join(format!("{HOST_NAME}.json")));
    }
    #[cfg(not(windows))]
    for d in manifest_dirs() {
        let _ = std::fs::remove_file(d.join(format!("{HOST_NAME}.json")));
    }
}
