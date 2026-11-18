//! Output and exit-code helpers shared by dlx and dlxctl (included with #[path]).
#![allow(dead_code)]

use clap::ValueEnum;
use dlcore::task::ConflictPolicy;
use serde_json::{Value, json};

pub const OK: i32 = 0;
pub const FAILED: i32 = 1;
pub const USAGE: i32 = 2;
pub const NETWORK: i32 = 3;
pub const AUTH: i32 = 4;
pub const VERIFY: i32 = 5;
pub const CONFLICT: i32 = 6;
pub const MODULE: i32 = 7;
pub const INTERRUPTED: i32 = 130;

/// Exit status for an error class: util::error_code classes plus "interrupted" and
/// the CLI-only classes. Anything unlisted is a plain failure (1).
pub fn exit_code(class: &str) -> i32 {
    match class {
        "invalid" => USAGE,
        "network" => NETWORK,
        "auth" => AUTH,
        "verify" => VERIFY,
        "conflict" | "duplicate" => CONFLICT,
        "module" => MODULE,
        "interrupted" => INTERRUPTED,
        _ => FAILED,
    }
}

/// An error with a stable machine-readable class and its human text.
#[derive(Debug)]
pub struct CliError {
    pub code: String,
    pub message: String,
}

impl CliError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
        }
    }

    pub fn exit_code(&self) -> i32 {
        exit_code(&self.code)
    }
}

impl From<String> for CliError {
    fn from(message: String) -> Self {
        Self {
            code: dlcore::util::error_code(&message).to_string(),
            message,
        }
    }
}

impl From<&str> for CliError {
    fn from(message: &str) -> Self {
        message.to_string().into()
    }
}

/// --on-conflict values.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ConflictArg {
    Rename,
    Overwrite,
    Skip,
}

impl From<ConflictArg> for ConflictPolicy {
    fn from(value: ConflictArg) -> Self {
        match value {
            ConflictArg::Rename => ConflictPolicy::Rename,
            ConflictArg::Overwrite => ConflictPolicy::Overwrite,
            ConflictArg::Skip => ConflictPolicy::Skip,
        }
    }
}

/// One JSON document per line on stdout.
pub fn print_json(value: &Value) {
    println!("{value}");
}

pub fn envelope_ok(data: Value) -> Value {
    json!({"ok": true, "data": data})
}

pub fn envelope_err(error: &CliError) -> Value {
    json!({"ok": false, "error": {"code": error.code, "message": error.message}})
}

/// "Name: value" as given to --header.
pub fn parse_header(raw: &str) -> Result<(String, String), String> {
    let (name, value) = raw
        .split_once(':')
        .ok_or_else(|| format!("invalid header {raw:?}: use NAME: VALUE"))?;
    let name = name.trim();
    if name.is_empty() {
        return Err(format!("invalid header {raw:?}: the name is empty"));
    }
    Ok((name.to_string(), value.trim().to_string()))
}
