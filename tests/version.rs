//! Task 18: version snapshots and the four-way decision gate.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::TempDir;
use walkdir::WalkDir;

use llm_wiki::config::Config;
use llm_wiki::hash;
use llm_wiki::init;
use llm_wiki::render;
use llm_wiki::staging::{self, Manifest, ManifestEntry};
use llm_wiki::store::{PageMetaRow, Store};
use llm_wiki::version::{self, ApproveOutcome};

/// A repository with the contract, a migrated ledger and an empty staging tree.
fn boot() -> (TempDir, Config, Store) {
    let dir = tempfile::tempdir().unwrap();
    init::run(dir.path(), false).unwrap();
    let cfg = Config::load(dir.path()).unwrap();
    let mut store = open(dir.path(), &cfg);
    store.migrate().unwrap();
    staging::ensure(dir.path(), &cfg).unwrap();
    (dir, cfg, store)
}

fn open(root: &Path, cfg: &Config) -> Store {
    Store::open(&root.join(&cfg.paths.engine).join("state.db")).unwrap()
}

/// Writes `body` at the repository-relative `rel`, creating parents.
fn write_file(root: &Path, rel: &str, body: &str) -> PathBuf {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, body).unwrap();
    path
}

fn page_text(id: &str, title: &str) -> String {
    format!(
        "---\nid: {id}\ntitle: {title}\ntype: concept\nstatus: draft\nupdated: 2026-01-01\n---\n正文\n"
    )
}

/// Stages one page and records the ledger row `ingest commit` would leave.
///
/// `rel` is relative to the wiki root, e.g. `concept/alpha.md`.
fn stage(root: &Path, cfg: &Config, store: &mut Store, rel: &str, title: &str) -> String {
    let id = rel.rsplit('/').next().unwrap().trim_end_matches(".md");
    let text = page_text(id, title);
    stage_text(root, cfg, store, rel, &text);
    text
}

/// Stages exactly `text` and records the ledger row `ingest commit` would leave.
fn stage_text(root: &Path, cfg: &Config, store: &mut Store, rel: &str, text: &str) {
    staging::write_page(root, cfg, rel.trim_end_matches(".md"), text).unwrap();
    let staged = staging::dir(root).join(rel);
    let key = format!("{}/{}", cfg.paths.wiki.trim_matches('/'), rel);
    store
        .upsert_page_meta(&PageMetaRow {
            page_key: key,
            hash: hash::hash_file(&staged, true).unwrap(),
            updated: String::from("2026-01-01"),
        })
        .unwrap();
}

/// A page that cites raws, for the overlap rule.
fn sourced_page(id: &str, title: &str, sources: &[&str]) -> String {
    let list: String = sources
        .iter()
        .map(|source| format!("  - {source}\n"))
        .collect();
    format!(
        "---\nid: {id}\ntitle: {title}\ntype: concept\nsources:\n{list}status: draft\nupdated: 2026-01-01\n---\n正文\n"
    )
}

/// Writes a page straight into `.wiki/pages/`, bypassing staging.
fn publish(root: &Path, rel: &str, text: &str) {
    write_file(root, &format!(".wiki/pages/{rel}"), text);
}

/// Replaces the manifest row for one raw-relative source key.
fn set_source(root: &Path, key: &str, baseline: Option<&str>, current: Option<&str>) {
    let mut manifest = Manifest::load(root).unwrap();
    manifest.sources.insert(
        String::from(key),
        ManifestEntry {
            baseline_hash: baseline.map(String::from),
            current_hash: current.map(String::from),
            ..ManifestEntry::default()
        },
    );
    Manifest::save(root, &manifest).unwrap();
}

/// Full digests of every file below `dir`, keyed by `/`-separated relative path.
fn dir_hashes(dir: &Path) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    if !dir.is_dir() {
        return out;
    }
    for entry in WalkDir::new(dir).sort_by_file_name() {
        let entry = entry.unwrap();
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry
            .path()
            .strip_prefix(dir)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        out.insert(rel, hash::hash_file(entry.path(), true).unwrap());
    }
    out
}

/// Page digests only: the catalogue and the log are not pages.
fn page_hashes(dir: &Path) -> BTreeMap<String, String> {
    render::page_files(dir)
        .unwrap()
        .into_iter()
        .map(|path| {
            let rel = path
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            (rel, hash::hash_file(&path, true).unwrap())
        })
        .collect()
}

fn version_dirs(root: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(root.join(".wiki").join("versions"))
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

fn read_maybe(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

fn log_of(root: &Path, cfg: &Config) -> String {
    read_maybe(&root.join(&cfg.paths.wiki).join("log.md"))
}

#[test]
fn checkpoint_names_versions_in_sequence() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();

    for expected in ["v001", "v002", "v003"] {
        let name = version::checkpoint(root, &cfg, &mut store, "manual").unwrap();
        assert_eq!(name, expected);
        assert!(
            root.join(".wiki/versions").join(expected).is_dir(),
            "version directory {expected} is missing"
        );
    }
}

#[test]
fn checkpoint_copies_current_wiki_and_prunes_old() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    let text = page_text("alpha", "Alpha");
    write_file(root, ".wiki/pages/concept/alpha.md", &text);
    write_file(root, ".wiki/pages/index.md", "# 索引\n");
    write_file(
        root,
        ".wiki/pages/log.md",
        "# 日志\n\n## [2026-01-01] init | test\n",
    );

    for seq in 1..=8 {
        let name = version::checkpoint(root, &cfg, &mut store, "manual").unwrap();
        assert_eq!(name, format!("v{seq:03}"));
    }

    let owned: Vec<String> = version_dirs(root)
        .into_iter()
        .filter(|name| {
            root.join(".wiki/versions")
                .join(name)
                .join(".version.json")
                .is_file()
        })
        .collect();
    let expected: Vec<String> = (4..=8).map(|seq| format!("v{seq:03}")).collect();
    assert_eq!(owned, expected);

    // A snapshot is a manifest of digests, not a copy: the bytes live once in
    // the object store, and `.wiki/versions/v008/` holds no page at all.
    let snapshot = root.join(".wiki/versions/v008");
    assert!(
        !snapshot.join("concept/alpha.md").exists(),
        "no byte copies"
    );
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(snapshot.join("manifest.json")).unwrap()).unwrap();
    let entries = manifest["entries"].as_array().unwrap();
    let entry = entries
        .iter()
        .find(|entry| entry["path"] == "pages/concept/alpha.md")
        .expect("the page is in the manifest");
    let sha = entry["sha"].as_str().unwrap().to_string();
    let hex = sha.strip_prefix("blake3:").unwrap();
    let object = root.join(".wiki/objects").join(&hex[..2]).join(hex);
    assert_eq!(fs::read_to_string(object).unwrap(), text);
    assert!(
        entries.iter().any(|entry| entry["path"] == "pages/log.md"),
        "bookkeeping is content too"
    );

    // Editing the live page must not reach the object the snapshot points at.
    fs::write(root.join(".wiki/pages/concept/alpha.md"), "改了\n").unwrap();
    let hex = sha.strip_prefix("blake3:").unwrap();
    let object = root.join(".wiki/objects").join(&hex[..2]).join(hex);
    assert_eq!(fs::read_to_string(object).unwrap(), text);
}

#[test]
fn checkpoint_prunes_but_keeps_foreign_directories() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    write_file(root, ".wiki/versions/keepme/backup.md", "手工备份\n");

    for seq in 1..=8 {
        version::checkpoint(root, &cfg, &mut store, "manual").unwrap();
        let _ = seq;
    }

    let names = version_dirs(root);
    assert!(
        names.contains(&String::from("keepme")),
        "a directory without .version.json is not ours to delete"
    );
    assert!(root.join(".wiki/versions/keepme/backup.md").is_file());
    let owned: Vec<&String> = names
        .iter()
        .filter(|name| {
            root.join(".wiki/versions")
                .join(name)
                .join(".version.json")
                .is_file()
        })
        .collect();
    assert_eq!(owned.len(), 5);
}

#[test]
fn checkpoint_record_names_label_and_page_count() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    write_file(
        root,
        ".wiki/pages/concept/alpha.md",
        &page_text("alpha", "Alpha"),
    );
    write_file(root, ".wiki/pages/audit/2026-01-01.md", "# 审计\n");

    let name = version::checkpoint(root, &cfg, &mut store, "before-merge").unwrap();
    let raw = fs::read_to_string(
        root.join(".wiki/versions")
            .join(&name)
            .join(".version.json"),
    )
    .unwrap();
    let record: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(record["label"], "before-merge");
    assert_eq!(record["pages"], 1, "only concept/alpha.md is a page");
    let created = record["created_at"].as_str().unwrap();
    assert_eq!(
        created.len(),
        20,
        "created_at is YYYY-MM-DDTHH:MM:SSZ, got {created}"
    );
    assert!(created.ends_with('Z'));

    // The row is registered in the ledger.
    assert_eq!(store.version_count().unwrap(), 1);
}

#[test]
fn approve_without_confirm_asks_a_human() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    write_file(root, ".wiki/raw/a.md", "one\n");
    let current = hash::hash_file(&root.join(".wiki/raw/a.md"), true).unwrap();
    set_source(root, "a.md", None, Some(&current));
    stage(root, &cfg, &mut store, "concept/alpha.md", "Alpha");

    let outcome = version::approve(root, &cfg, &mut store, false).unwrap();
    match outcome {
        ApproveOutcome::NeedsConfirmation { reason, detail } => {
            assert_eq!(reason, "merge_staging");
            assert_eq!(detail, "1 页待合并到 wiki/");
        }
        other => panic!("expected NeedsConfirmation, got {other:?}"),
    }
    assert!(
        !root.join(".wiki/pages/concept/alpha.md").exists(),
        "nothing may reach wiki without confirm"
    );
}

#[test]
fn approve_with_confirm_merges_into_wiki() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    write_file(root, ".wiki/raw/a.md", "one\n");
    let current = hash::hash_file(&root.join(".wiki/raw/a.md"), true).unwrap();
    set_source(root, "a.md", None, Some(&current));
    let text = stage(root, &cfg, &mut store, "concept/alpha.md", "Alpha");

    let outcome = version::approve(root, &cfg, &mut store, true).unwrap();
    match outcome {
        ApproveOutcome::Applied { version } => assert_eq!(version, "v001"),
        other => panic!("expected Applied, got {other:?}"),
    }

    assert_eq!(
        fs::read_to_string(root.join(".wiki/pages/concept/alpha.md")).unwrap(),
        text
    );
    assert!(
        !staging::dir(root).join("concept/alpha.md").exists(),
        "the candidate moved out of staging"
    );
    assert!(fs::read_to_string(root.join(".wiki/pages/index.md"))
        .unwrap()
        .contains("Alpha"));
    let log = log_of(root, &cfg);
    assert!(log.contains("approve"), "log.md records the merge: {log}");
    assert!(log.contains("v001"));
    assert!(staging::manifest_pending(root).unwrap().is_empty());
    assert!(!staging::dir(root).join("pending.json").exists());
    assert!(root.join(".wiki/versions/v001/.version.json").is_file());

    let manifest = Manifest::load(root).unwrap();
    assert_eq!(
        manifest.sources["a.md"].baseline_hash.as_deref(),
        Some(current.as_str())
    );
    assert_eq!(store.version_count().unwrap(), 1);
}

#[test]
fn approve_with_no_candidates_refuses() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();

    for confirm in [false, true] {
        let err = version::approve(root, &cfg, &mut store, confirm).unwrap_err();
        assert_eq!(err.code(), "E_NOTHING_TO_APPROVE");
    }
}

#[test]
fn approve_refuses_a_hand_edited_candidate() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    stage(root, &cfg, &mut store, "concept/alpha.md", "Alpha");
    // Same path, different bytes: this is the drift the gate must catch.
    write_file(
        &staging::dir(root),
        "concept/alpha.md",
        &page_text("alpha", "Alpha").replace("正文", "手改"),
    );

    let err = version::approve(root, &cfg, &mut store, true).unwrap_err();
    assert_eq!(err.code(), "E_STAGING_DRIFT");
    assert!(
        err.to_string().contains("concept/alpha.md"),
        "detail names the page: {err}"
    );
    assert!(!root.join(".wiki/pages/concept/alpha.md").exists());
    assert!(version_dirs(root).is_empty());
}

#[test]
fn approve_refuses_a_stale_source() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    write_file(root, ".wiki/raw/a.md", "one\n");
    let old = hash::hash_file(&root.join(".wiki/raw/a.md"), true).unwrap();
    stage(root, &cfg, &mut store, "concept/alpha.md", "Alpha");
    set_source(root, "a.md", None, Some(&old));
    // The source changed after the round: merging would publish stale synthesis.
    write_file(root, ".wiki/raw/a.md", "two\n");

    let err = version::approve(root, &cfg, &mut store, false).unwrap_err();
    assert_eq!(err.code(), "E_STAGING_STALE");
    assert!(
        err.to_string().contains("a.md"),
        "detail names the source: {err}"
    );
    assert!(!root.join(".wiki/pages/concept/alpha.md").exists());
}

#[test]
fn approve_refuses_a_vanished_source() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    write_file(root, ".wiki/raw/a.md", "one\n");
    let old = hash::hash_file(&root.join(".wiki/raw/a.md"), true).unwrap();
    stage(root, &cfg, &mut store, "concept/alpha.md", "Alpha");
    set_source(root, "a.md", None, Some(&old));
    fs::remove_file(root.join(".wiki/raw/a.md")).unwrap();

    let err = version::approve(root, &cfg, &mut store, true).unwrap_err();
    assert_eq!(err.code(), "E_STAGING_STALE");
}

#[test]
fn reject_restores_staging_to_the_wiki_mirror() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    let text = page_text("alpha", "Alpha");
    write_file(root, ".wiki/pages/concept/alpha.md", &text);
    staging::ensure(root, &cfg).unwrap();
    let staged = staging::dir(root).join("concept/alpha.md");
    fs::write(&staged, page_text("alpha", "Alpha").replace("正文", "手改")).unwrap();
    stage(root, &cfg, &mut store, "concept/beta.md", "Beta");
    set_source(root, "a.md", Some("blake3:old"), Some("blake3:new"));
    let mut pending = BTreeMap::new();
    pending.insert(String::from("a.md"), String::from("blake3:new"));
    staging::write_pending(root, &pending).unwrap();

    version::reject(root, &cfg, &mut store, "内容不对").unwrap();

    assert!(
        !staging::dir(root).join("concept/beta.md").exists(),
        "a candidate absent from wiki is deleted"
    );
    assert!(
        !root.join(".wiki/pages/concept/beta.md").exists(),
        "reject never publishes"
    );
    assert_eq!(
        fs::read(&staged).unwrap(),
        fs::read(root.join(".wiki/pages/concept/alpha.md")).unwrap()
    );
    let manifest = Manifest::load(root).unwrap();
    assert_eq!(
        manifest.sources["a.md"].baseline_hash.as_deref(),
        Some("blake3:old")
    );
    assert_eq!(
        manifest.sources["a.md"].current_hash.as_deref(),
        Some("blake3:old"),
        "reject rolls the current hash back to the baseline"
    );
    assert!(staging::manifest_pending(root).unwrap().is_empty());
    assert!(!staging::dir(root).join("pending.json").exists());
    let log = log_of(root, &cfg);
    assert!(log.contains("reject"), "log.md records the reject: {log}");
    assert!(
        log.contains("内容不对"),
        "the reason survives in log.md: {log}"
    );
}

#[test]
fn reject_requires_a_reason() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    stage(root, &cfg, &mut store, "concept/alpha.md", "Alpha");

    let err = version::reject(root, &cfg, &mut store, "").unwrap_err();
    assert_eq!(err.code(), "E_NO_REASON");
    assert!(
        !root.join(".wiki/pages/log.md").exists(),
        "a refused reject writes nothing"
    );
    assert!(staging::dir(root).join("concept/alpha.md").exists());
}

#[test]
fn instruct_writes_the_text_verbatim() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    let path = root.join(".wiki/next-instruction.md");

    version::instruct(root, &cfg, &mut store, "先写对比章节").unwrap();
    assert_eq!(fs::read_to_string(&path).unwrap(), "先写对比章节\n");

    version::instruct(root, &cfg, &mut store, "已经带换行\n").unwrap();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "已经带换行\n",
        "no second newline is added"
    );

    assert!(log_of(root, &cfg).contains("instruct"));
}

#[test]
fn instruct_requires_text() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();

    let err = version::instruct(root, &cfg, &mut store, "").unwrap_err();
    assert_eq!(err.code(), "E_NO_TEXT");
    assert!(!root.join(".wiki/next-instruction.md").exists());
}

#[test]
fn defer_leaves_baseline_untouched() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    stage(root, &cfg, &mut store, "concept/alpha.md", "Alpha");
    set_source(root, "a.md", Some("blake3:old"), Some("blake3:new"));
    let mut pending = BTreeMap::new();
    pending.insert(String::from("a.md"), String::from("blake3:new"));
    staging::write_pending(root, &pending).unwrap();

    version::defer(root, &cfg, &mut store).unwrap();

    let manifest = Manifest::load(root).unwrap();
    assert_eq!(
        manifest.sources["a.md"].baseline_hash.as_deref(),
        Some("blake3:old")
    );
    assert_eq!(
        manifest.sources["a.md"].current_hash.as_deref(),
        Some("blake3:new")
    );
    assert_eq!(staging::manifest_pending(root).unwrap().len(), 1);
    assert!(log_of(root, &cfg).contains("defer"));
}

#[test]
fn defer_changes_nothing_else() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    stage(root, &cfg, &mut store, "concept/alpha.md", "Alpha");
    let staging_before = dir_hashes(&staging::dir(root));
    let wiki_pages_before = page_hashes(&root.join(&cfg.paths.wiki));

    version::defer(root, &cfg, &mut store).unwrap();

    assert_eq!(
        dir_hashes(&staging::dir(root)),
        staging_before,
        "staging is untouched"
    );
    assert_eq!(
        page_hashes(&root.join(&cfg.paths.wiki)),
        wiki_pages_before,
        "no page moves"
    );
    assert!(version_dirs(root).is_empty(), "defer creates no version");
}

#[test]
fn rollback_restores_pages_and_raws_and_records_itself() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    write_file(
        root,
        ".wiki/pages/concept/alpha.md",
        &page_text("alpha", "Alpha"),
    );
    write_file(root, ".wiki/raw/a.md", "# raw a\n");
    let before = version::checkpoint(root, &cfg, &mut store, "before").unwrap();

    // Both trees move, and a page appears that the version never had.
    write_file(
        root,
        ".wiki/pages/concept/alpha.md",
        &page_text("alpha", "Changed"),
    );
    write_file(root, ".wiki/raw/a.md", "# changed\n");
    write_file(
        root,
        ".wiki/pages/concept/beta.md",
        &page_text("beta", "Beta"),
    );

    let outcome = version::rollback(root, &cfg, &mut store, &before).unwrap();
    assert_eq!(outcome.restored, before);
    assert_eq!(outcome.files, 2, "one page and one raw");
    assert_eq!(
        fs::read_to_string(root.join(".wiki/pages/concept/alpha.md")).unwrap(),
        page_text("alpha", "Alpha")
    );
    assert_eq!(
        fs::read_to_string(root.join(".wiki/raw/a.md")).unwrap(),
        "# raw a\n"
    );
    assert!(
        !root.join(".wiki/pages/concept/beta.md").exists(),
        "a page the version lacks is removed, not merged"
    );

    // The rollback is a version of its own, so it can be undone.
    let rows = version::history(root).unwrap();
    assert_eq!(rows[0].version, outcome.version);
    assert_eq!(rows[0].kind, "rollback");
    assert_eq!(rows[0].pages, 1, "only concept/alpha.md counts as a page");
    assert_eq!(version::diff(root, &cfg, &before).unwrap().changed.len(), 0);
}

#[test]
fn diff_reports_added_removed_and_changed_paths() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    write_file(
        root,
        ".wiki/pages/concept/alpha.md",
        &page_text("alpha", "Alpha"),
    );
    write_file(
        root,
        ".wiki/pages/concept/beta.md",
        &page_text("beta", "Beta"),
    );
    let before = version::checkpoint(root, &cfg, &mut store, "before").unwrap();

    write_file(
        root,
        ".wiki/pages/concept/alpha.md",
        &page_text("alpha", "Edited"),
    );
    fs::remove_file(root.join(".wiki/pages/concept/beta.md")).unwrap();
    write_file(
        root,
        ".wiki/pages/concept/gamma.md",
        &page_text("gamma", "Gamma"),
    );

    let diff = version::diff(root, &cfg, &before).unwrap();
    assert_eq!(diff.added, vec![String::from("pages/concept/gamma.md")]);
    assert_eq!(diff.removed, vec![String::from("pages/concept/beta.md")]);
    assert_eq!(diff.changed, vec![String::from("pages/concept/alpha.md")]);
}

#[test]
fn an_unknown_version_is_an_error_not_a_silent_noop() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    let err = version::rollback(root, &cfg, &mut store, "v999").unwrap_err();
    assert_eq!(err.code(), "E_NO_VERSION");
    let err = version::diff(root, &cfg, "v999").unwrap_err();
    assert_eq!(err.code(), "E_NO_VERSION");
    assert!(version::history(root).unwrap().is_empty());
}

#[test]
fn approve_confirm_overrules_the_page_budget() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    for seq in 0..21 {
        stage(
            root,
            &cfg,
            &mut store,
            &format!("concept/p{seq:02}.md"),
            &format!("P{seq}"),
        );
    }

    // 21 new pages against a budget of 20: the asking path refuses, but
    // `--confirm` is the human overruling the cap, not another ask.
    match version::approve(root, &cfg, &mut store, true).unwrap() {
        ApproveOutcome::Applied { .. } => {}
        other => panic!("expected the merge to go through, got {other:?}"),
    }
    assert!(root.join(".wiki/pages/concept/p00.md").is_file());
    assert!(root.join(".wiki/pages/concept/p20.md").is_file());
}

#[test]
fn approve_asks_when_a_round_would_add_too_many_pages() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    for seq in 0..21 {
        stage(
            root,
            &cfg,
            &mut store,
            &format!("concept/p{seq:02}.md"),
            &format!("P{seq}"),
        );
    }

    match version::approve(root, &cfg, &mut store, false).unwrap() {
        ApproveOutcome::NeedsConfirmation { reason, detail } => {
            assert_eq!(reason, "page_budget");
            assert!(detail.contains("21"), "{detail}");
        }
        other => panic!("expected page_budget, got {other:?}"),
    }
    assert!(
        staging::dir(root).join("concept/p00.md").is_file(),
        "the candidates stay staged: nothing was published"
    );

    // One fewer page fits the budget, so the only remaining reason to ask is
    // the merge itself.
    fs::remove_file(staging::dir(root).join("concept/p20.md")).unwrap();
    match version::approve(root, &cfg, &mut store, false).unwrap() {
        ApproveOutcome::NeedsConfirmation { reason, .. } => {
            assert_eq!(reason, "merge_staging");
        }
        other => panic!("expected merge_staging, got {other:?}"),
    }
}

#[test]
fn approve_asks_when_a_candidate_repeats_a_published_title() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    publish(
        root,
        "concept/alpha.md",
        &page_text("alpha", "Retry Policy"),
    );
    // Same title modulo punctuation and case: the same page under a new id.
    stage_text(
        root,
        &cfg,
        &mut store,
        "concept/retry-policy.md",
        &page_text("retry-policy", "retry_policy"),
    );

    match version::approve(root, &cfg, &mut store, false).unwrap() {
        ApproveOutcome::NeedsConfirmation { reason, detail } => {
            assert_eq!(reason, "page_overlap");
            assert!(detail.contains("concept/alpha.md"), "{detail}");
        }
        other => panic!("expected page_overlap, got {other:?}"),
    }
    assert!(staging::dir(root).join("concept/retry-policy.md").is_file());
}

#[test]
fn approve_asks_when_a_candidate_adds_no_new_source() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    publish(
        root,
        "concept/alpha.md",
        &sourced_page("alpha", "Alpha", &[".wiki/raw/a.md"]),
    );
    // The wiki already says what this raw says; a second page adds nothing.
    stage_text(
        root,
        &cfg,
        &mut store,
        "concept/beta.md",
        &sourced_page("beta", "The same fact", &[".wiki/raw/a.md"]),
    );

    match version::approve(root, &cfg, &mut store, false).unwrap() {
        ApproveOutcome::NeedsConfirmation { reason, detail } => {
            assert_eq!(reason, "page_overlap");
            assert!(detail.contains("没有新证据"), "{detail}");
        }
        other => panic!("expected page_overlap, got {other:?}"),
    }
}

#[test]
fn a_candidate_citing_a_new_source_is_not_an_overlap() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    publish(
        root,
        "concept/alpha.md",
        &sourced_page("alpha", "Alpha", &[".wiki/raw/a.md"]),
    );
    stage_text(
        root,
        &cfg,
        &mut store,
        "concept/beta.md",
        &sourced_page("beta", "Beta", &[".wiki/raw/a.md", ".wiki/raw/b.md"]),
    );

    match version::approve(root, &cfg, &mut store, false).unwrap() {
        ApproveOutcome::NeedsConfirmation { reason, .. } => assert_eq!(reason, "merge_staging"),
        other => panic!("expected merge_staging, got {other:?}"),
    }
}

#[test]
fn approve_confirm_overrules_the_overlap_gate() {
    let (dir, cfg, mut store) = boot();
    let root = dir.path();
    publish(
        root,
        "concept/alpha.md",
        &page_text("alpha", "Retry Policy"),
    );
    stage_text(
        root,
        &cfg,
        &mut store,
        "concept/retry-policy.md",
        &page_text("retry-policy", "Retry Policy"),
    );

    match version::approve(root, &cfg, &mut store, true).unwrap() {
        ApproveOutcome::Applied { .. } => {}
        other => panic!("expected the merge to go through, got {other:?}"),
    }
    assert!(root.join(".wiki/pages/concept/retry-policy.md").is_file());
}
