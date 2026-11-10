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
//!
//! Expiry is measured from the last acknowledgement, not from the moment the
//! batch opened: every `done` rewrites `refreshed_at`, so a batch that keeps
//! making progress never expires, while a batch whose agent died stops being
//! live [`LEASE_SECONDS`] after its last real action. `started_at` is kept
//! unchanged for the audit trail (`doctor` prints it).

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
    /// When the batch was delivered, RFC 3339. Never changes afterwards.
    pub started_at: String,
    /// When the batch last made progress (delivered or acknowledged), RFC 3339.
    /// Absent in files written before the heartbeat existed; those fall back to
    /// `started_at`.
    #[serde(default)]
    pub refreshed_at: String,
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

/// Open a fresh batch of `items`, or delete the lease when there is nothing to
/// hold.
pub fn write(root: &Path, run_id: &str, items: Vec<Item>) -> Result<(), LeaseError> {
    if items.is_empty() {
        return clear(root);
    }
    Lease::open(run_id, items).save(root)
}

/// Timestamp shape every field uses: RFC 3339, UTC, whole seconds.
fn stamp() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
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
    /// A batch that starts right now.
    pub fn open(run_id: &str, items: Vec<Item>) -> Lease {
        let now = stamp();
        Lease {
            started_at: now.clone(),
            refreshed_at: now,
            pid: std::process::id(),
            run_id: run_id.to_owned(),
            items,
        }
    }

    /// Persist the batch as it stands. An empty batch means the lease is gone.
    pub fn save(&self, root: &Path) -> Result<(), LeaseError> {
        if self.items.is_empty() {
            return clear(root);
        }
        let file = path(root);
        let body = serde_json::to_vec_pretty(self)
            .map_err(|_| LeaseError::Unreadable { path: file.clone() })?;
        write_atomic(&file, &body).map_err(|error| LeaseError::Io {
            path: file,
            source: std::io::Error::other(error.to_string()),
        })
    }

    /// Record that the batch made progress just now.
    pub fn refresh(&mut self) {
        self.refreshed_at = stamp();
    }

    /// When the batch last moved, for the expiry check.
    fn last_progress(&self) -> Option<chrono::DateTime<chrono::FixedOffset>> {
        let last = if self.refreshed_at.is_empty() {
            &self.started_at
        } else {
            &self.refreshed_at
        };
        chrono::DateTime::parse_from_rfc3339(last).ok()
    }

    /// Whether `key` belongs to this batch.
    pub fn covers(&self, key: &str) -> bool {
        self.items.iter().any(|item| item.source_key == key)
    }

    /// Whether the batch went [`LEASE_SECONDS`] without progress.
    ///
    /// A file whose timestamps cannot be parsed counts as expired: there is no
    /// way to tell how long it has been sitting there, and guessing "fresh"
    /// would let a corrupted lease block the repository forever.
    pub fn expired(&self) -> bool {
        let Some(last) = self.last_progress() else {
            return true;
        };
        Utc::now().signed_duration_since(last) > Duration::seconds(LEASE_SECONDS)
    }

    /// Drop `key` from the batch, reporting whether anything is left.
    pub fn settle(&mut self, key: &str) -> bool {
        self.items.retain(|item| item.source_key != key);
        !self.items.is_empty()
    }
}
