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

use chrono::{Local, NaiveDate, SecondsFormat, Utc};
use serde::Serialize;

use crate::config::Config;
use crate::ledger::Ledger;
use crate::lint;
use crate::page;
use crate::scan::{self, ChangeKind, Layer};
use crate::store::{PageMetaRow, Store, TaskRow};
use crate::{convert, hash, lock, moves, paths, render, source_refs, task};

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
    /// Convert exactly these keys again, even when their content did not
    /// change. Empty means a normal round.
    pub reconvert: Vec<String>,
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
    /// Pages deleted because their last source vanished.
    pub removed_pages: Vec<String>,
    /// Sources that produced a fresh raw file.
    pub converted: usize,
    /// Sources handed to an ingest round.
    pub ingested: Vec<String>,
    pub work_items: Vec<crate::store::WorkItem>,
    pub affected_pages: Vec<String>,
    /// Sources left for the next round by `limit`.
    pub queued: usize,
    /// Those sources by name, in the order the next round will take them.
    pub waiting: Vec<String>,
    /// Everything worth telling a human about this round.
    pub warnings: Vec<String>,
    /// Normal maintenance events, safe to summarize instead of relaying.
    pub events: Vec<String>,
    /// Conversions that failed.
    pub failures: usize,
    /// The one task the next round should run, if any.
    pub next_task: Option<TaskRow>,
    /// Pages this round merged into the wiki.
    pub merged_pages: Vec<String>,
    /// Sources whose settlement completed this round.
    pub confirmed_sources: Vec<String>,
    /// Known problems the round could not settle.
    pub issues: Vec<String>,
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
    /// The task tree could not be advanced.
    #[error(transparent)]
    Task(#[from] task::TaskError),
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
            SyncError::Task(err) => err.code(),
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

    // 2. What changed on disk. A file whose extension has no backend is not a
    //    conversion failure and never becomes a work item, but leaving it out of
    //    the report is how a person concludes the wiki already holds it. Name
    //    each one; the format is the reason, so nobody expects text from it.
    let scanned = scan::scan_source_with_force(root, cfg, store, args.force)?;
    for key in &scanned.unsupported {
        warnings.push(format!(
            "unsupported: '{key}' was not converted: no backend reads this format"
        ));
    }
    let changes = scanned.changes;

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

    // 3b. Nothing gates the list: every pending source is visible and the
    //     agent decides how many it can write this time.

    // 3c. Order the queue for the budget below. Files that already failed go
    //     behind files nobody has tried yet: a source that breaks conversion
    //     would otherwise hold a slot every single round while healthy files
    //     waited for it to be repaired. A note whose file is gone orders
    //     nothing, so it goes rather than accumulating in the ledger.
    let mut live = Vec::new();
    for attempt in store.attempts()? {
        let path = paths::join_key(&root.join(&cfg.paths.source), &attempt.key);
        if path.exists() {
            live.push(attempt);
        } else if !args.dry_run {
            Ledger::new(store).forget_failure(&attempt.key)?;
        }
    }
    order_pending(&mut pending, &live);

    // 3d. A source parked in `failed` by three consecutive conversion failures
    //     stays visible but takes no budget: it is a person's decision now.
    let parked: std::collections::BTreeSet<String> = store
        .failed_converts()?
        .into_iter()
        .filter_map(|task| task.source_key)
        .collect();
    for key in &parked {
        warnings.push(format!(
            "failed: {key} is parked after 3 conversion failures; fix it and run `task retry`"
        ));
    }
    // 3e. The candidate list. A normal round takes what `scan` called new or
    //     changed; an explicit reconvert takes exactly the keys the caller
    //     named, changed or not. Each named key is checked to be a real,
    //     readable source first: a missing file, a format no backend reads, an
    //     oversize file or a source still settling is reported and skipped
    //     rather than turned into a failure row.
    let mut candidates: Vec<String> = if args.reconvert.is_empty() {
        pending.iter().map(|change| change.key.clone()).collect()
    } else {
        let mut seen = std::collections::BTreeSet::new();
        let mut keys = Vec::new();
        for key in &args.reconvert {
            if !seen.insert(key.clone()) {
                continue;
            }
            let path = paths::join_key(&root.join(&cfg.paths.source), key);
            if !path.is_file() {
                warnings.push(format!("reconvert: no source has the key '{key}'"));
                continue;
            }
            if !scan::extension_allowed(cfg, &path) {
                warnings.push(format!(
                    "reconvert: '{key}' was not converted: no backend reads this format"
                ));
                continue;
            }
            let size = std::fs::metadata(&path).map(|meta| meta.len()).unwrap_or(0);
            if size > cfg.parse.max_source_bytes {
                warnings.push(format!(
                    "oversize: '{key}' is above parse.max_source_bytes ({} bytes); it stays unconverted until the file is ignored or the limit is raised",
                    cfg.parse.max_source_bytes
                ));
                continue;
            }
            if !(args.force || cfg.ingest.settle_seconds == 0) {
                if let Some(age) = quiet_seconds(&path) {
                    if age < cfg.ingest.settle_seconds {
                        warnings.push(format!(
                            "settle: '{key}' was modified {age}s ago; waiting another {}s",
                            cfg.ingest.settle_seconds - age
                        ));
                        continue;
                    }
                }
            }
            keys.push(key.clone());
        }
        keys
    };
    candidates.retain(|key| !parked.contains(key));

    // 4. Convert, first in queue order, up to the budget.
    let planned = candidates.len();
    let budget = planned.min(args.limit);
    let waiting: Vec<String> = candidates.iter().skip(budget).cloned().collect();
    let queued = waiting.len();

    if args.dry_run {
        // A reconvert preview must answer the question the real run answers
        // first: is this raw still a machine product nobody touched? `preflight`
        // only reads the source digest, the raw frontmatter and the ledger, so
        // no parser runs and nothing is written.
        if !args.reconvert.is_empty() {
            for key in candidates.iter().take(budget) {
                match convert::preflight(root, cfg, store, key) {
                    Ok(Some(parser)) if parser.is_empty() => {
                        warnings.push(format!("dry run: '{key}' would be skipped (pinned)"))
                    }
                    Ok(Some(parser)) => warnings.push(format!(
                        "dry run: '{key}' would be skipped (pinned, {parser})"
                    )),
                    Ok(None) => warnings.push(format!("dry run: would reconvert '{key}'")),
                    Err(err) => warnings.push(format!("reconvert: '{key}': {err}")),
                }
            }
        }
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
            removed_pages: Vec::new(),
            converted: 0,
            ingested: Vec::new(),
            work_items: store.work_items()?,
            affected_pages: Vec::new(),
            queued,
            waiting,
            warnings,
            events: Vec::new(),
            failures: 0,
            next_task: None,
            merged_pages: Vec::new(),
            confirmed_sources: Vec::new(),
            issues: Vec::new(),
        });
    }

    // 4a. A rename is a move, not a delete plus an add: migrate it before
    //     anything else so the rest of the round already works on the new name.
    //     A collision is left untouched for a human; it is a finding (exit 1),
    //     never a reason to overwrite a file.
    let mut failures = 0;
    let mut events = Vec::new();
    let moved = moves::apply(root, cfg, store, &changes)?;
    for applied in &moved.applied {
        events.push(format!(
            "moved: {} -> {} ({} page(s) rewritten)",
            applied.from,
            applied.to,
            applied.pages.len()
        ));
    }
    for key in &moved.busy {
        warnings.push(format!(
            "moved: '{key}' was being edited; its old citation stays until the next round"
        ));
    }
    for collision in &moved.collisions {
        failures += 1;
        warnings.push(format!(
            "moved: {} -> {} skipped: {} ({})",
            collision.from, collision.to, collision.message, collision.code
        ));
    }

    // 4b. A source that disappeared leaves conclusions without a source
    //     behind them. What happens next is `[ingest].on_missing`: `forget`
    //     (the default) removes the reference, the raw file and the ledger
    //     row; `mark_stale` only moves the pages that cited it to `stale`.
    let (stale_pages, removed_pages) =
        settle_missing(root, cfg, store, &changes, &mut warnings, &mut failures)?;

    // 5. Convert. Reading and rendering are independent per source, so they
    //    run on a small thread pool; writing the raw file and the ledger rows
    //    stays on this thread because `Store` is not `Sync`.
    let mut converted = 0;
    let keys: Vec<String> = candidates.iter().take(budget).cloned().collect();
    let threads = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
        .min(4)
        .min(keys.len().max(1));
    let chunk = keys.len().div_ceil(threads).max(1);
    let prepared: Vec<(String, convert::Result<convert::Prepared>)> = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        for slice in keys.chunks(chunk) {
            handles.push(scope.spawn(move || {
                slice
                    .iter()
                    .map(|key| (key.clone(), convert::prepare(root, cfg, key)))
                    .collect::<Vec<_>>()
            }));
        }
        let mut out = Vec::new();
        for handle in handles {
            if let Ok(rows) = handle.join() {
                out.extend(rows);
            }
        }
        out
    });

    for (key, prepared) in prepared {
        let key = key.as_str();
        let outcome = match prepared {
            Ok(prepared) => match convert::commit(store, prepared) {
                Ok(outcome) => outcome,
                Err(err) => {
                    failures += 1;
                    let message = format!("{}: {err}", err.code());
                    warnings.push(format!("{key}: conversion failed ({message})"));
                    let (_, parked) = Ledger::new(store).record_failure(
                        key,
                        &Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                        &message,
                    )?;
                    if parked {
                        warnings.push(format!(
                            "failed: {key} is parked after 3 conversion failures"
                        ));
                    }
                    continue;
                }
            },
            Err(err) => {
                // One bad source is a finding, not the end of the round — and
                // not a reason to hand it the budget again next round.
                failures += 1;
                let message = format!("{}: {err}", err.code());
                warnings.push(format!("{key}: conversion failed ({message})"));
                let (_, parked) = Ledger::new(store).record_failure(
                    key,
                    &Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
                    &message,
                )?;
                if parked {
                    warnings.push(format!(
                        "failed: {key} is parked after 3 conversion failures"
                    ));
                }
                continue;
            }
        };
        if outcome.skipped {
            warnings.push(format!(
                "{key}: skipped ({})",
                outcome.skip_reason.as_deref().unwrap_or("no reason given")
            ));
        } else {
            Ledger::new(store).clear_failure(key)?;
            converted += 1;
        }
        for warning in &outcome.warnings {
            warnings.push(format!("{key}: {}: {}", warning.code, warning.message));
        }
    }

    // The durable queue, not a staging round, is the handoff to the agent.
    if !args.dry_run {
        warnings.extend(crate::worklist::import_legacy(root, cfg, store)?);
    }
    let work_items = store.work_items()?;
    // Every converted source gets exactly one root, and the tree says what the
    // next round should run. A root keeps its baseline across rounds, so this
    // call is idempotent after the first one.
    let stamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    for item in &work_items {
        task::ensure_root(root, cfg, store, item, &stamp)?;
    }
    let settled = task::advance(root, cfg, store)?;
    let next_task = settled.task.clone();
    let ingested = work_items
        .iter()
        .map(|item| item.source_key.clone())
        .collect();
    let wanted: Vec<String> = work_items.iter().map(|item| item.raw_key.clone()).collect();
    let affected_pages = if wanted.is_empty() {
        Vec::new()
    } else {
        // `advance` just refreshed the index, so this is the same reverse
        // lookup as `query --source`, not a second page scan.
        task::affected_pages(store, &wanted)?
    };

    // 5b. A page the round just handed to an agent is checked before the next
    //     round trusts it. Only L12/L13 — the two findings that mean "this page
    //     is not finished" — and only for the pages this round touched, so a
    //     quiet round stays quiet and `lint` keeps owning the full contract.
    for key in &affected_pages {
        let path = paths::join_key(root, key);
        let Ok((front, body)) = page::read(&path) else {
            continue;
        };
        for finding in lint::page_quality(key, &front, &body) {
            warnings.push(format!("page: '{key}': {}", finding.msg));
        }
    }

    // 6. The index always reflects the raw layer, even on a quiet round.
    render::write_index(root, cfg)?;

    let report = SyncReport {
        run_id,
        changes,
        stale_pages,
        removed_pages,
        converted,
        ingested,
        work_items,
        affected_pages,
        queued,
        waiting,
        warnings,
        events,
        failures,
        next_task,
        merged_pages: settled.merged_pages,
        confirmed_sources: settled.confirmed_sources,
        issues: settled.issues,
    };

    // 7. The audit entry: what changed, what was converted, what was handed to
    //    ingest, and what this round could not do.
    write_audit(root, cfg, &report)?;

    Ok(report)
}

/// Settles the sources that vanished from disk, and returns the pages that went
/// stale because of it.
///
/// `[ingest].on_missing` picks the policy, per source:
///
/// - `mark_stale` moves the pages that cited the source to `stale` and leaves
///   every other byte alone. It is the conservative setting: nothing but a
///   status line changes, which suits a repository that treats `.wiki/` as
///   precious.
/// - `forget` (the default) settles the ledger instead of leaving a dangling
///   reference forever. Each citing page drops that one entry from `sources`; a
///   page left with no source at all is deleted; the raw file and the ledger row
///   (with its work item and cached facts) go too.
/// - `ignore` does nothing at all.
///
/// Forgetting is retryable by construction: the pages and the raw are cleaned
/// first and the ledger row is dropped last, so a failure half way leaves the
/// source in the ledger and the next round runs the same steps. Deleting a page
/// is the only irreversible step — that is why `mark_stale` still exists.
///
/// The page's own `sources:` list is the truth here, not the ledger's mapping: a
/// page may cite a raw the ledger no longer has, and it must still be cleaned.
fn settle_missing(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    changes: &[scan::Change],
    warnings: &mut Vec<String>,
    failures: &mut usize,
) -> Result<(Vec<String>, Vec<String>)> {
    if cfg.ingest.on_missing == "ignore" {
        return Ok((Vec::new(), Vec::new()));
    }

    let mut stale: Vec<String> = Vec::new();
    let mut deleted: Vec<String> = Vec::new();
    for change in changes {
        if change.kind != ChangeKind::Missing || change.layer != Layer::Source {
            continue;
        }
        let key = change.key.clone();
        // A source the ledger already forgot cannot vanish a second time.
        let Some(row) = store.get_source(&key)? else {
            continue;
        };
        match settle_one(root, cfg, store, &key, row.raw_key.as_deref()) {
            Ok(settled) => {
                stale.extend(settled.stale.iter().cloned());
                deleted.extend(settled.deleted.iter().cloned());
                if let Some(line) = settled.describe(&key) {
                    warnings.push(line);
                }
            }
            Err(err) => {
                // One source that cannot be settled is a finding, not the end of
                // the round; the ledger row stays, so the next round retries.
                *failures += 1;
                warnings.push(format!(
                    "vanished: '{key}' could not be settled ({}) {err}",
                    err.code()
                ));
            }
        }
    }
    stale.sort();
    stale.dedup();
    deleted.sort();
    deleted.dedup();
    Ok((stale, deleted))
}

/// What settling one vanished source did.
#[derive(Debug, Default)]
struct Settled {
    /// Pages that lost the citation and kept at least one other source.
    stale: Vec<String>,
    /// Pages deleted because the vanished source was their last one.
    deleted: Vec<String>,
    /// Pending work items dropped together with the source.
    work_items: usize,
    /// Whether a raw file was removed.
    raw_dropped: bool,
    /// The source never converted, so only its ledger row went.
    ledger_only: bool,
    /// Pages the author was editing while the round ran: untouched, retried next
    /// round rather than overwritten.
    busy: Vec<String>,
}

impl Settled {
    /// One line for the round's warnings, or `None` when the conservative
    /// policy ran and the round already reports the stale pages structurally.
    fn describe(&self, key: &str) -> Option<String> {
        if self.ledger_only {
            return Some(format!(
                "vanished: '{key}' -> never converted; ledger row dropped"
            ));
        }
        if self.stale.is_empty()
            && self.deleted.is_empty()
            && self.busy.is_empty()
            && !self.raw_dropped
        {
            return None;
        }
        let mut detail: Vec<String> = Vec::new();
        if self.raw_dropped {
            detail.push(String::from("raw dropped"));
        }
        if self.work_items > 0 {
            detail.push(format!("{} work item(s) dropped", self.work_items));
        }
        if !self.stale.is_empty() {
            detail.push(format!("{} page(s) lost the citation", self.stale.len()));
        }
        if !self.deleted.is_empty() {
            detail.push(format!(
                "{} page(s) deleted with no source left: {}",
                self.deleted.len(),
                self.deleted.join(", ")
            ));
        }
        if !self.busy.is_empty() {
            detail.push(format!(
                "{} page(s) were being edited and were left for the next round: {}",
                self.busy.len(),
                self.busy.join(", ")
            ));
        }
        Some(format!("vanished: '{key}' -> {}", detail.join("; ")))
    }
}

/// Applies `on_missing` to one vanished source.
fn settle_one(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    key: &str,
    raw_key: Option<&str>,
) -> Result<Settled> {
    let forget = cfg.ingest.on_missing == "forget";
    let mut settled = Settled::default();

    // A source that never converted left no raw file, so no page can cite it;
    // there is nothing to clean but the ledger row itself.
    let Some(raw_key) = raw_key else {
        if forget {
            Ledger::new(store).forget_source(key, None)?;
            settled.ledger_only = true;
        }
        return Ok(settled);
    };
    // `sources.raw_key` is the repository path (`.wiki/raw/a.md`); the `raws`
    // table and the page's citation key are `raw/`-relative (`a.md`).
    let raw_rel =
        source_refs::source_key(raw_key, &cfg.paths.raw).unwrap_or_else(|| raw_key.to_string());

    for path in moves::page_files(root, cfg) {
        let (front, _) = match page::read(&path) {
            Ok(read) => read,
            // A file without frontmatter is not a page; lint owns that finding.
            Err(page::PageError::BadFrontmatter { .. }) => continue,
            Err(err) => return Err(SyncError::Page(err)),
        };
        if !source_refs::source_keys(&front.sources, &cfg.paths.raw)
            .iter()
            .any(|cited| cited == &raw_rel)
        {
            continue;
        }
        let page_key = paths::identity_key(root, &path)?;

        if !forget {
            // Conservative: only the status line moves, and only for a page the
            // reader has already seen as `stable`. A draft is not promoted into
            // a staleness nobody published, and an already-stale page is not
            // touched again.
            if front.status == page::Status::Stable {
                match page::set_status(&path, page::Status::Stale) {
                    Ok(()) => {}
                    Err(page::PageError::Changed { .. }) => {
                        settled.busy.push(page_key);
                        continue;
                    }
                    Err(err) => return Err(SyncError::Page(err)),
                }
                refresh_page_meta(root, store, &path, &page_key, front.updated)?;
                settled.stale.push(page_key);
            }
            continue;
        }

        // Drop exactly this citation; every other source the page cites stays.
        let (removed, _) =
            match page::replace_sources(&path, std::slice::from_ref(&raw_key.to_string()), &[]) {
                Ok(counts) => counts,
                Err(page::PageError::Changed { .. }) => {
                    settled.busy.push(page_key);
                    continue;
                }
                Err(err) => return Err(SyncError::Page(err)),
            };
        if removed == 0 {
            continue;
        }
        let (front, _) = page::read(&path)?;
        if front.sources.is_empty() {
            // The page stood on this one source. With the source gone it has
            // nothing behind it, and an orphan left in place would keep being
            // reported by L6 and `orphan_pages` forever.
            fs::remove_file(&path).map_err(|source| SyncError::Io {
                path: path.clone(),
                source,
            })?;
            settled.deleted.push(page_key);
            continue;
        }
        if front.status == page::Status::Stable {
            match page::set_status(&path, page::Status::Stale) {
                Ok(()) => {}
                Err(page::PageError::Changed { .. }) => {
                    settled.busy.push(page_key);
                    continue;
                }
                Err(err) => return Err(SyncError::Page(err)),
            }
        }
        refresh_page_meta(root, store, &path, &page_key, front.updated)?;
        settled.stale.push(page_key);
    }

    if forget {
        settled.work_items = store
            .work_items()?
            .iter()
            .filter(|item| item.source_key == key)
            .count();
        let raw_path = paths::join_key(root, raw_key);
        match fs::remove_file(&raw_path) {
            Ok(()) => settled.raw_dropped = true,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(source) => {
                return Err(SyncError::Io {
                    path: raw_path,
                    source,
                })
            }
        }
        Ledger::new(store).forget_source(key, Some(&raw_rel))?;
    }
    Ok(settled)
}

/// Keeps the index's change detection in step with a page whose frontmatter the
/// round just rewrote: the digest moves, the `updated` date does not, so L9
/// cannot read a status change as undated drift.
fn refresh_page_meta(
    root: &Path,
    store: &mut Store,
    path: &Path,
    page_key: &str,
    updated: NaiveDate,
) -> Result<()> {
    let _ = root;
    let updated = match store.get_page_meta(page_key)? {
        Some(meta) => meta.updated,
        None => updated.to_string(),
    };
    store.upsert_page_meta(&PageMetaRow {
        page_key: page_key.to_owned(),
        hash: hash::hash_file(path, true)?,
        updated,
    })?;
    Ok(())
}

/// Puts sources that already failed behind sources nobody has tried yet.
///
/// `scan` hands back changes in key order, and the budget takes the first N, so
/// without this a file that breaks conversion occupies a slot every round while
/// healthy files queue behind it. A source that has never failed sorts as
/// `""` — ahead of every recorded attempt; a source that just failed carries the
/// newest timestamp and sinks to the back. Key order breaks ties, so the queue
/// is deterministic and a failing file still comes back once nothing healthier
/// is left.
fn order_pending(pending: &mut [&scan::Change], attempts: &[crate::store::Attempt]) {
    let failed: std::collections::HashMap<&str, &str> = attempts
        .iter()
        .map(|attempt| (attempt.key.as_str(), attempt.attempted_at.as_str()))
        .collect();
    pending.sort_by(|a, b| {
        let a_at = failed.get(a.key.as_str()).copied().unwrap_or("");
        let b_at = failed.get(b.key.as_str()).copied().unwrap_or("");
        a_at.cmp(b_at).then_with(|| a.key.cmp(&b.key))
    });
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

    fn change(key: &str) -> crate::scan::Change {
        crate::scan::Change {
            kind: crate::scan::ChangeKind::New,
            key: String::from(key),
            hash: None,
            moved_from: None,
            layer: crate::scan::Layer::Source,
        }
    }

    fn attempt(key: &str, at: &str) -> crate::store::Attempt {
        crate::store::Attempt {
            key: String::from(key),
            attempted_at: String::from(at),
            error: String::from("E_PARSE: broken"),
        }
    }

    #[test]
    fn a_failed_source_sinks_behind_untried_ones_but_keeps_its_turn() {
        let a = change("a.md");
        let b = change("b.md");
        let c = change("c.md");
        let mut pending = vec![&a, &b, &c];
        let attempts = [attempt("a.md", "2026-11-09T10:00:00Z")];

        super::order_pending(&mut pending, &attempts);
        let order: Vec<&str> = pending.iter().map(|change| change.key.as_str()).collect();
        assert_eq!(
            order,
            vec!["b.md", "c.md", "a.md"],
            "a source that failed waits behind the ones nobody has tried"
        );

        // With nothing healthier left, the failed source comes back rather
        // than being dropped: this is an ordering, not a filter.
        let mut only_failed = vec![&a];
        super::order_pending(&mut only_failed, &attempts);
        assert_eq!(only_failed.len(), 1);
    }

    #[test]
    fn the_oldest_failure_comes_back_before_the_newest() {
        let a = change("a.md");
        let b = change("b.md");
        let mut pending = vec![&b, &a];
        let attempts = [
            attempt("a.md", "2026-11-01T10:00:00Z"),
            attempt("b.md", "2026-11-09T10:00:00Z"),
        ];
        super::order_pending(&mut pending, &attempts);
        assert_eq!(pending[0].key, "a.md");
        assert_eq!(pending[1].key, "b.md");
    }
}
