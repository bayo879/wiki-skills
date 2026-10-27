//! The `.wiki/lock` file: one unattended round at a time.
//!
//! The lock is the operating system's, not a convention. The file is opened and
//! locked in one step ([`std::fs::File::try_lock`] on an `OpenOptions` handle
//! that creates it), so there is no window in which two processes both see "no
//! lock" and both decide to create one.
//!
//! Two failures are distinguished:
//!
//! - another live process holds the lock; the round stops with `E_LOCKED`
//!   (exit 4) and the human is told which file to delete once they are sure
//!   nothing is running;
//! - the file was left behind by a process that died. Its content survives but
//!   nobody holds the lock, so the next round takes the lock and rewrites the
//!   body; if a *live-looking* file is older than the TTL it is treated the same
//!   way (a hung process should not block the repository forever).
//!
//! A locked file is readable by others on unix but not on Windows, where byte
//! range locks are mandatory. Anything that needs to inspect a live lock uses
//! [`Lock::body`]; readers that must work on both platforms only look at the
//! file's mtime ([`is_stale`]) or read it after the lock is gone.
//!
//! On drop the lock is released and the file removed, in that order: another
//! process waiting on the same inode must never be told the lock is free while
//! the file describing it still names this run.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{SecondsFormat, Utc};

/// How long a lock file may sit untouched before it is treated as a crash.
pub const LOCK_TTL_SECS: u64 = 600;

/// A held repository lock; dropping it releases and removes the file.
#[derive(Debug)]
pub struct Lock {
    path: PathBuf,
    file: File,
    /// The exact body this run wrote, so `drop` only deletes its own file.
    body: String,
    run_id: String,
    reclaimed: bool,
}

impl Lock {
    /// The run id recorded in the lock file.
    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    /// Whether this acquisition replaced a stale lock file.
    pub fn reclaimed(&self) -> bool {
        self.reclaimed
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
        // Release first: a waiter may take the lock as soon as the OS lets it.
        let _ = self.file.unlock();
        // Only remove the file if it is still the one this run wrote. Another
        // process may have reclaimed it between the unlock and the read.
        if read_handle(&self.file).is_ok_and(|text| text == self.body) {
            let _ = fs::remove_file(&self.path);
        }
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
                "another process holds {}; if no process is running, delete that file",
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

/// Takes the repository lock, reclaiming it when the file outlived its TTL.
pub fn acquire(root: &Path) -> Result<Lock> {
    acquire_with_ttl(root, LOCK_TTL_SECS)
}

/// Takes the repository lock, treating a file older than `ttl_secs` as a crash.
///
/// The file is created if missing, so a fresh repository needs no preparation.
/// A takeover happens at most once per call; if the recreated file is locked
/// again by someone else, the round reports `E_LOCKED` rather than spinning.
pub fn acquire_with_ttl(root: &Path, ttl_secs: u64) -> Result<Lock> {
    let path = lock_path(root);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| LockError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let mut reclaimed = false;
    loop {
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
                // We hold the lock. A body left over from a dead process is
                // still here; overwriting it is the takeover.
                let leftover = read_handle(&file).unwrap_or_default();
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
                let reclaimed = reclaimed || !leftover.trim().is_empty();
                return Ok(Lock {
                    path,
                    file,
                    body,
                    run_id,
                    reclaimed,
                });
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                if !reclaimed && is_stale(&path, ttl_secs) {
                    // A crashed run's file must not block the repository. The
                    // name is removed and recreated; the dead process keeps its
                    // lock on the now-nameless inode.
                    match fs::remove_file(&path) {
                        Ok(()) => {}
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                        Err(source) => return Err(LockError::Io { path, source }),
                    }
                    reclaimed = true;
                    continue;
                }
                return Err(LockError::Held { path });
            }
            Err(std::fs::TryLockError::Error(source)) => {
                return Err(LockError::Io { path, source })
            }
        }
    }
}

/// Whether the lock file is older than `ttl_secs`.
///
/// A missing file is not stale: there is nothing to reclaim. A clock skew that
/// makes the mtime look like the future is not stale either, and a platform
/// without a modification time cannot answer the question at all.
pub fn is_stale(root: &Path, ttl_secs: u64) -> bool {
    stale(&lock_path(root), ttl_secs)
}

fn stale(path: &Path, ttl_secs: u64) -> bool {
    let Ok(meta) = fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = meta.modified() else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age > Duration::from_secs(ttl_secs))
}

/// Reads the whole file through a borrowed handle, without moving the cursor
/// used by the writer.
fn read_handle(file: &File) -> io::Result<String> {
    let mut handle = file;
    handle.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    handle.read_to_string(&mut text)?;
    Ok(text)
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
