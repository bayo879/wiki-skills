//! `version` is the only way back from a bad publish: `.wiki/` is local and
//! usually outside git, so a restore that does not work is a one-way door.
//! These tests drive the binary, because the gap this closes was a missing
//! command, not a missing function.

use std::fs;

use assert_cmd::Command;
use llm_wiki::{config::Config, init, staging};
use tempfile::TempDir;

fn repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    init::run(dir.path(), false).unwrap();
    dir
}

/// Backdates a source past `[ingest].settle_seconds`: these tests are about
/// versions, not about when a file stops moving.
fn settle(dir: &TempDir, name: &str) {
    let file = fs::File::options()
        .write(true)
        .open(dir.path().join(name))
        .unwrap();
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(600))
        .unwrap();
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

/// One round that produced a version, with one published page.
fn repo_with_published_page() -> TempDir {
    let dir = repo();
    fs::write(dir.path().join("a.md"), "# A\n\n订单超时后重试三次。\n").unwrap();
    settle(&dir, "a.md");
    let (code, body) = run(&dir, &["ingest"]);
    assert_eq!(code, 0, "{body}");

    let cfg = Config::load(dir.path()).unwrap();
    let text = "---\nid: alpha\ntitle: Alpha\ntype: concept\nsummary: 一句结论\n\
                status: draft\nupdated: 2026-01-01\nsources: [.wiki/raw/a.md]\n---\n正文\n";
    staging::write_page(dir.path(), &cfg, "concept/alpha", text).unwrap();
    let (code, body) = run(&dir, &["submit"]);
    assert_eq!(code, 0, "{body}");
    dir
}

#[test]
fn version_list_has_one_row_per_ingest_and_none_per_submit() {
    let dir = repo();
    fs::write(dir.path().join("a.md"), "# A\n\n订单超时后重试三次。\n").unwrap();
    settle(&dir, "a.md");
    let (code, body) = run(&dir, &["ingest"]);
    assert_eq!(code, 0, "{body}");
    let (code, body) = run(&dir, &["version", "list"]);
    assert_eq!(code, 0, "{body}");
    let after_ingest = body["data"]["versions"].as_array().unwrap().len();

    let (code, body) = run(&dir, &["submit"]);
    assert_eq!(code, 0, "{body}");
    let (code, body) = run(&dir, &["version", "list"]);
    assert_eq!(code, 0, "{body}");
    let versions = body["data"]["versions"].as_array().unwrap();
    assert_eq!(versions.len(), after_ingest, "submit does not snapshot");
    assert_eq!(versions[0]["kind"], "ingest", "newest first");
}

#[test]
fn version_diff_defaults_to_the_newest_snapshot() {
    let dir = repo_with_published_page();
    let (code, body) = run(&dir, &["version", "diff"]);
    assert_eq!(code, 0, "{body}");
    let added: Vec<&str> = body["data"]["added"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| path.as_str().unwrap())
        .collect();
    assert!(
        added.contains(&"pages/concept/alpha.md"),
        "the newest snapshot predates the publish: {added:?}"
    );
}

#[test]
fn version_diff_reports_a_file_edited_after_the_snapshot() {
    let dir = repo_with_published_page();
    let raw = dir.path().join(".wiki/raw/a.md");
    let mut text = fs::read_to_string(&raw).unwrap();
    text.push_str("人手改的一行。\n");
    fs::write(&raw, text).unwrap();

    let (code, body) = run(&dir, &["version", "diff"]);
    assert_eq!(code, 0, "{body}");
    let changed: Vec<&str> = body["data"]["changed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|path| path.as_str().unwrap())
        .collect();
    assert_eq!(changed, ["raw/a.md"]);
}

#[test]
fn version_restore_requires_confirm() {
    let dir = repo_with_published_page();
    let (code, body) = run(&dir, &["version", "restore", "v001"]);
    assert_eq!(code, 2, "{body}");
    assert_eq!(body["error"]["code"], "E_CONFIRM_REQUIRED");
    assert!(
        dir.path().join(".wiki/pages/concept/alpha.md").is_file(),
        "refusing must not touch the tree"
    );
}

#[test]
fn version_restore_of_an_unknown_tag_fails_cleanly() {
    let dir = repo_with_published_page();
    let (code, body) = run(&dir, &["version", "restore", "v999", "--confirm"]);
    assert_eq!(code, 2, "{body}");
    assert_eq!(body["error"]["code"], "E_NO_VERSION");
    assert!(
        dir.path().join(".wiki/pages/concept/alpha.md").is_file(),
        "an unknown tag must not empty the tree"
    );
}

#[test]
fn version_restore_removes_the_bad_publish_and_is_itself_a_snapshot() {
    let dir = repo_with_published_page();
    let page = dir.path().join(".wiki/pages/concept/alpha.md");
    assert!(page.is_file(), "the round published a page");
    let (code, body) = run(&dir, &["version", "list"]);
    assert_eq!(code, 0, "{body}");
    let ingest_tag = body["data"]["versions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["kind"] == "ingest")
        .unwrap()["version"]
        .as_str()
        .unwrap()
        .to_string();

    // The ingest snapshot predates the publish, so restoring it retracts it.
    let (code, body) = run(&dir, &["version", "restore", &ingest_tag, "--confirm"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["restored"], ingest_tag);
    assert!(
        !page.exists(),
        "the page written after the snapshot is gone"
    );

    let (code, body) = run(&dir, &["version", "list"]);
    assert_eq!(code, 0, "{body}");
    let latest = &body["data"]["versions"].as_array().unwrap()[0];
    assert_eq!(latest["kind"], "rollback", "a restore is undoable too");
}
