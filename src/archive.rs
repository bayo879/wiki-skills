//! The archive layer: sealed conversation summaries.
//!
//! Layering is strictly ordered: live session records are distilled into
//! `wiki/archive/<period>.md`, the pages that used to cite those records are
//! re-hung onto the summary, and only then may the originals be cleared. The
//! order is not optional: the moment a record disappears, every page still
//! citing it holds a dangling `sources` entry and `lint` fails the repository.
//!
//! This module owns the whole archive layer: [`status`] reports the four
//! numbers that tell a human whether the layering is keeping up, [`seal`]
//! records a summary and re-hangs its citations, [`purge`] clears what is both
//! over capacity and unread, and [`recall`] is the only reader of the summaries.
//!
//! `archive status` must never write: it is the command a scheduled job runs to
//! decide whether to nudge a human, and a status command with side effects
//! cannot be run speculatively.

use std::path::{Path, PathBuf};

use chrono::{Datelike, Local, NaiveDate};
use walkdir::WalkDir;

use crate::config::Config;
use crate::hash::{self, HashError};
use crate::ledger::Ledger;
use crate::page;
use crate::render;
use crate::store::{self, ArchiveRow, PageMetaRow, Store};

/// The four numbers `archive status` prints, plus where it looked.
///
/// The counts are deliberately a report and not a verdict: "over the cap" and
/// "past the deadline" are conditions, but clearing a summary is a human
/// decision (`archive purge --yes`), so nothing here takes an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusReport {
    /// Sealed summaries recorded in the ledger.
    pub archives: usize,
    /// Live session records under `.wiki/sessions/`.
    ///
    /// This is the L0 water mark: the volume that has not been distilled yet.
    /// Counting files rather than ledger rows is what makes the number honest —
    /// a session the agent wrote but never ingested still costs attention.
    pub live_sessions: usize,
    /// Summaries a `archive purge` would clear right now.
    ///
    /// Both conditions must hold (over [`Config::memory.purge_cap`] *and* past
    /// [`Config::memory.purge_days`]), so a quiet repository reports zero.
    pub purge_candidates: usize,
    /// Summaries still cited by at least one page's `sources`.
    ///
    /// This is the seal's own proof: it rises as pages are re-hung, so a seal
    /// that wrote the summary but forgot to rewire the pages shows up as
    /// `archives > 0` with `still_referenced == 0`.
    pub still_referenced: usize,
    /// Where the status looked, for the human rendering.
    pub note: String,
}

/// Anything that can go wrong while inspecting the archive.
#[derive(Debug, thiserror::Error)]
pub enum ArchiveError {
    /// A filesystem operation failed.
    #[error("cannot access {}: {source}", path.display())]
    Io {
        /// Path that failed.
        path: PathBuf,
        /// Underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// Walking a directory failed.
    #[error("cannot walk the archive: {0}")]
    Walk(#[from] walkdir::Error),
    /// A page could not be read or rendered.
    #[error(transparent)]
    Page(#[from] page::PageError),
    /// Walking or rendering the wiki tree failed.
    #[error(transparent)]
    Render(#[from] render::RenderError),
    /// A file could not be hashed.
    #[error(transparent)]
    Hash(#[from] HashError),
    /// A source handed to `seal` is not in the ledger.
    #[error("source `{key}` is not in the ledger")]
    NotLedgered {
        /// Offending ledger key.
        key: String,
    },
    /// The period already has a summary recorded.
    #[error("`{key}` is already sealed")]
    AlreadySealed {
        /// Summary key.
        key: String,
    },
    /// The agent has not written the summary yet.
    #[error("no summary at {}; the agent writes it before sealing", path.display())]
    NoSummary {
        /// Summary key.
        key: String,
        /// Where it should be.
        path: PathBuf,
    },
    /// Nothing was named to seal.
    #[error("no sources to seal")]
    NoSources,
    /// A purge was asked for without `--yes`.
    #[error("purge deletes bytes; pass --dry-run to look or --yes to clear")]
    NeedsConfirmation,
    /// `recall` was given an empty string.
    #[error("recall needs a search term")]
    EmptyQuery,
    /// Nothing matched.
    #[error("no sealed summary matches")]
    NoHit,
    /// The ledger failed.
    #[error(transparent)]
    Store(#[from] store::StoreError),
}

impl ArchiveError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            ArchiveError::Io { .. } | ArchiveError::Walk(_) => "E_IO",
            ArchiveError::Page(err) => err.code(),
            ArchiveError::Render(err) => err.code(),
            ArchiveError::Hash(_) => "E_IO",
            ArchiveError::NotLedgered { .. } => "E_NOT_LEDGERED",
            ArchiveError::AlreadySealed { .. } => "E_ALREADY_SEALED",
            ArchiveError::NoSummary { .. } => "E_NO_SUMMARY",
            ArchiveError::NoSources => "E_NO_SOURCES",
            ArchiveError::NeedsConfirmation => "E_NEEDS_CONFIRMATION",
            ArchiveError::EmptyQuery => "E_EMPTY_QUERY",
            ArchiveError::NoHit => "E_NO_HIT",
            ArchiveError::Store(_) => "E_DB",
        }
    }
}

/// Result alias for archive operations.
pub type Result<T> = std::result::Result<T, ArchiveError>;

/// What the caller must supply to seal a period.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SealRequest {
    /// Ledger keys of the session records being sealed.
    pub sources: Vec<String>,
    /// Summary id; defaults to the `YYYY-MM` of `period_end`.
    pub id: Option<String>,
    /// Summary title, for the human rendering only.
    pub title: Option<String>,
    /// First day the summary covers; defaults to the first of `period_end`'s month.
    pub period_start: Option<NaiveDate>,
    /// Last day the summary covers; required.
    pub period_end: Option<NaiveDate>,
    /// Who sealed it: `human` or `agent`.
    pub by: String,
}

/// What [`seal`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SealReport {
    /// Repo-relative key of the summary page.
    pub key: String,
    /// Ledger keys that were sealed, normalised.
    pub sources: Vec<String>,
    /// Pages whose `sources` were re-hung onto the summary.
    pub pages: Vec<String>,
    /// How many of those pages exist (the length of [`pages`]).
    pub updated_pages: usize,
}

/// Seals the period covered by `req`, as of today.
pub fn seal(root: &Path, cfg: &Config, store: &mut Store, req: &SealRequest) -> Result<SealReport> {
    seal_at(root, cfg, store, req, Local::now().date_naive())
}

/// Seals the period covered by `req`, as of `today`.
///
/// The order is load-bearing and runs backwards from what looks natural:
/// the pages are re-hung first and the ledger row is written last. A crash in
/// between then leaves pages citing a summary whose row is missing — the next
/// run finishes it, because nothing is sealed yet. Writing the row first would
/// make the same crash unrecoverable: the retry would be refused as a duplicate
/// and the pages would keep citing a record that is about to be purged.
///
/// The summary body is never written here. The agent writes
/// `wiki/archive/<id>.md` and hands its sources to this command; the CLI only
/// validates the ledger, records the seal, re-hangs the citations, and stamps
/// the dates.
pub fn seal_at(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    req: &SealRequest,
    today: NaiveDate,
) -> Result<SealReport> {
    let keys = normalise_keys(cfg, &req.sources);
    if keys.is_empty() {
        return Err(ArchiveError::NoSources);
    }
    for key in &keys {
        if store.get_source(key)?.is_none() {
            return Err(ArchiveError::NotLedgered { key: key.clone() });
        }
    }

    let period_end = req.period_end.unwrap_or(today);
    let period_start = req.period_start.unwrap_or_else(|| {
        NaiveDate::from_ymd_opt(period_end.year(), period_end.month(), 1).unwrap_or(period_end)
    });
    let id = req
        .id
        .clone()
        .unwrap_or_else(|| format!("{:04}-{:02}", period_end.year(), period_end.month()));
    let key = summary_key(cfg, &id);
    if store.get_archive(&key)?.is_some() {
        return Err(ArchiveError::AlreadySealed { key });
    }
    let summary = summary_file(root, &key);
    if !summary.is_file() {
        return Err(ArchiveError::NoSummary { key, path: summary });
    }
    // The summary must be readable before anything is rewired: a broken
    // frontmatter here would be discovered only after the pages moved.
    page::read(&summary)?;

    let pages = rehang(root, cfg, store, &keys, &key, today)?;
    let sealed_at = today.to_string();
    store.upsert_archive(&ArchiveRow {
        key: key.clone(),
        period_start: period_start.to_string(),
        period_end: period_end.to_string(),
        sources: keys.clone(),
        sealed_at: sealed_at.clone(),
        sealed_by: req.by.clone(),
        last_recalled_at: None,
        recall_count: 0,
    })?;
    store.record_version(&sealed_at, &format!("seal {key}"), "archive")?;

    Ok(SealReport {
        key,
        sources: keys,
        updated_pages: pages.len(),
        pages,
    })
}

/// Re-points every page that cites one of `keys` at the summary `key`.
///
/// Only `sources` is rewritten; the body keeps its prose and its `[[links]]`,
/// which is why a page does not need to be re-read by a human after a seal.
fn rehang(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    keys: &[String],
    key: &str,
    today: NaiveDate,
) -> Result<Vec<String>> {
    let wiki_dir = root.join(&cfg.paths.wiki);
    let archive_dir = dir(root, cfg);
    let raw_prefix = cfg.paths.raw.trim_end_matches('/');
    let wiki_prefix = cfg.paths.wiki.trim_end_matches('/');
    let mut touched = Vec::new();
    for path in render::page_files(&wiki_dir)? {
        if path.starts_with(&archive_dir) {
            continue;
        }
        let (front, _body) = page::read(&path)?;
        let matched: Vec<String> = front
            .sources
            .iter()
            .filter(|reference| {
                let base = base_reference(reference);
                let stripped = base
                    .strip_prefix(raw_prefix)
                    .map_or(base, |rest| rest.trim_start_matches('/'));
                keys.iter().any(|key| key == stripped)
            })
            .cloned()
            .collect();
        if matched.is_empty() {
            continue;
        }
        page::replace_sources(&path, &matched, &[String::from(key)])?;
        page::write_updated(&path, today)?;
        let rel = path.strip_prefix(&wiki_dir).unwrap_or(&path);
        let page_key = format!("{wiki_prefix}/{}", rel.to_string_lossy().replace('\\', "/"));
        store.upsert_page_meta(&PageMetaRow {
            page_key: page_key.clone(),
            hash: hash::hash_file(&path, true)?,
            updated: today.to_string(),
        })?;
        touched.push(page_key);
    }
    Ok(touched)
}

/// Repo-relative key of a summary page named `id`.
fn summary_key(cfg: &Config, id: &str) -> String {
    format!(
        "{}/archive/{}.md",
        cfg.paths.wiki.trim_end_matches('/'),
        id.trim_end_matches(".md")
    )
}

/// Absolute path of a summary page key.
fn summary_file(root: &Path, key: &str) -> PathBuf {
    root.join(key)
}

/// `key` without a `#p=..` fragment.
fn base_reference(key: &str) -> &str {
    key.split('#').next().unwrap_or(key).trim()
}

/// Normalises caller-supplied source keys to ledger form.
///
/// Accepting both `queries/a.md` and `raw/queries/a.md` costs one strip and
/// saves the caller from having to remember which side of the conversion it is
/// looking at: the ledger key is relative to the source root, but everything a
/// page cites is relative to the raw root.
fn normalise_keys(cfg: &Config, sources: &[String]) -> Vec<String> {
    let raw_prefix = cfg.paths.raw.trim_end_matches('/');
    let mut out: Vec<String> = Vec::new();
    for source in sources {
        let trimmed = base_reference(source);
        let key = trimmed
            .strip_prefix(raw_prefix)
            .map_or(trimmed, |rest| rest.trim_start_matches('/'));
        let key = String::from(key);
        if !out.contains(&key) {
            out.push(key);
        }
    }
    out
}
/// The archive directory's name inside the wiki tree.
///
/// `render` reads it to keep summaries out of the catalogue and `lint` reads it
/// to exempt their references, so the name has a single spelling.
pub const DIR: &str = "archive";

/// The archive directory for `root`, relative to the wiki tree.
pub fn dir(root: &Path, cfg: &Config) -> PathBuf {
    root.join(&cfg.paths.wiki).join(DIR)
}

/// The session scratch directory for `root`.
fn session_dir(root: &Path) -> PathBuf {
    root.join(".wiki").join("sessions")
}

/// Reports the water marks as of today.
pub fn status(root: &Path, cfg: &Config, store: &Store) -> Result<StatusReport> {
    status_at(root, cfg, store, Local::now().date_naive())
}

/// Reports the water marks as of `today`.
///
/// The clock is a parameter so the deadline arithmetic is testable without
/// waiting three months for a summary to age.
pub fn status_at(
    root: &Path,
    cfg: &Config,
    store: &Store,
    today: NaiveDate,
) -> Result<StatusReport> {
    let archives = store.all_archives()?;
    let cited = cited_keys(root, cfg, &archives)?;
    let purge_candidates = archives
        .iter()
        .filter(|row| !cited.contains(&row.key))
        .filter(|row| is_purge_candidate(row, archives.len(), cfg, today))
        .count();
    Ok(StatusReport {
        archives: archives.len(),
        live_sessions: count_live_sessions(root)?,
        purge_candidates,
        still_referenced: cited.len(),
        note: root.display().to_string(),
    })
}

/// Whether `row` is past both the capacity and the deadline condition.
///
/// Capacity is a property of the archive as a whole, so it arrives as `total`
/// and applies to every row: one summary too many makes the oldest ones
/// candidates, not all of them.
///
/// The deadline counts from the last `recall`, falling back to the end of the
/// period the summary covers: a summary nobody ever read a week after it was
/// sealed is stale, but a summary that covers last month only becomes old once
/// last month is far behind us.
fn is_purge_candidate(
    row: &store::ArchiveRow,
    total: usize,
    cfg: &Config,
    today: NaiveDate,
) -> bool {
    if total <= cfg.memory.purge_cap {
        return false;
    }
    let last_touch = row
        .last_recalled_at
        .as_deref()
        .and_then(parse_date)
        .or_else(|| parse_date(&row.period_end));
    match last_touch {
        Some(date) => (today - date).num_days() > i64::from(cfg.memory.purge_days),
        // An unparsable timestamp cannot prove freshness, so it is not cleared:
        // the safe direction for a delete is to keep the bytes.
        None => false,
    }
}

/// The date part of an ISO timestamp or bare date.
///
/// `lint`'s idle-summary rule ages a row the same way `purge` does, so the two
/// agree on what "days since" means.
pub(crate) fn parse_date(text: &str) -> Option<NaiveDate> {
    let head: String = text.chars().take(10).collect();
    NaiveDate::parse_from_str(&head, "%Y-%m-%d").ok()
}

/// The archive keys some page still names in its `sources`.
///
/// Only `sources` is scanned, not the body's `[[links]]`: re-hanging `sources`
/// is exactly the step a seal must perform, so the count measures that step.
/// Summaries are the only pages whose key is checked, and pages inside the
/// archive directory are skipped — a summary citing another summary is a
/// cross-reference between sealed periods, not a live dependency.
fn cited_keys(
    root: &Path,
    cfg: &Config,
    archives: &[store::ArchiveRow],
) -> Result<std::collections::BTreeSet<String>> {
    let mut cited = std::collections::BTreeSet::new();
    if archives.is_empty() {
        return Ok(cited);
    }
    let archive_dir = dir(root, cfg);
    let wiki_dir = root.join(&cfg.paths.wiki);
    for path in render::page_files(&wiki_dir)? {
        if path.starts_with(&archive_dir) {
            continue;
        }
        let (front, _body) = page::read(&path)?;
        for source in &front.sources {
            let key = source.split('#').next().unwrap_or(source);
            if archives.iter().any(|row| row.key == key) {
                cited.insert(key.to_string());
            }
        }
    }
    Ok(cited)
}

/// What `archive purge` was asked to do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PurgeRequest {
    /// Report the candidates and touch nothing.
    pub dry_run: bool,
    /// The human said yes; required for anything to be deleted.
    pub yes: bool,
}

/// What `archive purge` did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PurgeReport {
    /// Summary keys that were (or, for a dry run, would be) cleared.
    pub candidates: Vec<String>,
    /// Live source records whose bytes were removed.
    pub sources: usize,
    /// Summaries actually cleared.
    pub deleted: usize,
    /// Whether this was a dry run.
    pub dry_run: bool,
}

/// Clears the summaries that are both over capacity and past their deadline.
///
/// Three guards keep a delete from being a surprise:
/// - nothing happens without `--yes` (a bare run refuses with a confirmation
///   code rather than assuming the flag was meant);
/// - a summary a page still cites is never a candidate, because clearing it
///   would leave that page with a `sources` entry pointing at nothing;
/// - both the capacity and the deadline condition must hold, so a small wiki
///   never loses history merely because it is old.
///
/// Purging removes the summary page, the source bytes it distilled, the derived
/// raw, and every ledger row for them, then records a version entry. The pages
/// stay: the seal already re-hung them onto the summary, and with the summary
/// gone they keep a `sources` entry that the next `lint` will flag as stale —
/// which is the honest outcome, since a page citing a summary that was cleared
/// has lost its provenance.
pub fn purge(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    req: &PurgeRequest,
) -> Result<PurgeReport> {
    purge_at(root, cfg, store, req, Local::now().date_naive())
}

/// [`purge`] as of `today`.
pub fn purge_at(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    req: &PurgeRequest,
    today: NaiveDate,
) -> Result<PurgeReport> {
    if !req.dry_run && !req.yes {
        return Err(ArchiveError::NeedsConfirmation);
    }
    let archives = store.all_archives()?;
    let total = archives.len();
    let cited = cited_keys(root, cfg, &archives)?;
    let candidates: Vec<ArchiveRow> = archives
        .into_iter()
        .filter(|row| !cited.contains(&row.key))
        .filter(|row| is_purge_candidate(row, total, cfg, today))
        .collect();

    let mut report = PurgeReport {
        candidates: candidates.iter().map(|row| row.key.clone()).collect(),
        dry_run: req.dry_run,
        ..PurgeReport::default()
    };
    if req.dry_run {
        return Ok(report);
    }

    for row in &candidates {
        for key in &row.sources {
            if let Some(source) = store.get_source(key)? {
                remove_file(root.join(&cfg.paths.source).join(key))?;
                if let Some(raw_key) = &source.raw_key {
                    remove_file(root.join(&cfg.paths.raw).join(raw_key))?;
                }
                Ledger::new(store).forget_source(key, source.raw_key.as_deref())?;
                report.sources += 1;
            }
        }
        remove_file(root.join(&row.key))?;
        Ledger::new(store).forget_archive(&row.key)?;
        report.deleted += 1;
    }
    if report.deleted > 0 {
        store.record_version(
            &today.to_string(),
            &format!("purge {} summaries", report.deleted),
            "archive",
        )?;
    }
    Ok(report)
}

/// Deletes `path` if it exists; a missing file is not a failure.
///
/// A purge that found the bytes already gone did its job, and failing on that
/// would make it impossible to finish a run that was interrupted halfway.
fn remove_file(path: PathBuf) -> Result<()> {
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(ArchiveError::Io { path, source }),
    }
}

/// What to look for in the sealed summaries.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecallRequest {
    /// Case-insensitive substring, matched against the title, summary and body.
    pub query: String,
    /// Return the whole summary body instead of an excerpt.
    pub full: bool,
}

/// One summary that matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecallHit {
    /// Repo-relative key of the summary.
    pub key: String,
    /// Title from the summary's frontmatter.
    pub title: String,
    /// First day the summary covers.
    pub period_start: String,
    /// Last day the summary covers.
    pub period_end: String,
    /// The distilled text, or an excerpt of it.
    pub body: String,
}

/// What `recall` found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecallReport {
    /// Matches, newest period first.
    pub hits: Vec<RecallHit>,
    /// Summaries the ledger knows about whose page is gone.
    pub missing: usize,
}

/// Searches the sealed summaries.
///
/// This is the only reader of the archive, which is what makes the layer a
/// boundary rather than a convention: agents get the distilled text here and
/// nothing walks `wiki/archive/` directly, so a `recall` is also the signal that
/// keeps a summary out of the purge queue.
///
/// No configuration is taken: a summary is found by the ledger key and read from
/// the repository root, so the search depends on nothing the config could change.
pub fn recall(root: &Path, store: &mut Store, req: &RecallRequest) -> Result<RecallReport> {
    recall_at(root, store, req, Local::now().date_naive())
}

/// [`recall`] with the hit timestamps written by the caller's clock.
pub fn recall_at(
    root: &Path,
    store: &mut Store,
    req: &RecallRequest,
    today: NaiveDate,
) -> Result<RecallReport> {
    let needle = req.query.trim().to_lowercase();
    if needle.is_empty() {
        return Err(ArchiveError::EmptyQuery);
    }
    let mut archives = store.all_archives()?;
    // Newest first: the most recent period is the one a question is usually
    // about, and it keeps the hit list stable when several summaries match.
    archives.sort_by(|a, b| b.period_end.cmp(&a.period_end));

    let mut report = RecallReport::default();
    let mut hits: Vec<(String, RecallHit)> = Vec::new();
    for row in archives {
        let path = root.join(&row.key);
        if !path.is_file() {
            report.missing += 1;
            continue;
        }
        let (front, body) = page::read(&path)?;
        let haystack = format!(
            "{}\n{}\n{}",
            front.title.to_lowercase(),
            front.summary.to_lowercase(),
            body.to_lowercase()
        );
        if !haystack.contains(&needle) {
            continue;
        }
        let text = if req.full {
            body.trim().to_string()
        } else {
            excerpt(&body)
        };
        hits.push((
            row.period_end.clone(),
            RecallHit {
                key: row.key.clone(),
                title: front.title,
                period_start: row.period_start.clone(),
                period_end: row.period_end.clone(),
                body: text,
            },
        ));
    }
    for (_, hit) in hits {
        store.touch_archive_recall(&hit.key, &today.to_string())?;
        report.hits.push(hit);
    }
    if report.hits.is_empty() && report.missing == 0 {
        return Err(ArchiveError::NoHit);
    }
    Ok(report)
}

/// The first `LIMIT` characters of `body`, with an ellipsis when cut.
fn excerpt(body: &str) -> String {
    const LIMIT: usize = 480;
    let trimmed = body.trim();
    if trimmed.chars().count() <= LIMIT {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(LIMIT).collect();
    out.push('…');
    out
}
/// How many `.md` files sit under `.wiki/sessions/`.
fn count_live_sessions(root: &Path) -> Result<usize> {
    let dir = session_dir(root);
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in WalkDir::new(&dir).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("md")
        {
            count += 1;
        }
    }
    Ok(count)
}
