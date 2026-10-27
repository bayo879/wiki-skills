//! `ingest` drives one round: `begin` names the sources, the agent writes the
//! candidates, `commit` or `abort` closes the round.
//!
//! The suite pins the contract the CLI depends on: `begin` never writes into
//! `wiki/`, `commit` only ever writes staging (refreshing `updated` to the local
//! date and merging the round's `raw/<key>` references into `sources`), and
//! `abort` drops the round but keeps the candidates nobody has reviewed yet.
//!
//! Ledger rows are seeded explicitly: `ingest` resolves `--sources` through the
//! ledger, so a repository that has never been scanned has no ingestible source.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Local;
use llm_wiki::store::{RawRow, SourceRow, Store};
use llm_wiki::{config::Config, hash, ingest, init, page, staging};
use tempfile::{tempdir, TempDir};
use walkdir::WalkDir;

/// Opens (and migrates) the ledger under `root`.
fn open(root: &Path) -> Store {
    let mut store = Store::open(&root.join(".wiki/state.db")).unwrap();
    store.migrate().unwrap();
    store
}

/// A repository with `raw/a.md` converted and both ledger rows in place.
///
/// The raw row wins over the source row when `ingest` resolves `raw/a.md`, which
/// is the state after a real conversion: the text file is what a page cites.
fn setup() -> (TempDir, Config) {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    fs::write(d.path().join("raw/a.md"), "订单超时后重试三次。\n").unwrap();

    let mut store = open(d.path());
    store
        .upsert_source(&SourceRow {
            key: String::from("a.md"),
            hash: String::from("blake3:source"),
            size: 27,
            mtime: 1,
            raw_key: Some(String::from("raw/a.md")),
            converted_at: Some(String::from("2026-10-27T00:00:00Z")),
        })
        .unwrap();
    store
        .upsert_raw(&RawRow {
            key: String::from("a.md"),
            hash: String::from("blake3:raw"),
            source_key: String::from("a.md"),
            converted_at: String::from("2026-10-27T00:00:00Z"),
        })
        .unwrap();
    (d, cfg)
}

/// A page whose `sources` is an inline array.
fn page_inline(id: &str, title: &str, sources: &[&str]) -> String {
    format!(
        "---\nid: {id}\ntitle: {title}\ntype: concept\nstatus: stable\nupdated: 2026-01-01\nsources: [{}]\n---\n正文\n",
        sources.join(", ")
    )
}

/// A page whose `sources` is a block sequence (empty for `&[]`).
fn page_block(id: &str, title: &str, sources: &[&str]) -> String {
    let mut items = String::new();
    for source in sources {
        items.push_str("  - ");
        items.push_str(source);
        items.push('\n');
    }
    format!(
        "---\nid: {id}\ntitle: {title}\ntype: concept\nstatus: stable\nupdated: 2026-01-01\nsources:\n{items}---\n正文\n"
    )
}

/// A page with no `sources:` key at all.
fn page_bare(id: &str, title: &str) -> String {
    format!(
        "---\nid: {id}\ntitle: {title}\ntype: concept\nstatus: draft\nupdated: 2026-01-01\n---\n正文\n"
    )
}

/// Writes `body` at the repository-relative `rel`, creating parents.
fn write_file(root: &Path, rel: &str, body: &str) -> PathBuf {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, body).unwrap();
    path
}

/// `begin` with no explicit pages, as the CLI's common case drives it.
fn begin(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    sources: &[&str],
    pages: &[&str],
) -> ingest::BeginReport {
    ingest::begin(
        root,
        cfg,
        store,
        &ingest::BeginInput {
            sources: sources.iter().map(|s| String::from(*s)).collect(),
            pages: pages.iter().map(|s| String::from(*s)).collect(),
            dry_run: false,
        },
    )
    .unwrap()
}

/// `&["a", "b"]` as the `&[String]` the verbs take.
fn args(items: &[&str]) -> Vec<String> {
    items.iter().map(|item| String::from(*item)).collect()
}

/// Every file under `wiki/`, keyed by repository-relative path.
fn wiki_snapshot(root: &Path) -> BTreeMap<String, String> {
    let wiki = root.join("wiki");
    let mut out = BTreeMap::new();
    if !wiki.is_dir() {
        return out;
    }
    for entry in WalkDir::new(&wiki).follow_links(false) {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .into_owned();
        out.insert(rel, hash::hash_file(entry.path(), true).unwrap());
    }
    out
}

/// The sorted ids of the pages a begin report touches.
fn affected_ids(report: &ingest::BeginReport) -> Vec<String> {
    let mut ids: Vec<String> = report.affected.iter().map(|page| page.id.clone()).collect();
    ids.sort();
    ids
}

/// `.wiki/staging/pending.json`, the round carrier.
fn pending(root: &Path) -> PathBuf {
    root.join(".wiki/staging/pending.json")
}

/// The local date `commit` stamps into `updated`.
fn today() -> chrono::NaiveDate {
    Local::now().date_naive()
}

#[test]
fn commit_without_pages_is_a_usage_error() {
    let (d, cfg) = setup();
    let mut store = open(d.path());
    let err = ingest::commit(d.path(), &cfg, &mut store, &[]).unwrap_err();
    assert_eq!(err.code(), "E_NO_PAGES", "{err}");
}

#[test]
fn begin_reports_affected_pages_from_full_scan() {
    let (d, cfg) = setup();
    let root = d.path();
    write_file(
        root,
        "wiki/concept/retry.md",
        &page_block("retry", "重试", &["raw/a.md"]),
    );

    let mut store = open(root);
    let report = ingest::begin(
        root,
        &cfg,
        &mut store,
        &ingest::BeginInput {
            sources: vec![String::from("raw/a.md")],
            pages: Vec::new(),
            dry_run: false,
        },
    )
    .unwrap();

    assert!(
        report.affected.iter().any(|p| p.id == "retry"),
        "{report:?}"
    );
}

#[test]
fn abort_clears_pending_and_leaves_wiki_untouched() {
    let (d, cfg) = setup();
    let root = d.path();
    let mut store = open(root);
    begin(root, &cfg, &mut store, &["raw/a.md"], &[]);

    let report = ingest::abort(root, &cfg, &mut store).unwrap();
    assert_eq!(report.cleared, 1);

    let names: Vec<String> = fs::read_dir(root.join(&cfg.paths.wiki))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".md"))
        .collect();
    assert!(names.is_empty(), "wiki/ gained pages: {names:?}");
    assert!(!pending(root).exists());
}

#[test]
fn begin_leaves_wiki_untouched_and_writes_pending() {
    let (d, cfg) = setup();
    let root = d.path();
    write_file(
        root,
        "wiki/concept/retry.md",
        &page_block("retry", "重试", &["raw/a.md"]),
    );
    let before = wiki_snapshot(root);

    let mut store = open(root);
    let report = begin(root, &cfg, &mut store, &["raw/a.md"], &[]);

    assert_eq!(report.source_keys, args(&["a.md"]));
    assert_eq!(wiki_snapshot(root), before, "begin wrote into wiki/");
    // The round carries raw-relative keys, but the hash is the ledger hash of
    // the source row that produced the raw file: that is what the manifest
    // tracks for drift, and for a markdown source the two files are identical.
    assert_eq!(
        staging::pending_hashes(root).unwrap(),
        BTreeMap::from([(String::from("a.md"), String::from("blake3:source"))])
    );
    let body = fs::read(pending(root)).unwrap();
    assert_eq!(report.pending_hash, hash::hash_bytes(&body));
}

#[test]
fn begin_finds_every_page_that_cites_the_source() {
    let (d, cfg) = setup();
    let root = d.path();
    write_file(
        root,
        "wiki/concept/retry.md",
        &page_block("retry", "重试", &["raw/a.md"]),
    );
    // A fragment locates a passage; the page still cites the same raw file.
    write_file(
        root,
        "wiki/entity/order.md",
        &page_inline("order", "订单", &["raw/a.md#p=3"]),
    );

    let mut store = open(root);
    let report = begin(root, &cfg, &mut store, &["raw/a.md"], &[]);

    assert_eq!(affected_ids(&report), args(&["order", "retry"]));
    let order = report
        .affected
        .iter()
        .find(|page| page.id == "order")
        .unwrap();
    assert_eq!(order.path, "wiki/entity/order.md");
}

#[test]
fn begin_includes_explicit_pages() {
    let (d, cfg) = setup();
    let root = d.path();
    write_file(root, "wiki/concept/retry.md", &page_bare("retry", "重试"));

    let mut store = open(root);
    let report = begin(
        root,
        &cfg,
        &mut store,
        &["raw/a.md"],
        &["retry", "concept/cache"],
    );

    // The scan found nothing (no page cites the source); the explicit list is
    // authoritative, and the page without a file is counted as new.
    assert_eq!(affected_ids(&report), args(&["cache", "retry"]));
    assert_eq!(report.new_pages, 1);
    let retry = report
        .affected
        .iter()
        .find(|page| page.id == "retry")
        .unwrap();
    assert_eq!(retry.path, "wiki/concept/retry.md");
    let cache = report
        .affected
        .iter()
        .find(|page| page.id == "cache")
        .unwrap();
    assert_eq!(cache.path, "", "a page with no file has no path yet");
}

#[test]
fn begin_rejects_unknown_source() {
    let (d, cfg) = setup();
    let mut store = open(d.path());
    let err = ingest::begin(
        d.path(),
        &cfg,
        &mut store,
        &ingest::BeginInput {
            sources: vec![String::from("missing.md")],
            pages: Vec::new(),
            dry_run: false,
        },
    )
    .unwrap_err();
    assert_eq!(err.code(), "E_NO_SOURCE", "{err}");
}

#[test]
fn dry_run_writes_no_pending() {
    let (d, cfg) = setup();
    let root = d.path();
    let mut store = open(root);
    let report = ingest::begin(
        root,
        &cfg,
        &mut store,
        &ingest::BeginInput {
            sources: vec![String::from("raw/a.md")],
            pages: Vec::new(),
            dry_run: true,
        },
    )
    .unwrap();

    assert_eq!(report.source_keys, args(&["a.md"]));
    assert!(!report.pending_hash.is_empty());
    assert!(!pending(root).exists(), "dry_run wrote pending.json");
}

#[test]
fn commit_without_a_round_is_an_error() {
    let (d, cfg) = setup();
    let root = d.path();
    let mut store = open(root);
    staging::ensure(root, &cfg).unwrap();
    staging::write_page(
        root,
        &cfg,
        "concept/retry",
        &page_block("retry", "重试", &["raw/a.md"]),
    )
    .unwrap();

    let err = ingest::commit(root, &cfg, &mut store, &args(&["retry"])).unwrap_err();
    assert_eq!(err.code(), "E_NO_PENDING", "{err}");
}

#[test]
fn commit_rejects_a_page_that_is_not_staged() {
    let (d, cfg) = setup();
    let root = d.path();
    let mut store = open(root);
    begin(root, &cfg, &mut store, &["raw/a.md"], &[]);

    let err = ingest::commit(root, &cfg, &mut store, &args(&["ghost"])).unwrap_err();
    assert_eq!(err.code(), "E_NO_STAGED_PAGE", "{err}");
}

#[test]
fn commit_refreshes_the_staged_page_and_the_staging_index() {
    let (d, cfg) = setup();
    let root = d.path();
    let mut store = open(root);
    begin(root, &cfg, &mut store, &["raw/a.md"], &[]);

    staging::write_page(root, &cfg, "concept/retry", &page_bare("retry", "重试")).unwrap();
    staging::write_page(
        root,
        &cfg,
        "concept/cache",
        &page_inline("cache", "缓存", &["raw/other.md"]),
    )
    .unwrap();
    staging::write_page(
        root,
        &cfg,
        "concept/timeout",
        &page_block("timeout", "超时", &[]),
    )
    .unwrap();

    let report = ingest::commit(
        root,
        &cfg,
        &mut store,
        &args(&["retry", "concept/cache", "timeout"]),
    )
    .unwrap();

    assert_eq!(
        report.pages_written,
        args(&["concept/retry", "concept/cache", "concept/timeout"])
    );
    assert!(report.index_updated);

    let staging_dir = root.join(".wiki/staging");
    let retry = page::read(&staging_dir.join("concept/retry.md")).unwrap().0;
    assert_eq!(retry.updated, today());
    assert_eq!(
        retry.sources,
        args(&["raw/a.md"]),
        "absent field is created"
    );
    let cache = page::read(&staging_dir.join("concept/cache.md")).unwrap().0;
    assert_eq!(
        cache.sources,
        args(&["raw/other.md", "raw/a.md"]),
        "appended in order"
    );
    let timeout = page::read(&staging_dir.join("concept/timeout.md"))
        .unwrap()
        .0;
    assert_eq!(
        timeout.sources,
        args(&["raw/a.md"]),
        "empty block gets an item"
    );

    let index = fs::read_to_string(staging_dir.join("index.md")).unwrap();
    for title in ["重试", "缓存", "超时"] {
        assert!(
            index.contains(title),
            "staging index lost {title}:\n{index}"
        );
    }
    assert!(!pending(root).exists(), "commit left the round open");
}

#[test]
fn commit_leaves_wiki_untouched() {
    let (d, cfg) = setup();
    let root = d.path();
    write_file(
        root,
        "wiki/concept/retry.md",
        &page_block("retry", "重试", &["raw/a.md"]),
    );
    let before = wiki_snapshot(root);

    let mut store = open(root);
    begin(root, &cfg, &mut store, &["raw/a.md"], &[]);
    staging::write_page(
        root,
        &cfg,
        "concept/retry",
        &page_block("retry", "重试", &["raw/a.md"]),
    )
    .unwrap();
    ingest::commit(root, &cfg, &mut store, &args(&["retry"])).unwrap();

    assert_eq!(wiki_snapshot(root), before, "commit wrote into wiki/");
}

#[test]
fn abort_keeps_the_staged_candidates() {
    let (d, cfg) = setup();
    let root = d.path();
    let mut store = open(root);
    begin(root, &cfg, &mut store, &["raw/a.md"], &[]);
    staging::write_page(
        root,
        &cfg,
        "concept/retry",
        &page_block("retry", "重试", &[]),
    )
    .unwrap();

    let report = ingest::abort(root, &cfg, &mut store).unwrap();

    assert_eq!(report.cleared, 1);
    assert!(root.join(".wiki/staging/concept/retry.md").is_file());
    assert!(!pending(root).exists());
}

#[test]
fn commit_does_not_duplicate_an_existing_source() {
    let (d, cfg) = setup();
    let root = d.path();
    let mut store = open(root);
    begin(root, &cfg, &mut store, &["raw/a.md"], &[]);
    staging::write_page(
        root,
        &cfg,
        "concept/retry",
        &page_block("retry", "重试", &["raw/a.md"]),
    )
    .unwrap();

    ingest::commit(root, &cfg, &mut store, &args(&["retry"])).unwrap();

    let front = page::read(&root.join(".wiki/staging/concept/retry.md"))
        .unwrap()
        .0;
    assert_eq!(front.sources, args(&["raw/a.md"]));
}

#[test]
fn begin_reports_orphan_pages() {
    let (d, cfg) = setup();
    let root = d.path();
    write_file(
        root,
        "wiki/concept/stale.md",
        &page_block("stale", "过期", &["raw/gone.md"]),
    );

    let mut store = open(root);
    let report = begin(root, &cfg, &mut store, &["raw/a.md"], &[]);

    assert_eq!(report.orphan_pages, args(&["stale"]));
}
