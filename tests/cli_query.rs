//! `query --page`: exact page reads, relation pagination and unresolved links.
//!
//! The point of the command is that a caller never has to scan the wiki to walk
//! one hop: the page reference, the neighbours on both sides and the dead links
//! all come from the relation index, deterministically ordered and paged.

use std::fs;
use std::path::Path;

use assert_cmd::Command;
use tempfile::{tempdir, TempDir};

struct Fixture {
    dir: TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        let dir = tempdir().unwrap();
        let out = Command::cargo_bin("llm-wiki")
            .unwrap()
            .args(["doctor", "--fix"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "doctor failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        Fixture { dir }
    }

    fn root(&self) -> &Path {
        self.dir.path()
    }

    fn page(&self, rel: &str, id: &str, body: &str) {
        let path = self.root().join(".wiki/pages").join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            format!(
                "---\nid: {id}\ntitle: {id}\ntype: concept\nsummary: {id} summary\n\
                 status: stable\nupdated: 2026-10-27\n---\n\n{body}\n"
            ),
        )
        .unwrap();
    }

    fn query(&self, args: &[&str]) -> std::process::Output {
        Command::cargo_bin("llm-wiki")
            .unwrap()
            .arg("--json")
            .arg("query")
            .args(args)
            .current_dir(self.root())
            .output()
            .unwrap()
    }
}

fn json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|err| {
        panic!(
            "stdout is not JSON ({err}): {:?} / stderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[test]
fn a_page_read_lists_both_directions_and_dead_links() {
    let f = Fixture::new();
    f.page("concept/a.md", "a", "见 [[b]] 与 [[ghost]]");
    f.page("concept/b.md", "b", "");
    f.page("concept/c.md", "c", "见 [[a]]");

    let out = f.query(&["--page", "concept/a", "--relation", "outgoing"]);
    assert!(out.status.success(), "stderr: {}", String::from_utf8_lossy(&out.stderr));
    let body = json(&out);
    assert_eq!(body["data"]["page"], ".wiki/pages/concept/a.md");
    let outgoing = &body["data"]["outgoing"];
    assert_eq!(outgoing["total"], 1);
    assert_eq!(outgoing["items"][0]["page"], ".wiki/pages/concept/b.md");
    assert_eq!(outgoing["unresolved"][0]["literal"], "ghost");
    assert_eq!(outgoing["unresolved"][0]["resolution"], "missing");
    assert_eq!(body["data"]["incoming"]["total"], 0);

    let out = f.query(&["--page", "concept/a", "--relation", "incoming"]);
    let body = json(&out);
    let incoming = &body["data"]["incoming"];
    assert_eq!(incoming["total"], 1);
    assert_eq!(incoming["items"][0]["page"], ".wiki/pages/concept/c.md");
}

#[test]
fn relation_groups_page_by_offset_and_limit() {
    let f = Fixture::new();
    f.page("concept/a.md", "a", "");
    f.page("concept/b.md", "b", "见 [[a]]");
    f.page("concept/c.md", "c", "见 [[a]]");

    let out = f.query(&[
        "--page", "concept/a", "--relation", "incoming", "--limit", "1",
    ]);
    let body = json(&out);
    let group = &body["data"]["incoming"];
    assert_eq!(group["total"], 2);
    assert_eq!(group["items"].as_array().unwrap().len(), 1);
    assert_eq!(group["next_offset"], 1);
    assert!(group["truncated"].as_bool().unwrap());

    let out = f.query(&[
        "--page", "concept/a", "--relation", "incoming", "--limit", "1", "--offset", "1",
    ]);
    let group = &json(&out)["data"]["incoming"];
    assert_eq!(group["items"][0]["page"], ".wiki/pages/concept/c.md");
    assert!(group["next_offset"].is_null());
    assert!(!group["truncated"].as_bool().unwrap());

    let out = f.query(&[
        "--page", "concept/a", "--relation", "incoming", "--offset", "9",
    ]);
    let group = &json(&out)["data"]["incoming"];
    assert!(group["items"].as_array().unwrap().is_empty());
    assert!(group["next_offset"].is_null());
}

#[test]
fn an_unknown_page_is_a_usage_error() {
    let f = Fixture::new();
    f.page("concept/a.md", "a", "");

    let out = f.query(&["--page", "concept/missing"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json(&out)["error"]["code"], "E_PAGE_NOT_FOUND");
}

#[test]
fn a_bare_id_shared_by_two_types_is_ambiguous() {
    let f = Fixture::new();
    f.page("concept/b.md", "b", "");
    f.page("entity/b.md", "b", "");

    let out = f.query(&["--page", "b"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json(&out)["error"]["code"], "E_PAGE_AMBIGUOUS");

    let out = f.query(&["--page", "concept/b"]);
    assert!(out.status.success());
}

#[test]
fn citing_without_a_source_is_rejected() {
    let f = Fixture::new();
    f.page("concept/a.md", "a", "");

    let out = f.query(&["--page", "concept/a", "--relation", "citing"]);
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(json(&out)["error"]["code"], "E_USAGE");
}

#[test]
fn human_output_names_the_next_offset() {
    let f = Fixture::new();
    f.page("concept/a.md", "a", "");
    f.page("concept/b.md", "b", "见 [[a]]");
    f.page("concept/c.md", "c", "见 [[a]]");

    let out = Command::cargo_bin("llm-wiki")
        .unwrap()
        .args([
            "query", "--page", "concept/a", "--relation", "incoming", "--limit", "1",
        ])
        .current_dir(f.root())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("incoming (1-1 of 2)"), "{text}");
    assert!(text.contains("next: --offset 1"), "{text}");
}
