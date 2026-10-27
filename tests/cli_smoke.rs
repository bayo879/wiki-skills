use assert_cmd::Command;

#[test]
fn version_emits_one_json_document() {
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json", "version"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["cmd"], "version");
    assert!(v["data"]["version"].is_string());
}

#[test]
fn unknown_subcommand_exits_2() {
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json", "nope"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "E_USAGE");
}
