//! Start the desktop app when the user signs in, hidden in the tray.
//!
//! Windows: a value under `HKCU\...\CurrentVersion\Run`. Linux: an XDG
//! `~/.config/autostart` entry. macOS: a per-user LaunchAgent. All are per-user
//! and need no administrator rights.
use std::path::Path;

/// Command-line flag the login entry passes: start hidden in the tray.
pub const TRAY_FLAG: &str = "--tray";

#[cfg(windows)]
const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
#[cfg(windows)]
const VALUE_NAME: &str = "Downloader";

/// The command the login entry runs: the quoted program path plus `TRAY_FLAG`.
pub fn command_line(exe: &Path) -> String {
    format!("\"{}\" {TRAY_FLAG}", exe.display())
}

/// XDG autostart entry for Linux desktops.
pub fn desktop_entry(exe: &Path) -> String {
    format!(
        "[Desktop Entry]\nType=Application\nName=Downloader\nExec={}\nX-GNOME-Autostart-enabled=true\n",
        command_line(exe)
    )
}

/// Per-user LaunchAgent for macOS.
pub fn launch_agent(exe: &Path) -> String {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
<plist version=\"1.0\">\n<dict>\n\
  <key>Label</key><string>app.downloader</string>\n\
  <key>ProgramArguments</key><array><string>{}</string><string>{TRAY_FLAG}</string></array>\n\
  <key>RunAtLoad</key><true/>\n\
</dict>\n</plist>\n",
        escape(&exe.display().to_string())
    )
}

/// Turns the login entry on (pointing at `exe`) or off. Turning it off when no
/// entry exists succeeds.
pub fn set(enabled: bool, exe: &Path) -> Result<(), String> {
    platform::set(enabled, exe)
}

/// Whether a login entry for this app is present.
pub fn is_enabled() -> bool {
    platform::is_enabled()
}

/// Writes (`Some`) or removes (`None`) a file-based login entry at `path`.
/// Removing a missing file succeeds.
pub fn write_entry(path: &Path, contents: Option<&str>) -> Result<(), String> {
    match contents {
        Some(text) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)
                    .map_err(|e| format!("Cannot create {}: {e}", dir.display()))?;
            }
            std::fs::write(path, text).map_err(|e| format!("Cannot write {}: {e}", path.display()))
        }
        None => match std::fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                Err(format!("Cannot remove {}: {e}", path.display()))
            }
            _ => Ok(()),
        },
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use crate::browser::{check_command, reg_command};

    pub fn set(enabled: bool, exe: &Path) -> Result<(), String> {
        if enabled {
            let output = reg_command()
                .args(["add", RUN_KEY, "/v", VALUE_NAME, "/t", "REG_SZ", "/d"])
                .arg(command_line(exe))
                .arg("/f")
                .output()
                .map_err(|e| format!("Cannot run reg.exe: {e}"))?;
            check_command(output, "Enabling start at sign-in")
        } else if is_enabled() {
            let output = reg_command()
                .args(["delete", RUN_KEY, "/v", VALUE_NAME, "/f"])
                .output()
                .map_err(|e| format!("Cannot run reg.exe: {e}"))?;
            check_command(output, "Disabling start at sign-in")
        } else {
            Ok(())
        }
    }

    pub fn is_enabled() -> bool {
        reg_command()
            .args(["query", RUN_KEY, "/v", VALUE_NAME])
            .output()
            .is_ok_and(|o| o.status.success())
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    use std::path::PathBuf;

    fn entry_path() -> Option<PathBuf> {
        if cfg!(target_os = "macos") {
            dirs::home_dir().map(|h| h.join("Library/LaunchAgents/app.downloader.plist"))
        } else {
            dirs::config_dir().map(|c| c.join("autostart/downloader.desktop"))
        }
    }

    pub fn set(enabled: bool, exe: &Path) -> Result<(), String> {
        let path = entry_path().ok_or("No home directory for the login entry")?;
        let text = if cfg!(target_os = "macos") {
            launch_agent(exe)
        } else {
            desktop_entry(exe)
        };
        write_entry(&path, enabled.then_some(text.as_str()))
    }

    pub fn is_enabled() -> bool {
        entry_path().is_some_and(|p| p.exists())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_line_quotes_the_program_and_asks_for_the_tray() {
        let exe = Path::new("C:/Program Files/Downloader/downloader.exe");
        assert_eq!(
            command_line(exe),
            "\"C:/Program Files/Downloader/downloader.exe\" --tray"
        );
        assert!(desktop_entry(exe).contains("\nExec=\"C:/Program Files/Downloader/downloader.exe\" --tray\n"));
    }

    #[test]
    fn launch_agent_escapes_the_path_and_runs_at_load() {
        let plist = launch_agent(Path::new("/Apps/A&B/downloader"));
        assert!(plist.contains("<string>/Apps/A&amp;B/downloader</string><string>--tray</string>"));
        assert!(plist.contains("<key>RunAtLoad</key><true/>"));
    }

    #[test]
    fn write_entry_creates_replaces_and_removes_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("autostart/downloader.desktop");
        write_entry(&path, Some("one")).unwrap();
        write_entry(&path, Some("two")).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "two");
        write_entry(&path, None).unwrap();
        assert!(!path.exists());
        write_entry(&path, None).expect("removing a missing entry succeeds");
    }
}
