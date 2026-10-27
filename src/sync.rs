//! `sync`: one unattended round over the repository.
//!
//! The round is a fixed pipeline: take the lock, scan, drop what is still being
//! written, convert within the budget, observe how long each raw file has been
//! stable, hand the stable ones to an ingest round, render the index and write
//! the audit entry. Every step is allowed to be uninteresting; only a failure to
//! take the lock or to write the derived state ends the round.
//!
//! Three cross-round observations live in the `sync_state` table, never in
//! `wiki.toml`:
//!
//! - `hash:<source>` and `streak:<source>` — the raw digest seen last round and
//!   how many consecutive rounds it has been identical. `auto_ingest` only fires
//!   at `stable_streak`.
//! - `fail:<source>` — consecutive failed conversions. Three of them close the
//!   gate (`auto_ingest_disabled=1`) until a human clears the key; a scheduled
//!   round that keeps failing should stop handing half-converted trees to review.
//!
//! `--dry-run` walks the same path up to the conversion budget and reports what
//! it *would* do. It writes no file (the lock is taken and removed again) and
//! never touches `sync_state`.

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
use crate::{convert, hash, ingest, lock, moves, paths, render, staging};

/// `sync_state` key that closes the auto-ingest gate until a human clears it.
pub const AUTO_INGEST_DISABLED: &str = "auto_ingest_disabled";

/// How many consecutive failed conversions close the auto-ingest gate.
const FAILURE_LIMIT: u32 = 3;

/// Knobs for one round, all of them from the command line.
#[derive(Debug, Clone)]
pub struct SyncArgs {
    /// Convert at most this many pending sources this round.
    pub limit: usize,
    /// Report what would happen and write nothing.
    pub dry_run: bool,
    /// Ignore the settle window and the stable-streak gate.
    pub force: bool,
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
    let lock = lock::acquire(root)?;
    let run_id = lock.run_id().to_string();
    let mut warnings = Vec::new();
    if lock.reclaimed() {
        warnings.push(format!(
            "stale lock {} reclaimed",
            root.join(".wiki/lock").display()
        ));
    }

    // The gate is read once: a run that closes it must not use it in the same
    // round, which is what "the next run sees the flag" means in practice.
    let gate_closed = store.sync_state_get(AUTO_INGEST_DISABLED)?.as_deref() == Some("1");
    if gate_closed {
        warnings.push(format!(
            "auto_ingest is off because `{AUTO_INGEST_DISABLED}` is set in the ledger; \
             clear that key to re-enable it"
        ));
    }
    let auto_ingest = cfg.sync.auto_ingest && !gate_closed;

    // 2. What changed on disk.
    let changes = scan::scan_source(root, cfg, store)?.changes;

    // 3. The settle window: a file written moments ago may still be growing, so
    //    it waits for the next round. `--force` is the human override.
    let mut pending: Vec<&scan::Change> = Vec::new();
    for change in &changes {
        if !matches!(change.kind, ChangeKind::New | ChangeKind::Changed) {
            continue;
        }
        if args.force || cfg.sync.settle_seconds == 0 {
            pending.push(change);
            continue;
        }
        let path = paths::join_key(&root.join(&cfg.paths.source), &change.key);
        match quiet_seconds(&path) {
            Some(age) if age < cfg.sync.settle_seconds => warnings.push(format!(
                "settle: '{}' was modified {age}s ago; waiting another {}s",
                change.key,
                cfg.sync.settle_seconds - age
            )),
            _ => pending.push(change),
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
                // A success ends a failure streak; the reminder of the previous
                // failures is not allowed to outlive the fix.
                store.sync_state_set(&format!("fail:{key}"), "0")?;
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
                let fail_key = format!("fail:{key}");
                let count = store
                    .sync_state_get(&fail_key)?
                    .and_then(|value| value.parse::<u32>().ok())
                    .unwrap_or(0)
                    + 1;
                store.sync_state_set(&fail_key, &count.to_string())?;
                warnings.push(format!("{key}: conversion failed ({}) {err}", err.code()));
            }
        }
    }

    // 5. How long each raw file has been stable. The comparison is against the
    //    last digest this engine saw, so an untouched source keeps counting even
    //    in rounds that converted nothing.
    let mut stable: Vec<String> = Vec::new();
    for row in store.all_sources()? {
        let Some(raw_key) = row.raw_key.as_deref() else {
            continue;
        };
        let raw_path = paths::join_key(root, raw_key);
        if !raw_path.is_file() {
            continue;
        }
        let digest = hash::hash_file(&raw_path, true)?;
        let hash_key = format!("hash:{}", row.key);
        let streak_key = format!("streak:{}", row.key);
        let previous = store.sync_state_get(&hash_key)?;
        let streak = if previous.as_deref() == Some(digest.as_str()) {
            store
                .sync_state_get(&streak_key)?
                .and_then(|value| value.parse::<u32>().ok())
                .unwrap_or(0)
                + 1
        } else {
            store.sync_state_set(&hash_key, &digest)?;
            1
        };
        store.sync_state_set(&streak_key, &streak.to_string())?;
        if auto_ingest && (args.force || streak >= cfg.sync.stable_streak) {
            stable.push(row.key.clone());
        }
    }
    stable.sort();

    // 6. Hand the stable sources to one ingest round. A failure here is a
    //    warning: the raw layer is intact and the next round retries.
    let ingested = if stable.is_empty() {
        Vec::new()
    } else {
        let input = ingest::BeginInput {
            sources: stable.clone(),
            pages: Vec::new(),
            dry_run: false,
        };
        match ingest::begin(root, cfg, store, &input) {
            Ok(_) => {
                for key in &stable {
                    store.sync_state_set(&format!("streak:{key}"), "0")?;
                }
                stable
            }
            Err(err) => {
                warnings.push(format!("ingest begin failed: {err}"));
                Vec::new()
            }
        }
    };

    // 7. The index always reflects the raw layer, even on a quiet round.
    render::write_index(root, cfg)?;

    // 8. Failure backoff. Written before the audit so the entry explains the
    //    closing of the gate in the same round it happens.
    if !gate_closed {
        for (key, value) in store.sync_state_entries()? {
            let Some(source) = key.strip_prefix("fail:") else {
                continue;
            };
            if value.parse::<u32>().unwrap_or(0) >= FAILURE_LIMIT {
                store.sync_state_set(AUTO_INGEST_DISABLED, "1")?;
                warnings.push(format!(
                    "{source} failed {value} rounds in a row; auto_ingest disabled until \
                     `{AUTO_INGEST_DISABLED}` is cleared from the ledger"
                ));
                break;
            }
        }
    }

    let report = SyncReport {
        run_id,
        changes,
        stale_pages,
        converted,
        ingested,
        queued,
        warnings,
        failures,
    };

    // 9. The audit entry: what changed, what was converted, what was handed to
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
    if cfg.sync.on_missing != "mark_stale" {
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
        let cited = staging::source_keys(&front.sources, &cfg.paths.raw);
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
