//! Version snapshots and the four-way decision gate.
//!
//! `wiki/` is the current approved version; `.wiki/versions/vNNN/` holds the
//! history. A snapshot is a real byte copy of the whole wiki, bookkeeping and
//! audit output included, so it needs no git and no re-derivation to read.
//! Copies are never hard links: editing a snapshot must not reach the approved
//! tree, and editing `wiki/` must not rewrite history.
//!
//! The decision gate has four outcomes. `approve` publishes the staged
//! candidates and advances the baseline, `reject` throws them away and records
//! why, `instruct` leaves free text for the next agent session, and `defer`
//! keeps the queue for later. A merge is only allowed while every candidate
//! still matches what `ingest commit` wrote and every pending source still
//! matches the hash the round recorded: a hand edit or a changed source makes
//! the merge unsafe, so it is refused instead of silently published.
//!
//! The snapshot directory is fixed at `.wiki/versions` because the engine
//! location is part of the repository layout that `init` creates, matching how
//! [`crate::staging`] locates its directory.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{Local, NaiveDate, Utc};
use serde::Serialize;
use walkdir::WalkDir;

use crate::config::Config;
use crate::hash;
use crate::paths;
use crate::render::{self, LogEntry};
use crate::staging::{self, Manifest};
use crate::store::Store;

/// Marker file that makes a directory in `.wiki/versions` ours to prune.
///
/// A directory without it may be a hand-made backup, and is never deleted.
const VERSION_FILE: &str = ".version.json";

/// The engine directory that holds snapshots and the pending instruction.
const ENGINE: &str = ".wiki";

/// What a call to [`approve`] decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApproveOutcome {
    /// The candidates were published and a new version now exists.
    Applied {
        /// Name of the version the merge produced.
        version: String,
    },
    /// The merge is ready but a human has not confirmed it yet.
    NeedsConfirmation {
        /// Stable reason code: `merge_staging`.
        reason: String,
        /// Human-readable explanation.
        detail: String,
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
    /// The index or the log could not be regenerated.
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
            VersionError::Render(err) => err.code(),
            VersionError::Db(_) => "E_DB",
        }
    }
}

/// Result of the decision helpers.
pub type Result<T> = std::result::Result<T, VersionError>;

/// The record written into every snapshot.
#[derive(Debug, Serialize)]
struct VersionRecord {
    /// Why the snapshot was taken; free text.
    label: String,
    /// UTC creation time, `YYYY-MM-DDTHH:MM:SSZ`.
    created_at: String,
    /// Number of pages in the snapshot, bookkeeping excluded.
    pages: usize,
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

/// Publishes the staging candidates into `wiki/`, or asks for confirmation.
///
/// The checks run in this order, and a refusal never writes anything: nothing
/// to approve, a candidate that drifted after it was written, a pending source
/// that changed after it was processed, and only then the confirmation gate.
/// `confirm == true` renames the candidates into `wiki/`, regenerates the
/// index, logs the merge, snapshots the result and advances the baseline.
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
    if !confirm {
        return Ok(ApproveOutcome::NeedsConfirmation {
            reason: String::from("merge_staging"),
            detail: format!("{} 页待合并到 wiki/", candidates.len()),
        });
    }

    let name = next_version_name(root)?;
    for candidate in &candidates {
        if let Some(parent) = candidate.wiki.parent() {
            fs::create_dir_all(parent).map_err(|source| io(parent, source))?;
        }
        fs::rename(&candidate.staging, &candidate.wiki)
            .map_err(|source| io(&candidate.staging, source))?;
    }
    render::write_index(root, cfg)?;
    // The version name is known before the snapshot, so the merge line is part
    // of the snapshot it describes.
    render::append_log(
        &root.join(&cfg.paths.wiki).join("log.md"),
        &LogEntry {
            date: today(),
            action: String::from("approve"),
            title: name.clone(),
            detail: None,
        },
    )?;
    snapshot(root, cfg, store, &name, "approve", "approve")?;

    let mut manifest = Manifest::load(root)?;
    for entry in manifest.sources.values_mut() {
        entry.baseline_hash = entry.current_hash.clone();
    }
    Manifest::save(root, &manifest)?;
    staging::clear_pending(root)?;

    Ok(ApproveOutcome::Applied { version: name })
}

/// Discards the candidates and records why.
///
/// The staged tree is restored to a mirror of `wiki/`: a page the wiki does not
/// have is deleted, a page whose bytes differ is overwritten with the approved
/// bytes, and a page missing from staging is copied back. The baseline does not
/// move, so the next round reopens the same work.
pub fn reject(root: &Path, cfg: &Config, _store: &mut Store, reason: &str) -> Result<()> {
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

    let mut manifest = Manifest::load(root)?;
    for entry in manifest.sources.values_mut() {
        entry.current_hash = entry.baseline_hash.clone();
    }
    Manifest::save(root, &manifest)?;
    staging::clear_pending(root)?;
    render::append_log(
        &wiki_dir.join("log.md"),
        &LogEntry {
            date: today(),
            action: String::from("reject"),
            title: String::from(reason),
            detail: None,
        },
    )?;
    Ok(())
}

/// Leaves a free-text instruction for the next agent session.
///
/// The text is written verbatim to `.wiki/next-instruction.md`; a single
/// trailing newline is added when the text does not end with one, and nothing
/// else about the file is touched. The instruction is guidance for the agent,
/// not a version-ledger entry, so it is deliberately not snapshotted here.
pub fn instruct(root: &Path, cfg: &Config, _store: &mut Store, text: &str) -> Result<()> {
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
    render::append_log(
        &root.join(&cfg.paths.wiki).join("log.md"),
        &LogEntry {
            date: today(),
            action: String::from("instruct"),
            title: String::from(text),
            detail: None,
        },
    )?;
    Ok(())
}

/// Postpones the decision without changing any state.
///
/// Staging, the ledger and `wiki/` are left exactly as they were; only the log
/// gains a line, which is how a later reader tells "deferred" from "never
/// looked at".
pub fn defer(root: &Path, cfg: &Config, _store: &mut Store) -> Result<()> {
    render::append_log(
        &root.join(&cfg.paths.wiki).join("log.md"),
        &LogEntry {
            date: today(),
            action: String::from("defer"),
            title: String::new(),
            detail: None,
        },
    )?;
    Ok(())
}

/// The snapshot directory for `root`.
fn versions_dir(root: &Path) -> PathBuf {
    root.join(ENGINE).join("versions")
}

/// Today's date, matching the page and log stamps the other commands write.
fn today() -> NaiveDate {
    Local::now().date_naive()
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

/// Copies the wiki into `.wiki/versions/<name>/` and records what it holds.
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
    copy_tree(&root.join(&cfg.paths.wiki), &dir)?;

    let created_at = Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    let record = VersionRecord {
        label: String::from(label),
        created_at: created_at.clone(),
        pages: render::page_files(&dir)?.len(),
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
    owned.sort_by(|left, right| right.0.cmp(&left.0));
    for (_, name) in owned.into_iter().skip(keep) {
        let victim = dir.join(&name);
        fs::remove_dir_all(&victim).map_err(|source| io(&victim, source))?;
    }
    Ok(())
}

/// Copies every regular file of `src` into `dst`, recreating the directories.
fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    if !src.is_dir() {
        return Ok(());
    }
    for entry in WalkDir::new(src).follow_links(false).sort_by_file_name() {
        let entry = entry.map_err(|err| walk_error(src, err))?;
        let rel = entry.path().strip_prefix(src).unwrap_or(entry.path());
        let target = dst.join(rel);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target).map_err(|source| io(&target, source))?;
            continue;
        }
        if !entry.file_type().is_file() {
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|source| io(parent, source))?;
        }
        // `fs::copy` copies bytes; sharing an inode would let a snapshot edit
        // rewrite the approved page.
        fs::copy(entry.path(), &target).map_err(|source| io(&target, source))?;
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
