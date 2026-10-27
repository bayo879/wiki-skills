//! `archive status`: the four numbers the layering is steered by.
//!
//! The fixture seeds the ledger and the session directory directly, because
//! `status` is defined by what is on disk and in the database, not by what a
//! command happened to leave behind.

use std::fs;
use std::path::{Path, PathBuf};

use llm_wiki::archive::{PurgeRequest, RecallRequest, SealReport, SealRequest};
use llm_wiki::config::Config;
use llm_wiki::init;
use llm_wiki::store::{ArchiveRow, RawRow, SourceRow, Store};
use tempfile::{tempdir, TempDir};

/// A temporary repository with a loaded config and a migrated ledger.
struct Fixture {
    dir: TempDir,
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
        Fixture { dir, cfg, store }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    /// Writes a live session record under `.wiki/sessions/`.
    fn put_session(&self, name: &str) {
        let path = self
            .root()
            .join(".wiki/sessions")
            .join(format!("{name}.md"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, "## 用户\n\n讨论了一下同步。\n").unwrap();
    }

    /// Records a raw and its file, plus the source file it came from.
    fn put_converted_source(&mut self, key: &str) {
        self.put_source(key);
        let raw = self.root().join("raw").join(key);
        fs::create_dir_all(raw.parent().unwrap()).unwrap();
        fs::write(&raw, "---\nname: a\n---\n正文\n").unwrap();
        let source = self.root().join("source").join(key);
        fs::create_dir_all(source.parent().unwrap()).unwrap();
        fs::write(&source, "原始字节").unwrap();
        self.store
            .upsert_raw(&RawRow {
                key: String::from(key),
                hash: String::from("blake3:y"),
                source_key: String::from(key),
                converted_at: String::from("2026-01-02T10:00:00+08:00"),
            })
            .unwrap();
    }

    /// Records a source in the ledger so `seal` will accept it.
    fn put_source(&mut self, key: &str) {
        self.store
            .upsert_source(&SourceRow {
                key: String::from(key),
                hash: String::from("blake3:x"),
                size: 1,
                mtime: 0,
                raw_key: Some(String::from(key)),
                converted_at: None,
            })
            .unwrap();
    }

    /// Writes the summary the agent is expected to hand to `seal`.
    fn put_summary(&self, id: &str, sources: &[&str]) -> PathBuf {
        let path = self.root().join("wiki/archive").join(format!("{id}.md"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = page_text(id, sources);
        text = text.replace("type: concept", "type: concept\narchived: true");
        fs::write(&path, text).unwrap();
        path
    }

    /// Writes a page whose `sources` list is `sources`.
    fn put_page(&self, id: &str, sources: &[&str]) {
        let path = self.root().join("wiki/concept").join(format!("{id}.md"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, page_text(id, sources)).unwrap();
    }

    /// Records a sealed summary; `period_end` decides how old it is.
    fn seal(&mut self, key: &str, period_end: &str, recalled_at: Option<&str>) {
        self.store
            .upsert_archive(&ArchiveRow {
                key: String::from(key),
                period_start: String::from("2026-01-01"),
                period_end: String::from(period_end),
                sources: vec![String::from("queries/a.md")],
                sealed_at: String::from("2026-01-02T10:00:00+08:00"),
                sealed_by: String::from("human"),
                last_recalled_at: recalled_at.map(String::from),
                recall_count: i64::from(recalled_at.is_some()),
            })
            .unwrap();
    }
}

/// A page with every required field, citing `sources`.
fn page_text(id: &str, sources: &[&str]) -> String {
    let list = sources
        .iter()
        .map(|s| format!("  - {s}\n"))
        .collect::<String>();
    format!(
        "---\nid: {id}\ntitle: {id}\ntype: concept\nsummary: 摘要\nsources:\n{list}status: stable\nupdated: 2026-01-05\n---\n\n这一页的正文足够长，写清楚前因后果，测试才会认为它达标。\n"
    )
}

/// A fixed "today" so the deadline arithmetic is deterministic.
fn today() -> chrono::NaiveDate {
    chrono::NaiveDate::from_ymd_opt(2026, 6, 1).unwrap()
}

fn status(f: &Fixture) -> llm_wiki::archive::StatusReport {
    llm_wiki::archive::status_at(f.root(), &f.cfg, &f.store, today()).unwrap()
}

#[test]
fn empty_archive_reports_zeros() {
    let f = Fixture::new();
    let report = status(&f);
    assert_eq!(report.archives, 0);
    assert_eq!(report.live_sessions, 0);
    assert_eq!(report.purge_candidates, 0);
    assert_eq!(report.still_referenced, 0);
}

#[test]
fn counts_sealed_summaries_and_live_session_records() {
    let mut f = Fixture::new();
    f.put_session("2026-05-01-sync");
    f.put_session("2026-05-02-lint");
    f.seal("wiki/archive/2026-04.md", "2026-04-30", Some("2026-05-20"));

    let report = status(&f);
    assert_eq!(report.archives, 1);
    assert_eq!(report.live_sessions, 2);
}

#[test]
fn counts_only_archives_a_page_still_cites() {
    let mut f = Fixture::new();
    f.seal("wiki/archive/2026-04.md", "2026-04-30", None);
    f.seal("wiki/archive/2026-05.md", "2026-05-31", None);
    f.put_page("sync", &["wiki/archive/2026-04.md"]);

    // Re-hanging `sources` is what keeps a summary alive; the count is how the
    // seal proves it actually rewired the pages instead of orphaning them.
    assert_eq!(status(&f).still_referenced, 1);
}

#[test]
fn purge_candidates_need_capacity_and_deadline_both() {
    let mut f = Fixture::new();
    // Three summaries against a cap of two: over capacity, but only the ones
    // whose last touch is past the deadline are candidates.
    f.seal("wiki/archive/a.md", "2025-01-31", None);
    f.seal("wiki/archive/b.md", "2025-01-31", None);
    f.seal("wiki/archive/c.md", "2026-05-31", Some("2026-05-31"));
    f.cfg.memory.purge_cap = 2;
    f.cfg.memory.purge_days = 180;

    assert_eq!(status(&f).purge_candidates, 2);
}

#[test]
fn nothing_is_a_candidate_without_capacity_pressure() {
    let mut f = Fixture::new();
    // Ancient summaries, but the archive is nowhere near its cap: a quiet wiki
    // keeps its history rather than clearing it on age alone.
    f.seal("wiki/archive/a.md", "2024-01-31", None);
    f.seal("wiki/archive/b.md", "2024-01-31", None);
    f.cfg.memory.purge_cap = 200;
    f.cfg.memory.purge_days = 180;

    assert_eq!(status(&f).purge_candidates, 0);
}

#[test]
fn a_recent_recall_postpones_the_deadline() {
    let mut f = Fixture::new();
    f.seal("wiki/archive/a.md", "2024-01-31", Some("2026-05-31"));
    f.seal("wiki/archive/b.md", "2024-01-31", None);
    f.seal("wiki/archive/c.md", "2024-01-31", None);
    f.cfg.memory.purge_cap = 2;
    f.cfg.memory.purge_days = 180;

    // `a` was read last week, so only `b` and `c` are past the deadline.
    let report = status(&f);
    assert_eq!(report.purge_candidates, 2);
}

/// The four numbers are what `archive status` prints, so the type is public.
fn _report_is_public(r: &llm_wiki::archive::StatusReport) -> PathBuf {
    PathBuf::from(&r.note)
}

/// A fixed "today" for the seal stamp, matching [`today`].
fn seal_day() -> chrono::NaiveDate {
    today()
}

fn seal(
    f: &mut Fixture,
    req: SealRequest,
) -> std::result::Result<SealReport, llm_wiki::archive::ArchiveError> {
    let root = f.root().to_path_buf();
    let cfg = f.cfg.clone();
    llm_wiki::archive::seal_at(&root, &cfg, &mut f.store, &req, seal_day())
}

fn req_one(key: &str) -> SealRequest {
    SealRequest {
        sources: vec![String::from(key)],
        id: Some(String::from("2026-05")),
        title: Some(String::from("2026 年 5 月")),
        period_start: Some(chrono::NaiveDate::from_ymd_opt(2026, 5, 1).unwrap()),
        period_end: Some(chrono::NaiveDate::from_ymd_opt(2026, 5, 31).unwrap()),
        by: String::from("human"),
    }
}

#[test]
fn seal_records_the_summary_and_rehangs_the_pages() {
    let mut f = Fixture::new();
    f.put_source("queries/2026-05.md");
    f.put_summary("2026-05", &["raw/queries/2026-05.md"]);
    f.put_page("sync", &["raw/queries/2026-05.md", "raw/docs/a.md"]);

    let report = seal(&mut f, req_one("queries/2026-05.md")).unwrap();
    assert_eq!(report.key, "wiki/archive/2026-05.md");
    assert_eq!(report.updated_pages, 1);

    // 重挂是替换而不是追加：旧引用必须消失，否则清理原文后 L6 立刻报错。
    let text = fs::read_to_string(f.root().join("wiki/concept/sync.md")).unwrap();
    assert!(text.contains("raw/docs/a.md"), "无关键不能被顺手删掉");
    assert!(text.contains("wiki/archive/2026-05.md"));
    assert!(!text.contains("raw/queries/2026-05.md"), "会话引用应被替换");
    assert!(text.contains("updated: 2026-06-01"), "重挂要顺手更新日期");

    let row = f
        .store
        .get_archive("wiki/archive/2026-05.md")
        .unwrap()
        .expect("archive row");
    assert_eq!(row.period_start, "2026-05-01");
    assert_eq!(row.period_end, "2026-05-31");
    assert_eq!(row.sources, vec![String::from("queries/2026-05.md")]);
    assert_eq!(row.sealed_by, "human");
    assert_eq!(row.sealed_at, "2026-06-01");
}

#[test]
fn seal_refuses_a_source_that_was_never_ingested() {
    let mut f = Fixture::new();
    f.put_summary("2026-05", &["raw/queries/2026-05.md"]);
    f.put_page("sync", &["raw/queries/2026-05.md"]);

    let err = seal(&mut f, req_one("queries/2026-05.md")).unwrap_err();
    assert_eq!(err.code(), "E_NOT_LEDGERED");
    // 失败的封存不能留下半个摘要，否则页面已经换成摘要、账本里却没有记录。
    assert!(f
        .store
        .get_archive("wiki/archive/2026-05.md")
        .unwrap()
        .is_none());
    let text = fs::read_to_string(f.root().join("wiki/concept/sync.md")).unwrap();
    assert!(text.contains("raw/queries/2026-05.md"));
}

#[test]
fn seal_refuses_to_seal_the_same_period_twice() {
    let mut f = Fixture::new();
    f.put_source("queries/2026-05.md");
    f.put_summary("2026-05", &["raw/queries/2026-05.md"]);
    f.put_page("sync", &["raw/queries/2026-05.md"]);
    seal(&mut f, req_one("queries/2026-05.md")).unwrap();

    let err = seal(&mut f, req_one("queries/2026-05.md")).unwrap_err();
    assert_eq!(err.code(), "E_ALREADY_SEALED");
}

#[test]
fn seal_needs_the_summary_the_agent_writes() {
    let mut f = Fixture::new();
    f.put_source("queries/2026-05.md");
    // 没有摘要正文时封存是空的：CLI 不写正文，所以它只能拒绝。
    let err = seal(&mut f, req_one("queries/2026-05.md")).unwrap_err();
    assert_eq!(err.code(), "E_NO_SUMMARY");
}

#[test]
fn seal_leaves_pages_that_do_not_cite_the_source_alone() {
    let mut f = Fixture::new();
    f.put_source("queries/2026-05.md");
    f.put_summary("2026-05", &["raw/queries/2026-05.md"]);
    f.put_page("unrelated", &["raw/docs/a.md"]);
    let before = fs::read_to_string(f.root().join("wiki/concept/unrelated.md")).unwrap();

    let report = seal(&mut f, req_one("queries/2026-05.md")).unwrap();
    assert_eq!(report.updated_pages, 0);
    // 没引用的页不该被碰：连 updated 都不该动。
    assert_eq!(
        fs::read_to_string(f.root().join("wiki/concept/unrelated.md")).unwrap(),
        before
    );
}

fn recall(
    f: &mut Fixture,
    query: &str,
    full: bool,
) -> std::result::Result<llm_wiki::archive::RecallReport, llm_wiki::archive::ArchiveError> {
    let root = f.root().to_path_buf();
    let cfg = f.cfg.clone();
    llm_wiki::archive::recall_at(
        &root,
        &cfg,
        &mut f.store,
        &RecallRequest {
            query: String::from(query),
            full,
        },
        seal_day(),
    )
}

fn purge(f: &mut Fixture, dry_run: bool, yes: bool) -> llm_wiki::archive::PurgeReport {
    let root = f.root().to_path_buf();
    let cfg = f.cfg.clone();
    llm_wiki::archive::purge_at(
        &root,
        &cfg,
        &mut f.store,
        &PurgeRequest { dry_run, yes },
        seal_day(),
    )
    .unwrap()
}

/// Seals one old summary over a converted source, ready to be purged.
fn sealed_old(f: &mut Fixture) -> String {
    f.put_converted_source("queries/2026-01.md");
    f.put_summary("2026-01", &["raw/queries/2026-01.md"]);
    // 这一页引的是别的文档：被清理的会话没有任何页面依赖它，才清得干净。
    f.put_page("sync", &["raw/docs/a.md"]);
    seal(
        f,
        SealRequest {
            sources: vec![String::from("queries/2026-01.md")],
            id: Some(String::from("2026-01")),
            title: Some(String::from("2026 年 1 月")),
            period_start: Some(chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap()),
            period_end: Some(chrono::NaiveDate::from_ymd_opt(2026, 1, 31).unwrap()),
            by: String::from("human"),
        },
    )
    .unwrap();
    String::from("wiki/archive/2026-01.md")
}

#[test]
fn purge_dry_run_reports_without_deleting() {
    let mut f = Fixture::new();
    let key = sealed_old(&mut f);
    f.cfg.memory.purge_cap = 0;
    f.cfg.memory.purge_days = 1;

    let report = purge(&mut f, true, false);
    assert_eq!(report.candidates.len(), 1);
    assert_eq!(report.deleted, 0);
    assert!(f.root().join("source/queries/2026-01.md").exists());
    assert!(f.root().join(&key).exists(), "dry-run 不能动摘要");
}

#[test]
fn purge_clears_the_source_the_raw_and_the_summary() {
    let mut f = Fixture::new();
    let key = sealed_old(&mut f);
    f.cfg.memory.purge_cap = 0;
    f.cfg.memory.purge_days = 1;

    let report = purge(&mut f, false, true);
    assert_eq!(report.deleted, 1);
    assert!(!f.root().join("source/queries/2026-01.md").exists());
    assert!(!f.root().join("raw/queries/2026-01.md").exists());
    assert!(!f.root().join(&key).exists());
    assert!(f.store.get_archive(&key).unwrap().is_none());
    assert!(f.store.get_source("queries/2026-01.md").unwrap().is_none());
    assert!(f.store.get_raw("queries/2026-01.md").unwrap().is_none());
}

#[test]
fn purge_never_clears_a_summary_a_page_still_cites() {
    let mut f = Fixture::new();
    let key = sealed_old(&mut f);
    f.cfg.memory.purge_cap = 0;
    f.cfg.memory.purge_days = 1;
    // 摘要重建：页面重新指回它（模拟 seal 之后又有人手改过引用）。
    f.put_page("sync", &[&key]);

    let report = purge(&mut f, false, true);
    assert_eq!(report.deleted, 0, "还有页面指着它时清理会制造断链");
    assert!(f.root().join(&key).exists());
}

#[test]
fn recall_finds_a_summary_and_counts_the_hit() {
    let mut f = Fixture::new();
    let key = sealed_old(&mut f);
    let report = recall(&mut f, "摘要", false).unwrap();
    assert_eq!(report.hits.len(), 1);
    assert_eq!(report.hits[0].key, key);
    let row = f.store.get_archive(&key).unwrap().unwrap();
    assert_eq!(row.recall_count, 1, "recall 是 purge 第二条件的唯一数据源");
    assert_eq!(row.last_recalled_at.as_deref(), Some("2026-06-01"));
}

#[test]
fn recall_full_returns_more_than_the_default_excerpt() {
    let mut f = Fixture::new();
    let key = sealed_old(&mut f);
    let long = "长".repeat(1000);
    let path = f.root().join(&key);
    let text = fs::read_to_string(&path).unwrap();
    fs::write(&path, text + &long).unwrap();

    let short = recall(&mut f, "摘要", false).unwrap();
    let full = recall(&mut f, "摘要", true).unwrap();
    assert!(short.hits[0].body.chars().count() < 600);
    assert!(full.hits[0].body.chars().count() > 1000);
}

#[test]
fn recall_reports_a_miss() {
    let mut f = Fixture::new();
    sealed_old(&mut f);
    let err = recall(&mut f, "根本没有的词", false).unwrap_err();
    assert_eq!(err.code(), "E_NO_HIT");
}
