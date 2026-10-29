use assert_cmd::Command;

#[test]
fn version_flag_prints_the_version_alone() {
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("llm-wiki "), "{text}");
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
