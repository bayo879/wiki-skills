//! Behaviour of the raw-layer scan and the `raws` ledger.
//!
//! The raw layer is the normalised half of a sync run: `convert` writes
//! `raw/*.md` and records each file's digest, and `scan_raw` compares what is on
//! disk with that record. The scan must stay read-only, must judge only the
//! content (never `converted_at`), and must hash exactly the bytes `convert`
//! wrote, otherwise every run would report the whole raw layer as `New`.

use llm_wiki::hash;
use llm_wiki::{
    config::Config,
    convert, init, scan,
    store::{RawRow, Store},
};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

fn setup() -> (tempfile::TempDir, Store, Config) {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    (d, s, cfg)
}

/// A `raws` row carrying only the fields the scan compares.
fn raw_row(key: &str, hash: &str) -> RawRow {
    RawRow {
        key: String::from(key),
        hash: String::from(hash),
        source_key: String::from("a.pdf"),
        converted_at: String::from("2026-10-27T10:00:00Z"),
    }
}

/// The full digest of the file at `root/key`, as the scan computes it.
fn digest_of(root: &Path, key: &str) -> String {
    hash::hash_file(&root.join(key), true).unwrap()
}

fn count(report: &scan::ScanReport, kind: scan::ChangeKind) -> usize {
    report.changes.iter().filter(|c| c.kind == kind).count()
}

#[test]
fn metadata_only_change_on_source_does_not_trigger_raw_change() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut s = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    s.migrate().unwrap();
    fs::write(d.path().join("raw/a.md"), "正文").unwrap();
    let hash = digest_of(d.path(), "raw/a.md");
    s.upsert_raw(&raw_row("a.md", &hash)).unwrap();

    let changes = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert!(changes
        .changes
        .iter()
        .all(|c| c.kind != scan::ChangeKind::Changed));

    fs::write(d.path().join("raw/a.md"), "正文变了").unwrap();
    let changes = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert_eq!(
        changes
            .changes
            .iter()
            .filter(|c| c.kind == scan::ChangeKind::Changed)
            .count(),
        1
    );
}

#[test]
fn classifies_new_and_keeps_only_the_first_duplicate() {
    let (d, s, cfg) = setup();
    let raw = d.path().join("raw");
    fs::write(raw.join("a.md"), "alpha").unwrap();
    fs::write(raw.join("b.md"), "same").unwrap();
    fs::write(raw.join("c.md"), "same").unwrap();

    let rep = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert_eq!(count(&rep, scan::ChangeKind::New), 2);
    assert_eq!(count(&rep, scan::ChangeKind::Duplicate), 1);
    assert_eq!(rep.ignored_count, 0);

    let kind = |key: &str| rep.changes.iter().find(|c| c.key == key).map(|c| c.kind);
    // The lexicographically first key of a digest is the canonical one.
    assert_eq!(kind("a.md"), Some(scan::ChangeKind::New));
    assert_eq!(kind("b.md"), Some(scan::ChangeKind::New));
    assert_eq!(kind("c.md"), Some(scan::ChangeKind::Duplicate));
    // A duplicate still carries the digest it collided on.
    let c = rep.changes.iter().find(|c| c.key == "c.md").unwrap();
    assert_eq!(c.hash, Some(digest_of(d.path(), "raw/b.md")));
}

#[test]
fn raw_keys_are_relative_to_the_raw_directory() {
    let (d, s, cfg) = setup();
    fs::create_dir_all(d.path().join("raw/sub")).unwrap();
    fs::write(d.path().join("raw/sub/note.md"), "nested").unwrap();

    let rep = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert_eq!(rep.changes.len(), 1);
    assert_eq!(rep.changes[0].kind, scan::ChangeKind::New);
    assert_eq!(rep.changes[0].key, "sub/note.md");
}

#[test]
fn dot_paths_under_raw_are_skipped() {
    let (d, s, cfg) = setup();
    let raw = d.path().join("raw");
    fs::write(raw.join(".draft.md"), "hidden").unwrap();
    fs::create_dir_all(raw.join(".git")).unwrap();
    fs::write(raw.join(".git/inside.md"), "hidden").unwrap();

    let rep = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);
    assert_eq!(rep.ignored_count, 0);
}

#[test]
fn missing_raw_key_is_reported_without_a_hash() {
    let (d, mut s, cfg) = setup();
    s.upsert_raw(&raw_row("gone.md", "blake3:aa")).unwrap();

    let rep = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    let missing = rep.changes.iter().find(|c| c.key == "gone.md").unwrap();
    assert_eq!(missing.kind, scan::ChangeKind::Missing);
    assert_eq!(missing.hash, None);
    assert_eq!(missing.moved_from, None);
}

#[test]
fn pinned_raw_edited_by_hand_is_reported_changed() {
    let (d, mut s, cfg) = setup();
    let raw = d.path().join("raw");
    let pinned = "---\nsource: source/a.txt\nparser: builtin-text\n\
                  converted_at: 2026-10-27T10:00:00Z\npin: true\n---\n正文\n";
    fs::write(raw.join("a.md"), pinned).unwrap();
    s.upsert_raw(&raw_row("a.md", &digest_of(d.path(), "raw/a.md")))
        .unwrap();

    // The pin is convert's concern: the scan accepts the recorded content.
    let rep = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);

    // A hand edit is a real change, pin or not.
    fs::write(raw.join("a.md"), pinned.replace("正文", "手改")).unwrap();
    let rep = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert_eq!(count(&rep, scan::ChangeKind::Changed), 1);
    assert_eq!(rep.changes[0].key, "a.md");
    assert_eq!(rep.changes[0].hash, Some(digest_of(d.path(), "raw/a.md")));
}

#[test]
fn converted_at_does_not_participate_in_the_comparison() {
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("raw/a.md"), "正文").unwrap();
    s.upsert_raw(&raw_row("a.md", &digest_of(d.path(), "raw/a.md")))
        .unwrap();
    let before = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert!(before.changes.is_empty(), "{:?}", before.changes);

    // Only the digest decides; a fresh conversion timestamp changes nothing.
    let mut row = s.get_raw("a.md").unwrap().unwrap();
    row.converted_at = String::from("2030-01-01T00:00:00Z");
    s.upsert_raw(&row).unwrap();
    let after = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert!(after.changes.is_empty(), "{:?}", after.changes);
}

#[test]
fn crlf_and_trailing_newline_differences_are_changed() {
    let (d, mut s, cfg) = setup();
    let raw = d.path().join("raw");

    fs::write(raw.join("a.md"), "body\n").unwrap();
    s.upsert_raw(&raw_row("a.md", &digest_of(d.path(), "raw/a.md")))
        .unwrap();
    assert!(scan::scan_raw(d.path(), &cfg, &s)
        .unwrap()
        .changes
        .is_empty());

    // No newline normalisation: CRLF is a hand edit like any other.
    fs::write(raw.join("a.md"), "body\r\n").unwrap();
    assert_eq!(
        count(
            &scan::scan_raw(d.path(), &cfg, &s).unwrap(),
            scan::ChangeKind::Changed
        ),
        1
    );

    fs::write(raw.join("a.md"), "body").unwrap();
    assert_eq!(
        count(
            &scan::scan_raw(d.path(), &cfg, &s).unwrap(),
            scan::ChangeKind::Changed
        ),
        1
    );
}

#[test]
fn scan_raw_is_read_only_and_stable() {
    let (d, s, cfg) = setup();
    fs::write(d.path().join("raw/a.md"), "alpha").unwrap();
    fs::write(d.path().join("raw/b.md"), "beta").unwrap();
    let ledger = s.all_raws().unwrap();

    let first = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    let second = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert_eq!(first, second);
    assert_eq!(s.all_raws().unwrap(), ledger);
}

#[test]
fn convert_records_the_digest_that_scan_recomputes() {
    let (d, mut s, cfg) = setup();
    fs::write(d.path().join("source/a.txt"), "正文\n").unwrap();

    let outcome = convert::convert_one(d.path(), &cfg, &mut s, "a.txt").unwrap();
    assert!(!outcome.skipped);

    let raw_key = "a.md";
    let recorded = s.get_raw(raw_key).unwrap().unwrap();
    assert_eq!(recorded.source_key, "a.txt");
    assert!(!recorded.converted_at.is_empty());
    assert_eq!(recorded.hash, digest_of(d.path(), "raw/a.md"));
    // The ledger key space is `raw/`-relative, not a repository path.
    assert!(s.get_raw("raw/a.md").unwrap().is_none());

    // The closed loop: a conversion must not look like pending work.
    let rep = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);

    // Re-converting a changed source refreshes the row and stays in the loop.
    fs::write(d.path().join("source/a.txt"), "正文第二版\n").unwrap();
    convert::convert_one(d.path(), &cfg, &mut s, "a.txt").unwrap();
    let refreshed = s.get_raw(raw_key).unwrap().unwrap();
    assert_ne!(refreshed.hash, recorded.hash);
    assert_eq!(refreshed.hash, digest_of(d.path(), "raw/a.md"));
    let rep = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert!(rep.changes.is_empty(), "{:?}", rep.changes);

    // A hand edit after the conversion is the only thing reported.
    let text = fs::read_to_string(d.path().join("raw/a.md")).unwrap();
    fs::write(d.path().join("raw/a.md"), format!("{text}补一段\n")).unwrap();
    let rep = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert_eq!(count(&rep, scan::ChangeKind::Changed), 1);
    assert_eq!(rep.changes[0].key, raw_key);
}

#[test]
fn a_tracked_raw_is_never_demoted_to_a_duplicate() {
    let (d, mut s, cfg) = setup();
    let raw = d.path().join("raw");
    fs::write(raw.join("a.md"), "same").unwrap();
    fs::write(raw.join("b.md"), "same").unwrap();
    // Only the second key is tracked. Demoting it because an untracked copy
    // now sorts first would ask `sync` to delete a file `convert` owns, and the
    // next conversion would put it back: deduplication stays among `New` files.
    s.upsert_raw(&raw_row("b.md", &digest_of(d.path(), "raw/b.md")))
        .unwrap();

    let rep = scan::scan_raw(d.path(), &cfg, &s).unwrap();
    assert_eq!(count(&rep, scan::ChangeKind::Duplicate), 0);
    assert_eq!(count(&rep, scan::ChangeKind::New), 1);
    assert!(rep.changes.iter().all(|c| c.key != "b.md"));
}

#[test]
fn raw_rows_roundtrip_and_replace() {
    let (_d, mut s, _) = setup();
    assert!(s.all_raws().unwrap().is_empty());
    assert!(s.get_raw("a.md").unwrap().is_none());

    let first = raw_row("b.md", "blake3:bb");
    s.upsert_raw(&first).unwrap();
    s.upsert_raw(&raw_row("a.md", "blake3:aa")).unwrap();
    // Upserting the same key replaces the row rather than adding one.
    let replaced = raw_row("b.md", "blake3:cc");
    s.upsert_raw(&replaced).unwrap();

    assert_eq!(s.get_raw("b.md").unwrap().unwrap(), replaced);
    let keys: Vec<String> = s
        .all_raws()
        .unwrap()
        .into_iter()
        .map(|row| row.key)
        .collect();
    assert_eq!(keys, ["a.md", "b.md"]);
}
