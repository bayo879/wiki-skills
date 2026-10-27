//! Tests for `graph`: the `[[wikilink]]` structure of `wiki/`.
//!
//! Every fixture is written to a temp dir with the minimal frontmatter the
//! reader accepts, so the tests exercise the real directory walk rather than an
//! in-memory page list.

use std::fs;
use std::path::Path;

use llm_wiki::config::Config;
use llm_wiki::graph;
use tempfile::tempdir;

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
fn counts_in_and_out_degrees() {
    let d = tempdir().unwrap();
    let root = d.path();
    write_page(root, "concept", "a", "见 [[b]] 与 [[entity/c]]");
    write_page(root, "concept", "b", "没有链接");
    write_page(root, "entity", "c", "回到 [[a]]");

    let g = graph::build(root, &Config::default()).unwrap();

    let by_page = |key: &str| g.nodes.iter().find(|n| n.page == key).cloned().unwrap();
    let a = by_page("wiki/concept/a.md");
    assert_eq!((a.in_degree, a.out_degree), (1, 2), "{:?}", a);
    let b = by_page("wiki/concept/b.md");
    assert_eq!((b.in_degree, b.out_degree), (1, 0), "{:?}", b);
    let c = by_page("wiki/entity/c.md");
    assert_eq!((c.in_degree, c.out_degree), (1, 1), "{:?}", c);
}

#[test]
fn edges_are_sorted_and_deduplicated() {
    let d = tempdir().unwrap();
    let root = d.path();
    write_page(root, "concept", "a", "[[b]] [[b]] [[c]]");
    write_page(root, "concept", "b", "");
    write_page(root, "concept", "c", "");

    let g = graph::build(root, &Config::default()).unwrap();

    assert_eq!(
        g.edges,
        vec![
            graph::Edge {
                from: String::from("wiki/concept/a.md"),
                to: String::from("wiki/concept/b.md")
            },
            graph::Edge {
                from: String::from("wiki/concept/a.md"),
                to: String::from("wiki/concept/c.md")
            },
        ]
    );
    let a = g
        .nodes
        .iter()
        .find(|n| n.page == "wiki/concept/a.md")
        .unwrap();
    assert_eq!(a.out_degree, 3, "degrees count mentions, edges are unique");
}

#[test]
fn orphan_pages_have_neither_in_nor_out_links() {
    let d = tempdir().unwrap();
    let root = d.path();
    write_page(root, "concept", "hub", "[[leaf]]");
    write_page(root, "concept", "leaf", "");
    write_page(root, "concept", "lonely", "没有任何链接");

    let g = graph::build(root, &Config::default()).unwrap();

    assert_eq!(g.orphans, vec![String::from("wiki/concept/lonely.md")]);
}

#[test]
fn a_link_may_name_the_bare_id_or_the_wiki_relative_path() {
    let d = tempdir().unwrap();
    let root = d.path();
    write_page(root, "concept", "a", "[[concept/b]] [[b]] [[wiki/concept/b.md]]");
    write_page(root, "concept", "b", "");

    let g = graph::build(root, &Config::default()).unwrap();

    assert_eq!(g.edges.len(), 1, "three spellings, one edge: {:?}", g.edges);
    assert!(g.broken.is_empty(), "{:?}", g.broken);
}

#[test]
fn a_self_link_is_not_an_edge() {
    let d = tempdir().unwrap();
    let root = d.path();
    write_page(root, "concept", "a", "[[a]] 自指");

    let g = graph::build(root, &Config::default()).unwrap();

    assert!(g.edges.is_empty());
    assert_eq!(g.nodes[0].in_degree, 0);
    assert_eq!(g.nodes[0].out_degree, 0);
}

#[test]
fn broken_links_are_reported_not_dropped() {
    let d = tempdir().unwrap();
    let root = d.path();
    write_page(root, "concept", "a", "[[nowhere]]");

    let g = graph::build(root, &Config::default()).unwrap();

    assert_eq!(g.broken, vec![String::from("nowhere")]);
    assert!(g.edges.is_empty());
}

#[test]
fn links_inside_fenced_code_are_ignored() {
    let d = tempdir().unwrap();
    let root = d.path();
    write_page(root, "concept", "a", "```\n[[b]]\n```\n\n正文");
    write_page(root, "concept", "b", "");

    let g = graph::build(root, &Config::default()).unwrap();

    assert!(g.edges.is_empty());
}

#[test]
fn nodes_are_ordered_by_in_degree_then_path() {
    let d = tempdir().unwrap();
    let root = d.path();
    write_page(root, "concept", "hub", "");
    write_page(root, "concept", "a", "[[hub]]");
    write_page(root, "concept", "b", "[[hub]]");
    write_page(root, "concept", "quiet", "");

    let g = graph::build(root, &Config::default()).unwrap();

    let order: Vec<&str> = g.nodes.iter().map(|n| n.page.as_str()).collect();
    assert_eq!(
        order,
        vec![
            "wiki/concept/hub.md",
            "wiki/concept/a.md",
            "wiki/concept/b.md",
            "wiki/concept/quiet.md",
        ]
    );
}

#[test]
fn a_missing_type_directory_is_not_an_error() {
    let d = tempdir().unwrap();
    let root = d.path();
    write_page(root, "concept", "a", "");
    // `entity/`, `source/` … do not exist at all.
    let g = graph::build(root, &Config::default()).unwrap();
    assert_eq!(g.nodes.len(), 1);
}
