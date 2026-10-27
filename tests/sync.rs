//! `sync` is one unattended round: nobody watches it, so the guard rails have to
//! hold by themselves.
//!
//! The suite pins the contract a scheduler depends on. A lock makes a second
//! round exit 4 instead of racing the first (and a crashed lock is reclaimed
//! after its TTL, not left to block forever). The settle window keeps a file
//! somebody is still writing from being converted half-done. The stable-streak
//! gate makes `auto_ingest` wait for a source to stop moving. `--dry-run` writes
//! nothing at all (the whole tree is hashed before and after), and a failure is
//! a per-source finding — it neither aborts the round nor hides the sources that
//! did convert. Repeated failures close `auto_ingest` through `sync_state` only:
//! the CLI never rewrites `wiki.toml`.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime};

use chrono::Local;
use llm_wiki::config::Config;
use llm_wiki::store::Store;
use llm_wiki::{convert, hash, init, lock, page, sync};
use tempfile::{tempdir, TempDir};

/// Opens (and migrates) the ledger under `root`.
fn open(root: &Path) -> Store {
    let mut store = Store::open(&root.join(".wiki/state.db")).unwrap();
    store.migrate().unwrap();
    store
}

/// An initialised repository with the settle window set.
fn repo(settle_seconds: u64) -> (TempDir, Config) {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let mut cfg = Config::load(d.path()).unwrap();
    cfg.sync.settle_seconds = settle_seconds;
    (d, cfg)
}

fn args(limit: usize) -> sync::SyncArgs {
    sync::SyncArgs {
        limit,
        dry_run: false,
        force: false,
    }
}

/// Every regular file under `root`, keyed by `/`-separated relative path and
/// valued by its content hash.
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(root)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        map.insert(rel, hash::hash_bytes(&fs::read(entry.path()).unwrap()));
    }
    map
}

fn write(root: &Path, key: &str, body: &str) {
    let path = root.join("source").join(key);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// A lock file nobody holds, aged past `ttl` seconds.
fn plant_stale_lock(root: &Path, ttl: u64) {
    let path = root.join(".wiki/lock");
    fs::write(
        &path,
        "{\"pid\":1,\"host\":\"old\",\"started_at\":\"2020-01-01T00:00:00Z\",\"run_id\":\"old\"}",
    )
    .unwrap();
    let file = fs::File::options().write(true).open(&path).unwrap();
    file.set_modified(SystemTime::now() - Duration::from_secs(ttl + 60))
        .unwrap();
}

#[test]
fn second_lock_is_exit_4() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let _h = lock::acquire(d.path()).unwrap();

    let err = lock::acquire(d.path()).unwrap_err();
    let text = err.to_string();
    assert!(text.contains("E_LOCKED"), "the code names the exit: {text}");
    assert!(
        text.contains(".wiki/lock"),
        "the message points at the file: {text}"
    );

    // The holder records who it is, so a human can tell a live run from a crash.
    // It is read through the handle: on Windows a second handle cannot read a
    // locked file at all, and this test should not be the exception.
    let body: serde_json::Value = serde_json::from_str(_h.body()).unwrap();
    for key in ["pid", "host", "started_at", "run_id"] {
        assert!(body.get(key).is_some(), "lock file must record {key}");
    }
    assert!(body["run_id"].as_str().unwrap().len() >= 8);
}

#[test]
fn dropping_the_lock_releases_the_file() {
    let d = tempdir().unwrap();
    init::run(d.path(), false).unwrap();
    let path = d.path().join(".wiki/lock");
    {
        let _h = lock::acquire(d.path()).unwrap();
        assert!(path.exists(), "the lock file exists while it is held");
    }
    assert!(!path.exists(), "the lock file is removed on drop");
}

#[test]
fn fresh_files_are_skipped_by_settle_window() {
    let (d, cfg) = repo(3600);
    write(d.path(), "a.md", "# hi");
    let mut store = open(d.path());

    let report = sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
    assert_eq!(report.converted, 0, "a just-written file must wait");
    assert!(
        report.warnings.iter().any(|w| w.contains("settle")),
        "the skipped file is explained: {:?}",
        report.warnings
    );
    assert!(store.get_source("a.md").unwrap().is_none());
}

#[test]
fn stable_streak_gates_auto_ingest() {
    let (d, mut cfg) = repo(0);
    cfg.sync.auto_ingest = true;
    cfg.sync.stable_streak = 3;
    write(d.path(), "a.md", "# hi");
    let mut store = open(d.path());

    let mut ingested_at = None;
    for round in 0..3 {
        let report = sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
        if !report.ingested.is_empty() {
            ingested_at = Some(round);
            break;
        }
    }
    assert_eq!(
        ingested_at,
        Some(2),
        "only the third unchanged round may ingest"
    );
}

#[test]
fn stale_lock_is_reclaimed_and_reported() {
    let (d, cfg) = repo(0);
    plant_stale_lock(d.path(), 1);
    assert!(lock::is_stale(d.path(), 1), "an hour-old lock is stale");

    let held = lock::acquire_with_ttl(d.path(), 1).unwrap();
    assert!(held.reclaimed(), "the leftover is reported as reclaimed");
    drop(held);
    assert!(!d.path().join(".wiki/lock").exists());

    // Through a full round the reclamation is a warning, not a silent event.
    plant_stale_lock(d.path(), lock::LOCK_TTL_SECS);
    let mut store = open(d.path());
    let report = sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("stale lock") && w.contains("reclaimed")),
        "warnings: {:?}",
        report.warnings
    );
}

#[test]
fn dry_run_writes_nothing() {
    let (d, cfg) = repo(0);
    write(d.path(), "a.md", "# hi");
    // Opening the ledger creates it; the dry run must not change it after that.
    drop(open(d.path()));
    let before = snapshot(d.path());

    let mut store = open(d.path());
    let report = sync::run(
        d.path(),
        &cfg,
        &mut store,
        &sync::SyncArgs {
            limit: 10,
            dry_run: true,
            force: false,
        },
    )
    .unwrap();

    assert_eq!(report.converted, 0, "a dry run converts nothing");
    assert!(
        report
            .warnings
            .iter()
            .any(|w| w.contains("would be converted")),
        "the would-be work is reported: {:?}",
        report.warnings
    );
    assert!(
        store.sync_state_entries().unwrap().is_empty(),
        "no streak or failure state is recorded"
    );
    drop(store);

    let after = snapshot(d.path());
    assert_eq!(before, after, "a dry run leaves the tree untouched");
}

#[test]
fn limit_caps_conversions_and_reports_the_rest_as_queued() {
    let (d, cfg) = repo(0);
    // Distinct bodies: identical content is a `Duplicate`, not a pending
    // source, and the limit would never get a chance to bite.
    for key in ["a.md", "b.md", "c.md"] {
        write(d.path(), key, &format!("# {key}"));
    }
    let mut store = open(d.path());

    let report = sync::run(d.path(), &cfg, &mut store, &args(2)).unwrap();
    assert_eq!(report.converted, 2);
    assert_eq!(
        report.queued, 1,
        "the third source waits for the next round"
    );
    assert_eq!(report.failures, 0);
}

#[test]
fn one_failed_conversion_does_not_stop_the_round() {
    let (d, cfg) = repo(0);
    write(d.path(), "bad.pdf", "this is not a pdf");
    write(d.path(), "good.md", "# good");
    let mut store = open(d.path());

    let report = sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
    assert_eq!(report.failures, 1, "the bad source is a finding");
    assert_eq!(report.converted, 1, "the good source still converts");
    assert!(d.path().join("raw/good.md").is_file());
    assert_eq!(
        store.sync_state_get("fail:bad.pdf").unwrap().as_deref(),
        Some("1"),
        "the streak of failures starts at one"
    );
}

#[test]
fn repeated_failures_disable_auto_ingest_without_touching_the_config() {
    let (d, mut cfg) = repo(0);
    cfg.sync.auto_ingest = true;
    cfg.sync.stable_streak = 1;
    write(d.path(), "bad.pdf", "this is not a pdf");
    write(d.path(), "good.md", "# good");
    let toml_before = fs::read(d.path().join("wiki.toml")).unwrap();
    let mut store = open(d.path());

    for _ in 0..3 {
        let report = sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
        assert_eq!(report.failures, 1);
    }
    assert_eq!(
        store
            .sync_state_get(sync::AUTO_INGEST_DISABLED)
            .unwrap()
            .as_deref(),
        Some("1"),
        "three failed rounds close the gate"
    );

    // `good.md` has a long enough streak by now; the disabled flag wins.
    let report = sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
    assert!(report.ingested.is_empty(), "the gate stays closed");
    assert!(
        report.warnings.iter().any(|w| w.contains("auto_ingest")),
        "the closed gate is explained: {:?}",
        report.warnings
    );
    assert_eq!(
        toml_before,
        fs::read(d.path().join("wiki.toml")).unwrap(),
        "sync never rewrites wiki.toml"
    );
}

#[test]
fn changing_a_source_resets_its_streak() {
    let (d, cfg) = repo(0);
    write(d.path(), "a.md", "version one");
    let mut store = open(d.path());

    sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
    assert_eq!(
        store.sync_state_get("streak:a.md").unwrap().as_deref(),
        Some("1")
    );
    sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
    assert_eq!(
        store.sync_state_get("streak:a.md").unwrap().as_deref(),
        Some("2"),
        "an unchanged source keeps counting"
    );

    write(d.path(), "a.md", "version two");
    let report = sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
    assert_eq!(report.converted, 1);
    assert_eq!(
        store.sync_state_get("streak:a.md").unwrap().as_deref(),
        Some("1"),
        "a changed raw resets the streak"
    );
}

#[test]
fn every_round_writes_an_audit_entry() {
    let (d, cfg) = repo(0);
    write(d.path(), "a.md", "# hi");
    let mut store = open(d.path());

    let report = sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
    let today = Local::now().date_naive().format("%Y-%m-%d");
    let path = d.path().join("wiki/audit").join(format!("{today}-sync.md"));
    let text = fs::read_to_string(&path).unwrap_or_else(|e| panic!("no audit at {path:?}: {e}"));
    assert!(text.contains(&report.run_id), "audit: {text}");
    assert!(text.contains("converted"), "audit: {text}");
    assert!(text.contains("a.md"), "audit: {text}");
}

#[test]
fn a_second_round_over_unchanged_sources_converts_nothing() {
    let (d, cfg) = repo(0);
    write(d.path(), "a.md", "# hi");
    let mut store = open(d.path());

    let first = sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
    assert_eq!(first.converted, 1);

    let second = sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
    assert_eq!(second.converted, 0);
    assert!(
        second.changes.is_empty(),
        "an unchanged tree has no changes: {:?}",
        second.changes
    );
}

/// Writes one page under `wiki/` with the frontmatter the tests care about.
fn write_page(root: &Path, key: &str, status: &str, sources: &[&str]) {
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

/// The audit file a round ran today appends to.
fn audit_text(root: &Path) -> String {
    let path = root
        .join("wiki/audit")
        .join(format!("{}-sync.md", Local::now().date_naive()));
    fs::read_to_string(&path).unwrap_or_default()
}

#[test]
fn a_missing_source_turns_its_stable_pages_stale_and_names_them_in_the_audit() {
    let (d, cfg) = repo(0);
    write(d.path(), "a.md", "# A\n\nalpha body\n");
    let mut store = open(d.path());
    convert::convert_one(d.path(), &cfg, &mut store, "a.md").unwrap();
    write_page(d.path(), "wiki/concept/alpha.md", "stable", &["raw/a.md"]);
    write_page(d.path(), "wiki/concept/beta.md", "stable", &["raw/a.md"]);
    write_page(d.path(), "wiki/concept/gamma.md", "draft", &["raw/a.md"]);
    fs::remove_file(d.path().join("source/a.md")).unwrap();

    let report = sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
    assert_eq!(
        report.stale_pages,
        vec!["wiki/concept/alpha.md", "wiki/concept/beta.md"],
        "only published pages are demoted; the draft stays a draft"
    );
    for key in ["wiki/concept/alpha.md", "wiki/concept/beta.md"] {
        let (front, _) = page::read(&d.path().join(key)).unwrap();
        assert_eq!(front.status, page::Status::Stale, "{key}");
    }
    let (draft, _) = page::read(&d.path().join("wiki/concept/gamma.md")).unwrap();
    assert_eq!(draft.status, page::Status::Draft);

    let meta = store
        .get_page_meta("wiki/concept/alpha.md")
        .unwrap()
        .expect("a page_meta record for the demoted page");
    assert_eq!(
        meta.hash,
        hash::hash_file(&d.path().join("wiki/concept/alpha.md"), true).unwrap(),
        "the status write does not leave lint L9 drift behind"
    );

    let audit = audit_text(d.path());
    assert!(
        audit.contains("## 源消失导致转 stale 的页面"),
        "the audit names the section: {audit}"
    );
    assert!(audit.contains("wiki/concept/alpha.md"), "{audit}");
    assert!(audit.contains("wiki/concept/beta.md"), "{audit}");
    assert!(!audit.contains("wiki/concept/gamma.md"), "{audit}");

    // A second round finds the pages already stale and rewrites nothing.
    let report = sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
    assert!(report.stale_pages.is_empty(), "{:?}", report.stale_pages);
}

#[test]
fn on_missing_ignore_leaves_the_pages_alone() {
    let (d, mut cfg) = repo(0);
    cfg.sync.on_missing = String::from("ignore");
    write(d.path(), "a.md", "# A\n\nalpha body\n");
    let mut store = open(d.path());
    convert::convert_one(d.path(), &cfg, &mut store, "a.md").unwrap();
    write_page(d.path(), "wiki/concept/alpha.md", "stable", &["raw/a.md"]);
    fs::remove_file(d.path().join("source/a.md")).unwrap();

    let report = sync::run(d.path(), &cfg, &mut store, &args(10)).unwrap();
    assert!(report.stale_pages.is_empty());
    let (front, _) = page::read(&d.path().join("wiki/concept/alpha.md")).unwrap();
    assert_eq!(front.status, page::Status::Stable);
}

#[test]
fn an_unknown_on_missing_value_fails_config_validation() {
    let (d, _cfg) = repo(0);
    let path = d.path().join("wiki.toml");
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text.replace("\"mark_stale\"", "\"explode\"")).unwrap();
    let err = Config::load(d.path()).expect_err("an unknown on_missing is a config error");
    assert_eq!(err.code(), "E_BAD_CONFIG");
}
