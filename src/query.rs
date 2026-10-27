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

use crate::archive;
use crate::config::Config;
use crate::hash;
use crate::page;
use crate::paths;
use crate::render;
use crate::store::{FtsHit, FtsScope, PageMetaRow, Store, StoreError};

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
    /// Whether the page sits under `wiki/archive/`.
    pub archived: bool,
    /// The command that opens an archived summary's full body.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recall: Option<String>,
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

/// Which pages a search may return. The index always contains both halves;
/// the scope only filters the query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// Live pages and archived summaries.
    All,
    /// Live pages only.
    Wiki,
    /// Archived summaries only.
    Archive,
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

/// Rebuilds the whole index from `wiki/`, returning how many pages were indexed.
///
/// A page whose frontmatter does not parse is skipped, not fatal: `lint` is
/// where a broken page is reported, and a query command that refuses to answer
/// because one page is malformed is worse than one that leaves it out. The
/// index and its `page_meta` records are written together, so the next
/// [`refresh`] agrees with what was indexed.
pub fn reindex(root: &Path, cfg: &Config, store: &mut Store) -> Result<usize> {
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
            page_key: key,
            hash: digest,
            updated: front.updated.to_string(),
        });
    }
    let pages = rows.len();
    store.fts_replace_all(&rows)?;

    // Records for pages that no longer exist must not outlive the rebuild, or
    // the next `refresh` would keep believing they are indexed.
    let current: BTreeSet<&str> = metas.iter().map(|meta| meta.page_key.as_str()).collect();
    for row in store.all_page_metas()? {
        if !current.contains(row.page_key.as_str()) {
            store.delete_page_meta(&row.page_key)?;
        }
    }
    for meta in &metas {
        store.upsert_page_meta(meta)?;
    }
    Ok(pages)
}

/// Indexes the pages that changed since the last write, returning how many
/// index rows changed.
///
/// The `page_meta` digest from the last write is the change detector: an
/// untouched page is hashed but not read, so a repeat run does no database
/// write at all. Pages that disappeared from disk lose their index row (and
/// their stale hits with it).
pub fn refresh(root: &Path, cfg: &Config, store: &mut Store) -> Result<usize> {
    let pages = page_paths(root, cfg)?;
    let known: BTreeMap<String, PageMetaRow> = store
        .all_page_metas()?
        .into_iter()
        .map(|row| (row.page_key.clone(), row))
        .collect();
    let current: BTreeSet<String> = pages.iter().map(|(key, _)| key.clone()).collect();

    let mut upserts: Vec<(String, String, String)> = Vec::new();
    let mut metas: Vec<PageMetaRow> = Vec::new();
    for (key, path) in &pages {
        let digest = hash::hash_file(path, true)?;
        if known.get(key).is_some_and(|row| row.hash == digest) {
            continue;
        }
        let Ok((front, body)) = page::read(path) else {
            continue;
        };
        upserts.push((
            key.clone(),
            segment(&front.title),
            index_body(&front, &body),
        ));
        metas.push(PageMetaRow {
            page_key: key.clone(),
            hash: digest,
            updated: front.updated.to_string(),
        });
    }

    // Anything the index knows but the disk does not is a page that was deleted
    // (or renamed, which is the same thing to a path-keyed index).
    let mut deletes: BTreeSet<String> = known
        .keys()
        .filter(|key| !current.contains(*key))
        .cloned()
        .collect();
    for page_id in store.fts_page_ids()? {
        if !current.contains(&page_id) {
            deletes.insert(page_id);
        }
    }
    let deletes: Vec<String> = deletes.into_iter().collect();

    if upserts.is_empty() && deletes.is_empty() {
        return Ok(0);
    }
    store.fts_apply(&deletes, &upserts)?;
    for key in &deletes {
        store.delete_page_meta(key)?;
    }
    for meta in &metas {
        store.upsert_page_meta(meta)?;
    }
    Ok(upserts.len() + deletes.len())
}

/// Searches the index. Read-only: it never writes to the ledger.
///
/// Keeping the read path read-only is what lets the CLI decide when to pay for
/// an index update, and lets tests tell a stale index from a broken one. The
/// CLI refreshes before every search (`query --reindex` rebuilds instead).
pub fn search(root: &Path, cfg: &Config, store: &Store, query: &SearchQuery) -> Result<Vec<Hit>> {
    let expression = match_expr(&query.text).ok_or(QueryError::EmptyQuery)?;
    let wiki = cfg.paths.wiki.trim_end_matches('/');
    let archive_glob = format!("{wiki}/{}/%", archive::DIR);
    let scope = match query.scope {
        Scope::All => FtsScope::All,
        Scope::Wiki => FtsScope::NotLike(&archive_glob),
        Scope::Archive => FtsScope::Like(&archive_glob),
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
    /// Frontmatter `id`.
    id: String,
    /// Frontmatter `title`.
    title: String,
    /// Frontmatter `summary`.
    summary: String,
    /// Frontmatter `status`, as a string.
    status: String,
    /// Why the page is here, in case someone asks.
    sources: Vec<String>,
    /// Whether the page lives under `wiki/archive/`.
    archived: bool,
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
            let archived = rel.starts_with(&format!("{}/", archive::DIR));
            let id = if front.id.is_empty() {
                String::from(stem)
            } else {
                front.id
            };
            targets.push((key.clone(), wikilinks(&body)));
            pages.insert(
                key,
                PageInfo {
                    id,
                    title: front.title,
                    summary: front.summary,
                    status: front.status.as_str().to_string(),
                    sources: front.sources,
                    archived,
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
        let (title, summary, status, archived, recall) = match page {
            Some(page) => (
                page.title.clone(),
                page.summary.clone(),
                page.status.clone(),
                page.archived,
                page.archived
                    .then(|| format!("llm-wiki recall {}", page.id)),
            ),
            // The index row outlived its file: report what the index knows
            // rather than dropping a hit the user can still act on.
            None => (
                row.title.clone(),
                String::new(),
                String::from("draft"),
                false,
                None,
            ),
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
            archived,
            recall,
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
/// A missing raw is a fact, not an error: `raw/` is committed but the files a
/// clone might not have are unrelated to the query, and the hit is still worth
/// returning with `raw_available: false`.
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
