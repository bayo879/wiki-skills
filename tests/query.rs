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
//!   `raw_available` stat-ed for real, archived pages carry their `recall` line,
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
use llm_wiki::query::{self, ExpandedRef, Hit, LinkRef, Scope, SearchQuery, SourceRef};
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

    /// Writes a page with hand-written frontmatter (for `sources:` and archive
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
        self.root().join(dir).join(rel)
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
    assert_eq!(hit.page, "wiki/concept/order.md");
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

    assert_eq!(fx.only("timeout").page, "wiki/concept/timeout.md");
    assert_eq!(fx.only("30").page, "wiki/concept/timeout.md");
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
    assert_eq!(hits[0].page, "wiki/concept/a.md");
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
fn scope_separates_the_wiki_from_the_archive() {
    let mut fx = Fixture::new();
    fx.page("concept/live.md", "live", "现行页", "现行", "同步流程。");
    fx.page_with(
        "archive/2026-01/old.md",
        "id: old\ntitle: 归档摘要\ntype: archive\nsummary: 归档\n\
         status: stable\nupdated: 2026-01-31",
        "同步的历史结论。",
    );
    fx.reindex();

    let wiki = fx.search("同步", 10, 0, Scope::Wiki);
    assert_eq!(wiki.len(), 1);
    assert_eq!(wiki[0].page, "wiki/concept/live.md");

    let archive = fx.search("同步", 10, 0, Scope::Archive);
    assert_eq!(archive.len(), 1);
    assert_eq!(archive[0].page, "wiki/archive/2026-01/old.md");

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
    assert_eq!(
        hit.links,
        vec![LinkRef {
            to: String::from("wiki/concept/b.md"),
            title: String::from("乙页"),
        }]
    );
}

#[test]
fn dead_links_are_dropped_not_returned() {
    let mut fx = Fixture::new();
    fx.page("concept/a.md", "a", "甲页", "甲", "订单见 [[不存在的页]]。");
    fx.reindex();

    assert!(fx.only("订单").links.is_empty());
}

#[test]
fn sources_split_the_chunk_and_check_the_raw_file() {
    let mut fx = Fixture::new();
    fx.page_with(
        "concept/sourced.md",
        "id: sourced\ntitle: 有源页\ntype: concept\nsummary: 源\n\
         status: stable\nupdated: 2026-10-27\nsources:\n  - raw/a.md#p=12-40",
        "订单系统设计。",
    );
    fx.raw("a.md", "source/a.pdf");
    fx.reindex();

    let hit = fx.only("订单");
    assert_eq!(
        hit.sources,
        vec![SourceRef {
            raw: String::from("raw/a.md"),
            source: Some(String::from("source/a.pdf")),
            chunk: Some(String::from("p=12-40")),
            raw_available: true,
        }]
    );
}

#[test]
fn a_missing_raw_is_reported_not_fatal() {
    // `raw/` is in git and `source/` is not, so a clone has the raw and misses
    // the source; the opposite also happens mid-conversion. Neither is an error.
    let mut fx = Fixture::new();
    fx.page_with(
        "concept/sourced.md",
        "id: sourced\ntitle: 有源页\ntype: concept\nsummary: 源\n\
         status: stable\nupdated: 2026-10-27\nsources:\n  - raw/missing.md#p=1-2",
        "订单系统设计。",
    );
    fx.reindex();

    let hit = fx.only("订单");
    assert_eq!(hit.sources.len(), 1);
    assert_eq!(hit.sources[0].raw, "raw/missing.md");
    assert_eq!(hit.sources[0].chunk.as_deref(), Some("p=1-2"));
    assert!(!hit.sources[0].raw_available);
    assert_eq!(hit.sources[0].source, None);
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

    assert_eq!(fx.only("订单").backlinks, 2);
    // A page nobody links to has zero.
    assert_eq!(fx.only("同步").backlinks, 0);
}

#[test]
fn archived_hits_carry_the_recall_command() {
    let mut fx = Fixture::new();
    fx.page_with(
        "archive/2026-01/vip-retry.md",
        "id: vip-retry\ntitle: VIP 重试策略\ntype: archive\nsummary: 归档\n\
         status: stable\nupdated: 2026-01-31",
        "订单重试最终结论：三次封顶。",
    );
    fx.reindex();

    let hit = fx.only("订单");
    assert!(hit.archived);
    assert_eq!(hit.recall.as_deref(), Some("llm-wiki recall vip-retry"));
    // A live page never claims to be archived.
    fx.page("concept/live.md", "live", "现行页", "现行", "订单同步。");
    fx.reindex();
    assert!(!fx.only("同步").archived);
    assert_eq!(fx.only("同步").recall, None);
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
            page: String::from("wiki/concept/b.md"),
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
    assert_eq!(fx.only("检索").page, "wiki/concept/new.md");
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
fn reindex_without_text_reports_the_page_count() {
    let fx = Fixture::new();
    fx.page("concept/a.md", "a", "甲", "甲", "订单。");
    fx.page("concept/b.md", "b", "乙", "乙", "同步。");

    let out = run(fx.root(), &["query", "--reindex"]);
    assert_eq!(out.status.code(), Some(0));
    let v = stdout_json(&out);
    assert_eq!(v["ok"], true);
    assert_eq!(v["cmd"], "query");
    assert_eq!(v["data"]["indexed"], 2);
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
    assert_eq!(v["data"]["hits"][0]["page"], "wiki/concept/order.md");
    assert_eq!(v["data"]["hits"][0]["title"], "订单超时");
}

#[test]
fn the_human_readable_form_has_one_block_per_hit() {
    let fx = Fixture::new();
    fx.page_with(
        "concept/order.md",
        "id: order\ntitle: 订单超时\ntype: concept\nsummary: 未支付超时\n\
         status: stable\nupdated: 2026-10-27\nsources:\n  - raw/a.md",
        "订单超过三十分钟未支付时关闭。",
    );
    fx.raw("a.md", "source/a.pdf");

    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["query", "订单"])
        .current_dir(fx.root())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("1. 订单超时 [stable] wiki/concept/order.md"),
        "stdout: {text}"
    );
    assert!(
        text.contains("订单超过三十分钟未支付时关闭"),
        "stdout: {text}"
    );
    assert!(text.contains("→ links:"), "stdout: {text}");
    assert!(text.contains("sources: raw/a.md (ok)"), "stdout: {text}");
    assert!(text.contains("backlinks 0"), "stdout: {text}");
}
