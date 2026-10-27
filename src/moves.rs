//! `moved`: migrating a rename without re-ingesting anything.
//!
//! A rename is not a delete plus an add. If the content digest is unchanged the
//! source is the same source, and the only things that must move are the names:
//!
//! - the `sources` ledger row (key, and `raw_key` when the raw path follows the
//!   source name),
//! - the `raws` ledger row and the raw file itself, whose `source:` field points
//!   back at the source,
//! - the `source_pages` mapping,
//! - every page's `sources:` list, rewritten in place with
//!   [`page::replace_sources`] so the rest of the frontmatter is untouched.
//!
//! Ingest is deliberately not part of this: the pages already hold conclusions
//! drawn from that content, so re-converting and re-ingesting would only churn
//! them. `scan` stays read-only; `scan --apply-moves` and `sync` call [`apply`].

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use walkdir::WalkDir;

use crate::config::Config;
use crate::convert;
use crate::hash::{self, HashError};
use crate::page::{self, PageError};
use crate::paths;
use crate::scan::{Change, ChangeKind, Layer};
use crate::store::{RawRow, Store, StoreError};

/// Stable code reported when a rename would overwrite something.
pub const E_MOVED_COLLISION: &str = "E_MOVED_COLLISION";

/// Why a migration could not run.
#[derive(Debug, thiserror::Error)]
pub enum MoveError {
    /// A file could not be renamed or rewritten.
    #[error("cannot access {}: {source}", path.display())]
    Io {
        /// The path that failed.
        path: PathBuf,
        /// The underlying OS error.
        #[source]
        source: std::io::Error,
    },
    /// The ledger could not be read or written.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// A raw file could not be digested again after its `source:` field moved.
    #[error(transparent)]
    Hash(#[from] HashError),
    /// A page could not be read or rewritten.
    #[error(transparent)]
    Page(#[from] PageError),
    /// A path could not be turned into a repository key.
    #[error(transparent)]
    Path(#[from] paths::PathError),
}

impl MoveError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            MoveError::Io { .. } | MoveError::Hash(_) | MoveError::Page(_) | MoveError::Path(_) => {
                "E_IO"
            }
            MoveError::Store(_) => "E_DB",
        }
    }
}

/// Result of the migration helpers.
pub type Result<T> = std::result::Result<T, MoveError>;

/// One rename that was migrated.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AppliedMove {
    /// The old source key.
    pub from: String,
    /// The new source key.
    pub to: String,
    /// Page keys whose `sources:` list was rewritten.
    pub pages: Vec<String>,
}

/// One rename that was left alone because it would overwrite something.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Collision {
    /// The old source key.
    pub from: String,
    /// The new source key.
    pub to: String,
    /// Always [`E_MOVED_COLLISION`].
    pub code: &'static str,
    /// What already occupies the destination.
    pub message: String,
}

/// What one [`apply`] call did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct MoveReport {
    /// The renames that were migrated.
    pub applied: Vec<AppliedMove>,
    /// The renames that were skipped to avoid overwriting something.
    pub collisions: Vec<Collision>,
}

impl MoveReport {
    /// Whether every rename went through.
    pub fn is_clean(&self) -> bool {
        self.collisions.is_empty()
    }
}

/// Migrates the `Moved` changes in `changes`.
///
/// Changes from the raw layer are ignored: a rename there has no source behind
/// it, and the source-layer migration already moves the raw identity with it.
pub fn apply(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    changes: &[Change],
) -> Result<MoveReport> {
    let mut report = MoveReport::default();
    for change in changes {
        if change.kind != ChangeKind::Moved || change.layer != Layer::Source {
            continue;
        }
        let Some(from) = change.moved_from.as_deref() else {
            continue;
        };
        match migrate_one(root, cfg, store, from, &change.key)? {
            Outcome::Migrated(pages) => report.applied.push(AppliedMove {
                from: String::from(from),
                to: change.key.clone(),
                pages,
            }),
            Outcome::Collision(message) => report.collisions.push(Collision {
                from: String::from(from),
                to: change.key.clone(),
                code: E_MOVED_COLLISION,
                message,
            }),
        }
    }
    Ok(report)
}

/// What one rename did.
enum Outcome {
    /// The migration ran; the listed pages were rewritten.
    Migrated(Vec<String>),
    /// Something already occupies the destination; nothing was written.
    Collision(String),
}

/// The raw key a source key converts to, under the configured raw directory.
fn raw_key_for(cfg: &Config, source_key: &str) -> String {
    format!(
        "{}/{}",
        cfg.paths.raw.trim_end_matches('/'),
        convert::raw_key_for(source_key)
    )
}

fn migrate_one(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    from: &str,
    to: &str,
) -> Result<Outcome> {
    // Already migrated (or renamed twice): the old key is gone, nothing to do.
    let Some(old_row) = store.get_source(from)? else {
        return Ok(Outcome::Migrated(Vec::new()));
    };
    if from == to {
        return Ok(Outcome::Migrated(Vec::new()));
    }
    if store.get_source(to)?.is_some() {
        return Ok(Outcome::Collision(format!(
            "the ledger already has a row for {to}; not overwriting it"
        )));
    }

    let raw_from = old_row.raw_key.clone();
    let raw_to = raw_from.as_ref().map(|_| raw_key_for(cfg, to));

    let mut raw_digest = None;
    if let (Some(old_raw), Some(new_raw)) = (raw_from.as_deref(), raw_to.as_deref()) {
        let old_path = paths::join_key(root, old_raw);
        let new_path = paths::join_key(root, new_raw);
        if old_raw != new_raw {
            // The destination is never overwritten: it may be a hand-maintained
            // raw file that the rename has nothing to do with.
            if new_path.exists() {
                return Ok(Outcome::Collision(format!(
                    "{new_raw} already exists; not overwriting it"
                )));
            }
            if old_path.is_file() {
                if let Some(parent) = new_path.parent() {
                    fs::create_dir_all(parent).map_err(|source| MoveError::Io {
                        path: parent.to_path_buf(),
                        source,
                    })?;
                }
                fs::rename(&old_path, &new_path).map_err(|source| MoveError::Io {
                    path: old_path.clone(),
                    source,
                })?;
            }
        }
        if new_path.is_file() {
            retarget_raw(&new_path, &slash_join(&cfg.paths.source, to))?;
            raw_digest = Some(hash::hash_file(&new_path, true)?);
        }
    }

    // The raw ledger follows the raw file, keeping the page mapping intact. Its
    // keys are relative to `raw/` (`scan_raw` reports them that way, and
    // `convert` records them that way), while `source_pages` and
    // `sources.raw_key` carry the repository key.
    if let (Some(old_raw), Some(new_raw)) = (raw_from.as_deref(), raw_to.as_deref()) {
        let rel_from = convert::raw_key_for(from);
        let rel_to = convert::raw_key_for(to);
        let previous = store.get_raw(&rel_from)?;
        let pages = store.pages_for_source(old_raw)?;
        if old_raw != new_raw {
            store.delete_raw(&rel_from)?;
            if let Some(previous) = previous {
                store.upsert_raw(&RawRow {
                    key: rel_to,
                    hash: raw_digest.unwrap_or(previous.hash),
                    source_key: String::from(to),
                    converted_at: previous.converted_at,
                })?;
            }
            store.set_source_pages(new_raw, &pages)?;
            store.set_source_pages(old_raw, &[])?;
        } else if let Some(previous) = previous {
            store.upsert_raw(&RawRow {
                source_key: String::from(to),
                hash: raw_digest.unwrap_or(previous.hash),
                ..previous
            })?;
        }
    }

    // The source ledger row moves last among the ledgers, so a failure above
    // leaves the old key in place and the next round simply retries.
    let mut new_row = old_row;
    new_row.key = String::from(to);
    if let Some(new_raw) = &raw_to {
        new_row.raw_key = Some(new_raw.clone());
    }
    store.delete_source(from)?;
    store.upsert_source(&new_row)?;

    let pages = match (raw_from.as_deref(), raw_to.as_deref()) {
        (Some(old_raw), Some(new_raw)) if old_raw != new_raw => {
            rewrite_pages(root, cfg, store, old_raw, new_raw)?
        }
        _ => Vec::new(),
    };
    Ok(Outcome::Migrated(pages))
}

/// `base/rel`, with no double slash when `base` already ends in one.
fn slash_join(base: &str, rel: &str) -> String {
    format!("{}/{}", base.trim_end_matches('/'), rel)
}

/// Rewrites `source:` in the raw file's frontmatter, and nothing else.
///
/// The field is a single top-level line in a block this engine wrote itself, so
/// a line swap is enough; the body and every other field stay byte-identical.
fn retarget_raw(raw_path: &Path, source_ref: &str) -> Result<()> {
    let bytes = fs::read(raw_path).map_err(|source| MoveError::Io {
        path: raw_path.to_path_buf(),
        source,
    })?;
    let text = std::str::from_utf8(&bytes).map_err(|_| MoveError::Io {
        path: raw_path.to_path_buf(),
        source: std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "the raw file is not valid UTF-8",
        ),
    })?;

    let mut out = String::with_capacity(text.len());
    // 0: before the opening `---`, 1: inside the frontmatter, 2: the body.
    let mut state = 0;
    let mut replaced = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end();
        match state {
            0 => {
                out.push_str(line);
                if trimmed == "---" {
                    state = 1;
                }
            }
            1 => {
                if trimmed == "---" {
                    out.push_str(line);
                    state = 2;
                } else if !replaced && key_of(trimmed) == "source" {
                    let eol = if line.ends_with("\r\n") { "\r\n" } else { "\n" };
                    out.push_str("source: ");
                    out.push_str(&convert::yaml_scalar(source_ref));
                    out.push_str(eol);
                    replaced = true;
                } else {
                    out.push_str(line);
                }
            }
            _ => out.push_str(line),
        }
    }

    if replaced {
        fs::write(raw_path, out).map_err(|source| MoveError::Io {
            path: raw_path.to_path_buf(),
            source,
        })?;
    }
    Ok(())
}

/// The key of a `key: value` frontmatter line.
fn key_of(line: &str) -> &str {
    match line.find(':') {
        Some(colon) => line[..colon].trim(),
        None => "",
    }
}

/// Rewrites every page and staging candidate that cites `old_raw`.
///
/// Returns the page keys that changed, so a caller can report them. A page's
/// `page_meta` record is refreshed at the same time, with its old `updated`
/// date: the body did not change, so the page is not re-dated, and lint L9 must
/// not read a source rename as undated drift.
fn rewrite_pages(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    old_raw: &str,
    new_raw: &str,
) -> Result<Vec<String>> {
    let mut rewritten = Vec::new();
    for path in page_files(root, cfg) {
        let (front, _) = match page::read(&path) {
            Ok(read) => read,
            // A file without frontmatter is not a page; lint owns that finding.
            Err(PageError::BadFrontmatter { .. }) => continue,
            Err(err) => return Err(MoveError::Page(err)),
        };
        if !front
            .sources
            .iter()
            .any(|item| page::same_reference(item, old_raw))
        {
            continue;
        }
        page::replace_sources(&path, &[String::from(old_raw)], &[String::from(new_raw)])?;
        let key = paths::identity_key(root, &path)?;
        let updated = match store.get_page_meta(&key)? {
            Some(meta) => meta.updated,
            None => front.updated.to_string(),
        };
        store.upsert_page_meta(&crate::store::PageMetaRow {
            page_key: key.clone(),
            hash: hash::hash_file(&path, true)?,
            updated,
        })?;
        rewritten.push(key);
    }
    rewritten.sort();
    Ok(rewritten)
}

/// Every markdown page: published pages under `wiki/`, candidates in staging.
pub(crate) fn page_files(root: &Path, cfg: &Config) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in [root.join(&cfg.paths.wiki), crate::staging::dir(root)] {
        for entry in WalkDir::new(&dir)
            .into_iter()
            .filter_map(std::result::Result::ok)
        {
            if !entry.file_type().is_file() {
                continue;
            }
            if entry.path().extension().and_then(|ext| ext.to_str()) == Some("md") {
                files.push(entry.path().to_path_buf());
            }
        }
    }
    files.sort();
    files
}
