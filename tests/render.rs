//! Behaviour of the bookkeeping render: the generated `wiki/index.md` catalogue
//! and the append-only `wiki/log.md`.
//!
//! The tests treat both files as black boxes: they build inputs, call the public
//! functions, and assert on the exact bytes. The two properties that matter most
//! are that `index.md` is a pure function of the page set (same input, same
//! bytes) and that `log.md` is only ever appended to — a bookkeeping file that
//! rewrites history is worse than no log at all.

use std::path::Path;

use chrono::NaiveDate;
use llm_wiki::config::Config;
use llm_wiki::render::{self, LogEntry, PageMeta};
use tempfile::tempdir;

/// A page meta with a one-character summary, so the index line is predictable.
fn meta(id: &str, title: &str, typ: &str, status: &str) -> PageMeta {
    PageMeta {
        id: String::from(id),
        title: String::from(title),
        typ: String::from(typ),
        summary: String::from("s"),
        status: String::from(status),
    }
}

/// A page with the minimal frontmatter the reader accepts.
fn write_page(root: &Path, typ: &str, id: &str, title: &str) {
    let path = root.join("wiki").join(typ).join(format!("{id}.md"));
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(
        &path,
        format!(
            "---\nid: {id}\ntitle: {title}\ntype: {typ}\nsummary: 摘要{id}\n\
             status: stable\nupdated: 2026-10-27\n---\n\n正文\n"
        ),
    )
    .unwrap();
}

#[test]
fn index_groups_by_type_and_sorts_titles() {
    let pages = vec![
        meta("b", "乙", "concept", "stable"),
        meta("a", "甲", "concept", "stable"),
    ];
    let out = render::render_index(&pages, &["concept".to_string()]);
    let ia = out.find("甲").unwrap();
    let ib = out.find("乙").unwrap();
    assert!(ia < ib);
    assert!(out.contains("## concept"));
}

#[test]
fn append_log_preserves_existing_bytes() {
    let d = tempdir().unwrap();
    let p = d.path().join("log.md");
    let before = "# 日志\n\n## [2026-01-01] init | 初始化\n";
    std::fs::write(&p, before).unwrap();
    let e = LogEntry {
        date: NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
        action: String::from("ingest"),
        title: String::from("订单"),
        detail: None,
    };
    render::append_log(&p, &e).unwrap();
    let after = std::fs::read_to_string(&p).unwrap();
    assert!(after.starts_with(before), "append-only，前缀必须原样");
    assert!(after.contains("## [2026-10-27] ingest | 订单"));
}

#[test]
fn log_title_with_pipe_does_not_break_parsing() {
    let e = LogEntry {
        date: NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
        action: String::from("ingest"),
        title: String::from("a | b"),
        detail: None,
    };
    let line = e.to_line();
    assert_eq!(line.matches(" | ").count(), 1, "标题里的竖线要转义");
}

#[test]
fn index_render_is_byte_identical_on_a_second_run() {
    let pages = vec![
        meta("b", "乙", "concept", "draft"),
        meta("a", "甲", "entity", "stable"),
    ];
    let types = vec![String::from("concept"), String::from("entity")];
    let first = render::render_index(&pages, &types);
    let second = render::render_index(&pages, &types);
    assert_eq!(first, second);
}

#[test]
fn type_without_pages_gets_no_section() {
    let pages = vec![meta("a", "甲", "concept", "stable")];
    let types = vec![String::from("concept"), String::from("entity")];
    let out = render::render_index(&pages, &types);
    assert!(out.contains("## concept"));
    assert!(!out.contains("## entity"), "空 type 不应出现小节");
}

#[test]
fn draft_pages_are_marked() {
    let pages = vec![
        meta("a", "草稿页", "concept", "draft"),
        meta("b", "稳定页", "concept", "stable"),
    ];
    let out = render::render_index(&pages, &[String::from("concept")]);
    let draft_line = out.lines().find(|l| l.contains("草稿页")).unwrap();
    let stable_line = out.lines().find(|l| l.contains("稳定页")).unwrap();
    assert!(draft_line.ends_with("（草稿）"), "got: {draft_line}");
    assert!(!stable_line.contains("（草稿）"), "got: {stable_line}");
}

#[test]
fn square_brackets_in_titles_are_escaped() {
    let pages = vec![meta("x", "A[B]C", "concept", "stable")];
    let out = render::render_index(&pages, &[String::from("concept")]);
    assert!(
        out.contains(r"[A\[B\]C](wiki/concept/x.md)"),
        "link text must escape brackets, got: {out}"
    );
}

#[test]
fn empty_page_set_renders_a_valid_index() {
    let out = render::render_index(&[], &[String::from("concept")]);
    assert_eq!(out, "# 索引\n");
}

#[test]
fn append_log_to_empty_file_starts_with_the_entry() {
    let d = tempdir().unwrap();
    let p = d.path().join("log.md");
    std::fs::write(&p, "").unwrap();
    let e = LogEntry {
        date: NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
        action: String::from("ingest"),
        title: String::from("空"),
        detail: None,
    };
    render::append_log(&p, &e).unwrap();
    let after = std::fs::read_to_string(&p).unwrap();
    assert!(after.starts_with("## ["), "got: {after:?}");
    assert!(after.contains("| 空"));
}

#[test]
fn two_appends_are_adjacent_lines() {
    let d = tempdir().unwrap();
    let p = d.path().join("missing-log.md");
    let date = NaiveDate::from_ymd_opt(2026, 10, 27).unwrap();
    render::append_log(
        &p,
        &LogEntry {
            date,
            action: String::from("ingest"),
            title: String::from("一"),
            detail: None,
        },
    )
    .unwrap();
    render::append_log(
        &p,
        &LogEntry {
            date,
            action: String::from("ingest"),
            title: String::from("二"),
            detail: None,
        },
    )
    .unwrap();
    let after = std::fs::read_to_string(&p).unwrap();
    assert_eq!(
        after,
        "# 日志\n\n## [2026-10-27] ingest | 一\n## [2026-10-27] ingest | 二\n"
    );
}

#[test]
fn missing_trailing_newline_is_repaired_before_appending() {
    let d = tempdir().unwrap();
    let p = d.path().join("log.md");
    std::fs::write(&p, "# 日志\n\n## [2026-01-01] init | 初始化").unwrap();
    let e = LogEntry {
        date: NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
        action: String::from("ingest"),
        title: String::from("二"),
        detail: None,
    };
    render::append_log(&p, &e).unwrap();
    let after = std::fs::read_to_string(&p).unwrap();
    assert_eq!(
        after,
        "# 日志\n\n## [2026-01-01] init | 初始化\n## [2026-10-27] ingest | 二\n"
    );
}

#[test]
fn multiline_detail_stays_multiline() {
    let d = tempdir().unwrap();
    let p = d.path().join("log.md");
    let e = LogEntry {
        date: NaiveDate::from_ymd_opt(2026, 10, 27).unwrap(),
        action: String::from("ingest"),
        title: String::from("详"),
        detail: Some(String::from("第一行\n第二行")),
    };
    render::append_log(&p, &e).unwrap();
    let after = std::fs::read_to_string(&p).unwrap();
    assert_eq!(
        after,
        "# 日志\n\n## [2026-10-27] ingest | 详\n\n第一行\n第二行\n\n"
    );
}

#[test]
fn write_index_reads_pages_and_leaves_log_and_audit_alone() {
    let d = tempdir().unwrap();
    let root = d.path();
    write_page(root, "concept", "alpha", "甲");
    write_page(root, "entity", "beta", "乙");
    write_page(root, "audit", "audit-page", "审计");

    let log = root.join("wiki/log.md");
    let log_before = "# 日志\n\n## [2026-01-01] init | 初始化\n";
    std::fs::write(&log, log_before).unwrap();

    let cfg = Config::default();
    let written = render::write_index(root, &cfg).unwrap();
    assert_eq!(written, 2, "audit/ 下的页面不算");

    let index = std::fs::read_to_string(root.join("wiki/index.md")).unwrap();
    assert!(index.contains("甲"));
    assert!(index.contains("乙"));
    assert!(!index.contains("审计"));
    assert_eq!(std::fs::read_to_string(&log).unwrap(), log_before);
}

#[test]
fn write_index_overwrites_a_stale_index() {
    let d = tempdir().unwrap();
    let root = d.path();
    std::fs::create_dir_all(root.join("wiki")).unwrap();
    std::fs::write(root.join("wiki/index.md"), "陈旧内容\n").unwrap();
    write_page(root, "concept", "alpha", "甲");

    let cfg = Config::default();
    assert_eq!(render::write_index(root, &cfg).unwrap(), 1);
    let index = std::fs::read_to_string(root.join("wiki/index.md")).unwrap();
    assert!(!index.contains("陈旧内容"));
    assert!(index.contains("甲"));
}

#[test]
fn long_summaries_are_truncated() {
    let long = "长".repeat(100);
    let mut page = meta("a", "甲", "concept", "stable");
    page.summary = long.clone();
    let out = render::render_index(&[page], &[String::from("concept")]);
    assert!(out.contains(&format!("{}…", "长".repeat(80))), "got: {out}");
    assert!(!out.contains(&"长".repeat(81)), "summary must be cut at 80");
}

#[test]
fn unknown_types_come_after_configured_ones_sorted_by_name() {
    let pages = vec![
        meta("z", "Z", "zzz", "stable"),
        meta("a", "A", "aaa", "stable"),
        meta("c", "C", "concept", "stable"),
    ];
    let out = render::render_index(&pages, &[String::from("concept")]);
    let concept = out.find("## concept").unwrap();
    let aaa = out.find("## aaa").unwrap();
    let zzz = out.find("## zzz").unwrap();
    assert!(concept < aaa && aaa < zzz, "got: {out}");
}

// ------------------------------------------------------------------ archive

#[test]
fn index_leaves_the_archive_out() {
    let d = tempdir().unwrap();
    llm_wiki::init::run(d.path(), false).unwrap();
    let cfg = Config::load(d.path()).unwrap();
    write_page(d.path(), "concept", "a", "甲");
    write_page(d.path(), "archive", "2026-05", "五月");

    let count = render::write_index(d.path(), &cfg).unwrap();
    assert_eq!(count, 1, "a summary is not a catalogue entry");
    let index = std::fs::read_to_string(d.path().join("wiki/index.md")).unwrap();
    assert!(index.contains("甲"), "{index}");
    assert!(!index.contains("五月"), "{index}");
    // The index is the only consumer that skips the archive; `lint` still reads
    // the page, so `index_text` must agree with what was written.
    assert_eq!(render::index_text(d.path(), &cfg).unwrap(), index);
}
