//! Writing the raw file, and undoing that write when the ledger refuses it.
//!
//! The order is fixed: file first, ledger second. The ledger rows describe bytes
//! that already exist, so a row written before the file would point at content
//! nobody can read. The price is the window in between — if the database fails
//! after the rename, the raw file on disk no longer matches its rows.
//!
//! That window is what this module closes. When registration fails, the previous
//! raw bytes are written back (or the new file is removed when there was no
//! previous one) and the original error is returned; the conversion reports a
//! failure, so the next run retries instead of treating the source as done. If
//! even the restore fails, the old bytes are stashed next to the raw file and the
//! path is reported — losing the ledger row is recoverable, losing the text a
//! person wrote is not.
//!
//! The restore is itself guarded. Between our write and our undo, somebody may
//! have edited the raw file; writing the baseline over that edit would destroy
//! exactly what the protection checks exist to protect. So the undo only puts the
//! old bytes back when the file still holds what we wrote, and otherwise reports
//! the material it saved instead of touching the file.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::convert::protection::Baseline;
use crate::convert::{ConvertError, Ready, Result};
use crate::hash;
use crate::ledger::Ledger;
use crate::store::Store;

/// Writes the prepared document and records it, undoing the write on failure.
pub(crate) fn apply(store: &mut Store, ready: &Ready) -> Result<()> {
    write_atomic(&ready.raw_path, ready.document.as_bytes())?;
    let written = hash::hash_bytes(ready.document.as_bytes());
    let registered = Ledger::new(store).record_conversion_input(
        &ready.source_row,
        &ready.raw_row,
        ready.mtime_ns,
    );
    match registered {
        Ok(()) => Ok(()),
        Err(error) => Err(undo(ready, &written, error.into())),
    }
}

/// Puts the baseline back and returns the original error, or explains why not.
fn undo(ready: &Ready, written: &str, cause: ConvertError) -> ConvertError {
    match restore(&ready.baseline, &ready.raw_path, written) {
        Ok(()) => cause,
        Err(failure) => ConvertError::RollbackFailed {
            raw_path: ready.raw_path.display().to_string(),
            cause: cause.to_string(),
            rollback: failure.rollback,
            material: failure.material,
        },
    }
}

/// Why a restore did not happen, and where the old bytes went instead.
#[derive(Debug)]
struct RollbackFailure {
    /// What went wrong, phrased for the user.
    rollback: String,
    /// Where the previous bytes can be found, or why they could not be saved.
    material: String,
}

/// Restores the baseline, unless the file is no longer the one we wrote.
///
/// `written` is the digest of the document this conversion put on disk. A file
/// with any other digest was edited by somebody else after our write, so it is
/// left alone and the baseline is only stashed.
fn restore(
    baseline: &Baseline,
    raw_path: &Path,
    written: &str,
) -> std::result::Result<(), RollbackFailure> {
    let current = read_hash(raw_path);
    if let Ok(found) = &current {
        if found != written {
            return Err(RollbackFailure {
                rollback: String::from(
                    "the raw file changed after this conversion wrote it, so it was left as it is",
                ),
                material: stash_material(raw_path, baseline),
            });
        }
    }
    let restored = match &baseline.bytes {
        Some(bytes) => write_atomic(raw_path, bytes),
        None => remove_file(raw_path),
    };
    match restored {
        Ok(()) => Ok(()),
        Err(error) => Err(RollbackFailure {
            rollback: error.to_string(),
            material: stash_material(raw_path, baseline),
        }),
    }
}

/// The digest of the file at `path`, or the read error.
fn read_hash(path: &Path) -> std::result::Result<String, std::io::Error> {
    std::fs::read(path).map(|bytes| hash::hash_bytes(&bytes))
}

/// Saves the baseline bytes and describes where they are.
fn stash_material(raw_path: &Path, baseline: &Baseline) -> String {
    let Some(bytes) = &baseline.bytes else {
        return String::from("there was no previous file to save");
    };
    match stash(raw_path, bytes) {
        Ok(path) => format!("the previous bytes were saved to {}", path.display()),
        Err(error) => format!(
            "the previous bytes could not be saved either ({error}); they are only in the \
             prepared conversion's memory"
        ),
    }
}

/// Saves `bytes` in a dot-prefixed sibling so a failed restore loses nothing.
///
/// The name is hidden from the source walkers, which skip dot-prefixed entries,
/// and it is not a raw file anybody would mistake for converted output. When the
/// raw directory itself cannot be written, its parent and the system temporary
/// directory are tried in turn, because the point is to keep the bytes.
fn stash(raw_path: &Path, bytes: &[u8]) -> std::result::Result<PathBuf, String> {
    let mut last = String::from("no directory was writable");
    for directory in stash_dirs(raw_path) {
        let path = directory.join(format!(
            ".{}.{}.recovery",
            file_name(raw_path),
            std::process::id()
        ));
        match write_atomic(&path, bytes) {
            Ok(()) => return Ok(path),
            Err(error) => last = error.to_string(),
        }
    }
    Err(last)
}

/// The directories a recovery file may be written to, most local first.
fn stash_dirs(raw_path: &Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(parent) = raw_path.parent() {
        dirs.push(parent.to_path_buf());
        if let Some(grandparent) = parent.parent() {
            dirs.push(grandparent.to_path_buf());
        }
    }
    dirs.push(std::env::temp_dir());
    dirs
}

/// The file name used in the recovery file's name.
fn file_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| String::from("raw.md"))
}

/// Writes `body` to `path` through a temporary sibling plus a rename.
///
/// A rename is atomic on the same filesystem, so readers (and `ingest`) never
/// observe a partially written document. The temporary name is dot-prefixed and
/// process-tagged so a leftover from a killed run is invisible to the walkers.
pub(crate) fn write_atomic(path: &Path, body: &[u8]) -> Result<()> {
    let parent = match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&parent).map_err(|source| ConvertError::Io {
        path: parent.clone(),
        source,
    })?;

    let tmp = parent.join(format!(".{}.{}.tmp", file_name(path), std::process::id()));

    let written = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(body)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(source) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(ConvertError::Io { path: tmp, source });
    }
    if let Err(source) = std::fs::rename(&tmp, path) {
        let _ = std::fs::remove_file(&tmp);
        return Err(ConvertError::Io {
            path: path.to_path_buf(),
            source,
        });
    }
    Ok(())
}

/// Removes a file a failed commit created, reporting a removal that did not work.
fn remove_file(path: &Path) -> Result<()> {
    std::fs::remove_file(path).map_err(|source| ConvertError::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::convert::protection::RawState;

    /// The baseline a conversion found on disk before overwriting it.
    fn baseline(previous: &[u8]) -> Baseline {
        Baseline {
            state: RawState::Overwritable,
            bytes: Some(previous.to_vec()),
            hash: Some(hash::hash_bytes(previous)),
        }
    }

    #[test]
    fn a_file_that_is_still_ours_is_restored() {
        let d = tempfile::tempdir().unwrap();
        let raw = d.path().join("a.md");
        std::fs::write(&raw, "ours").unwrap();

        restore(&baseline(b"old"), &raw, &hash::hash_bytes(b"ours")).unwrap();
        assert_eq!(std::fs::read_to_string(&raw).unwrap(), "old");
    }

    #[test]
    fn a_file_edited_after_the_write_is_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let raw = d.path().join("a.md");
        std::fs::write(&raw, "a hand edit").unwrap();

        let failure = restore(&baseline(b"old"), &raw, &hash::hash_bytes(b"ours")).unwrap_err();
        assert_eq!(
            std::fs::read_to_string(&raw).unwrap(),
            "a hand edit",
            "the hand edit must survive the rollback"
        );
        // The old bytes are not lost even though they were not put back.
        let saved = failure
            .material
            .split_once("saved to ")
            .expect("the material path must be reported")
            .1;
        assert_eq!(std::fs::read_to_string(saved).unwrap(), "old");
    }

    // Making a directory read-only is a Unix permission; Windows has no
    // equivalent that would stop the write in the same way.
    #[cfg(unix)]
    #[test]
    fn a_restore_that_cannot_write_reports_where_the_bytes_went() {
        let d = tempfile::tempdir().unwrap();
        let dir = d.path().join("raw");
        std::fs::create_dir(&dir).unwrap();
        let raw = dir.join("a.md");
        std::fs::write(&raw, "ours").unwrap();

        let mut perms = std::fs::metadata(&dir).unwrap().permissions();
        let original = perms.clone();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o555);
        std::fs::set_permissions(&dir, perms).unwrap();

        let failure = restore(&baseline(b"old"), &raw, &hash::hash_bytes(b"ours")).unwrap_err();
        std::fs::set_permissions(&dir, original).unwrap();

        assert!(
            !failure.material.contains("could not be saved"),
            "the bytes must be stashed somewhere: {}",
            failure.material
        );
        let saved = failure
            .material
            .split_once("saved to ")
            .expect("the material path must be reported")
            .1;
        assert_eq!(std::fs::read_to_string(saved).unwrap(), "old");
    }
}
