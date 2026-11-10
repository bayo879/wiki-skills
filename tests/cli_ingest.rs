//! `ingest` converts sources and hands durable work to an author of live pages.

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

/// Backdates a source past `[ingest].settle_seconds` (five minutes by default),
/// so the round sees a file that has stopped moving.
fn settle(dir: &TempDir, name: &str) {
    let file = fs::File::options()
        .write(true)
        .open(dir.path().join(name))
        .unwrap();
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(600))
        .unwrap();
}

#[test]
fn ingest_without_arguments_converts_and_hands_off_live_pages() {
    let dir = repo();
    write_source(&dir, "a.md", "# A\n\n订单超时后重试三次。\n");
    write_source(&dir, "b.md", "# B\n\n退款走人工审核。\n");
    settle(&dir, "a.md");
    settle(&dir, "b.md");

    let (code, body) = run(&dir, &["ingest"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["converted"], 2);
    assert_eq!(body["data"]["ingested"].as_array().unwrap().len(), 2);
    assert_eq!(body["data"]["write_to"], ".wiki/pages");
    assert_eq!(body["data"]["schema"], ".wiki/SCHEMA.md");
    assert!(body["data"]["types"]
        .as_array()
        .unwrap()
        .iter()
        .any(|ty| ty == "concept"));
    assert_eq!(body["data"]["pending"].as_array().unwrap().len(), 2);
    assert!(body["data"]["affected_pages"].is_array());
    assert!(!dir.path().join(".wiki/staging/pending.json").exists());
}

#[test]
fn ingest_with_keys_reads_only_named_sources() {
    let dir = repo();
    write_source(&dir, "a.md", "# A\n\n订单超时后重试三次。\n");
    write_source(&dir, "b.md", "# B\n\n退款走人工审核。\n");
    settle(&dir, "a.md");
    settle(&dir, "b.md");

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
fn ingest_auto_runs_one_default_round() {
    let dir = repo();
    write_source(&dir, "a.md", "# A\n\n订单超时后重试三次。\n");
    settle(&dir, "a.md");

    let (code, body) = run(&dir, &["ingest", "--auto"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["converted"], 1);
    assert_eq!(body["data"]["write_to"], ".wiki/pages");
}

#[test]
fn ingest_auto_refuses_manual_options() {
    let dir = repo();
    let (code, body) = run(&dir, &["ingest", "--auto", "--dry-run"]);
    assert_eq!(code, 2, "{body}");
    assert_eq!(body["error"]["code"], "E_USAGE");
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
fn ingest_dry_run_key_prints_the_parse_preview() {
    let dir = repo();
    write_source(&dir, "a.md", "# A\n\n订单超时后重试三次。\n");

    let (code, body) = run(&dir, &["ingest", "--dry-run", "--key", "a.md"]);
    assert_eq!(code, 0, "{body}");
    assert!(body["data"]["backend"].is_string(), "{body}");
    assert!(
        body["data"]["markdown_head"]
            .as_str()
            .unwrap_or_default()
            .contains("订单超时后重试三次"),
        "{body}"
    );
}

#[test]
fn ingest_dry_run_preserves_pending_work() {
    let dir = repo();
    write_source(&dir, "a.md", "# A\n\n订单超时后重试三次。\n");
    settle(&dir, "a.md");
    let (code, body) = run(&dir, &["ingest"]);
    assert_eq!(code, 0);
    assert_eq!(body["data"]["pending"].as_array().unwrap().len(), 1);

    let (code, body) = run(&dir, &["ingest", "--dry-run"]);
    assert_eq!(code, 0, "{body}");
}

#[test]
fn ingest_waits_for_the_settle_window() {
    let dir = repo();
    write_source(&dir, "a.md", "# A\n\n订单超时后重试三次。\n");

    let (code, body) = run(&dir, &["ingest"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["converted"], 0, "a fresh file waits");
    assert!(
        body["data"]["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap_or_default().contains("settle")),
        "{body}"
    );
    assert!(!dir.path().join(".wiki/staging/pending.json").exists());
}

#[test]
fn ingest_force_converts_a_fresh_source() {
    let dir = repo();
    write_source(&dir, "a.md", "# A\n\n订单超时后重试三次。\n");

    let (code, body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["converted"], 1);
}

#[test]
fn preview_force_large_does_not_bypass_the_byte_limit() {
    let dir = repo();
    let mut cfg = llm_wiki::config::Config::load(dir.path()).unwrap();
    cfg.parse.max_source_bytes = 4;
    fs::write(
        dir.path().join(".wiki/wiki.toml"),
        toml::to_string(&cfg).unwrap(),
    )
    .unwrap();
    write_source(&dir, "a.md", "too large for the configured budget\n");
    let (code, body) = run(
        &dir,
        &["ingest", "--dry-run", "--key", "a.md", "--force-large"],
    );
    assert_eq!(code, 2, "{body}");
    assert_eq!(body["error"]["code"], "E_OVERSIZE");
    assert!(!dir.path().join(".wiki/raw/a.md").exists());
}

/// An oversize source is never converted. Silence is the failure mode: the
/// author would read "converted: 0, exit 0" as "nothing to do" and never learn
/// that one file is above the limit.
#[test]
fn an_oversize_source_is_named_in_the_warnings_of_every_round() {
    let dir = repo();
    let mut cfg = llm_wiki::config::Config::load(dir.path()).unwrap();
    cfg.parse.max_source_bytes = 4;
    fs::write(
        dir.path().join(".wiki/wiki.toml"),
        toml::to_string(&cfg).unwrap(),
    )
    .unwrap();
    write_source(&dir, "big.md", "far too large for the configured budget\n");

    let (code, body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(
        code, 0,
        "an oversize source is a warning, not a failure: {body}"
    );
    assert_eq!(body["data"]["converted"], 0, "{body}");
    let warnings = body["data"]["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|warning| {
            let text = warning.as_str().unwrap_or_default();
            text.contains("oversize")
                && text.contains("big.md")
                && text.contains("max_source_bytes")
        }),
        "the warning must name the file and the limit: {body}"
    );

    // The human rendering is the one a person actually reads.
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .current_dir(dir.path())
        .arg("ingest")
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("warning: oversize: 'big.md'"),
        "human output must show the skipped file:\n{text}"
    );
}

#[test]
fn a_round_names_the_page_it_handed_over_that_is_still_unfinished() {
    let dir = repo();
    write_source(&dir, "a.md", "# A\n\n订单超时后重试三次。\n");
    settle(&dir, "a.md");
    let (code, _body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(code, 0);

    let page = dir.path().join(".wiki/pages/concept/alpha.md");
    fs::create_dir_all(page.parent().unwrap()).unwrap();
    fs::write(
        &page,
        "---\nid: alpha\ntitle: alpha\nstatus: draft\nupdated: 2026-11-10\nsources: [.wiki/raw/a.md]\n---\n\nTODO: 补上重试次数的出处。\n",
    )
    .unwrap();

    let (code, body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(code, 0, "{body}");
    let warnings = body["data"]["warnings"].as_array().unwrap();
    assert!(
        warnings.iter().any(|warning| {
            let text = warning.as_str().unwrap_or_default();
            text.contains(".wiki/pages/concept/alpha.md") && text.contains("unfinished marker")
        }),
        "an unfinished page is named in the round that handed it over: {body}"
    );
}
