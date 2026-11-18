//! `dlxctl modules`: approval is the SHA-256 of a module's module.json, stored in settings.json.
use crate::cli::CliError;
use crate::print_envelope;
use crate::settings::{APPLIES, read_settings, save_settings, saved};
use clap::Subcommand;
use dlcore::modules::Registry;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::path::Path;

#[derive(Subcommand)]
pub enum ModuleCommand {
    /// Modules in the modules folder with their approval state.
    List,
    /// Approve the module's current module.json (stores its SHA-256). Any later edit disables it.
    Approve {
        id: String,
    },
    /// Remove the approval.
    Revoke {
        id: String,
    },
    /// Allow approved external modules to run.
    Enable,
    Disable,
}

fn valid_module_id(id: &str) -> bool {
    (1..=40).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// SHA-256 (lowercase hex) of the module's module.json bytes, as stored in settings.
fn module_hash(modules_dir: &Path, id: &str) -> Result<String, CliError> {
    if !valid_module_id(id) {
        return Err(CliError::new(
            "invalid",
            format!("invalid module id {id:?}: use 1–40 characters a-z, 0-9 or -"),
        ));
    }
    let path = modules_dir.join(id).join("module.json");
    let bytes = std::fs::read(&path).map_err(|e| {
        CliError::new(
            "module",
            format!("module {id}: cannot read {}: {e}", path.display()),
        )
    })?;
    Ok(hex::encode(Sha256::digest(&bytes)))
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
                    if let Some(error) = &module.error {
                        println!("    error: {error}");
                    }
                }
            }
        }
        ModuleCommand::Approve { id } => {
            let hash = module_hash(&modules_dir, &id)?;
            settings.modules.approved.insert(id.clone(), hash.clone());
            save_settings(dir, &settings)?;
            saved(
                json,
                &format!("Approved module {id} (sha256 {hash}). {APPLIES}"),
                json!({"module": id, "approved": true, "sha256": hash}),
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
