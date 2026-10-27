//! Staging is the reviewable copy: `wiki/` is approved, `.wiki/staging/` is not.
//!
//! These tests pin the properties that make the isolation real — a byte copy
//! rather than a hard link or a hash-only record — plus the manifest's role as
//! the ledger of what is still unmerged. They also pin the failure modes that
//! must be loud: a damaged manifest is reported instead of silently rebuilt,
//! and a page id cannot escape the staging directory.

use std::collections::BTreeMap;
use std::fs;

use llm_wiki::{config::Config, init, staging};
use tempfile::tempdir;

/// A page whose frontmatter names `sources`.
fn page(id: &str, title: &str, sources: &[&str]) -> String {
    let list = sources
        .iter()
        .map(|source| format!("{source:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "---\nid: {id}\ntitle: {title}\ntype: concept\nstatus: draft\nupdated: 2026-10-27\nsources: [{list}]\n---\n正文\n"
    )
}

/// A manifest entry with both hash fields spelled out.
fn entry(baseline: Option<&str>, current: Option<&str>, pages: &[&str]) -> staging::ManifestEntry {
    staging::ManifestEntry {
        baseline_hash: baseline.map(String::from),
        current_hash: current.map(String::from),
        last_processed_at: None,
        pages: pages.iter().map(|page| String::from(*page)).collect(),
    }
}

/// An initialised repository and its loaded contract.
fn boot() -> (tempfile::TempDir, Config) {
    let dir = tempdir().unwrap();
    init::run(dir.path(), false).unwrap();
    let cfg = Config::load(dir.path()).unwrap();
    (dir, cfg)
}

fn manifest_path(root: &std::path::Path) -> std::path::PathBuf {
    root.join(".wiki/staging/manifest.json")
}

#[test]
fn staging_is_a_real_copy_and_editing_it_leaves_wiki_untouched() {
    let (dir, cfg) = boot();
    fs::create_dir_all(dir.path().join("wiki/concept")).unwrap();
    fs::write(dir.path().join("wiki/concept/a.md"), "A").unwrap();

    let out = staging::ensure(dir.path(), &cfg).unwrap();
    assert!(out.created);
    assert_eq!(out.copied, 1);

    let approved = dir.path().join("wiki/concept/a.md");
    let staged = dir.path().join(".wiki/staging/concept/a.md");
    assert_eq!(fs::read_to_string(&staged).unwrap(), "A");

    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_ne!(
            fs::metadata(&approved).unwrap().ino(),
            fs::metadata(&staged).unwrap().ino(),
            "禁止 hardlink，会破坏隔离"
        );
    }

    // The whole point of the copy: a candidate edit must not reach `wiki/`.
    fs::write(&staged, "candidate").unwrap();
    assert_eq!(
        fs::read_to_string(&approved).unwrap(),
        "A",
        "改 staging 不能影响已批准的 wiki"
    );
}

#[test]
fn ensure_twice_does_not_recopy() {
    let (dir, cfg) = boot();
    fs::write(dir.path().join("wiki/index.md"), "# 索引\n").unwrap();

    let first = staging::ensure(dir.path(), &cfg).unwrap();
    assert!(first.created);
    assert_eq!(first.copied, 1);

    let before = fs::read_to_string(manifest_path(dir.path())).unwrap();
    let again = staging::ensure(dir.path(), &cfg).unwrap();
    assert!(!again.created);
    assert_eq!(again.copied, 0);
    assert_eq!(
        fs::read_to_string(manifest_path(dir.path())).unwrap(),
        before,
        "第二次 ensure 不得改写已有 manifest"
    );
}

#[test]
fn ensure_rebuilds_after_staging_is_deleted() {
    let (dir, cfg) = boot();
    fs::create_dir_all(dir.path().join("wiki/concept")).unwrap();
    fs::write(dir.path().join("wiki/concept/a.md"), "A").unwrap();
    fs::write(dir.path().join("wiki/index.md"), "# 索引\n").unwrap();
    staging::ensure(dir.path(), &cfg).unwrap();

    fs::remove_dir_all(dir.path().join(".wiki/staging")).unwrap();
    let out = staging::ensure(dir.path(), &cfg).unwrap();
    assert!(out.created);
    assert_eq!(out.copied, 2);
    assert_eq!(
        fs::read_to_string(dir.path().join(".wiki/staging/concept/a.md")).unwrap(),
        "A"
    );
    assert!(manifest_path(dir.path()).is_file());
}

#[test]
fn ensure_keeps_unmerged_manifest_entries() {
    let (dir, cfg) = boot();
    fs::write(dir.path().join("wiki/index.md"), "# 索引\n").unwrap();
    staging::ensure(dir.path(), &cfg).unwrap();

    let mut manifest = staging::Manifest::load(dir.path()).unwrap();
    manifest.sources.insert(
        String::from("a.md"),
        entry(Some("blake3:old"), Some("blake3:new"), &["concept/a"]),
    );
    staging::Manifest::save(dir.path(), &manifest).unwrap();

    let out = staging::ensure(dir.path(), &cfg).unwrap();
    assert!(!out.created);
    let after = staging::Manifest::load(dir.path()).unwrap();
    assert_eq!(
        after.sources.get("a.md"),
        Some(&entry(
            Some("blake3:old"),
            Some("blake3:new"),
            &["concept/a"]
        )),
        "ensure 不能丢掉上一轮没处理完的条目"
    );
    assert_eq!(
        staging::manifest_pending(dir.path()).unwrap().get("a.md"),
        Some(&String::from("blake3:new"))
    );
}

#[test]
fn write_page_accumulates_pages_per_source_without_duplicates() {
    let (dir, cfg) = boot();
    staging::write_page(
        dir.path(),
        &cfg,
        "concept/a",
        &page("a", "甲", &["raw/x.md#p=1", "raw/x.md#p=2"]),
    )
    .unwrap();
    staging::write_page(
        dir.path(),
        &cfg,
        "concept/b",
        &page("b", "乙", &["raw/x.md"]),
    )
    .unwrap();
    // Writing the same page again must not append it a second time.
    staging::write_page(
        dir.path(),
        &cfg,
        "concept/a",
        &page("a", "甲", &["raw/x.md"]),
    )
    .unwrap();

    let manifest = staging::Manifest::load(dir.path()).unwrap();
    assert_eq!(
        manifest.sources.get("x.md").map(|entry| &entry.pages),
        Some(&vec![String::from("concept/a"), String::from("concept/b")]),
        "同一源的页面要累积、按写入顺序、不重复"
    );
}

#[test]
fn empty_wiki_yields_an_empty_manifest() {
    let (dir, cfg) = boot();
    fs::remove_dir_all(dir.path().join("wiki")).unwrap();

    let out = staging::ensure(dir.path(), &cfg).unwrap();
    assert!(out.created);
    assert_eq!(out.copied, 0);

    let json: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(manifest_path(dir.path())).unwrap()).unwrap();
    assert_eq!(json, serde_json::json!({"sources": {}}));
    assert!(staging::Manifest::load(dir.path())
        .unwrap()
        .sources
        .is_empty());
}

#[test]
fn corrupt_manifest_is_reported_not_rebuilt() {
    let (dir, cfg) = boot();
    fs::write(dir.path().join("wiki/index.md"), "# 索引\n").unwrap();
    staging::ensure(dir.path(), &cfg).unwrap();

    let half = "{\"sources\": {";
    fs::write(manifest_path(dir.path()), half).unwrap();
    let err = staging::ensure(dir.path(), &cfg).unwrap_err();
    assert_eq!(err.code(), "E_STAGING_CORRUPT");
    assert_eq!(
        fs::read_to_string(manifest_path(dir.path())).unwrap(),
        half,
        "坏 manifest 不能被静默重建覆盖"
    );
}

#[test]
fn write_page_rejects_paths_that_escape_staging() {
    let (dir, cfg) = boot();
    for id in [
        "../escape",
        "concept/../../escape",
        "/absolute/path",
        "concept/../evil",
    ] {
        let err = staging::write_page(dir.path(), &cfg, id, "x").unwrap_err();
        assert_eq!(err.code(), "E_STAGING_PATH", "id {id:?} 必须被拒绝");
    }
    assert!(!dir.path().join("escape.md").exists());
    assert!(!dir.path().join("evil.md").exists());
}

#[test]
fn pending_hashes_lists_only_sources_that_moved_since_baseline() {
    let (dir, cfg) = boot();
    fs::write(dir.path().join("wiki/index.md"), "# 索引\n").unwrap();
    staging::ensure(dir.path(), &cfg).unwrap();

    let mut manifest = staging::Manifest::default();
    manifest.sources.insert(
        String::from("changed.md"),
        entry(Some("old"), Some("new"), &[]),
    );
    manifest
        .sources
        .insert(String::from("same.md"), entry(Some("h"), Some("h"), &[]));
    manifest
        .sources
        .insert(String::from("new.md"), entry(None, Some("h2"), &[]));
    manifest
        .sources
        .insert(String::from("untouched.md"), entry(None, None, &[]));
    manifest
        .sources
        .insert(String::from("vanished.md"), entry(Some("h3"), None, &[]));
    staging::Manifest::save(dir.path(), &manifest).unwrap();

    let pending = staging::manifest_pending(dir.path()).unwrap();
    let expected: BTreeMap<String, String> = [("changed.md", "new"), ("new.md", "h2")]
        .iter()
        .map(|(key, hash)| (String::from(*key), String::from(*hash)))
        .collect();
    assert_eq!(pending, expected);
}

#[test]
fn ensure_copies_new_wiki_pages_without_overwriting_candidates() {
    let (dir, cfg) = boot();
    fs::write(dir.path().join("wiki/index.md"), "# 索引\n").unwrap();
    staging::ensure(dir.path(), &cfg).unwrap();

    // A candidate edit in staging must survive a later ensure.
    fs::write(dir.path().join(".wiki/staging/index.md"), "# 候选索引\n").unwrap();
    fs::create_dir_all(dir.path().join("wiki/concept")).unwrap();
    fs::write(dir.path().join("wiki/concept/b.md"), "B").unwrap();

    let out = staging::ensure(dir.path(), &cfg).unwrap();
    assert!(!out.created);
    assert_eq!(out.copied, 1, "只补 wiki 新增的文件");
    assert_eq!(
        fs::read_to_string(dir.path().join(".wiki/staging/concept/b.md")).unwrap(),
        "B"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join(".wiki/staging/index.md")).unwrap(),
        "# 候选索引\n",
        "已存在的候选不得被 wiki 覆盖"
    );
}

#[test]
fn write_page_writes_the_page_and_creates_the_manifest() {
    let (dir, cfg) = boot();
    let content = page("a", "甲", &["raw/doc.md"]);
    staging::write_page(dir.path(), &cfg, "concept/a", &content).unwrap();

    assert_eq!(
        fs::read_to_string(dir.path().join(".wiki/staging/concept/a.md")).unwrap(),
        content
    );
    assert!(
        !dir.path().join("wiki/concept/a.md").exists(),
        "write_page 只写候选区"
    );
    let manifest = staging::Manifest::load(dir.path()).unwrap();
    assert_eq!(
        manifest
            .sources
            .get("doc.md")
            .map(|entry| entry.pages.clone()),
        Some(vec![String::from("concept/a")])
    );
}

#[test]
fn manifest_roundtrips_with_sorted_keys() {
    let (dir, _cfg) = boot();
    let mut manifest = staging::Manifest::default();
    manifest.sources.insert(
        String::from("b.md"),
        entry(None, Some("hb"), &["concept/b"]),
    );
    manifest
        .sources
        .insert(String::from("a.md"), entry(Some("ha"), None, &[]));
    staging::Manifest::save(dir.path(), &manifest).unwrap();

    let text = fs::read_to_string(manifest_path(dir.path())).unwrap();
    let a = text.find("\"a.md\"").unwrap();
    let b = text.find("\"b.md\"").unwrap();
    assert!(a < b, "键必须排序，git diff 才稳定");
    assert!(text.ends_with('\n'));
    assert_eq!(staging::Manifest::load(dir.path()).unwrap(), manifest);
}

#[test]
fn a_missing_manifest_loads_as_empty() {
    let (dir, _cfg) = boot();
    assert!(staging::Manifest::load(dir.path())
        .unwrap()
        .sources
        .is_empty());
    assert!(!manifest_path(dir.path()).exists());
}

#[test]
fn pending_json_from_ingest_begin_takes_precedence() {
    let (dir, cfg) = boot();
    fs::write(dir.path().join("wiki/index.md"), "# 索引\n").unwrap();
    staging::ensure(dir.path(), &cfg).unwrap();
    let mut manifest = staging::Manifest::load(dir.path()).unwrap();
    manifest
        .sources
        .insert(String::from("old.md"), entry(Some("a"), Some("b"), &[]));
    staging::Manifest::save(dir.path(), &manifest).unwrap();

    fs::write(
        dir.path().join(".wiki/staging/pending.json"),
        "{\"round.md\":\"blake3:abc\"}",
    )
    .unwrap();
    assert_eq!(
        staging::pending_hashes(dir.path()).unwrap(),
        BTreeMap::from([(String::from("round.md"), String::from("blake3:abc"))])
    );

    assert_eq!(
        staging::pending_hashes(&dir.path().join("nowhere")).unwrap(),
        BTreeMap::new(),
        "没有在途轮次时是空表，不是错误"
    );

    fs::write(dir.path().join(".wiki/staging/pending.json"), "{oops").unwrap();
    assert_eq!(
        staging::pending_hashes(dir.path()).unwrap_err().code(),
        "E_STAGING_CORRUPT"
    );
}

#[test]
fn a_lost_manifest_beside_candidates_is_not_rebuilt() {
    let (dir, cfg) = boot();
    let content = page("a", "甲", &["raw/x.md"]);
    staging::write_page(dir.path(), &cfg, "concept/a", &content).unwrap();
    fs::remove_file(manifest_path(dir.path())).unwrap();

    // The ledger is the only record of the review queue; losing it must be
    // loud, because rebuilding an empty one would orphan the candidate.
    assert_eq!(
        staging::Manifest::load(dir.path()).unwrap_err().code(),
        "E_STAGING_CORRUPT"
    );
    assert_eq!(
        staging::ensure(dir.path(), &cfg).unwrap_err().code(),
        "E_STAGING_CORRUPT"
    );
    assert_eq!(
        fs::read_to_string(dir.path().join(".wiki/staging/concept/a.md")).unwrap(),
        content
    );
}
