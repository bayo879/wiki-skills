//! `ingest` is the one write-line command a human or agent runs: it scans,
//! converts, and opens a round. These tests drive it through the binary.

use std::fs;

use assert_cmd::Command;
use tempfile::TempDir;

/// A repository with the contract written and no source yet.
fn repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    llm_wiki::init::run(dir.path(), false).unwrap();
    dir
}

/// Runs `llm-wiki --json <args>` inside `dir` and parses the envelope.
fn run(dir: &TempDir, args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .current_dir(dir.path())
        .args(["--json"])
        .args(args)
        .output()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout is not JSON: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    (out.status.code().unwrap_or(-1), body)
}

fn write_source(dir: &TempDir, name: &str, body: &str) {
    fs::write(dir.path().join(name), body).unwrap();
}

#[test]
fn ingest_without_arguments_converts_and_opens_a_round() {
    let dir = repo();
    write_source(&dir, "a.md", "# A\n\n订单超时后重试三次。\n");
    write_source(&dir, "b.md", "# B\n\n退款走人工审核。\n");

    let (code, body) = run(&dir, &["ingest"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["converted"], 2);
    assert_eq!(body["data"]["ingested"].as_array().unwrap().len(), 2);
    assert!(dir.path().join(".wiki/staging/pending.json").is_file());
}

#[test]
fn ingest_with_keys_reads_only_named_sources() {
    let dir = repo();
    write_source(&dir, "a.md", "# A\n\n订单超时后重试三次。\n");
    write_source(&dir, "b.md", "# B\n\n退款走人工审核。\n");

    let (code, body) = run(&dir, &["ingest", "a.md"]);
    assert_eq!(code, 0, "{body}");
    let ingested = body["data"]["ingested"].as_array().unwrap();
    assert_eq!(ingested.len(), 1);
    assert_eq!(ingested[0], "a.md");
}

#[test]
fn ingest_on_an_empty_repo_reports_nothing_to_do() {
    let dir = repo();
    let (code, body) = run(&dir, &["ingest"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["converted"], 0);
    assert!(body["data"]["ingested"].as_array().unwrap().is_empty());
}

#[test]
fn ingest_dry_run_writes_no_round() {
    let dir = repo();
    write_source(&dir, "a.md", "# A\n\n订单超时后重试三次。\n");

    let (code, body) = run(&dir, &["ingest", "--dry-run"]);
    assert_eq!(code, 0, "{body}");
    assert!(!dir.path().join(".wiki/staging/pending.json").exists());
    assert!(!dir.path().join(".wiki/raw/a.md").exists());
}

#[test]
fn ingest_dry_run_tolerates_an_open_round() {
    let dir = repo();
    write_source(&dir, "a.md", "# A\n\n订单超时后重试三次。\n");
    let (code, _) = run(&dir, &["ingest"]);
    assert_eq!(code, 0);
    assert!(dir.path().join(".wiki/staging/pending.json").is_file());

    let (code, body) = run(&dir, &["ingest", "--dry-run"]);
    assert_eq!(code, 0, "{body}");
}
