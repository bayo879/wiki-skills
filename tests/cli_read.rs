//! `read` prints an internal wiki file, windowed so a long page cannot flood
//! the caller.

use std::fs;

use assert_cmd::Command;
use tempfile::TempDir;

fn repo() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    llm_wiki::init::run(dir.path(), false).unwrap();
    dir
}

fn run(dir: &TempDir, args: &[&str]) -> (i32, serde_json::Value) {
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .current_dir(dir.path())
        .args(["--json"])
        .args(args)
        .output()
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout is not JSON: {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    (out.status.code().unwrap_or(-1), body)
}

fn write_page(dir: &TempDir, name: &str, body: &str) {
    let dir = dir.path().join(".wiki/pages/concept");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), body).unwrap();
}

fn write_raw(dir: &TempDir, name: &str, body: &str) {
    let dir = dir.path().join(".wiki/raw");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join(name), body).unwrap();
}

#[test]
fn reads_a_page_by_its_ingest_reference() {
    let dir = repo();
    write_page(&dir, "retry.md", "---\nid: retry\n---\n\n三次。\n");

    let (code, body) = run(&dir, &["read", "page:concept/retry.md"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["reference"], "page:concept/retry.md");
    assert_eq!(body["data"]["line_start"], 1);
    assert_eq!(body["data"]["line_end"], 5);
    assert_eq!(body["data"]["total_lines"], 5);
    assert_eq!(body["data"]["truncated"], false);
    assert!(body["data"]["content"].as_str().unwrap().contains("三次。"));
}

#[test]
fn reads_the_schema_and_raw_by_alias() {
    let dir = repo();
    write_raw(&dir, "a.md", "# A\n\n正文。\n");

    let (code, body) = run(&dir, &["read", "schema", "--limit", "2"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["reference"], "schema");
    assert_eq!(body["data"]["line_end"], 2);
    assert!(body["data"]["content"]
        .as_str()
        .unwrap()
        .starts_with("# Page schema"));

    let (code, body) = run(&dir, &["read", "raw:a.md"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["content"], "# A\n\n正文。\n");
}

#[test]
fn a_long_file_is_windowed_and_capped_at_two_thousand_lines() {
    let dir = repo();
    let body: String = (1..=2500).map(|n| format!("line {n}\n")).collect();
    write_page(&dir, "long.md", &body);

    let (code, body) = run(&dir, &["read", "page:concept/long.md"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["line_start"], 1);
    assert_eq!(body["data"]["line_end"], 2000);
    assert_eq!(body["data"]["total_lines"], 2500);
    assert_eq!(body["data"]["truncated"], true);
    assert_eq!(
        body["data"]["content"].as_str().unwrap().lines().count(),
        2000
    );

    let (code, body) = run(&dir, &["read", "page:concept/long.md", "--offset", "2001"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["line_start"], 2001);
    assert_eq!(body["data"]["line_end"], 2500);
    assert_eq!(body["data"]["truncated"], false);
    assert_eq!(
        body["data"]["content"].as_str().unwrap().lines().count(),
        500
    );

    let (code, body) = run(&dir, &["read", "page:concept/long.md", "--limit", "9999"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["line_end"], 2000);
}

#[test]
fn empty_files_and_zero_arguments_are_handled() {
    let dir = repo();
    write_page(&dir, "empty.md", "");

    let (code, body) = run(&dir, &["read", "page:concept/empty.md"]);
    assert_eq!(code, 0, "{body}");
    assert_eq!(body["data"]["content"], "");
    assert_eq!(body["data"]["total_lines"], 0);
    assert_eq!(body["data"]["line_start"], 0);

    let (code, body) = run(&dir, &["read", "page:concept/empty.md", "--offset", "0"]);
    assert_eq!(code, 2, "{body}");
    assert_eq!(body["error"]["code"], "E_USAGE");

    let (code, body) = run(&dir, &["read", "page:concept/empty.md", "--limit", "0"]);
    assert_eq!(code, 2, "{body}");
    assert_eq!(body["error"]["code"], "E_USAGE");
}

#[test]
fn absolute_and_parent_references_are_refused() {
    let dir = repo();
    for reference in ["/etc/passwd", "../a.md", "pages/../a.md"] {
        let (code, body) = run(&dir, &["read", reference]);
        assert_eq!(code, 2, "{body}");
        assert_eq!(body["error"]["code"], "E_BAD_REF");
    }
}

#[test]
fn a_missing_file_and_binary_content_are_errors() {
    let dir = repo();
    let (code, body) = run(&dir, &["read", "page:concept/nope.md"]);
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_NO_FILE");
    assert!(body["error"]["message"]
        .as_str()
        .unwrap()
        .contains("page:concept/nope.md"));

    fs::create_dir_all(dir.path().join(".wiki/pages/concept")).unwrap();
    fs::write(dir.path().join(".wiki/pages/concept/blob.md"), [0xff, 0xfe]).unwrap();
    let (code, body) = run(&dir, &["read", "page:concept/blob.md"]);
    assert_eq!(code, 1, "{body}");
    assert_eq!(body["error"]["code"], "E_NOT_TEXT");
}

#[test]
fn human_output_shows_the_window_and_next_offset() {
    let dir = repo();
    write_page(&dir, "a.md", "one\ntwo\nthree\n");
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .current_dir(dir.path())
        .args(["read", "page:concept/a.md", "--limit", "2"])
        .output()
        .unwrap();
    assert!(out.status.success());
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(text.starts_with("reference: page:concept/a.md\n"));
    assert!(text.contains("lines: 1-2 of 3 (next --offset 3)"));
    assert!(text.contains("one\ntwo\n"));
}
