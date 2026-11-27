//! End-to-end checks for the agent-facing CLIs: NDJSON from dlx, the dlxctl JSON envelope and exit codes.
mod server;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, BufReader};

const TOKEN: &str = "test-token-0123456789abcdef";

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

impl Run {
    /// Every stdout line must be one JSON document; returns them in order.
    fn events(&self) -> Vec<Value> {
        self.stdout
            .lines()
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|e| panic!("stdout line is not JSON ({e}): {line}"))
            })
            .collect()
    }
    fn envelope(&self) -> Value {
        let lines = self.events();
        assert_eq!(
            lines.len(),
            1,
            "expected one JSON line, got: {}",
            self.stdout
        );
        lines.into_iter().next().unwrap()
    }
}

async fn finish(mut command: tokio::process::Command) -> Run {
    let out = command.output().await.expect("spawn");
    Run {
        code: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

/// dlx with an isolated state directory, so no user settings or resume files are touched.
fn dlx(state: &Path, args: &[&str]) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_dlx"));
    command
        .args(args)
        .env("DOWNLOADER_STATE_DIR", state)
        .stdin(Stdio::null());
    command
}

fn dlxctl(state: &Path, args: &[&str]) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_dlxctl"));
    command
        .args(args)
        .env("DOWNLOADER_STATE_DIR", state)
        .env_remove("DOWNLOADER_REMOTE_TOKEN")
        .stdin(Stdio::null());
    command
}

fn event<'a>(events: &'a [Value], name: &str) -> Vec<&'a Value> {
    events.iter().filter(|e| e["event"] == name).collect()
}

#[tokio::test]
async fn dlx_json_is_ndjson_and_completes() {
    let data = server::test_data(256 << 10);
    let s = server::start(data.clone(), true, 0).await;
    let state = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let url = s.url("sample.bin");
    let run = finish(dlx(
        state.path(),
        &["--json", "-o", out.path().to_str().unwrap(), &url],
    ))
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    let events = run.events();
    assert_eq!(events.first().unwrap()["event"], "start");
    let done = event(&events, "done");
    assert_eq!(done.len(), 1);
    assert_eq!(done[0]["size"], json!(data.len()));
    assert!(done[0]["verification"].is_null() || done[0]["verification"].is_object());
    let summary = events.last().unwrap();
    assert_eq!(summary["event"], "summary");
    assert_eq!(summary["completed"], 1);
    assert_eq!(summary["exit_code"], 0);
    assert_eq!(std::fs::read(out.path().join("sample.bin")).unwrap(), data);
}

#[tokio::test]
async fn dlx_checksum_mismatch_exits_5() {
    let data = server::test_data(64 << 10);
    let s = server::start(data, true, 0).await;
    let state = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let url = s.url("wrong.bin");
    let wrong = "0".repeat(64);
    let run = finish(dlx(
        state.path(),
        &[
            "--json",
            "-q",
            "--verify",
            &wrong,
            "-o",
            out.path().to_str().unwrap(),
            &url,
        ],
    ))
    .await;
    assert_eq!(run.code, 5, "stdout: {}", run.stdout);
    let events = run.events();
    let errors = event(&events, "error");
    assert_eq!(errors.len(), 1);
    assert_eq!(errors[0]["code"], "verify");
    assert_eq!(events.last().unwrap()["exit_code"], 5);
}

#[tokio::test]
async fn dlx_skip_existing_file_exits_6() {
    let data = server::test_data(64 << 10);
    let s = server::start(data, true, 0).await;
    let state = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    std::fs::write(out.path().join("kept.bin"), b"old").unwrap();
    let url = s.url("kept.bin");
    let run = finish(dlx(
        state.path(),
        &[
            "--json",
            "-q",
            "--on-conflict",
            "skip",
            "-o",
            out.path().to_str().unwrap(),
            &url,
        ],
    ))
    .await;
    assert_eq!(run.code, 6, "stdout: {}", run.stdout);
    assert_eq!(event(&run.events(), "error")[0]["code"], "conflict");
    assert_eq!(std::fs::read(out.path().join("kept.bin")).unwrap(), b"old");
}

#[tokio::test]
async fn dlx_usage_errors_exit_2_and_report_json() {
    let state = tempfile::tempdir().unwrap();
    let bad_verify = finish(dlx(
        state.path(),
        &[
            "--json",
            "--verify",
            "not-a-digest",
            "https://example.com/a.bin",
        ],
    ))
    .await;
    assert_eq!(bad_verify.code, 2);
    assert_eq!(event(&bad_verify.events(), "error")[0]["code"], "invalid");

    let cookie = finish(dlx(
        state.path(),
        &[
            "--json",
            "--header",
            "Cookie: a=b",
            "https://example.com/a.bin",
        ],
    ))
    .await;
    assert_eq!(cookie.code, 2);
    assert_eq!(event(&cookie.events(), "error")[0]["code"], "invalid");
}

#[tokio::test]
async fn audio_flag_conflicts_with_video() {
    let state = tempfile::tempdir().unwrap();
    let both = finish(dlx(
        state.path(),
        &["--json", "--audio", "--video", "https://example.com/a"],
    ))
    .await;
    assert_eq!(both.code, 2);
    let ctl = finish(dlxctl(
        state.path(),
        &["add", "--audio", "--video", "https://example.com/a"],
    ))
    .await;
    assert_eq!(ctl.code, 2);
    // A non-YouTube link fails as a usage error before any request (port 9 would be
    // a network error, exit 3).
    let out = state.path().join("out");
    let other = finish(dlx(
        state.path(),
        &[
            "--audio",
            "-o",
            out.to_str().unwrap(),
            "http://127.0.0.1:9/x",
        ],
    ))
    .await;
    assert_eq!(other.code, 2, "{}", other.stderr);
    assert!(
        other
            .stderr
            .contains("audio download supports YouTube links only")
    );
    // --audio alone parses: help lists it on both binaries.
    let help = finish(dlx(state.path(), &["--help"])).await;
    assert!(help.stdout.contains("--audio"));
    let help = finish(dlxctl(state.path(), &["add", "--help"])).await;
    assert!(help.stdout.contains("--audio"));
}

#[tokio::test]
async fn dlx_tools_json_is_one_document() {
    let state = tempfile::tempdir().unwrap();
    let run = finish(dlx(state.path(), &["--tools", "--json"])).await;
    assert_eq!(run.code, 0);
    let events = run.events();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["event"], "tools");
    assert!(events[0]["tools"].as_array().is_some_and(|t| !t.is_empty()));
}

#[tokio::test]
async fn dlxctl_without_token_exits_4_with_json_error() {
    let state = tempfile::tempdir().unwrap();
    let run = finish(dlxctl(
        state.path(),
        &["--json", "--endpoint", "http://127.0.0.1:9", "list"],
    ))
    .await;
    assert_eq!(run.code, 4);
    let envelope = run.envelope();
    assert_eq!(envelope["ok"], false);
    assert_eq!(envelope["error"]["code"], "auth");
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Starts "dlxctl serve" with --json and returns the child and its endpoint once it is listening.
async fn start_serve(state: &Path) -> (tokio::process::Child, String) {
    let bind = format!("127.0.0.1:{}", free_port());
    let endpoint = format!("http://{bind}");
    let mut serve = dlxctl(
        state,
        &[
            "--json",
            "--data-dir",
            state.to_str().unwrap(),
            "serve",
            "--bind",
            &bind,
        ],
    )
    .env("DOWNLOADER_REMOTE_TOKEN", TOKEN)
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)
    .spawn()
    .expect("spawn serve");
    let mut stdout = BufReader::new(serve.stdout.take().unwrap());
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(30), stdout.read_line(&mut line))
        .await
        .expect("serve did not report its address")
        .unwrap();
    let ready: Value = serde_json::from_str(&line).expect("serve JSON line");
    assert_eq!(ready["ok"], true);
    assert_eq!(ready["data"]["listening"], bind);
    (serve, endpoint)
}

#[tokio::test]
async fn dlxctl_json_list_against_serve() {
    let state = tempfile::tempdir().unwrap();
    let (mut serve, endpoint) = start_serve(state.path()).await;
    let list = finish(dlxctl(
        state.path(),
        &["--json", "--endpoint", &endpoint, "--token", TOKEN, "list"],
    ))
    .await;
    assert_eq!(list.code, 0, "stderr: {}", list.stderr);
    let envelope = list.envelope();
    assert_eq!(envelope["ok"], true);
    assert!(envelope["data"].as_array().is_some_and(|d| d.is_empty()));

    let wrong = finish(dlxctl(
        state.path(),
        &[
            "--json",
            "--endpoint",
            &endpoint,
            "--token",
            "wrong-token-0123456789",
            "list",
        ],
    ))
    .await;
    assert_eq!(wrong.code, 4);
    assert_eq!(wrong.envelope()["error"]["code"], "auth");
    let _ = serve.start_kill();
    let _ = serve.wait().await;
}

#[tokio::test]
async fn dlxctl_queue_routes_add_wait_duplicate_export_import() {
    let data = server::test_data(128 << 10);
    let s = server::start(data.clone(), true, 0).await;
    let state = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let url = s.url("queued.bin");
    let (mut serve, endpoint) = start_serve(state.path()).await;
    let auth = ["--endpoint", endpoint.as_str(), "--token", TOKEN];
    let dir = out.path().to_str().unwrap();
    let call = |args: &[&str]| dlxctl(state.path(), &[&["--json"], &auth[..], args].concat());

    // Paused, so the duplicate check and export see it as unfinished.
    let add = finish(call(&["add", &url, "--dir", dir, "--unique", "--paused"])).await;
    assert_eq!(add.code, 0, "stderr: {}", add.stderr);
    let id = add.envelope()["data"]["id"].as_u64().expect("id");
    let id_text = id.to_string();

    let duplicate = finish(call(&["add", &url, "--dir", dir, "--unique"])).await;
    assert_eq!(duplicate.code, 6, "stdout: {}", duplicate.stdout);
    assert_eq!(duplicate.envelope()["error"]["code"], "duplicate");

    let bad_checksum = finish(call(&["add", &url, "--checksum", "nope"])).await;
    assert_eq!(bad_checksum.code, 2);

    let export = finish(call(&["export"])).await;
    assert_eq!(export.code, 0, "stderr: {}", export.stderr);
    let queue = export.envelope()["data"].clone();
    assert!(queue["downloads"].as_array().is_some_and(|d| !d.is_empty()));
    let file = state.path().join("queue.json");
    std::fs::write(&file, serde_json::to_vec(&queue).unwrap()).unwrap();
    let import = finish(call(&["import", file.to_str().unwrap()])).await;
    assert_eq!(import.code, 0, "stderr: {}", import.stderr);
    assert!(import.envelope()["data"]["added"].is_array());

    let start = finish(call(&["start", &id_text])).await;
    assert_eq!(start.code, 0, "stderr: {}", start.stderr);
    let wait = finish(call(&["wait", &id_text, "--timeout", "60"])).await;
    assert_eq!(wait.code, 0, "stderr: {}", wait.stderr);
    assert_eq!(wait.envelope()["data"][0]["status"], "Completed");

    let status = finish(call(&["status", &id_text])).await;
    assert_eq!(status.code, 0);
    assert_eq!(status.envelope()["data"]["status"], "Completed");
    assert_eq!(std::fs::read(out.path().join("queued.bin")).unwrap(), data);

    let _ = serve.start_kill();
    let _ = serve.wait().await;
}

#[tokio::test]
async fn dlxctl_modules_approve_records_manifest_hash() {
    let state = tempfile::tempdir().unwrap();
    let dir = state.path().to_str().unwrap();
    let module = state.path().join("modules").join("example-resolver");
    std::fs::create_dir_all(&module).unwrap();
    let manifest = br#"{"schema":1,"id":"example-resolver","name":"Example","version":"1.0.0","kinds":["resolver"],"match":{"schemes":["https"],"hosts":["example.com"]},"command":["example-resolver","--stdio"]}"#;
    std::fs::write(module.join("module.json"), manifest).unwrap();
    // Approval validates the manifest, so command[0] must exist inside the folder.
    std::fs::write(module.join("example-resolver"), b"").unwrap();
    let expected = hex::encode(Sha256::digest(manifest));

    let approve = finish(dlxctl(
        state.path(),
        &[
            "--json",
            "--data-dir",
            dir,
            "modules",
            "approve",
            "example-resolver",
        ],
    ))
    .await;
    assert_eq!(approve.code, 0, "stderr: {}", approve.stderr);
    assert_eq!(approve.envelope()["data"]["sha256"], expected.as_str());
    let settings: Value =
        serde_json::from_slice(&std::fs::read(state.path().join("settings.json")).unwrap())
            .unwrap();
    assert_eq!(
        settings["modules"]["approved"]["example-resolver"],
        expected.as_str()
    );

    let list = finish(dlxctl(
        state.path(),
        &["--json", "--data-dir", dir, "modules", "list"],
    ))
    .await;
    assert_eq!(list.code, 0);
    assert_eq!(list.envelope()["ok"], true);

    let missing = finish(dlxctl(
        state.path(),
        &[
            "--json",
            "--data-dir",
            dir,
            "modules",
            "approve",
            "no-such-module",
        ],
    ))
    .await;
    assert_eq!(missing.code, 7);
    assert_eq!(missing.envelope()["error"]["code"], "module");

    let invalid = finish(dlxctl(
        state.path(),
        &["--json", "--data-dir", dir, "modules", "approve", "Bad_Id"],
    ))
    .await;
    assert_eq!(invalid.code, 2);
}

#[tokio::test]
async fn dlx_modules_dir_is_opt_in_and_never_breaks_plain_downloads() {
    let data = server::test_data(64 << 10);
    let s = server::start(data.clone(), true, 0).await;
    let state = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    // A resolver for example.com that is NOT approved, plus the fixture host, which no module matches.
    let module = state.path().join("modules").join("example-resolver");
    std::fs::create_dir_all(&module).unwrap();
    std::fs::write(
        module.join("module.json"),
        br#"{"schema":1,"id":"example-resolver","name":"Example","version":"1.0.0","kinds":["resolver"],"match":{"schemes":["https"],"hosts":["example.com"]},"command":["example-resolver","--stdio"]}"#,
    )
    .unwrap();
    let url = s.url("plain.bin");
    let run = finish(dlx(
        state.path(),
        &[
            "--json",
            "-q",
            "--modules-dir",
            state.path().join("modules").to_str().unwrap(),
            "-o",
            out.path().to_str().unwrap(),
            &url,
        ],
    ))
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.events().last().unwrap()["exit_code"], 0);
    assert_eq!(std::fs::read(out.path().join("plain.bin")).unwrap(), data);
}

/// A resolver for the fixture host that always fails, so whether it ran shows in the exit code.
fn failing_resolver(modules: &Path) -> String {
    let module = modules.join("res");
    std::fs::create_dir_all(&module).unwrap();
    let manifest = json!({
        "schema": 1, "id": "res", "name": "Res", "version": "1.0.0", "kinds": ["resolver"],
        "match": {"schemes": ["http"], "hosts": ["127.0.0.1"]},
        "command": [env!("CARGO_BIN_EXE_dlx-module-example"), "--fail", "boom"]
    });
    std::fs::write(module.join("module.json"), manifest.to_string()).unwrap();
    dlcore::modules::manifest_hash(modules, "res").unwrap()
}

#[tokio::test]
async fn dlx_modules_honour_the_enable_switch_and_only_the_state_folder() {
    let data = server::test_data(16 << 10);
    let s = server::start(data.clone(), true, 0).await;
    let state = tempfile::tempdir().unwrap();
    let modules = state.path().join("modules");
    let hash = failing_resolver(&modules);
    let url = s.url("m.bin");
    let run = |enabled: bool, dir: &Path| {
        let settings = json!({"modules": {"enabled": enabled, "approved": {"res": hash}}});
        std::fs::write(state.path().join("settings.json"), settings.to_string()).unwrap();
        let out = tempfile::tempdir().unwrap();
        let command = dlx(
            state.path(),
            &[
                "--json",
                "-q",
                "--modules-dir",
                dir.to_str().unwrap(),
                "-o",
                out.path().to_str().unwrap(),
                &url,
            ],
        );
        async move { (finish(command).await, out) }
    };
    // Approved but modules switched off: the resolver must not run.
    let (off, out) = run(false, &modules).await;
    assert_eq!(off.code, 0, "stderr: {}", off.stderr);
    assert_eq!(std::fs::read(out.path().join("m.bin")).unwrap(), data);
    // Switched on: it runs (and fails the download with a module error).
    let (on, _out) = run(true, &modules).await;
    assert_eq!(on.code, 7, "stdout: {}", on.stdout);
    assert!(
        event(&on.events(), "error")
            .iter()
            .any(|e| e["code"] == "module"),
        "{}",
        on.stdout
    );
    // The same approved module copied elsewhere is refused: approvals bind to the state folder.
    let elsewhere = tempfile::tempdir().unwrap();
    failing_resolver(elsewhere.path());
    let (other, _out) = run(true, elsewhere.path()).await;
    assert_eq!(other.code, 2, "stdout: {}", other.stdout);
}

#[tokio::test]
async fn dlxctl_modules_approve_validates_and_shows_the_command() {
    let state = tempfile::tempdir().unwrap();
    let dir = state.path().to_str().unwrap();
    let modules = state.path().join("modules");
    let hash = failing_resolver(&modules);
    let approve = |extra: &[&str]| {
        let mut args = vec!["--json", "--data-dir", dir, "modules", "approve", "res"];
        args.extend_from_slice(extra);
        finish(dlxctl(state.path(), &args))
    };
    let wrong = approve(&["--sha256", &"0".repeat(64)]).await;
    assert_eq!(wrong.code, 7, "stdout: {}", wrong.stdout);
    assert!(!state.path().join("settings.json").exists());

    let ok = approve(&["--sha256", &hash.to_uppercase()]).await;
    assert_eq!(ok.code, 0, "stderr: {}", ok.stderr);
    let data = &ok.envelope()["data"];
    assert_eq!(data["sha256"], hash.as_str());
    assert_eq!(data["command"][1], "--fail");
    assert!(
        data["program"]
            .as_str()
            .unwrap()
            .contains("dlx-module-example")
    );

    let human = finish(dlxctl(
        state.path(),
        &["--data-dir", dir, "modules", "approve", "res"],
    ))
    .await;
    assert_eq!(human.code, 0);
    assert!(human.stdout.contains(&hash), "{}", human.stdout);
    assert!(human.stdout.contains("--fail boom"), "{}", human.stdout);

    // An invalid manifest cannot be approved (same validation as the app).
    std::fs::write(modules.join("res").join("module.json"), b"{not json").unwrap();
    let invalid = approve(&[]).await;
    assert_eq!(invalid.code, 7, "stdout: {}", invalid.stdout);
}

/// A hook module for completed and failed events that logs every request line to events.log.
fn hook_module(state: &Path) -> std::path::PathBuf {
    let modules = state.join("modules");
    let module = modules.join("hook");
    std::fs::create_dir_all(&module).unwrap();
    let manifest = json!({
        "schema": 1, "id": "hook", "name": "Hook", "version": "1.0.0", "kinds": ["hook"],
        "events": ["completed", "failed"], "match": {}, "timeout_ms": 10000,
        "command": [env!("CARGO_BIN_EXE_dlx-module-example"), "--log", "events.log"]
    });
    std::fs::write(module.join("module.json"), manifest.to_string()).unwrap();
    let hash = dlcore::modules::manifest_hash(&modules, "hook").unwrap();
    let settings = json!({"modules": {"enabled": true, "approved": {"hook": hash}}});
    std::fs::write(state.join("settings.json"), settings.to_string()).unwrap();
    module.join("events.log")
}

fn hook_lines(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[tokio::test]
async fn dlx_waits_for_hooks_and_reports_each_result_once() {
    let data = server::test_data(32 << 10);
    let s = server::start(data.clone(), true, 0).await;
    let state = tempfile::tempdir().unwrap();
    let log = hook_module(state.path());
    let modules = state.path().join("modules");
    let out = tempfile::tempdir().unwrap();
    let args = |extra: &[&str], url: &str| {
        let mut args = vec!["--json", "-q", "-o", out.path().to_str().unwrap()];
        args.extend_from_slice(extra);
        args.push(url);
        args.iter().map(|a| a.to_string()).collect::<Vec<_>>()
    };
    let go = |args: Vec<String>| {
        let refs: Vec<&str> = args.iter().map(String::as_str).collect();
        finish(dlx(state.path(), &refs))
    };
    // Without --modules-dir no hook runs.
    let plain = go(args(&[], &s.url("plain.bin"))).await;
    assert_eq!(plain.code, 0, "stderr: {}", plain.stderr);
    assert!(hook_lines(&log).is_empty());

    let with = ["--modules-dir", modules.to_str().unwrap()];
    let ok = go(args(&with, &s.url("hooked.bin"))).await;
    assert_eq!(ok.code, 0, "stderr: {}", ok.stderr);
    // The log is complete the moment dlx has exited.
    let lines = hook_lines(&log);
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert_eq!(lines[0]["event"], "completed");
    assert_eq!(lines[0]["id"], 0);
    assert_eq!(lines[0]["size"], data.len());
    assert!(lines[0]["path"].as_str().unwrap().ends_with("hooked.bin"));
    let events = ok.events();
    assert_eq!(events.last().unwrap()["event"], "summary");
    assert_eq!(events.last().unwrap()["exit_code"], 0);

    // A failing URL gives one failed event and the same exit code as without hooks.
    let missing = s.url("missing.sha256");
    let failed = go(args(&with, &missing)).await;
    let baseline = go(args(&[], &missing)).await;
    assert_ne!(failed.code, 0);
    assert_eq!(failed.code, baseline.code);
    let lines = hook_lines(&log);
    assert_eq!(lines.len(), 2, "{lines:?}");
    assert_eq!(lines[1]["event"], "failed");
    assert!(lines[1]["error"].as_str().is_some_and(|e| !e.is_empty()));
    assert_eq!(failed.events().last().unwrap()["event"], "summary");
}

#[tokio::test]
async fn dlx_human_output_keeps_its_shape_with_hooks() {
    let s = server::start(server::test_data(8 << 10), true, 0).await;
    let state = tempfile::tempdir().unwrap();
    let log = hook_module(state.path());
    let modules = state.path().join("modules");
    let out = tempfile::tempdir().unwrap();
    let run = finish(dlx(
        state.path(),
        &[
            "-q",
            "--modules-dir",
            modules.to_str().unwrap(),
            "-o",
            out.path().to_str().unwrap(),
            &s.url("human.bin"),
        ],
    ))
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(run.stdout.lines().count(), 1, "{}", run.stdout);
    assert!(run.stdout.contains("human.bin"), "{}", run.stdout);
    assert!(run.stderr.is_empty(), "{}", run.stderr);
    assert_eq!(hook_lines(&log).len(), 1);
}

#[tokio::test]
async fn dlx_mirror_flag_is_validated_before_any_transfer() {
    let state = tempfile::tempdir().unwrap();
    let cases: [&[&str]; 3] = [
        &[
            "--mirror",
            "https://b.test/a.bin",
            "https://a.test/a.bin",
            "https://c.test/c.bin",
        ],
        &["--mirror", "ftp://b.test/a.bin", "https://a.test/a.bin"],
        &["--mirror", "not a url", "https://a.test/a.bin"],
    ];
    for case in cases {
        let mut args = vec!["--json"];
        args.extend_from_slice(case);
        let run = finish(dlx(state.path(), &args)).await;
        assert_eq!(run.code, 2, "{case:?}: {}", run.stdout);
        assert_eq!(event(&run.events(), "error")[0]["code"], "invalid");
    }
    let nine: Vec<String> = (0..9).map(|i| format!("https://m{i}.test/a.bin")).collect();
    let mut args = vec!["--json"];
    for mirror in &nine {
        args.extend(["--mirror", mirror]);
    }
    args.push("https://a.test/a.bin");
    assert_eq!(finish(dlx(state.path(), &args)).await.code, 2);
}

#[tokio::test]
async fn dlx_mirror_is_accepted_for_a_single_url() {
    let data = server::test_data(16 << 10);
    let s = server::start(data.clone(), true, 0).await;
    let state = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let url = s.url("one.bin");
    let run = finish(dlx(
        state.path(),
        &[
            "--json",
            "-q",
            "--mirror",
            "http://127.0.0.1:9/one.bin",
            "-o",
            out.path().to_str().unwrap(),
            &url,
        ],
    ))
    .await;
    assert_eq!(run.code, 0, "stderr: {}", run.stderr);
    assert_eq!(std::fs::read(out.path().join("one.bin")).unwrap(), data);
}

#[tokio::test]
async fn dlxctl_add_mirror_is_stored_and_exported() {
    let state = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let (mut serve, endpoint) = start_serve(state.path()).await;
    let auth = ["--endpoint", endpoint.as_str(), "--token", TOKEN];
    let dir = out.path().to_str().unwrap();
    let call = |args: &[&str]| dlxctl(state.path(), &[&["--json"], &auth[..], args].concat());
    let bad = finish(call(&[
        "add",
        "http://127.0.0.1:9/a.bin",
        "--paused",
        "--mirror",
        "ftp://x.test/a.bin",
    ]))
    .await;
    assert_eq!(bad.code, 2, "stdout: {}", bad.stdout);
    let add = finish(call(&[
        "add",
        "http://127.0.0.1:9/a.bin",
        "--dir",
        dir,
        "--paused",
        "--mirror",
        "http://127.0.0.1:10/a.bin",
        "--mirror",
        "https://mirror.test/a.bin",
    ]))
    .await;
    assert_eq!(add.code, 0, "stdout: {}", add.stdout);
    let export = finish(call(&["export"])).await;
    let queue = export.envelope()["data"].clone();
    let mirrors = &queue["downloads"][0]["mirrors"];
    assert_eq!(
        mirrors,
        &json!(["http://127.0.0.1:10/a.bin", "https://mirror.test/a.bin"]),
        "{queue}"
    );
    let _ = serve.start_kill();
    let _ = serve.wait().await;
}
