use super::registration::{inspect_manifest, remove_manifest};
use super::*;
#[test]
fn invalid_host_is_rejected_before_any_registration() {
    assert!(register(Path::new("downloader-host")).is_err());
    let dir = tempfile::tempdir().unwrap();
    assert!(register(dir.path()).is_err());
    assert!(register(&dir.path().join("missing-host")).is_err());
}
#[test]
fn status_checks_identity_origin_host_and_installation() {
    let dir = tempfile::tempdir().unwrap();
    let host = dir.path().join(host_exe_name());
    std::fs::write(&host, "fixture host, never executed").unwrap();
    let file = dir.path().join("manifest.json");
    std::fs::write(&file, manifest_json(&host)).unwrap();
    assert!(inspect_manifest(&file, &host).registered);
    let mut value: serde_json::Value = serde_json::from_str(&manifest_json(&host)).unwrap();
    for field in ["name", "type", "allowed_origins"] {
        let original = value[field].clone();
        value[field] = serde_json::json!("wrong");
        std::fs::write(&file, value.to_string()).unwrap();
        assert!(!inspect_manifest(&file, &host).registered, "{field}");
        value[field] = original;
    }
    value["path"] = serde_json::json!("relative-host");
    std::fs::write(&file, value.to_string()).unwrap();
    assert!(!inspect_manifest(&file, &host).registered);
    std::fs::write(&file, manifest_json(&host)).unwrap();
    let other = dir.path().join("other-host");
    std::fs::write(&other, "other").unwrap();
    assert!(!inspect_manifest(&file, &other).registered);
    std::fs::remove_file(&host).unwrap();
    assert!(!inspect_manifest(&file, &host).registered);
}
#[test]
fn missing_corrupt_manifest_and_failed_removal_are_not_success() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("manifest.json");
    assert!(!inspect_manifest(&file, dir.path()).registered);
    std::fs::write(&file, "not JSON").unwrap();
    assert!(!inspect_manifest(&file, dir.path()).registered);
    assert!(remove_manifest(dir.path()).is_err());
    assert!(remove_manifest(&file).is_ok());
    assert!(remove_manifest(&file).is_ok());
}
#[test]
fn validates_bundle_and_rejects_unrelated_or_incomplete_folders() {
    assert!(
        validate_extension_dir(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../extension")).is_ok()
    );
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("manifest.json"), BUNDLED_MANIFEST).unwrap();
    assert!(
        validate_extension_dir(dir.path())
            .unwrap_err()
            .contains("missing")
    );
    let mut value: serde_json::Value = serde_json::from_str(BUNDLED_MANIFEST).unwrap();
    value["key"] = serde_json::json!("another extension key");
    std::fs::write(dir.path().join("manifest.json"), value.to_string()).unwrap();
    assert!(
        validate_extension_dir(dir.path())
            .unwrap_err()
            .contains("key")
    );
}
#[test]
fn browser_index_and_internal_urls_are_explicit() {
    assert_eq!(Browser::from_index(-1), None);
    assert_eq!(Browser::from_index(3), None);
    assert_eq!(Browser::from_index(1), Some(Browser::Brave));
    assert_eq!(Browser::Chrome.extensions_url(), "chrome://extensions/");
    assert_eq!(Browser::Brave.extensions_url(), "brave://extensions/");
    assert_eq!(Browser::Edge.extensions_url(), "edge://extensions/");
}
