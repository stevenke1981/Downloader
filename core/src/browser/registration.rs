use super::*;

pub(super) fn inspect_manifest(file: &Path, expected_host: &Path) -> NativeHostStatus {
    let mut result = NativeHostStatus {
        registered: false,
        manifest_path: Some(file.to_path_buf()),
        host_path: None,
        issue: None,
    };
    let checked = (|| {
        if !file.is_absolute() {
            return Err("The registered manifest path is not absolute".into());
        }
        let text = std::fs::read_to_string(file)
            .map_err(|e| format!("Cannot read native host manifest {}: {e}", file.display()))?;
        let manifest: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| format!("Invalid native host manifest: {e}"))?;
        if manifest["name"] != HOST_NAME
            || manifest["type"] != "stdio"
            || manifest["allowed_origins"]
                != serde_json::json!([format!("chrome-extension://{EXTENSION_ID}/")])
        {
            return Err(
                "The native host manifest identity or extension origin is incorrect".into(),
            );
        }
        let host = manifest["path"]
            .as_str()
            .ok_or("The native host manifest has no executable path")?;
        result.host_path = Some(PathBuf::from(host));
        if absolute_file(Path::new(host))? != absolute_file(expected_host)? {
            return Err("The connector points to another Downloader installation. Register it again to repair it".into());
        }
        Ok(())
    })();
    match checked {
        Ok(()) => result.registered = true,
        Err(error) => result.issue = Some(error),
    }
    result
}

pub fn native_host_status(browser: Browser, expected_host: &Path) -> NativeHostStatus {
    match registration_target(browser) {
        Ok(Some(file)) => inspect_manifest(&file, expected_host),
        Ok(None) => NativeHostStatus {
            registered: false,
            manifest_path: None,
            host_path: None,
            issue: Some(format!(
                "{} connector is not registered for this user",
                browser.name()
            )),
        },
        Err(error) => NativeHostStatus {
            registered: false,
            manifest_path: None,
            host_path: None,
            issue: Some(error),
        },
    }
}

#[cfg(not(windows))]
fn manifest_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<_> = Browser::ALL
        .into_iter()
        .filter_map(Browser::manifest_dir)
        .collect();
    #[cfg(target_os = "macos")]
    let chromium = dirs::home_dir()
        .map(|h| h.join("Library/Application Support/Chromium/NativeMessagingHosts"));
    #[cfg(not(target_os = "macos"))]
    let chromium = dirs::config_dir().map(|c| c.join("chromium/NativeMessagingHosts"));
    dirs.extend(chromium);
    dirs
}

#[cfg(not(windows))]
fn registration_target(browser: Browser) -> Result<Option<PathBuf>, String> {
    let dir = browser
        .manifest_dir()
        .ok_or("Cannot locate the current user's browser configuration")?;
    let file = dir.join(format!("{HOST_NAME}.json"));
    match std::fs::metadata(&file) {
        Ok(_) => Ok(Some(file)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Cannot inspect {}: {e}", file.display())),
    }
}

#[cfg(windows)]
const REG_KEYS: [&str; 4] = [
    r"Software\Google\Chrome\NativeMessagingHosts",
    r"Software\Microsoft\Edge\NativeMessagingHosts",
    r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts",
    r"Software\Chromium\NativeMessagingHosts",
];

#[cfg(windows)]
fn registration_target(browser: Browser) -> Result<Option<PathBuf>, String> {
    // Chromium queries 32-bit first: a stale entry there must not be hidden by
    // a valid 64-bit entry in the Settings health indicator.
    for view in [32, 64] {
        let key = format!(r"{}\{HOST_NAME}", browser.registry_key());
        if let Some(path) = registry_default(&key, view)? {
            return Ok(Some(path));
        }
        // Brave inherits Chromium's launch_context_win.cc, including its
        // Chrome fallback. A BraveSoftware key alone does not prove health.
        if browser == Browser::Brave {
            let chrome = format!(r"{}\{HOST_NAME}", Browser::Chrome.registry_key());
            if let Some(path) = registry_default(&chrome, view)? {
                return Ok(Some(path));
            }
        }
    }
    Ok(None)
}

#[cfg(windows)]
fn registry_default(key: &str, view: u32) -> Result<Option<PathBuf>, String> {
    registry_default_at(0x80000001_u32 as i32 as isize, key, view)
}

/// Windows App Paths covers custom install locations without a shell command.
#[cfg(windows)]
pub fn browser_app_path(browser: Browser) -> Result<Option<PathBuf>, String> {
    let name = match browser {
        Browser::Chrome => "chrome.exe",
        Browser::Brave => "brave.exe",
        Browser::Edge => "msedge.exe",
    };
    let key = format!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\{name}");
    for hive in [0x80000001_u32, 0x80000002_u32] {
        for view in [32, 64] {
            if let Some(path) = registry_default_at(hive as i32 as isize, &key, view)?
                && path.is_absolute()
                && path.is_file()
            {
                return absolute_file(&path).map(Some);
            }
        }
    }
    Ok(None)
}

#[cfg(windows)]
fn registry_default_at(hive: isize, key: &str, view: u32) -> Result<Option<PathBuf>, String> {
    use std::os::windows::ffi::OsStringExt;
    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn RegGetValueW(
            hkey: isize,
            subkey: *const u16,
            value: *const u16,
            flags: u32,
            value_type: *mut u32,
            data: *mut std::ffi::c_void,
            data_size: *mut u32,
        ) -> i32;
    }
    let key_wide: Vec<u16> = key.encode_utf16().chain(Some(0)).collect();
    // RRF_RT_REG_SZ with an explicit view. Native API errors distinguish a
    // missing entry from permission/IO failures on any Windows display language.
    let flags = 0x2 | if view == 32 { 0x20000 } else { 0x10000 };
    let mut bytes = 0u32;
    // SAFETY: key is NUL terminated; bytes is a valid output pointer. Null data
    // requests the size without writing to a data buffer.
    let code = unsafe {
        RegGetValueW(
            hive,
            key_wide.as_ptr(),
            std::ptr::null(),
            flags,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut bytes,
        )
    };
    if code == 2 || code == 3 {
        return Ok(None);
    }
    if code != 0 {
        return Err(format!(
            "Cannot read {key} ({view}-bit registry): Windows error {code}"
        ));
    }
    if bytes == 0 || bytes > 65536 || !bytes.is_multiple_of(2) {
        return Err(format!("Invalid registry value: {key}"));
    }
    let mut value = vec![0u16; bytes as usize / 2];
    // SAFETY: value owns the reported buffer size, all pointers remain valid.
    // Concurrent registry changes return an error instead of a false PASS.
    let code = unsafe {
        RegGetValueW(
            hive,
            key_wide.as_ptr(),
            std::ptr::null(),
            flags,
            std::ptr::null_mut(),
            value.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if code != 0 {
        return Err(format!(
            "Cannot read {key} ({view}-bit registry): Windows error {code}"
        ));
    }
    value.truncate(bytes as usize / 2);
    while value.last() == Some(&0) {
        value.pop();
    }
    if value.is_empty() || value.contains(&0) {
        return Err(format!("Empty or invalid registry value: {key}"));
    }
    Ok(Some(std::ffi::OsString::from_wide(&value).into()))
}

#[cfg(windows)]
fn reg_command() -> std::process::Command {
    use std::os::windows::process::CommandExt;
    let mut command = std::process::Command::new("reg.exe");
    command.creation_flags(0x08000000); // CREATE_NO_WINDOW for Settings actions.
    command
}

#[cfg(windows)]
fn check_command(output: std::process::Output, action: &str) -> Result<(), String> {
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "{action} failed ({}): {} {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
            String::from_utf8_lossy(&output.stdout).trim()
        ))
    }
}

/// Register all supported browsers for the current user. Validate the host
/// before touching configuration, then verify the resulting registration.
pub fn register(host_path: &Path) -> Result<Vec<PathBuf>, String> {
    let host_path = absolute_file(host_path)?;
    let json = manifest_json(&host_path);
    #[cfg(windows)]
    let files = {
        let dir = crate::ipc::default_state_dir();
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        let file = dir.join(format!("{HOST_NAME}.json"));
        std::fs::write(&file, json).map_err(|e| e.to_string())?;
        for key in REG_KEYS {
            for view in [32, 64] {
                let output = reg_command()
                    .args([
                        "add",
                        &format!(r"HKCU\{key}\{HOST_NAME}"),
                        "/ve",
                        "/t",
                        "REG_SZ",
                        "/d",
                    ])
                    .arg(&file)
                    .args(["/f", &format!("/reg:{view}")])
                    .output()
                    .map_err(|e| format!("Cannot start registry registration: {e}"))?;
                check_command(output, &format!("Registering {key} ({view}-bit)"))?;
            }
        }
        vec![file]
    };
    #[cfg(not(windows))]
    let files = {
        let dirs = manifest_dirs();
        if dirs.is_empty() {
            return Err("Cannot locate the current user's browser configuration".into());
        }
        let mut files = vec![];
        for dir in dirs {
            std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
            let file = dir.join(format!("{HOST_NAME}.json"));
            std::fs::write(&file, &json).map_err(|e| e.to_string())?;
            files.push(file);
        }
        files
    };
    for browser in Browser::ALL {
        let status = native_host_status(browser, &host_path);
        if !status.registered {
            return Err(status.issue.unwrap_or_else(|| {
                format!("{} registration could not be verified", browser.name())
            }));
        }
    }
    Ok(files)
}

pub(super) fn remove_manifest(file: &Path) -> Result<(), String> {
    match std::fs::remove_file(file) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("Cannot remove {}: {e}", file.display())),
    }
}

/// Only current-user entries are removed. Missing entries are already removed;
/// permission and process failures are reported instead of claiming success.
pub fn unregister() -> Result<(), String> {
    let mut failures = vec![];
    #[cfg(windows)]
    {
        for key in REG_KEYS {
            let relative = format!(r"{key}\{HOST_NAME}");
            for view in [32, 64] {
                let removed = (|| {
                    if registry_default(&relative, view)?.is_none() {
                        return Ok(());
                    }
                    let output = reg_command()
                        .args([
                            "delete",
                            &format!(r"HKCU\{relative}"),
                            "/f",
                            &format!("/reg:{view}"),
                        ])
                        .output()
                        .map_err(|e| format!("Cannot start registry removal: {e}"))?;
                    check_command(output, &format!("Removing {relative} ({view}-bit)"))?;
                    if registry_default(&relative, view)?.is_some() {
                        return Err(format!("Registry entry still exists: HKCU\\{relative}"));
                    }
                    Ok(())
                })();
                if let Err(error) = removed {
                    failures.push(error);
                }
            }
        }
        // Retain the manifest if any deletion failed so surviving keys do not
        // become dangling registrations.
        if failures.is_empty()
            && let Err(error) =
                remove_manifest(&crate::ipc::default_state_dir().join(format!("{HOST_NAME}.json")))
        {
            failures.push(error);
        }
    }
    #[cfg(not(windows))]
    for dir in manifest_dirs() {
        if let Err(error) = remove_manifest(&dir.join(format!("{HOST_NAME}.json"))) {
            failures.push(error);
        }
    }
    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n"))
    }
}
