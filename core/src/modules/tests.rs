use super::manifest::{check, host_matches, sha256_hex};
use super::*;
use serde_json::json;

fn module(root: &Path, id: &str, manifest: &serde_json::Value) -> PathBuf {
    let dir = root.join(id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("tool.exe"), b"").unwrap();
    std::fs::write(dir.join("module.json"), manifest.to_string()).unwrap();
    dir
}

fn manifest(id: &str) -> serde_json::Value {
    json!({
        "schema": 1, "id": id, "name": "Example", "version": "1.0.0",
        "kinds": ["resolver", "hook"],
        "match": {"schemes": ["https"], "hosts": ["example.com", "*.example.org"]},
        "command": ["tool.exe", "--stdio"], "timeout_ms": 10000, "events": ["completed", "failed"]
    })
}

fn checked(root: &Path, value: &serde_json::Value) -> Result<manifest::Checked, String> {
    let id = value["id"].as_str().unwrap_or("x");
    let dir = module(root, id, value);
    check(&dir, &std::fs::read(dir.join("module.json")).unwrap())
}

#[test]
fn settings_validation() {
    let mut s = ModuleSettings::default();
    assert!(s.validate().is_ok());
    s.approved.insert("example-resolver".into(), "a".repeat(64));
    assert!(s.validate().is_ok());
    s.approved.insert("Bad".into(), "a".repeat(64));
    assert!(s.validate().is_err());
    s.approved.remove("Bad");
    s.approved.insert("x".repeat(41), "a".repeat(64));
    assert!(s.validate().is_err());
    s.approved.remove(&"x".repeat(41));
    s.approved.insert("ok".into(), "A".repeat(64));
    assert!(s.validate().is_err());
    s.approved.insert("ok".into(), "a".repeat(63));
    assert!(s.validate().is_err());
}

#[test]
fn empty_registry_lists_builtins_only() {
    let r = Registry::load(&ModuleSettings::default(), Path::new("does-not-exist"));
    let ids: Vec<_> = r.list().into_iter().map(|m| (m.id, m.builtin)).collect();
    assert_eq!(
        ids,
        [
            ("media", true),
            ("ftp", true),
            ("torrent", true),
            ("metalink", true)
        ]
        .map(|(i, b)| (i.to_string(), b))
    );
    assert!(r.runnable.is_empty());
    assert_eq!(Registry::empty().list().len(), 4);
    assert!(modules_dir().ends_with("modules"));
}

#[test]
fn valid_manifest_is_accepted() {
    let root = tempfile::tempdir().unwrap();
    let c = checked(root.path(), &manifest("example-resolver")).unwrap();
    assert_eq!(c.manifest.id, "example-resolver");
    assert!(c.program.ends_with("tool.exe"));
    let bytes = std::fs::read(root.path().join("example-resolver/module.json")).unwrap();
    assert_eq!(c.sha256, sha256_hex(&bytes));
    assert_eq!(c.timeout().as_millis(), 10000);
    assert!(c.claims(&reqwest::Url::parse("https://example.com/a").unwrap()));
    assert!(c.claims(&reqwest::Url::parse("https://cdn.EXAMPLE.org/a").unwrap()));
    assert!(!c.claims(&reqwest::Url::parse("http://example.com/a").unwrap()));
    assert!(!c.claims(&reqwest::Url::parse("https://example.org/a").unwrap()));
    assert!(!c.claims(&reqwest::Url::parse("https://badexample.com/a").unwrap()));
    // Absolute program paths are allowed; hooks need no match block.
    let mut hook = manifest("hook-only");
    hook["kinds"] = json!(["hook"]);
    hook["match"] = json!({});
    hook["command"] = json!([c.program.to_string_lossy()]);
    assert!(
        checked(root.path(), &hook).is_ok(),
        "{:?}",
        checked(root.path(), &hook).err()
    );
}

#[test]
fn host_patterns() {
    assert!(host_matches("example.com", "example.com"));
    assert!(!host_matches("example.com", "a.example.com"));
    assert!(host_matches("*.example.com", "a.example.com"));
    assert!(host_matches("*.example.com", "a.b.example.com"));
    assert!(!host_matches("*.example.com", "example.com"));
    assert!(!host_matches("*.example.com", "aexample.com"));
}

#[test]
fn invalid_manifests_are_rejected() {
    let root = tempfile::tempdir().unwrap();
    let outside = root.path().join("outside.exe");
    std::fs::write(&outside, b"").unwrap();
    let cases: Vec<(&str, serde_json::Value)> = vec![
        ("schema", json!(2)),
        ("name", json!("")),
        ("version", json!("1\n2")),
        ("kinds", json!([])),
        ("kinds", json!(["downloader"])),
        (
            "match",
            json!({"schemes": ["ftp"], "hosts": ["example.com"]}),
        ),
        (
            "match",
            json!({"schemes": ["https"], "hosts": ["Example.com"]}),
        ),
        ("match", json!({"schemes": ["https"], "hosts": ["a/b"]})),
        ("match", json!({"schemes": ["https"], "hosts": []})),
        (
            "match",
            json!({"schemes": ["https"], "hosts": ["x"], "paths": ["/"]}),
        ),
        ("events", json!([])),
        ("events", json!(["started"])),
        ("timeout_ms", json!(0)),
        ("timeout_ms", json!(60001)),
        ("command", json!([])),
        ("command", json!(["missing.exe"])),
        ("command", json!(["../outside.exe"])),
        ("command", json!(["sub/tool.exe"])),
        ("command", json!(["sub\\tool.exe"])),
        ("command", json!(["C:tool.exe"])),
        (
            "command",
            json!([root
                .path()
                .join("example-resolver/../outside.exe")
                .to_string_lossy()]),
        ),
        ("command", json!(["tool.exe", "a\u{0}b"])),
        ("unexpected", json!(true)),
    ];
    for (field, value) in cases {
        let mut m = manifest("example-resolver");
        m[field] = value.clone();
        assert!(checked(root.path(), &m).is_err(), "{field} = {value}");
    }
    let dir = module(root.path(), "folder-name", &manifest("other-id"));
    let err = check(&dir, &std::fs::read(dir.join("module.json")).unwrap()).unwrap_err();
    assert!(err.contains("does not match"), "{err}");
    let mut bad = manifest("Bad");
    bad["id"] = json!("Bad");
    assert!(checked(root.path(), &bad).is_err());
    assert!(check(root.path(), b"not json").is_err());
}

#[test]
fn approval_state_controls_what_can_run() {
    let root = tempfile::tempdir().unwrap();
    module(root.path(), "alpha", &manifest("alpha"));
    module(root.path(), "beta", &manifest("beta"));
    module(root.path(), "gamma", &json!({"schema": 1}));
    let alpha = manifest_hash(root.path(), "alpha").unwrap();
    assert!(
        manifest_hash(root.path(), "gamma")
            .unwrap_err()
            .starts_with("module gamma: ")
    );
    assert!(manifest_hash(root.path(), "missing").is_err());
    assert!(manifest_hash(root.path(), "../alpha").is_err());
    let mut settings = ModuleSettings {
        enabled: true,
        approved: BTreeMap::from([
            ("alpha".into(), alpha.clone()),
            ("beta".into(), "0".repeat(64)),
        ]),
    };
    let r = Registry::load(&settings, root.path());
    let info = |r: &Registry, id: &str| r.list().into_iter().find(|m| m.id == id).unwrap();
    assert!(info(&r, "alpha").approved && info(&r, "alpha").error.is_none());
    assert_eq!(info(&r, "alpha").manifest_sha256, alpha);
    assert!(!info(&r, "beta").approved);
    assert!(info(&r, "beta").error.unwrap().contains("changed"));
    assert!(info(&r, "gamma").error.is_some() && !info(&r, "gamma").approved);
    assert_eq!(
        r.runnable
            .iter()
            .map(|c| c.manifest.id.as_str())
            .collect::<Vec<_>>(),
        ["alpha"]
    );
    settings.enabled = false;
    let r = Registry::load(&settings, root.path());
    assert!(info(&r, "alpha").approved && r.runnable.is_empty());
    // Editing the manifest revokes it.
    settings.enabled = true;
    let mut edited = manifest("alpha");
    edited["version"] = json!("1.0.1");
    module(root.path(), "alpha", &edited);
    let r = Registry::load(&settings, root.path());
    assert!(!info(&r, "alpha").approved && r.runnable.is_empty());
}

#[test]
fn resolver_output_urls() {
    assert!(check_output_url("https://cdn.example.com/a.zip").is_ok());
    assert!(check_output_url("ftp://example.com/a.zip").is_err());
    assert!(check_output_url("https://example.com/a.torrent").is_err());
    assert!(check_output_url("https://u:p@example.com/a").is_err());
    assert!(check_output_url("file:///C:/a.zip").is_err());
    assert!(check_output_url("not a url").is_err());
    assert_eq!(clean(" a\nb\u{7} "), "a b");
    assert_eq!(clean(&"x".repeat(500)).len(), 300);
}
