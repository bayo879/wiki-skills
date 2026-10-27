//! Source-layer scan: classify what changed under `source/`.
//!
//! The scan compares the files currently in `source/` with the `sources` ledger
//! in `state.db` and reports one [`Change`] per affected key. It is read-only:
//! nothing is written to the store, so `sync` can decide what to do with the
//! report (and can call the scan twice without side effects).
//!
//! Classification runs in a fixed order, because the steps constrain each other:
//!
//! 1. Walk `source/` without following links and skip dot-paths.
//! 2. Files whose extension is not in `formats.allow` only bump
//!    [`ScanReport::ignored_count`]; they produce no change.
//! 3. Files above `parse.max_source_bytes` are `Oversize` and are never opened.
//! 4. Content digest: full for files up to `parse.chunk_threshold`, sampled
//!    above it.
//! 5. Join with the ledger by key: unknown is `New`, equal digest is unchanged,
//!    different digest is `Changed`.
//! 6. Deduplicate the `New` candidates by digest, keeping the lexicographically
//!    first key and demoting the rest to `Duplicate`. This must happen before
//!    move detection, otherwise a same-content copy would be mistaken for a
//!    moved file.
//! 7. A remaining `New` candidate whose digest matches a ledger key that has
//!    disappeared from disk is `Moved`, with that key in `moved_from`. Each
//!    ledger key can be claimed by at most one candidate.
//! 8. Ledger keys missing from disk and claimed by nobody are `Missing`.
//!
//! Only the digest decides whether a file changed; `mtime` is not part of the
//! judgement, so touching a file does not schedule a re-ingest. A digest that
//! moves across `chunk_threshold` switches from full to sampled and is reported
//! as `Changed` once, which is accepted.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use serde::Serialize;
use walkdir::WalkDir;

use crate::config::Config;
use crate::hash;
use crate::paths;
use crate::store::{SourceRow, Store, StoreError};

/// How one key differs from the ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChangeKind {
    /// A file the ledger does not know about.
    New,
    /// A known file whose content digest differs from the ledger.
    Changed,
    /// A `New` file whose digest matches a ledger key that vanished from disk.
    Moved,
    /// A ledger key with no file on disk.
    Missing,
    /// A `New` file with the same digest as another `New` file.
    Duplicate,
    /// A file whose extension is not in `formats.allow`; counted, not reported.
    Ignored,
    /// A file above `parse.max_source_bytes`; its content was not read.
    Oversize,
}

/// One classified key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    /// What happened to `key`.
    pub kind: ChangeKind,
    /// Path identity key relative to `source/`, `/`-separated and NFC.
    pub key: String,
    /// Content digest, or `None` for `Missing` and `Oversize`.
    pub hash: Option<String>,
    /// For `Moved`, the ledger key the content came from.
    pub moved_from: Option<String>,
}

/// Everything one scan found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ScanReport {
    /// The classified changes, sorted by key.
    pub changes: Vec<Change>,
    /// Number of files skipped because their extension is not allowed.
    pub ignored_count: usize,
}

/// Why a scan could not finish.
#[derive(Debug, thiserror::Error)]
pub enum ScanError {
    /// Walking `source/` failed.
    #[error(transparent)]
    Walk(#[from] walkdir::Error),
    /// A file could not be digested.
    #[error(transparent)]
    Hash(#[from] hash::HashError),
    /// A path could not be turned into an identity key.
    #[error(transparent)]
    Path(#[from] paths::PathError),
    /// The ledger could not be read.
    #[error(transparent)]
    Store(#[from] StoreError),
}

/// Result of the scan helpers.
pub type Result<T> = std::result::Result<T, ScanError>;

/// Digest outcome for one file on disk.
enum DiskDigest {
    /// `blake3:` digest of the content, full or sampled.
    Digest(String),
    /// Above the size cap; the file was not opened.
    Oversize,
}

/// One allowed file found under `source/`.
struct DiskFile {
    /// Path identity key relative to `source/`.
    key: String,
    /// What the scan learned about its content.
    digest: DiskDigest,
}

/// Scans `source/` under `root` and classifies every key against `store`.
///
/// `store` is only read; the function never writes to it.
pub fn scan_source(root: &Path, cfg: &Config, store: &Store) -> Result<ScanReport> {
    let source_root = root.join(&cfg.paths.source);
    let walked = walk_source(&source_root, cfg)?;
    let ledger = store.all_sources()?;
    let changes = classify(&walked.disk, &walked.present, &ledger);
    Ok(ScanReport {
        changes,
        ignored_count: walked.ignored_count,
    })
}

/// What one pass over `source/` found.
struct Walked {
    /// Allowed files, sorted by key.
    disk: Vec<DiskFile>,
    /// Every regular file seen, including ignored extensions. A ledger key in
    /// this set exists on disk and therefore cannot be reported as missing.
    present: BTreeSet<String>,
    /// Files skipped for their extension.
    ignored_count: usize,
}

/// Walks `source/` and digests the allowed files.
///
/// A missing `source/` directory is treated as an empty one: every ledger key is
/// then missing, which is what a deleted tree means.
fn walk_source(source_root: &Path, cfg: &Config) -> Result<Walked> {
    let mut disk = Vec::new();
    let mut present = BTreeSet::new();
    let mut ignored_count = 0usize;

    if source_root.is_dir() {
        let walker = WalkDir::new(source_root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| !is_ignored_path(source_root, entry));
        for entry in walker {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            let key = paths::identity_key(source_root, entry.path())?;
            present.insert(key.clone());
            if !extension_allowed(cfg, entry.path()) {
                ignored_count += 1;
                continue;
            }
            let size = entry.metadata()?.len();
            let digest = if size > cfg.parse.max_source_bytes {
                DiskDigest::Oversize
            } else if size <= cfg.parse.chunk_threshold {
                DiskDigest::Digest(hash::hash_file(entry.path(), true)?)
            } else {
                DiskDigest::Digest(hash::fingerprint(entry.path())?.sample)
            };
            disk.push(DiskFile { key, digest });
        }
    }

    // Walk order is filesystem order; every later step wants a stable one.
    disk.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(Walked {
        disk,
        present,
        ignored_count,
    })
}

/// Whether the walker should skip this entry and, for directories, not descend
/// into it: any relative component starting with `.` marks a dot-path.
fn is_ignored_path(source_root: &Path, entry: &walkdir::DirEntry) -> bool {
    match entry.path().strip_prefix(source_root) {
        Ok(rel) => paths::is_ignored(&paths::to_slash(rel)),
        Err(_) => false,
    }
}

/// Whether the file extension is whitelisted, ignoring ASCII case.
fn extension_allowed(cfg: &Config, path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    cfg.formats
        .allow
        .iter()
        .any(|allowed| allowed.eq_ignore_ascii_case(ext))
}

/// Applies the eight comparison steps to the walked files and the ledger.
fn classify(disk: &[DiskFile], present: &BTreeSet<String>, ledger: &[SourceRow]) -> Vec<Change> {
    let mut changes: Vec<Change> = Vec::new();
    let mut new_candidates: Vec<&DiskFile> = Vec::new();

    let mut by_key: HashMap<&str, &SourceRow> = HashMap::new();
    for row in ledger {
        by_key.insert(row.key.as_str(), row);
    }

    // Steps 3-5: oversize files first, then a ledger join by key and digest.
    for file in disk {
        match &file.digest {
            DiskDigest::Oversize => {
                changes.push(change(ChangeKind::Oversize, &file.key, None, None));
            }
            DiskDigest::Digest(digest) => match by_key.get(file.key.as_str()) {
                None => new_candidates.push(file),
                Some(row) if row.hash.as_str() == digest => {}
                Some(_) => changes.push(change(
                    ChangeKind::Changed,
                    &file.key,
                    Some(digest.clone()),
                    None,
                )),
            },
        }
    }

    // Step 6: one canonical key per digest among the new candidates.
    let mut canonical: HashMap<&str, &str> = HashMap::new();
    for file in &new_candidates {
        if let DiskDigest::Digest(digest) = &file.digest {
            canonical
                .entry(digest.as_str())
                .or_insert(file.key.as_str());
        }
    }
    let mut survivors: Vec<&DiskFile> = Vec::new();
    for file in new_candidates {
        let DiskDigest::Digest(digest) = &file.digest else {
            continue;
        };
        match canonical.get(digest.as_str()) {
            Some(canonical_key) if *canonical_key == file.key.as_str() => survivors.push(file),
            _ => changes.push(change(
                ChangeKind::Duplicate,
                &file.key,
                Some(digest.clone()),
                None,
            )),
        }
    }

    // Steps 7-8: what the ledger knows but nothing on disk accounts for.
    let mut unclaimed: Vec<&SourceRow> = ledger
        .iter()
        .filter(|row| !present.contains(&row.key))
        .collect();
    let mut claimed: BTreeSet<&str> = BTreeSet::new();
    for file in survivors {
        let DiskDigest::Digest(digest) = &file.digest else {
            continue;
        };
        match unclaimed.iter().position(|row| row.hash.as_str() == digest) {
            // The ledger is sorted by key, so the first match is deterministic,
            // and removing it enforces "one ledger key, one claim".
            Some(position) => {
                let row = unclaimed.remove(position);
                claimed.insert(row.key.as_str());
                changes.push(change(
                    ChangeKind::Moved,
                    &file.key,
                    Some(digest.clone()),
                    Some(row.key.clone()),
                ));
            }
            None => changes.push(change(
                ChangeKind::New,
                &file.key,
                Some(digest.clone()),
                None,
            )),
        }
    }
    for row in ledger {
        if present.contains(&row.key) || claimed.contains(row.key.as_str()) {
            continue;
        }
        changes.push(change(ChangeKind::Missing, &row.key, None, None));
    }

    // One key can only carry one change, so the sort is total.
    changes.sort_by(|a, b| a.key.cmp(&b.key));
    changes
}

/// Convenience constructor keeping the field order visible.
fn change(kind: ChangeKind, key: &str, hash: Option<String>, moved_from: Option<String>) -> Change {
    Change {
        kind,
        key: String::from(key),
        hash,
        moved_from,
    }
}
