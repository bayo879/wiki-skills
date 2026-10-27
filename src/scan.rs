//! Layer scans: classify what changed under `source/` and under `raw/`.
//!
//! [`scan_source`] compares the files currently in `source/` with the `sources`
//! ledger in `state.db`; [`scan_raw`] does the same for the normalised markdown
//! in `raw/` against the `raws` ledger. Both report one [`Change`] per affected
//! key, and both are read-only: nothing is written to the store, so `sync` can
//! decide what to do with the report (and can call a scan twice without side
//! effects).
//!
//! The raw layer is written by `convert`, which records the digest of the exact
//! bytes it put on disk. `scan_raw` recomputes the full digest of every file —
//! raw files are text, so sampling would hide hand edits — and reports a file
//! whose digest no longer matches its row as `Changed`, even when the frontmatter
//! says `pin: true` (the pin belongs to `convert` and to lint, not to this scan).
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
use crate::store::{RawRow, SourceRow, Store, StoreError};

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

/// Which layer a change was found in.
///
/// Both layers share one key space in the merged report, so a caller that only
/// cares about one half filters on this rather than guessing from the key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    /// A file under `source/`, classified against the `sources` ledger.
    Source,
    /// A file under `raw/`, classified against the `raws` ledger.
    Raw,
}

/// One classified key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    /// What happened to `key`.
    pub kind: ChangeKind,
    /// Path identity key relative to `source/` or `raw/`, `/`-separated, NFC.
    pub key: String,
    /// Content digest, or `None` for `Missing` and `Oversize`.
    pub hash: Option<String>,
    /// For `Moved`, the ledger key the content came from.
    pub moved_from: Option<String>,
    /// The layer `key` was found in.
    pub layer: Layer,
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

/// Scans `raw/` under `root` and classifies every key against the `raws` ledger.
///
/// `store` is only read; the function never writes to it. The raw layer has no
/// extension whitelist or size cap — `convert` owns everything under `raw/` — so
/// `ignored_count` is always zero.
pub fn scan_raw(root: &Path, cfg: &Config, store: &Store) -> Result<ScanReport> {
    let raw_root = root.join(&cfg.paths.raw);
    let disk = walk_raw(&raw_root)?;
    let ledger = store.all_raws()?;
    Ok(ScanReport {
        changes: classify_raw(&disk, &ledger),
        ignored_count: 0,
    })
}

/// Scans both layers and merges their reports.
///
/// `changes` is the union of the two layers, ordered by key and then by layer so
/// the same report comes out every time; `ignored_count` is the sum, which only
/// the source layer can raise. Like its two halves this is read-only, so `scan`
/// may run it as often as it likes.
pub fn scan_all(root: &Path, cfg: &Config, store: &Store) -> Result<ScanReport> {
    let source = scan_source(root, cfg, store)?;
    let raw = scan_raw(root, cfg, store)?;
    let mut changes = source.changes;
    changes.extend(raw.changes);
    changes.sort_by(|a, b| a.key.cmp(&b.key).then_with(|| a.layer.cmp(&b.layer)));
    Ok(ScanReport {
        changes,
        ignored_count: source.ignored_count + raw.ignored_count,
    })
}

/// One raw file found under `raw/`, with the digest of its full content.
struct RawFile {
    /// Path identity key relative to `raw/`.
    key: String,
    /// `blake3:` digest of every byte on disk, frontmatter included.
    hash: String,
}

/// Walks `raw/` and digests every file completely.
///
/// A missing `raw/` directory is treated as an empty one: every ledger key is
/// then missing, which is what a deleted tree means.
fn walk_raw(raw_root: &Path) -> Result<Vec<RawFile>> {
    let mut disk = Vec::new();
    if raw_root.is_dir() {
        let walker = WalkDir::new(raw_root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| !is_ignored_path(raw_root, entry));
        for entry in walker {
            let entry = entry?;
            if !entry.file_type().is_file() {
                continue;
            }
            disk.push(RawFile {
                key: paths::identity_key(raw_root, entry.path())?,
                hash: hash::hash_file(entry.path(), true)?,
            });
        }
    }
    // Walk order is filesystem order; every later step wants a stable one.
    disk.sort_by(|a, b| a.key.cmp(&b.key));
    Ok(disk)
}

/// Applies the raw-layer comparison to the walked files and the ledger.
///
/// A key the ledger knows is judged by its digest alone: `converted_at` moving
/// forward is not a change. A key the ledger does not know is `New`, unless
/// another new file already carries the same digest — then the lexicographically
/// first key is kept and the rest are `Duplicate`.
///
/// Deduplication deliberately ignores files the ledger already tracks, the same
/// way [`classify`] restricts its own duplicate rule to the `New` candidates: a
/// tracked raw that still matches its row is never demoted, so a duplicated
/// content pair cannot make `sync` delete and re-convert the same file forever.
fn classify_raw(disk: &[RawFile], ledger: &[RawRow]) -> Vec<Change> {
    let mut changes: Vec<Change> = Vec::new();
    let mut by_key: HashMap<&str, &RawRow> = HashMap::new();
    for row in ledger {
        by_key.insert(row.key.as_str(), row);
    }

    let mut new_candidates: Vec<&RawFile> = Vec::new();
    for file in disk {
        match by_key.get(file.key.as_str()) {
            None => new_candidates.push(file),
            Some(row) if row.hash == file.hash => {}
            Some(_) => changes.push(change(
                Layer::Raw,
                ChangeKind::Changed,
                &file.key,
                Some(file.hash.clone()),
                None,
            )),
        }
    }

    // `disk` is sorted, so the first key seen for a digest is the smallest one.
    let mut canonical: HashMap<&str, &str> = HashMap::new();
    for file in &new_candidates {
        canonical
            .entry(file.hash.as_str())
            .or_insert(file.key.as_str());
    }
    for file in new_candidates {
        let kind = match canonical.get(file.hash.as_str()) {
            Some(canonical_key) if *canonical_key == file.key.as_str() => ChangeKind::New,
            _ => ChangeKind::Duplicate,
        };
        changes.push(change(
            Layer::Raw,
            kind,
            &file.key,
            Some(file.hash.clone()),
            None,
        ));
    }

    let present: BTreeSet<&str> = disk.iter().map(|file| file.key.as_str()).collect();
    for row in ledger {
        if !present.contains(row.key.as_str()) {
            changes.push(change(
                Layer::Raw,
                ChangeKind::Missing,
                &row.key,
                None,
                None,
            ));
        }
    }

    // One key can only carry one change, so the sort is total.
    changes.sort_by(|a, b| a.key.cmp(&b.key));
    changes
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
fn is_ignored_path(root: &Path, entry: &walkdir::DirEntry) -> bool {
    match entry.path().strip_prefix(root) {
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
                changes.push(change(
                    Layer::Source,
                    ChangeKind::Oversize,
                    &file.key,
                    None,
                    None,
                ));
            }
            DiskDigest::Digest(digest) => match by_key.get(file.key.as_str()) {
                None => new_candidates.push(file),
                Some(row) if row.hash.as_str() == digest => {}
                Some(_) => changes.push(change(
                    Layer::Source,
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
                Layer::Source,
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
                    Layer::Source,
                    ChangeKind::Moved,
                    &file.key,
                    Some(digest.clone()),
                    Some(row.key.clone()),
                ));
            }
            None => changes.push(change(
                Layer::Source,
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
        changes.push(change(
            Layer::Source,
            ChangeKind::Missing,
            &row.key,
            None,
            None,
        ));
    }

    // One key can only carry one change, so the sort is total.
    changes.sort_by(|a, b| a.key.cmp(&b.key));
    changes
}

/// Convenience constructor keeping the field order visible.
fn change(
    layer: Layer,
    kind: ChangeKind,
    key: &str,
    hash: Option<String>,
    moved_from: Option<String>,
) -> Change {
    Change {
        kind,
        key: String::from(key),
        hash,
        moved_from,
        layer,
    }
}
