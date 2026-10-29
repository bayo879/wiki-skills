//! `submit` is the close of a round: it refreshes the staged candidates, then
//! publishes the ones the brakes allow. These tests drive it through the binary.

use std::fs;

use assert_cmd::Command;
use llm_wiki::{config::Config, init, staging};
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

/// One open round: source converted, pending list written.
fn repo_with_round() -> TempDir {
    let dir = repo();
    fs::write(dir.path().join("a.md"), "# A\n\n订单超时后重试三次。\n").unwrap();
    let (code, body) = run(&dir, &["ingest"]);
    assert_eq!(code, 0, "{body}");
    dir
}

/// Stages a candidate page that cites the round's raw.
fn stage(dir: &TempDir, id: &str, title: &str) {
    let cfg = Config::load(dir.path()).unwrap();
    let text = format!(
        "---\nid: {id}\ntitle: {title}\ntype: concept\nsummary: 一句结论\nstatus: draft\nupdated: 2026-01-01\nsources: [.wiki/raw/a.md]\n---\n正文\n"
    );
    staging::write_page(dir.path(), &cfg, &format!("concept/{id}"), &text).unwrap();
}

#[test]
fn submit_publishes_the_staged_candidates() {
    let dir = repo_with_round();
    stage(&dir, "alpha", "Alpha");
    stage(&dir, "beta", "Beta");

    let (code, body) = run(&dir, &["submit"]);
    assert_eq!(code, 0, "{body}");
    assert!(dir.path().join(".wiki/pages/concept/alpha.md").is_file());
    assert!(dir.path().join(".wiki/pages/concept/beta.md").is_file());
    assert!(
        !dir.path().join(".wiki/staging/concept/alpha.md").exists(),
        "published candidates leave staging"
    );
}

#[test]
fn submit_without_candidates_succeeds_with_nothing_published() {
    let dir = repo_with_round();
    let (code, body) = run(&dir, &["submit"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["published"].as_array().unwrap().len(), 0);
}

#[test]
fn submit_auto_skips_a_braked_candidate_and_exits_zero() {
    let dir = repo_with_round();
    stage(&dir, "alpha", "Alpha");
    let (code, body) = run(&dir, &["submit"]);
    assert_eq!(code, 0, "{body}");

    // Same title as a published page: the overlap brake holds it.
    stage(&dir, "beta", "Alpha");
    let (code, body) = run(&dir, &["submit", "--auto"]);
    assert_eq!(code, 0, "{body}");
    assert!(
        !dir.path().join(".wiki/pages/concept/beta.md").exists(),
        "the braked candidate is not published"
    );
    assert!(
        !body["data"]["held_back"]
            .as_array()
            .map(|a| a.is_empty())
            .unwrap_or(true),
        "the skip is reported in held_back: {body}"
    );
}

#[test]
fn submit_confirm_skips_every_brake() {
    let dir = repo_with_round();
    stage(&dir, "alpha", "Alpha");
    let (code, body) = run(&dir, &["submit"]);
    assert_eq!(code, 0, "{body}");

    stage(&dir, "beta", "Alpha");
    let (code, body) = run(&dir, &["submit", "--confirm"]);
    assert_eq!(code, 0, "{body}");
    assert!(
        dir.path().join(".wiki/pages/concept/beta.md").is_file(),
        "--confirm publishes even an overlapping page"
    );
}
