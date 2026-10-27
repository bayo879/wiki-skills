//! The `graph` command and `parse --force-large`, as the binary exposes them.
//!
//! `graph::build` is covered by `tests/graph.rs`; what is checked here is the
//! wiring and the two exit codes a caller depends on: `graph` always succeeds on
//! a readable tree, and `parse` refuses an oversize source unless the operator
//! asks for it, because that refusal is the pipeline's only size guard.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use tempfile::{tempdir, TempDir};

fn run_json(dir: &Path, args: &[&str]) -> std::process::Output {
    Command::cargo_bin("llm-wiki")
        .unwrap()
        .args(["--json"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap()
}

fn json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not one JSON document ({err}): {:?}",
            String::from_utf8_lossy(&out.stdout)
        )
    })
}

fn repo() -> TempDir {
    let dir = tempdir().unwrap();
    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .arg("init")
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(out.status.success(), "init failed");
    dir
}

fn write_page(root: &Path, typ: &str, id: &str, body: &str) {
    let path = root.join("wiki").join(typ).join(format!("{id}.md"));
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        format!(
            "---\nid: {id}\ntitle: 标题{id}\ntype: {typ}\nsummary: 摘要{id}\n\
             status: stable\nupdated: 2026-10-27\n---\n\n{body}\n"
        ),
    )
    .unwrap();
}

#[test]
fn graph_reports_nodes_edges_and_orphans() {
    let dir = repo();
    let root = dir.path();
    write_page(root, "concept", "hub", "起点");
    write_page(root, "concept", "leaf", "回到 [[hub]]");
    write_page(root, "concept", "lonely", "没人链我");

    let out = run_json(root, &["graph"]);
    assert!(out.status.success(), "graph should succeed");

    let data = &json(&out)["data"];
    assert_eq!(data["edges"].as_array().unwrap().len(), 1);
    assert_eq!(
        data["orphans"],
        serde_json::json!(["wiki/concept/lonely.md"])
    );
    let hub = data["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["page"] == "wiki/concept/hub.md")
        .unwrap();
    assert_eq!(hub["in_degree"], 1);
}

#[test]
fn graph_on_an_empty_wiki_is_empty_not_an_error() {
    let dir = repo();
    let out = run_json(dir.path(), &["graph"]);
    assert!(out.status.success());
    let data = &json(&out)["data"];
    assert_eq!(data["nodes"], serde_json::json!([]));
    assert_eq!(data["orphans"], serde_json::json!([]));
}

#[test]
fn parse_refuses_an_oversize_source_without_force_large() {
    let dir = repo();
    let root = dir.path();
    // Shrink the ceiling so a normal note counts as oversize: the guard is the
    // same code path whatever the number is.
    let toml = fs::read_to_string(root.join("wiki.toml")).unwrap();
    let toml = toml.replace("max_source_bytes = \"100MB\"", "max_source_bytes = 16");
    fs::write(root.join("wiki.toml"), toml).unwrap();
    fs::create_dir_all(root.join("source")).unwrap();
    fs::write(
        root.join("source/note.md"),
        "这是一份超过十六字节的笔记正文。\n",
    )
    .unwrap();

    let refused = run_json(root, &["parse", "--key", "note.md"]);
    assert_eq!(refused.status.code(), Some(2), "oversize is a usage error");
    assert_eq!(json(&refused)["error"]["code"], "E_OVERSIZE");

    let forced = run_json(root, &["parse", "--key", "note.md", "--force-large"]);
    assert!(forced.status.success(), "force-large overrides the ceiling");
    let data = &json(&forced)["data"];
    assert_eq!(data["backend"], "builtin-text");
    assert!(
        data["chars"].as_u64().unwrap() > 0,
        "the extraction still happens: {data}"
    );
}

#[test]
fn force_large_still_writes_nothing() {
    let dir = repo();
    let root = dir.path();
    let toml = fs::read_to_string(root.join("wiki.toml")).unwrap();
    let toml = toml.replace("max_source_bytes = \"100MB\"", "max_source_bytes = 16");
    fs::write(root.join("wiki.toml"), toml).unwrap();
    fs::create_dir_all(root.join("source")).unwrap();
    fs::write(
        root.join("source/note.md"),
        "这是一份超过十六字节的笔记正文。\n",
    )
    .unwrap();

    let out = run_json(root, &["parse", "--key", "note.md", "--force-large"]);
    assert!(out.status.success());
    assert!(
        !root.join("raw/note.md").exists(),
        "parse never writes the raw layer"
    );
}
