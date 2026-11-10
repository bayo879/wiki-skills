//! One batch of sources at a time: the work lease.
//!
//! `ingest` hands the agent a set of pending sources; until that set is
//! acknowledged with `done`, a second batch would give the agent two moving
//! targets at once. The lease is that handoff written down: `.wiki/work.lock`
//! lists the sources delivered to the current batch, `done` removes the source
//! it finished, and the file disappears with the last one.
//!
//! The lease is deliberately a file, not a database row: it is state about a
//! human-sized batch, it has to be readable and clearable without the ledger,
//! and `doctor --fix` must be able to drop an expired one after a crashed or
//! abandoned batch. It never blocks reading, only converting and acknowledging.

use std::path::{Path, PathBuf};

use chrono::{Duration, Utc};
use serde::{Deserialize, Serialize};

use crate::render::write_atomic;

/// Where the lease lives, relative to the repository root.
pub const LEASE_FILE: &str = ".wiki/work.lock";

/// How long a batch may stay open before `doctor --fix` treats it as abandoned.
///
/// Two hours is long enough for a careful pass over a batch and short enough
/// that a forgotten batch does not block a repository for a day.
pub const LEASE_SECONDS: i64 = 2 * 60 * 60;

/// One source delivered in the batch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    /// Source key, as `ingest` named it.
    pub source_key: String,
    /// Raw key the page will cite.
    pub raw_key: String,
    /// Digest of that raw file when it was handed over.
    pub raw_hash: String,
}

/// The batch currently handed to the agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    /// When the batch was delivered, RFC 3339.
    pub started_at: String,
    /// Process that delivered it, for `doctor` and for humans.
    pub pid: u32,
    /// Round identifier the handoff belongs to.
    pub run_id: String,
    /// Sources still waiting for `done`.
    pub items: Vec<Item>,
}

#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    /// The lease file could not be read or written.
    #[error("cannot access {}: {source}", path.display())]
    Io {
        /// The file involved.
        path: PathBuf,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
    /// The file exists but is not a lease this version understands.
    #[error("{} is not a work lease; remove it and run `ingest` again", path.display())]
    Unreadable {
        /// The lease file.
        path: PathBuf,
    },
}

impl LeaseError {
    /// Stable error code for the response envelope.
    pub fn code(&self) -> &'static str {
        "E_LEASE"
    }
}

/// Absolute path of the lease file.
pub fn path(root: &Path) -> PathBuf {
    root.join(LEASE_FILE)
}

/// Read the lease, if one is present.
pub fn read(root: &Path) -> Result<Option<Lease>, LeaseError> {
    let file = path(root);
    let bytes = match std::fs::read(&file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(LeaseError::Io { path: file, source }),
    };
    match serde_json::from_slice(&bytes) {
        Ok(lease) => Ok(Some(lease)),
        Err(_) => Err(LeaseError::Unreadable { path: file }),
    }
}

/// Replace the lease with `items`, or delete it when there is nothing to hold.
pub fn write(root: &Path, run_id: &str, items: Vec<Item>) -> Result<(), LeaseError> {
    let file = path(root);
    if items.is_empty() {
        return clear(root);
    }
    let lease = Lease {
        started_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        pid: std::process::id(),
        run_id: run_id.to_owned(),
        items,
    };
    let body = serde_json::to_vec_pretty(&lease)
        .map_err(|_| LeaseError::Unreadable { path: file.clone() })?;
    write_atomic(&file, &body).map_err(|error| LeaseError::Io {
        path: file,
        source: std::io::Error::other(error.to_string()),
    })
}

/// Remove the lease file; a missing file is not an error.
pub fn clear(root: &Path) -> Result<(), LeaseError> {
    let file = path(root);
    match std::fs::remove_file(&file) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(LeaseError::Io { path: file, source }),
    }
}

impl Lease {
    /// Whether `key` belongs to this batch.
    pub fn covers(&self, key: &str) -> bool {
        self.items.iter().any(|item| item.source_key == key)
    }

    /// Whether the batch has been open longer than [`LEASE_SECONDS`].
    pub fn expired(&self) -> bool {
        let Ok(started) = chrono::DateTime::parse_from_rfc3339(&self.started_at) else {
            return true;
        };
        Utc::now().signed_duration_since(started) > Duration::seconds(LEASE_SECONDS)
    }

    /// Drop `key` from the batch, reporting whether anything is left.
    pub fn settle(&mut self, key: &str) -> bool {
        self.items.retain(|item| item.source_key != key);
        !self.items.is_empty()
    }
}
