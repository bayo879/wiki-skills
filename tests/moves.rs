//! `moved`: a renamed source must keep its raw identity and its pages.
//!
//! `scan` only reports the rename. The migration itself lives behind
//! `scan --apply-moves` (and runs inside `sync`), because a read-only command
//! that silently rewrote pages would break the "scan twice, same answer" rule.
//! These tests pin what a migration has to move: the ledger row, the raw file
//! and its `source:` field, the `source_pages` mapping, and every page's
//! `sources:` list — and that running it again changes nothing.

use assert_cmd::Command;
use llm_wiki::page::{self, Status};
use llm_wiki::store::Store;
use llm_wiki::{hash, init};
use serde_json::Value;
use std::fs;
use std::path::Path;
use tempfile::tempdir;

/// Runs `llm-wiki --json <args>` in `dir` and returns (exit code, envelope).
fn cli(dir: &Path, args: &[&str]) -> (i32, Value) {
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .arg("--json")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    let code = out.status.code().expect("llm-wiki exits normally");
    let value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout is not JSON: {}\nstderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    });
    (code, value)
}

/// Opens (and migrates) the ledger under `root`.
fn open(root: &Path) -> Store {
    let mut store = Store::open(&root.join(".wiki/state.db")).unwrap();
    store.migrate().unwrap();
    store
}

/// Writes one page under `wiki/` with the frontmatter the tests care about.
fn write_page(root: &Path, key: &str, status: Status, sources: &[&str]) {
    let mut text = String::from("---\n");
    text.push_str(&format!("status: {status}\n"));
    text.push_str("updated: 2026-10-27\n");
    text.push_str("sources:\n");
    for source in sources {
        text.push_str(&format!("  - {source}\n"));
    }
    text.push_str("---\n\nbody\n");
    let path = root.join(key);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn apply_moves_renames_the_source_and_every_reference_to_it() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    fs::write(d.path().join("source/a.md"), "# A\n\nalpha body\n").unwrap();
    let (code, v) = cli(d.path(), &["convert", "--key", "a.md"]);
    assert_eq!(code, 0, "{v}");
    write_page(
        d.path(),
        "wiki/concept/alpha.md",
        Status::Stable,
        &["raw/a.md"],
    );
    let mut store = open(d.path());
    store
        .set_source_pages("raw/a.md", &[String::from("wiki/concept/alpha.md")])
        .unwrap();
    drop(store);

    fs::rename(d.path().join("source/a.md"), d.path().join("source/b.md")).unwrap();

    // `scan` reports the rename and writes nothing.
    let (code, v) = cli(d.path(), &["scan"]);
    assert_eq!(code, 0, "{v}");
    let changes = v["data"]["changes"].as_array().unwrap();
    assert_eq!(changes.len(), 1, "{v}");
    assert_eq!(changes[0]["kind"], "moved");
    assert_eq!(changes[0]["key"], "b.md");
    assert_eq!(changes[0]["moved_from"], "a.md");
    assert_eq!(changes[0]["layer"], "source");
    assert!(d.path().join("raw/a.md").is_file(), "scan is read-only");
    let (fm, _) = page::read(&d.path().join("wiki/concept/alpha.md")).unwrap();
    assert_eq!(fm.sources, vec!["raw/a.md"], "scan is read-only");

    // `--apply-moves` performs the migration.
    let (code, v) = cli(d.path(), &["scan", "--apply-moves"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true);

    let (fm, _) = page::read(&d.path().join("wiki/concept/alpha.md")).unwrap();
    assert_eq!(fm.sources, vec!["raw/b.md"]);
    assert!(d.path().join("raw/b.md").is_file());
    assert!(!d.path().join("raw/a.md").exists());
    let raw = fs::read_to_string(d.path().join("raw/b.md")).unwrap();
    assert!(
        raw.contains("source: source/b.md"),
        "the raw frontmatter points at the new source: {raw}"
    );

    let store = open(d.path());
    assert!(store.get_source("a.md").unwrap().is_none());
    let row = store.get_source("b.md").unwrap().expect("a ledger row");
    assert_eq!(row.raw_key.as_deref(), Some("raw/b.md"));
    assert!(!store
        .pages_for_source("raw/a.md")
        .unwrap()
        .iter()
        .any(|page| page == "wiki/concept/alpha.md"));
    assert_eq!(
        store.pages_for_source("raw/b.md").unwrap(),
        vec!["wiki/concept/alpha.md"]
    );
    // The `raws` ledger speaks raw-relative keys (`scan_raw` and `convert` do),
    // so the rename has to move the row in that key space, not the repo one.
    assert!(store.get_raw("a.md").unwrap().is_none());
    assert!(store.get_raw("b.md").unwrap().is_some());
    let meta = store
        .get_page_meta("wiki/concept/alpha.md")
        .unwrap()
        .expect("an updated page_meta record");
    assert_eq!(
        meta.hash,
        hash::hash_file(&d.path().join("wiki/concept/alpha.md"), true).unwrap(),
        "the page rewrite does not leave lint L9 drift behind"
    );

    // Idempotent: the rename is gone, and applying again is a no-op.
    let (code, v) = cli(d.path(), &["scan"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["data"]["changes"].as_array().unwrap().len(), 0, "{v}");
    let (code, v) = cli(d.path(), &["scan", "--apply-moves"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true);
    assert!(v["data"]["moved"]["applied"].as_array().unwrap().is_empty());
}

#[test]
fn apply_moves_refuses_to_overwrite_the_destination_raw_file() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    fs::write(d.path().join("source/a.md"), "# A\n\nalpha body\n").unwrap();
    assert_eq!(cli(d.path(), &["convert", "--key", "a.md"]).0, 0);
    write_page(
        d.path(),
        "wiki/concept/alpha.md",
        Status::Stable,
        &["raw/a.md"],
    );
    fs::rename(d.path().join("source/a.md"), d.path().join("source/b.md")).unwrap();
    // A hand-maintained raw/b.md is already there; the migration must not eat it.
    fs::write(d.path().join("raw/b.md"), "hand written, keep me\n").unwrap();

    let (code, v) = cli(d.path(), &["scan", "--apply-moves"]);
    assert_eq!(code, 1, "a collision is a finding: {v}");
    assert_eq!(v["ok"], true);
    let collisions = v["data"]["moved"]["collisions"].as_array().unwrap();
    assert_eq!(collisions.len(), 1, "{v}");
    assert_eq!(collisions[0]["from"], "a.md");
    assert_eq!(collisions[0]["to"], "b.md");
    assert_eq!(collisions[0]["code"], "E_MOVED_COLLISION");

    assert_eq!(
        fs::read_to_string(d.path().join("raw/b.md")).unwrap(),
        "hand written, keep me\n"
    );
    let (fm, _) = page::read(&d.path().join("wiki/concept/alpha.md")).unwrap();
    assert_eq!(fm.sources, vec!["raw/a.md"]);
    let store = open(d.path());
    assert!(store.get_source("a.md").unwrap().is_some());
    assert!(store.get_source("b.md").unwrap().is_none());
}
