//! Removed snapshot commands cannot restore or alter legacy content.
use assert_cmd::Command;

#[test]
fn version_subcommands_are_rejected_and_preserve_legacy_bytes() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join(".wiki/versions/v001/manifest.json");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "legacy manifest").unwrap();
    for args in [
        vec!["version", "list"],
        vec!["version", "diff"],
        vec!["version", "restore", "v001", "--confirm"],
    ] {
        let out = Command::cargo_bin("llm-wiki")
            .unwrap()
            .current_dir(dir.path())
            .arg("--json")
            .args(args)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
        assert_eq!(body["error"]["code"], "E_USAGE");
        assert_eq!(std::fs::read_to_string(&legacy).unwrap(), "legacy manifest");
    }
}
