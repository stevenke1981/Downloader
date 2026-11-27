//! Agent discovery and diagnosis: dlxctl doctor, dlx --schema, dlxctl help --json.
use serde_json::Value;
use std::path::Path;
use std::process::{Command, Stdio};

const TOKEN: &str = "doctor-secret-token-9f8e7d6c";

struct Out {
    code: i32,
    stdout: String,
}

fn run(bin: &str, state: &Path, args: &[&str]) -> Out {
    let out = Command::new(bin)
        .args(args)
        .env("DOWNLOADER_STATE_DIR", state)
        .env("DOWNLOADER_REMOTE_TOKEN", TOKEN)
        .stdin(Stdio::null())
        .output()
        .expect("spawn");
    Out {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

fn dlxctl(state: &Path, args: &[&str]) -> Out {
    run(env!("CARGO_BIN_EXE_dlxctl"), state, args)
}

fn names(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .filter_map(|a| a["long"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn doctor_json_has_checks_and_never_prints_the_token() {
    let state = tempfile::tempdir().unwrap();
    let out = dlxctl(
        state.path(),
        &["--json", "--endpoint", "http://127.0.0.1:9", "doctor"],
    );
    assert!(!out.stdout.contains(TOKEN));
    let envelope: Value = serde_json::from_str(out.stdout.trim()).expect("one JSON line");
    let checks = envelope["data"]["checks"].as_array().expect("checks");
    let ids: Vec<&str> = checks.iter().map(|c| c["id"].as_str().unwrap()).collect();
    for id in [
        "state-dir",
        "settings",
        "download-dir",
        "disk-space",
        "token",
        "remote",
        "modules",
        "native-host",
        "binaries",
    ] {
        assert!(ids.contains(&id), "missing check {id}: {ids:?}");
    }
    assert!(!ids.contains(&"net-youtube"), "online checks need --online");
    let token = checks.iter().find(|c| c["id"] == "token").unwrap();
    assert_eq!(token["status"], "ok");
    let remote = checks.iter().find(|c| c["id"] == "remote").unwrap();
    assert_eq!(remote["status"], "warn", "nothing listens on port 9");
    let summary = &envelope["data"]["summary"];
    let total: u64 = ["ok", "warn", "fail"]
        .iter()
        .map(|k| summary[k].as_u64().unwrap())
        .sum();
    assert_eq!(total as usize, checks.len());
    if summary["fail"] == 0 {
        assert_eq!(out.code, 0);
        assert_eq!(envelope["ok"], true);
    } else {
        assert_eq!(out.code, 1);
        assert_eq!(envelope["ok"], false);
        assert_eq!(envelope["error"]["code"], "doctor");
    }
}

#[test]
fn doctor_fails_on_broken_settings() {
    let state = tempfile::tempdir().unwrap();
    std::fs::write(state.path().join("settings.json"), "{ not json").unwrap();
    let out = dlxctl(state.path(), &["--json", "doctor"]);
    assert_eq!(out.code, 1);
    let envelope: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "doctor");
    assert!(envelope["data"]["summary"]["fail"].as_u64().unwrap() >= 1);
    assert!(!out.stdout.contains(TOKEN));
}

#[test]
fn doctor_human_has_status_lines() {
    let state = tempfile::tempdir().unwrap();
    let out = dlxctl(state.path(), &["doctor"]);
    assert!(
        out.stdout
            .lines()
            .any(|l| l.starts_with("[ok]") && l.contains("settings"))
    );
    assert!(!out.stdout.contains(TOKEN));
}

#[test]
fn dlx_schema_lists_flags_and_codes() {
    let state = tempfile::tempdir().unwrap();
    let out = run(env!("CARGO_BIN_EXE_dlx"), state.path(), &["--schema"]);
    assert_eq!(out.code, 0);
    let schema: Value = serde_json::from_str(out.stdout.trim()).expect("one JSON object");
    assert_eq!(schema["name"], "dlx");
    let longs = names(&schema["args"]);
    for flag in ["json", "mirror", "header", "on-conflict", "schema", "tools"] {
        assert!(longs.contains(&flag.to_string()), "missing --{flag}");
    }
    let conflict = schema["args"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["long"] == "on-conflict")
        .unwrap();
    assert_eq!(conflict["possible_values"].as_array().unwrap().len(), 3);
    assert_eq!(conflict["default"], "rename");
    assert!(schema["exit_codes"].as_array().unwrap().len() >= 9);
    assert!(schema["ndjson_events"]["summary"].is_array());
    assert!(!schema["capabilities"].as_array().unwrap().is_empty());
}

#[test]
fn dlxctl_help_json_describes_every_command() {
    let state = tempfile::tempdir().unwrap();
    let out = dlxctl(state.path(), &["help", "--json"]);
    assert_eq!(out.code, 0);
    let envelope: Value = serde_json::from_str(out.stdout.trim()).unwrap();
    assert_eq!(envelope["ok"], true);
    let data = &envelope["data"];
    let commands = data["commands"].as_array().unwrap();
    let find = |name: &str| commands.iter().find(|c| c["name"] == name);
    for name in ["add", "wait", "doctor", "schema", "modules", "list"] {
        assert!(find(name).is_some(), "missing command {name}");
    }
    assert!(names(&find("add").unwrap()["args"]).contains(&"mirror".to_string()));
    assert_eq!(find("add").unwrap()["remote"]["path"], "/downloads");
    let nested = &find("modules").unwrap()["subcommands"];
    assert!(
        nested
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c["name"] == "approve")
    );
    assert!(names(&data["global_options"]).contains(&"json".to_string()));
    assert!(data["exit_codes"].is_array() && data["remote_routes"].is_array());
    let single = dlxctl(state.path(), &["help", "add", "--json"]);
    let one: Value = serde_json::from_str(single.stdout.trim()).unwrap();
    assert_eq!(one["data"]["name"], "add");
    assert_eq!(dlxctl(state.path(), &["help", "nope"]).code, 2);
    let schema = dlxctl(state.path(), &["schema"]);
    assert!(serde_json::from_str::<Value>(schema.stdout.trim()).is_ok());
}

#[test]
fn dlxctl_help_command_matches_clap_help() {
    let state = tempfile::tempdir().unwrap();
    let a = dlxctl(state.path(), &["help", "add"]);
    let b = dlxctl(state.path(), &["add", "--help"]);
    assert_eq!(a.code, 0);
    assert_eq!(a.stdout, b.stdout);
    let a = dlxctl(state.path(), &["help", "modules", "approve"]);
    let b = dlxctl(state.path(), &["modules", "approve", "--help"]);
    assert_eq!(a.stdout, b.stdout);
}
