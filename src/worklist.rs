//! Source completion is a hash acknowledgement, never a page publication.
use std::path::Path;

use crate::{
    config::Config,
    hash, lease,
    ledger::Ledger,
    paths,
    store::{Store, StoreError},
};

#[derive(Debug, thiserror::Error)]
pub enum WorkError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Hash(#[from] hash::HashError),
    #[error("unknown source: {0}")]
    UnknownSource(String),
    #[error("source has no registered raw: {0}")]
    MissingRaw(String),
    #[error("source or raw changed; reread before acknowledging: {0}")]
    Drift(String),
    /// Another batch is open, and this source is not part of it.
    #[error(
        "work lease: the open batch does not contain '{0}'; finish the batch listed in .wiki/work.lock first"
    )]
    NotLeased(String),
    /// The lease file could not be read or updated.
    #[error(transparent)]
    Lease(#[from] lease::LeaseError),
}

/// Import only source hints from the retired queue. Never publish or alter it.
/// Broken items remain visible and retryable; successful items are marked once.
pub fn import_legacy(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
) -> Result<Vec<String>, WorkError> {
    let path = root.join(".wiki/staging/pending.json");
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Ok(vec![format!("legacy queue {}: {error}", path.display())]),
    };
    let hints: std::collections::BTreeMap<String, serde_json::Value> =
        match serde_json::from_slice(&bytes) {
            Ok(hints) => hints,
            Err(error) => return Ok(vec![format!("legacy queue {}: {error}", path.display())]),
        };
    let raws = store.all_raws()?;
    let mut warnings = Vec::new();
    for key in hints.keys() {
        if store.legacy_work_imported(key)? {
            continue;
        }
        let Some(raw) = raws.iter().find(|raw| raw.key == *key) else {
            warnings.push(format!("legacy queue {key}: raw mapping is missing"));
            continue;
        };
        let Some(source) = store.get_source(&raw.source_key)? else {
            warnings.push(format!("legacy queue {key}: source mapping is missing"));
            continue;
        };
        let expected_path = format!("{}/{}", cfg.paths.raw.trim_end_matches('/'), key);
        if source.raw_key.as_deref() != Some(expected_path.as_str()) {
            warnings.push(format!("legacy queue {key}: source/raw mappings disagree"));
            continue;
        }
        let digest = match hash::hash_file(&paths::join_key(root, &expected_path), true) {
            Ok(digest) if digest == raw.hash => digest,
            Ok(_) => {
                warnings.push(format!(
                    "legacy queue {key}: raw changed outside the ledger"
                ));
                continue;
            }
            Err(error) => {
                warnings.push(format!("legacy queue {key}: {error}"));
                continue;
            }
        };
        Ledger::new(store).import_work_item(&crate::store::WorkItem {
            source_key: source.key,
            raw_key: key.clone(),
            raw_hash: digest,
        })?;
    }
    Ok(warnings)
}

/// Caller holds the repository lock. Returns false for a validated no-op.
/// Direct page writes are outside this lock and are never reverted here.
pub fn complete(
    root: &Path,
    cfg: &Config,
    store: &mut Store,
    key: &str,
    expected_hash: &str,
) -> Result<bool, WorkError> {
    let source = store
        .get_source(key)?
        .ok_or_else(|| WorkError::UnknownSource(key.into()))?;
    let raw_path = source
        .raw_key
        .as_deref()
        .ok_or_else(|| WorkError::MissingRaw(key.into()))?;
    // No work item means nothing to acknowledge: a second `done` for the same
    // source is a validated no-op, not a reason to complain about the lease.
    let Some(item) = store
        .work_items()?
        .into_iter()
        .find(|item| item.source_key == key)
    else {
        return Ok(false);
    };
    let raw = store
        .all_raws()?
        .into_iter()
        .find(|row| row.source_key == key)
        .ok_or_else(|| WorkError::MissingRaw(key.into()))?;
    let path = paths::join_key(root, raw_path);
    let current_hash = hash::hash_file(&path, true)?;
    if current_hash != expected_hash || raw.hash != expected_hash {
        return Err(WorkError::Drift(key.into()));
    }
    if item.raw_key != raw.key || item.raw_hash != expected_hash {
        return Err(WorkError::Drift(key.into()));
    }
    let input = hash::read_source(
        &paths::join_key(&root.join(&cfg.paths.source), key),
        cfg.parse.max_source_bytes,
    )?;
    if input.hash != source.hash {
        return Err(WorkError::Drift(key.into()));
    }
    // A batch that is still open is the agent's working set: completing a source
    // outside it would let a second batch start while the first is underway.
    // An expired batch is not enforced, only cleaned up when its own key lands.
    let held = lease::read(root)?;
    if let Some(open) = &held {
        if !open.expired() && !open.covers(key) {
            return Err(WorkError::NotLeased(key.into()));
        }
    }
    let completed = Ledger::new(store).complete_work_item(key, &raw.key, expected_hash)?;
    if completed {
        if let Some(mut open) = held {
            if open.covers(key) {
                if open.settle(key) {
                    lease::write(root, &open.run_id.clone(), open.items)?;
                } else {
                    lease::clear(root)?;
                }
            }
        }
    }
    Ok(completed)
}
