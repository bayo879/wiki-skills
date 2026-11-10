//! `sync`: one unattended round over the repository.
//!
//! The round is a fixed pipeline: take the lock, scan, convert what has settled,
//! hand the converted sources (plus whatever the last round left waiting for its
//! candidates) to an ingest round, render the index and write the audit entry.
//! Every step is allowed to be uninteresting; only a failure to take the lock or
//! to write the derived state ends the round.
//!
//! Debouncing is one rule, applied per source and remembered nowhere: a source is
//! converted only when its mtime is at least `[ingest].settle_seconds` old (five
//! minutes by default) **and** its bytes still hash to what `scan` read. A file
//! written moments ago waits for the next round; a file that changed while the
//! round was running waits too, because converting it now would hand review a
//! tree that no longer matches the one on disk. `--force` and
//! `[ingest].settle_seconds = 0` skip the wait; naming a key does not, because
//! the wait is about the file, not about who asked.
//!
//! `--dry-run` walks the same path up to the conversion budget and reports what
//! it *would* do. It writes no file (the lock is taken and removed again).

use std::fs;
use std::io::Write as _;
use std::path::Path;
use std::time::SystemTime;

use chrono::Local;
use serde::Serialize;

use crate::config::Config;
use crate::page;
use crate::scan::{self, ChangeKind, Layer};
use crate::store::{PageMetaRow, Store};
use crate::{convert, hash, lock, moves, paths, render, source_refs};

/// Knobs for one round, all of them from the command line.
#[derive(Debug, Clone)]
pub struct SyncArgs {
    /// Convert at most this many pending sources this round.
    pub limit: usize,
    /// Report what would happen and write nothing.
    pub dry_run: bool,
    /// Convert regardless of how recently the source was modified.
    pub force: bool,
    /// Convert only these repo-relative keys; empty means every pending source.
    pub only: Vec<String>,
}

/// What one round did.
#[derive(Debug, Clone, Serialize)]
pub struct SyncReport {
    /// Identity of this round, recorded in the lock file and the audit entry.
    pub run_id: String,
    /// The classified changes the round started from.
    pub changes: Vec<scan::Change>,
    /// Pages turned stale this round because the source they cited disappeared.
    pub stale_pages: Vec<String>,
    /// Sources that produced a fresh raw file.
    pub converted: usize,
    /// Sources handed to an ingest round.
    pub ingested: Vec<String>,
    pub work_items: Vec<crate::store::WorkItem>,
    pub affected_pages: Vec<String>,
    /// Sources left for the next round by `limit`.
    pub queued: usize,
    /// Everything worth telling a human about this round.
    pub warnings: Vec<String>,
    /// Conversions that failed.
    pub failures: usize,
}

/// Failure that ends a round before it can report.
#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    /// The repository lock is held by another process, or cannot be written.
    #[error(transparent)]
    Lock(#[from] crate::lock::LockError),
    /// `source/` could not be scanned.
    #[error(transparent)]
    Scan(#[from] crate::scan::ScanError),
    /// The ledger could not be read or written.
    #[error(transparent)]
    Store(#[from] crate::store::StoreError),
    /// A raw file could not be hashed.
    #[error(transparent)]
    Hash(#[from] crate::hash::HashError),
    /// `wiki/index.md` could not be rendered.
    #[error(transparent)]
    Render(#[from] crate::render::RenderError),
    /// Source work queue could not be read or migrated.
    #[error(transparent)]
    Work(#[from] crate::worklist::WorkError),
    /// A page could not be read or rewritten.
    #[error(transparent)]
    Page(#[from] crate::page::PageError),
    /// A rename could not be migrated.
    #[error(transparent)]
    Move(#[from] crate::moves::MoveError),
    /// A path could not be turned into a repository key.
    #[error(transparent)]
    Path(#[from] crate::paths::PathError),
    /// A derived file could not be written.
    #[error("cannot write {}: {source}", path.display())]
    Io {
        /// The file the round tried to write.
        path: std::path::PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
}

impl SyncError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            SyncError::Lock(err) => err.code(),
            SyncError::Scan(_) | SyncError::Hash(_) | SyncError::Render(_) | SyncError::Page(_) => {
                "E_IO"
            }
            SyncError::Store(_) => "E_DB",
            SyncError::Work(_) => "E_DB",
            SyncError::Move(err) => err.code(),
            SyncError::Path(_) => "E_IO",
            SyncError::Io { .. } => "E_IO",
        }
    }
}

/// Result of [`run`].
pub type Result<T> = std::result::Result<T, SyncError>;

/// One unattended round.
pub fn run(root: &Path, cfg: &Config, store: &mut Store, args: &SyncArgs) -> Result<SyncReport> {
    // 1. The lock, before anything is read or written: two rounds must never
    //    interleave, and the lock names the run the audit entry will carry.
    let lock = if args.dry_run {
        None
    } else {
        Some(lock::acquire(root)?)
    };
    let run_id = lock
        .as_ref()
        .map(|held| held.run_id().to_string())
        .unwrap_or_else(|| "dry-run".into());
    let mut warnings = Vec::new();

    // 2. What changed on disk.
    let changes = scan::scan_source_with_force(root, cfg, store, args.force)?.changes;

    // 3. The settle gate, in one pass and with no memory between rounds: a
    //    source is taken when its mtime is old enough and its bytes are still
    //    the ones `scan` read. `--force` and a zero window are the two ways to
    //    skip the wait; they are explicit, so they are also explainable.
    let explicit = !args.only.is_empty();
    if explicit {
        for key in &args.only {
            if !changes.iter().any(|change| &change.key == key) {
                warnings.push(format!("{key}: no new or changed source has that key"));
            }
        }
    }

    // A source above `parse.max_source_bytes` is never opened, so it never
    // becomes a work item and never fails the round. Naming it once per round is
    // the only way a human learns that `ingest` left it alone on purpose: a
    // silent skip reads as "already handled".
    for change in &changes {
        if change.kind != ChangeKind::Oversize {
            continue;
        }
        if explicit && !args.only.iter().any(|key| key == &change.key) {
            continue;
        }
        warnings.push(format!(
            "oversize: '{}' is above parse.max_source_bytes ({} bytes); it stays unconverted until the file is ignored or the limit is raised",
            change.key, cfg.parse.max_source_bytes
        ));
    }
    let mut pending: Vec<&scan::Change> = Vec::new();
    for change in &changes {
        if !matches!(change.kind, ChangeKind::New | ChangeKind::Changed) {
            continue;
        }
        if explicit && !args.only.iter().any(|key| key == &change.key) {
            continue;
        }
        if args.force || cfg.ingest.settle_seconds == 0 {
            pending.push(change);
            continue;
        }
        let path = paths::join_key(&root.join(&cfg.paths.source), &change.key);
        match quiet_seconds(&path) {
            Some(age) if age < cfg.ingest.settle_seconds => warnings.push(format!(
                "settle: '{}' was modified {age}s ago; waiting another {}s",
                change.key,
                cfg.ingest.settle_seconds - age
            )),
            Some(_) => {
                // The mtime says "nobody wrote it for a while"; the digest says
                // "and it is still the file `scan` classified". A source that
                // fails the second check is not converted on a stale verdict.
                if still_the_same_bytes(&path, change.hash.as_deref()) {
                    pending.push(change);
                } else {
                    warnings.push(format!(
                        "settle: '{}' changed during the round; taking it next round",
                        change.key
                    ));
                }
            }
            // No usable mtime (missing metadata, clock skew): fall back to the
            // digest-only check rather than blocking a source forever.
            None if still_the_same_bytes(&path, change.hash.as_deref()) => pending.push(change),
            None => warnings.push(format!(
                "settle: '{}' changed during the round; taking it next round",
                change.key
            )),
        }
    }

    // 4. Convert, newest unchanged set first, up to the budget.
    let planned = pending.len();
    let budget = planned.min(args.limit);
    let queued = planned - budget;

    if args.dry_run {
        if budget > 0 {
            warnings.push(format!("dry run: {budget} source(s) would be converted"));
        }
        if queued > 0 {
            warnings.push(format!(
                "dry run: {queued} source(s) stay queued beyond the limit"
            ));
        }
        return Ok(SyncReport {
            run_id,
            changes,
            stale_pages: Vec::new(),
            converted: 0,
            ingested: Vec::new(),
            work_items: store.work_items()?,
            affected_pages: Vec::new(),
            queued,
            warnings,
            failures: 0,
        });
    }

    // 4a. A rename is a move, not a delete plus an add: migrate it before
    //     anything else so the rest of the round already works on the new name.
    //     A collision is left untouched for a human; it is a finding (exit 1),
    //     never a reason to overwrite a file.
    let mut failures = 0;
    let moved = moves::apply(root, cfg, store, &changes)?;
    for applied in &moved.applied {
        warnings.push(format!(
            "moved: {} -> {} ({} page(s) rewritten)",
            applied.from,
            applied.to,
            applied.pages.len()
        ));
    }
    for collision in &moved.collisions {
        failures += 1;
        warnings.push(format!(
            "moved: {} -> {} skipped: {} ({})",
            collision.from, collision.to, collision.message, collision.code
        ));
    }

    // 4b. A source that disappeared leaves conclusions without a source behind
    //     them. `on_missing = "mark_stale"` marks the published pages that cited
    //     it stale; drafts are left alone, because marking one stale would claim
    //     a staleness its reader never saw.
    let stale_pages = stale_pages_for_missing(root, cfg, store, &changes)?;

    let mut converted = 0;
    for change in pending.iter().take(budget) {
        let key = change.key.as_str();
        match convert::convert_one(root, cfg, store, key) {
            Ok(outcome) => {
                if outcome.skipped {
                    warnings.push(format!(
                        "{key}: skipped ({})",
                        outcome.skip_reason.as_deref().unwrap_or("no reason given")
                    ));
                } else {
                    converted += 1;
                }
                for warning in &outcome.warnings {
                    warnings.push(format!("{key}: {}: {}", warning.code, warning.message));
                }
            }
            Err(err) => {
                // One bad source is a finding, not the end of the round.
                failures += 1;
                warnings.push(format!("{key}: conversion failed ({}) {err}", err.code()));
            }
        }
    }

    // The durable queue, not a staging round, is the handoff to the agent.
    if !args.dry_run {
        warnings.extend(crate::worklist::import_legacy(root, cfg, store)?);
    }
    let work_items = store.work_items()?;
    let ingested = work_items
        .iter()
        .map(|item| item.source_key.clone())
        .collect();
    let wanted: std::collections::BTreeSet<_> = work_items
        .iter()
        .map(|item| item.raw_key.as_str())
        .collect();
    let mut affected_pages = Vec::new();
    for entry in walkdir::WalkDir::new(root.join(&cfg.paths.wiki)).follow_links(false) {
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_file() || entry.path().extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        let Ok((front, _)) = page::read(entry.path()) else {
            continue;
        };
        if source_refs::source_keys(&front.sources, &cfg.paths.raw)
            .iter()
            .any(|key| wanted.contains(key.as_str()))
        {
            affected_pages.push(paths::identity_key(root, entry.path())?);
        }
    }
    affected_pages.sort();

    // 6. The index always reflects the raw layer, even on a quiet round.
    render::write_index(root, cfg)?;

    let report = SyncReport {
        run_id,
        changes,
        stale_pages,
        converted,
        ingested,
        work_items,
        affected_pages,
        queued,
        warnings,
        failures,
    };

    // 7. The audit entry: what changed, what was converted, what was handed to
    //    ingest, and what this round could not do.
    write_audit(root, cfg, &report)?;

    Ok(report)
}

/// The published pages that must go stale because a source they cite vanished.
///
/// The page's own `sources:` list is the truth here, not the ledger's
/// mapping: nothing in the engine writes that table after `init`, so a round
/// that trusted it would mark nothing. A `draft` is left alone — marking it
/// stale would claim a staleness its reader never saw — and a page already
/// `stale` is not touched again, so a long-gone source rewrites nothing after
/// the first round. The rewritten page refreshes its `page_meta` digest with the
/// old `updated` date: only the status line moved, so the page is not re-dated
/// and lint L9 cannot read this as undated drift.
fn stale_pages_for_missing(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    changes: &[scan::Change],
) -> Result<Vec<String>> {
    if cfg.ingest.on_missing != "mark_stale" {
        return Ok(Vec::new());
    }
    let mut vanished: Vec<String> = Vec::new();
    for change in changes {
        if change.kind != ChangeKind::Missing || change.layer != Layer::Source {
            continue;
        }
        // A source that never converted left no raw file, so no page can cite it.
        let Some(row) = store.get_source(&change.key)? else {
            continue;
        };
        if row.raw_key.is_none() {
            continue;
        }
        vanished.push(convert::raw_key_for(&change.key));
    }
    if vanished.is_empty() {
        return Ok(Vec::new());
    }

    let mut stale = Vec::new();
    for path in moves::page_files(root, cfg) {
        let (front, _) = match page::read(&path) {
            Ok(read) => read,
            // A file without frontmatter is not a page; lint owns that finding.
            Err(page::PageError::BadFrontmatter { .. }) => continue,
            Err(err) => return Err(SyncError::Page(err)),
        };
        if front.status != page::Status::Stable {
            continue;
        }
        let cited = source_refs::source_keys(&front.sources, &cfg.paths.raw);
        if !cited.iter().any(|key| vanished.contains(key)) {
            continue;
        }
        page::set_status(&path, page::Status::Stale)?;
        let key = paths::identity_key(root, &path)?;
        let updated = match store.get_page_meta(&key)? {
            Some(meta) => meta.updated,
            None => front.updated.to_string(),
        };
        store.upsert_page_meta(&PageMetaRow {
            page_key: key.clone(),
            hash: hash::hash_file(&path, true)?,
            updated,
        })?;
        stale.push(key);
    }
    stale.sort();
    stale.dedup();
    Ok(stale)
}

/// Whether `path` still hashes to the digest `scan` classified.
///
/// `None` means `scan` recorded no digest, so there is nothing to compare and the
/// source is taken. A file that vanished or cannot be read hashes to nothing:
/// that is not the digest `scan` saw, so the source waits for the next round
/// instead of ending this one.
fn still_the_same_bytes(path: &Path, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return true;
    };
    hash::hash_file(path, true).is_ok_and(|digest| digest == expected)
}

/// Seconds since the file was last modified, when it exists and the platform
/// reports a usable time.
fn quiet_seconds(path: &Path) -> Option<u64> {
    let modified = fs::metadata(path).ok()?.modified().ok()?;
    SystemTime::now()
        .duration_since(modified)
        .ok()
        .map(|age| age.as_secs())
}

/// Appends one round's entry to `wiki/audit/<today>-sync.md`.
///
/// The date is the local day, matching the convention of the other reports. A
/// second round on the same day appends rather than replaces: the file is the
/// day's history, and each entry names its own run id.
fn write_audit(root: &Path, cfg: &Config, report: &SyncReport) -> Result<()> {
    let dir = root.join(&cfg.paths.wiki).join("audit");
    fs::create_dir_all(&dir).map_err(|source| SyncError::Io {
        path: dir.clone(),
        source,
    })?;
    let path = dir.join(format!("{}-sync.md", Local::now().date_naive()));
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|source| SyncError::Io {
            path: path.clone(),
            source,
        })?;
    let mut text = String::new();
    if file.metadata().map(|meta| meta.len()).unwrap_or(0) > 0 {
        text.push_str("\n---\n\n");
    }
    text.push_str(&render_audit(report));
    file.write_all(text.as_bytes())
        .map_err(|source| SyncError::Io { path, source })
}

/// One markdown entry, as appended to the day's audit file.
fn render_audit(report: &SyncReport) -> String {
    let mut out = String::new();
    out.push_str(&format!("# sync {}\n\n", report.run_id));
    out.push_str(&format!("- run_id: {}\n", report.run_id));
    out.push_str(&format!("- converted: {}\n", report.converted));
    out.push_str(&format!("- ingested: {}\n", report.ingested.len()));
    out.push_str(&format!("- queued: {}\n", report.queued));
    out.push_str(&format!("- failures: {}\n", report.failures));

    out.push_str("\n## changes\n");
    if report.changes.is_empty() {
        out.push_str("- (none)\n");
    }
    for change in &report.changes {
        out.push_str(&format!("- {:?} {}\n", change.kind, change.key));
    }

    out.push_str("\n## 源消失导致转 stale 的页面\n");
    if report.stale_pages.is_empty() {
        out.push_str("- (none)\n");
    }
    for page in &report.stale_pages {
        out.push_str(&format!("- {page}\n"));
    }

    out.push_str("\n## ingested\n");
    if report.ingested.is_empty() {
        out.push_str("- (none)\n");
    }
    for key in &report.ingested {
        out.push_str(&format!("- {key}\n"));
    }

    out.push_str("\n## warnings\n");
    if report.warnings.is_empty() {
        out.push_str("- (none)\n");
    }
    for warning in &report.warnings {
        out.push_str(&format!("- {warning}\n"));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::still_the_same_bytes;
    use crate::hash;
    use std::fs;

    #[test]
    fn the_digest_from_a_different_body_is_not_the_same_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        fs::write(&path, "one").unwrap();
        let digest = hash::hash_file(&path, true).unwrap();
        assert!(still_the_same_bytes(&path, Some(&digest)));

        fs::write(&path, "two").unwrap();
        assert!(
            !still_the_same_bytes(&path, Some(&digest)),
            "a file that changed while the round ran waits for the next one"
        );
    }

    #[test]
    fn a_change_without_a_digest_is_taken() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a.md");
        assert!(still_the_same_bytes(&path, None));
    }
}
