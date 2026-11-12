//! `ingest --reconvert`: an explicit, protected re-run for sources whose raw
//! file was left wrong by an older backend.
use std::fs;

use assert_cmd::Command;
use serde_json::Value;
use tempfile::TempDir;

fn run(dir: &TempDir, args: &[&str]) -> (i32, Value) {
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .current_dir(dir.path())
        .arg("--json")
        .args(args)
        .output()
        .unwrap();
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    let data = match value.get("data") {
        Some(data) => data.clone(),
        None => value,
    };
    (out.status.code().unwrap(), data)
}

/// Runs a command that may fail before producing a report.
fn run_plain(dir: &TempDir, args: &[&str]) -> (i32, String) {
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .current_dir(dir.path())
        .args(args)
        .output()
        .unwrap();
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    (out.status.code().unwrap(), text)
}

fn fresh() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    llm_wiki::init::run(dir.path(), false).unwrap();
    fs::write(dir.path().join("a.md"), "# A\n\nEvidence A\n").unwrap();
    dir
}

fn ingested() -> TempDir {
    let dir = fresh();
    let (code, body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(code, 0, "{body}");
    dir
}

fn raw_path(dir: &TempDir, key: &str) -> std::path::PathBuf {
    dir.path().join(".wiki").join("raw").join(key)
}

/// Rewrites the ledger's digest for a raw, so a hand-written file looks like
/// the output of the last conversion — the state a fixed engine finds in a
/// repository converted by an older one.
fn forge_raw_digest(dir: &TempDir, key: &str) {
    let bytes = fs::read(raw_path(dir, key)).unwrap();
    let digest = llm_wiki::hash::hash_bytes(&bytes);
    let conn = rusqlite::Connection::open(dir.path().join(".wiki/state.db")).unwrap();
    let changed = conn
        .execute(
            "UPDATE raws SET hash=?1 WHERE key=?2",
            rusqlite::params![digest, key],
        )
        .unwrap();
    assert_eq!(
        changed, 1,
        "the raw '{key}' must have a ledger row to forge"
    );
}

#[test]
fn a_reconvert_rebuilds_a_raw_the_old_backend_wrote() {
    let dir = ingested();
    let path = raw_path(&dir, "a.md");
    let good = fs::read_to_string(&path).unwrap();
    let close = good[4..].find("\n---\n").unwrap() + 4;
    let frontmatter = &good[..close + 5];
    // What the old PDF backend published: a frontmatter that names the source
    // and a body that is only a link.
    let placeholder = format!("{frontmatter}[attachment](.wiki/assets/a.bin)\n");
    fs::write(&path, &placeholder).unwrap();
    forge_raw_digest(&dir, "a.md");

    let (code, body) = run(&dir, &["ingest", "--reconvert", "a.md", "--force"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["converted"], 1, "{body}");
    let raw = fs::read_to_string(&path).unwrap();
    assert!(
        raw.contains("Evidence A"),
        "the raw must hold the text: {raw}"
    );
    assert!(!raw.contains(".wiki/assets/a.bin"), "{raw}");
}

#[test]
fn a_reconvert_blocks_the_root_and_a_replan_recovers() {
    let dir = ingested();
    let root = run(&dir, &["task", "status"]).1["roots"][0]["id"]
        .as_i64()
        .unwrap();
    let path = raw_path(&dir, "a.md");
    let good = fs::read_to_string(&path).unwrap();
    let close = good[4..].find("\n---\n").unwrap() + 4;
    fs::write(&path, format!("{}only a link\n", &good[..close + 5])).unwrap();
    forge_raw_digest(&dir, "a.md");
    // `converted_at` has one-second resolution: wait so the rebuilt raw cannot
    // be byte-identical to the one the root was planned from.
    std::thread::sleep(std::time::Duration::from_millis(1100));

    let (code, body) = run(&dir, &["ingest", "--reconvert", "a.md", "--force"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["converted"], 1, "{body}");
    let (_, status) = run(&dir, &["task", "status"]);
    let row = &status["roots"][0];
    assert_eq!(row["id"], root);
    assert_eq!(row["status"], "blocked", "{status}");
    assert!(
        row["last_error"]
            .as_str()
            .unwrap()
            .contains("E_SOURCE_DRIFT"),
        "{status}"
    );

    // The documented way out: rebuild the plan from the new raw.
    let replan = format!("task retry {root} --replan");
    let args: Vec<&str> = replan.split(' ').collect();
    let (code, body) = run(&dir, &args);
    assert_eq!(code, 0, "{body}");
    let (_, status) = run(&dir, &["task", "status"]);
    assert_eq!(status["roots"][0]["status"], "active", "{status}");
    assert!(status["roots"][0]["last_error"].is_null(), "{status}");
}

/// A hand-pinned raw may predate the `parser:` field or have lost it; the pin
/// alone must be enough, both for a real reconvert and for its preview.
#[test]
fn a_pin_without_a_parser_field_is_honoured() {
    let dir = ingested();
    let path = raw_path(&dir, "a.md");
    let pinned = "---\nsource: a.md\npin: true\n---\nhand-written notes\n";
    fs::write(&path, pinned).unwrap();

    let (code, body) = run(
        &dir,
        &["ingest", "--reconvert", "a.md", "--dry-run", "--force"],
    );
    assert_eq!(code, 0, "{body}");
    let warnings = body["warnings"].to_string();
    assert!(warnings.contains("would be skipped (pinned)"), "{body}");

    let (code, body) = run(&dir, &["ingest", "--reconvert", "a.md", "--force"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["converted"], 0, "{body}");
    assert!(
        body["warnings"].to_string().contains("skipped (pinned)"),
        "{body}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), pinned);
}

/// The per-round budget applies to a reconvert round too: the keys past the
/// limit stay waiting, under the same names a normal round would report.
#[test]
fn a_reconvert_respects_the_round_budget() {
    let dir = ingested();
    fs::write(dir.path().join("b.md"), "# B\n\nEvidence B\n").unwrap();
    let (code, body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(code, 0, "{body}");
    let config = dir.path().join(".wiki/wiki.toml");
    let text = fs::read_to_string(&config).unwrap();
    let text = text.replace("max_per_run         = 20", "max_per_run         = 1");
    fs::write(&config, text).unwrap();

    let (code, body) = run(&dir, &["ingest", "--reconvert", "a.md", "b.md", "--force"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["converted"], 1, "{body}");
    assert_eq!(body["remaining"]["waiting"], 1, "{body}");
    assert_eq!(body["waiting"], serde_json::json!(["b.md"]), "{body}");
}

#[test]
fn a_reconvert_requires_keys_and_refuses_other_selectors() {
    let dir = fresh();
    for args in [
        vec!["ingest", "--reconvert"],
        vec!["ingest", "--reconvert", "a.md", "--auto"],
        vec!["ingest", "--reconvert", "a.md", "--key", "a.md"],
        vec!["ingest", "--reconvert", "a.md", "--chunk", "1"],
        vec!["ingest", "--reconvert", "a.md", "--range", "1"],
        vec!["ingest", "--reconvert", "a.md", "--force-large"],
    ] {
        let (code, text) = run_plain(&dir, &args);
        assert_eq!(code, 2, "{args:?} must be a usage error: {text}");
        assert!(
            text.contains("cannot be used with") || text.contains("required"),
            "{args:?}: {text}"
        );
    }
    // The two allowed companions.
    for args in [
        vec!["ingest", "--reconvert", "a.md", "--dry-run"],
        vec!["ingest", "--reconvert", "a.md", "--force"],
    ] {
        let (code, text) = run_plain(&dir, &args);
        assert_eq!(code, 0, "{args:?}: {text}");
    }
}

#[test]
fn a_reconvert_refuses_keys_that_are_not_plain_names() {
    let dir = fresh();
    for key in [
        "*.md",
        "../a.md",
        "/etc/passwd",
        "a//b.md",
        "a/./b.md",
        "a\\b.md",
        "",
    ] {
        let (code, text) = run_plain(&dir, &["ingest", "--reconvert", key]);
        assert_eq!(code, 2, "'{key}' must be refused: {text}");
        assert!(text.contains("--reconvert"), "{text}");
    }
}

#[test]
fn a_reconvert_reports_keys_it_cannot_use() {
    let dir = ingested();
    fs::write(dir.path().join("b.md"), "# B\n\nEvidence B\n").unwrap();
    let (code, body) = run(
        &dir,
        &[
            "ingest",
            "--reconvert",
            "missing.md",
            "a.md",
            "a.md",
            "--force",
        ],
    );
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["converted"], 1, "a duplicate must not convert twice");
    let warnings = body["warnings"].to_string();
    assert!(
        warnings.contains("no source has the key 'missing.md'"),
        "{body}"
    );
    assert!(
        !warnings.contains("'b.md'"),
        "only named keys are taken: {body}"
    );
}

#[test]
fn a_reconvert_of_a_settling_source_waits() {
    let dir = fresh();
    let (code, body) = run(&dir, &["ingest", "--reconvert", "a.md"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["converted"], 0, "{body}");
    assert!(body["warnings"].to_string().contains("settle:"), "{body}");

    let (code, body) = run(&dir, &["ingest", "--reconvert", "a.md", "--force"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["converted"], 1, "{body}");
}

#[test]
fn a_reconvert_leaves_a_pinned_raw_alone() {
    let dir = ingested();
    let path = raw_path(&dir, "a.md");
    let raw = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("---\npin: true\n{}", &raw[4..])).unwrap();
    forge_raw_digest(&dir, "a.md");

    let (code, body) = run(&dir, &["ingest", "--reconvert", "a.md", "--force"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["converted"], 0, "{body}");
    assert!(
        body["warnings"].to_string().contains("skipped (pinned)"),
        "{body}"
    );
    assert!(fs::read_to_string(&path).unwrap().contains("pin: true"));
}

#[test]
fn a_reconvert_dry_run_checks_protection_and_writes_nothing() {
    let dir = ingested();
    let path = raw_path(&dir, "a.md");
    let good = fs::read_to_string(&path).unwrap();
    let close = good[4..].find("\n---\n").unwrap() + 4;
    let edited = format!("{}only a link\n", &good[..close + 5]);
    fs::write(&path, &edited).unwrap();
    // The ledger still records the bytes the conversion wrote, so this looks
    // like a hand edit: the dry run must say so instead of promising a rewrite.
    let pages_before = fs::read_dir(dir.path().join(".wiki")).unwrap().count();

    let (code, body) = run(
        &dir,
        &["ingest", "--reconvert", "a.md", "--dry-run", "--force"],
    );
    assert_eq!(code, 0, "{body}");
    let warnings = body["warnings"].to_string();
    assert!(warnings.contains("may have been edited"), "{body}");
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        edited,
        "the raw must not move"
    );
    assert_eq!(
        fs::read_dir(dir.path().join(".wiki")).unwrap().count(),
        pages_before,
        "a dry run must not create engine files"
    );
}

#[test]
fn a_reconvert_failure_does_not_stop_the_next_key() {
    let dir = fresh();
    // Only frontmatter: the text backend has nothing usable to publish.
    fs::write(dir.path().join("bad.md"), "---\ntitle: x\n---\n").unwrap();

    let (code, body) = run(
        &dir,
        &["ingest", "--reconvert", "bad.md", "a.md", "--force"],
    );
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["failures"], 1, "{body}");
    assert_eq!(body["converted"], 1, "{body}");
    assert!(raw_path(&dir, "a.md").exists());
}

#[test]
fn an_unsupported_file_is_named_in_the_round_report() {
    let dir = fresh();
    fs::write(dir.path().join("notes.docx"), "binary").unwrap();

    let (code, body) = run(&dir, &["ingest", "--force"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["failures"], 0, "{body}");
    let warnings = body["warnings"].to_string();
    assert!(warnings.contains("unsupported: 'notes.docx'"), "{body}");
    assert!(!raw_path(&dir, "notes.docx").exists());
}
