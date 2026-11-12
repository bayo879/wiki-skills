//! `query`: full-text search over the wiki, backed by the FTS5 table in
//! `state.db` (see [`crate::store`]).
//!
//! Only `wiki/**/*.md` is indexed, archived summaries included. `raw/` never
//! enters the index: it is orders of magnitude larger than the wiki, and a
//! query answers "what did we write down", not "which page of the source says
//! this word". A hit carries the paths needed to read the source instead.
//!
//! Chinese has no word boundaries, so `unicode61` would index 「订单处理」 as one
//! token and 「订单」 would never match it. Both sides therefore go through
//! [`segment`]: the index stores 「订 单 处 理」, the query is a phrase
//! 「"订 单"」, and [`unsegment`] puts snippets back together before they are
//! shown. The two sides must share this function — a mismatch is silent.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::config::Config;
use crate::hash;
use crate::page;
use crate::paths;
use crate::relation::{PageIdentity, Resolver};
use crate::render;
use crate::store::{
    FtsScope, PageMetaRow, PageRelations, RefreshWrite, RelationLink, RelationNode, RelationSource,
    Store, StoreError,
};

/// How many neighbour summaries `--expand` may inline per hit.
///
/// One hop of expansion is cheap; an unbounded list is not. Five neighbours
/// already turn ten hits into fifty summaries, which is the point where the
/// caller should be indexing their own notes instead of reading more of ours.
const EXPANDED_LIMIT: usize = 5;

/// One search hit: the page identity plus the three kinds of next step that
/// make it worth opening (neighbours, sources, fan-in).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Hit {
    /// Repository-relative page path, such as `wiki/concept/order-timeout.md`.
    pub page: String,
    /// Page title from the frontmatter.
    pub title: String,
    /// One-line summary from the frontmatter.
    pub summary: String,
    /// Frontmatter status: `draft`, `stable` or `stale`.
    pub status: String,
    /// Relevance, higher is better; `-bm25`, so the ordering is descending.
    pub score: f64,
    /// Body excerpt around the match, already un-segmented.
    pub snippet: String,
    /// Pages this page links to, paged from the relation index.
    pub outgoing: PageGroup,
    /// Pages linking to this one, paged from the relation index.
    pub incoming: PageGroup,
    /// Source references from the frontmatter, with the chunk split out.
    pub sources: SourceGroup,
    /// One-hop neighbour summaries, only filled when `--expand` is at least 1.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub expanded: Vec<ExpandedRef>,
}

/// One neighbour summary inlined by `--expand`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ExpandedRef {
    /// Repository-relative path of the neighbour page.
    pub page: String,
    /// The neighbour's title.
    pub title: String,
    /// The neighbour's summary; never its body.
    pub summary: String,
}

/// Which edge set an exact page read returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RelationKind {
    /// Pages this page links to.
    Outgoing,
    /// Pages linking to this page.
    Incoming,
    /// Only the raw files this page cites.
    Sources,
    /// Every group.
    All,
}

/// Default page size for one relation group.
pub const RELATION_LIMIT_DEFAULT: usize = 20;
/// Upper bound a caller cannot raise.
pub const RELATION_LIMIT_MAX: usize = 100;

/// One page plus the relation groups a reader may traverse.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecallItem {
    /// Repository-relative page path.
    pub page: String,
    pub title: String,
    pub summary: String,
    pub status: String,
    /// Body excerpt for a search hit; empty for an exact read.
    pub snippet: String,
    /// Pages this page links to.
    pub outgoing: PageGroup,
    /// Pages linking to this page.
    pub incoming: PageGroup,
    /// Raw files this page cites.
    pub sources: SourceGroup,
    /// Relation index generation this answer was read from.
    pub generation: i64,
    /// True when the page itself could not be fully indexed.
    pub incomplete: bool,
}

/// One paginated page group.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct PageGroup {
    /// Whether the caller asked for this relation. A group that was not
    /// requested is empty for that reason, not because no page matched.
    pub requested: bool,
    /// Resolved items in the group, before pagination.
    pub total: usize,
    pub offset: usize,
    pub limit: usize,
    /// Offset for the next page, `null` at the end.
    pub next_offset: Option<usize>,
    /// Command that reads the next page, `null` at the end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_command: Option<String>,
    pub truncated: bool,
    pub items: Vec<RecallPage>,
    /// Link literals that do not resolve, never paginated.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<Unresolved>,
}

/// One neighbour page in a group.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecallPage {
    pub page: String,
    pub title: String,
    pub summary: String,
    pub status: String,
}

/// A link literal that resolved to nothing, or to more than one page.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Unresolved {
    pub literal: String,
    pub resolution: String,
}

/// The source references of one page; frontmatter bounds the list, so it is
/// returned whole rather than paginated.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SourceGroup {
    /// Whether the caller asked for sources; false means "not requested".
    pub requested: bool,
    pub total: usize,
    pub items: Vec<RecallSource>,
}

/// One raw reference with the facts needed to open it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecallSource {
    /// The reference as written, fragment included.
    pub raw: String,
    /// Raw path without the fragment.
    pub raw_key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fragment: Option<String>,
    pub raw_available: bool,
    /// Read command for the raw file. `read` never understands `#fragment`,
    /// so this command drops it and starts at line 1; it still returns a line
    /// window rather than every line of a long file.
    pub read_command: String,
}

/// Exact reverse lookup: one raw reference and the pages citing it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceView {
    /// The reference as the caller wrote it.
    pub raw: String,
    /// Raw path without the fragment, as stored in the index.
    pub raw_key: String,
    /// Whether that raw file is present in this checkout.
    pub available: bool,
    /// Pages whose frontmatter cites the raw.
    pub citing: PageGroup,
    /// Index generation the answer came from.
    pub generation: i64,
}

/// What to search for, and how much to bring back.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchQuery {
    /// Words the user typed, unsegmented.
    pub text: String,
    /// Maximum number of hits.
    pub limit: usize,
    /// `0` for the hit alone, `1` (or more) to inline one hop of neighbours.
    pub expand: usize,
    /// Which half of the index to look in.
    pub scope: Scope,
}

/// Which pages a search may return. The index always contains the whole wiki;
/// the scope only filters the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Every indexed page.
    All,
    /// Every indexed page except the session-distilled directory.
    ExcludingSessions,
}

/// Result of the query helpers.
pub type Result<T> = std::result::Result<T, QueryError>;

/// Why a search could not run.
#[derive(Debug, thiserror::Error)]
pub enum QueryError {
    /// The query had no searchable term.
    #[error("query text is empty")]
    EmptyQuery,
    /// The ledger refused a read or a write.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The wiki directory could not be walked.
    #[error(transparent)]
    Render(#[from] render::RenderError),
    /// A page file could not be digested.
    #[error(transparent)]
    Hash(#[from] hash::HashError),
    /// A page path could not be turned into a repository-relative key.
    #[error(transparent)]
    Path(#[from] paths::PathError),
    /// An exact reference names no indexed page.
    #[error("no page matches `{0}`")]
    PageNotFound(String),
    /// An exact reference matches more than one page.
    #[error("`{0}` is ambiguous; use the type-qualified name")]
    PageAmbiguous(String),
    /// `--relation` does not apply to the given target.
    #[error("{0}")]
    BadRelation(String),
}

impl QueryError {
    /// Stable code for the CLI envelope.
    pub fn code(&self) -> &'static str {
        match self {
            QueryError::EmptyQuery => "E_EMPTY_QUERY",
            QueryError::Store(_) => "E_DB",
            QueryError::Render(err) => err.code(),
            QueryError::Hash(_) | QueryError::Path(_) => "E_IO",
            QueryError::PageNotFound(_) => "E_PAGE_NOT_FOUND",
            QueryError::PageAmbiguous(_) => "E_PAGE_AMBIGUOUS",
            QueryError::BadRelation(_) => "E_USAGE",
        }
    }
}

/// One page read once per refresh: the FTS fields and the relation input come
/// from the same bytes, so a page is never parsed twice in one round.
struct IndexedPage {
    key: String,
    digest: String,
    front: page::FrontMatter,
    body: String,
}

/// Rebuilds the whole index from `wiki/`, returning how many pages were indexed.
///
/// A page whose frontmatter does not parse is skipped, not fatal: `lint` is
/// where a broken page is reported, and a query command that refuses to answer
/// because one page is malformed is worse than one that leaves it out. FTS,
/// `page_meta` and the relation index are written in one transaction, so the
/// three cannot disagree about which pages exist.
pub fn reindex(root: &Path, cfg: &Config, store: &mut Store) -> Result<usize> {
    let mut pages = Vec::new();
    let mut rows: Vec<(String, String, String)> = Vec::new();
    let mut metas: Vec<PageMetaRow> = Vec::new();
    for (key, path) in page_paths(root, cfg)? {
        let Ok((front, body)) = page::read(&path) else {
            continue;
        };
        let digest = hash::hash_file(&path, true)?;
        rows.push((
            key.clone(),
            segment(&front.title),
            index_body(&front, &body),
        ));
        metas.push(PageMetaRow {
            page_key: key.clone(),
            hash: digest.clone(),
            updated: front.updated.to_string(),
        });
        pages.push(IndexedPage {
            key,
            digest,
            front,
            body,
        });
    }
    let indexed = rows.len();

    // Records for pages that no longer exist must not outlive the rebuild, or
    // the next `refresh` would keep believing they are indexed.
    let current: BTreeSet<&str> = metas.iter().map(|meta| meta.page_key.as_str()).collect();
    let meta_deletes: Vec<String> = store
        .all_page_metas()?
        .into_iter()
        .filter(|row| !current.contains(row.page_key.as_str()))
        .map(|row| row.page_key)
        .collect();
    let fts_deletes = store.fts_page_ids()?;
    let relations = relations(&cfg.paths.wiki, &cfg.paths.raw, &pages);
    store.apply_refresh(&RefreshWrite {
        fts_deletes,
        fts_upserts: rows,
        meta_deletes,
        metas,
        relation_deletes: Vec::new(),
        relation_pages: Vec::new(),
        full_relations: Some(relations),
    })?;
    Ok(indexed)
}

/// Indexes the pages that changed since the last write, returning how many
/// index rows changed.
///
/// The `page_meta` digest is the change detector for FTS; `page_nodes.content_hash`
/// is the same detector for relations, so a database upgraded from a build
/// without the relation index fills it in on the first run. A page whose id or
/// type changed, or a page appearing/disappearing, invalidates every resolution
/// (a bare id can become ambiguous), so those cases rebuild the whole relation
/// index; an ordinary body edit only rewrites that page's rows.
pub fn refresh(root: &Path, cfg: &Config, store: &mut Store) -> Result<usize> {
    let pages = page_paths(root, cfg)?;
    let known: BTreeMap<String, PageMetaRow> = store
        .all_page_metas()?
        .into_iter()
        .map(|row| (row.page_key.clone(), row))
        .collect();
    let current: BTreeSet<String> = pages.iter().map(|(key, _)| key.clone()).collect();
    // A `page_meta` row alone does not prove a page is searchable: publishing
    // through `approve`, `ingest submit` or `archive` records the digest
    // without touching the FTS table. The digest only counts when the index
    // also holds the page, or such a page would stay invisible until
    // a full rebuild.
    let indexed: BTreeSet<String> = store.fts_page_ids()?.into_iter().collect();
    let stored: BTreeMap<String, RelationNode> = store
        .all_relation_nodes()?
        .into_iter()
        .map(|node| (node.page_key.clone(), node))
        .collect();

    let mut upserts: Vec<(String, String, String)> = Vec::new();
    let mut metas: Vec<PageMetaRow> = Vec::new();
    let mut deletes: BTreeSet<String> = BTreeSet::new();
    let mut digests: BTreeMap<String, String> = BTreeMap::new();
    let mut changed: Vec<IndexedPage> = Vec::new();
    let mut unreadable: BTreeSet<String> = BTreeSet::new();
    let mut bad: BTreeSet<String> = BTreeSet::new();
    let mut alias_changed = false;

    for (key, path) in &pages {
        let digest = hash::hash_file(path, true)?;
        digests.insert(key.clone(), digest.clone());
        let meta_ok = known.get(key).is_some_and(|row| row.hash == digest) && indexed.contains(key);
        let node_ok = stored
            .get(key)
            .is_some_and(|node| node.content_hash == digest);
        if meta_ok && node_ok {
            continue;
        }
        let Ok((front, body)) = page::read(path) else {
            // A malformed page is no longer searchable, even if an older valid
            // version still has cached text. Leave no digest so a repaired page
            // is indexed again on the next refresh.
            if known.contains_key(key) || indexed.contains(key) {
                deletes.insert(key.clone());
            }
            bad.insert(key.clone());
            if stored.contains_key(key) {
                unreadable.insert(key.clone());
            }
            continue;
        };
        if stored
            .get(key)
            .is_some_and(|node| node.page_id != front.id || node.page_type != front.typ)
        {
            alias_changed = true;
        }
        upserts.push((
            key.clone(),
            segment(&front.title),
            index_body(&front, &body),
        ));
        metas.push(PageMetaRow {
            page_key: key.clone(),
            hash: digest.clone(),
            updated: front.updated.to_string(),
        });
        changed.push(IndexedPage {
            key: key.clone(),
            digest,
            front,
            body,
        });
    }

    // Anything the index knows but the disk does not is a page that was deleted
    // (or renamed, which is the same thing to a path-keyed index).
    deletes.extend(known.keys().filter(|key| !current.contains(*key)).cloned());
    for page_id in &indexed {
        if !current.contains(page_id) {
            deletes.insert(page_id.clone());
        }
    }

    let stored_keys: BTreeSet<String> = stored.keys().cloned().collect();
    let removed = stored_keys.difference(&current).next().is_some();
    // A readable page with no node is new, or predates the relation index; both
    // mean the alias table changed and every stored resolution may be stale.
    // An unreadable page is not "new": the rebuild leaves it out on purpose, so
    // treating it as new would rebuild the whole index on every round.
    let missing_node = current
        .iter()
        .any(|key| !stored.contains_key(key) && !bad.contains(key));
    let full = removed || missing_node || alias_changed || !unreadable.is_empty();
    let wiki_prefix = &cfg.paths.wiki;
    let (relation_pages, full_relations) = if full {
        let mut all = changed;
        let already: BTreeSet<String> = all.iter().map(|page| page.key.clone()).collect();
        for (key, path) in &pages {
            if already.contains(key) {
                continue;
            }
            match page::read(path) {
                Ok((front, body)) => {
                    let digest = digests.get(key).cloned().unwrap_or_default();
                    all.push(IndexedPage {
                        key: key.clone(),
                        digest,
                        front,
                        body,
                    });
                }
                Err(_) => {
                    if known.contains_key(key) || indexed.contains(key) {
                        deletes.insert(key.clone());
                    }
                }
            }
        }
        (
            Vec::new(),
            Some(relations(wiki_prefix, &cfg.paths.raw, &all)),
        )
    } else {
        let mut identities: BTreeMap<String, PageIdentity> = stored
            .iter()
            .map(|(key, node)| (key.clone(), identity_from_node(wiki_prefix, key, node)))
            .collect();
        for page in &changed {
            identities.insert(page.key.clone(), identity(wiki_prefix, page));
        }
        let ids: Vec<PageIdentity> = identities.into_values().collect();
        let resolver = Resolver::new(&ids);
        (
            relations_with(wiki_prefix, &cfg.paths.raw, &resolver, &changed),
            None,
        )
    };

    if upserts.is_empty()
        && deletes.is_empty()
        && relation_pages.is_empty()
        && full_relations.is_none()
    {
        return Ok(0);
    }

    let relation_deletes: Vec<String> = if full {
        Vec::new()
    } else {
        unreadable.into_iter().collect()
    };
    let changed_count =
        upserts.len() + deletes.len() + relation_pages.len() + relation_deletes.len();
    let deletes: Vec<String> = deletes.into_iter().collect();
    store.apply_refresh(&RefreshWrite {
        fts_deletes: deletes.clone(),
        fts_upserts: upserts,
        meta_deletes: deletes,
        metas,
        relation_deletes,
        relation_pages,
        full_relations,
    })?;
    Ok(changed_count)
}

/// Relation rows for every page, with resolutions against the whole set.
fn relations(wiki_prefix: &str, raw_prefix: &str, pages: &[IndexedPage]) -> Vec<PageRelations> {
    let ids: Vec<PageIdentity> = pages
        .iter()
        .map(|page| identity(wiki_prefix, page))
        .collect();
    let resolver = Resolver::new(&ids);
    relations_with(wiki_prefix, raw_prefix, &resolver, pages)
}

/// Relation rows for `pages`, resolved against a caller-built alias table.
fn relations_with(
    wiki_prefix: &str,
    raw_prefix: &str,
    resolver: &Resolver,
    pages: &[IndexedPage],
) -> Vec<PageRelations> {
    pages
        .iter()
        .map(|page| {
            let node = RelationNode {
                page_key: page.key.clone(),
                page_id: page.front.id.clone(),
                page_type: page.front.typ.clone(),
                title: page.front.title.clone(),
                summary: page.front.summary.clone(),
                status: page.front.status.as_str().to_string(),
                content_hash: page.digest.clone(),
            };
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut links = Vec::new();
            for literal in crate::relation::extract_links(&page.body) {
                if !seen.insert(literal.clone()) {
                    continue;
                }
                let (resolution, to_page) = resolver.resolve(&literal, wiki_prefix);
                if to_page.as_deref() == Some(page.key.as_str()) {
                    continue;
                }
                links.push(RelationLink {
                    from_page: page.key.clone(),
                    target_literal: literal,
                    to_page,
                    resolution: resolution.as_str().to_string(),
                });
            }
            let sources: Vec<RelationSource> = page
                .front
                .sources
                .iter()
                .filter_map(|entry| {
                    let parts = crate::relation::split_source_ref(entry)?;
                    let raw_key = crate::source_refs::source_key(&parts.raw_ref, raw_prefix)?;
                    Some(RelationSource {
                        page_key: page.key.clone(),
                        raw_ref: parts.raw_ref,
                        raw_key,
                        fragment: parts.fragment,
                    })
                })
                .collect();
            (node, links, sources)
        })
        .collect()
}

fn identity(wiki_prefix: &str, page: &IndexedPage) -> PageIdentity {
    PageIdentity {
        key: page.key.clone(),
        id: page.front.id.clone(),
        wiki_rel: wiki_rel(&page.key, wiki_prefix),
    }
}

fn identity_from_node(wiki_prefix: &str, key: &str, node: &RelationNode) -> PageIdentity {
    PageIdentity {
        key: String::from(key),
        id: node.page_id.clone(),
        wiki_rel: wiki_rel(key, wiki_prefix),
    }
}

/// Path under the wiki directory without `.md`, e.g. `concept/orders`.
fn wiki_rel(key: &str, wiki_prefix: &str) -> String {
    let prefix = wiki_prefix.trim_end_matches('/');
    key.strip_prefix(prefix)
        .unwrap_or(key)
        .trim_start_matches('/')
        .trim_end_matches(".md")
        .to_string()
}

/// Reads one page and one or both of its relation groups.
///
/// The reference is accepted in every spelling the resolver knows: full key,
/// `type/id`, or a unique bare id. The relation index must already be current;
/// the CLI refreshes before calling, and a direct caller gets the last snapshot
/// with its `generation`.
pub fn exact_page(
    root: &Path,
    cfg: &Config,
    store: &Store,
    reference: &str,
    kind: RelationKind,
    offset: usize,
    limit: usize,
) -> Result<RecallItem> {
    let nodes = store.all_relation_nodes()?;
    let by_key: BTreeMap<String, RelationNode> = nodes
        .iter()
        .map(|node| (node.page_key.clone(), node.clone()))
        .collect();
    let ids: Vec<PageIdentity> = nodes
        .iter()
        .map(|node| PageIdentity {
            key: node.page_key.clone(),
            id: node.page_id.clone(),
            wiki_rel: wiki_rel(&node.page_key, &cfg.paths.wiki),
        })
        .collect();
    let resolver = Resolver::new(&ids);
    let key = match resolver.resolve(reference, &cfg.paths.wiki) {
        (crate::relation::Resolution::Resolved, Some(key)) => key,
        (crate::relation::Resolution::Ambiguous, _) => {
            return Err(QueryError::PageAmbiguous(String::from(reference)));
        }
        _ => return Err(QueryError::PageNotFound(String::from(reference))),
    };
    let node = by_key
        .get(&key)
        .ok_or_else(|| QueryError::PageNotFound(String::from(reference)))?;
    let limit = if limit == 0 {
        RELATION_LIMIT_DEFAULT
    } else {
        limit.min(RELATION_LIMIT_MAX)
    };

    let outgoing = if matches!(kind, RelationKind::Outgoing | RelationKind::All) {
        attach_next(
            page_group(
                &store.relation_links_from(&key)?,
                &by_key,
                offset,
                limit,
                true,
            ),
            &format!(
                "llm-wiki query --page {} --relation outgoing",
                crate::output_context::shell_quote(reference)
            ),
        )
    } else {
        PageGroup::default()
    };
    let incoming = if matches!(kind, RelationKind::Incoming | RelationKind::All) {
        attach_next(
            page_group(
                &store.relation_links_to(&key)?,
                &by_key,
                offset,
                limit,
                false,
            ),
            &format!(
                "llm-wiki query --page {} --relation incoming",
                crate::output_context::shell_quote(reference)
            ),
        )
    } else {
        PageGroup::default()
    };
    let sources = if matches!(kind, RelationKind::Sources | RelationKind::All) {
        source_group(root, &cfg.paths.raw, &store.relation_sources(&key)?)
    } else {
        SourceGroup::default()
    };
    Ok(RecallItem {
        page: node.page_key.clone(),
        title: node.title.clone(),
        summary: node.summary.clone(),
        status: node.status.clone(),
        snippet: String::new(),
        outgoing,
        incoming,
        sources,
        generation: store.relation_generation()?,
        incomplete: false,
    })
}

/// Reads one raw reference and the pages citing it, paged.
///
/// A reference may be written with or without the raw directory prefix, and
/// with or without a fragment; an absent raw is an empty answer, not an error.
pub fn exact_source(
    root: &Path,
    cfg: &Config,
    store: &Store,
    reference: &str,
    offset: usize,
    limit: usize,
) -> Result<SourceView> {
    let parts = crate::relation::split_source_ref(reference)
        .ok_or_else(|| QueryError::BadRelation(String::from("source reference is empty")))?;
    let raw_key = crate::source_refs::source_key(&parts.raw_ref, &cfg.paths.raw)
        .ok_or_else(|| QueryError::BadRelation(format!("`{reference}` names no raw file")))?;
    let raw_path = root.join(format!(
        "{}/{}",
        cfg.paths.raw.trim_end_matches('/'),
        raw_key
    ));
    let available = raw_path.is_file();

    let nodes = store.all_relation_nodes()?;
    let by_key: BTreeMap<String, RelationNode> = nodes
        .iter()
        .map(|node| (node.page_key.clone(), node.clone()))
        .collect();
    let mut keys = store.pages_citing(&raw_key)?;
    keys.sort();

    let mut items: Vec<RecallPage> = keys
        .iter()
        .filter_map(|key| by_key.get(key))
        .map(|node| RecallPage {
            page: node.page_key.clone(),
            title: node.title.clone(),
            summary: node.summary.clone(),
            status: node.status.clone(),
        })
        .collect();
    items.sort_by(|a, b| a.page.cmp(&b.page));
    let limit = if limit == 0 {
        RELATION_LIMIT_DEFAULT
    } else {
        limit.min(RELATION_LIMIT_MAX)
    };
    let total = items.len();
    let start = offset.min(total);
    let end = (start + limit).min(total);
    let next_offset = if end < total { Some(end) } else { None };

    Ok(SourceView {
        raw: String::from(reference),
        raw_key,
        available,
        citing: attach_next(
            PageGroup {
                requested: true,
                total,
                offset,
                limit,
                next_offset,
                next_command: None,
                truncated: next_offset.is_some(),
                items: items[start..end].to_vec(),
                unresolved: Vec::new(),
            },
            &format!(
                "llm-wiki query --source {} --relation citing",
                crate::output_context::shell_quote(reference)
            ),
        ),
        generation: store.relation_generation()?,
    })
}

/// Slices one direction's links into a page group.
///
/// `unresolved` is never paginated: a reader asking for the next page of
/// neighbours should not be handed the same broken literals again.
fn page_group(
    links: &[RelationLink],
    nodes: &BTreeMap<String, RelationNode>,
    offset: usize,
    limit: usize,
    outbound: bool,
) -> PageGroup {
    let mut items: Vec<RecallPage> = Vec::new();
    let mut unresolved: Vec<Unresolved> = Vec::new();
    for link in links {
        match &link.to_page {
            Some(target) => {
                let key = if outbound { target } else { &link.from_page };
                if let Some(node) = nodes.get(key) {
                    items.push(RecallPage {
                        page: node.page_key.clone(),
                        title: node.title.clone(),
                        summary: node.summary.clone(),
                        status: node.status.clone(),
                    });
                }
            }
            None if outbound => unresolved.push(Unresolved {
                literal: link.target_literal.clone(),
                resolution: link.resolution.clone(),
            }),
            None => {}
        }
    }
    items.sort_by(|a, b| a.page.cmp(&b.page));
    items.dedup_by(|a, b| a.page == b.page);
    let total = items.len();
    let start = offset.min(total);
    let end = (start + limit).min(total);
    let next_offset = if end < total { Some(end) } else { None };
    PageGroup {
        requested: true,
        total,
        offset,
        limit,
        next_offset,
        next_command: None,
        truncated: next_offset.is_some(),
        items: items[start..end].to_vec(),
        unresolved,
    }
}

/// Fills `next_command` from `next_offset`; `prefix` is the command without the
/// offset, already quoted.
fn attach_next(mut group: PageGroup, prefix: &str) -> PageGroup {
    group.next_command = group
        .next_offset
        .map(|next| format!("{prefix} --offset {next}"));
    group
}

/// Joins indexed source references to the facts on disk.
fn source_group(root: &Path, raw_prefix: &str, sources: &[RelationSource]) -> SourceGroup {
    let items: Vec<RecallSource> = sources
        .iter()
        .map(|source| {
            let raw_dir = raw_prefix.trim_end_matches('/');
            let path = if raw_dir.is_empty() {
                root.join(&source.raw_key)
            } else {
                root.join(raw_dir).join(&source.raw_key)
            };
            let raw_available = path.is_file();
            let original = if raw_available {
                raw_source(&path)
            } else {
                None
            };
            RecallSource {
                read_command: format!(
                    "llm-wiki read {} --offset 1 --limit 200",
                    crate::output_context::shell_quote(&format!("raw:{}", source.raw_key))
                ),
                raw: source.raw_ref.clone(),
                raw_key: source.raw_key.clone(),
                source: original,
                fragment: source.fragment.clone(),
                raw_available,
            }
        })
        .collect();
    SourceGroup {
        requested: true,
        total: items.len(),
        items,
    }
}

/// Searches the index. Read-only: it never writes to the ledger.
///
/// Keeping the read path read-only is what lets the CLI decide when to pay for
/// an index update, and lets tests tell a stale index from a broken one. The
/// CLI refreshes before every search (a full rebuild is `doctor --fix`).
pub fn search(root: &Path, cfg: &Config, store: &Store, query: &SearchQuery) -> Result<Vec<Hit>> {
    let expression = match_expr(&query.text).ok_or(QueryError::EmptyQuery)?;
    let wiki = cfg.paths.wiki.trim_end_matches('/');
    let session_dir = cfg.ingest.session_dir.trim().trim_matches('/');
    // A page distilled from a source under `session_dir` keeps that prefix in
    // its id, so it lands at `<wiki>/<type>/<session_dir>/…`; a page an agent
    // writes straight into the directory lands at `<wiki>/<session_dir>/…`.
    // Both shapes have to be excluded, or `--no-archive` silently keeps
    // returning the distilled pages.
    let nested = format!("{wiki}/%/{session_dir}/%");
    let flat = format!("{wiki}/{session_dir}/%");
    let patterns = [flat.as_str(), nested.as_str()];
    let scope = match query.scope {
        Scope::All => FtsScope::All,
        Scope::ExcludingSessions if session_dir.is_empty() => FtsScope::All,
        Scope::ExcludingSessions => FtsScope::NotLike(&patterns),
    };

    let rows = store.fts_search(&expression, query.limit, scope)?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let nodes = store.all_relation_nodes()?;
    let by_key: BTreeMap<String, RelationNode> = nodes
        .iter()
        .map(|node| (node.page_key.clone(), node.clone()))
        .collect();
    let mut hits = Vec::new();
    for row in rows {
        let Some(node) = by_key.get(&row.page_id) else {
            continue;
        };
        let outgoing = attach_next(
            page_group(
                &store.relation_links_from(&node.page_key)?,
                &by_key,
                0,
                RELATION_LIMIT_DEFAULT,
                true,
            ),
            &format!(
                "llm-wiki query --page {} --relation outgoing",
                crate::output_context::shell_quote(&node.page_key)
            ),
        );
        let incoming = attach_next(
            page_group(
                &store.relation_links_to(&node.page_key)?,
                &by_key,
                0,
                RELATION_LIMIT_DEFAULT,
                false,
            ),
            &format!(
                "llm-wiki query --page {} --relation incoming",
                crate::output_context::shell_quote(&node.page_key)
            ),
        );
        let sources = source_group(
            root,
            &cfg.paths.raw,
            &store.relation_sources(&node.page_key)?,
        );
        let expanded = if query.expand == 0 {
            Vec::new()
        } else {
            outgoing
                .items
                .iter()
                .take(EXPANDED_LIMIT)
                .map(|page| ExpandedRef {
                    page: page.page.clone(),
                    title: page.title.clone(),
                    summary: page.summary.clone(),
                })
                .collect()
        };
        hits.push(Hit {
            page: node.page_key.clone(),
            title: node.title.clone(),
            summary: node.summary.clone(),
            status: node.status.clone(),
            score: row.score,
            snippet: unsegment(row.snippet.trim()),
            outgoing,
            incoming,
            sources,
            expanded,
        });
    }
    Ok(hits)
}

/// Every page the index covers: `wiki/**/*.md`, bookkeeping excluded, archived
/// summaries included.
fn page_paths(root: &Path, cfg: &Config) -> Result<Vec<(String, PathBuf)>> {
    let wiki = root.join(&cfg.paths.wiki);
    let mut out = Vec::new();
    for path in render::page_files(&wiki)? {
        out.push((paths::identity_key(root, &path)?, path));
    }
    Ok(out)
}

/// What one page contributes to the index: its title (indexed separately) is
/// handled by the caller; summary and body are concatenated and segmented.
fn index_body(front: &page::FrontMatter, body: &str) -> String {
    segment(&format!("{}\n{}", front.summary, body))
}

// ---------------------------------------------------------------------------
// Segmentation
// ---------------------------------------------------------------------------

/// Splits every CJK character out with a space around it, leaving everything
/// else alone.
///
/// `订单处理` becomes `订 单 处 理`. The index stores this form and the query
/// builds the same phrase from it, which is the only reason `unicode61` can
/// match a Chinese word at all.
pub fn segment(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 2);
    let mut previous: Option<char> = None;
    for c in text.chars() {
        if let Some(previous) = previous {
            // Separate every boundary that involves a CJK character: those are
            // the characters `unicode61` would otherwise glue together. Existing
            // whitespace is never doubled or moved.
            if !previous.is_whitespace() && !c.is_whitespace() && (is_cjk(previous) || is_cjk(c)) {
                out.push(' ');
            }
        }
        out.push(c);
        previous = Some(c);
    }
    out
}

/// Undoes [`segment`] on display text: drops the spaces between two CJK
/// characters, so a snippet reads 「订单处理」 instead of 「订 单 处 理」.
pub fn unsegment(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c == ' '
            && index > 0
            && index + 1 < chars.len()
            && is_cjk(chars[index - 1])
            && is_cjk(chars[index + 1])
        {
            index += 1;
            continue;
        }
        out.push(c);
        index += 1;
    }
    out
}

/// Whether a character is written without word spaces (Han, kana, Hangul).
///
/// These are the characters `unicode61` would glue into one token, so these are
/// the ones that must be split one by one.
fn is_cjk(c: char) -> bool {
    matches!(
        c as u32,
        0x1100..=0x11FF   // Hangul Jamo
        | 0x3040..=0x30FF // Hiragana and Katakana
        | 0x3400..=0x4DBF // CJK Unified Ideographs Extension A
        | 0x4E00..=0x9FFF // CJK Unified Ideographs
        | 0xAC00..=0xD7AF // Hangul Syllables
        | 0xF900..=0xFAFF // CJK Compatibility Ideographs
    )
}

/// Turns typed text into an FTS5 `MATCH` expression.
///
/// Every run of CJK characters becomes a phrase of single characters — the
/// shape [`segment`] gave the index — and every ASCII word or number becomes a
/// quoted term, because a bare FTS5 word cannot contain `-` or another operator
/// without changing the meaning. Runs are joined with `AND`.
///
/// `None` means the text held no term at all: whitespace and punctuation only.
fn match_expr(text: &str) -> Option<String> {
    let mut terms: Vec<String> = Vec::new();
    let mut run = String::new();
    let mut run_is_cjk = false;

    let close = |terms: &mut Vec<String>, run: &mut String, is_cjk: bool| {
        if run.is_empty() {
            return;
        }
        let term = if is_cjk { segment(run) } else { run.clone() };
        terms.push(format!("\"{term}\""));
        run.clear();
    };

    for c in text.chars() {
        if is_cjk(c) {
            if !run.is_empty() && !run_is_cjk {
                close(&mut terms, &mut run, run_is_cjk);
            }
            run_is_cjk = true;
            run.push(c);
        } else if c.is_alphanumeric() || c == '_' {
            if !run.is_empty() && run_is_cjk {
                close(&mut terms, &mut run, run_is_cjk);
            }
            run_is_cjk = false;
            run.push(c);
        } else if !run.is_empty() {
            close(&mut terms, &mut run, run_is_cjk);
        }
    }
    close(&mut terms, &mut run, run_is_cjk);

    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" AND "))
    }
}

/// Reads the `source:` field out of a raw file's frontmatter.
///
/// Raw files carry `source:` but no `updated:`, so [`page::read`] refuses them
/// by design; this one field is all a hit needs, read leniently.
fn raw_source(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "---" {
        return None;
    }
    for line in lines {
        let line = line.trim();
        if line == "---" {
            break;
        }
        let Some(value) = line.strip_prefix("source:") else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'').trim();
        if value.is_empty() {
            return None;
        }
        return Some(String::from(value));
    }
    None
}
