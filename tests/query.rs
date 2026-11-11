//! `query`: the FTS5 search and its two index maintenance paths.
//!
//! The suite is organised around the three promises the command makes:
//!
//! - **Chinese is searchable.** `unicode61` alone sees 「订单处理」 as one token,
//!   so both the index and the query go through the same single-character
//!   segmentation. The tests pin the segmentation, the phrase construction and
//!   the un-segmented snippet, including the one accepted failure mode (a
//!   two-character query straddling a word boundary still matches).
//! - **A hit is a usable next step.** Links resolve to real pages, dead links
//!   disappear, source references are split into raw path plus chunk with
//!   `raw_available` stat-ed for real, links and sources filled from disk,
//!   and `--expand` inlines exactly one hop.
//! - **The index is a cache.** `reindex` rebuilds from `wiki/`, `refresh` only
//!   touches pages whose digest changed, and `search` itself is read-only — the
//!   CLI layers the refresh on top, which is what lets the tests tell the two
//!   apart.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use llm_wiki::config::Config;
use llm_wiki::init;
use llm_wiki::query::{self, ExpandedRef, Hit, Scope, SearchQuery};
use llm_wiki::store::Store;
use tempfile::{tempdir, TempDir};

/// An initialised repository with a migrated ledger.
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

    /// Writes a page with the usual frontmatter; `rel` is relative to `wiki/`.
    fn page(&self, rel: &str, id: &str, title: &str, summary: &str, body: &str) {
        let meta = format!(
            "id: {id}\ntitle: {title}\ntype: concept\nsummary: {summary}\n\
             status: stable\nupdated: 2026-10-27"
        );
        self.page_with(rel, &meta, body);
    }

    /// Writes a page with hand-written frontmatter (for `sources:` and session
    /// ids).
    fn page_with(&self, rel: &str, meta: &str, body: &str) {
        let path = self.path("wiki", rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, format!("---\n{meta}\n---\n\n{body}\n")).unwrap();
    }

    /// Writes a raw markdown file with its own frontmatter, as `convert` does.
    fn raw(&self, rel: &str, source: &str) {
        let path = self.path("raw", rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!("---\nsource: {source}\nsource_hash: blake3:00\nparser: text\n---\n\n原文\n"),
        )
        .unwrap();
    }

    fn path(&self, dir: &str, rel: &str) -> PathBuf {
        let base = match dir {
            "wiki" => ".wiki/pages",
            "raw" => ".wiki/raw",
            other => other,
        };
        self.root().join(base).join(rel)
    }

    fn reindex(&mut self) -> usize {
        let root = self.dir.path().to_path_buf();
        query::reindex(&root, &self.cfg, &mut self.store).unwrap()
    }

    fn refresh(&mut self) -> usize {
        let root = self.dir.path().to_path_buf();
        query::refresh(&root, &self.cfg, &mut self.store).unwrap()
    }

    fn search(&self, text: &str, limit: usize, expand: usize, scope: Scope) -> Vec<Hit> {
        let q = SearchQuery {
            text: String::from(text),
            limit,
            expand,
            scope,
        };
        query::search(self.root(), &self.cfg, &self.store, &q).unwrap()
    }

    fn hits(&self, text: &str) -> Vec<Hit> {
        self.search(text, 10, 0, Scope::All)
    }

    /// The single hit a query must return.
    fn only(&self, text: &str) -> Hit {
        let hits = self.hits(text);
        assert_eq!(hits.len(), 1, "expected one hit for {text:?}: {hits:?}");
        hits.into_iter().next().unwrap()
    }
}

/// Runs `llm-wiki --json <args...>` inside `dir`.
fn run(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("llm-wiki")
        .unwrap()
        .arg("--json")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn stdout_json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not one JSON document ({err}): {:?} / stderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

// ---------------------------------------------------------------------------
// Segmentation: both sides go through the same function.
// ---------------------------------------------------------------------------

#[test]
fn cjk_characters_are_split_one_by_one() {
    assert_eq!(query::segment("订单超时"), "订 单 超 时");
    // ASCII words keep their letters together; only the boundary is spaced.
    assert_eq!(query::segment("PDF解析ok"), "PDF 解 析 ok");
    assert_eq!(query::segment("timeout 30s"), "timeout 30s");
    // Display text is put back together.
    assert_eq!(query::unsegment("订 单 超 时"), "订单超时");
    // A space between two CJK characters is indistinguishable from the one
    // segmentation inserted, so it disappears too. That is invisible in
    // practice (Chinese does not space characters inside a word) and it is
    // pinned here so nobody "fixes" it with a second separator by accident.
    assert_eq!(
        query::unsegment(&query::segment("订单超时 后 30s")),
        "订单超时后 30s"
    );
    // A space that touches ASCII text survives, which is the case a snippet
    // actually shows.
    assert_eq!(
        query::unsegment(&query::segment("超时 30s 后重试")),
        "超时 30s 后重试"
    );
}

#[test]
fn chinese_words_hit_through_single_char_indexing() {
    let mut fx = Fixture::new();
    fx.page(
        "concept/order.md",
        "order",
        "订单超时",
        "未支付超时",
        "订单超时后自动重试三次。",
    );
    assert_eq!(fx.reindex(), 1);

    let hit = fx.only("订单");
    assert_eq!(hit.page, ".wiki/pages/concept/order.md");
    assert_eq!(hit.title, "订单超时");
    assert_eq!(hit.summary, "未支付超时");
}

#[test]
fn a_two_char_phrase_matches_across_a_word_boundary() {
    // Single-character indexing cannot know words, so 「单处」 matches inside
    // 「订单处理」. That is the accepted price of the scheme; pinning it means a
    // later change to the segmentation has to update this test on purpose.
    let mut fx = Fixture::new();
    fx.page(
        "concept/flow.md",
        "flow",
        "处理流程",
        "流程",
        "订单处理流程说明。",
    );
    fx.reindex();

    assert_eq!(fx.hits("单处").len(), 1);
    // Position still matters: the reversed pair is not adjacent in the text.
    assert!(fx.hits("单订").is_empty());
}

#[test]
fn ascii_words_and_numbers_hit() {
    let mut fx = Fixture::new();
    fx.page(
        "concept/timeout.md",
        "timeout",
        "超时设置",
        "超时",
        "The request timeout is 30 seconds.",
    );
    fx.reindex();

    assert_eq!(fx.only("timeout").page, ".wiki/pages/concept/timeout.md");
    assert_eq!(fx.only("30").page, ".wiki/pages/concept/timeout.md");
}

#[test]
fn snippets_are_un_segmented_for_reading() {
    let mut fx = Fixture::new();
    fx.page(
        "concept/order.md",
        "order",
        "订单超时",
        "未支付超时",
        "订单超时后自动重试三次，仍然失败则转人工。",
    );
    fx.reindex();

    let hit = fx.only("订单");
    assert!(hit.snippet.contains("订单"), "snippet: {}", hit.snippet);
    for broken in ["订 单", "单 超", "超 时"] {
        assert!(
            !hit.snippet.contains(broken),
            "snippet keeps the index spacing {broken:?}: {}",
            hit.snippet
        );
    }
}

// ---------------------------------------------------------------------------
// Ranking, limit, scope.
// ---------------------------------------------------------------------------

#[test]
fn hits_are_ranked_by_score_and_carry_status() {
    let mut fx = Fixture::new();
    fx.page(
        "concept/a.md",
        "a",
        "甲",
        "甲摘要",
        "订单 订单 订单 订单 订单 订单",
    );
    fx.page("concept/b.md", "b", "乙", "乙摘要", "订单只出现一次。");
    fx.reindex();

    let hits = fx.hits("订单");
    assert_eq!(hits.len(), 2);
    assert_eq!(hits[0].page, ".wiki/pages/concept/a.md");
    assert!(
        hits[0].score > hits[1].score,
        "the page repeating the term must rank first: {:?}",
        hits.iter().map(|h| h.score).collect::<Vec<_>>()
    );
    assert!(hits[0].score.is_finite() && hits[0].score > 0.0);
    assert_eq!(hits[0].status, "stable");
}

#[test]
fn limit_caps_the_number_of_hits() {
    let mut fx = Fixture::new();
    for id in ["a", "b", "c"] {
        fx.page(
            &format!("concept/{id}.md"),
            id,
            &format!("页{id}"),
            "同步",
            "同步流程说明。",
        );
    }
    fx.reindex();

    assert_eq!(fx.search("同步", 2, 0, Scope::All).len(), 2);
    assert_eq!(fx.search("同步", 10, 0, Scope::All).len(), 3);
}

#[test]
fn scope_hides_the_session_distilled_pages() {
    let mut fx = Fixture::new();
    fx.page("concept/live.md", "live", "现行页", "现行", "同步流程。");
    // A distilled page keeps the source prefix in its id, so it lands one level
    // below the type directory. This is the shape the writing tools produce.
    fx.page_with(
        "concept/sessions/2026-10/a.md",
        "id: sessions/2026-10/a\ntitle: 会话摘要\ntype: source\nsummary: 会话\n\
         status: stable\nupdated: 2026-10-01",
        "同步的历史结论。",
    );
    fx.reindex();

    let hidden = fx.search("同步", 10, 0, Scope::ExcludingSessions);
    assert_eq!(hidden.len(), 1);
    assert_eq!(hidden[0].page, ".wiki/pages/concept/live.md");

    assert_eq!(fx.search("同步", 10, 0, Scope::All).len(), 2);
}

#[test]
fn scope_hides_session_pages_written_straight_under_the_directory() {
    let mut fx = Fixture::new();
    fx.page("concept/live.md", "live", "现行页", "现行", "同步流程。");
    fx.page_with(
        "sessions/2026-10/b.md",
        "id: sess-b\ntitle: 另一份会话\ntype: source\nsummary: 会话\n\
         status: stable\nupdated: 2026-10-01",
        "同步的历史结论。",
    );
    fx.reindex();

    let hidden = fx.search("同步", 10, 0, Scope::ExcludingSessions);
    assert_eq!(hidden.len(), 1, "the flat layout must be excluded too");
    assert_eq!(fx.search("同步", 10, 0, Scope::All).len(), 2);
}

// ---------------------------------------------------------------------------
// The three kinds of next step: links, sources, backlinks.
// ---------------------------------------------------------------------------

#[test]
fn links_resolve_to_page_paths_and_titles() {
    let mut fx = Fixture::new();
    fx.page("concept/a.md", "a", "甲页", "甲", "订单见 [[b]]。");
    fx.page("concept/b.md", "b", "乙页", "乙摘要", "重试策略。");
    fx.reindex();

    let hit = fx.only("订单");
    assert_eq!(hit.outgoing.total, 1);
    assert_eq!(hit.outgoing.items[0].page, ".wiki/pages/concept/b.md");
    assert_eq!(hit.outgoing.items[0].title, "乙页");
}

#[test]
fn dead_links_are_dropped_not_returned() {
    let mut fx = Fixture::new();
    fx.page("concept/a.md", "a", "甲页", "甲", "订单见 [[不存在的页]]。");
    fx.reindex();

    let hit = fx.only("订单");
    assert!(hit.outgoing.items.is_empty());
    assert_eq!(hit.outgoing.unresolved[0].literal, "不存在的页");
    assert_eq!(hit.outgoing.unresolved[0].resolution, "missing");
}

#[test]
fn sources_split_the_chunk_and_check_the_raw_file() {
    let mut fx = Fixture::new();
    fx.page_with(
        "concept/sourced.md",
        "id: sourced\ntitle: 有源页\ntype: concept\nsummary: 源\n\
         status: stable\nupdated: 2026-10-27\nsources:\n  - .wiki/raw/a.md#p=12-40",
        "订单系统设计。",
    );
    fx.raw("a.md", "a.pdf");
    fx.reindex();

    let hit = fx.only("订单");
    assert_eq!(hit.sources.total, 1);
    let source = &hit.sources.items[0];
    assert_eq!(source.raw, ".wiki/raw/a.md#p=12-40");
    assert_eq!(source.raw_key, "a.md");
    assert_eq!(source.source.as_deref(), Some("a.pdf"));
    assert_eq!(source.fragment.as_deref(), Some("p=12-40"));
    assert!(source.raw_available);
}

#[test]
fn a_missing_raw_is_reported_not_fatal() {
    // The raw can be purged while the page keeps citing it, so a missing file
    // is a fact worth reporting, never an error.
    let mut fx = Fixture::new();
    fx.page_with(
        "concept/sourced.md",
        "id: sourced\ntitle: 有源页\ntype: concept\nsummary: 源\n\
         status: stable\nupdated: 2026-10-27\nsources:\n  - .wiki/raw/missing.md#p=1-2",
        "订单系统设计。",
    );
    fx.reindex();

    let hit = fx.only("订单");
    assert_eq!(hit.sources.total, 1);
    assert_eq!(hit.sources.items[0].raw_key, "missing.md");
    assert_eq!(hit.sources.items[0].fragment.as_deref(), Some("p=1-2"));
    assert!(!hit.sources.items[0].raw_available);
    assert_eq!(hit.sources.items[0].source, None);
}

#[test]
fn backlinks_count_the_pages_that_point_here() {
    let mut fx = Fixture::new();
    for (id, title, body) in [
        ("a", "甲", "同步见 [[hub]]。"),
        ("b", "乙", "重试见 [[hub]]。"),
        ("hub", "枢纽", "订单总览。"),
    ] {
        fx.page(&format!("concept/{id}.md"), id, title, "摘要", body);
    }
    fx.reindex();

    let hit = fx.only("订单");
    assert_eq!(hit.incoming.total, 2);
    assert_eq!(hit.incoming.items[0].title, "甲");
    assert_eq!(hit.incoming.items[1].title, "乙");
    // A page nobody links to has zero.
    assert_eq!(fx.only("同步").incoming.total, 0);
}

#[test]
fn expand_inlines_exactly_one_hop() {
    let mut fx = Fixture::new();
    fx.page("concept/a.md", "a", "甲", "甲摘要", "订单见 [[b]]。");
    fx.page("concept/b.md", "b", "乙页", "乙摘要", "见 [[c]]。");
    fx.page("concept/c.md", "c", "丙页", "丙摘要", "正文。");
    fx.reindex();

    assert!(fx.search("订单", 10, 0, Scope::All)[0].expanded.is_empty());

    let expanded = &fx.search("订单", 10, 1, Scope::All)[0].expanded;
    assert_eq!(
        expanded,
        &vec![ExpandedRef {
            page: String::from(".wiki/pages/concept/b.md"),
            title: String::from("乙页"),
            summary: String::from("乙摘要"),
        }],
        "only direct neighbours, never their neighbours"
    );
}

// ---------------------------------------------------------------------------
// Index maintenance: full rebuild versus incremental refresh.
// ---------------------------------------------------------------------------

#[test]
fn reindex_is_what_makes_a_new_page_searchable() {
    let mut fx = Fixture::new();
    fx.page("concept/old.md", "old", "旧页", "旧", "同步流程。");
    assert_eq!(fx.reindex(), 1);
    assert_eq!(fx.hits("同步").len(), 1);

    fx.page("concept/new.md", "new", "新页", "新", "检索新增内容。");
    // `search` is a read of the index, so it cannot see the new file yet.
    assert!(fx.hits("检索").is_empty());

    assert_eq!(fx.reindex(), 2);
    assert_eq!(fx.only("检索").page, ".wiki/pages/concept/new.md");
}

#[test]
fn refresh_touches_only_the_pages_whose_hash_changed() {
    let mut fx = Fixture::new();
    fx.page("concept/old.md", "old", "旧页", "旧", "同步流程。");
    fx.reindex();

    // Nothing changed: the digests match and no page is read for indexing.
    assert_eq!(fx.refresh(), 0);

    fx.page("concept/new.md", "new", "新页", "新", "检索新增内容。");
    assert_eq!(fx.refresh(), 1);
    assert_eq!(fx.hits("检索").len(), 1);
    assert_eq!(fx.refresh(), 0);

    // A deleted page leaves the index too.
    fs::remove_file(fx.path("wiki", "concept/old.md")).unwrap();
    assert_eq!(fx.refresh(), 1);
    assert!(fx.hits("同步").is_empty());
}

#[test]
fn refresh_removes_stale_hits_when_an_indexed_page_becomes_malformed() {
    let mut fx = Fixture::new();
    fx.page("concept/broken.md", "broken", "旧页", "旧摘要", "旧正文。");
    fx.page(
        "concept/healthy.md",
        "healthy",
        "正常页",
        "摘要",
        "正常正文。",
    );
    assert_eq!(fx.reindex(), 2);
    assert_eq!(fx.hits("旧正文").len(), 1);

    fx.page_with("concept/broken.md", "title: 坏页", "损坏后的正文。");
    fx.refresh();
    assert!(fx.hits("旧正文").is_empty(), "坏页不能继续命中旧正文");
    assert_eq!(fx.hits("正常正文").len(), 1);
    assert!(!fx
        .store
        .fts_page_ids()
        .unwrap()
        .contains(&String::from(".wiki/pages/concept/broken.md")));
    assert!(!fx
        .store
        .all_page_metas()
        .unwrap()
        .iter()
        .any(|row| { row.page_key == ".wiki/pages/concept/broken.md" }));
    assert_eq!(fx.refresh(), 0, "持续损坏的页不应重复写入索引");

    fx.page(
        "concept/broken.md",
        "broken",
        "修复页",
        "新摘要",
        "修复正文。",
    );
    assert_eq!(fx.refresh(), 1);
    assert_eq!(fx.only("修复正文").title, "修复页");
    assert!(fx.hits("旧正文").is_empty());
    assert_eq!(fx.refresh(), 0);
}

#[test]
fn refresh_removes_a_malformed_fts_page_even_without_page_meta() {
    let mut fx = Fixture::new();
    fx.page("concept/broken.md", "broken", "旧页", "摘要", "旧正文。");
    fx.reindex();
    fx.store
        .delete_page_meta(".wiki/pages/concept/broken.md")
        .unwrap();
    fs::write(fx.path("wiki", "concept/broken.md"), [0xff, 0xfe]).unwrap();

    fx.refresh();
    assert!(fx.hits("旧正文").is_empty(), "孤立的旧 FTS 正文也必须清除");
    assert_eq!(fx.refresh(), 0);
}

// ---------------------------------------------------------------------------
// The CLI wiring.
// ---------------------------------------------------------------------------

#[test]
fn empty_query_is_exit_2_with_a_code() {
    let fx = Fixture::new();

    let out = run(fx.root(), &["query", ""]);
    assert_eq!(out.status.code(), Some(2));
    let v = stdout_json(&out);
    assert_eq!(v["ok"], false);
    assert_eq!(v["error"]["code"], "E_EMPTY_QUERY");

    // No text at all is the same mistake, unless `--reindex` asks for a rebuild.
    let out = run(fx.root(), &["query"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(stdout_json(&out)["error"]["code"], "E_EMPTY_QUERY");
}

#[test]
fn a_plain_query_catches_up_on_the_pages_written_since_the_last_run() {
    let fx = Fixture::new();
    fx.page(
        "concept/order.md",
        "order",
        "订单超时",
        "未支付超时",
        "订单超过三十分钟未支付时关闭。",
    );

    // No `--reindex`: the CLI refreshes incrementally before searching, so a
    // first query on a fresh clone still finds the page.
    let out = run(fx.root(), &["query", "订单"]);
    assert_eq!(out.status.code(), Some(0));
    let v = stdout_json(&out);
    assert_eq!(v["ok"], true);
    assert_eq!(v["data"]["count"], 1);
    assert_eq!(v["data"]["hits"][0]["page"], ".wiki/pages/concept/order.md");
    assert_eq!(v["data"]["hits"][0]["title"], "订单超时");
}

#[test]
fn the_human_readable_form_has_one_block_per_hit() {
    let fx = Fixture::new();
    fx.page_with(
        "concept/order.md",
        "id: order\ntitle: 订单超时\ntype: concept\nsummary: 未支付超时\n\
         status: stable\nupdated: 2026-10-27\nsources:\n  - .wiki/raw/a.md",
        "订单超过三十分钟未支付时关闭。",
    );
    fx.raw("a.md", "a.pdf");

    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["query", "订单"])
        .current_dir(fx.root())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("1. 订单超时 [stable] .wiki/pages/concept/order.md"),
        "stdout: {text}"
    );
    assert!(
        text.contains("订单超过三十分钟未支付时关闭"),
        "stdout: {text}"
    );
    assert!(text.contains("→ out:"), "stdout: {text}");
    assert!(
        text.contains("sources: .wiki/raw/a.md (ok)"),
        "stdout: {text}"
    );
    assert!(text.contains("in: 0"), "stdout: {text}");
}

// ---------------------------------------------------------------------------
// Relation index
// ---------------------------------------------------------------------------

#[test]
fn refresh_keeps_links_both_ways_and_survives_page_deletion() {
    let mut f = Fixture::new();
    let a = ".wiki/pages/concept/a.md";
    let b = ".wiki/pages/concept/b.md";
    f.page("concept/a.md", "a", "A", "", "见 [[b]] 与 [[gone]]");
    f.page("concept/b.md", "b", "B", "", "");
    f.refresh();

    let from = f.store.relation_links_from(a).unwrap();
    assert_eq!(from.len(), 2);
    assert_eq!(from[0].target_literal, "b");
    assert_eq!(from[0].to_page.as_deref(), Some(b));
    assert_eq!(from[1].resolution, "missing");
    assert_eq!(f.store.relation_links_to(b).unwrap().len(), 1);

    std::fs::remove_file(f.path("wiki", "concept/b.md")).unwrap();
    f.refresh();
    assert!(f.store.relation_links_to(b).unwrap().is_empty());
    let from = f.store.relation_links_from(a).unwrap();
    assert_eq!(from[0].to_page, None);
    assert_eq!(from[0].resolution, "missing");
}

#[test]
fn a_damaged_page_does_not_clear_other_pages() {
    let mut f = Fixture::new();
    let a = ".wiki/pages/concept/a.md";
    f.page("concept/a.md", "a", "A", "", "见 [[b]]");
    f.page("concept/b.md", "b", "B", "", "");
    f.refresh();
    let before = f.store.relation_generation().unwrap();

    std::fs::write(f.path("wiki", "concept/b.md"), "not a page").unwrap();
    f.refresh();

    let from = f.store.relation_links_from(a).unwrap();
    assert_eq!(from.len(), 1);
    assert_eq!(from[0].target_literal, "b");
    assert_eq!(from[0].resolution, "missing");
    assert!(f
        .store
        .all_relation_nodes()
        .unwrap()
        .iter()
        .all(|n| n.page_id != "b"));
    assert!(f.store.relation_generation().unwrap() > before);
}

#[test]
fn source_refs_are_indexed_and_reverse_lookup_works() {
    let mut f = Fixture::new();
    f.raw("notes.md", "notes.pdf");
    f.page_with(
        "concept/a.md",
        "id: a\ntitle: A\ntype: concept\nsummary: x\nstatus: stable\nupdated: 2026-10-27\n\
         sources:\n  - .wiki/raw/notes.md#p=1",
        "",
    );
    f.refresh();
    let a = ".wiki/pages/concept/a.md";
    let sources = f.store.relation_sources(a).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].raw_key, "notes.md");
    assert_eq!(sources[0].fragment.as_deref(), Some("p=1"));
    assert_eq!(
        f.store.pages_citing("notes.md").unwrap(),
        vec![String::from(a)]
    );
}
