//! Raw baselines: what a conversion is about to replace, and how it re-checks it.
//!
//! Preparing a conversion reads the raw file to decide two things: whether it is
//! ours and whether it is pinned. Those answers are only valid at that instant.
//! Another process can rewrite the source, a human can flip `pin: true` in an
//! editor, and the ledger can be edited in between, so the commit half re-reads
//! both the source and the raw file and refuses to write when either moved.
//!
//! The rule the checks enforce is the one from the design: never destroy
//! something a person wrote by accident. Losing a race is reported as a
//! conflict, not retried silently; a pinned raw is a skip, and a foreign raw is
//! a refusal.
//!
//! [`preflight`] runs the same checks without parsing anything, so `ingest
//! --dry-run` can report a conflict before a single byte is written or fetched.

use std::path::Path;

use crate::config::Config;
use crate::convert::{slash_join, ConvertError, Ready, Result};
use crate::hash;
use crate::paths;
use crate::store::Store;

/// Frontmatter key that records the source path.
pub(crate) const SOURCE_FIELD: &str = "source";

/// What already sits at the raw path, judged by the file itself.
#[derive(Debug)]
pub(crate) enum RawState {
    /// Nothing has been written there yet.
    Absent,
    /// Ours, and not pinned: safe to replace.
    Overwritable,
    /// Ours, but pinned: hand-maintained, so leave it alone.
    Pinned { parser: Option<String> },
    /// Another source's file, or one that cannot prove it is ours.
    Foreign { source: String },
}

/// The raw file as it was when the conversion started.
#[derive(Debug)]
pub(crate) struct Baseline {
    /// Ownership and pin state read from the file.
    pub(crate) state: RawState,
    /// The exact bytes, kept so a failed commit can put them back.
    pub(crate) bytes: Option<Vec<u8>>,
    /// The same bytes as a blake3 digest, cheap to compare later.
    pub(crate) hash: Option<String>,
}

impl Baseline {
    /// Whether the raw file asks to be left alone, with or without a `parser:`.
    ///
    /// A raw that predates the field, or one a person edited by hand, may carry
    /// only `pin: true`; treating that as "not pinned" would overwrite it.
    pub(crate) fn is_pinned(&self) -> bool {
        matches!(self.state, RawState::Pinned { .. })
    }

    /// The parser recorded in a pinned raw's frontmatter, if any.
    pub(crate) fn pinned_parser(&self) -> Option<String> {
        match &self.state {
            RawState::Pinned { parser } => parser.clone(),
            _ => None,
        }
    }

    /// The source path claimed by a foreign raw.
    pub(crate) fn foreign_source(&self) -> Option<&str> {
        match &self.state {
            RawState::Foreign { source } => Some(source),
            _ => None,
        }
    }
}

/// Reads the raw file once, recording its bytes, digest and ownership.
pub(crate) fn inspect(raw_path: &Path, source_ref: &str) -> Result<Baseline> {
    if !raw_path.exists() {
        return Ok(Baseline {
            state: RawState::Absent,
            bytes: None,
            hash: None,
        });
    }
    let bytes = std::fs::read(raw_path).map_err(|source| ConvertError::Io {
        path: raw_path.to_path_buf(),
        source,
    })?;
    let state = classify(&bytes, source_ref);
    Ok(Baseline {
        state,
        hash: Some(hash::hash_bytes(&bytes)),
        bytes: Some(bytes),
    })
}

/// Decides who owns `bytes`; the file is the authority, not the ledger.
fn classify(bytes: &[u8], source_ref: &str) -> RawState {
    let text = String::from_utf8_lossy(bytes);
    match frontmatter_field(&text, SOURCE_FIELD) {
        Some(found) if found == source_ref => {
            if is_pinned(&text) {
                RawState::Pinned {
                    parser: frontmatter_field(&text, "parser"),
                }
            } else {
                RawState::Overwritable
            }
        }
        Some(found) => RawState::Foreign { source: found },
        None => RawState::Foreign {
            source: String::from("an unknown source (no `source` field)"),
        },
    }
}

/// Whether the raw file's `pin` field is `true`.
fn is_pinned(text: &str) -> bool {
    frontmatter_field(text, "pin").is_some_and(|value| value.eq_ignore_ascii_case("true"))
}

/// Re-checks everything [`inspect`] and the ledger said, right before writing.
///
/// Checks run in the order that makes the report most useful: the source first
/// (a changed source usually explains the rest), then the raw file, then the
/// ledger mapping. Any disagreement is a [`ConvertError::RawConflict`] and
/// nothing is written.
pub(crate) fn recheck(store: &Store, ready: &Ready) -> Result<()> {
    confirm_source_unchanged(ready)?;
    confirm_raw_unchanged(ready)?;
    confirm_ledger(store, ready)?;
    Ok(())
}

/// Re-reads the source and compares its digest with the one that was parsed.
fn confirm_source_unchanged(ready: &Ready) -> Result<()> {
    let input = hash::read_source(&ready.source_path, ready.max_source_bytes)?;
    if input.hash != ready.source_row.hash {
        return Err(conflict(
            &ready.raw_path,
            format!(
                "{} changed after it was read (expected {}, found {})",
                ready.source_ref, ready.source_row.hash, input.hash
            ),
        ));
    }
    Ok(())
}

/// Re-reads the raw file and compares it with the baseline taken at prepare.
fn confirm_raw_unchanged(ready: &Ready) -> Result<()> {
    let current = inspect(&ready.raw_path, &ready.source_ref)?;
    match (&ready.baseline.hash, &current.hash) {
        (None, None) => {}
        (None, Some(_)) => {
            return Err(conflict(
                &ready.raw_path,
                String::from("a raw file appeared there after the conversion started"),
            ));
        }
        (Some(_), None) => {
            return Err(conflict(
                &ready.raw_path,
                String::from("the previous raw file disappeared after the conversion started"),
            ));
        }
        (Some(expected), Some(found)) if expected != found => {
            return Err(conflict(
                &ready.raw_path,
                String::from(
                    "the raw file was edited after the conversion started; \
                     run the conversion again to replace it deliberately",
                ),
            ));
        }
        (Some(_), Some(_)) => {}
    }
    // The bytes match, but `pin` can be flipped without changing ownership.
    if current.state.is_pinned() {
        return Err(conflict(
            &ready.raw_path,
            String::from("the raw file was pinned after the conversion started"),
        ));
    }
    Ok(())
}

/// Confirms the ledger and the raw file still describe the same conversion.
///
/// The source digest moving on disk is normal — that is what a re-conversion is
/// for — so the comparison is between the ledger and the raw file, the two
/// records of the last conversion. Fewer than two matching records means the
/// file is no longer provably the machine output: a missing `raws` row, a digest
/// that differs from the recorded one, or a `source_hash` the ledger disagrees
/// with all end the conversion with nothing written. A body edit keeps the
/// frontmatter intact, so only the digest can catch it — which is why it is
/// checked.
fn confirm_ledger(store: &Store, ready: &Ready) -> Result<()> {
    let recorded = ready
        .baseline
        .bytes
        .as_deref()
        .and_then(|bytes| frontmatter_field(&String::from_utf8_lossy(bytes), "source_hash"));
    check_records(
        store,
        &ready.source_row.key,
        &ready.raw_row.key,
        &ready.raw_path,
        ready.baseline.hash.as_deref(),
        recorded.as_deref(),
    )
}

/// The one comparison of ledger rows against the raw file on disk.
///
/// Shared by the real run and the dry run so a preview cannot promise a
/// conversion the real run would refuse. `raw_digest` is `None` when there is no
/// file; a file without a ledger row, a digest that differs from the recorded
/// one, or free text posing as a raw all count as "not provably machine output".
fn check_records(
    store: &Store,
    source_key: &str,
    raw_rel: &str,
    raw_path: &Path,
    raw_digest: Option<&str>,
    raw_source_hash: Option<&str>,
) -> Result<()> {
    let source_row = store.get_source(source_key)?;
    // A raw row without the matching source row is half a record: the last
    // conversion never committed both, so nothing proves the file is ours.
    if raw_digest.is_some() && store.get_raw(raw_rel)?.is_some() && source_row.is_none() {
        return Err(conflict(
            raw_path,
            format!(
                "the ledger records the raw file but not the source {source_key}, so the file cannot be proven to be machine output; delete it to let it be rebuilt"
            ),
        ));
    }
    if let (Some(row), Some(recorded)) = (source_row, raw_source_hash) {
        if row.hash != recorded {
            return Err(conflict(
                raw_path,
                format!(
                    "the ledger records {source_key} at {}, but the raw file records {recorded}",
                    row.hash
                ),
            ));
        }
    }
    match (raw_digest, store.get_raw(raw_rel)?) {
        // No raw file: nothing of ours to destroy, so it may be recreated.
        (None, _) => Ok(()),
        (Some(_), None) => Err(conflict(
            raw_path,
            String::from(
                "no ledger row records this raw file, so it cannot be proven to be machine \
                 output; delete it to let it be rebuilt",
            ),
        )),
        (Some(found), Some(row)) => {
            if row.source_key != source_key {
                return Err(conflict(
                    raw_path,
                    format!(
                        "the ledger says {} belongs to {}, not {source_key}",
                        raw_path.display(),
                        row.source_key
                    ),
                ));
            }
            if row.hash != found {
                return Err(conflict(
                    raw_path,
                    format!(
                        "the raw file is not the one the last conversion wrote (the ledger \
                         records {}, the file is {found}); it may have been edited, and \
                         deleting it lets the conversion rebuild it",
                        row.hash
                    ),
                ));
            }
            Ok(())
        }
    }
}

/// Runs the protection checks alone, without parsing or writing.
///
/// This is what `ingest --dry-run` needs: the same refusals a real run would
/// produce for a pinned, foreign or stale raw, but decided from file metadata,
/// digests and the ledger only. A pinned raw is reported as `Ok(Some(parser))`
/// rather than an error, so the dry run can say `skipped (pinned)` like the real
/// run; a foreign or stale one is an error, because no run would overwrite it.
pub(crate) fn preflight(
    root: &Path,
    cfg: &Config,
    store: &Store,
    key: &str,
) -> Result<Option<String>> {
    let source_path = paths::join_key(&root.join(&cfg.paths.source), key);
    let source_ref = slash_join(&cfg.paths.source, key);
    let raw_rel = crate::convert::raw_key_for(key);
    let raw_key = slash_join(&cfg.paths.raw, &raw_rel);
    let raw_path = paths::join_key(root, &raw_key);

    // The source is read for its side conditions (readable, within the size
    // limit); its digest is the real run's business, not the dry run's.
    hash::read_source(&source_path, cfg.parse.max_source_bytes)?;
    let baseline = inspect(&raw_path, &source_ref)?;
    if let Some(existing) = baseline.foreign_source() {
        return Err(ConvertError::RawCollision {
            raw_path: raw_key,
            existing: String::from(existing),
            key: source_ref,
        });
    }
    // A pinned raw is never replaced, so its digest is beside the point: the
    // preview says "skipped" for the same reason the real run would.
    if baseline.is_pinned() {
        return Ok(Some(baseline.pinned_parser().unwrap_or_default()));
    }
    let recorded = baseline
        .bytes
        .as_deref()
        .and_then(|bytes| frontmatter_field(&String::from_utf8_lossy(bytes), "source_hash"));
    check_records(
        store,
        key,
        &raw_rel,
        &raw_path,
        baseline.hash.as_deref(),
        recorded.as_deref(),
    )?;
    Ok(None)
}

impl RawState {
    /// Whether the state means "hand-maintained, leave it alone".
    pub(crate) fn is_pinned(&self) -> bool {
        matches!(self, RawState::Pinned { .. })
    }
}

/// Builds a [`ConvertError::RawConflict`] naming the raw file and the reason.
fn conflict(raw_path: &Path, reason: impl Into<String>) -> ConvertError {
    ConvertError::RawConflict {
        raw_path: raw_path.display().to_string(),
        reason: reason.into(),
    }
}

/// The value of `name` in the first frontmatter block of `text`.
///
/// Only top-level `key: value` lines are understood, which is all this module
/// writes; anything else (indented children, block scalars, single quotes) is
/// reported as absent, and [`classify`] treats that as "owner unknown".
pub(crate) fn frontmatter_field(text: &str, name: &str) -> Option<String> {
    let body = text.strip_prefix("---\n")?;
    for line in body.split_inclusive('\n') {
        let line = line.trim_end_matches('\n');
        if line.trim_end() == "---" {
            return None;
        }
        let Some(rest) = line
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix(':'))
        else {
            continue;
        };
        return Some(unquote(rest.trim()));
    }
    None
}

/// Removes one layer of YAML quoting from `value`.
pub(crate) fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        let inner = &value[1..value.len() - 1];
        let mut out = String::with_capacity(inner.len());
        let mut chars = inner.chars();
        while let Some(c) = chars.next() {
            if c != '\\' {
                out.push(c);
                continue;
            }
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('r') => out.push('\r'),
                Some('t') => out.push('\t'),
                Some(other) => out.push(other),
                None => break,
            }
        }
        return out;
    }
    String::from(value)
}
