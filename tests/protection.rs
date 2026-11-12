//! The protection checks around a conversion, and the compensation behind them.
//!
//! A conversion is two steps that cannot be atomic together: rename the raw file
//! into place, then record it. These tests cover what happens around that
//! window — a source that changed, a raw file a person edited, a ledger that
//! refuses the row — and assert the same thing every time: the previous text is
//! still on disk and the ledger still describes it.

use std::fs;

use llm_wiki::config::Config;
use llm_wiki::store::Store;
use llm_wiki::{convert, init, store::SourceRow};

/// A fresh wiki with an initialised ledger.
fn setup() -> (tempfile::TempDir, Store, Config) {
    let d = tempfile::tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    let mut store = Store::open(&d.path().join(".wiki/state.db")).unwrap();
    store.migrate().unwrap();
    (d, store, cfg)
}

/// Makes every ledger registration fail, the way a locked database would.
fn break_the_ledger(root: &std::path::Path) {
    let conn = rusqlite::Connection::open(root.join(".wiki/state.db")).unwrap();
    // `register_conversion` upserts this table, so both paths must fail: an
    // insert on the first conversion, an update on every later one.
    conn.execute_batch(
        "CREATE TRIGGER fail_insert BEFORE INSERT ON work_items \
         BEGIN SELECT RAISE(ABORT, 'injected ledger failure'); END;
         CREATE TRIGGER fail_update BEFORE UPDATE ON work_items \
         BEGIN SELECT RAISE(ABORT, 'injected ledger failure'); END;",
    )
    .unwrap();
}

#[test]
fn a_raw_file_edited_after_prepare_is_a_conflict() {
    let (d, mut store, cfg) = setup();
    fs::write(d.path().join("a.md"), "first body\n").unwrap();
    convert::convert_one(d.path(), &cfg, &mut store, "a.md").unwrap();

    // A person edits the body between preparing and committing; the edit is not
    // in the ledger either, so the commit must not treat the new text as ours.
    fs::write(d.path().join("a.md"), "second body\n").unwrap();
    let prepared = convert::prepare(d.path(), &cfg, "a.md").unwrap();
    let edited = fs::read_to_string(d.path().join(".wiki/raw/a.md"))
        .unwrap()
        .replace("first body", "hand-edited body");
    fs::write(d.path().join(".wiki/raw/a.md"), &edited).unwrap();

    let error = convert::commit(&mut store, prepared).unwrap_err();
    assert_eq!(error.code(), "E_RAW_COLLISION");
    assert_eq!(
        fs::read_to_string(d.path().join(".wiki/raw/a.md")).unwrap(),
        edited,
        "the hand edit must survive a refused commit"
    );
    assert!(
        !d.path().join(".wiki/raw/.a.md.tmp").exists(),
        "no temporary file may be left behind"
    );
}

#[test]
fn a_source_that_changed_after_prepare_is_a_conflict() {
    let (d, mut store, cfg) = setup();
    fs::write(d.path().join("a.md"), "as prepared\n").unwrap();
    let prepared = convert::prepare(d.path(), &cfg, "a.md").unwrap();

    fs::write(d.path().join("a.md"), "changed meanwhile\n").unwrap();
    let error = convert::commit(&mut store, prepared).unwrap_err();
    assert_eq!(error.code(), "E_RAW_COLLISION");
    assert!(
        !d.path().join(".wiki/raw/a.md").exists(),
        "nothing may be published for a source that moved"
    );
}

#[test]
fn a_raw_that_appears_after_prepare_is_a_conflict() {
    let (d, mut store, cfg) = setup();
    fs::write(d.path().join("a.md"), "body\n").unwrap();
    let prepared = convert::prepare(d.path(), &cfg, "a.md").unwrap();

    let foreign = "---\nsource: source/other.md\npin: false\n---\nother\n";
    fs::write(d.path().join(".wiki/raw/a.md"), foreign).unwrap();

    let error = convert::commit(&mut store, prepared).unwrap_err();
    assert_eq!(error.code(), "E_RAW_COLLISION");
    assert_eq!(
        fs::read_to_string(d.path().join(".wiki/raw/a.md")).unwrap(),
        foreign,
        "the file that appeared must not be overwritten"
    );
}

#[test]
fn a_ledger_failure_restores_the_previous_raw() {
    let (d, mut store, cfg) = setup();
    fs::write(d.path().join("a.md"), "old body\n").unwrap();
    convert::convert_one(d.path(), &cfg, &mut store, "a.md").unwrap();
    let before = fs::read_to_string(d.path().join(".wiki/raw/a.md")).unwrap();
    let old_hash = store.get_source("a.md").unwrap().unwrap().hash;

    break_the_ledger(d.path());
    fs::write(d.path().join("a.md"), "new body\n").unwrap();
    let error = convert::convert_one(d.path(), &cfg, &mut store, "a.md").unwrap_err();
    assert_eq!(error.code(), "E_DB");

    assert_eq!(
        fs::read_to_string(d.path().join(".wiki/raw/a.md")).unwrap(),
        before,
        "the previous raw must be back on disk"
    );
    assert_eq!(
        store.get_source("a.md").unwrap().unwrap().hash,
        old_hash,
        "the ledger must still describe the bytes on disk"
    );
}

#[test]
fn a_ledger_failure_on_a_first_conversion_removes_the_raw() {
    let (d, mut store, cfg) = setup();
    fs::write(d.path().join("b.md"), "body\n").unwrap();
    break_the_ledger(d.path());

    let error = convert::convert_one(d.path(), &cfg, &mut store, "b.md").unwrap_err();
    assert_eq!(error.code(), "E_DB");
    assert!(
        !d.path().join(".wiki/raw/b.md").exists(),
        "a raw whose row was refused must not stay on disk"
    );
    assert!(store.get_source("b.md").unwrap().is_none());
}

/// Both ledger rows are written in one transaction, so a raw row without its
/// source row means the pair is incomplete — the file cannot be proven ours.
#[test]
fn a_raw_whose_source_row_is_missing_is_a_conflict() {
    let (d, mut store, cfg) = setup();
    fs::write(d.path().join("a.md"), "first body\n").unwrap();
    convert::convert_one(d.path(), &cfg, &mut store, "a.md").unwrap();

    let conn = rusqlite::Connection::open(d.path().join(".wiki/state.db")).unwrap();
    conn.execute("DELETE FROM sources WHERE key = 'a.md'", [])
        .unwrap();

    let error = convert::convert_one(d.path(), &cfg, &mut store, "a.md").unwrap_err();
    assert_eq!(error.code(), "E_RAW_COLLISION", "{error}");
    assert!(
        error.to_string().contains("not the source a.md"),
        "the message must name the missing row: {error}"
    );
    assert!(
        d.path().join(".wiki/raw/a.md").exists(),
        "a conflicting raw is left in place"
    );
}

#[test]
fn preflight_checks_protection_without_parsing() {
    let (d, store, cfg) = setup();
    // `bad.pdf` cannot be parsed at all; preflight must still answer, because a
    // dry run reports the protection state, not the parse result.
    fs::write(d.path().join("bad.pdf"), b"not a pdf").unwrap();
    convert::preflight(d.path(), &cfg, &store, "bad.pdf").unwrap();

    let foreign = "---\nsource: source/other.md\n---\nother\n";
    fs::write(d.path().join(".wiki/raw/bad.md"), foreign).unwrap();
    let error = convert::preflight(d.path(), &cfg, &store, "bad.pdf").unwrap_err();
    assert_eq!(error.code(), "E_RAW_COLLISION");
}

#[test]
fn preflight_reports_a_ledger_that_disagrees_with_the_raw() {
    let (d, mut store, cfg) = setup();
    fs::write(d.path().join("a.md"), "body\n").unwrap();
    convert::convert_one(d.path(), &cfg, &mut store, "a.md").unwrap();
    let row = store.get_source("a.md").unwrap().unwrap();
    store
        .upsert_source(&SourceRow {
            hash: String::from("blake3:stale"),
            ..row
        })
        .unwrap();

    let error = convert::preflight(d.path(), &cfg, &store, "a.md").unwrap_err();
    assert_eq!(error.code(), "E_RAW_COLLISION");
}

#[test]
fn a_raw_without_a_ledger_row_is_refused_until_it_is_deleted() {
    let (d, mut store, cfg) = setup();
    fs::write(d.path().join("a.md"), "body\n").unwrap();
    // The file names us, but no ledger row proves it is still machine output;
    // it may have been edited after the row was lost, so it is kept as is.
    let old = "---\nsource: a.md\nsource_hash: blake3:stale\npin: false\n---\nold\n";
    fs::write(d.path().join(".wiki/raw/a.md"), old).unwrap();

    let error = convert::convert_one(d.path(), &cfg, &mut store, "a.md").unwrap_err();
    assert_eq!(error.code(), "E_RAW_COLLISION");
    assert_eq!(
        fs::read_to_string(d.path().join(".wiki/raw/a.md")).unwrap(),
        old,
        "a raw nobody can vouch for must be left alone"
    );

    // Deleting it is the documented way out: with no text there is nothing to
    // destroy, so the conversion rebuilds the file and the row.
    fs::remove_file(d.path().join(".wiki/raw/a.md")).unwrap();
    convert::convert_one(d.path(), &cfg, &mut store, "a.md").unwrap();
    let raw = fs::read_to_string(d.path().join(".wiki/raw/a.md")).unwrap();
    assert!(raw.contains("body"), "the raw must hold the new text");
    assert!(store.get_source("a.md").unwrap().unwrap().raw_key.is_some());
}

#[test]
fn a_raw_whose_body_was_edited_is_not_replaced() {
    let (d, mut store, cfg) = setup();
    fs::write(d.path().join("a.md"), "body\n").unwrap();
    convert::convert_one(d.path(), &cfg, &mut store, "a.md").unwrap();
    let path = d.path().join(".wiki/raw/a.md");
    let edited = format!(
        "{}a hand-written note\n",
        fs::read_to_string(&path).unwrap()
    );
    fs::write(&path, &edited).unwrap();

    let error = convert::convert_one(d.path(), &cfg, &mut store, "a.md").unwrap_err();
    assert_eq!(error.code(), "E_RAW_COLLISION");
    assert!(
        error.to_string().contains("may have been edited"),
        "the report must say why: {error}"
    );
    assert_eq!(fs::read_to_string(&path).unwrap(), edited);
}

#[test]
fn a_raw_deleted_after_a_conversion_is_rebuilt() {
    let (d, mut store, cfg) = setup();
    fs::write(d.path().join("a.md"), "body\n").unwrap();
    convert::convert_one(d.path(), &cfg, &mut store, "a.md").unwrap();
    fs::remove_file(d.path().join(".wiki/raw/a.md")).unwrap();

    convert::convert_one(d.path(), &cfg, &mut store, "a.md").unwrap();
    assert!(d.path().join(".wiki/raw/a.md").exists());
    assert_eq!(
        store.get_raw("a.md").unwrap().unwrap().source_key,
        "a.md",
        "the raws row must still point at its source"
    );
}
