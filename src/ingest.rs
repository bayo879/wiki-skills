//! Ingest rounds: `begin` opens one, the agent writes candidates, `commit` or
//! `abort` closes it.
//!
//! The CLI never writes page prose. A round is a bookkeeping envelope around
//! work the agent does itself:
//!
//! 1. the agent writes candidates under `.wiki/staging/<type>/<id>.md`;
//! 2. [`begin`] records which sources the round reads and reports the pages
//!    those sources affect, so the agent knows what to open;
//! 3. the agent rewrites the candidates' prose;
//! 4. [`commit`] refreshes `updated`, merges the round's `raw/<key>` references
//!    into `sources`, regenerates the staging index, clears the round and
//!    stamps the manifest; [`abort`] drops the round and keeps the candidates.
//!
//! The open round lives in `.wiki/staging/pending.json` rather than sqlite
//! because it is review state, not ledger state: it is written and deleted by
//! the same commands that touch the staging directory, and a human can read it.
//!
//! Nothing here touches `wiki/`: approval is a separate, later step.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use chrono::Local;
use serde::Serialize;

use crate::config::Config;
use crate::hash;
use crate::page;
use crate::paths;
use crate::render;
use crate::staging;
use crate::store::{PageMetaRow, Store, StoreError};

/// What `begin` was asked to do.
#[derive(Debug, Clone, Default)]
pub struct BeginInput {
    /// Source keys to open the round for (source- or raw-relative).
    pub sources: Vec<String>,
    /// Pages the caller already knows are affected, merged into the scan.
    pub pages: Vec<String>,
    /// Report only: compute everything, write no `pending.json`.
    pub dry_run: bool,
}

/// One page a round touches.
#[derive(Debug, Clone, Serialize)]
pub struct PageRef {
    /// Page id: the frontmatter `id`, or the file stem when it is empty.
    pub id: String,
    /// Repository-relative path, empty when the page does not exist yet.
    pub path: String,
}

/// What `begin` found.
#[derive(Debug, Clone, Serialize)]
pub struct BeginReport {
    /// The round's sources as raw-relative keys, in the order given.
    pub source_keys: Vec<String>,
    /// Pages the round is expected to touch.
    pub affected: Vec<PageRef>,
    /// Affected pages that have no file yet.
    pub new_pages: usize,
    /// Pages citing a raw file the ledger does not know, sorted by id.
    pub orphan_pages: Vec<String>,
    /// Digest of the `pending.json` body this round writes or would write.
    pub pending_hash: String,
}

/// What `commit` published.
#[derive(Debug, Clone, Serialize)]
pub struct CommitReport {
    /// The pages it refreshed, as type-qualified ids.
    pub pages_written: Vec<String>,
    /// Whether the staging index was regenerated.
    pub index_updated: bool,
}

/// What `abort` dropped.
#[derive(Debug, Clone, Serialize)]
pub struct AbortReport {
    /// How many pending sources the closed round carried.
    pub cleared: usize,
}

/// Why an ingest verb refused.
#[derive(Debug, thiserror::Error)]
pub enum IngestError {
    /// `begin` was called without a source.
    #[error("ingest begin needs at least one `--sources` key")]
    NoSources,
    /// A named source is not in the ledger.
    #[error("source `{key}` is not in the ledger; scan and convert it first")]
    NoSource {
        /// The key as the caller wrote it.
        key: String,
    },
    /// `commit` was called without a page.
    #[error("ingest commit needs at least one `--pages` id")]
    NoPages,
    /// `commit` found no open round.
    #[error("no open ingest round; run `ingest begin` first")]
    NoPending,
    /// A named page has no candidate under staging.
    #[error("page `{id}` has no candidate under .wiki/staging/")]
    NoStagedPage {
        /// The id as the caller wrote it.
        id: String,
    },
    /// A bare page id names more than one candidate.
    #[error("page `{id}` matches several staged candidates: {candidates}")]
    AmbiguousStagedPage {
        /// The id as the caller wrote it.
        id: String,
        /// The competing type-qualified ids, comma separated.
        candidates: String,
    },
    /// The staging directory or its ledger is damaged.
    #[error(transparent)]
    Staging(#[from] staging::StagingError),
    /// The staging index could not be rebuilt.
    #[error(transparent)]
    Render(#[from] render::RenderError),
    /// A candidate page could not be read or rewritten.
    #[error(transparent)]
    Page(#[from] page::PageError),
    /// The ledger could not be read.
    #[error(transparent)]
    Db(#[from] StoreError),
    /// A candidate could not be digested.
    #[error(transparent)]
    Hash(#[from] hash::HashError),
    /// The wiki directory could not be walked.
    #[error("cannot walk {}: {source}", path.display())]
    Walk {
        /// Directory that failed.
        path: PathBuf,
        /// Underlying walk error.
        #[source]
        source: walkdir::Error,
    },
}

impl IngestError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            IngestError::NoSources | IngestError::NoSource { .. } => "E_NO_SOURCE",
            IngestError::NoPages => "E_NO_PAGES",
            IngestError::NoPending => "E_NO_PENDING",
            IngestError::NoStagedPage { .. } | IngestError::AmbiguousStagedPage { .. } => {
                "E_NO_STAGED_PAGE"
            }
            IngestError::Staging(err) => err.code(),
            IngestError::Render(err) => err.code(),
            IngestError::Page(err) => err.code(),
            IngestError::Db(_) => "E_DB",
            IngestError::Hash(_) => "E_IO",
            IngestError::Walk { .. } => "E_IO",
        }
    }
}

/// Result of the ingest verbs.
pub type Result<T> = std::result::Result<T, IngestError>;

/// One page under `wiki/`, with the raw keys its `sources` name.
struct WikiPage {
    /// Page id: the frontmatter `id`, or the file stem when it is empty.
    id: String,
    /// Repository-relative, slash-separated path.
    path: String,
    /// Path below the wiki directory, without the `.md` extension.
    rel: String,
    /// The page's `sources` as raw-relative keys.
    keys: Vec<String>,
}

impl WikiPage {
    /// Whether `id` names this page, by page id or by type-qualified path.
    fn matches(&self, id: &str) -> bool {
        let wanted = strip_md(id);
        self.id == wanted || self.rel == wanted
    }
}

/// Opens a round for `input.sources` and reports the pages it affects.
///
/// `affected` is the union of two answers: every page under `wiki/` whose
/// `sources` cite one of the round's sources, and every page the caller named
/// explicitly. The scan is a suggestion — `commit` only ever touches the pages
/// it is given — but it is what lets an agent find the pages it does not know
/// about. A named page without a file is counted in `new_pages` and reported
/// with an empty path: the type directory it will live in is not known yet.
///
/// `orphan_pages` reports, without fixing, pages citing a raw file the ledger
/// does not have: a page cannot be traced back to a converted source, which is
/// a lint problem, not a reason to refuse a round.
pub fn begin(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    input: &BeginInput,
) -> Result<BeginReport> {
    if input.sources.is_empty() {
        return Err(IngestError::NoSources);
    }

    let mut source_keys: Vec<String> = Vec::new();
    let mut pending: BTreeMap<String, String> = BTreeMap::new();
    for raw in &input.sources {
        let (key, digest) = resolve_source(store, cfg, raw)?;
        if !source_keys.iter().any(|seen| seen == &key) {
            source_keys.push(key.clone());
        }
        pending.insert(key, digest);
    }

    let wiki = wiki_pages(root, cfg)?;
    let wanted: BTreeSet<&str> = source_keys.iter().map(String::as_str).collect();
    let mut affected: Vec<PageRef> = Vec::new();
    for entry in &wiki {
        if entry.keys.iter().any(|key| wanted.contains(key.as_str())) {
            push_affected(
                &mut affected,
                PageRef {
                    id: entry.id.clone(),
                    path: entry.path.clone(),
                },
            );
        }
    }

    let mut new_pages = 0usize;
    for id in &input.pages {
        match wiki.iter().find(|entry| entry.matches(id)) {
            Some(entry) => push_affected(
                &mut affected,
                PageRef {
                    id: entry.id.clone(),
                    path: entry.path.clone(),
                },
            ),
            None => {
                let id = new_page_id(id);
                if affected.iter().any(|seen| seen.id == id) {
                    continue;
                }
                affected.push(PageRef {
                    id,
                    path: String::new(),
                });
                new_pages += 1;
            }
        }
    }

    let known: BTreeSet<String> = store.all_raws()?.into_iter().map(|row| row.key).collect();
    let mut orphan_pages: Vec<String> = Vec::new();
    for entry in &wiki {
        let orphaned = entry.keys.iter().any(|key| !known.contains(key));
        if orphaned && !orphan_pages.contains(&entry.id) {
            orphan_pages.push(entry.id.clone());
        }
    }
    orphan_pages.sort();

    let body = staging::encode_pending(&pending)?;
    let pending_hash = hash::hash_bytes(&body);
    if !input.dry_run {
        // The ledger comes before the pending list, and the first candidate
        // only after `begin` returns: without this, that candidate would look
        // like a staging directory whose ledger was lost.
        staging::Manifest::ensure(root)?;
        staging::write_pending(root, &pending)?;
    }

    Ok(BeginReport {
        source_keys,
        affected,
        new_pages,
        orphan_pages,
        pending_hash,
    })
}

/// Closes the open round: refreshes the named candidates and the staging index.
///
/// Per page, `updated` is stamped with the local date and every pending source
/// the page does not cite yet is appended to `sources` as `raw/<key>`; existing
/// order is kept, and a source already present (with or without a fragment) is
/// not duplicated. The staging index is rebuilt before the round closes, and
/// `pending.json` is deleted last: a failure anywhere above leaves the round
/// open and the whole commit retryable.
///
/// `wiki/` is never touched. Approval publishes, and that is a separate step.
pub fn commit(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    pages: &[String],
) -> Result<CommitReport> {
    if pages.is_empty() {
        return Err(IngestError::NoPages);
    }
    let pending = staging::pending_hashes(root)?;
    if pending.is_empty() {
        return Err(IngestError::NoPending);
    }

    let staging_dir = staging::dir(root);
    let staged = staged_pages(&staging_dir)?;
    let mut targets: Vec<(String, PathBuf)> = Vec::new();
    for id in pages {
        let target = find_staged(&staged, id)?;
        if !targets.iter().any(|(rel, _path)| rel == &target.0) {
            targets.push(target);
        }
    }

    let today = Local::now().date_naive();
    let prefix = clean_prefix(&cfg.paths.raw);
    let mut written: Vec<String> = Vec::new();
    for (rel, path) in &targets {
        page::write_updated(path, today)?;
        let (front, _body) = page::read(path)?;
        let present = staging::source_keys(&front.sources, prefix);
        let missing: Vec<String> = pending
            .keys()
            .filter(|key| !present.iter().any(|seen| seen == *key))
            .cloned()
            .collect();
        if !missing.is_empty() {
            let references: Vec<String> = missing
                .iter()
                .map(|key| raw_reference(prefix, key))
                .collect();
            page::append_sources(path, &references)?;
        }
        written.push(rel.clone());
    }

    // Record what the round wrote: `lint` compares these digests with the files
    // on disk to notice a body edit that never moved `updated`. Only the
    // candidates written above are recorded; the record hashes the bytes after
    // the date and the source references were stamped.
    let prefix = cfg.paths.wiki.trim_end_matches('/');
    for (rel, path) in &targets {
        store.upsert_page_meta(&PageMetaRow {
            page_key: format!("{prefix}/{rel}.md"),
            hash: hash::hash_file(path, true)?,
            updated: today.to_string(),
        })?;
    }

    render::write_index_in(&staging_dir, &cfg.schema.types)?;

    let stamp = today.format("%Y-%m-%d").to_string();
    let mut manifest = staging::Manifest::load(root)?;
    for key in pending.keys() {
        manifest
            .sources
            .entry(key.clone())
            .or_default()
            .last_processed_at = Some(stamp.clone());
    }
    staging::Manifest::save(root, &manifest)?;
    staging::clear_pending(root)?;

    Ok(CommitReport {
        pages_written: written,
        index_updated: true,
    })
}

/// Drops the open round, keeping the candidates already written.
///
/// The agent may still be mid-page when a round is abandoned, so the candidates
/// are left exactly as they are; a later round overwrites them by id. Closing a
/// round that was never opened is a no-op, not a failure: `cleared` is 0.
pub fn abort(root: &Path, _cfg: &Config, _store: &mut Store) -> Result<AbortReport> {
    let cleared = staging::pending_hashes(root)?.len();
    if cleared > 0 {
        staging::clear_pending(root)?;
    }
    Ok(AbortReport { cleared })
}

/// Resolves one `--sources` entry to its raw-relative key and ledger hash.
///
/// A source key is the identity the ledger is keyed by, so a PDF opens the round
/// as `a.pdf`; the pending list, the manifest and the page `sources` all speak
/// raw-relative keys, so the row's `raw_key` wins when it has one. A caller may
/// also pass the raw key directly (`raw/a.md`), which is accepted both with the
/// configured prefix and without it.
fn resolve_source(store: &Store, cfg: &Config, raw: &str) -> Result<(String, String)> {
    let prefix = clean_prefix(&cfg.paths.raw);
    let key = strip_prefix(raw, prefix);
    if let Some(row) = store.get_source(&key)? {
        let mapped = row
            .raw_key
            .as_deref()
            .map_or_else(|| key.clone(), |raw_key| strip_prefix(raw_key, prefix));
        return Ok((mapped, row.hash));
    }
    if let Some(row) = store.get_raw(&key)? {
        return Ok((key, row.hash));
    }
    Err(IngestError::NoSource {
        key: String::from(raw),
    })
}

/// Every page under the configured wiki directory.
fn wiki_pages(root: &Path, cfg: &Config) -> Result<Vec<WikiPage>> {
    let wiki = root.join(&cfg.paths.wiki);
    let prefix = clean_prefix(&cfg.paths.wiki);
    let raw_prefix = clean_prefix(&cfg.paths.raw);
    let mut out = Vec::new();
    for path in render::page_files(&wiki)? {
        let rel = path.strip_prefix(&wiki).unwrap_or(&path);
        let rel = String::from(strip_md(&paths::to_slash(rel)));
        let (front, _body) = page::read(&path)?;
        let id = if front.id.is_empty() {
            String::from(rel.rsplit('/').next().unwrap_or(&rel))
        } else {
            front.id
        };
        let path = if prefix.is_empty() {
            format!("{rel}.md")
        } else {
            format!("{prefix}/{rel}.md")
        };
        out.push(WikiPage {
            id,
            path,
            rel,
            keys: staging::source_keys(&front.sources, raw_prefix),
        });
    }
    Ok(out)
}

/// The candidate pages under staging, as `(type-qualified id, path)`.
fn staged_pages(staging_dir: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut out = Vec::new();
    for path in render::page_files(staging_dir)? {
        let rel = path.strip_prefix(staging_dir).unwrap_or(&path);
        out.push((String::from(strip_md(&paths::to_slash(rel))), path));
    }
    Ok(out)
}

/// Finds the one candidate `id` names.
///
/// An id may be bare (`retry`) or type-qualified (`concept/retry`). A bare id
/// that names candidates in two type directories is ambiguous, and guessing
/// would publish the wrong page, so the caller has to disambiguate.
fn find_staged(staged: &[(String, PathBuf)], id: &str) -> Result<(String, PathBuf)> {
    let wanted = strip_md(id);
    let mut hits: Vec<&(String, PathBuf)> = staged
        .iter()
        .filter(|(rel, _path)| rel.as_str() == wanted || page_stem(rel) == wanted)
        .collect();
    match hits.len() {
        0 => Err(IngestError::NoStagedPage {
            id: String::from(id),
        }),
        1 => Ok(hits.remove(0).clone()),
        _ => {
            let candidates = hits
                .iter()
                .map(|(rel, _path)| rel.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            Err(IngestError::AmbiguousStagedPage {
                id: String::from(id),
                candidates,
            })
        }
    }
}

/// Appends `page` unless a page with the same id is already listed.
fn push_affected(affected: &mut Vec<PageRef>, page: PageRef) {
    if affected.iter().any(|seen| seen.id == page.id) {
        return;
    }
    affected.push(page);
}

/// The page id inside a caller-written id: `concept/retry.md` → `retry`.
fn new_page_id(id: &str) -> String {
    let id = strip_md(id);
    String::from(id.rsplit('/').next().unwrap_or(id))
}

/// The file stem of a type-qualified id.
fn page_stem(rel: &str) -> &str {
    rel.rsplit('/').next().unwrap_or(rel)
}

/// Drops a trailing `.md`, so a caller may name a page by its file.
fn strip_md(value: &str) -> &str {
    value.strip_suffix(".md").unwrap_or(value)
}

/// A configured directory prefix without `./` decoration or a trailing slash.
fn clean_prefix(value: &str) -> &str {
    value.trim_start_matches("./").trim_end_matches('/')
}

/// Strips the configured raw prefix from a key, leaving bare keys alone.
fn strip_prefix(value: &str, prefix: &str) -> String {
    let mut text = value.trim_start_matches("./");
    if !prefix.is_empty() {
        if let Some(rest) = text
            .strip_prefix(prefix)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            text = rest;
        }
    }
    String::from(text)
}

/// A raw key as the repository-relative reference a page cites.
fn raw_reference(prefix: &str, key: &str) -> String {
    if prefix.is_empty() {
        String::from(key)
    } else {
        format!("{prefix}/{key}")
    }
}
