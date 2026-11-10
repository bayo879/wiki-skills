//! The one place the ledger is written.
//!
//! [`Store`] can write the `sources` and `raws` tables, but only this module
//! does. Four callers have a reason to change the ledger — `convert` registers
//! what it wrote, and `moves` follows a rename.
//! deleted, `reject` accepts the bytes a human just reviewed — and each of them
//! states its intent here instead of touching the tables itself.
//! `tests/invariants.rs::only_ledger_writes_the_ledger` holds production code
//! to that.

use crate::store::{RawRow, Result, SourceRow, Store};

/// What moving a source did to its raw row.
pub enum RawRelocation {
    /// The raw file kept its key; only its source pointer or digest changed.
    InPlace(RawRow),
    /// The raw file moved: `from` goes away, `row` appears when there was one.
    Moved { from: String, row: Option<RawRow> },
    /// This source has no raw file.
    Absent,
}

/// A handle that exists only to be the ledger's single write path.
pub struct Ledger<'a> {
    store: &'a mut Store,
}

impl<'a> Ledger<'a> {
    pub fn new(store: &'a mut Store) -> Self {
        Self { store }
    }

    /// Register what a conversion just wrote: the `sources` row, then the
    /// `raws` row.
    ///
    /// The file is already on disk. Writing `sources` first means a crash in
    /// between leaves a source that claims a raw the ledger does not know,
    /// which `scan` reports as a `new` raw and `lint` as `L7` — visible, and
    /// repaired by `convert --key`.
    pub fn record_conversion(&mut self, source: &SourceRow, raw: &RawRow) -> Result<()> {
        self.store.register_conversion(source, raw, None)
    }

    /// Conversion plus precise observations commit as one unit.
    pub fn record_conversion_input(
        &mut self,
        source: &SourceRow,
        raw: &RawRow,
        mtime_ns: Option<i64>,
    ) -> Result<()> {
        self.store.register_conversion(source, raw, mtime_ns)
    }

    /// Record precise metadata only after successful conversion registration.
    pub fn record_source_facts(&mut self, source: &SourceRow, mtime_ns: i64) -> Result<()> {
        self.store
            .record_source_facts(&source.key, &source.hash, source.size, mtime_ns)
    }

    /// Remember that this source failed to convert, so the next round can put
    /// it behind the files that have not been tried yet.
    pub fn record_failure(&mut self, key: &str, attempted_at: &str, error: &str) -> Result<()> {
        self.store.record_attempt(key, attempted_at, error)
    }

    /// Forget the failure note for a source that never converted and is now
    /// gone, so a note cannot outlive the file it describes.
    pub fn forget_failure(&mut self, key: &str) -> Result<()> {
        self.store.delete_attempt(key)
    }

    /// Follow a source rename: raw rows first, the `sources` row last, so a
    /// failure anywhere above leaves the old key readable and the next round
    /// retries.
    pub fn relocate_source(
        &mut self,
        from: &str,
        to: &SourceRow,
        raw: RawRelocation,
    ) -> Result<()> {
        match raw {
            RawRelocation::InPlace(row) => {
                self.store.relocate_source_work(from, to, None, Some(&row))
            }
            RawRelocation::Moved { from: old, row } => {
                self.store
                    .relocate_source_work(from, to, Some(&old), row.as_ref())
            }
            RawRelocation::Absent => self.store.relocate_source_work(from, to, None, None),
        }
    }

    pub fn import_work_item(&mut self, item: &crate::store::WorkItem) -> Result<()> {
        self.store.import_work_item(item)
    }

    pub fn complete_work_item(&mut self, key: &str, raw_key: &str, raw_hash: &str) -> Result<bool> {
        self.store.complete_work_item(key, raw_key, raw_hash)
    }

    /// Forget a source and, when it had one, its raw row.
    pub fn forget_source(&mut self, key: &str, raw_key: Option<&str>) -> Result<()> {
        if let Some(raw_key) = raw_key {
            self.store.delete_raw(raw_key)?;
        }
        self.store.delete_source(key)
    }

    /// Accept the bytes on disk as the state a human has reviewed.
    ///
    /// A rejected round is a judgement on the *sources*, not only on the pages:
    /// the change was looked at and refused as wiki material, which still
    /// settles what the ledger knows about that file. Only the file facts move;
    /// `raw_key` and `converted_at` stay, because the raw on disk is still the
    /// one this source last produced.
    pub fn mark_reviewed(&mut self, row: &SourceRow) -> Result<()> {
        self.store.upsert_source(row)
    }
}
