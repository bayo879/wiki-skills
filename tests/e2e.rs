//! The end-to-end story, driven through the real binary.
//!
//! One repository goes from empty to mined: fixtures are dropped in, scanned,
//! converted, written directly as a live page, searched, acknowledged with the
//! raw hash, checked, and queried again. Every step's exit code and the fields
//! later steps depend on are asserted, so a regression anywhere in the chain
//! shows up as a broken link rather than a green unit test.

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

/// Backdates a fixture source past `[ingest].settle_seconds` (five minutes by
/// default), so the round below sees a file that has stopped moving.
fn settle(dir: &Path, rel: &str) {
    let file = fs::File::options()
        .write(true)
        .open(dir.join(rel))
        .unwrap_or_else(|error| panic!("{rel} is readable: {error}"));
    file.set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(600))
        .expect("set mtime");
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

fn page_body() -> String {
    // Long enough that the audit's min_page_chars (200) is satisfied.
    format!(
        "# 重试与熔断\n\n{}",
        "重试与熔断的决定逻辑写在这里。".repeat(20)
    )
}

/// Every regular file outside the engine, keyed by relative path, valued by
/// content. This is the user's own tree; no command may write there.
fn outside_engine(root: &Path) -> BTreeMap<String, String> {
    snapshot(root)
        .into_iter()
        .filter(|(path, _)| !path.starts_with(".wiki/"))
        .collect()
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
fn doctor_ingest_direct_page_query_round_trip() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();

    // ----------------------------------------------------- doctor --fix
    // `init` is gone: repair is the only way into a fresh repository.
    ok_call(root, &["doctor", "--fix"]);
    assert!(root.join(".wiki/wiki.toml").is_file());
    assert!(root.join(".wiki/SCHEMA.md").is_file());

    // -------------------------------------------------------- fixtures
    write(
        root,
        "notes/retry.md",
        format!("# 重试与熔断\n\n{}", page_body()),
    );
    write(
        root,
        "notes/page.html",
        "<html><body><h1>Retry</h1><p>Backoff and circuit breaking.</p></body></html>",
    );
    write(root, "img/diagram.png", PNG);
    settle(root, "notes/retry.md");
    settle(root, "notes/page.html");

    // The sources are the user's files: the whole pipeline below must leave
    // every byte outside `.wiki/` untouched.
    let expected_outside = outside_engine(root);

    // ---------------------------------------------------------- ingest
    // One round: select, convert, and report the durable source work queue.
    let ingest = ok_call(root, &["ingest"]);
    let ingested = ingest["ingested"].as_array().expect("ingested array");
    assert!(
        ingested.iter().any(|key| key == "notes/retry.md"),
        "the round ingests the new source: {ingest}"
    );
    assert_eq!(ingest["write_to"], ".wiki/pages");
    assert!(
        ingest["types"]
            .as_array()
            .is_some_and(|types| types.iter().any(|typ| typ == "concept")),
        "the round names the page types: {ingest}"
    );
    assert!(
        root.join(".wiki/raw/notes/retry.md").is_file(),
        "conversion happened"
    );
    assert!(
        ingest["pending"].as_array().is_some_and(|pending| pending
            .iter()
            .any(|item| item["source_key"] == "notes/retry.md")),
        "the source is queued: {ingest}"
    );

    // The agent works the task tree instead of editing pages: a breakdown
    // lists the leaves, each leaf submits content, and the last close merges
    // the batch into the live tree. The round hands out one task at a time
    // until the whole forest settles.
    let mut next = ingest["next_task"].clone();
    assert_eq!(next["kind"], "breakdown");
    while !next.is_null() {
        let id = next["id"].as_i64().expect("task id");
        let kind = next["kind"].as_str().expect("task kind");
        let source = next["source_key"].as_str().unwrap_or_default();
        match kind {
            "breakdown" => {
                let (leaf_id, title) = if source == "notes/retry.md" {
                    ("alpha", "重试与熔断")
                } else {
                    ("beta", "分页与重试")
                };
                let mut leaves = tempfile::NamedTempFile::new().unwrap();
                std::io::Write::write_all(
                    &mut leaves,
                    serde_json::json!({"leaves":[{"type":"concept","id":leaf_id,"title":title,"brief":"写出来源里的结论"}]})
                        .to_string()
                        .as_bytes(),
                )
                .unwrap();
                let out = ok_call(
                    root,
                    &[
                        "task",
                        "done",
                        &id.to_string(),
                        "--leaves",
                        leaves.path().to_str().unwrap(),
                    ],
                );
                next = out["next_task"].clone();
            }
            "page" => {
                let note: serde_json::Value =
                    serde_json::from_str(next["payload_json"].as_str().expect("leaf note"))
                        .unwrap();
                let mut payload = tempfile::NamedTempFile::new().unwrap();
                let body = serde_json::json!({
                    "title": note["title"],
                    "summary": "来源给出的结论",
                    "tags": ["resilience"],
                    "status": "draft",
                    "body": page_body(),
                });
                std::io::Write::write_all(&mut payload, body.to_string().as_bytes()).unwrap();
                let out = ok_call(
                    root,
                    &[
                        "task",
                        "done",
                        &id.to_string(),
                        "--payload",
                        payload.path().to_str().unwrap(),
                    ],
                );
                next = out["next_task"].clone();
            }
            other => panic!("unexpected task kind {other}"),
        }
    }
    assert_eq!(next, serde_json::Value::Null);
    assert!(
        root.join(".wiki/pages/concept/alpha.md").is_file(),
        "the last leaf merged the batch into the live tree"
    );

    // ----------------------------------------------------------- query
    let query = ok_call(root, &["query", "熔断"]);
    assert!(
        query["count"].as_u64().is_some_and(|count| count >= 1),
        "a merged page is searchable: {query}"
    );
    // ------------------------------------------------------------ done
    let again = ok_call(root, &["ingest"]);
    let still_pending = again["pending"].as_array().expect("pending array");
    assert!(
        !still_pending
            .iter()
            .any(|item| item["source_key"] == "notes/retry.md"),
        "closing the tree clears only its queue entry: {again}"
    );
    // The index is rendered by a round, not by a write: the next round sees
    // the page the batch left behind.
    let index = read(root, ".wiki/pages/index.md");
    assert!(
        index.contains("concept/alpha.md") && index.contains("重试与熔断"),
        "the next round lists the merged page: {index}"
    );
    assert_eq!(
        outside_engine(root),
        expected_outside,
        "the task tree never rewrites the author's files"
    );

    // ---------------------------------------------------------- doctor
    let health = ok_call(root, &["doctor"]);
    assert!(
        health["checks"]
            .as_array()
            .is_some_and(|checks| !checks.is_empty()),
        "doctor reports every check: {health}"
    );
    let (code, missed) = call(root, &["query", "没有这个词"]);
    assert_eq!(code, 0, "no hits is not a failure: {missed}");

    // --------------------------------------------- the boundary holds
    let after = outside_engine(root);
    assert_eq!(
        after, expected_outside,
        "a command created, changed or deleted a file outside .wiki/"
    );
}
