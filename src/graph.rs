//! `graph`: the `[[wikilink]]` structure of `wiki/`, as data rather than prose.
//!
//! The edges come from the relation index, the same one `query --page` reads, so
//! a caller can walk in-degrees and exact neighbours without the two commands
//! ever disagreeing about whether a link resolves. `wiki/` is still the fact;
//! this is a read of its derived view, refreshed by the caller.
//!
//! Dead links are not dropped silently — they are reported in `broken`, because
//! a caller diffing in-degrees needs to know an edge went nowhere rather than
//! vanished.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::store::{RelationLink, Store, StoreError};

/// Failure while reading the relation index.
#[derive(Debug, thiserror::Error)]
pub enum GraphError {
    /// The ledger refused the read.
    #[error(transparent)]
    Store(#[from] StoreError),
}

impl GraphError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            GraphError::Store(_) => "E_DB",
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
    /// Link targets that name no page or name more than one (also `lint` L2).
    pub broken: Vec<String>,
}

/// Builds the link graph from the current relation index.
///
/// The index must already be current; the CLI refreshes before calling, and
/// `doctor --fix` rebuilds it from scratch.
pub fn build(store: &Store) -> Result<Graph> {
    let pages = store.all_relation_nodes()?;
    let mut out_degree: BTreeMap<String, usize> = pages
        .iter()
        .map(|node| (node.page_key.clone(), 0usize))
        .collect();
    let mut in_degree = out_degree.clone();
    let mut edges: Vec<Edge> = Vec::new();
    let mut broken: BTreeSet<String> = BTreeSet::new();

    for node in &pages {
        for link in store.relation_links_from(&node.page_key)? {
            let RelationLink {
                target_literal,
                to_page,
                ..
            } = link;
            let Some(to) = to_page else {
                broken.insert(target_literal);
                continue;
            };
            if to == node.page_key {
                continue;
            }
            edges.push(Edge {
                from: node.page_key.clone(),
                to: to.clone(),
            });
            *out_degree.entry(node.page_key.clone()).or_default() += 1;
            *in_degree.entry(to).or_default() += 1;
        }
    }

    edges.sort_by(|a, b| (&a.from, &a.to).cmp(&(&b.from, &b.to)));
    edges.dedup();

    let mut nodes: Vec<Node> = pages
        .iter()
        .map(|node| Node {
            page: node.page_key.clone(),
            in_degree: in_degree.get(&node.page_key).copied().unwrap_or(0),
            out_degree: out_degree.get(&node.page_key).copied().unwrap_or(0),
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
