//! The end-to-end story, driven through the real binary.
//!
//! One repository goes from empty to mined: fixtures are dropped in, scanned,
//! converted, distilled into a page, reviewed, published, checked, searched,
//! synced (dry run), archived and finally recalled. Every step's exit code and
//! the fields later steps depend on are asserted, so a regression anywhere in
//! the chain shows up as a broken link rather than a green unit test.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_llm-wiki")
}

fn run(dir: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .current_dir(dir)
        .args(args)
        .output()
        .expect("llm-wiki is built for the tests")
}

/// Runs one command with `--json` and returns (exit code, parsed envelope).
fn call(dir: &Path, args: &[&str]) -> (i32, Value) {
    let mut full = vec!["--json"];
    full.extend_from_slice(args);
    let output = run(dir, &full);
    let json: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|_| {
        panic!(
            "`{}` must emit JSON: stdout={:?} stderr={:?}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code().expect("llm-wiki exits normally"), json)
}

/// Runs one command that must succeed and return `ok: true`.
fn ok_call(dir: &Path, args: &[&str]) -> Value {
    let (code, json) = call(dir, args);
    assert_eq!(code, 0, "`{}` failed: {json}", args.join(" "));
    assert_eq!(
        json["ok"],
        Value::Bool(true),
        "`{}`: {json}",
        args.join(" ")
    );
    json["data"].clone()
}

fn write(dir: &Path, rel: &str, body: impl AsRef<[u8]>) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).expect("parent dir");
    fs::write(&path, body).expect("fixture is writable");
}

fn read(dir: &Path, rel: &str) -> String {
    fs::read_to_string(dir.join(rel)).unwrap_or_else(|_| panic!("{rel} exists"))
}

/// A valid 1x1 RGBA PNG; images are linked, never parsed.
const PNG: &[u8] = &[
    0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, b'I', b'H', b'D', b'R',
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4,
    0x89, 0x00, 0x00, 0x00, 0x0A, b'I', b'D', b'A', b'T', b'x', 0x9C, 0x63, 0x00, 0x01, 0x00, 0x00,
    0x05, 0x00, 0x01, 0x0D, 0x0A, 0x2D, 0xB4, 0x00, 0x00, 0x00, 0x00, b'I', b'E', b'N', b'D', 0xAE,
    0x42, 0x60, 0x82,
];

const PAGE_FRONT: &str = "\
---
id: alpha
title: 重试与熔断
type: concept
summary: 重试与熔断的取舍
sources: [\"raw/notes/retry.md\"]
status: draft
updated: 2026-10-27
---";

fn page_body() -> String {
    // Long enough that the audit's min_page_chars (200) is satisfied.
    format!(
        "# 重试与熔断\n\n{}",
        "重试与熔断的决定逻辑写在这里。".repeat(20)
    )
}

/// Every regular file under `root`, keyed by relative path, valued by content.
fn snapshot(root: &Path) -> BTreeMap<String, String> {
    fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, String>) {
        for entry in fs::read_dir(dir).expect("readable dir") {
            let entry = entry.expect("entry");
            let path = entry.path();
            if entry.file_type().expect("file type").is_dir() {
                walk(root, &path, out);
            } else {
                let rel = path
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(
                    rel,
                    llm_wiki::hash::hash_bytes(&fs::read(&path).expect("read")),
                );
            }
        }
    }
    let mut map = BTreeMap::new();
    walk(root, root, &mut map);
    map
}

#[test]
fn init_scan_convert_ingest_publish_lint_checkpoint_query_sync_archive_recall() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();

    // ---------------------------------------------------------------- init
    let init = ok_call(root, &["init"]);
    assert!(
        init["created"]
            .as_array()
            .is_some_and(|created| !created.is_empty()),
        "init reports what it made: {init}"
    );
    assert!(root.join("wiki.toml").is_file());
    assert!(root.join("SCHEMA.md").is_file());
    assert!(root.join("source").is_dir());

    // -------------------------------------------------------------- fixtures
    write(
        root,
        "source/notes/retry.md",
        format!("# 重试与熔断\n\n{}", page_body()),
    );
    write(
        root,
        "source/notes/page.html",
        "<html><body><h1>Retry</h1><p>Backoff and circuit breaking.</p></body></html>",
    );
    write(root, "source/img/diagram.png", PNG);

    // ---------------------------------------------------------------- scan
    let scan = ok_call(root, &["scan"]);
    let changes = scan["changes"].as_array().expect("changes array");
    assert_eq!(changes.len(), 3, "three new sources: {scan}");
    for change in changes {
        assert_eq!(change["kind"], "new", "all three are new: {change}");
        assert!(
            change["hash"]
                .as_str()
                .is_some_and(|hash| hash.starts_with("blake3:")),
            "the ledger stores a blake3 hash: {change}"
        );
    }

    // ------------------------------------------------------------- convert
    let convert = ok_call(root, &["convert"]);
    let converted = convert["converted"].as_array().expect("converted array");
    assert_eq!(converted.len(), 3, "every source converts: {convert}");
    let raw_of = |rel: &str| -> String {
        converted
            .iter()
            .find(|item| item["raw_path"] == rel)
            .unwrap_or_else(|| panic!("{rel} in {convert}"))["raw_path"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(raw_of("raw/notes/retry.md"), "raw/notes/retry.md");
    assert_eq!(raw_of("raw/notes/page.md"), "raw/notes/page.md");
    assert_eq!(raw_of("raw/img/diagram.md"), "raw/img/diagram.md");
    let backend = |rel: &str| -> String {
        converted
            .iter()
            .find(|item| item["raw_path"] == rel)
            .unwrap_or_else(|| panic!("{rel} in {convert}"))["backend"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(backend("raw/notes/retry.md"), "builtin-text");
    assert_eq!(backend("raw/notes/page.md"), "builtin-html");
    assert_eq!(backend("raw/img/diagram.md"), "builtin-asset");
    let raw = read(root, "raw/notes/retry.md");
    assert!(
        raw.contains("source: source/notes/retry.md"),
        "raw frontmatter: {raw}"
    );
    assert!(
        raw.contains("parser: builtin-text"),
        "raw frontmatter: {raw}"
    );

    // ---------------------------------------------------- ingest begin
    let begin = ok_call(root, &["ingest", "begin", "--sources", "notes/retry.md"]);
    assert_eq!(begin["source_keys"], serde_json::json!(["notes/retry.md"]));
    let pending = begin["pending_hash"]
        .as_str()
        .expect("pending hash")
        .to_string();
    assert!(pending.starts_with("blake3:"), "pending hash: {pending}");

    // ------------------------------------------------- write the page
    write(
        root,
        ".wiki/staging/concept/alpha.md",
        format!("{PAGE_FRONT}\n\n{}\n", page_body()),
    );

    // ------------------------------------------------- ingest commit
    let commit = ok_call(root, &["ingest", "commit", "--pages", "concept/alpha"]);
    assert_eq!(
        commit["pages_written"],
        serde_json::json!(["concept/alpha"])
    );
    assert_eq!(commit["index_updated"], Value::Bool(true));
    assert!(root.join(".wiki/staging/concept/alpha.md").is_file());
    assert!(
        !root.join("wiki/concept/alpha.md").exists(),
        "commit only finishes staging; publishing is a human decision"
    );

    // ------------------------------------------------------------- lint
    let lint = ok_call(root, &["lint"]);
    let findings = lint["findings"].as_array().expect("findings array");
    assert!(
        findings.iter().all(|finding| finding["level"] != "error"),
        "a well-formed page lints clean: {lint}"
    );

    // -------------------------------------------------- approve (confirmed)
    let (code, refused) = call(root, &["approve"]);
    assert_eq!(
        code, 3,
        "publishing without confirmation is refused: {refused}"
    );
    assert_eq!(refused["ok"], Value::Bool(false));
    assert_eq!(refused["error"]["code"], "NEEDS_CONFIRMATION");

    let approve = ok_call(root, &["approve", "--confirm"]);
    assert_eq!(approve["version"], "v001");
    assert!(
        root.join("wiki/concept/alpha.md").is_file(),
        "the page is published"
    );
    let index = read(root, "wiki/index.md");
    assert!(
        index.contains("wiki/concept/alpha.md") && index.contains("重试与熔断"),
        "the index lists the page: {index}"
    );
    let log = read(root, "wiki/log.md");
    assert!(
        log.contains("approve") && log.contains("v001"),
        "publishing is journalled: {log}"
    );

    // ------------------------------------------------------- checkpoint
    let checkpoint = ok_call(root, &["checkpoint", "--label", "after publish"]);
    assert_eq!(checkpoint["version"], "v002");
    assert!(
        root.join(".wiki/versions/v002").is_dir(),
        "a snapshot exists"
    );
    assert!(root.join(".wiki/versions/v001").is_dir());

    // ------------------------------------------------------------- query
    let query = ok_call(root, &["query", "熔断"]);
    assert_eq!(
        query["count"], 1,
        "the published page is searchable at once: {query}"
    );
    let hit = &query["hits"][0];
    assert_eq!(hit["page"], "wiki/concept/alpha.md");
    assert_eq!(hit["title"], "重试与熔断");
    assert_eq!(hit["status"], "draft");
    assert_eq!(hit["archived"], Value::Bool(false));
    let sources = hit["sources"].as_array().expect("sources array");
    assert_eq!(sources[0]["raw"], "raw/notes/retry.md");
    assert_eq!(sources[0]["raw_available"], Value::Bool(true));

    let (code, missed) = call(root, &["query", "没有这个词"]);
    assert_eq!(code, 0, "an empty result is not an error: {missed}");
    assert_eq!(missed["data"]["count"], 0);

    // -------------------------------------------------------------- sync
    // A changed source is picked up by scan, then the dry run must not write.
    write(
        root,
        "source/notes/retry.md",
        format!("# 重试与熔断\n\n{}\n\n补充一句。", page_body()),
    );
    let scan = ok_call(root, &["scan"]);
    assert!(
        scan["changes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|change| change["kind"] == "changed"),
        "the edit is seen: {scan}"
    );

    let before = snapshot(root);
    let sync = ok_call(root, &["sync", "--dry-run", "--force"]);
    assert_eq!(sync["converted"], 0, "a dry run converts nothing: {sync}");
    assert!(
        sync["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|warning| warning.as_str().unwrap().contains("dry run")),
        "the would-be work is reported: {sync}"
    );
    let after = snapshot(root);
    assert_eq!(before, after, "a dry run leaves the tree untouched");

    // ------------------------------------------------------- archive seal
    // The agent writes the summary; the CLI only validates it, records the
    // seal and re-hangs the citations.
    write(
        root,
        "wiki/archive/2026-10.md",
        format!(
            "---\nid: 2026-10\ntitle: 重试与熔断\ntype: concept\nsummary: 十月会话\n\
             archived: true\nstatus: stable\nupdated: 2026-10-27\n---\n\n{}\n",
            page_body()
        ),
    );
    let seal = ok_call(
        root,
        &[
            "archive",
            "seal",
            "--sources",
            "notes/retry.md",
            "--title",
            "重试与熔断",
            "--period-end",
            "2026-10-27",
        ],
    );
    assert_eq!(seal["action"], "seal");
    assert_eq!(seal["key"], "wiki/archive/2026-10.md");
    assert_eq!(seal["pages"], serde_json::json!(["wiki/concept/alpha.md"]));
    assert_eq!(seal["updated_pages"], 1);
    let page = read(root, "wiki/concept/alpha.md");
    assert!(
        page.contains("wiki/archive/2026-10.md"),
        "the citing page now points at the sealed summary: {page}"
    );
    assert!(
        !page.contains("raw/notes/retry.md"),
        "the raw citation is retired"
    );

    let lint = ok_call(root, &["lint"]);
    let findings = lint["findings"].as_array().expect("findings array");
    assert!(
        findings.iter().all(|finding| finding["rule"] != "L6"),
        "a sealed source is no longer a dangling citation: {lint}"
    );

    // ------------------------------------------------------------- recall
    let recall = ok_call(root, &["recall", "熔断"]);
    let hits = recall["hits"].as_array().expect("hits array");
    assert_eq!(hits.len(), 1, "the sealed summary is recalled: {recall}");
    assert_eq!(hits[0]["title"], "重试与熔断");
    assert_eq!(hits[0]["period_end"], "2026-10-27");
    assert!(
        hits[0]["body"]
            .as_str()
            .is_some_and(|body| body.contains("重试与熔断")),
        "the summary body comes back: {recall}"
    );
    assert_eq!(recall["missing"], 0);

    let (code, missed) = call(root, &["recall", "没有这个词"]);
    assert_eq!(code, 1, "a recall miss is its own exit code: {missed}");
    assert_eq!(missed["error"]["code"], "E_NO_HIT");
}
