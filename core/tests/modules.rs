//! External modules end to end, using the dlx-module-example binary.
mod server;

use dlcore::modules::{HookEvent, ModuleSettings, Registry, manifest_hash};
use dlcore::{Ctx, Outcome, RateLimiter, Settings, Task, engine};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

const EXE: &str = env!("CARGO_BIN_EXE_dlx-module-example");
const HOST_URL: &str = "http://dl.example.test/big.bin";

fn install(root: &Path, id: &str, args: &[&str], extra: Value) -> PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut command = vec![Value::from(EXE)];
    command.extend(args.iter().map(|a| Value::from(*a)));
    let mut manifest = json!({
        "schema": 1, "id": id, "name": "Test module", "version": "1.0.0",
        "kinds": ["resolver", "hook"],
        "match": {"schemes": ["http"], "hosts": ["dl.example.test"]},
        "command": command, "timeout_ms": 10000, "events": ["completed"]
    });
    for (k, v) in extra.as_object().unwrap() {
        manifest[k] = v.clone();
    }
    std::fs::write(dir.join("module.json"), manifest.to_string()).unwrap();
    dir
}

fn approved(root: &Path, ids: &[&str]) -> ModuleSettings {
    ModuleSettings {
        enabled: true,
        approved: ids
            .iter()
            .map(|id| (id.to_string(), manifest_hash(root, id).unwrap()))
            .collect(),
    }
}

fn task(url: &str) -> Arc<Mutex<Task>> {
    Arc::new(Mutex::new(Task {
        id: 7,
        url: url.into(),
        ..Default::default()
    }))
}

async fn resolve(registry: &Registry, url: &str) -> (Result<(), String>, Task) {
    let t = task(url);
    let result = registry.prepare(&t, &CancellationToken::new()).await;
    let snapshot = t.lock().unwrap().clone();
    (result, snapshot)
}

#[tokio::test]
async fn modules_never_spawn_unless_enabled_and_approved() {
    let root = tempfile::tempdir().unwrap();
    let dir = install(
        root.path(),
        "gate",
        &["--log", "calls.log", "--base", "http://127.0.0.1:9/"],
        json!({}),
    );
    let marker = dir.join("calls.log");
    let good = approved(root.path(), &["gate"]);
    let disabled = ModuleSettings {
        enabled: false,
        ..good.clone()
    };
    let unapproved = ModuleSettings {
        enabled: true,
        ..Default::default()
    };
    let mut stale = good.clone();
    stale.approved.insert("gate".into(), "0".repeat(64));
    for settings in [&disabled, &unapproved, &stale] {
        let registry = Registry::load(settings, root.path());
        let (result, t) = resolve(&registry, HOST_URL).await;
        assert!(result.is_ok());
        assert!(t.resolved_url.is_none() && t.module.is_none());
        assert!(!marker.exists(), "{settings:?}");
    }
    let registry = Registry::load(&good, root.path());
    let info = registry
        .list()
        .into_iter()
        .find(|m| m.id == "gate")
        .unwrap();
    assert!(info.approved && info.error.is_none() && !info.builtin);
    // Built-in protocols and unclaimed hosts never reach the module.
    for url in [
        "http://dl.example.test/a.torrent",
        "http://other.test/big.bin",
        "https://dl.example.test/big.bin",
    ] {
        assert!(resolve(&registry, url).await.0.is_ok());
    }
    assert!(!marker.exists());
    let (result, t) = resolve(&registry, HOST_URL).await;
    result.unwrap();
    assert_eq!(
        t.resolved_url.as_deref(),
        Some("http://127.0.0.1:9/big.bin")
    );
    assert_eq!(t.module.as_deref(), Some("gate"));
    let request: Value =
        serde_json::from_str(std::fs::read_to_string(&marker).unwrap().trim()).unwrap();
    assert_eq!(
        request,
        json!({"v": 1, "op": "resolve", "url": HOST_URL, "referer": null})
    );
    // Editing module.json after loading disables it before anything is started.
    let manifest = dir.join("module.json");
    let mut bytes = std::fs::read(&manifest).unwrap();
    bytes.push(b' ');
    std::fs::write(&manifest, bytes).unwrap();
    let err = resolve(&registry, HOST_URL).await.0.unwrap_err();
    assert!(
        err.starts_with("module gate: ") && err.contains("changed"),
        "{err}"
    );
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
    let registry = Registry::load(&good, root.path());
    let info = registry
        .list()
        .into_iter()
        .find(|m| m.id == "gate")
        .unwrap();
    assert!(!info.approved && info.error.is_some());
}

#[tokio::test]
async fn timeouts_and_cancellation_kill_the_module() {
    let root = tempfile::tempdir().unwrap();
    install(
        root.path(),
        "sleepy",
        &["--sleep"],
        json!({"timeout_ms": 500}),
    );
    let registry = Registry::load(&approved(root.path(), &["sleepy"]), root.path());
    let started = Instant::now();
    let err = resolve(&registry, HOST_URL).await.0.unwrap_err();
    assert!(err.starts_with("module sleepy: timed out"), "{err}");
    assert!(started.elapsed() < Duration::from_secs(8));
    let t = task(HOST_URL);
    let cancel = CancellationToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        trigger.cancel();
    });
    let started = Instant::now();
    registry.prepare(&t, &cancel).await.unwrap();
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(t.lock().unwrap().resolved_url.is_none());
}

#[tokio::test]
async fn unsafe_or_malformed_answers_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let cases: [(&str, &[&str], &str); 7] = [
        ("flood", &["--flood"], "64 KiB"),
        ("garbage", &["--garbage"], "not a JSON"),
        ("ftp-out", &["--base", "ftp://127.0.0.1/"], "only http(s)"),
        (
            "torrent-out",
            &["--base", "http://127.0.0.1/x/", "--header", "X-A:1"],
            "",
        ),
        (
            "cookie",
            &["--base", "http://127.0.0.1/", "--header", "Cookie:sid=1"],
            "Cookie",
        ),
        (
            "mirror",
            &[
                "--base",
                "http://127.0.0.1/",
                "--mirror",
                "file:///etc/passwd",
            ],
            "only http(s)",
        ),
        ("boom", &["--fail", "boom"], "boom"),
    ];
    let ids: Vec<&str> = cases.iter().map(|c| c.0).collect();
    for (id, args, _) in &cases {
        install(
            root.path(),
            id,
            args,
            json!({"match": {"schemes": ["http"], "hosts": [format!("{id}.test")]}}),
        );
    }
    let registry = Registry::load(&approved(root.path(), &ids), root.path());
    for (id, _, expected) in cases {
        let url = if id == "torrent-out" {
            format!("http://{id}.test/a.torrent.bin")
        } else {
            format!("http://{id}.test/a.bin")
        };
        let (result, t) = resolve(&registry, &url).await;
        if id == "torrent-out" {
            result.unwrap();
            assert_eq!(t.headers, [("X-A".to_string(), "1".to_string())]);
            continue;
        }
        let err = result.unwrap_err();
        assert!(
            err.starts_with(&format!("module {id}: ")) && err.contains(expected),
            "{id}: {err}"
        );
        assert!(t.resolved_url.is_none() && t.headers.is_empty() && t.module.is_none());
        assert_eq!(dlcore::util::error_code(&err), "module", "{err}");
    }
    let (result, _) = resolve(&registry, "http://torrent-out.test/a.torrent").await;
    assert!(
        result.is_ok(),
        "built-in protocol links are not offered to modules"
    );
}

#[test]
fn path_escaping_commands_are_never_loaded() {
    let root = tempfile::tempdir().unwrap();
    let dir = install(
        root.path(),
        "escape",
        &[],
        json!({"command": ["../escape.exe"]}),
    );
    std::fs::copy(EXE, root.path().join("escape.exe")).unwrap();
    assert!(manifest_hash(root.path(), "escape").is_err());
    let mut settings = ModuleSettings {
        enabled: true,
        ..Default::default()
    };
    let bytes = std::fs::read(dir.join("module.json")).unwrap();
    use sha2::Digest;
    settings
        .approved
        .insert("escape".into(), hex::encode(sha2::Sha256::digest(&bytes)));
    let registry = Registry::load(&settings, root.path());
    let info = registry
        .list()
        .into_iter()
        .find(|m| m.id == "escape")
        .unwrap();
    assert!(info.error.unwrap().contains(".."));
}

#[tokio::test]
async fn resolver_rewrites_a_download_end_to_end() {
    let data = server::test_data(256 << 10);
    let s = server::start(data.clone(), true, 0).await;
    let base = s.url("");
    let root = tempfile::tempdir().unwrap();
    install(
        root.path(),
        "example-resolver",
        &[
            "--base",
            &base,
            "--header",
            "X-Token:abc",
            "--filename",
            "named.bin",
        ],
        json!({}),
    );
    let registry = Registry::load(&approved(root.path(), &["example-resolver"]), root.path());
    let out = tempfile::tempdir().unwrap();
    let t = task(HOST_URL);
    t.lock().unwrap().dir = out.path().into();
    registry
        .prepare(&t, &CancellationToken::new())
        .await
        .unwrap();
    {
        let t = t.lock().unwrap();
        assert_eq!(t.resolved_url.as_deref(), Some(s.url("big.bin").as_str()));
        assert_eq!(t.url, HOST_URL, "the original link is kept");
        assert_eq!(t.headers, [("X-Token".to_string(), "abc".to_string())]);
        assert_eq!(t.suggested_name.as_deref(), Some("named.bin"));
        assert!(t.log.iter().any(|l| l.contains("example-resolver")));
    }
    let ctx = Ctx {
        protocol_client: Settings::default().build_protocol_client().unwrap(),
        client: Settings::default().build_client().unwrap(),
        global: Arc::new(RateLimiter::new(0)),
        local: Arc::new(RateLimiter::new(0)),
        retries: 1,
        cancel: CancellationToken::new(),
        verify: false,
    };
    assert_eq!(
        engine::run(t.clone(), ctx).await.unwrap(),
        Outcome::Completed
    );
    let path = t.lock().unwrap().final_path();
    assert_eq!(std::fs::read(path).unwrap(), data);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hooks_fire_once_per_subscribed_event() {
    let root = tempfile::tempdir().unwrap();
    let dir = install(
        root.path(),
        "hook",
        &["--log", "events.log"],
        json!({"kinds": ["hook"], "match": {}}),
    );
    let registry = Registry::load(&approved(root.path(), &["hook"]), root.path());
    let event = |name: &'static str| HookEvent {
        event: name,
        id: 3,
        url: "https://example.com/a.zip".into(),
        path: PathBuf::from("C:/Downloads/a.zip"),
        size: Some(10),
        error: None,
    };
    let rt = tokio::runtime::Handle::current();
    registry.notify(&rt, event("failed"));
    registry.notify(&rt, event("completed"));
    let log = dir.join("events.log");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !log.exists() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let text = std::fs::read_to_string(&log).unwrap();
    let lines: Vec<Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 1, "{text}");
    assert_eq!(lines[0]["op"], "event");
    assert_eq!(lines[0]["v"], 1);
    assert_eq!(lines[0]["event"], "completed");
    assert_eq!(lines[0]["id"], 3);
    assert_eq!(lines[0]["size"], 10);
}
