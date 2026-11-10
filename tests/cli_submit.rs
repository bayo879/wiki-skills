//! Removed publishing commands must never mutate legacy data.
use assert_cmd::Command;

#[test]
fn submit_is_rejected_without_touching_legacy_candidates() {
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join(".wiki/staging/concept/a.md");
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, "legacy candidate").unwrap();
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .current_dir(dir.path())
        .args(["--json", "submit", "--confirm"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(body["error"]["code"], "E_USAGE");
    assert_eq!(std::fs::read_to_string(legacy).unwrap(), "legacy candidate");
}
