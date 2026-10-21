//! Settings actions never silently install an extension or alter browser policy.
pub use dlcore::browser::{Browser, NativeHostStatus};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn host_path() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    dlcore::browser::absolute_file(&exe.with_file_name(dlcore::browser::host_exe_name()))
}

fn extension_candidates(exe: &Path) -> Vec<PathBuf> {
    let Some(dir) = exe.parent() else {
        return vec![];
    };
    let mut candidates = vec![dir.join("extension")];
    // Standard macOS .app: Contents/MacOS/downloader, Resources/extension.
    if dir.file_name().is_some_and(|name| name == "MacOS") {
        if let Some(contents) = dir.parent() {
            candidates.push(contents.join("Resources/extension"));
        }
    }
    // Only look in a repository for a binary in its target tree. Installed apps
    // must not accidentally load a developer's stale checkout instead.
    for ancestor in exe.ancestors().skip(2).take(4) {
        if ancestor.file_name().is_some_and(|name| name == "target") {
            if let Some(root) = ancestor.parent() {
                if root.join("Cargo.toml").is_file() && root.join("gui/Cargo.toml").is_file() {
                    candidates.push(root.join("extension"));
                }
            }
        }
    }
    candidates
}

fn extension_dir_for_exe(exe: &Path) -> Result<PathBuf, String> {
    let mut invalid = None;
    for candidate in extension_candidates(exe) {
        if candidate.is_dir() {
            match dlcore::browser::validate_extension_dir(&candidate) {
                Ok(path) => return Ok(path),
                Err(error) => invalid = Some(error),
            }
        }
    }
    Err(invalid.unwrap_or_else(|| "The bundled extension folder is missing. Extract the complete portable package or reinstall Downloader".into()))
}

pub fn extension_dir() -> Result<PathBuf, String> {
    extension_dir_for_exe(&std::env::current_exe().map_err(|e| e.to_string())?)
}

pub fn status(browser: Browser) -> NativeHostStatus {
    match host_path() {
        Ok(host) => dlcore::browser::native_host_status(browser, &host),
        Err(error) => NativeHostStatus {
            registered: false,
            manifest_path: None,
            host_path: None,
            issue: Some(error),
        },
    }
}

pub fn register() -> Result<(), String> {
    // Validate the extension bundle as well, so repair does not claim that a
    // partial portable extraction is ready to install.
    extension_dir()?;
    dlcore::browser::register(&host_path()?).map(|_| ())
}

/// Use the retained App clipboard: on Linux dropping its owner loses the text.
pub fn copy_extension_path(clipboard: &mut arboard::Clipboard) -> Result<(), String> {
    clipboard
        .set_text(extension_dir()?.to_string_lossy().into_owned())
        .map_err(|e| e.to_string())
}

fn existing_file(candidates: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    candidates
        .into_iter()
        .find_map(|path| dlcore::browser::absolute_file(&path).ok())
}

#[cfg(windows)]
fn installed_browser(browser: Browser) -> Result<Option<PathBuf>, String> {
    if let Some(path) = dlcore::browser::browser_app_path(browser)? {
        return Ok(Some(path));
    }
    let relative = match browser {
        Browser::Chrome => "Google/Chrome/Application/chrome.exe",
        Browser::Brave => "BraveSoftware/Brave-Browser/Application/brave.exe",
        Browser::Edge => "Microsoft/Edge/Application/msedge.exe",
    };
    Ok(existing_file(
        [
            "LOCALAPPDATA",
            "ProgramW6432",
            "ProgramFiles",
            "ProgramFiles(x86)",
        ]
        .into_iter()
        .filter_map(std::env::var_os)
        .map(|root| PathBuf::from(root).join(relative)),
    ))
}

#[cfg(target_os = "macos")]
fn installed_browser(browser: Browser) -> Result<Option<PathBuf>, String> {
    let relative = match browser {
        Browser::Chrome => "Google Chrome.app/Contents/MacOS/Google Chrome",
        Browser::Brave => "Brave Browser.app/Contents/MacOS/Brave Browser",
        Browser::Edge => "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
    };
    let mut roots = vec![PathBuf::from("/Applications")];
    roots.extend(dirs::home_dir().map(|home| home.join("Applications")));
    Ok(existing_file(
        roots.into_iter().map(|root| root.join(relative)),
    ))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn installed_browser(browser: Browser) -> Result<Option<PathBuf>, String> {
    let names: &[&str] = match browser {
        Browser::Chrome => &["google-chrome", "google-chrome-stable"],
        Browser::Brave => &["brave-browser", "brave-browser-stable", "brave"],
        Browser::Edge => &["microsoft-edge", "microsoft-edge-stable"],
    };
    let path = std::env::var_os("PATH").unwrap_or_default();
    Ok(existing_file(
        std::env::split_paths(&path)
            .filter(|dir| dir.is_absolute())
            .flat_map(|dir| names.iter().map(move |name| dir.join(name))),
    ))
}

pub fn browser_executable(browser: Browser) -> Result<PathBuf, String> {
    installed_browser(browser)?.ok_or_else(|| {
        format!(
            "{} is not installed or its executable could not be found",
            browser.name()
        )
    })
}

pub fn open_extensions_page(browser: Browser) -> Result<(), String> {
    // Internal browser URLs must be passed directly to that browser, not to the
    // OS default URL handler (which cannot route chrome:/brave:/edge: reliably).
    let exe = browser_executable(browser)?;
    let mut command = Command::new(&exe);
    command.arg(browser.extensions_url());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("Cannot open {}: {e}", browser.name()))
}

pub fn open_extension_folder() -> Result<(), String> {
    let path = extension_dir()?;
    #[cfg(windows)]
    {
        let system = std::env::var_os("SystemRoot").ok_or("Windows SystemRoot is unavailable")?;
        return Command::new(PathBuf::from(system).join("explorer.exe"))
            .arg(path)
            .spawn()
            .map(|_| ())
            .map_err(|e| format!("Cannot open the extension folder: {e}"));
    }
    #[cfg(not(windows))]
    {
        let tool = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        let status = Command::new(tool)
            .arg(path)
            .status()
            .map_err(|e| format!("Cannot open the extension folder: {e}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("File manager exited with {status}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn copy_bundle(target: &Path) {
        fn copy_dir(source: &Path, target: &Path) {
            std::fs::create_dir_all(target).unwrap();
            for file in std::fs::read_dir(source).unwrap() {
                let file = file.unwrap();
                if file.file_type().unwrap().is_dir() {
                    copy_dir(&file.path(), &target.join(file.file_name()));
                } else {
                    std::fs::copy(file.path(), target.join(file.file_name())).unwrap();
                }
            }
        }
        copy_dir(
            &Path::new(env!("CARGO_MANIFEST_DIR")).join("../extension"),
            target,
        );
    }
    #[test]
    fn finds_installed_portable_and_macos_resource_layouts() {
        let dir = tempfile::tempdir().unwrap();
        let portable = dir.path().join("portable 測試 & $ (1)");
        copy_bundle(&portable.join("extension"));
        assert_eq!(
            extension_dir_for_exe(&portable.join("downloader.exe")).unwrap(),
            dlcore::browser::normal_path(portable.join("extension").canonicalize().unwrap())
        );
        let app = dir.path().join("Downloader.app/Contents");
        copy_bundle(&app.join("Resources/extension"));
        assert_eq!(
            extension_dir_for_exe(&app.join("MacOS/downloader")).unwrap(),
            dlcore::browser::normal_path(app.join("Resources/extension").canonicalize().unwrap())
        );
    }
    #[test]
    fn dev_lookup_requires_binary_under_its_own_target_tree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("gui")).unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "fixture").unwrap();
        std::fs::write(dir.path().join("gui/Cargo.toml"), "fixture").unwrap();
        copy_bundle(&dir.path().join("extension"));
        assert!(extension_dir_for_exe(&dir.path().join("target/release/downloader.exe")).is_ok());
        assert!(extension_dir_for_exe(&dir.path().join("installed/downloader.exe")).is_err());
    }
    #[test]
    fn rejects_incomplete_bundle_and_keeps_metacharacters_as_path_data() {
        let dir = tempfile::tempdir().unwrap();
        assert!(extension_dir_for_exe(&dir.path().join("downloader.exe")).is_err());
        copy_bundle(&dir.path().join("extension"));
        std::fs::remove_file(dir.path().join("extension/options.js")).unwrap();
        assert!(
            extension_dir_for_exe(&dir.path().join("downloader.exe"))
                .unwrap_err()
                .contains("options.js")
        );
        let file = dir.path().join("browser 測試 & $(1).exe");
        std::fs::write(&file, "never executed").unwrap();
        assert_eq!(
            existing_file([dir.path().to_path_buf(), file.clone()]),
            Some(dlcore::browser::absolute_file(&file).unwrap())
        );
    }
}
