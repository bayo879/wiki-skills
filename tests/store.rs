//! Behaviour of the `state.db` ledger: persistence, migrations, and the source
//! page mapping. These tests treat the store as a black box and only use its
//! public API.

use llm_wiki::store::{ArchiveRow, SourceRow, Store};
use tempfile::tempdir;

/// A row with every optional column unset.
fn row(key: &str, hash: &str) -> SourceRow {
    SourceRow {
        key: String::from(key),
        hash: String::from(hash),
        size: 10,
        mtime: 1,
        raw_key: None,
        converted_at: None,
    }
}

#[test]
fn rows_survive_reopen() {
    let d = tempdir().unwrap();
    let p = d.path().join("state.db");
    {
        let mut s = Store::open(&p).unwrap();
        s.migrate().unwrap();
        s.upsert_source(&row("a.pdf", "blake3:aa")).unwrap();
    }
    let s = Store::open(&p).unwrap();
    let got = s.get_source("a.pdf").unwrap().unwrap();
    assert_eq!(got.hash, "blake3:aa");
    assert_eq!(got.size, 10);
    assert_eq!(got.mtime, 1);
    assert_eq!(got.raw_key, None);
    assert_eq!(got.converted_at, None);
}

#[test]
fn bundled_sqlite_has_fts5() {
    let d = tempdir().unwrap();
    let mut s = Store::open(&d.path().join("state.db")).unwrap();
    s.migrate().unwrap();
    assert!(s.fts5_available().unwrap(), "bundled sqlite must ship FTS5");
}

#[test]
fn migrate_is_idempotent() {
    let d = tempdir().unwrap();
    let p = d.path().join("state.db");
    let mut s = Store::open(&p).unwrap();
    s.migrate().unwrap();
    s.upsert_source(&row("a.pdf", "blake3:aa")).unwrap();

    // A second migrate on the same connection keeps the data and does not fail.
    s.migrate().unwrap();
    assert!(s.get_source("a.pdf").unwrap().is_some());

    // Reopening an already-migrated database and migrating again is a no-op too.
    drop(s);
    let mut s = Store::open(&p).unwrap();
    s.migrate().unwrap();
    assert_eq!(s.get_source("a.pdf").unwrap().unwrap().hash, "blake3:aa");
    assert_eq!(s.all_sources().unwrap().len(), 1);
}

#[test]
fn open_creates_missing_parent_directory() {
    let d = tempdir().unwrap();
    // `init` creates `.wiki/`, but the store must not depend on that.
    let p = d.path().join(".wiki").join("state.db");
    let mut s = Store::open(&p).unwrap();
    s.migrate().unwrap();
    assert!(p.is_file());
}

#[test]
fn full_row_roundtrips_and_updates() {
    let d = tempdir().unwrap();
    let mut s = Store::open(&d.path().join("state.db")).unwrap();
    s.migrate().unwrap();

    let full = SourceRow {
        key: String::from("source/report.pdf"),
        hash: String::from("blake3:bb"),
        size: 2048,
        mtime: 1_700_000_000,
        raw_key: Some(String::from("raw/report.md")),
        converted_at: Some(String::from("2026-10-27T11:30:00+08:00")),
    };
    s.upsert_source(&full).unwrap();
    assert_eq!(s.get_source(&full.key).unwrap().unwrap(), full);

    // Upserting the same key replaces the whole row rather than adding one.
    let changed = SourceRow {
        hash: String::from("blake3:cc"),
        ..full.clone()
    };
    s.upsert_source(&changed).unwrap();
    assert_eq!(s.get_source(&full.key).unwrap().unwrap().hash, "blake3:cc");
    assert_eq!(s.all_sources().unwrap(), vec![changed]);

    s.upsert_source(&row("b.pdf", "blake3:dd")).unwrap();
    let keys: Vec<String> = s
        .all_sources()
        .unwrap()
        .into_iter()
        .map(|entry| entry.key)
        .collect();
    assert_eq!(keys, ["b.pdf", "source/report.pdf"]);

    s.delete_source(&full.key).unwrap();
    assert!(s.get_source(&full.key).unwrap().is_none());
    // Deleting a key that is already gone is a no-op, not an error.
    s.delete_source(&full.key).unwrap();
}

#[test]
fn source_pages_roundtrip_and_replace() {
    let d = tempdir().unwrap();
    let mut s = Store::open(&d.path().join("state.db")).unwrap();
    s.migrate().unwrap();

    assert!(s.pages_for_source("raw/a.md").unwrap().is_empty());

    s.set_source_pages(
        "raw/a.md",
        &[String::from("page/b"), String::from("page/a")],
    )
    .unwrap();
    let mut pages = s.pages_for_source("raw/a.md").unwrap();
    pages.sort();
    assert_eq!(pages, ["page/a", "page/b"]);

    // Setting again replaces the set instead of appending to it.
    s.set_source_pages("raw/a.md", &[String::from("page/c")])
        .unwrap();
    assert_eq!(s.pages_for_source("raw/a.md").unwrap(), ["page/c"]);

    s.set_source_pages("raw/a.md", &[]).unwrap();
    assert!(s.pages_for_source("raw/a.md").unwrap().is_empty());
}

/// A sealed-session record with every column filled.
fn sealed(key: &str) -> ArchiveRow {
    ArchiveRow {
        key: String::from(key),
        period_start: String::from("2026-09-01"),
        period_end: String::from("2026-09-30"),
        sources: vec![String::from("queries/a.md"), String::from("queries/b.md")],
        sealed_at: String::from("2026-10-27T11:30:00+08:00"),
        sealed_by: String::from("human"),
        last_recalled_at: None,
        recall_count: 0,
    }
}

#[test]
fn archive_rows_roundtrip() {
    let d = tempdir().unwrap();
    let mut s = Store::open(&d.path().join("state.db")).unwrap();
    s.migrate().unwrap();

    assert!(s.all_archives().unwrap().is_empty());
    assert!(s.get_archive("wiki/archive/a.md").unwrap().is_none());

    let first = sealed("wiki/archive/a.md");
    s.upsert_archive(&first).unwrap();
    assert_eq!(s.get_archive(&first.key).unwrap().unwrap(), first);

    // Upserting the same key replaces the whole row rather than adding one.
    let changed = ArchiveRow {
        period_end: String::from("2026-10-02"),
        ..first.clone()
    };
    s.upsert_archive(&changed).unwrap();
    assert_eq!(s.all_archives().unwrap(), vec![changed]);
}

#[test]
fn archive_rows_are_ordered_by_key() {
    let d = tempdir().unwrap();
    let mut s = Store::open(&d.path().join("state.db")).unwrap();
    s.migrate().unwrap();

    s.upsert_archive(&sealed("wiki/archive/c.md")).unwrap();
    s.upsert_archive(&sealed("wiki/archive/a.md")).unwrap();
    let keys: Vec<String> = s
        .all_archives()
        .unwrap()
        .into_iter()
        .map(|entry| entry.key)
        .collect();
    assert_eq!(keys, ["wiki/archive/a.md", "wiki/archive/c.md"]);
}

#[test]
fn touch_recall_records_the_hit() {
    let d = tempdir().unwrap();
    let mut s = Store::open(&d.path().join("state.db")).unwrap();
    s.migrate().unwrap();
    s.upsert_archive(&sealed("wiki/archive/a.md")).unwrap();

    // The first hit stamps the time and starts the count at one.
    s.touch_archive_recall("wiki/archive/a.md", "2026-10-27")
        .unwrap();
    let touched = s.get_archive("wiki/archive/a.md").unwrap().unwrap();
    assert_eq!(touched.last_recalled_at.as_deref(), Some("2026-10-27"));
    assert_eq!(touched.recall_count, 1);

    // A later hit moves the time forward and accumulates the count.
    s.touch_archive_recall("wiki/archive/a.md", "2026-11-02")
        .unwrap();
    let touched = s.get_archive("wiki/archive/a.md").unwrap().unwrap();
    assert_eq!(touched.last_recalled_at.as_deref(), Some("2026-11-02"));
    assert_eq!(touched.recall_count, 2);

    // Touching an unknown archive is a no-op, not an error.
    s.touch_archive_recall("wiki/archive/nope.md", "2026-11-02")
        .unwrap();
    assert!(s.get_archive("wiki/archive/nope.md").unwrap().is_none());
}

#[test]
fn delete_archive_removes_only_that_row() {
    let d = tempdir().unwrap();
    let mut s = Store::open(&d.path().join("state.db")).unwrap();
    s.migrate().unwrap();
    s.upsert_archive(&sealed("wiki/archive/a.md")).unwrap();
    s.upsert_archive(&sealed("wiki/archive/b.md")).unwrap();

    s.delete_archive("wiki/archive/a.md").unwrap();
    assert!(s.get_archive("wiki/archive/a.md").unwrap().is_none());
    assert!(s.get_archive("wiki/archive/b.md").unwrap().is_some());
    // Deleting a key that is already gone is a no-op, not an error.
    s.delete_archive("wiki/archive/a.md").unwrap();
}
