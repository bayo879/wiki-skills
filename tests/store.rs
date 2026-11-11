//! Behaviour of the `state.db` ledger: persistence, migrations, and the source
//! page mapping. These tests treat the store as a black box and only use its
//! public API.

use llm_wiki::store::{SourceRow, Store};
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
        key: String::from("report.pdf"),
        hash: String::from("blake3:bb"),
        size: 2048,
        mtime: 1_700_000_000,
        raw_key: Some(String::from(".wiki/raw/report.md")),
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
    assert_eq!(keys, ["b.pdf", "report.pdf"]);

    s.delete_source(&full.key).unwrap();
    assert!(s.get_source(&full.key).unwrap().is_none());
    // Deleting a key that is already gone is a no-op, not an error.
    s.delete_source(&full.key).unwrap();
}

/// Two `llm-wiki` processes share one `state.db`. rusqlite already installs a
/// 5s busy handler on every connection, but that is the driver's default and the
/// driver documents it as subject to change: `Store::open` sets the value
/// itself, so a second writer waits for a short index write instead of failing
/// at once with `SQLITE_BUSY`. If the timeout drops to zero, the writer thread
/// panics here.
#[test]
fn a_second_writer_waits_for_the_lock_instead_of_failing() {
    let d = tempdir().unwrap();
    let p = d.path().join("state.db");
    {
        let mut s = Store::open(&p).unwrap();
        s.migrate().unwrap();
    }

    // Hold the write lock the way another process sitting in `BEGIN IMMEDIATE`
    // would, then let a `Store` writer try to get in.
    let holder = rusqlite::Connection::open(&p).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let writer = {
        let p = p.clone();
        std::thread::spawn(move || {
            let mut s = Store::open(&p).unwrap();
            s.upsert_source(&row("a.md", "blake3:aa")).unwrap();
        })
    };
    std::thread::sleep(std::time::Duration::from_millis(300));
    holder.execute_batch("COMMIT").unwrap();
    writer.join().expect("the writer must wait, not fail");

    let s = Store::open(&p).unwrap();
    assert!(s.get_source("a.md").unwrap().is_some());
}

// ---------------------------------------------------------------------------
// Relation index
// ---------------------------------------------------------------------------

use llm_wiki::store::{RelationLink, RelationNode, RelationSource};

fn node(key: &str, id: &str, page_type: &str) -> RelationNode {
    RelationNode {
        page_key: String::from(key),
        page_id: String::from(id),
        page_type: String::from(page_type),
        title: String::from(id),
        summary: String::new(),
        status: String::from("stable"),
        content_hash: String::from("blake3:00"),
    }
}

fn link(from: &str, literal: &str, to: Option<&str>) -> RelationLink {
    RelationLink {
        from_page: String::from(from),
        target_literal: String::from(literal),
        to_page: to.map(String::from),
        resolution: if to.is_some() {
            String::from("resolved")
        } else {
            String::from("missing")
        },
    }
}

fn source(page: &str, raw: &str) -> RelationSource {
    RelationSource {
        page_key: String::from(page),
        raw_ref: String::from(raw),
        raw_key: String::from(raw.split('#').next().unwrap()),
        fragment: raw.split_once('#').map(|(_, f)| String::from(f)),
    }
}

#[test]
fn replacing_a_page_rewires_its_edges_and_bumps_generation() {
    let d = tempdir().unwrap();
    let mut s = Store::open(&d.path().join("state.db")).unwrap();
    s.migrate().unwrap();
    let a = ".wiki/pages/concept/a.md";
    let b = ".wiki/pages/concept/b.md";

    let first = s.relation_generation().unwrap();
    s.replace_page_relations(&node(b, "b", "concept"), &[], &[]).unwrap();
    s.replace_page_relations(
        &node(a, "a", "concept"),
        &[link(a, "b", Some(b)), link(a, "gone", None)],
        &[source(a, ".wiki/raw/s.md#p=1")],
    )
    .unwrap();
    let after_write = s.relation_generation().unwrap();
    assert_eq!(after_write, first + 2);

    // An identical write changes nothing, so the generation stays put.
    s.replace_page_relations(
        &node(a, "a", "concept"),
        &[link(a, "b", Some(b)), link(a, "gone", None)],
        &[source(a, ".wiki/raw/s.md#p=1")],
    )
    .unwrap();
    assert_eq!(s.relation_generation().unwrap(), after_write);

    let from = s.relation_links_from(a).unwrap();
    assert_eq!(from.len(), 2);
    assert_eq!(from[0].target_literal, "b");
    assert_eq!(from[0].to_page.as_deref(), Some(b));
    assert_eq!(from[1].resolution, "missing");
    assert_eq!(s.relation_links_to(b).unwrap().len(), 1);
    assert_eq!(s.pages_citing(".wiki/raw/s.md").unwrap(), vec![String::from(a)]);

    // Deleting b keeps a's claim, but it can no longer resolve.
    s.delete_page_relations(b).unwrap();
    let from = s.relation_links_from(a).unwrap();
    assert_eq!(from[0].to_page, None);
    assert_eq!(from[0].resolution, "missing");
    assert!(s.relation_links_to(b).unwrap().is_empty());
    assert!(s.all_relation_nodes().unwrap().iter().all(|n| n.page_key != b));
}
