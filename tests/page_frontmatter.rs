//! Page frontmatter: the parser must understand the small YAML subset pages
//! use, and the two writers must touch nothing but the scalar they name.
//!
//! Every write assertion compares the whole file, not just the changed line:
//! comments, indentation, quote style, CRLF and the BOM are all load-bearing
//! for a tool that edits markdown in place.

use chrono::NaiveDate;
use llm_wiki::page;
use std::fs;
use std::path::PathBuf;
use tempfile::tempdir;

const SRC: &str = "---\n# 这是注释，必须保留\nstatus: draft   # 行尾注释也保留\nid: order\nunknown_key: 保留我\ntitle: \"订单\"\nupdated: 2026-01-01\n---\n\n正文\n";

/// Writes `text` to `name` in `dir` and returns the path.
fn write_page(dir: &tempfile::TempDir, name: &str, text: &str) -> PathBuf {
    let path = dir.path().join(name);
    fs::write(&path, text).unwrap();
    path
}

fn date(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

#[test]
fn write_updated_touches_only_that_line() {
    let d = tempdir().unwrap();
    let p = write_page(&d, "order.md", SRC);
    page::write_updated(&p, date(2026, 10, 27)).unwrap();
    let got = fs::read_to_string(&p).unwrap();
    let expect = SRC.replace("updated: 2026-01-01", "updated: 2026-10-27");
    assert_eq!(got, expect, "除 updated 外必须逐字节不变");
}

#[test]
fn read_parses_known_and_keeps_unknown() {
    let d = tempdir().unwrap();
    let p = write_page(&d, "order.md", SRC);
    let (fm, body) = page::read(&p).unwrap();
    assert_eq!(fm.id, "order");
    assert_eq!(fm.title, "订单");
    assert_eq!(fm.typ, "");
    assert_eq!(fm.status, page::Status::Draft);
    assert!(body.contains("正文"));
    assert!(fm.extra.iter().any(|(k, _)| k == "unknown_key"));
}

#[test]
fn crlf_is_preserved_byte_for_byte() {
    let d = tempdir().unwrap();
    let crlf = SRC.replace('\n', "\r\n");
    let p = write_page(&d, "order.md", &crlf);
    page::write_updated(&p, date(2026, 10, 27)).unwrap();
    let got = fs::read_to_string(&p).unwrap();
    let expect = crlf.replace("updated: 2026-01-01", "updated: 2026-10-27");
    assert_eq!(got, expect, "CRLF 文件除 updated 外必须逐字节不变");
    assert!(
        !got.contains("\n")
            || got
                .match_indices('\n')
                .all(|(i, _)| i > 0 && got.as_bytes()[i - 1] == b'\r'),
        "每一行都必须仍是 CRLF"
    );
}

#[test]
fn set_status_keeps_the_trailing_comment() {
    let d = tempdir().unwrap();
    let p = write_page(&d, "order.md", SRC);
    page::set_status(&p, page::Status::Stable).unwrap();
    let got = fs::read_to_string(&p).unwrap();
    let expect = SRC.replace("status: draft", "status: stable");
    assert_eq!(got, expect, "行尾注释必须一字不差");
}

#[test]
fn hash_inside_quotes_is_not_a_comment() {
    let d = tempdir().unwrap();
    let text = "---\ntitle: \"a #b\"\nsummary: x #y\nupdated: 2026-01-01\n---\nbody\n";
    let p = write_page(&d, "a.md", text);
    let (fm, _) = page::read(&p).unwrap();
    assert_eq!(fm.title, "a #b");
    assert_eq!(fm.summary, "x");
}

#[test]
fn inline_arrays_parse() {
    let d = tempdir().unwrap();
    let text = "---\ntags: [a, b]\nsources: []\nupdated: 2026-01-01\n---\nbody\n";
    let p = write_page(&d, "a.md", text);
    let (fm, _) = page::read(&p).unwrap();
    assert_eq!(fm.tags, vec!["a", "b"]);
    assert!(fm.sources.is_empty());
}

#[test]
fn single_quotes_unescape() {
    let d = tempdir().unwrap();
    let text = "---\ntitle: 'it''s'\nupdated: 2026-01-01\n---\nbody\n";
    let p = write_page(&d, "a.md", text);
    let (fm, _) = page::read(&p).unwrap();
    assert_eq!(fm.title, "it's");
}

#[test]
fn missing_updated_errors_in_both_directions() {
    let d = tempdir().unwrap();
    let text = "---\nid: x\n---\nbody\n";
    let p = write_page(&d, "a.md", text);
    let err = page::read(&p).unwrap_err();
    assert_eq!(err.code(), "E_BAD_FRONTMATTER");
    assert!(
        err.to_string().contains("updated"),
        "消息要指明缺的是 updated: {err}"
    );
    assert!(err.to_string().contains("line "), "消息要带行号: {err}");

    let err = page::write_updated(&p, date(2026, 10, 27)).unwrap_err();
    assert_eq!(err.code(), "E_NO_UPDATED");
    assert_eq!(fs::read_to_string(&p).unwrap(), text, "报错时不得改动文件");
}

#[test]
fn bad_date_carries_the_line_number() {
    let d = tempdir().unwrap();
    let text = "---\nupdated: 2026-13-40\n---\nbody\n";
    let p = write_page(&d, "a.md", text);
    let err = page::read(&p).unwrap_err();
    assert_eq!(err.code(), "E_BAD_FRONTMATTER");
    let msg = err.to_string();
    assert!(msg.contains("updated"), "消息要指明 updated: {msg}");
    assert!(msg.contains("line 2"), "消息要带行号 2: {msg}");
}

#[test]
fn no_frontmatter_is_an_error() {
    let d = tempdir().unwrap();
    let p = write_page(&d, "a.md", "只有正文\n");
    let err = page::read(&p).unwrap_err();
    assert_eq!(err.code(), "E_BAD_FRONTMATTER");

    let p = write_page(&d, "b.md", "---\nid: x\nupdated: 2026-01-01\n");
    let err = page::read(&p).unwrap_err();
    assert_eq!(err.code(), "E_BAD_FRONTMATTER", "没有闭合的 --- 也要报错");
}

#[test]
fn sources_keep_fragments_verbatim() {
    let d = tempdir().unwrap();
    let text = "---\nsources: [raw/a.md, raw/big.pdf#p=12-40]\nupdated: 2026-01-01\n---\nbody\n";
    let p = write_page(&d, "a.md", text);
    let (fm, _) = page::read(&p).unwrap();
    assert_eq!(fm.sources, vec!["raw/a.md", "raw/big.pdf#p=12-40"]);
}

#[test]
fn writes_keep_extra_keys_in_order() {
    let d = tempdir().unwrap();
    let text =
        "---\nstatus: draft  # 注释\nunknown_b: 2\nunknown_a: 1\nupdated: 2026-01-01\n---\nbody\n";
    let p = write_page(&d, "a.md", text);
    page::write_updated(&p, date(2026, 10, 27)).unwrap();
    page::set_status(&p, page::Status::Stable).unwrap();

    let (fm, _) = page::read(&p).unwrap();
    let keys: Vec<&str> = fm.extra.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(keys, vec!["unknown_b", "unknown_a"]);

    let expect = text
        .replace("status: draft", "status: stable")
        .replace("updated: 2026-01-01", "updated: 2026-10-27");
    assert_eq!(fs::read_to_string(&p).unwrap(), expect);
}

#[test]
fn bom_is_preserved_and_tolerated_on_read() {
    let d = tempdir().unwrap();
    let text = format!("\u{feff}{SRC}");
    let p = write_page(&d, "order.md", &text);
    let (fm, _) = page::read(&p).unwrap();
    assert_eq!(fm.id, "order", "BOM 不应妨碍识别 frontmatter");

    page::write_updated(&p, date(2026, 10, 27)).unwrap();
    let got = fs::read_to_string(&p).unwrap();
    assert!(got.starts_with('\u{feff}'), "BOM 必须原样保留");
    let expect = text.replace("updated: 2026-01-01", "updated: 2026-10-27");
    assert_eq!(got, expect);
}

#[test]
fn block_sequences_parse() {
    let d = tempdir().unwrap();
    let text = "---\nid: retry\nsources:\n  - raw/a.md\n  - raw/b.md  # 行尾注释\ntags:\n  - 重试\nstatus: stable\nupdated: 2026-01-01\n---\n正文\n";
    let p = write_page(&d, "retry.md", text);
    let (fm, _) = page::read(&p).unwrap();
    assert_eq!(fm.sources, vec!["raw/a.md", "raw/b.md"]);
    assert_eq!(fm.tags, vec!["重试"]);
    assert_eq!(fm.status, page::Status::Stable);
}

#[test]
fn read_parses_every_known_field() {
    let d = tempdir().unwrap();
    let text = "---\nid: retry\ntitle: 超时重试\ntype: concept\nsummary: 失败后重试三次\ntags: [retry, 网络]\nsources: [raw/a.md]\nstatus: stable\nupdated: 2026-01-01\n---\n正文\n";
    let p = write_page(&d, "retry.md", text);
    let (fm, _) = page::read(&p).unwrap();
    assert_eq!(fm.id, "retry");
    assert_eq!(fm.title, "超时重试");
    assert_eq!(fm.typ, "concept");
    assert_eq!(fm.summary, "失败后重试三次");
    assert_eq!(fm.tags, vec!["retry", "网络"]);
    assert_eq!(fm.sources, vec!["raw/a.md"]);
    assert_eq!(fm.status, page::Status::Stable);
    assert_eq!(fm.updated, date(2026, 1, 1));
    assert!(fm.extra.is_empty());
}

#[test]
fn nested_mappings_are_rejected_instead_of_guessed() {
    let d = tempdir().unwrap();
    let text = "---\nmeta:\n  updated: 2026-01-01\n---\nbody\n";
    let p = write_page(&d, "a.md", text);
    let err = page::read(&p).unwrap_err();
    assert_eq!(err.code(), "E_BAD_FRONTMATTER");
    assert!(
        err.to_string().contains("line 3"),
        "缩进的嵌套行必须报出行号: {err}"
    );
}

#[test]
fn set_status_inserts_a_missing_line() {
    let d = tempdir().unwrap();
    let text = "---\nid: x\nupdated: 2026-01-01\n---\nbody\n";
    let p = write_page(&d, "a.md", text);
    page::set_status(&p, page::Status::Stable).unwrap();
    let expect = "---\nstatus: stable\nid: x\nupdated: 2026-01-01\n---\nbody\n";
    assert_eq!(fs::read_to_string(&p).unwrap(), expect);
    let (fm, _) = page::read(&p).unwrap();
    assert_eq!(fm.status, page::Status::Stable);
}

#[test]
fn body_starts_right_after_the_closing_delimiter() {
    let d = tempdir().unwrap();
    let p = write_page(&d, "order.md", SRC);
    let (_, body) = page::read(&p).unwrap();
    assert_eq!(body, "\n正文\n");
}

#[test]
fn quoted_date_keeps_its_quotes() {
    let d = tempdir().unwrap();
    let text = "---\nupdated: \"2026-01-01\"\n---\nbody\n";
    let p = write_page(&d, "a.md", text);
    page::write_updated(&p, date(2026, 10, 27)).unwrap();
    assert_eq!(
        fs::read_to_string(&p).unwrap(),
        "---\nupdated: \"2026-10-27\"\n---\nbody\n"
    );
}

#[test]
fn append_sources_grows_an_inline_array() {
    let d = tempdir().unwrap();
    let text = "---\nid: x\nsources: [raw/a.md, raw/b.md]\nupdated: 2026-01-01\n---\nbody\n";
    let p = write_page(&d, "a.md", text);
    page::append_sources(&p, &[String::from("raw/c.md")]).unwrap();
    assert_eq!(
        fs::read_to_string(&p).unwrap(),
        "---\nid: x\nsources: [raw/a.md, raw/b.md, \"raw/c.md\"]\nupdated: 2026-01-01\n---\nbody\n"
    );

    let empty = write_page(
        &d,
        "b.md",
        "---\nid: x\nsources: []\nupdated: 2026-01-01\n---\nbody\n",
    );
    page::append_sources(&empty, &[String::from("raw/c.md")]).unwrap();
    assert_eq!(
        fs::read_to_string(&empty).unwrap(),
        "---\nid: x\nsources: [\"raw/c.md\"]\nupdated: 2026-01-01\n---\nbody\n"
    );
}

#[test]
fn append_sources_keeps_a_block_list_indent_and_crlf() {
    let d = tempdir().unwrap();
    let text = "---\r\nid: x\r\nsources:\r\n  - raw/a.md  # 保留\r\n  - raw/b.md\r\nupdated: 2026-01-01\r\n---\r\nbody\r\n";
    let p = write_page(&d, "a.md", text);
    page::append_sources(&p, &[String::from("raw/c.md")]).unwrap();
    assert_eq!(
        fs::read_to_string(&p).unwrap(),
        "---\r\nid: x\r\nsources:\r\n  - raw/a.md  # 保留\r\n  - raw/b.md\r\n  - \"raw/c.md\"\r\nupdated: 2026-01-01\r\n---\r\nbody\r\n"
    );
    let (front, _) = page::read(&p).unwrap();
    assert_eq!(
        front.sources,
        vec![
            String::from("raw/a.md"),
            String::from("raw/b.md"),
            String::from("raw/c.md")
        ]
    );
}

#[test]
fn append_sources_converts_a_scalar_to_a_list() {
    let d = tempdir().unwrap();
    let text = "---\nid: x\nsources: raw/a.md  # 保留\nupdated: 2026-01-01\n---\nbody\n";
    let p = write_page(&d, "a.md", text);
    page::append_sources(&p, &[String::from("raw/b.md")]).unwrap();
    assert_eq!(
        fs::read_to_string(&p).unwrap(),
        "---\nid: x\nsources:\n  - raw/a.md  # 保留\n  - \"raw/b.md\"\nupdated: 2026-01-01\n---\nbody\n"
    );
}

#[test]
fn append_sources_inserts_a_missing_field() {
    let d = tempdir().unwrap();
    let p = write_page(&d, "a.md", "---\nid: x\nupdated: 2026-01-01\n---\nbody\n");
    page::append_sources(&p, &[String::from("raw/a.md")]).unwrap();
    assert_eq!(
        fs::read_to_string(&p).unwrap(),
        "---\nsources:\n  - \"raw/a.md\"\nid: x\nupdated: 2026-01-01\n---\nbody\n"
    );
    assert_eq!(
        page::append_sources(&p, &[]).unwrap(),
        0,
        "no items, no write"
    );
}
