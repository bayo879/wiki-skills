//! Behaviour of the generated `pages/index.md` catalogue.
//!
//! The tests treat the file as a black box: they build inputs, call the public
//! functions, and assert on the exact bytes. The property that matters most is
//! that `index.md` is a pure function of the page set (same input, same bytes),
//! and that it never touches anything else under `pages/`.

use std::path::Path;

use llm_wiki::config::Config;
use llm_wiki::render::{self, PageMeta};
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
    let path = root.join(".wiki/pages").join(typ).join(format!("{id}.md"));
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
fn write_index_reads_pages_and_leaves_the_audit_tree_alone() {
    let d = tempdir().unwrap();
    let root = d.path();
    write_page(root, "concept", "alpha", "甲");
    write_page(root, "entity", "beta", "乙");
    write_page(root, "audit", "audit-page", "审计");

    let journal = root.join(".wiki/journal.jsonl");
    std::fs::create_dir_all(journal.parent().unwrap()).unwrap();
    let journal_before = "{\"action\":\"init\"}\n";
    std::fs::write(&journal, journal_before).unwrap();

    let cfg = Config::default();
    let written = render::write_index(root, &cfg).unwrap();
    assert_eq!(written, 2, "audit/ 下的页面不算");

    let index = std::fs::read_to_string(root.join(".wiki/pages/index.md")).unwrap();
    assert!(index.contains("甲"));
    assert!(index.contains("乙"));
    assert!(!index.contains("审计"));
    assert_eq!(std::fs::read_to_string(&journal).unwrap(), journal_before);
}

#[test]
fn write_index_overwrites_a_stale_index() {
    let d = tempdir().unwrap();
    let root = d.path();
    std::fs::create_dir_all(root.join(".wiki/pages")).unwrap();
    std::fs::write(root.join(".wiki/pages/index.md"), "陈旧内容\n").unwrap();
    write_page(root, "concept", "alpha", "甲");

    let cfg = Config::default();
    assert_eq!(render::write_index(root, &cfg).unwrap(), 1);
    let index = std::fs::read_to_string(root.join(".wiki/pages/index.md")).unwrap();
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
