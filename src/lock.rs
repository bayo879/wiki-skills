//! Repository exclusion uses one persistent inode and an OS file lock.
//! A live holder is never displaced based on age. When a process exits, the OS
//! releases its lock; the next holder rewrites the leftover diagnostic body.
//! Drop unlocks the handle but deliberately keeps the file in place.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use chrono::{SecondsFormat, Utc};

/// A held repository lock; dropping it releases the OS lock, not the file.
#[derive(Debug)]
pub struct Lock {
    file: File,
    /// The exact body this run wrote into the shared file.
    body: String,
    run_id: String,
}

impl Lock {
    /// The run id recorded in the lock file.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// The JSON body this run wrote into the lock file.
    ///
    /// On unix the body can also be read from disk while the lock is held; on
    /// Windows the lock is mandatory, so a second handle cannot read the locked
    /// region at all. Callers that need to inspect a *live* lock go through this
    /// accessor instead of reopening the file.
    pub fn body(&self) -> &str {
        &self.body
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Keep the inode stable: unlinking lets competitors lock different files.
        let _ = self.file.unlock();
    }
}

/// Failure while taking or holding the repository lock.
#[derive(Debug)]
pub enum LockError {
    /// Another process holds the lock.
    Held {
        /// The lock file that is held.
        path: PathBuf,
    },
    /// The lock file could not be opened, written or removed.
    Io {
        /// The lock file involved.
        path: PathBuf,
        /// The underlying failure.
        source: io::Error,
    },
}

impl LockError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        match self {
            LockError::Held { .. } => "E_LOCKED",
            LockError::Io { .. } => "E_IO",
        }
    }

    /// The sentence a human reads, without the machine code.
    ///
    /// `Display` prefixes the code so a log line names the exit condition even
    /// when it is only the error's text that is recorded; the response envelope
    /// already carries the code separately, so the CLI reports this form.
    pub fn hint(&self) -> String {
        match self {
            LockError::Held { path } => format!(
                "another process holds {}; wait for it to finish and do not delete that file",
                path.display()
            ),
            LockError::Io { path, source } => format!("cannot access {}: {source}", path.display()),
        }
    }
}

impl std::fmt::Display for LockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code(), self.hint())
    }
}

impl std::error::Error for LockError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LockError::Held { .. } => None,
            LockError::Io { source, .. } => Some(source),
        }
    }
}

/// Result of the lock helpers.
pub type Result<T> = std::result::Result<T, LockError>;

/// The lock file under `root`.
fn lock_path(root: &Path) -> PathBuf {
    root.join(".wiki/lock")
}

/// Takes the repository lock.
///
/// The file is created if missing and reused afterwards, so the inode never
/// changes under a competitor. Only the OS lock decides who holds it: the age
/// of the file says nothing, because `Drop` keeps it and the body is rewritten
/// by each holder. A file whose body is left over from a finished run is
/// therefore normal, not a crash to reclaim.
pub fn acquire(root: &Path) -> Result<Lock> {
    let path = lock_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| LockError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    {
        // The body is read first (to detect a takeover) and truncated by
        // hand afterwards, so the file must not be truncated at open time.
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)
            .map_err(|source| LockError::Io {
                path: path.clone(),
                source,
            })?;
        match file.try_lock() {
            Ok(()) => {
                // We hold the lock. A body left over from an earlier run is
                // still here; it describes nobody, so it is overwritten.
                let run_id = new_run_id();
                let body = lock_body(&run_id);
                // `read_handle` left the cursor at EOF; rewind before truncating
                // and writing, or the body lands after a hole of stale bytes.
                let mut handle = &file;
                handle
                    .seek(SeekFrom::Start(0))
                    .and_then(|_| handle.set_len(0))
                    .and_then(|()| handle.write_all(body.as_bytes()))
                    .map_err(|source| LockError::Io {
                        path: path.clone(),
                        source,
                    })?;
                Ok(Lock { file, body, run_id })
            }
            Err(std::fs::TryLockError::WouldBlock) => Err(LockError::Held { path }),
            Err(std::fs::TryLockError::Error(source)) => Err(LockError::Io { path, source }),
        }
    }
}

/// Whether some live process currently holds the repository lock.
///
/// This is the only question the file can answer; its mtime cannot. A missing
/// or unreadable file, or a platform that cannot try the lock, answers `false`
/// — "not held" — rather than inventing a holder.
pub fn is_held(root: &Path) -> bool {
    let Ok(file) = OpenOptions::new()
        .read(true)
        .write(true)
        .open(lock_path(root))
    else {
        return false;
    };
    match file.try_lock() {
        Ok(()) => {
            let _ = file.unlock();
            false
        }
        Err(std::fs::TryLockError::WouldBlock) => true,
        Err(std::fs::TryLockError::Error(_)) => false,
    }
}

/// A run id unique to this second: `20261027T154500-7f3a`.
fn new_run_id() -> String {
    let now = Utc::now();
    let suffix = (now.timestamp_subsec_nanos() ^ std::process::id()) & 0xffff;
    format!("{}-{suffix:04x}", now.format("%Y%m%dT%H%M%S"))
}

/// The JSON written into the lock file: who holds it and since when.
fn lock_body(run_id: &str) -> String {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| String::from("unknown"));
    let body = serde_json::json!({
        "pid": std::process::id(),
        "host": host,
        "started_at": Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        "run_id": run_id,
    });
    body.to_string()
}
