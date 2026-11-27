//! Machine-readable self-description shared by dlx and dlxctl (included with #[path]).
//! Everything about flags comes from clap introspection, so new flags appear automatically.
#![allow(dead_code)]

use crate::cli;
use clap::{Arg, ArgAction, Command};
use serde_json::{Value, json};

fn arg_schema(arg: &Arg) -> Value {
    let action = arg.get_action();
    let takes_value = action.takes_values();
    let multiple = matches!(action, ArgAction::Append)
        || arg
            .get_num_args()
            .is_some_and(|range| range.max_values() > 1);
    let defaults: Vec<String> = arg
        .get_default_values()
        .iter()
        .map(|v| v.to_string_lossy().into_owned())
        .collect();
    let possible: Vec<String> = arg
        .get_possible_values()
        .iter()
        .filter(|v| !v.is_hide_set())
        .map(|v| v.get_name().to_string())
        .collect();
    json!({
        "name": arg.get_id().as_str(),
        "long": arg.get_long(),
        "short": arg.get_short().map(String::from),
        "positional": arg.is_positional(),
        "value_name": arg.get_value_names().and_then(|n| n.first()).map(|n| n.to_string()),
        "takes_value": takes_value,
        "multiple": multiple,
        "default": if defaults.is_empty() { Value::Null } else { json!(defaults.join(",")) },
        "possible_values": possible,
        "help": arg.get_help().map(|h| h.to_string()),
        "required": arg.is_required_set(),
    })
}

fn visible_args(command: &Command, globals: bool) -> Vec<Value> {
    command
        .get_arguments()
        .filter(|a| {
            !a.is_hide_set()
                && !matches!(a.get_id().as_str(), "help" | "version")
                && a.is_global_set() == globals
        })
        .map(arg_schema)
        .collect()
}

/// The schema of one (built) command and, recursively, its subcommands.
pub fn command_schema(command: &Command, path: &str) -> Value {
    let subcommands: Vec<Value> = command
        .get_subcommands()
        .filter(|s| !s.is_hide_set() && s.get_name() != "help")
        .map(|s| command_schema(s, &format!("{path} {}", s.get_name())))
        .collect();
    let mut value = json!({
        "name": command.get_name(),
        "path": path,
        "about": command.get_about().map(|a| a.to_string()),
        "args": visible_args(command, false),
        "subcommands": subcommands,
    });
    if let Some(route) = remote_route(path) {
        value["remote"] = route;
    }
    value
}

/// The built root command (bin names and global flags propagated).
pub fn built(mut command: Command) -> Command {
    command.build();
    command
}

pub fn exit_codes() -> Value {
    json!([
        {"code": cli::OK, "class": "ok", "meaning": "success"},
        {"code": cli::FAILED, "class": "error", "meaning": "failed (HTTP status, disk, other)"},
        {"code": cli::USAGE, "class": "invalid", "meaning": "usage or invalid input"},
        {"code": cli::NETWORK, "class": "network", "meaning": "network failure, timeout or server unreachable"},
        {"code": cli::AUTH, "class": "auth", "meaning": "missing or wrong token"},
        {"code": cli::VERIFY, "class": "verify", "meaning": "integrity verification failed"},
        {"code": cli::CONFLICT, "class": "conflict", "meaning": "file exists (skip) or duplicate download"},
        {"code": cli::MODULE, "class": "module", "meaning": "module error"},
        {"code": cli::INTERRUPTED, "class": "interrupted", "meaning": "interrupted (Ctrl+C); run again to resume"},
    ])
}

pub fn error_classes() -> Value {
    let classes = [
        "invalid",
        "network",
        "auth",
        "verify",
        "conflict",
        "duplicate",
        "module",
        "interrupted",
        "http",
        "disk",
        "timeout",
        "doctor",
        "error",
    ];
    let rows: Vec<Value> = classes
        .iter()
        .map(|class| json!({"class": class, "exit_code": cli::exit_code(class)}))
        .collect();
    json!(rows)
}

pub fn env_vars() -> Value {
    json!([
        {"name": "DOWNLOADER_STATE_DIR", "meaning": "state directory (settings.json, modules/, queue)"},
        {"name": "DOWNLOADER_REMOTE_TOKEN", "meaning": "bearer token for the remote control API; never printed"},
    ])
}

/// Protocol engines, as printed by `dlx --tools --json`.
pub fn capabilities() -> Value {
    let list: Vec<Value> = dlcore::protocols::capabilities()
        .iter()
        .map(|tool| json!({"name": tool.name, "available": tool.available}))
        .collect();
    json!(list)
}

/// The remote HTTP call a dlxctl command makes, when it makes exactly one.
pub fn remote_route(path: &str) -> Option<Value> {
    let (method, route) = match path.strip_prefix("dlxctl ")? {
        "list" => ("GET", "/downloads"),
        "status" => ("GET", "/downloads/{id}"),
        "wait" => ("GET", "/downloads/{id} (polled every second)"),
        "add" => ("POST", "/downloads"),
        "start" => ("POST", "/downloads/{id}/start"),
        "pause" => ("POST", "/downloads/{id}/pause"),
        "remove" => ("DELETE", "/downloads/{id}"),
        "start-all" => ("POST", "/start-all"),
        "pause-all" => ("POST", "/pause-all"),
        "export" => ("GET", "/export"),
        "import" => ("POST", "/import?start={bool}"),
        _ => return None,
    };
    Some(json!({"method": method, "path": route}))
}

pub fn remote_routes() -> Value {
    json!([
        {"method": "GET", "path": "/downloads"},
        {"method": "POST", "path": "/downloads"},
        {"method": "GET", "path": "/downloads/{id}"},
        {"method": "DELETE", "path": "/downloads/{id}"},
        {"method": "POST", "path": "/downloads/{id}/start"},
        {"method": "POST", "path": "/downloads/{id}/pause"},
        {"method": "POST", "path": "/start-all"},
        {"method": "POST", "path": "/pause-all"},
        {"method": "GET", "path": "/export"},
        {"method": "POST", "path": "/import"},
        {"method": "GET", "path": "/modules"},
        {"method": "GET", "path": "/version"},
    ])
}

/// dlx --json events, with the fields each carries.
pub fn ndjson_events() -> Value {
    json!({
        "tools": ["tools"],
        "start": ["url", "resumed_bytes"],
        "progress": ["url", "downloaded", "total", "bytes_per_sec", "connections"],
        "done": ["url", "path", "size", "seconds", "verification"],
        "error": ["url (absent for usage errors)", "code", "message"],
        "summary": ["completed", "failed", "exit_code"],
    })
}

pub fn envelope() -> Value {
    json!({
        "success": {"ok": true, "data": "<command specific>"},
        "failure": {"ok": false, "error": {"code": "<error class>", "message": "<text>"}},
        "note": "dlxctl --json prints one such line on stdout; the process exit code follows exit_codes",
    })
}

/// Schema of `dlx`: one JSON object.
pub fn dlx_schema(command: Command) -> Value {
    let command = built(command);
    json!({
        "name": command.get_name(),
        "version": env!("CARGO_PKG_VERSION"),
        "about": command.get_about().map(|a| a.to_string()),
        "args": visible_args(&command, false)
            .into_iter()
            .chain(visible_args(&command, true))
            .collect::<Vec<_>>(),
        "exit_codes": exit_codes(),
        "error_classes": error_classes(),
        "ndjson_events": ndjson_events(),
        "env": env_vars(),
        "capabilities": capabilities(),
    })
}

/// Schema of a dlxctl command: the whole tool for the root, else that command's subtree.
pub fn dlxctl_schema(root: Command, names: &[String]) -> Result<Value, String> {
    let root = built(root);
    let target = find(&root, names)?;
    if !names.is_empty() {
        let path = format!("{} {}", root.get_name(), names.join(" "));
        return Ok(command_schema(target, &path));
    }
    let name = root.get_name().to_string();
    Ok(json!({
        "name": name,
        "version": env!("CARGO_PKG_VERSION"),
        "about": root.get_about().map(|a| a.to_string()),
        "global_options": visible_args(&root, true)
            .into_iter()
            .chain(visible_args(&root, false))
            .collect::<Vec<_>>(),
        "commands": root
            .get_subcommands()
            .filter(|s| !s.is_hide_set() && s.get_name() != "help")
            .map(|s| command_schema(s, &format!("{name} {}", s.get_name())))
            .collect::<Vec<_>>(),
        "envelope": envelope(),
        "exit_codes": exit_codes(),
        "error_classes": error_classes(),
        "remote_routes": remote_routes(),
        "env": env_vars(),
        "capabilities": capabilities(),
    }))
}

/// Walks `names` down from the root; `Err` names the first unknown command.
pub fn find<'a>(root: &'a Command, names: &[String]) -> Result<&'a Command, String> {
    let mut current = root;
    for name in names {
        current = current
            .find_subcommand(name)
            .ok_or_else(|| format!("unknown command {name:?}; run \"dlxctl help\""))?;
    }
    Ok(current)
}
