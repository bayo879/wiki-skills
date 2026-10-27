//! `lint` and `lint --fix`: one test per rule, plus the ordering, the exit-code
//! contract and the CLI wrapper it feeds.
//!
//! The fixtures write real files under a temporary repository and seed the
//! ledger directly, because `lint` is defined by what it finds on disk and in
//! the database, not by what a command happened to leave behind.

use std::fs;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use chrono::Local;
use llm_wiki::config::Config;
use llm_wiki::ingest::{self, BeginInput};
use llm_wiki::lint::{self, Finding, Level};
use llm_wiki::store::{PageMetaRow, RawRow, Store};
use llm_wiki::{hash, init, page, render, staging};
use tempfile::{tempdir, TempDir};

/// Body long enough for the lowered `min_page_chars` every fixture uses.
const BODY: &str = "这一页的正文足够长，写清楚前因后果，测试才会认为它达标。";

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
        let mut cfg = Config::load(dir.path()).unwrap();
        cfg.audit.min_page_chars = 10;
        let mut store = Store::open(&dir.path().join(".wiki/state.db")).unwrap();
        store.migrate().unwrap();
        Fixture { dir, cfg, store }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn db(&mut self) -> &mut Store {
        &mut self.store
    }

    /// Writes a page at `wiki/<typ>/<id>.md` and returns its path.
    fn put_page(&self, typ: &str, id: &str, text: &str) -> PathBuf {
        let path = self.root().join("wiki").join(typ).join(format!("{id}.md"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }

    /// Writes a raw file at `raw/<key>` and returns its path.
    fn put_raw(&self, key: &str, body: &str) -> PathBuf {
        let path = self.root().join("raw").join(key);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, body).unwrap();
        path
    }

    fn lint(&self) -> Vec<Finding> {
        lint::lint(self.root(), &self.cfg, &self.store).unwrap()
    }

    fn fix(&mut self) -> lint::FixReport {
        let root = self.root().to_path_buf();
        let cfg = self.cfg.clone();
        lint::fix(&root, &cfg, &mut self.store).unwrap()
    }
}

/// A page with every required field and `body` as its body.
fn page_text(id: &str, typ: &str, body: &str) -> String {
    format!(
        "---\nid: {id}\ntitle: {id}\ntype: {typ}\nsummary: 摘要\nstatus: stable\nupdated: 2026-10-20\n---\n\n{body}\n"
    )
}

fn rules(findings: &[Finding]) -> Vec<&'static str> {
    findings.iter().map(|f| f.rule).collect()
}

fn only<'a>(findings: &'a [Finding], rule: &str) -> Vec<&'a Finding> {
    findings.iter().filter(|f| f.rule == rule).collect()
}

// ---------------------------------------------------------------- L1, L2, L3

#[test]
fn empty_wiki_has_no_findings() {
    let f = Fixture::new();
    assert!(f.lint().is_empty());
}

#[test]
fn l1_reports_each_missing_required_field() {
    let f = Fixture::new();
    // No id, no title, no status.
    f.put_page(
        "concept",
        "a",
        &format!("---\ntype: concept\nsummary: 摘要\nupdated: 2026-10-20\n---\n\n{BODY}\n"),
    );

    let findings = f.lint();
    let l1 = only(&findings, "L1");
    assert_eq!(l1.len(), 3, "{findings:?}");
    assert!(l1.iter().all(|f| f.level == Level::Error));
    assert!(l1.iter().any(|f| f.msg.contains("id")));
    assert!(l1.iter().any(|f| f.msg.contains("title")));
    assert!(l1.iter().any(|f| f.msg.contains("status")));
    assert_eq!(
        l1[0].page.as_deref(),
        Some("wiki/concept/a.md"),
        "every finding names its page"
    );
}

#[test]
fn l2_reports_dead_link_wikilinks_only() {
    let f = Fixture::new();
    f.put_page("concept", "a", &page_text("a", "concept", BODY));
    f.put_page("concept", "b", &page_text("b", "concept", BODY));
    let body = format!("{BODY}\n\n见 [[b]]、[[concept/b]] 与 [[ghost]]。");
    f.put_page("concept", "c", &page_text("c", "concept", &body));

    let findings = f.lint();
    let l2 = only(&findings, "L2");
    assert_eq!(l2.len(), 1, "{findings:?}");
    assert!(l2[0].msg.contains("ghost"));
    assert_eq!(l2[0].level, Level::Error);
}

#[test]
fn l2_ignores_links_inside_code_fences_and_markdown_links() {
    let f = Fixture::new();
    let body = format!("{BODY}\n\n```\n[[ghost]]\n```\n\n[doc](other.md) 见 [[a]]。");
    f.put_page("concept", "a", &page_text("a", "concept", &body));

    let findings = f.lint();
    let l2 = only(&findings, "L2");
    assert!(l2.is_empty(), "{l2:?}");
}

#[test]
fn l3_reports_a_duplicate_id_in_two_type_directories() {
    let f = Fixture::new();
    f.put_page("concept", "retry", &page_text("retry", "concept", BODY));
    f.put_page("entity", "retry", &page_text("retry", "entity", BODY));

    let findings = f.lint();
    let l3 = only(&findings, "L3");
    assert_eq!(l3.len(), 2, "one finding per page");
    assert!(l3.iter().all(|f| f.level == Level::Error));
    let pages: Vec<&str> = l3.iter().filter_map(|f| f.page.as_deref()).collect();
    assert!(pages.contains(&"wiki/concept/retry.md"));
    assert!(pages.contains(&"wiki/entity/retry.md"));
}

// ------------------------------------------------------------------ L4, L5

#[test]
fn l4_reports_a_non_canonical_status() {
    let f = Fixture::new();
    let text = page_text("a", "concept", BODY).replace("status: stable", "status: broken");
    f.put_page("concept", "a", &text);

    let findings = f.lint();
    let l4 = only(&findings, "L4");
    assert_eq!(l4.len(), 1, "{findings:?}");
    assert_eq!(l4[0].level, Level::Error);
    assert!(l4[0].msg.contains("broken"));
    assert!(
        only(&findings, "L1").is_empty(),
        "an invalid status is L4, not a missing field"
    );
}

#[test]
fn l5_reports_a_missing_updated_field() {
    let f = Fixture::new();
    let text = page_text("a", "concept", BODY).replace("updated: 2026-10-20\n", "");
    f.put_page("concept", "a", &text);

    let findings = f.lint();
    assert_eq!(rules(&findings), vec!["L5"], "{findings:?}");
    assert_eq!(findings[0].level, Level::Warning);
    assert!(findings[0].msg.contains("updated"));
}

#[test]
fn l5_reports_an_updated_that_is_not_a_date() {
    let f = Fixture::new();
    let text =
        page_text("a", "concept", BODY).replace("updated: 2026-10-20", "updated: 2026/10/20");
    f.put_page("concept", "a", &text);

    let findings = f.lint();
    assert_eq!(rules(&findings), vec!["L5"], "{findings:?}");
    assert_eq!(findings[0].page.as_deref(), Some("wiki/concept/a.md"));
}

// ------------------------------------------------------------------ L6, L7

#[test]
fn l6_reports_sources_that_do_not_exist() {
    let mut f = Fixture::new();
    f.put_raw("here.md", "存在。\n");
    f.db()
        .upsert_raw(&RawRow {
            key: String::from("here.md"),
            hash: String::from("blake3:x"),
            source_key: String::from("here.pdf"),
            converted_at: String::from("2026-10-27T00:00:00Z"),
        })
        .unwrap();

    let text = page_text("a", "concept", BODY).replace(
        "status: stable",
        "status: stable\nsources: [raw/missing.md]",
    );
    f.put_page("concept", "a", &text);
    let text = page_text("b", "concept", BODY)
        .replace("status: stable", "status: stable\nsources: [raw/here.md]");
    f.put_page("concept", "b", &text);

    let findings = f.lint();
    let l6 = only(&findings, "L6");
    assert_eq!(l6.len(), 1, "{l6:?}");
    assert!(l6[0].msg.contains("raw/missing.md"));
    assert_eq!(l6[0].level, Level::Error);
}

#[test]
fn l7_reports_a_raw_file_that_is_not_in_the_ledger() {
    let f = Fixture::new();
    f.put_raw("orphan.md", "没有登记。\n");

    let findings = f.lint();
    let l7 = only(&findings, "L7");
    assert_eq!(l7.len(), 1, "{findings:?}");
    assert!(l7[0].msg.contains("orphan.md"));
    assert_eq!(l7[0].level, Level::Warning);
}

// ------------------------------------------------------------------ L8, L9

#[test]
fn l8_counts_characters_not_bytes() {
    let f = Fixture::new();
    // Nine characters, twenty-seven bytes: a byte count would call it long.
    f.put_page(
        "concept",
        "short",
        &page_text("short", "concept", "订单超时后重试三次"),
    );

    let findings = f.lint();
    let l8 = only(&findings, "L8");
    assert_eq!(l8.len(), 1, "{findings:?}");
    assert_eq!(l8[0].level, Level::Warning);
    assert_eq!(l8[0].page.as_deref(), Some("wiki/concept/short.md"));
}

#[test]
fn l8_leaves_a_long_enough_page_alone() {
    let f = Fixture::new();
    f.put_page("concept", "a", &page_text("a", "concept", BODY));
    let findings = f.lint();
    assert!(only(&findings, "L8").is_empty());
}

#[test]
fn l9_is_silent_when_the_ledger_has_no_record() {
    let f = Fixture::new();
    f.put_page("concept", "a", &page_text("a", "concept", BODY));
    // No page_meta row: a fresh clone has none because state.db is not in git.
    assert!(only(&f.lint(), "L9").is_empty());
}

#[test]
fn l9_reports_a_body_edit_that_left_updated_behind() {
    let mut f = Fixture::new();
    let path = f.put_page("concept", "a", &page_text("a", "concept", BODY));
    let text = fs::read_to_string(&path).unwrap();
    f.db()
        .upsert_page_meta(&PageMetaRow {
            page_key: String::from("wiki/concept/a.md"),
            hash: hash::hash_bytes(text.as_bytes()),
            updated: String::from("2026-10-20"),
        })
        .unwrap();

    let findings = f.lint();
    assert!(only(&findings, "L9").is_empty(), "record matches the file");

    fs::write(&path, format!("{text}\n补充一段。\n")).unwrap();
    let findings = f.lint();
    let l9 = only(&findings, "L9");
    assert_eq!(l9.len(), 1, "an edited body with an old date is L9");
    assert_eq!(l9[0].level, Level::Warning);
    assert_eq!(l9[0].page.as_deref(), Some("wiki/concept/a.md"));
}

#[test]
fn l9_exempts_asset_pages() {
    let mut f = Fixture::new();
    let text =
        page_text("a", "asset", BODY).replace("status: stable", "status: stable\nasset: true");
    let path = f.put_page("asset", "a", &text);
    f.db()
        .upsert_page_meta(&PageMetaRow {
            page_key: String::from("wiki/asset/a.md"),
            hash: String::from("blake3:now-stale"),
            updated: String::from("2026-10-20"),
        })
        .unwrap();
    fs::write(&path, format!("{text}\n补充的说明。")).unwrap();

    let findings = f.lint();
    assert!(
        only(&findings, "L9").is_empty(),
        "asset pages change in place and carry no dates worth bumping"
    );
}

// ---------------------------------------------------------------- L10..L12

#[test]
fn l10_reports_a_raw_file_cited_by_more_than_three_pages() {
    let f = Fixture::new();
    let text = page_text("a", "concept", BODY)
        .replace("status: stable", "status: stable\nsources: [raw/shared.md]");
    for id in ["a", "b", "c", "d"] {
        f.put_page("concept", id, &text.replace("id: a", &format!("id: {id}")));
    }

    let findings = f.lint();
    let l10 = only(&findings, "L10");
    assert_eq!(l10.len(), 1, "{l10:?}");
    assert_eq!(l10[0].level, Level::Info);
    assert!(l10[0].msg.contains("shared.md"));
}

#[test]
fn l10_does_not_fire_at_three_pages() {
    let f = Fixture::new();
    let text = page_text("a", "concept", BODY)
        .replace("status: stable", "status: stable\nsources: [raw/shared.md]");
    for id in ["a", "b", "c"] {
        f.put_page("concept", id, &text.replace("id: a", &format!("id: {id}")));
    }
    let findings = f.lint();
    assert!(only(&findings, "L10").is_empty());
}

#[test]
fn l11_reports_a_log_line_that_is_not_a_log_entry() {
    let f = Fixture::new();
    let log = "## [2026-10-27] ingest | ok\n\n- detail\n\n## 2026-10-27 manual\n";
    fs::write(f.root().join("wiki/log.md"), log).unwrap();

    let findings = f.lint();
    let l11 = only(&findings, "L11");
    assert_eq!(l11.len(), 1, "{findings:?}");
    assert_eq!(l11[0].level, Level::Warning);
    assert_eq!(l11[0].page.as_deref(), Some("wiki/log.md"));
    assert!(
        l11[0].msg.contains("line 5"),
        "the line number locates it: {}",
        l11[0].msg
    );
}

#[test]
fn l12_reports_an_unreferenced_asset() {
    let mut f = Fixture::new();
    f.db()
        .upsert_raw(&RawRow {
            key: String::from("img.png"),
            hash: String::from("blake3:x"),
            source_key: String::from("img.png"),
            converted_at: String::from("2026-10-27T00:00:00Z"),
        })
        .unwrap();
    f.db()
        .upsert_raw(&RawRow {
            key: String::from("readme.md"),
            hash: String::from("blake3:y"),
            source_key: String::from("readme.pdf"),
            converted_at: String::from("2026-10-27T00:00:00Z"),
        })
        .unwrap();

    let findings = f.lint();
    let l12 = only(&findings, "L12");
    assert_eq!(l12.len(), 1, "{l12:?}");
    assert_eq!(l12[0].level, Level::Info);
    assert!(l12[0].msg.contains("img.png"));
}

// ------------------------------------------------------------- ordering, exit

#[test]
fn findings_are_sorted_by_level_then_rule_then_page() {
    let mut f = Fixture::new();
    // A warning from a missing date.
    let text = page_text("c", "concept", BODY).replace("updated: 2026-10-20\n", "");
    f.put_page("concept", "c", &text);
    // A warning from a short page.
    f.put_page("concept", "a", &page_text("a", "concept", "短"));
    // An error from a dead link.
    let body = format!("{BODY} [[ghost]]");
    f.put_page("concept", "b", &page_text("b", "concept", &body));
    // An info from an unreferenced asset.
    f.db()
        .upsert_raw(&RawRow {
            key: String::from("pic.png"),
            hash: String::from("blake3:x"),
            source_key: String::from("pic.png"),
            converted_at: String::from("2026-10-27T00:00:00Z"),
        })
        .unwrap();

    assert_eq!(
        rules(&f.lint()),
        vec!["L2", "L5", "L8", "L12"],
        "error, then warnings by rule, then info"
    );
}

#[test]
fn fails_is_strict_about_warnings_only() {
    let info = Finding {
        rule: "L12",
        level: Level::Info,
        page: None,
        msg: String::from("x"),
    };
    let warning = Finding {
        rule: "L5",
        level: Level::Warning,
        page: None,
        msg: String::from("x"),
    };
    let error = Finding {
        rule: "L1",
        level: Level::Error,
        page: None,
        msg: String::from("x"),
    };

    assert!(!lint::fails(std::slice::from_ref(&info), true));
    assert!(!lint::fails(std::slice::from_ref(&warning), false));
    assert!(lint::fails(&[warning], true));
    assert!(lint::fails(&[error], false));
}

// ---------------------------------------------------------------------- fix

#[test]
fn fix_repairs_l5_and_lint_is_clean_afterwards() {
    let mut f = Fixture::new();
    let missing = page_text("a", "concept", BODY).replace("updated: 2026-10-20\n", "");
    let malformed =
        page_text("b", "concept", BODY).replace("updated: 2026-10-20", "updated: 2026/10/20");
    let a = f.put_page("concept", "a", &missing);
    let b = f.put_page("concept", "b", &malformed);

    let report = f.fix();
    assert_eq!(report.fixed, 3, "two pages plus the index it had to create");
    assert!(report.remaining.is_empty(), "{:?}", report.remaining);

    let today = Local::now().date_naive();
    for path in [a, b] {
        let (front, _body) = page::read(&path).unwrap();
        assert_eq!(front.updated, today);
    }
    assert!(f.root().join("wiki/index.md").is_file());
    assert!(only(&f.lint(), "L5").is_empty());
}

#[test]
fn fix_refreshes_page_meta_after_a_body_edit() {
    let mut f = Fixture::new();
    let path = f.put_page("concept", "a", &page_text("a", "concept", BODY));
    let text = fs::read_to_string(&path).unwrap();
    f.db()
        .upsert_page_meta(&PageMetaRow {
            page_key: String::from("wiki/concept/a.md"),
            hash: hash::hash_bytes(text.as_bytes()),
            updated: String::from("2026-10-20"),
        })
        .unwrap();
    fs::write(&path, format!("{text}\n加了一句。\n")).unwrap();
    render::write_index(f.root(), &f.cfg).unwrap();

    let report = f.fix();
    assert_eq!(report.fixed, 1, "{:?}", report.remaining);
    assert!(only(&f.lint(), "L9").is_empty());

    // The refreshed record must describe the fixed file, not the old one.
    let fixed = fs::read_to_string(&path).unwrap();
    fs::write(&path, format!("{fixed}\n又改了一句。\n")).unwrap();
    let findings = f.lint();
    assert_eq!(only(&findings, "L9").len(), 1);
}

#[test]
fn fix_rewrites_a_stale_index_without_touching_pages() {
    let mut f = Fixture::new();
    f.put_page("concept", "a", &page_text("a", "concept", BODY));
    let index = f.root().join("wiki/index.md");
    assert!(!index.exists());

    let report = f.fix();
    assert_eq!(report.fixed, 1);
    assert!(index.is_file());

    let before = fs::read(&index).unwrap();
    let report = f.fix();
    assert_eq!(report.fixed, 0, "an up-to-date index is not rewritten");
    assert_eq!(fs::read(&index).unwrap(), before);
}

#[test]
fn fix_leaves_judgement_calls_alone() {
    let mut f = Fixture::new();
    let text = page_text("a", "concept", BODY).replace("title: a\n", "");
    f.put_page("concept", "a", &text);
    render::write_index(f.root(), &f.cfg).unwrap();

    let report = f.fix();
    assert_eq!(report.fixed, 0);
    assert_eq!(rules(&report.remaining), vec!["L1"]);
}

// -------------------------------------------------------------------- CLI

fn cli_fixture(min_chars: usize) -> TempDir {
    let dir = tempdir().unwrap();
    init::run(dir.path(), false).unwrap();
    let toml = dir.path().join("wiki.toml");
    let body = fs::read_to_string(&toml).unwrap().replace(
        "min_page_chars = 200",
        &format!("min_page_chars = {min_chars}"),
    );
    fs::write(&toml, body).unwrap();
    dir
}

fn cli(dir: &Path) -> Command {
    let mut cmd = Command::cargo_bin("llm-wiki").unwrap();
    cmd.current_dir(dir);
    cmd
}

#[test]
fn cli_lint_json_reports_findings_and_exit_codes() {
    let dir = cli_fixture(12);
    let root = dir.path();
    fs::create_dir_all(root.join("wiki/concept")).unwrap();
    fs::write(
        root.join("wiki/concept/a.md"),
        page_text("a", "concept", "订单超时后重试三次"),
    )
    .unwrap();

    let out = cli(root).args(["--json", "lint"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "a warning is not a failure");
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["cmd"], "lint");
    assert_eq!(v["data"]["findings"][0]["rule"], "L8");

    let out = cli(root)
        .args(["--json", "lint", "--strict"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "--strict turns warnings into 1");

    let broken = page_text("b", "concept", BODY).replace("status: stable", "status: broken");
    fs::write(root.join("wiki/concept/b.md"), broken).unwrap();
    let out = cli(root).args(["--json", "lint"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1), "an error always fails");
}

#[test]
fn cli_lint_info_does_not_fail_even_strict() {
    let dir = cli_fixture(12);
    let root = dir.path();
    fs::create_dir_all(root.join("wiki/concept")).unwrap();
    fs::write(
        root.join("wiki/concept/a.md"),
        page_text("a", "concept", BODY),
    )
    .unwrap();

    let mut store = Store::open(&root.join(".wiki/state.db")).unwrap();
    store.migrate().unwrap();
    store
        .upsert_raw(&RawRow {
            key: String::from("pic.png"),
            hash: String::from("blake3:x"),
            source_key: String::from("pic.png"),
            converted_at: String::from("2026-10-27T00:00:00Z"),
        })
        .unwrap();
    drop(store);

    let out = cli(root)
        .args(["--json", "lint", "--strict"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["data"]["findings"][0]["rule"], "L12");
}

#[test]
fn cli_lint_human_output_names_the_rule_and_page() {
    let dir = cli_fixture(12);
    let root = dir.path();
    fs::create_dir_all(root.join("wiki/concept")).unwrap();
    fs::write(
        root.join("wiki/concept/a.md"),
        page_text("a", "concept", "短"),
    )
    .unwrap();

    let out = cli(root).args(["lint", "--strict"]).output().unwrap();
    assert_eq!(out.status.code(), Some(1));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("L8 wiki/concept/a.md"), "{text}");
}

#[test]
fn cli_fix_repairs_l5_then_exits_zero() {
    let dir = cli_fixture(12);
    let root = dir.path();
    fs::create_dir_all(root.join("wiki/concept")).unwrap();
    let text =
        page_text("a", "concept", BODY).replace("updated: 2026-10-20", "updated: 2026/10/20");
    fs::write(root.join("wiki/concept/a.md"), text).unwrap();

    let out = cli(root)
        .args(["--json", "lint", "--fix"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["cmd"], "lint");
    assert_eq!(v["data"]["fixed"], 2, "the page and the index");

    let (front, _body) = page::read(&root.join("wiki/concept/a.md")).unwrap();
    assert_eq!(front.updated, Local::now().date_naive());
    let out = cli(root).args(["--json", "lint"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
}

// --------------------------------------------------- ingest writes page_meta

#[test]
fn commit_records_page_meta_for_written_candidates() {
    let mut f = Fixture::new();
    let text = page_text("a", "concept", BODY);
    staging::write_page(f.root(), &f.cfg, "concept/a", &text).unwrap();
    staging::write_pending(
        f.root(),
        &[(String::from("a.md"), String::from("blake3:source"))]
            .into_iter()
            .collect(),
    )
    .unwrap();

    let root = f.root().to_path_buf();
    let cfg = f.cfg.clone();
    ingest::commit(&root, &cfg, &mut f.store, &[String::from("concept/a")]).unwrap();

    let staged = f.root().join(".wiki/staging/concept/a.md");
    let row = f
        .store
        .get_page_meta("wiki/concept/a.md")
        .unwrap()
        .expect("commit records the candidate it wrote");
    assert_eq!(
        row.hash,
        hash::hash_file(&staged, true).unwrap(),
        "the record hashes the bytes after the date was stamped"
    );
    assert_eq!(row.updated, Local::now().date_naive().to_string());
}

#[test]
fn begin_without_a_source_is_an_error_the_cli_reports() {
    // Guards the ledger requirement the CLI layer relies on.
    let mut f = Fixture::new();
    let root = f.root().to_path_buf();
    let cfg = f.cfg.clone();
    let err = ingest::begin(
        &root,
        &cfg,
        &mut f.store,
        &BeginInput {
            sources: Vec::new(),
            pages: Vec::new(),
            dry_run: true,
        },
    )
    .unwrap_err();
    assert_eq!(err.code(), "E_NO_SOURCE");
}

// ------------------------------------------------------- archive exemptions

/// A summary page: an ordinary page marked `archived: true`.
fn archived_page(id: &str) -> String {
    page_text(id, "concept", BODY).replace("status: stable", "status: stable\narchived: true")
}

/// `days` before today, so a fixture ages with the wall clock.
fn days_ago(days: i64) -> String {
    (Local::now().date_naive() - chrono::Duration::days(days)).to_string()
}

/// Records a sealed summary, sealed (or last read) `age` days ago.
fn seal_archive(f: &mut Fixture, key: &str, age: i64, recalled: Option<i64>) {
    f.db()
        .upsert_archive(&llm_wiki::store::ArchiveRow {
            key: String::from(key),
            period_start: String::from("2026-01-01"),
            period_end: String::from("2026-01-31"),
            sources: vec![String::from("queries/a.md")],
            sealed_at: days_ago(age),
            sealed_by: String::from("agent"),
            last_recalled_at: recalled.map(days_ago),
            recall_count: i64::from(recalled.is_some()),
        })
        .unwrap();
}

#[test]
fn l6_exempts_a_source_in_the_archive() {
    let f = Fixture::new();
    // A seal replaces the sealed sources with the summary, whose bytes are the
    // distilled text rather than a raw file: it is not under `raw/`, and
    // requiring one would make every sealed page fail the audit.
    let text = page_text("a", "concept", BODY).replace(
        "status: stable",
        "status: stable\nsources: [wiki/archive/2026-01.md]",
    );
    f.put_page("concept", "a", &text);
    assert!(only(&f.lint(), "L6").is_empty());
}

#[test]
fn l6_still_reports_a_missing_source_beside_an_archive_one() {
    let f = Fixture::new();
    let text = page_text("a", "concept", BODY).replace(
        "status: stable",
        "status: stable\nsources: [wiki/archive/2026-01.md, raw/missing.md]",
    );
    f.put_page("concept", "a", &text);
    let findings = f.lint();
    let l6 = only(&findings, "L6");
    assert_eq!(l6.len(), 1, "the exemption is per reference: {l6:?}");
    assert!(l6[0].msg.contains("raw/missing.md"));
}

#[test]
fn l8_exempts_an_archived_page() {
    let f = Fixture::new();
    // A summary may be short: it is a distillation, not a page with prose.
    let text = archived_page("2026-01").replace(BODY, "短。");
    f.put_page("archive", "2026-01", &text);
    assert!(only(&f.lint(), "L8").is_empty(), "{:?}", f.lint());
}

#[test]
fn l13_reports_an_idle_summary_nothing_cites() {
    let mut f = Fixture::new();
    f.put_page("archive", "2026-01", &archived_page("2026-01"));
    seal_archive(&mut f, "wiki/archive/2026-01.md", 200, None);

    let findings = f.lint();
    let l13 = only(&findings, "L13");
    assert_eq!(l13.len(), 1, "{findings:?}");
    assert_eq!(l13[0].level, Level::Info);
    assert_eq!(l13[0].page.as_deref(), Some("wiki/archive/2026-01.md"));
    assert!(l13[0].msg.contains("200"), "{}", l13[0].msg);
}

#[test]
fn l13_ignores_a_summary_a_page_still_cites() {
    let mut f = Fixture::new();
    f.put_page("archive", "2026-01", &archived_page("2026-01"));
    let text = page_text("a", "concept", BODY).replace(
        "status: stable",
        "status: stable\nsources: [wiki/archive/2026-01.md]",
    );
    f.put_page("concept", "a", &text);
    seal_archive(&mut f, "wiki/archive/2026-01.md", 200, None);
    assert!(only(&f.lint(), "L13").is_empty());
}

#[test]
fn l13_counts_a_wikilink_as_an_inbound_reference() {
    let mut f = Fixture::new();
    f.put_page("archive", "2026-01", &archived_page("2026-01"));
    f.put_page(
        "concept",
        "a",
        &page_text("a", "concept", &format!("{BODY} 详见 [[2026-01]]。")),
    );
    seal_archive(&mut f, "wiki/archive/2026-01.md", 200, None);
    assert!(only(&f.lint(), "L13").is_empty(), "{:?}", f.lint());
}

#[test]
fn l13_ignores_a_summary_recalled_recently() {
    let mut f = Fixture::new();
    f.put_page("archive", "2026-01", &archived_page("2026-01"));
    // The deadline runs from the last read, not the seal: a recall resets it.
    seal_archive(&mut f, "wiki/archive/2026-01.md", 400, Some(3));
    assert!(only(&f.lint(), "L13").is_empty());
}

#[test]
fn l13_ignores_a_summary_sealed_within_the_window() {
    let mut f = Fixture::new();
    f.put_page("archive", "2026-01", &archived_page("2026-01"));
    seal_archive(&mut f, "wiki/archive/2026-01.md", 5, None);
    assert!(only(&f.lint(), "L13").is_empty());
}

#[test]
fn l13_ignores_an_archive_page_with_no_ledger_row() {
    let f = Fixture::new();
    // A hand-written page under `archive/` was never sealed, so nothing knows
    // its period; reporting it would be a guess about age.
    f.put_page("archive", "2026-01", &archived_page("2026-01"));
    assert!(only(&f.lint(), "L13").is_empty());
}
