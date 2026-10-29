//! `ignore` is the only way to stop a source from coming back. Before it there
//! was no CLI to edit the list at all, so a source a person did not want was
//! unremovable except by hand-editing `.wiki/ignore`.

use std::fs;

use assert_cmd::Command;
use llm_wiki::init;
use tempfile::TempDir;

fn repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    init::run(dir.path(), false).unwrap();
    dir
}

fn run(dir: &TempDir, args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .current_dir(dir.path())
        .args(["--json"])
        .args(args)
        .output()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|_| panic!("not JSON: {}", String::from_utf8_lossy(&out.stdout)));
    (out.status.code().unwrap_or(-1), body)
}

#[test]
fn adding_a_pattern_writes_it_and_listing_shows_it() {
    let dir = repo();
    let (code, body) = run(&dir, &["ignore", "--add", "drafts/**", "*.tmp"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["count"], 2);

    let (code, body) = run(&dir, &["ignore"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["patterns"][0], "drafts/**");
    assert_eq!(body["data"]["patterns"][1], "*.tmp");
}

#[test]
fn removing_a_pattern_leaves_the_others() {
    let dir = repo();
    run(&dir, &["ignore", "--add", "drafts/**", "*.tmp"]);
    let (code, body) = run(&dir, &["ignore", "--remove", "drafts/**"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["removed"][0], "drafts/**");
    assert_eq!(body["data"]["count"], 1);

    let (_, body) = run(&dir, &["ignore"]);
    assert_eq!(body["data"]["patterns"][0], "*.tmp");
}

#[test]
fn check_names_the_pattern_that_matches() {
    let dir = repo();
    run(&dir, &["ignore", "--add", "drafts/**"]);

    let (code, body) = run(&dir, &["ignore", "--check", "drafts/a.md"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["ignored"], true);
    assert_eq!(body["data"]["pattern"], "drafts/**");

    let (_, body) = run(&dir, &["ignore", "--check", "notes/a.md"]);
    assert_eq!(body["data"]["ignored"], false);
    assert!(body["data"]["pattern"].is_null());
}

#[test]
fn an_ignored_source_is_not_scanned() {
    let dir = repo();
    fs::write(dir.path().join("keep.md"), "# Keep\n\n保留。\n").unwrap();
    fs::write(dir.path().join("drop.md"), "# Drop\n\n丢弃。\n").unwrap();
    run(&dir, &["ignore", "--add", "drop.md"]);

    let (code, body) = run(&dir, &["ingest", "--dry-run"]);
    assert_eq!(code, 0, "{body}");
    let keys: Vec<&str> = body["data"]["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|change| change["key"].as_str().unwrap())
        .collect();
    assert!(keys.contains(&"keep.md"), "{keys:?}");
    assert!(!keys.contains(&"drop.md"), "{keys:?}");
}
