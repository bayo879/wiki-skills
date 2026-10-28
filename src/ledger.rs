//! The one place the ledger is written.
//!
//! [`Store`] can write the `sources` and `raws` tables, but only this module
//! does. Three callers have a reason to change the ledger — `convert` registers
//! what it wrote, `moves` follows a rename, `archive purge` forgets what it
//! deleted — and each of them states its intent here instead of touching the
//! tables itself. `tests/invariants.rs::only_ledger_writes_the_ledger` holds
//! production code to that.

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
        self.store.upsert_source(source)?;
        self.store.upsert_raw(raw)
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
            RawRelocation::InPlace(row) => self.store.upsert_raw(&row)?,
            RawRelocation::Moved { from: old, row } => {
                self.store.delete_raw(&old)?;
                if let Some(row) = row {
                    self.store.upsert_raw(&row)?;
                }
            }
            RawRelocation::Absent => {}
        }
        self.store.delete_source(from)?;
        self.store.upsert_source(to)
    }

    /// Forget a source and, when it had one, its raw row.
    pub fn forget_source(&mut self, key: &str, raw_key: Option<&str>) -> Result<()> {
        if let Some(raw_key) = raw_key {
            self.store.delete_raw(raw_key)?;
        }
        self.store.delete_source(key)
    }

    /// Forget a sealed archive summary.
    pub fn forget_archive(&mut self, key: &str) -> Result<()> {
        self.store.delete_archive(key)
    }
}
