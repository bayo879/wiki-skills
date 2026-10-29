//! Version snapshots and the four-way decision gate.
//!
//! `.wiki/pages/` and `.wiki/raw/` are the current content; `.wiki/versions/vNNN/`
//! holds the history. A snapshot records the path and the digest of every
//! content file, and stores each distinct byte sequence once under
//! `.wiki/objects/`. Two snapshots that differ in one page therefore cost one
//! page, not two full copies, and `rollback` is a plain materialisation of a
//! manifest — no git, no re-derivation, and no way for an edit of the live tree
//! to rewrite history.
//!
//! The index is derived and never versioned: a rollback rebuilds it from the
//! restored pages, so a version cannot disagree with what `query` returns.
//!
//! The decision gate has four outcomes. `approve` publishes the staged
//! candidates and advances the baseline, `reject` throws them away, records
//! why, and settles the round's sources at the bytes on disk, `instruct` leaves free text for the next agent session, and `defer`
//! keeps the queue for later. A merge is only allowed while every candidate
//! still matches what `ingest submit` wrote and every pending source still
//! matches the hash the round recorded: a hand edit or a changed source makes
//! the merge unsafe, so it is refused instead of silently published.
//!
//! The snapshot directory is fixed at `.wiki/versions` because the engine
//! location is part of the repository layout that `init` creates, matching how
//! [`crate::staging`] locates its directory.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::Serialize;
use walkdir::WalkDir;

use crate::config::Config;
use crate::hash;
use crate::journal;
use crate::ledger::Ledger;
use crate::paths;
use crate::render;
use crate::staging::{self, Manifest};
use crate::store::{SourceRow, Store};

/// Marker file that makes a directory in `.wiki/versions` ours to prune.
///
/// A directory without it may be a hand-made backup, and is never deleted.
const VERSION_FILE: &str = ".version.json";

/// The content map of one snapshot: what each path held, by digest.
const MANIFEST_FILE: &str = "manifest.json";

/// Content-addressed store shared by every snapshot.
const OBJECTS_DIR: &str = "objects";

/// Prefix a snapshot path carries for a page.
const PAGES_PREFIX: &str = "pages";

/// Prefix a snapshot path carries for a raw file.
const RAW_PREFIX: &str = "raw";

/// The engine directory that holds snapshots and the pending instruction.
const ENGINE: &str = ".wiki";

/// A candidate the brakes kept in staging, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HeldBack {
    /// Page id, relative to the wiki root (`concept/alpha.md`).
    pub page: String,
    /// Stable reason code: `page_overlap`, `page_slice_budget`, `page_budget`.
    pub reason: String,
    /// Human-readable explanation.
    pub detail: String,
}

/// What a call to [`approve`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApproveOutcome {
    /// Some candidates were published and a new version now exists.
    Applied {
        /// Name of the version the merge produced.
        version: String,
        /// Pages that landed in `wiki/`.
        published: Vec<String>,
        /// Pages the brakes kept in staging.
        held_back: Vec<HeldBack>,
    },
    /// Every candidate was held back, so nothing was published.
    NothingPublished {
        /// Pages the brakes kept in staging.
        held_back: Vec<HeldBack>,
    },
}

/// Anything that can go wrong while finishing a decision.
#[derive(Debug, thiserror::Error)]
pub enum VersionError {
    /// A filesystem operation failed.
    #[error("cannot access {}: {source}", path.display())]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// A directory could not be walked.
    #[error("cannot walk {}: {source}", path.display())]
    Walk {
        /// Directory that failed.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// A page path could not be turned into an identity key.
    #[error(transparent)]
    Path(#[from] paths::PathError),
    /// A candidate no longer matches what ingest recorded for it.
    #[error("staging page {page} changed after it was written; refusing to merge")]
    Drift {
        /// Page path relative to the wiki root.
        page: String,
    },
    /// A source changed after the round that produced the candidates.
    #[error("source {source_key} changed after it was processed; refusing to merge")]
    Stale {
        /// Source key relative to the raw directory.
        source_key: String,
    },
    /// Staging mirrors the wiki, so there is nothing to publish.
    #[error("there is nothing to approve: staging mirrors wiki/")]
    NothingToApprove,
    /// A reject without a reason cannot inform anything.
    #[error("a reject needs a reason")]
    NoReason,
    /// An instruction without text cannot be followed.
    #[error("an instruction needs text")]
    NoText,
    /// The snapshot record could not be encoded.
    #[error("cannot encode {}: {source}", path.display())]
    Encode {
        /// Record path that could not be written.
        path: PathBuf,
        /// Encoding error.
        #[source]
        source: serde_json::Error,
    },
    /// The staging ledger could not be read or written.
    #[error(transparent)]
    Staging(#[from] staging::StagingError),
    /// The index could not be rebuilt after a rollback.
    #[error(transparent)]
    Index(#[from] crate::query::QueryError),
    /// A snapshot named on the command line does not exist.
    #[error("there is no version named {name}")]
    NoVersion {
        /// The name the caller asked for.
        name: String,
    },
    /// A snapshot exists but carries no content manifest.
    #[error("version {name} has no content manifest; take a new `checkpoint`")]
    NoManifest {
        /// The name the caller asked for.
        name: String,
    },
    /// A manifest names an object the store no longer holds.
    #[error("object {sha} is missing from .wiki/objects; version {name} cannot be restored")]
    MissingObject {
        /// The version that referenced it.
        name: String,
        /// Digest of the missing object.
        sha: String,
    },
    /// The engine journal could not be appended to.
    #[error(transparent)]
    Journal(#[from] crate::journal::JournalError),
    /// The index could not be regenerated.
    #[error(transparent)]
    Render(#[from] render::RenderError),
    /// The ledger could not be read or written.
    #[error(transparent)]
    Db(#[from] crate::store::StoreError),
    /// A file could not be hashed.
    #[error(transparent)]
    Hash(#[from] hash::HashError),
}

impl VersionError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            VersionError::Io { .. }
            | VersionError::Walk { .. }
            | VersionError::Path(_)
            | VersionError::Encode { .. }
            | VersionError::Hash(_) => "E_IO",
            VersionError::Drift { .. } => "E_STAGING_DRIFT",
            VersionError::Stale { .. } => "E_STAGING_STALE",
            VersionError::NothingToApprove => "E_NOTHING_TO_APPROVE",
            VersionError::NoReason => "E_NO_REASON",
            VersionError::NoText => "E_NO_TEXT",
            VersionError::Staging(err) => err.code(),
            VersionError::Index(err) => err.code(),
            VersionError::NoVersion { .. } | VersionError::NoManifest { .. } => "E_NO_VERSION",
            VersionError::MissingObject { .. } => "E_MISSING_OBJECT",
            VersionError::Journal(err) => err.code(),
            VersionError::Render(err) => err.code(),
            VersionError::Db(_) => "E_DB",
        }
    }
}

/// Result of the decision helpers.
pub type Result<T> = std::result::Result<T, VersionError>;

/// The record written into every snapshot.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
struct VersionRecord {
    /// Why the snapshot was taken; free text.
    label: String,
    /// UTC creation time, `YYYY-MM-DDTHH:MM:SSZ`.
    created_at: String,
    /// Number of pages in the snapshot, bookkeeping excluded.
    pages: usize,
    /// `checkpoint`, `approve` or `rollback`; defaults for older snapshots.
    #[serde(default)]
    kind: String,
}

/// One staged page that differs from its counterpart in `wiki/`.
struct Candidate {
    /// Path relative to the wiki root, `/`-separated and NFC.
    rel: String,
    /// The staged file.
    staging: PathBuf,
    /// Where the page lands in `wiki/`.
    wiki: PathBuf,
}

/// A published page, reduced to the facts the overlap rule compares.
///
/// Overlap is deliberately mechanical: it never reads the prose. A candidate
/// whose title says the same thing as a page already in the wiki, or which
/// cites only sources the wiki already cites, is almost certainly the same page
/// written twice under two ids — the failure mode a page budget cannot see,
/// because writing it was cheap.
struct Published {
    /// Path relative to the wiki root, `/`-separated (`concept/alpha.md`).
    rel: String,
    /// Case-, space- and punctuation-insensitive title.
    title: String,
    /// Raw keys the page cites, raw-relative.
    sources: BTreeSet<String>,
    /// The `sources` entries verbatim, fragments included.
    refs: Vec<String>,
}

/// A staged candidate, reduced to the facts the brakes compare.
struct Staged {
    /// Case-, space- and punctuation-insensitive title.
    title: String,
    /// The `sources` entries verbatim, fragments included.
    sources: Vec<String>,
}

/// Case-, space- and punctuation-insensitive title key.
///
/// `Retry Policy`, `retry-policy` and `retry_policy` all collapse to
/// `retrypolicy`, so a rename that only changes punctuation still collides.
fn title_key(title: &str) -> String {
    title
        .chars()
        .filter(|ch| ch.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// The raw keys a page cites, as raw-relative keys.
fn cited_sources(cfg: &Config, sources: &[String]) -> BTreeSet<String> {
    staging::source_keys(sources, &cfg.paths.raw)
        .into_iter()
        .collect()
}

/// One unit of source information: a chunk of a raw, or the whole raw.
///
/// Chunk `0` stands for the whole file. A page names a chunk with `#p=<n>`, so
/// the budget follows the text rather than the file count: a long document is
/// cut into more slices and earns proportionally more pages, a short one is a
/// single slice.
type Slice = (String, usize);

/// Every slice a page's `sources` point at.
fn page_slices(root: &Path, cfg: &Config, sources: &[String]) -> BTreeSet<Slice> {
    let mut out = BTreeSet::new();
    for source in sources {
        let Some(key) = staging::source_key(source, &cfg.paths.raw) else {
            continue;
        };
        // A file below the chunk threshold is one slice whatever fragment a
        // page names: otherwise a fragment invented on a one-page note would
        // mint slices that do not exist.
        if !is_chunked(root, cfg, &key) {
            out.insert((key, 0));
            continue;
        }
        match source
            .split_once('#')
            .and_then(|(_, fragment)| fragment.trim().strip_prefix("p="))
            .and_then(parse_chunk_range)
        {
            // A page claiming one absurd span is treated as citing the whole
            // file, so nobody inflates their share with `#p=1-999999`.
            Some((from, to)) if to.saturating_sub(from) <= MAX_SLICE_SPAN => {
                for index in from..=to {
                    out.insert((key.clone(), index));
                }
            }
            _ => {
                out.insert((key, 0));
            }
        }
    }
    out
}

/// Whether `key` is large enough that `parse` cuts it into chunks.
///
/// A raw that is missing or unreadable is not chunked, which counts every
/// fragment as the whole file — the stricter reading.
fn is_chunked(root: &Path, cfg: &Config, key: &str) -> bool {
    let path = paths::join_key(&root.join(&cfg.paths.raw), key);
    fs::metadata(&path)
        .map(|meta| meta.len() > cfg.parse.chunk_threshold)
        .unwrap_or(false)
}

/// Widest `#p=<a>-<b>` span still read as a chunk list rather than the whole file.
const MAX_SLICE_SPAN: usize = 4096;

/// `12-40` -> `(12, 40)`, `3` -> `(3, 3)`; one-based, `None` when malformed.
fn parse_chunk_range(range: &str) -> Option<(usize, usize)> {
    let (from, to) = range.split_once('-').unwrap_or((range, range));
    let from: usize = from.trim().parse().ok()?;
    let to: usize = to.trim().parse().ok()?;
    (from >= 1 && to >= from).then_some((from, to))
}

/// The pages already in the wiki that the overlap rule compares against.
///
/// Bookkeeping files and the two summary trees are skipped: an audit report or
/// an archived conversation is not a page competing for the same subject.
fn published(root: &Path, cfg: &Config) -> Vec<Published> {
    let dir = root.join(&cfg.paths.wiki);
    let mut out = Vec::new();
    for path in render::page_files(&dir).unwrap_or_default() {
        let Ok(rel) = path.strip_prefix(&dir) else {
            continue;
        };
        let rel = rel.to_string_lossy().replace('\\', "/");
        if rel == "index.md" || rel.starts_with("audit/") || rel.starts_with("archive/") {
            continue;
        }
        let Ok(raw) = fs::read(&path) else {
            continue;
        };
        // A page that does not parse is `lint`'s problem, not a reason to
        // refuse a merge.
        let Ok((front, _body)) = crate::page::parse(&raw) else {
            continue;
        };
        out.push(Published {
            rel,
            title: title_key(&front.title),
            sources: cited_sources(cfg, &front.sources),
            refs: front.sources.clone(),
        });
    }
    out
}

/// Why `candidate` looks like a page the wiki already holds, if it does.
///
/// A candidate that rewrites an existing page is compared only with *other*
/// pages: the page it overwrites is its own predecessor, so matching its title
/// or citing a subset of its sources is the normal case, not an overlap.
fn overlap_reason(
    candidate: &str,
    title: &str,
    sources: &BTreeSet<String>,
    pages: &[Published],
) -> Option<String> {
    for page in pages {
        if page.rel == candidate {
            continue;
        }
        if !title.is_empty() && title == page.title {
            return Some(format!("`{candidate}` 与已发布的 `{}` 标题相同", page.rel));
        }
        if !sources.is_empty() && sources.is_subset(&page.sources) {
            return Some(format!(
                "`{candidate}` 只引用了 `{}` 已经引用过的源，没有新证据",
                page.rel
            ));
        }
    }
    None
}

/// Snapshots the approved wiki as a new version.
///
/// The name is `vNNN`, one past the highest numbered directory already in
/// `.wiki/versions`. Afterwards only the newest `versions.keep` snapshots
/// survive; a directory without [`VERSION_FILE`] is left alone, and
/// `keep == 0` is honoured literally: everything, including the snapshot just
/// written, is pruned.
pub fn checkpoint(root: &Path, cfg: &Config, store: &mut Store, label: &str) -> Result<String> {
    let name = next_version_name(root)?;
    snapshot(root, cfg, store, &name, label, "checkpoint")
}

/// Publishes the staging candidates that fit the brakes into `wiki/`.
///
/// Nothing is asked: the unattended path has no human to ask, so the three
/// brakes filter instead of blocking. A candidate that overlaps a published
/// page, or that would push a slice or the run past its budget, stays in
/// `.wiki/staging/` with a reason; the rest are renamed into `wiki/`,
/// `index.md` is regenerated, the merge is journalled and snapshotted, and the
/// baseline advances.
///
/// `confirm == true` is the explicit override: it publishes every candidate and
/// ignores the brakes. The two checks that make a merge unsafe — a candidate
/// edited after it was submitted, a source that changed after it was processed
/// — still refuse instead of filtering, because publishing them would publish
/// synthesis of bytes nobody saw.
pub fn approve(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    confirm: bool,
) -> Result<ApproveOutcome> {
    let candidates = candidates(root, cfg)?;
    if candidates.is_empty() {
        return Err(VersionError::NothingToApprove);
    }
    let prefix = cfg.paths.wiki.trim_end_matches('/');
    for candidate in &candidates {
        let key = format!("{prefix}/{}", candidate.rel);
        let recorded = store.get_page_meta(&key)?;
        let current = hash::hash_file(&candidate.staging, true)?;
        if recorded.as_ref().map(|row| row.hash.as_str()) != Some(current.as_str()) {
            return Err(VersionError::Drift {
                page: candidate.rel.clone(),
            });
        }
    }
    check_sources(root, cfg)?;

    let (published, held_back) = if confirm {
        (
            candidates.iter().map(|c| c.rel.clone()).collect::<Vec<_>>(),
            Vec::new(),
        )
    } else {
        screen(root, cfg, &candidates)?
    };
    if published.is_empty() {
        return Ok(ApproveOutcome::NothingPublished { held_back });
    }

    // Publishing does not take a snapshot: the round opened by `ingest` is the
    // version, and `approve` only moves bytes the round already accounted for.
    let name = history(root)?
        .first()
        .map(|row| row.version.clone())
        .unwrap_or_default();
    for candidate in &candidates {
        if !published.iter().any(|rel| rel == &candidate.rel) {
            continue;
        }
        if let Some(parent) = candidate.wiki.parent() {
            fs::create_dir_all(parent).map_err(|source| io(parent, source))?;
        }
        fs::rename(&candidate.staging, &candidate.wiki)
            .map_err(|source| io(&candidate.staging, source))?;
    }
    // `submit` records each candidate's digest so the drift check above can
    // spot a hand edit. Publishing moves the bytes but not the FTS row, and
    // `query::refresh` would see a matching digest over an already-indexed key
    // and skip the page, so the search would keep answering with the pre-edit
    // body. Drop the record: the next refresh sees no digest and reindexes.
    for rel in &published {
        store.delete_page_meta(&format!("{prefix}/{rel}"))?;
    }
    render::write_index(root, cfg)?;
    let detail = if held_back.is_empty() {
        format!("{} 页", published.len())
    } else {
        format!("{} 页，{} 页留在暂存区", published.len(), held_back.len())
    };
    journal::append(root, "submit", Some(&name), &detail)?;

    let mut manifest = Manifest::load(root)?;
    for entry in manifest.sources.values_mut() {
        entry.baseline_hash = entry.current_hash.clone();
    }
    Manifest::save(root, &manifest)?;
    staging::clear_pending(root)?;

    Ok(ApproveOutcome::Applied {
        version: name,
        published,
        held_back,
    })
}

/// Splits the candidates into what can be published now and what waits.
///
/// Order matters: earlier candidates keep their place, the ones that would
/// cross a limit wait, so a run publishes the pages the agent wrote first rather
/// than an arbitrary subset. A candidate that does not parse is not held here —
/// a broken page is `lint`'s finding, not a reason to stop publishing.
fn screen(
    root: &Path,
    cfg: &Config,
    candidates: &[Candidate],
) -> Result<(Vec<String>, Vec<HeldBack>)> {
    let existing = published(root, cfg);
    let mut held_back: Vec<HeldBack> = Vec::new();

    let mut parsed: Vec<Option<Staged>> = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let page = fs::read(&candidate.staging).ok().and_then(|raw| {
            crate::page::parse(&raw).ok().map(|(front, _body)| Staged {
                title: title_key(&front.title),
                sources: front.sources,
            })
        });
        parsed.push(page);
    }

    // 1. Overlap: a page that says what the wiki already says is not published.
    let mut accepted: Vec<&Candidate> = Vec::new();
    for (index, candidate) in candidates.iter().enumerate() {
        if let Some(page) = &parsed[index] {
            let keys = cited_sources(cfg, &page.sources);
            if let Some(why) = overlap_reason(&candidate.rel, &page.title, &keys, &existing) {
                held_back.push(HeldBack {
                    page: candidate.rel.clone(),
                    reason: String::from("page_overlap"),
                    detail: format!("{why}；改 id/title 或补一个新源后再 submit"),
                });
                continue;
            }
        }
        accepted.push(candidate);
    }

    // 2. Slice budget: a slice of a source justifies a bounded number of pages,
    //    so a long document earns room in proportion to its length rather than
    //    its file count.
    let limit = cfg.ingest.max_pages_per_slice;
    if limit > 0 {
        let mut counts: BTreeMap<Slice, Vec<String>> = BTreeMap::new();
        for page in &existing {
            for slice in page_slices(root, cfg, &page.refs) {
                counts.entry(slice).or_default().push(page.rel.clone());
            }
        }
        let mut kept: Vec<&Candidate> = Vec::new();
        for candidate in accepted {
            let index = candidates
                .iter()
                .position(|other| other.rel == candidate.rel)
                .unwrap_or(0);
            let slices: BTreeSet<Slice> = parsed[index]
                .as_ref()
                .map(|page| page_slices(root, cfg, &page.sources))
                .unwrap_or_default();
            let over = slices.iter().find_map(|slice| {
                let used = counts.get(slice).map(|who| who.len()).unwrap_or(0);
                (used >= limit).then(|| (slice.clone(), used))
            });
            if let Some(((key, slice), used)) = over {
                let subject = if slice == 0 {
                    format!("整份 `{key}`")
                } else {
                    format!("`{key}` 的第 {slice} 个切片")
                };
                held_back.push(HeldBack {
                    page: candidate.rel.clone(),
                    reason: String::from("page_slice_budget"),
                    detail: format!(
                        "{subject} 已经有 {used} 页，再加这一页就超过 ingest.max_pages_per_slice（{limit}）；\
                         删掉旧页，或确认后用 approve --confirm 合并"
                    ),
                });
                continue;
            }
            for slice in slices {
                counts.entry(slice).or_default().push(candidate.rel.clone());
            }
            kept.push(candidate);
        }
        accepted = kept;
    }

    // 3. Page budget: a page the wiki already holds is an overwrite, not growth.
    let budget = cfg.ingest.max_new_pages_per_run;
    let mut kept: Vec<&Candidate> = Vec::new();
    let mut new_pages = 0usize;
    for candidate in accepted {
        if !candidate.wiki.is_file() {
            if budget > 0 && new_pages >= budget {
                held_back.push(HeldBack {
                    page: candidate.rel.clone(),
                    reason: String::from("page_budget"),
                    detail: format!(
                        "本轮新增页已达 ingest.max_new_pages_per_run（{budget}），\
                         这一页留到下一轮；确认后用 approve --confirm 立刻合并"
                    ),
                });
                continue;
            }
            new_pages += 1;
        }
        kept.push(candidate);
    }

    Ok((kept.into_iter().map(|c| c.rel.clone()).collect(), held_back))
}

/// Discards the candidates and records why.
///
/// The staged tree is restored to a mirror of `wiki/`: a page the wiki does not
/// have is deleted, a page whose bytes differ is overwritten with the approved
/// bytes, and a page missing from staging is copied back.
///
/// The round's sources stay settled even though its pages do not: the human
/// looked at what changed and refused it as wiki material, which is still a
/// judgement on the source. Their ledger rows take the bytes now on disk, so
/// `scan` stops reporting the same edit as `changed` every round, and the
/// staging baseline for them moves to the recorded state so `approve` does not
/// call them stale. Sources outside the round are left alone — their drift was
/// never reviewed.
pub fn reject(root: &Path, cfg: &Config, store: &mut Store, reason: &str) -> Result<()> {
    if reason.is_empty() {
        return Err(VersionError::NoReason);
    }
    let staging_dir = staging::dir(root);
    let wiki_dir = root.join(&cfg.paths.wiki);
    for path in render::page_files(&staging_dir)? {
        let rel = paths::identity_key(&staging_dir, &path)?;
        let approved = paths::join_key(&wiki_dir, &rel);
        if approved.is_file() {
            fs::copy(&approved, &path).map_err(|source| io(&path, source))?;
        } else {
            fs::remove_file(&path).map_err(|source| io(&path, source))?;
        }
    }
    staging::ensure(root, cfg)?;

    let keys = round_sources(root)?;
    accept_reviewed_sources(root, cfg, store, &keys)?;

    let mut manifest = Manifest::load(root)?;
    for key in &keys {
        if let Some(entry) = manifest.sources.get_mut(key) {
            entry.baseline_hash = entry.current_hash.clone();
        }
    }
    Manifest::save(root, &manifest)?;
    staging::clear_pending(root)?;
    journal::append(root, "reject", None, reason)?;
    Ok(())
}

/// Takes the bytes now on disk as the accepted state of every source in a
/// rejected round.
///
/// `pending` is keyed the way `pending.json` is — relative to `.wiki/raw/` —
/// while ledger rows are keyed by the source path, so the mapping goes through
/// each row's `raw_key`. Sources that cannot be digested right now (oversize,
/// unreadable) are skipped: there is no state to accept, and `scan` keeps
/// reporting them, which is the honest answer.
fn accept_reviewed_sources(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    keys: &BTreeSet<String>,
) -> Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    let mut reviewed: Vec<SourceRow> = Vec::new();
    for row in store.all_sources()? {
        let Some(raw_key) = row.raw_key.as_deref() else {
            continue;
        };
        if !keys.contains(&raw_relative(cfg, raw_key)) {
            continue;
        }
        let path = paths::join_key(&root.join(&cfg.paths.source), &row.key);
        let Some(digest) = reviewed_digest(cfg, &path)? else {
            continue;
        };
        reviewed.push(SourceRow {
            key: row.key,
            hash: digest.0,
            size: digest.1,
            mtime: digest.2,
            raw_key: row.raw_key,
            converted_at: row.converted_at,
        });
    }
    let mut ledger = Ledger::new(store);
    for row in &reviewed {
        ledger.mark_reviewed(row)?;
    }
    Ok(())
}

/// The sources this round is still waiting on a decision for.
///
/// Two records name them: `pending.json` while the round is open, and
/// `manifest.json`'s `current_hash` after `ingest submit` (which is why it has
/// to be written there). A reject can arrive in either state, so both count.
fn round_sources(root: &Path) -> Result<BTreeSet<String>> {
    let mut keys: BTreeSet<String> = staging::pending_hashes(root)?.into_keys().collect();
    keys.extend(staging::manifest_pending(root)?.into_keys());
    Ok(keys)
}

/// `raw_key` with the configured raw directory stripped, matching the keys
/// `ingest` writes to `pending.json`.
fn raw_relative(cfg: &Config, raw_key: &str) -> String {
    let prefix = cfg.paths.raw.trim_matches('/');
    let raw_key = raw_key.trim_start_matches("./");
    match raw_key.strip_prefix(prefix) {
        Some(rest) => String::from(rest.trim_start_matches('/')),
        None => String::from(raw_key),
    }
}

/// Digest, size and mtime of the source at `path`, chosen the way `scan`
/// chooses them (full up to `parse.chunk_threshold`, sampled above) so the next
/// scan sees an unchanged source instead of a spurious `changed`.
///
/// `None` for a source too large to read: `scan` reports it as `oversize` and
/// keeps doing so, which is the state there is nothing to accept.
fn reviewed_digest(cfg: &Config, path: &Path) -> Result<Option<(String, i64, i64)>> {
    let fingerprint = hash::fingerprint(path)?;
    if fingerprint.size > cfg.parse.max_source_bytes {
        return Ok(None);
    }
    let digest = if fingerprint.size <= cfg.parse.chunk_threshold {
        hash::hash_file(path, true)?
    } else {
        fingerprint.sample.clone()
    };
    Ok(Some((digest, fingerprint.size as i64, fingerprint.mtime)))
}

/// Leaves a free-text instruction for the next agent session.
///
/// The text is written verbatim to `.wiki/next-instruction.md`; a single
/// trailing newline is added when the text does not end with one, and nothing
/// else about the file is touched. The instruction is guidance for the agent,
/// not a version-ledger entry, so it is deliberately not snapshotted here.
pub fn instruct(root: &Path, _cfg: &Config, _store: &mut Store, text: &str) -> Result<()> {
    if text.is_empty() {
        return Err(VersionError::NoText);
    }
    let path = root.join(ENGINE).join("next-instruction.md");
    let mut body = String::from(text);
    if !body.ends_with('\n') {
        body.push('\n');
    }
    let parent = path.parent().unwrap_or(root);
    fs::create_dir_all(parent).map_err(|source| io(parent, source))?;
    fs::write(&path, body.as_bytes()).map_err(|source| io(&path, source))?;
    journal::append(root, "instruct", None, text)?;
    Ok(())
}

/// Postpones the decision without changing any state.
///
/// Staging, the ledger and `wiki/` are left exactly as they were; only the log
/// gains a line, which is how a later reader tells "deferred" from "never
/// looked at".
pub fn defer(root: &Path, _cfg: &Config, _store: &mut Store) -> Result<()> {
    journal::append(root, "defer", None, "")?;
    Ok(())
}

/// The snapshot directory for `root`.
fn versions_dir(root: &Path) -> PathBuf {
    root.join(ENGINE).join("versions")
}

/// The next free version name, `vNNN`.
///
/// Only names of the form `v` plus digits are counted, so a hand-made backup
/// next to the snapshots cannot inflate the sequence.
fn next_version_name(root: &Path) -> Result<String> {
    let dir = versions_dir(root);
    let mut highest = 0u32;
    match fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries {
                let entry = entry.map_err(|source| io(&dir, source))?;
                if let Some(seq) = parse_version_name(&entry.file_name().to_string_lossy()) {
                    highest = highest.max(seq);
                }
            }
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(io(&dir, source)),
    }
    Ok(format!("v{:03}", highest + 1))
}

/// Sequence number of a name like `v012`, or `None` for anything else.
fn parse_version_name(name: &str) -> Option<u32> {
    let digits = name.strip_prefix('v')?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// One path in a snapshot, with the digest of the bytes it held.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
struct ManifestEntry {
    /// `pages/<rel>` or `raw/<rel>`, `/`-separated.
    path: String,
    /// `blake3:` digest of the content.
    sha: String,
}

/// What one snapshot held.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
struct VersionManifest {
    /// Every content file, sorted by path.
    entries: Vec<ManifestEntry>,
}

/// One row of the version history.
#[derive(Debug, Clone, Serialize)]
pub struct VersionRow {
    /// `vNNN`.
    pub version: String,
    /// Who took the snapshot: `ingest`, `submit` or `restore`.
    pub kind: String,
    /// Why the snapshot was taken.
    pub label: String,
    /// UTC creation time.
    pub created_at: String,
    /// Pages the snapshot held.
    pub pages: usize,
}

/// What a rollback changed.
#[derive(Debug, Clone, Serialize)]
pub struct RollbackOutcome {
    /// The version that was restored.
    pub restored: String,
    /// The new version recording the rollback itself.
    pub version: String,
    /// Content files materialised from the store.
    pub files: usize,
}

/// What one version holds, compared with the current working tree.
#[derive(Debug, Clone, Default, Serialize)]
pub struct VersionDiff {
    /// Paths the current tree has and the version does not.
    pub added: Vec<String>,
    /// Paths the version has and the current tree does not.
    pub removed: Vec<String>,
    /// Paths both have, with different bytes.
    pub changed: Vec<String>,
}

/// The directory of the content-addressed store.
fn objects_dir(root: &Path) -> PathBuf {
    root.join(ENGINE).join(OBJECTS_DIR)
}

/// Where one digest lives inside the store.
fn object_path(root: &Path, sha: &str) -> PathBuf {
    let hex = sha.strip_prefix("blake3:").unwrap_or(sha);
    let shard = &hex[..hex.len().min(2)];
    objects_dir(root).join(shard).join(hex)
}

/// The live location of a snapshot path, or `None` for an unknown prefix.
fn content_path(root: &Path, cfg: &Config, path: &str) -> Option<PathBuf> {
    if let Some(rel) = path
        .strip_prefix(PAGES_PREFIX)
        .and_then(|r| r.strip_prefix('/'))
    {
        return Some(paths::join_key(&root.join(&cfg.paths.wiki), rel));
    }
    let rel = path
        .strip_prefix(RAW_PREFIX)
        .and_then(|r| r.strip_prefix('/'))?;
    Some(paths::join_key(&root.join(&cfg.paths.raw), rel))
}

/// Every content file, as `(snapshot path, live path)`, sorted by snapshot path.
fn collect_content(root: &Path, cfg: &Config) -> Result<Vec<(String, PathBuf)>> {
    let mut out: Vec<(String, PathBuf)> = Vec::new();
    for (prefix, dir) in [
        (PAGES_PREFIX, root.join(&cfg.paths.wiki)),
        (RAW_PREFIX, root.join(&cfg.paths.raw)),
    ] {
        if !dir.is_dir() {
            continue;
        }
        for entry in WalkDir::new(&dir).follow_links(false).sort_by_file_name() {
            let entry = entry.map_err(|err| walk_error(&dir, err))?;
            if !entry.file_type().is_file() {
                continue;
            }
            let rel = entry.path().strip_prefix(&dir).unwrap_or(entry.path());
            let rel = rel.to_string_lossy().replace('\\', "/");
            out.push((format!("{prefix}/{rel}"), entry.path().to_path_buf()));
        }
    }
    out.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(out)
}

/// Stores one file's bytes under their digest and returns the digest.
fn store_object(root: &Path, source: &Path) -> Result<String> {
    let bytes = fs::read(source).map_err(|err| io(source, err))?;
    let digest = hash::hash_bytes(&bytes);
    let target = object_path(root, &digest);
    if !target.is_file() {
        write_atomic(&target, &bytes)?;
    }
    Ok(digest)
}

/// Writes `body` to `path` through a temporary file in the same directory.
fn write_atomic(path: &Path, body: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|source| io(parent, source))?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = parent.join(format!(".{name}.tmp"));
    fs::write(&temp, body).map_err(|source| io(&temp, source))?;
    fs::rename(&temp, path).map_err(|source| io(path, source))
}

/// Reads the manifest of a snapshot, naming it in every error.
fn read_manifest(root: &Path, name: &str) -> Result<VersionManifest> {
    let path = versions_dir(root).join(name).join(MANIFEST_FILE);
    let body = match fs::read(&path) {
        Ok(body) => body,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Err(VersionError::NoManifest {
                name: String::from(name),
            })
        }
        Err(source) => return Err(io(&path, source)),
    };
    serde_json::from_slice(&body).map_err(|source| VersionError::Encode { path, source })
}

/// Records the content of `.wiki/pages/` and `.wiki/raw/` as a new version.
///
/// Only bytes that are new to the object store are written, so the cost of a
/// snapshot follows the size of the change, not the size of the wiki.
fn snapshot(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    name: &str,
    label: &str,
    kind: &str,
) -> Result<String> {
    let dir = versions_dir(root).join(name);
    fs::create_dir_all(&dir).map_err(|source| io(&dir, source))?;

    let mut entries = Vec::new();
    for (path, live) in collect_content(root, cfg)? {
        let sha = store_object(root, &live)?;
        entries.push(ManifestEntry { path, sha });
    }
    let manifest = VersionManifest { entries };
    let manifest_path = dir.join(MANIFEST_FILE);
    let body = serde_json::to_vec_pretty(&manifest).map_err(|source| VersionError::Encode {
        path: manifest_path.clone(),
        source,
    })?;
    fs::write(&manifest_path, body).map_err(|source| io(&manifest_path, source))?;

    let created_at = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let record = VersionRecord {
        label: String::from(label),
        created_at: created_at.clone(),
        pages: manifest
            .entries
            .iter()
            .filter(|entry| {
                let Some(rel) = entry.path.strip_prefix("pages/") else {
                    return false;
                };
                // Bookkeeping is in the snapshot but is not a page: `audit/` is
                // a record of a run, `index.md` is generated.
                let first = rel.split('/').next().unwrap_or("");
                first != "audit" && rel != "index.md"
            })
            .count(),
        kind: String::from(kind),
    };
    let record_path = dir.join(VERSION_FILE);
    let body = serde_json::to_vec_pretty(&record).map_err(|source| VersionError::Encode {
        path: record_path.clone(),
        source,
    })?;
    fs::write(&record_path, body).map_err(|source| io(&record_path, source))?;

    store.record_version(&created_at, label, kind)?;
    prune(root, cfg.versions.keep)?;
    Ok(String::from(name))
}

/// Restores `.wiki/pages/` and `.wiki/raw/` to what `name` held.
///
/// The live tree is replaced, not merged: a file the version does not have is
/// deleted. The index is rebuilt from the restored pages, and the rollback
/// itself is snapshotted, so the next rollback can undo this one.
pub fn rollback(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    name: &str,
) -> Result<RollbackOutcome> {
    if !versions_dir(root).join(name).is_dir() {
        return Err(VersionError::NoVersion {
            name: String::from(name),
        });
    }
    let manifest = read_manifest(root, name)?;

    for entry in &manifest.entries {
        let source = object_path(root, &entry.sha);
        let body = match fs::read(&source) {
            Ok(body) => body,
            Err(source_err) if source_err.kind() == std::io::ErrorKind::NotFound => {
                return Err(VersionError::MissingObject {
                    name: String::from(name),
                    sha: entry.sha.clone(),
                })
            }
            Err(source_err) => return Err(io(&source, source_err)),
        };
        let Some(target) = content_path(root, cfg, &entry.path) else {
            continue;
        };
        write_atomic(&target, &body)?;
    }

    let keep: std::collections::BTreeSet<&str> = manifest
        .entries
        .iter()
        .map(|entry| entry.path.as_str())
        .collect();
    for (path, live) in collect_content(root, cfg)? {
        if !keep.contains(path.as_str()) {
            fs::remove_file(&live).map_err(|source| io(&live, source))?;
        }
    }

    crate::query::reindex(root, cfg, store)?;
    journal::append(root, "rollback", Some(name), "")?;

    let new_name = next_version_name(root)?;
    snapshot(
        root,
        cfg,
        store,
        &new_name,
        &format!("rollback {name}"),
        "rollback",
    )?;
    Ok(RollbackOutcome {
        restored: String::from(name),
        version: new_name,
        files: manifest.entries.len(),
    })
}

/// Every snapshot, newest first.
pub fn history(root: &Path) -> Result<Vec<VersionRow>> {
    let dir = versions_dir(root);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(io(&dir, source)),
    };
    let mut rows = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| io(&dir, source))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if parse_version_name(&name).is_none() {
            continue;
        }
        let record_path = entry.path().join(VERSION_FILE);
        let Ok(body) = fs::read(&record_path) else {
            continue;
        };
        let Ok(record) = serde_json::from_slice::<VersionRecord>(&body) else {
            continue;
        };
        rows.push(VersionRow {
            version: name,
            kind: record.kind,
            label: record.label,
            created_at: record.created_at,
            pages: record.pages,
        });
    }
    rows.sort_by(|left, right| right.version.cmp(&left.version));
    Ok(rows)
}

/// Compares what `name` held with the current working tree.
pub fn diff(root: &Path, cfg: &Config, name: &str) -> Result<VersionDiff> {
    if !versions_dir(root).join(name).is_dir() {
        return Err(VersionError::NoVersion {
            name: String::from(name),
        });
    }
    let manifest = read_manifest(root, name)?;
    let recorded: std::collections::BTreeMap<&str, &str> = manifest
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry.sha.as_str()))
        .collect();
    let mut current: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for (path, live) in collect_content(root, cfg)? {
        let bytes = fs::read(&live).map_err(|source| io(&live, source))?;
        current.insert(path, hash::hash_bytes(&bytes));
    }

    let mut out = VersionDiff::default();
    for (path, sha) in &current {
        match recorded.get(path.as_str()) {
            None => out.added.push(path.clone()),
            Some(recorded_sha) if *recorded_sha != sha.as_str() => out.changed.push(path.clone()),
            Some(_) => {}
        }
    }
    for path in recorded.keys() {
        if !current.contains_key(*path) {
            out.removed.push(String::from(*path));
        }
    }
    Ok(out)
}

/// Deletes the oldest snapshots beyond `keep`.
///
/// Only directories carrying [`VERSION_FILE`] are considered, so a directory a
/// human dropped into the folder is never a pruning victim.
fn prune(root: &Path, keep: usize) -> Result<()> {
    let dir = versions_dir(root);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io(&dir, source)),
    };
    let mut owned: Vec<(u32, String)> = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| io(&dir, source))?;
        let path = entry.path();
        if !path.is_dir() || !path.join(VERSION_FILE).is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if let Some(seq) = parse_version_name(&name) {
            owned.push((seq, name));
        }
    }
    owned.sort_by_key(|(seq, _)| std::cmp::Reverse(*seq));
    for (_, name) in owned.into_iter().skip(keep) {
        let victim = dir.join(&name);
        fs::remove_dir_all(&victim).map_err(|source| io(&victim, source))?;
    }
    Ok(())
}

/// Staged pages that differ from `wiki/`, in path order.
///
/// A page counts when its path is absent from the wiki or its bytes differ;
/// pages that merely mirror the approved tree are not candidates, so a wiki that
/// predates the page ledger cannot block every merge.
fn candidates(root: &Path, cfg: &Config) -> Result<Vec<Candidate>> {
    let staging_dir = staging::dir(root);
    let wiki_dir = root.join(&cfg.paths.wiki);
    let approved: Vec<(String, PathBuf)> = render::page_files(&wiki_dir)?
        .into_iter()
        .map(|path| {
            let rel = paths::identity_key(&wiki_dir, &path)?;
            Ok((rel, path))
        })
        .collect::<Result<Vec<_>>>()?;

    let mut out = Vec::new();
    for path in render::page_files(&staging_dir)? {
        let rel = paths::identity_key(&staging_dir, &path)?;
        let differs = match approved.iter().find(|(key, _)| key == &rel) {
            Some((_, approved_path)) => {
                fs::read(&path).map_err(|source| io(&path, source))?
                    != fs::read(approved_path).map_err(|source| io(approved_path, source))?
            }
            None => true,
        };
        if differs {
            out.push(Candidate {
                wiki: paths::join_key(&wiki_dir, &rel),
                staging: path,
                rel,
            });
        }
    }
    Ok(out)
}

/// Refuses the merge when a pending source no longer matches its recorded hash.
fn check_sources(root: &Path, cfg: &Config) -> Result<()> {
    let raw_dir = root.join(&cfg.paths.raw);
    for (source_key, recorded) in staging::manifest_pending(root)? {
        let path = paths::join_key(&raw_dir, &source_key);
        match hash::hash_file(&path, true) {
            Ok(current) if current == recorded => {}
            Ok(_) => return Err(VersionError::Stale { source_key }),
            Err(hash::HashError::Io { source, .. })
                if source.kind() == std::io::ErrorKind::NotFound =>
            {
                return Err(VersionError::Stale { source_key })
            }
            Err(err) => return Err(err.into()),
        }
    }
    Ok(())
}

/// Builds an [`VersionError::Io`] for `path`.
fn io(path: &Path, source: std::io::Error) -> VersionError {
    VersionError::Io {
        path: path.to_path_buf(),
        source,
    }
}

/// Maps a `walkdir` failure onto [`VersionError::Walk`].
fn walk_error(path: &Path, err: walkdir::Error) -> VersionError {
    let source = err.io_error().map_or_else(
        || std::io::Error::other("cannot walk the directory"),
        |source| std::io::Error::new(source.kind(), source.to_string()),
    );
    VersionError::Walk {
        path: path.to_path_buf(),
        source,
    }
}
