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
use crate::render;
use crate::relation::{PageIdentity, Resolver};
use crate::store::{
    FtsHit, FtsScope, PageMetaRow, RefreshWrite, RelationLink, RelationNode, RelationSource,
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
    /// Neighbour pages the body links to, with their titles.
    pub links: Vec<LinkRef>,
    /// Source references from the frontmatter, with the chunk split out.
    pub sources: Vec<SourceRef>,
    /// How many other pages link to this one.
    pub backlinks: usize,
    /// One-hop neighbour summaries, only filled when `--expand` is at least 1.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub expanded: Vec<ExpandedRef>,
}

/// One neighbour linked from a hit's body.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LinkRef {
    /// Repository-relative path of the neighbour page.
    pub to: String,
    /// The neighbour's title.
    pub title: String,
}

/// One `raw/...` reference from a hit's frontmatter.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceRef {
    /// Repository-relative raw path, such as `raw/订单系统设计.md`.
    pub raw: String,
    /// The original source the raw file was converted from, if recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// The fragment of the reference, such as `p=12-40`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chunk: Option<String>,
    /// Whether the raw file exists on disk right now.
    pub raw_available: bool,
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
}

impl QueryError {
    /// Stable code for the CLI envelope.
    pub fn code(&self) -> &'static str {
        match self {
            QueryError::EmptyQuery => "E_EMPTY_QUERY",
            QueryError::Store(_) => "E_DB",
            QueryError::Render(err) => err.code(),
            QueryError::Hash(_) | QueryError::Path(_) => "E_IO",
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
    let relations = relations(&cfg.paths.wiki, &pages);
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
        (Vec::new(), Some(relations(wiki_prefix, &all)))
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
        (relations_with(wiki_prefix, &resolver, &changed), None)
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
    let changed_count = upserts.len()
        + deletes.len()
        + relation_pages.len()
        + relation_deletes.len();
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
fn relations(
    wiki_prefix: &str,
    pages: &[IndexedPage],
) -> Vec<(RelationNode, Vec<RelationLink>, Vec<RelationSource>)> {
    let ids: Vec<PageIdentity> = pages.iter().map(|page| identity(wiki_prefix, page)).collect();
    let resolver = Resolver::new(&ids);
    relations_with(wiki_prefix, &resolver, pages)
}

/// Relation rows for `pages`, resolved against a caller-built alias table.
fn relations_with(
    wiki_prefix: &str,
    resolver: &Resolver,
    pages: &[IndexedPage],
) -> Vec<(RelationNode, Vec<RelationLink>, Vec<RelationSource>)> {
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
                .filter_map(|entry| crate::relation::split_source_ref(entry))
                .map(|parts| RelationSource {
                    page_key: page.key.clone(),
                    raw_ref: parts.raw_ref,
                    raw_key: parts.raw_key,
                    fragment: parts.fragment,
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
    let index = PageIndex::scan(root, cfg)?;
    Ok(rows
        .into_iter()
        .map(|row| index.hit(root, row, query.expand))
        .collect())
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

// ---------------------------------------------------------------------------
// Page scan: links, backlinks, titles
// ---------------------------------------------------------------------------

/// Every page on disk, read once per search.
///
/// The index knows the matched page; it does not know which pages link to it or
/// what their titles are, and it never will — those are relations between
/// files. One scan of `wiki/` turns them into real paths, and a dead `[[link]]`
/// simply does not resolve, so it never leaves the library.
struct PageIndex {
    /// Pages by repository-relative key.
    pages: BTreeMap<String, PageInfo>,
    /// Fan-in: how many pages link to each key.
    backlinks: BTreeMap<String, usize>,
}

/// The parts of a page a hit needs beyond the index.
struct PageInfo {
    /// Frontmatter `title`.
    title: String,
    /// Frontmatter `summary`.
    summary: String,
    /// Frontmatter `status`, as a string.
    status: String,
    /// Why the page is here, in case someone asks.
    sources: Vec<String>,
    /// Resolved outgoing links, deduplicated, in body order.
    links: Vec<String>,
}

impl PageIndex {
    /// Reads `wiki/` and resolves every link between its pages.
    fn scan(root: &Path, cfg: &Config) -> Result<PageIndex> {
        let mut pages: BTreeMap<String, PageInfo> = BTreeMap::new();
        let mut aliases: BTreeMap<String, String> = BTreeMap::new();
        let mut targets: Vec<(String, Vec<String>)> = Vec::new();
        let wiki_prefix = format!("{}/", cfg.paths.wiki.trim_end_matches('/'));

        for (key, path) in page_paths(root, cfg)? {
            let rel = key.strip_prefix(&wiki_prefix).unwrap_or(&key).to_string();
            let stem = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .unwrap_or_default();
            for alias in [
                key.clone(),
                key.trim_end_matches(".md").to_string(),
                rel.clone(),
                rel.trim_end_matches(".md").to_string(),
                String::from(stem),
            ] {
                if !alias.is_empty() {
                    aliases.insert(alias, key.clone());
                }
            }

            let Ok((front, body)) = page::read(&path) else {
                continue;
            };
            // The frontmatter id is the canonical name, even when it drifted
            // away from the file stem.
            if !front.id.is_empty() {
                aliases.insert(front.id.clone(), key.clone());
            }
            targets.push((key.clone(), wikilinks(&body)));
            pages.insert(
                key,
                PageInfo {
                    title: front.title,
                    summary: front.summary,
                    status: front.status.as_str().to_string(),
                    sources: front.sources,
                    links: Vec::new(),
                },
            );
        }

        // Resolve after the scan: a link may point at a page read later.
        let mut backlinks: BTreeMap<String, usize> = BTreeMap::new();
        for (key, raw_targets) in &targets {
            let mut seen: BTreeSet<String> = BTreeSet::new();
            let mut links: Vec<String> = Vec::new();
            for target in raw_targets {
                let Some(to) = resolve(&aliases, &wiki_prefix, target) else {
                    continue;
                };
                if !pages.contains_key(&to) || !seen.insert(to.clone()) {
                    continue;
                }
                links.push(to);
            }
            if let Some(page) = pages.get_mut(key) {
                page.links = links.clone();
            }
            for to in links {
                *backlinks.entry(to).or_insert(0) += 1;
            }
        }

        Ok(PageIndex { pages, backlinks })
    }

    /// Completes one index row into a [`Hit`].
    fn hit(&self, root: &Path, row: FtsHit, expand: usize) -> Hit {
        let page = self.pages.get(&row.page_id);
        let mut links: Vec<LinkRef> = Vec::new();
        let mut expanded: Vec<ExpandedRef> = Vec::new();
        if let Some(page) = page {
            for to in &page.links {
                let Some(neighbour) = self.pages.get(to) else {
                    continue;
                };
                links.push(LinkRef {
                    to: to.clone(),
                    title: neighbour.title.clone(),
                });
                if expand >= 1 && expanded.len() < EXPANDED_LIMIT {
                    expanded.push(ExpandedRef {
                        page: to.clone(),
                        title: neighbour.title.clone(),
                        summary: neighbour.summary.clone(),
                    });
                }
            }
        }

        let sources: &[String] = page.map(|page| page.sources.as_slice()).unwrap_or_default();
        let (title, summary, status) = match page {
            Some(page) => (
                page.title.clone(),
                page.summary.clone(),
                page.status.clone(),
            ),
            // The index row outlived its file: report what the index knows
            // rather than dropping a hit the user can still act on.
            None => (row.title.clone(), String::new(), String::from("draft")),
        };

        Hit {
            page: row.page_id.clone(),
            title,
            summary,
            status,
            score: row.score,
            snippet: unsegment(row.snippet.trim()),
            links,
            sources: source_refs(root, sources),
            backlinks: self.backlinks.get(&row.page_id).copied().unwrap_or(0),
            expanded,
        }
    }
}

/// Resolves one `[[target]]` to a page key, if the page exists.
fn resolve(aliases: &BTreeMap<String, String>, wiki_prefix: &str, target: &str) -> Option<String> {
    let target = target.split('#').next().unwrap_or_default().trim();
    let target = target.strip_prefix("./").unwrap_or(target);
    let target = target.strip_prefix(wiki_prefix).unwrap_or(target);
    let target = target.trim_end_matches(".md");
    if target.is_empty() {
        return None;
    }
    aliases.get(target).cloned()
}

/// Every `[[target]]` in a body, in order, with fenced code blocks ignored.
fn wikilinks(body: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut fence: Option<&str> = None;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if let Some(marker) = fence {
            if trimmed.starts_with(marker) {
                fence = None;
            }
            continue;
        }
        if let Some(marker) = fence_marker(trimmed) {
            fence = Some(marker);
            continue;
        }
        let mut rest = line;
        while let Some(open) = rest.find("[[") {
            let after = &rest[open + 2..];
            let Some(close) = after.find("]]") else {
                break;
            };
            let target = after[..close].trim();
            if !target.is_empty() {
                out.push(String::from(target));
            }
            rest = &after[close + 2..];
        }
    }
    out
}

/// The fence marker a line opens, if it opens one.
fn fence_marker(trimmed: &str) -> Option<&'static str> {
    if trimmed.starts_with("```") {
        Some("```")
    } else if trimmed.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// Sources
// ---------------------------------------------------------------------------

/// Splits the frontmatter `sources` entries into raw path and chunk, and stats
/// each raw file.
///
/// A missing raw is a fact, not an error: `.wiki/` is not shared between
/// machines, so a raw another checkout has may be absent here, and the hit is
/// still worth returning with `raw_available: false`.
fn source_refs(root: &Path, sources: &[String]) -> Vec<SourceRef> {
    sources
        .iter()
        .map(|entry| {
            let entry = entry.trim();
            let (raw, chunk) = match entry.split_once('#') {
                Some((raw, chunk)) => (
                    raw.trim(),
                    Some(String::from(chunk.trim())).filter(|chunk| !chunk.is_empty()),
                ),
                None => (entry, None),
            };
            let path = root.join(raw);
            let raw_available = path.is_file();
            let source = if raw_available {
                raw_source(&path)
            } else {
                None
            };
            SourceRef {
                raw: String::from(raw),
                source,
                chunk,
                raw_available,
            }
        })
        .collect()
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
