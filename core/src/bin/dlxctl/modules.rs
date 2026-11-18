//! `dlxctl modules`: approval is the SHA-256 of a module's module.json, stored in settings.json.
use crate::cli::CliError;
use crate::print_envelope;
use crate::settings::{APPLIES, read_settings, save_settings, saved};
use clap::Subcommand;
use dlcore::modules::{ManifestSummary, Registry, inspect_manifest, valid_id};
use serde_json::json;
use std::path::Path;

#[derive(Subcommand)]
pub enum ModuleCommand {
    /// Modules in the modules folder with their approval state.
    List,
    /// Approve the module's current module.json (stores its SHA-256). Any later edit disables it.
    Approve {
        id: String,
        /// Approve only if module.json has this SHA-256 (as shown by "modules list")
        #[arg(long, value_name = "HEX")]
        sha256: Option<String>,
    },
    /// Remove the approval.
    Revoke {
        id: String,
    },
    /// Allow approved external modules to run.
    Enable,
    Disable,
}

/// The validated manifest (size-capped, same checks as the app) the user is about to approve.
fn inspect(
    modules_dir: &Path,
    id: &str,
    expected: Option<&str>,
) -> Result<ManifestSummary, CliError> {
    if !valid_id(id) {
        return Err(CliError::new(
            "invalid",
            format!("invalid module id {id:?}: use 1–40 characters a-z, 0-9 or -"),
        ));
    }
    let summary = inspect_manifest(modules_dir, id).map_err(|e| CliError::new("module", e))?;
    if let Some(expected) = expected
        && !expected.trim().eq_ignore_ascii_case(&summary.sha256)
    {
        return Err(CliError::new(
            "module",
            format!(
                "module {id}: module.json has sha256 {}, not the expected {}; not approved",
                summary.sha256,
                expected.trim()
            ),
        ));
    }
    Ok(summary)
}

pub fn modules_command(dir: &Path, json: bool, command: ModuleCommand) -> Result<(), CliError> {
    let modules_dir = dir.join("modules");
    let mut settings = read_settings(dir)?;
    match command {
        ModuleCommand::List => {
            let list = Registry::load(&settings.modules, &modules_dir).list();
            if json {
                print_envelope(serde_json::to_value(&list).map_err(|e| e.to_string())?);
            } else {
                println!(
                    "modules {}",
                    if settings.modules.enabled {
                        "enabled"
                    } else {
                        "disabled"
                    }
                );
                if list.is_empty() {
                    println!("no modules in {}", modules_dir.display());
                }
                for module in &list {
                    println!(
                        "{}  {}  {}  {}",
                        module.id,
                        module.version,
                        if module.approved {
                            "approved"
                        } else {
                            "not approved"
                        },
                        module.kinds.join(",")
                    );
                    if !module.manifest_sha256.is_empty() {
                        println!("    sha256: {}", module.manifest_sha256);
                    }
                    if let Some(error) = &module.error {
                        println!("    error: {error}");
                    }
                }
            }
        }
        ModuleCommand::Approve { id, sha256 } => {
            let m = inspect(&modules_dir, &id, sha256.as_deref())?;
            settings
                .modules
                .approved
                .insert(id.clone(), m.sha256.clone());
            save_settings(dir, &settings)?;
            saved(
                json,
                &format!(
                    "Approved module {id} {} (sha256 {}).\n  runs: {} {}\n  command: {:?}\n{APPLIES}",
                    m.version,
                    m.sha256,
                    m.program.display(),
                    m.command[1..].join(" "),
                    m.command,
                ),
                json!({
                    "module": id,
                    "approved": true,
                    "sha256": m.sha256,
                    "version": m.version,
                    "kinds": m.kinds,
                    "command": m.command,
                    "program": m.program.display().to_string(),
                }),
            );
        }
        ModuleCommand::Revoke { id } => {
            settings.modules.approved.remove(&id);
            save_settings(dir, &settings)?;
            saved(
                json,
                &format!("Revoked module {id}. {APPLIES}"),
                json!({"module": id, "approved": false}),
            );
        }
        ModuleCommand::Enable | ModuleCommand::Disable => {
            settings.modules.enabled = matches!(command, ModuleCommand::Enable);
            save_settings(dir, &settings)?;
            let state = if settings.modules.enabled {
                "enabled"
            } else {
                "disabled"
            };
            saved(
                json,
                &format!("Modules {state}. {APPLIES}"),
                json!({"enabled": settings.modules.enabled}),
            );
        }
    }
    Ok(())
}
