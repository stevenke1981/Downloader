//! Settings file helpers for dlxctl. Edits apply when the manager next starts.
use crate::cli::CliError;
use crate::output;
use dlcore::Settings;
use serde_json::Value;
use std::path::Path;

pub fn read_settings(dir: &Path) -> Result<Settings, String> {
    match std::fs::read(dir.join("settings.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Settings::default()),
        Err(error) => Err(error.to_string()),
    }
}

pub fn save_settings(dir: &Path, settings: &Settings) -> Result<(), CliError> {
    use std::io::Write;
    settings.validate()?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let json = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    let temporary = dir.join("settings.json.tmp");
    let mut file = std::fs::File::create(&temporary).map_err(|e| e.to_string())?;
    file.write_all(&json)
        .and_then(|_| file.sync_all())
        .map_err(|e| e.to_string())?;
    drop(file);
    std::fs::rename(&temporary, dir.join("settings.json")).map_err(|e| e.to_string())?;
    Ok(())
}

/// Confirms a settings edit: the human line, or the JSON data.
pub fn saved(json: bool, human: &str, data: Value) {
    output(json, human, data);
}

pub const APPLIES: &str = "Settings changes apply when the manager next starts.";

pub const APPLIES_SAVED: &str = "Saved. Settings changes apply when the manager next starts.";
