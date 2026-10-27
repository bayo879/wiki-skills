//! `graph`: the `[[wikilink]]` structure of `wiki/`, as data rather than prose.
//!
//! `lint` already walks these links to report dead ones; this module exposes the
//! same edges so an agent can ask "what is a hub, what is a leaf, what is not
//! linked at all" without grepping bodies. It is deliberately read-only and
//! cheap: one directory walk, no database, no cache.
//!
//! Dead links are not dropped silently — they are reported in `broken`, because
//! a caller diffing in-degrees needs to know an edge went nowhere rather than
//! vanished.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::config::Config;
use crate::lint::{normalise_link, wikilinks};
use crate::page;
use crate::paths;

/// Failure while reading pages for the graph.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// A directory or page could not be read.
    #[error("cannot access {}: {source}", path.display())]
    Io {
        /// Path that failed.
        path: std::path::PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// A page's frontmatter could not be parsed.
    #[error(transparent)]
    Page(#[from] page::PageError),
}

impl GraphError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            GraphError::Io { .. } => "E_IO",
            GraphError::Page(err) => err.code(),
        }
    }
}

/// Result of the graph helpers.
pub type Result<T> = std::result::Result<T, GraphError>;

/// One node: a page and how connected it is.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Node {
    /// Repository-relative, `/`-separated path (`wiki/concept/a.md`).
    pub page: String,
    /// Links pointing at this page.
    pub in_degree: usize,
    /// Links leaving this page that resolve to a page.
    pub out_degree: usize,
}

/// One resolved edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Edge {
    /// Source page, repository-relative.
    pub from: String,
    /// Target page, repository-relative.
    pub to: String,
}

/// The whole graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Graph {
    /// Every page, most linked first, then by path.
    pub nodes: Vec<Node>,
    /// Every resolved link, sorted by `(from, to)`.
    pub edges: Vec<Edge>,
    /// Pages with no inbound and no outbound link.
    pub orphans: Vec<String>,
    /// Link targets that name no page (already reported by `lint` as dead links).
    pub broken: Vec<String>,
}

/// Builds the link graph of `wiki/`.
pub fn build(root: &Path, cfg: &Config) -> Result<Graph> {
    let pages = read_pages(root, cfg)?;

    // A link may name either the wiki-relative path (`concept/a`) or the bare id
    // (`a`); ids are unique across types, or `lint` reports L3.
    let mut by_name: BTreeMap<String, String> = BTreeMap::new();
    for (key, rel, id, _) in &pages {
        by_name.insert(rel.clone(), key.clone());
        if !id.is_empty() {
            by_name.insert(id.clone(), key.clone());
        }
    }

    let mut out_degree: BTreeMap<String, usize> = pages
        .iter()
        .map(|(key, ..)| (key.clone(), 0usize))
        .collect();
    let mut in_degree: BTreeMap<String, usize> = out_degree.clone();
    let mut edges: Vec<Edge> = Vec::new();
    let mut broken: BTreeSet<String> = BTreeSet::new();

    for (key, _, _, body) in &pages {
        for target in wikilinks(body) {
            let wanted = normalise_link(&target, &cfg.paths.wiki);
            if wanted.is_empty() {
                continue;
            }
            let Some(to) = by_name.get(&wanted) else {
                broken.insert(wanted);
                continue;
            };
            if to == key {
                continue;
            }
            edges.push(Edge {
                from: key.clone(),
                to: to.clone(),
            });
            *out_degree.entry(key.clone()).or_default() += 1;
            *in_degree.entry(to.clone()).or_default() += 1;
        }
    }

    edges.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    edges.dedup();

    let mut nodes: Vec<Node> = pages
        .iter()
        .map(|(key, ..)| Node {
            page: key.clone(),
            in_degree: in_degree.get(key).copied().unwrap_or(0),
            out_degree: out_degree.get(key).copied().unwrap_or(0),
        })
        .collect();
    nodes.sort_by(|a, b| {
        b.in_degree
            .cmp(&a.in_degree)
            .then_with(|| a.page.cmp(&b.page))
    });

    let orphans = nodes
        .iter()
        .filter(|node| node.in_degree == 0 && node.out_degree == 0)
        .map(|node| node.page.clone())
        .collect();

    Ok(Graph {
        nodes,
        edges,
        orphans,
        broken: broken.into_iter().collect(),
    })
}

/// Pages as `(key, wiki-relative path without `.md`, id, body)`.
fn read_pages(root: &Path, cfg: &Config) -> Result<Vec<(String, String, String, String)>> {
    let wiki_dir = root.join(&cfg.paths.wiki);
    let mut out = Vec::new();
    for typ in &cfg.schema.types {
        let dir = wiki_dir.join(typ);
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
            Err(source) => return Err(GraphError::Io { path: dir, source }),
        };
        for entry in entries {
            let entry = entry.map_err(|source| GraphError::Io {
                path: dir.clone(),
                source,
            })?;
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("md") {
                continue;
            }
            if !entry
                .file_type()
                .map(|kind| kind.is_file())
                .unwrap_or(false)
            {
                continue;
            }
            let (front, body) = page::read(&path)?;
            let key = paths::identity_key(root, &path).map_err(|err| GraphError::Io {
                path: path.clone(),
                source: std::io::Error::other(err.to_string()),
            })?;
            out.push((key, format!("{typ}/{}", front.id), front.id, body));
        }
    }
    out.sort();
    Ok(out)
}
