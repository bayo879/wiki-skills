//! One question, one answer: does the `raws` ledger know this file?
//!
//! Three consumers ask it. `scan --layer raw` turns an unknown key into `New`,
//! `lint` `L7` reports the file, and `ingest begin` reports the pages that cite
//! it. The join used to be written out at each call site, which is how `L7` grew
//! its own copy of "known" and drifted from `scan`. These tests pin the
//! agreement, not the wording of any one message.

use llm_wiki::{
    config::Config,
    convert, hash, ingest, init, lint, scan,
    store::{RawRow, Store},
};
use std::fs;
use std::path::Path;
use tempfile::tempdir;

/// A temporary repository with one converted source.
struct Fixture {
    dir: tempfile::TempDir,
    cfg: Config,
    store: Store,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempdir().unwrap();
        init::run(dir.path(), false).unwrap();
        let cfg = Config::load(dir.path()).unwrap();
        let mut store = Store::open(&dir.path().join(".wiki/state.db")).unwrap();
        store.migrate().unwrap();
        let mut fixture = Fixture { dir, cfg, store };
        fixture.convert_source();
        fixture
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Converts `source/a.md`, so `ingest begin` has a key it accepts.
    fn convert_source(&mut self) {
        fs::write(self.root().join("source/a.md"), "重试上限是 3 次。\n").unwrap();
        let root = self.root().to_path_buf();
        let cfg = self.cfg.clone();
        convert::convert_one(&root, &cfg, &mut self.store, "a.md").unwrap();
    }

    /// Writes `raw/<key>` straight to disk, the way a hand edit would.
    fn put_raw(&self, key: &str) {
        let path = self.root().join(self.cfg.paths.raw.as_str()).join(key);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "手写的正文\n").unwrap();
    }

    /// Registers `raw/<key>` the way `convert` does: digest of the bytes on disk.
    fn ledger(&mut self, key: &str) {
        let digest = hash::hash_file(&self.root().join("raw").join(key), true).unwrap();
        self.store
            .upsert_raw(&RawRow {
                key: String::from(key),
                hash: digest,
                source_key: String::from("a.md"),
                converted_at: String::from("2026-10-27T10:00:00Z"),
            })
            .unwrap();
    }

    /// Writes a page citing `raw/<key>`.
    fn put_page(&self, id: &str, key: &str) {
        let path = self.root().join("wiki/concept").join(format!("{id}.md"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let text = format!(
            "---\nid: {id}\ntitle: {id}\ntype: concept\nsummary: 摘要\nsources:\n  - raw/{key}\n\
             status: stable\nupdated: 2026-10-20\n---\n\n\
             这一页的正文足够长，写清楚前因后果，测试才会认为它达标。\n"
        );
        fs::write(path, text).unwrap();
    }

    /// Raw-layer keys `scan` calls new.
    fn scanned_new(&self) -> Vec<String> {
        let report = scan::scan_raw(self.root(), &self.cfg, &self.store).unwrap();
        let mut keys: Vec<String> = report
            .changes
            .iter()
            .filter(|change| change.kind == scan::ChangeKind::New)
            .map(|change| change.key.clone())
            .collect();
        keys.sort();
        keys
    }

    /// Keys `lint` `L7` names as unledgered.
    fn linted(&self) -> Vec<String> {
        let findings = lint::lint(self.root(), &self.cfg, &self.store).unwrap();
        let mut keys: Vec<String> = findings
            .iter()
            .filter(|finding| finding.rule == "L7")
            .filter_map(|finding| key_in_backticks(&finding.msg))
            .collect();
        keys.sort();
        keys
    }

    /// Pages `ingest begin` reports as citing an unledgered raw.
    fn orphaned(&mut self) -> Vec<String> {
        let root = self.root().to_path_buf();
        let cfg = self.cfg.clone();
        let report = ingest::begin(
            &root,
            &cfg,
            &mut self.store,
            &ingest::BeginInput {
                sources: vec![String::from("a.md")],
                pages: Vec::new(),
                dry_run: true,
            },
        )
        .unwrap();
        report.orphan_pages
    }
}

/// The first `` `…` `` span in `msg`, without the backticks.
fn key_in_backticks(msg: &str) -> Option<String> {
    let start = msg.find('`')? + 1;
    let rest = &msg[start..];
    let end = rest.find('`')?;
    Some(String::from(&rest[..end]))
}

#[test]
fn an_unledgered_raw_is_reported_the_same_way_by_all_three() {
    let mut fixture = Fixture::new();
    fixture.put_raw("stray.md");
    fixture.put_page("retry", "stray.md");

    assert_eq!(fixture.scanned_new(), vec![String::from("stray.md")]);
    assert_eq!(fixture.linted(), vec![String::from("stray.md")]);
    assert_eq!(fixture.orphaned(), vec![String::from("retry")]);
}

#[test]
fn registering_the_raw_silences_all_three() {
    let mut fixture = Fixture::new();
    fixture.put_raw("stray.md");
    fixture.put_page("retry", "stray.md");
    fixture.ledger("stray.md");

    assert!(fixture.scanned_new().is_empty());
    assert!(fixture.linted().is_empty());
    assert!(fixture.orphaned().is_empty());
}

#[test]
fn a_converted_raw_is_in_the_ledger_for_every_consumer() {
    let mut fixture = Fixture::new();
    // `convert_one` above already registered `raw/a.md`; nothing should dispute it.
    assert!(!fixture.scanned_new().contains(&String::from("a.md")));
    assert!(!fixture.linted().contains(&String::from("a.md")));

    fixture.put_page("retry", "a.md");
    assert!(fixture.orphaned().is_empty());
}
