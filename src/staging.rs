//! The reviewable half of the wiki.
//!
//! `wiki/` holds approved pages; `.wiki/staging/` holds candidates awaiting a
//! human. Candidates are a real byte copy — never a hard link and never a bare
//! hash — because approving is what publishes, and nothing may reach `wiki/`
//! before that. Editing a candidate therefore cannot leak into the approved
//! tree, and the two files have different inodes.
//!
//! `manifest.json` is the ledger: for every source it records the baseline hash
//! (what the last merge consumed), the current hash (what the wiki is built
//! from), when it was last processed, and the pages it has produced. A source
//! whose two hashes differ is still unprocessed. The ledger is written before
//! the first candidate ever appears, so "candidates exist but the manifest is
//! gone" is a torn state that is reported as [`StagingError::Corrupt`] instead
//! of being rebuilt — rebuilding would silently drop the review queue.
//!
//! The directory is fixed at `.wiki/staging` because the engine location is part
//! of the repository layout that `init` creates; callers that already know the
//! root do not need the config just to locate it.

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::config::Config;
use crate::page;
use crate::paths;

/// The ledger file inside the staging directory.
const MANIFEST: &str = "manifest.json";

/// The list `ingest begin` writes for the current round; it takes precedence
/// over the manifest while it exists.
const PENDING: &str = "pending.json";

/// What the manifest remembers about a single source.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    /// Hash the last merge consumed; `None` until the source has been merged.
    pub baseline_hash: Option<String>,
    /// Hash of the wiki currently built from the source.
    pub current_hash: Option<String>,
    /// When the source was last processed.
    pub last_processed_at: Option<String>,
    /// Page ids this source has contributed. Append-only and sorted by first
    /// write, so a re-run does not reorder history.
    pub pages: Vec<String>,
}

/// The staging ledger. Keys are source keys relative to the raw directory.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// One entry per source the repository has ever ingested.
    #[serde(default)]
    pub sources: BTreeMap<String, ManifestEntry>,
}

/// Outcome of [`ensure`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnsureOutcome {
    /// Staging did not exist and was built from the approved wiki.
    pub created: bool,
    /// Regular files copied by this call.
    pub copied: usize,
}

/// Anything that can go wrong while touching staging.
#[derive(Debug, thiserror::Error)]
pub enum StagingError {
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
    #[error("cannot walk staging: {0}")]
    Walk(#[from] walkdir::Error),
    /// The ledger (or the pending list) exists but cannot be trusted.
    #[error("staging is damaged at {}: {message}", path.display())]
    Corrupt {
        /// Offending file.
        path: PathBuf,
        /// What is wrong.
        message: String,
    },
    /// A page id would leave the staging directory.
    #[error("page id `{id}` is not a safe relative path")]
    UnsafeId {
        /// Rejected id.
        id: String,
    },
    /// A candidate's frontmatter could not be parsed.
    #[error(transparent)]
    Page(#[from] page::PageError),
}

impl StagingError {
    /// Stable machine-readable code.
    pub fn code(&self) -> &'static str {
        match self {
            StagingError::Io { .. } | StagingError::Walk(_) => "E_IO",
            StagingError::Corrupt { .. } => "E_STAGING_CORRUPT",
            StagingError::UnsafeId { .. } => "E_STAGING_PATH",
            StagingError::Page(err) => err.code(),
        }
    }
}

/// Result alias for staging operations.
pub type Result<T> = std::result::Result<T, StagingError>;

/// The staging directory for `root`.
pub fn dir(root: &Path) -> PathBuf {
    root.join(".wiki").join("staging")
}

/// The ledger path for `root`.
fn manifest_path(root: &Path) -> PathBuf {
    dir(root).join(MANIFEST)
}

/// Whether staging already holds candidate pages.
///
/// Only regular files count: an empty directory left behind by a deleted
/// candidate is not a review queue worth refusing to rebuild.
fn has_candidates(staging: &Path) -> Result<bool> {
    if !staging.is_dir() {
        return Ok(false);
    }
    for entry in WalkDir::new(staging).follow_links(false).min_depth(1) {
        let entry = entry?;
        let relative = entry.path().strip_prefix(staging).unwrap_or(entry.path());
        if paths::is_ignored(&paths::to_slash(relative)) {
            continue;
        }
        if entry.file_type().is_file() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Serialise one manifest entry, sorted by source key.
fn encode(manifest: &Manifest, path: &Path) -> Result<Vec<u8>> {
    let mut body = serde_json::to_vec_pretty(manifest).map_err(|source| StagingError::Corrupt {
        path: path.to_path_buf(),
        message: format!("cannot encode manifest: {source}"),
    })?;
    body.push(b'\n');
    Ok(body)
}

/// Write `body` to `path` through a temporary sibling plus a rename.
///
/// A rename within one directory is atomic, so a reader never observes half a
/// manifest. The temporary name is dot-prefixed so walkers ignore leftovers.
fn write_atomic(path: &Path, body: &[u8]) -> Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    fs::create_dir_all(&parent).map_err(|source| StagingError::Io {
        path: parent.clone(),
        source,
    })?;

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("page.md"));
    let tmp = parent.join(format!(".{name}.{}.tmp", std::process::id()));

    let mut file = match fs::File::create(&tmp) {
        Ok(file) => file,
        Err(source) => return Err(StagingError::Io { path: tmp, source }),
    };
    if let Err(source) = file.write_all(body).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&tmp);
        return Err(StagingError::Io { path: tmp, source });
    }
    drop(file);
    if let Err(source) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(StagingError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

impl Manifest {
    /// Read the ledger for `root`.
    ///
    /// A missing file is an empty ledger *only* when staging holds no candidate
    /// files; otherwise the ledger was lost and is reported as
    /// [`StagingError::Corrupt`] rather than silently started over.
    pub fn load(root: &Path) -> Result<Manifest> {
        let path = manifest_path(root);
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                if has_candidates(&dir(root))? {
                    return Err(StagingError::Corrupt {
                        path,
                        message: String::from(
                            "manifest is missing but staging holds candidates; refusing to rebuild it",
                        ),
                    });
                }
                return Ok(Manifest::default());
            }
            Err(source) => return Err(StagingError::Io { path, source }),
        };
        serde_json::from_slice(&bytes).map_err(|source| StagingError::Corrupt {
            path,
            message: source.to_string(),
        })
    }

    /// Persist the ledger with sorted keys and a trailing newline.
    pub fn save(root: &Path, manifest: &Manifest) -> Result<()> {
        let path = manifest_path(root);
        let body = encode(manifest, &path)?;
        write_atomic(&path, &body)
    }
}

/// Make sure the staging tree mirrors `wiki/`, without touching `wiki/`.
///
/// The first call on a repository writes an empty ledger and copies every
/// regular file of the approved wiki. A later call only copies files that are
/// new in `wiki/`: an existing candidate is never overwritten, and the ledger is
/// left byte-for-byte alone, so an unprocessed source stays pending across
/// runs.
///
/// If staging was deleted, the next call rebuilds it from `wiki/`; that loses
/// candidates by definition, which is why it only happens when staging is gone
/// rather than merely stale.
pub fn ensure(root: &Path, cfg: &Config) -> Result<EnsureOutcome> {
    let staging = dir(root);
    let known = manifest_path(root).is_file();
    // Always load: a damaged ledger must be reported even though it exists.
    // When it is missing, `load` also refuses if staging already holds
    // candidates, so nothing is copied over an unaccounted review queue.
    let manifest = Manifest::load(root)?;
    if !known {
        // The ledger comes first: it keeps the invariant that any candidate
        // file in staging is accompanied by one, even if the copy is
        // interrupted.
        Manifest::save(root, &manifest)?;
    }
    let copied = copy_missing(&root.join(&cfg.paths.wiki), &staging)?;
    Ok(EnsureOutcome {
        created: !known,
        copied,
    })
}

/// Copy every regular file of `src` that `dst` does not already have.
///
/// Dot-paths are skipped, symlinks and other special files are not copied, and
/// an existing destination is never replaced.
fn copy_missing(src: &Path, dst: &Path) -> Result<usize> {
    fs::create_dir_all(dst).map_err(|source| StagingError::Io {
        path: dst.to_path_buf(),
        source,
    })?;
    if !src.is_dir() {
        return Ok(0);
    }

    let mut copied = 0;
    for entry in WalkDir::new(src).follow_links(false) {
        let entry = entry?;
        if entry.path() == src {
            continue;
        }
        let relative = entry.path().strip_prefix(src).unwrap_or(entry.path());
        if paths::is_ignored(&paths::to_slash(relative)) {
            continue;
        }
        let target = dst.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target).map_err(|source| StagingError::Io {
                path: target,
                source,
            })?;
            continue;
        }
        if !entry.file_type().is_file() || target.exists() {
            continue;
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|source| StagingError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        // `fs::copy` copies bytes; `fs::hard_link` would alias the approved
        // file and let a candidate edit reach `wiki/`.
        fs::copy(entry.path(), &target).map_err(|source| StagingError::Io {
            path: target,
            source,
        })?;
        copied += 1;
    }
    Ok(copied)
}

/// Turn a page id into a path relative to the staging directory.
///
/// Ids are `/`-separated and carry no extension. Empty components, `.`, `..`,
/// an absolute prefix and a backslash (a separator on Windows) are all
/// rejected, so the returned path cannot leave staging.
fn page_path(id: &str) -> Result<PathBuf> {
    if id.is_empty() || id.starts_with('/') || id.contains('\\') || id.contains('\0') {
        return Err(StagingError::UnsafeId {
            id: String::from(id),
        });
    }
    let mut out = PathBuf::new();
    for part in id.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return Err(StagingError::UnsafeId {
                id: String::from(id),
            });
        }
        out.push(part);
    }
    out.set_extension("md");
    Ok(out)
}

/// Source keys named by a page's `sources`, normalised to raw-relative keys.
///
/// A source such as `raw/concept/x.md#p=3` becomes `x.md`-style
/// `concept/x.md`: the fragment locates a passage, the configured raw prefix is
/// presentation, and the manifest keys on the source file itself.
fn source_keys(sources: &[String], raw_prefix: &str) -> Vec<String> {
    let prefix = raw_prefix.trim_start_matches("./").trim_end_matches('/');
    let mut keys: Vec<String> = Vec::new();
    for source in sources {
        let mut text = source
            .split_once('#')
            .map_or(source.as_str(), |(head, _)| head);
        while let Some(rest) = text.strip_prefix("./") {
            text = rest;
        }
        if text == prefix {
            continue;
        }
        if !prefix.is_empty() {
            if let Some(rest) = text
                .strip_prefix(prefix)
                .and_then(|rest| rest.strip_prefix('/'))
            {
                text = rest;
            }
        }
        if text.is_empty() {
            continue;
        }
        if !keys.iter().any(|key| key == text) {
            keys.push(String::from(text));
        }
    }
    keys
}

/// Write a candidate page and record it against the sources it names.
///
/// The file lands under staging only; `wiki/` is not touched. Validation runs
/// before anything is written, and the ledger is created before the first
/// candidate so an interrupted run leaves no unaccounted files. Re-writing an
/// existing page replaces its bytes and does not duplicate its id in `pages`.
pub fn write_page(root: &Path, cfg: &Config, id: &str, content: &str) -> Result<()> {
    let relative = page_path(id)?;
    let known = manifest_path(root).is_file();
    let mut manifest = Manifest::load(root)?;
    let (front, _body) = page::parse(content.as_bytes())?;
    let keys = source_keys(&front.sources, &cfg.paths.raw);

    if !known {
        Manifest::save(root, &manifest)?;
    }
    let path = dir(root).join(&relative);
    write_atomic(&path, content.as_bytes())?;

    for key in keys {
        let entry = manifest.sources.entry(key).or_default();
        if !entry.pages.iter().any(|page| page == id) {
            entry.pages.push(String::from(id));
        }
    }
    Manifest::save(root, &manifest)
}

/// Sources that still need agent work this round.
///
/// `ingest begin` writes `pending.json` for the round it is driving; while that
/// file exists it is authoritative, because it names the exact sources the
/// round read. Otherwise the answer is derived from the ledger: every source
/// with a current hash different from its baseline has moved since the last
/// merge. Sources with no current hash are not built at all, so they are not
/// pending.
pub fn pending_hashes(root: &Path) -> Result<BTreeMap<String, String>> {
    let path = dir(root).join(PENDING);
    match fs::read(&path) {
        Ok(bytes) => {
            let pending: BTreeMap<String, String> =
                serde_json::from_slice(&bytes).map_err(|source| StagingError::Corrupt {
                    path,
                    message: source.to_string(),
                })?;
            return Ok(pending);
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => return Err(StagingError::Io { path, source }),
    }

    let manifest = Manifest::load(root)?;
    let mut pending = BTreeMap::new();
    for (key, entry) in manifest.sources {
        let Some(current) = entry.current_hash else {
            continue;
        };
        if entry.baseline_hash.as_deref() != Some(current.as_str()) {
            pending.insert(key, current);
        }
    }
    Ok(pending)
}
