//! Signature test for the source-layer scan.
//!
//! The first test is the plan's original scenario (New / Duplicate / Moved); the
//! rest cover the change kinds it does not reach (Changed / Missing / Oversize),
//! the read-only idempotency guarantee, and the JSON shape of `llm-wiki scan`.

use assert_cmd::Command;
use llm_wiki::store::SourceRow;
use llm_wiki::{config::Config, init, scan, store::Store};
use std::fs;
use tempfile::tempdir;

fn setup() -> (tempfile::TempDir, Store, Config) {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    (d, s, cfg)
}

/// A ledger row carrying only the fields the classifier reads.
fn ledger_row(key: &str, hash: &str) -> SourceRow {
    SourceRow {
        key: String::from(key),
        hash: String::from(hash),
        size: 4,
        mtime: 1,
        raw_key: None,
        converted_at: None,
    }
}

#[test]
fn classifies_new_changed_moved_missing_duplicate() {
    let (d, mut s, cfg) = setup();
    let src = d.path().join("source");
    fs::write(src.join("a.md"), "alpha").unwrap();
    fs::write(src.join("b.md"), "same").unwrap();
    fs::write(src.join("c.md"), "same").unwrap();
    fs::write(src.join("x.unknownext"), "??").unwrap();

    let rep = scan::scan_source(d.path(), &cfg, &s).unwrap();
    let kinds = |k| rep.changes.iter().filter(|c| c.kind == k).count();
    assert_eq!(kinds(scan::ChangeKind::New), 2);
    assert_eq!(kinds(scan::ChangeKind::Duplicate), 1);
    assert_eq!(rep.ignored_count, 1);

    let b = rep.changes.iter().find(|c| c.key == "b.md").unwrap();
    s.upsert_source(&llm_wiki::store::SourceRow {
        key: "b.md".into(),
        hash: b.hash.clone().unwrap(),
        size: 4,
        mtime: 1,
        raw_key: None,
        converted_at: None,
    })
    .unwrap();
    fs::rename(src.join("b.md"), src.join("b2.md")).unwrap();
    let rep = scan::scan_source(d.path(), &cfg, &s).unwrap();
    let moved = rep
        .changes
        .iter()
        .find(|c| c.kind == scan::ChangeKind::Moved)
        .unwrap();
    assert_eq!(moved.moved_from.as_deref(), Some("b.md"));
    assert_eq!(moved.key, "b2.md");
}

#[test]
fn reports_changed_when_the_digest_differs_from_the_ledger() {
    let (d, mut s, cfg) = setup();
    let src = d.path().join("source");
    fs::write(src.join("a.md"), "alpha").unwrap();

    let first = scan::scan_source(d.path(), &cfg, &s).unwrap();
    let digest = first
        .changes
        .iter()
        .find(|c| c.key == "a.md")
        .unwrap()
        .hash
        .clone()
        .unwrap();
    assert!(digest.starts_with("blake3:"));

    // A stale ledger row: same key, different content digest.
    s.upsert_source(&ledger_row(
        "a.md",
        "blake3:0000000000000000000000000000000000000000000000000000000000000000",
    ))
    .unwrap();

    let rep = scan::scan_source(d.path(), &cfg, &s).unwrap();
    let changed: Vec<_> = rep
        .changes
        .iter()
        .filter(|c| c.kind == scan::ChangeKind::Changed)
        .collect();
    assert_eq!(changed.len(), 1);
    assert_eq!(changed[0].key, "a.md");
    assert_eq!(changed[0].hash.as_deref(), Some(digest.as_str()));
    assert_eq!(changed[0].moved_from, None);
}

#[test]
fn reports_missing_for_a_ledger_key_no_longer_on_disk() {
    let (d, mut s, cfg) = setup();
    s.upsert_source(&ledger_row("gone.md", "blake3:aa"))
        .unwrap();

    let rep = scan::scan_source(d.path(), &cfg, &s).unwrap();
    let missing = rep.changes.iter().find(|c| c.key == "gone.md").unwrap();
    assert_eq!(missing.kind, scan::ChangeKind::Missing);
    assert_eq!(missing.hash, None);
    assert_eq!(missing.moved_from, None);
}

#[test]
fn reports_oversize_without_hashing() {
    let (d, s, _) = setup();
    // A ten-byte file against a four-byte cap must not be read.
    let path = d.path().join("wiki.toml");
    let text = fs::read_to_string(&path).unwrap();
    let text = text.replace("max_source_bytes = \"100MB\"", "max_source_bytes = \"4\"");
    fs::write(&path, text).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    assert_eq!(cfg.parse.max_source_bytes, 4);

    fs::write(d.path().join("source/big.md"), "0123456789").unwrap();
    let rep = scan::scan_source(d.path(), &cfg, &s).unwrap();
    let oversize = rep.changes.iter().find(|c| c.key == "big.md").unwrap();
    assert_eq!(oversize.kind, scan::ChangeKind::Oversize);
    assert_eq!(oversize.hash, None);
}

#[test]
fn scanning_twice_yields_the_same_report() {
    let (d, s, cfg) = setup();
    let src = d.path().join("source");
    fs::write(src.join("a.md"), "alpha").unwrap();
    fs::write(src.join("b.md"), "same").unwrap();
    fs::write(src.join("c.md"), "same").unwrap();
    fs::write(src.join("x.unknownext"), "??").unwrap();

    let first = scan::scan_source(d.path(), &cfg, &s).unwrap();
    let second = scan::scan_source(d.path(), &cfg, &s).unwrap();
    assert_eq!(first, second);
    // Reading the ledger must not have changed it.
    assert!(s.all_sources().unwrap().is_empty());
}

#[test]
fn keys_are_relative_to_the_source_directory() {
    let (d, s, cfg) = setup();
    fs::create_dir_all(d.path().join("source/sub")).unwrap();
    fs::write(d.path().join("source/sub/note.md"), "nested").unwrap();

    let rep = scan::scan_source(d.path(), &cfg, &s).unwrap();
    assert_eq!(rep.changes.len(), 1);
    assert_eq!(rep.changes[0].kind, scan::ChangeKind::New);
    assert_eq!(rep.changes[0].key, "sub/note.md");
}

#[test]
fn extension_matching_ignores_case_and_dot_paths_are_skipped() {
    let (d, s, cfg) = setup();
    let src = d.path().join("source");
    fs::write(src.join("NOTE.MD"), "upper").unwrap();
    fs::write(src.join(".draft.md"), "hidden").unwrap();
    fs::create_dir_all(src.join(".git")).unwrap();
    fs::write(src.join(".git/inside.md"), "hidden").unwrap();

    let rep = scan::scan_source(d.path(), &cfg, &s).unwrap();
    assert_eq!(rep.ignored_count, 0);
    assert_eq!(rep.changes.len(), 1);
    assert_eq!(rep.changes[0].key, "NOTE.MD");
    assert_eq!(rep.changes[0].kind, scan::ChangeKind::New);
}

#[test]
fn cli_scan_emits_json_changes() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    fs::write(d.path().join("source/a.md"), "alpha").unwrap();

    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json", "scan"])
        .current_dir(d.path())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["ok"], true);
    assert_eq!(v["cmd"], "scan");
    assert_eq!(v["data"]["ignored_count"], 0);
    assert_eq!(v["data"]["changes"][0]["kind"], "new");
    assert_eq!(v["data"]["changes"][0]["key"], "a.md");
    assert!(v["data"]["changes"][0]["hash"].is_string());
    assert!(v["data"]["changes"][0]["moved_from"].is_null());
}

/// Runs `llm-wiki --json <args>` in `dir` and returns the parsed envelope.
fn cli(dir: &std::path::Path, args: &[&str]) -> serde_json::Value {
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .arg("--json")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
    serde_json::from_slice(&out.stdout).unwrap()
}

/// Converts one source through the CLI, so both ledger layers have a row.
fn convert_one(dir: &std::path::Path, key: &str) {
    let v = cli(dir, &["convert", "--key", key]);
    assert_eq!(v["ok"], true, "{v}");
}

#[test]
fn cli_scan_reports_a_hand_edited_raw_file_under_the_raw_layer() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    fs::write(d.path().join("source/a.md"), "# A\n\nalpha body\n").unwrap();
    convert_one(d.path(), "a.md");

    // Right after `convert` both layers are in sync.
    let clean = cli(d.path(), &["scan"]);
    assert_eq!(
        clean["data"]["changes"].as_array().unwrap().len(),
        0,
        "a fresh conversion must leave nothing pending: {clean}"
    );

    // A hand edit to the raw layer is invisible to the source layer, so only
    // the raw half of the scan can report it.
    let raw = d.path().join("raw/a.md");
    let mut text = fs::read_to_string(&raw).unwrap();
    text.push_str("\nhand edit\n");
    fs::write(&raw, text).unwrap();

    let edited = cli(d.path(), &["scan"]);
    let changes = edited["data"]["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "only the raw layer changed: {edited}");
    assert_eq!(changes[0]["kind"], "changed");
    assert_eq!(changes[0]["key"], "a.md");
    assert_eq!(changes[0]["layer"], "raw", "{edited}");
    assert!(changes[0]["hash"].is_string());
}

#[test]
fn cli_scan_merges_both_layers_and_labels_each_change() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    fs::write(d.path().join("source/a.md"), "# A\n\nalpha body\n").unwrap();
    convert_one(d.path(), "a.md");

    // One pending source in the source layer and one edited raw file in the raw
    // layer: the merged list must carry both, each with its own layer.
    fs::write(d.path().join("source/b.md"), "# B\n\nbeta body\n").unwrap();
    let raw = d.path().join("raw/a.md");
    let mut text = fs::read_to_string(&raw).unwrap();
    text.push_str("\nhand edit\n");
    fs::write(&raw, text).unwrap();

    let merged = cli(d.path(), &["scan"]);
    let changes = merged["data"]["changes"].as_array().unwrap();
    let pairs: Vec<(&str, &str, &str)> = changes
        .iter()
        .map(|c| {
            (
                c["key"].as_str().unwrap(),
                c["kind"].as_str().unwrap(),
                c["layer"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        pairs,
        vec![("a.md", "changed", "raw"), ("b.md", "new", "source")],
        "{merged}"
    );
}

#[test]
fn cli_scan_is_idempotent_across_both_layers() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    fs::write(d.path().join("source/a.md"), "# A\n\nalpha body\n").unwrap();
    convert_one(d.path(), "a.md");
    fs::write(d.path().join("source/b.md"), "# B\n\nbeta body\n").unwrap();
    let raw = d.path().join("raw/a.md");
    let mut text = fs::read_to_string(&raw).unwrap();
    text.push_str("\nhand edit\n");
    fs::write(&raw, text).unwrap();

    let first = cli(d.path(), &["scan"]);
    let second = cli(d.path(), &["scan"]);
    assert_eq!(first, second, "scan must be read-only and idempotent");

    // Both findings are still there on a repeat run.
    assert_eq!(first["data"]["changes"].as_array().unwrap().len(), 2);
    assert_eq!(first["data"]["ignored_count"], 0);
}
