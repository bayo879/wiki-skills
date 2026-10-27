//! The `archive` and `recall` commands as the binary exposes them.
//!
//! The core logic is covered by `tests/archive.rs`; what is checked here is the
//! wiring: the four verbs exist, the envelope carries the action, and the exit
//! codes say what the contract promises. Three of them are easy to get wrong and
//! matter to a caller:
//!
//! - `purge` without `--yes` is exit 3, a human decision, not a failure;
//! - `recall` with no hit is exit 1, a finding;
//! - `seal` without the summary the agent was supposed to write is exit 2, a
//!   broken invocation.

use std::path::Path;

use assert_cmd::Command;
use llm_wiki::store::{SourceRow, Store};
use tempfile::{tempdir, TempDir};

/// Runs `llm-wiki --json <args...>` with `dir` as the working directory.
fn run_json(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

/// Runs `llm-wiki <args...>` in the human-readable format.
fn run_text(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

/// A temp repository with the contract files and directories in place.
fn repo() -> TempDir {
    let dir = tempdir().unwrap();
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "init failed: {}", stderr(&out));
    dir
}

fn stderr(out: &std::process::Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

fn stdout_json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not one JSON document ({err}): {:?} / stderr: {}",
            String::from_utf8_lossy(&out.stdout),
            stderr(out)
        )
    })
}

/// Records a source key in the ledger, the way `scan` would.
fn ledger(root: &Path, key: &str) {
    let mut store = Store::open(&root.join(".wiki/state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_source(&SourceRow {
            key: String::from(key),
            hash: String::from("blake3:x"),
            size: 1,
            mtime: 0,
            raw_key: Some(String::from(key)),
            converted_at: None,
        })
        .unwrap();
}

/// Writes the summary the agent is expected to hand to `seal`.
fn put_summary(root: &Path, id: &str) {
    let path = root.join("wiki/archive").join(format!("{id}.md"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(
            "---\nid: {id}\ntitle: 2026 年 5 月\ntype: concept\nsummary: 五月会话\n\
             archived: true\nstatus: stable\nupdated: 2026-05-31\n---\n\n五月做完了转换器。\n"
        ),
    )
    .unwrap();
}

/// Writes a page that cites `sources`.
fn put_page(root: &Path, sources: &[&str]) {
    let list: String = sources.iter().map(|s| format!("  - {s}\n")).collect();
    let path = root.join("wiki/concept/sync.md");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(
            "---\nid: sync\ntitle: 同步\ntype: concept\nsummary: 摘要\nsources:\n{list}\
             status: stable\nupdated: 2026-05-01\n---\n\n这一页的正文足够长，写清楚前因后果，测试才会认为它达标。\n"
        ),
    )
    .unwrap();
}

// ------------------------------------------------------------------- status

#[test]
fn archive_status_reports_the_four_numbers() {
    let d = repo();
    let out = run_json(d.path(), &["archive", "status"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let v = stdout_json(&out);
    assert_eq!(v["ok"], true);
    assert_eq!(v["cmd"], "archive");
    assert_eq!(v["data"]["action"], "status");
    assert_eq!(v["data"]["archives"], 0);
    assert_eq!(v["data"]["live_sessions"], 0);
    assert_eq!(v["data"]["purge_candidates"], 0);
    assert_eq!(v["data"]["still_referenced"], 0);
}

#[test]
fn archive_status_prints_four_lines_for_a_human() {
    let d = repo();
    let out = run_text(d.path(), &["archive", "status"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = String::from_utf8(out.stdout).unwrap();
    assert_eq!(
        text.lines().count(),
        4,
        "status is four lines of numbers, got: {text:?}"
    );
    for needle in [
        "summaries",
        "live sessions",
        "purge candidates",
        "still referenced",
    ] {
        assert!(text.contains(needle), "missing `{needle}` in {text:?}");
    }
}

// -------------------------------------------------------------------- purge

#[test]
fn archive_purge_dry_run_reports_candidates_and_deletes_nothing() {
    let d = repo();
    let out = run_json(d.path(), &["archive", "purge", "--dry-run"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let v = stdout_json(&out);
    assert_eq!(v["cmd"], "archive");
    assert_eq!(v["data"]["action"], "purge");
    assert_eq!(v["data"]["dry_run"], true);
    assert_eq!(v["data"]["deleted"], 0);
    assert!(v["data"]["candidates"].is_array());
    assert!(v["data"]["candidates"].as_array().unwrap().is_empty());
}

#[test]
fn archive_purge_without_yes_exits_3() {
    let d = repo();
    let out = run_json(d.path(), &["archive", "purge"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "a purge that needs a human is not a failure; stderr: {}",
        stderr(&out)
    );
    let v = stdout_json(&out);
    assert_eq!(v["ok"], false);
    assert_eq!(v["cmd"], "archive");
    assert_eq!(v["error"]["code"], "E_NEEDS_CONFIRMATION");
}

// --------------------------------------------------------------------- seal

#[test]
fn archive_seal_without_a_summary_exits_2() {
    let d = repo();
    ledger(d.path(), "queries/2026-05.md");
    let out = run_json(
        d.path(),
        &[
            "archive",
            "seal",
            "--sources",
            "queries/2026-05.md",
            "--id",
            "2026-05",
            "--period-end",
            "2026-05-31",
        ],
    );
    assert_eq!(out.status.code(), Some(2), "stderr: {}", stderr(&out));
    let v = stdout_json(&out);
    assert_eq!(v["ok"], false);
    assert_eq!(v["cmd"], "archive");
    // The agent forgot the body; that is the caller's mistake, and the message
    // names the file it expected.
    assert_eq!(v["error"]["code"], "E_NO_SUMMARY");
    assert!(
        v["error"]["message"]
            .as_str()
            .unwrap()
            .contains("wiki/archive/2026-05.md"),
        "the message names the missing summary: {}",
        v["error"]["message"]
    );
}

#[test]
fn archive_seal_rehangs_the_citing_page_and_records_the_period() {
    let d = repo();
    let repo = d.path();
    ledger(repo, "queries/2026-05.md");
    ledger(repo, "docs/a.md");
    put_page(repo, &["raw/queries/2026-05.md", "raw/docs/a.md"]);
    put_summary(repo, "2026-05");

    let out = run_text(
        repo,
        &[
            "archive",
            "seal",
            "--sources",
            "queries/2026-05.md",
            "--id",
            "2026-05",
            "--period-end",
            "2026-05-31",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        "sealed wiki/archive/2026-05.md (1 pages re-hung)"
    );

    // The sealed source is replaced by the summary; the source nobody sealed
    // stays where it was.
    let page = std::fs::read_to_string(repo.join("wiki/concept/sync.md")).unwrap();
    assert!(page.contains("wiki/archive/2026-05.md"), "{page}");
    assert!(!page.contains("queries/2026-05.md"), "{page}");
    assert!(page.contains("docs/a.md"), "{page}");

    let status = stdout_json(&run_json(repo, &["archive", "status"]));
    assert_eq!(status["data"]["archives"], 1);
    assert_eq!(status["data"]["still_referenced"], 1);
}

#[test]
fn archive_seal_reports_the_summary_in_json() {
    let d = repo();
    let repo = d.path();
    ledger(repo, "queries/2026-05.md");
    put_page(repo, &["raw/queries/2026-05.md"]);
    put_summary(repo, "2026-05");

    let out = run_json(
        repo,
        &[
            "archive",
            "seal",
            "--sources",
            "queries/2026-05.md",
            "--id",
            "2026-05",
            "--period-end",
            "2026-05-31",
        ],
    );
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let v = stdout_json(&out);
    assert_eq!(v["cmd"], "archive");
    assert_eq!(v["data"]["action"], "seal");
    assert_eq!(v["data"]["key"], "wiki/archive/2026-05.md");
    assert_eq!(
        v["data"]["sources"],
        serde_json::json!(["queries/2026-05.md"])
    );
    assert_eq!(
        v["data"]["pages"],
        serde_json::json!(["wiki/concept/sync.md"])
    );
    assert_eq!(v["data"]["updated_pages"], 1);
}

// ------------------------------------------------------------------- recall

#[test]
fn recall_without_a_hit_exits_1() {
    let d = repo();
    let out = run_json(d.path(), &["recall", "根本没有这个词"]);
    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let v = stdout_json(&out);
    assert_eq!(v["ok"], false);
    assert_eq!(v["cmd"], "recall");
    assert_eq!(v["error"]["code"], "E_NO_HIT");
}

#[test]
fn recall_returns_the_summary_it_sealed() {
    let d = repo();
    let repo = d.path();
    ledger(repo, "queries/2026-05.md");
    put_page(repo, &["raw/queries/2026-05.md"]);
    put_summary(repo, "2026-05");
    let seal = run_json(
        repo,
        &[
            "archive",
            "seal",
            "--sources",
            "queries/2026-05.md",
            "--id",
            "2026-05",
            "--period-end",
            "2026-05-31",
        ],
    );
    assert_eq!(seal.status.code(), Some(0), "stderr: {}", stderr(&seal));

    let out = run_text(repo, &["recall", "五月"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let text = String::from_utf8(out.stdout).unwrap();
    let mut lines = text.lines();
    let heading = lines.next().unwrap();
    assert!(
        heading.starts_with("2026 年 5 月 [2026-05-01..2026-05-31] "),
        "heading: {heading:?}"
    );
    assert!(heading.ends_with("wiki/archive/2026-05.md"), "{heading:?}");
    assert_eq!(lines.next(), Some("    五月做完了转换器。"));
}

#[test]
fn recall_reports_the_hits_in_json() {
    let d = repo();
    let repo = d.path();
    ledger(repo, "queries/2026-05.md");
    put_page(repo, &["raw/queries/2026-05.md"]);
    put_summary(repo, "2026-05");
    let seal = run_json(
        repo,
        &[
            "archive",
            "seal",
            "--sources",
            "queries/2026-05.md",
            "--id",
            "2026-05",
            "--period-end",
            "2026-05-31",
        ],
    );
    assert_eq!(seal.status.code(), Some(0), "stderr: {}", stderr(&seal));

    let out = run_json(repo, &["recall", "五月", "--full"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let v = stdout_json(&out);
    assert_eq!(v["cmd"], "recall");
    let hits = v["data"]["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0]["key"], "wiki/archive/2026-05.md");
    assert_eq!(hits[0]["title"], "2026 年 5 月");
    assert_eq!(hits[0]["period_start"], "2026-05-01");
    assert_eq!(hits[0]["period_end"], "2026-05-31");
    assert_eq!(hits[0]["body"], "五月做完了转换器。");
    assert_eq!(v["data"]["missing"], 0);
}
