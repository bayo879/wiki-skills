//! `state.db`: the SQLite ledger under the engine directory.
//!
//! The database records what `source/` contains, which raw markdown belongs to
//! which source, the version and sync bookkeeping, and the full-text index the
//! query commands search. It lives at `<root>/.wiki/state.db` and is disposable
//! in principle — it can be rebuilt from `source/` — but losing it forces a full
//! re-scan, so the layout is versioned with `PRAGMA user_version` and migrated in
//! place.
//!
//! One [`Store`] owns one `rusqlite::Connection`; concurrent access is handled by
//! SQLite's own locking rather than by a pool. SQL transaction bookkeeping for
//! sync runs is a later concern: this module only creates the `pending` table.

use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};

/// Schema version this build reads and writes.
pub const SCHEMA_VERSION: i64 = 7;

/// High precision observations only certify bytes that were successfully converted.
const MIGRATION_7: &str = r#"
CREATE TABLE source_facts(
    key TEXT PRIMARY KEY,
    hash TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime_ns INTEGER NOT NULL
);
CREATE TABLE work_items(
    source_key TEXT PRIMARY KEY,
    raw_key TEXT NOT NULL,
    raw_hash TEXT NOT NULL
);
CREATE TABLE legacy_work_imports(raw_key TEXT PRIMARY KEY);
"#;

/// One source still awaiting knowledge curation.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorkItem {
    pub source_key: String,
    pub raw_key: String,
    pub raw_hash: String,
}

/// DDL for version 1, applied once per database.
const MIGRATION_1: &str = r#"
CREATE TABLE sources(
    key TEXT PRIMARY KEY,
    hash TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime INTEGER NOT NULL,
    raw_key TEXT,
    converted_at TEXT
);
CREATE TABLE versions(
    id INTEGER PRIMARY KEY,
    created_at TEXT NOT NULL,
    note TEXT,
    kind TEXT NOT NULL
);
CREATE TABLE pending(
    id INTEGER PRIMARY KEY CHECK(id = 1),
    raw_key TEXT,
    raw_hash TEXT,
    index_hash TEXT,
    log_hash TEXT,
    began_at TEXT
);
CREATE TABLE sync_state(
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE VIRTUAL TABLE fts USING fts5(
    page_id UNINDEXED,
    title,
    body,
    tokenize = 'unicode61'
);
"#;

/// DDL for version 2: the normalised raw layer's ledger.
///
/// Version 1 is left untouched; this only adds the table, so an in-place upgrade
/// keeps every `sources` row and its page mapping.
const MIGRATION_2: &str = r#"
CREATE TABLE raws(
    key TEXT PRIMARY KEY,
    hash TEXT NOT NULL,
    source_key TEXT NOT NULL,
    converted_at TEXT NOT NULL
);
"#;

/// DDL for version 3: what each page looked like when the CLI last wrote it.
///
/// The table is what lets `lint` tell "body edited but `updated` left alone"
/// apart from "page written before this build existed". An empty table is the
/// normal state of a fresh clone, so a missing record is silent.
const MIGRATION_3: &str = r#"
CREATE TABLE page_meta(
    page_key TEXT PRIMARY KEY,
    hash TEXT NOT NULL,
    updated TEXT NOT NULL
);
"#;

/// DDL for version 4: the sealed-session archive ledger.
///
/// Kept as history: version 6 drops the table again. A database that starts at
/// v4 and one that starts fresh both pass through it, which is why the migration
/// is never edited.
const MIGRATION_4: &str = r#"
CREATE TABLE archive(
    key TEXT PRIMARY KEY,
    period_start TEXT NOT NULL,
    period_end TEXT NOT NULL,
    sources_json TEXT NOT NULL,
    sealed_at TEXT NOT NULL,
    sealed_by TEXT NOT NULL,
    last_recalled_at TEXT,
    recall_count INTEGER NOT NULL DEFAULT 0
);
"#;

/// DDL for version 5: the `source_pages` mapping is gone.
///
/// v1 created it, but nothing ever wrote a row: which pages cite a source is
/// already recorded in each page's `sources:` field, and a second copy in SQLite
/// is the kind of duplication that drifts. The table is dropped rather than left
/// empty, so a reader of the schema is not invited to start filling it.
const MIGRATION_5: &str = r#"
DROP TABLE IF EXISTS source_pages;
"#;

/// Column order shared by the `sources` reader helpers.
const SOURCE_COLUMNS: &str = "key, hash, size, mtime, raw_key, converted_at";

/// DDL for version 6: the archive table is gone.
///
/// The archive family was removed from the CLI, so nothing writes this table any
/// more. Dropping it keeps a fresh database and an upgraded one identical, and a
/// reader of the schema is not invited to revive it.
const MIGRATION_6: &str = r#"
DROP TABLE IF EXISTS archive;
"#;

/// Column order shared by the `page_meta` reader helpers.
const PAGE_META_COLUMNS: &str = "page_key, hash, updated";

/// One `page_meta` record: what one page looked like when the CLI last wrote it.
///
/// `page_key` is the repository-relative path (`wiki/concept/a.md`), `hash` the
/// `blake3:` digest of the file's bytes and `updated` the frontmatter date it
/// carried. `lint` compares the record with the file on disk to notice a body
/// edit that never touched `updated`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageMetaRow {
    /// Repository-relative, `/`-separated page path.
    pub page_key: String,
    /// `blake3:` digest of the page bytes at write time.
    pub hash: String,
    /// Frontmatter `updated` value at write time, `YYYY-MM-DD`.
    pub updated: String,
}

impl PageMetaRow {
    /// Reads one `page_meta` row in [`PAGE_META_COLUMNS`] order.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PageMetaRow> {
        Ok(PageMetaRow {
            page_key: row.get(0)?,
            hash: row.get(1)?,
            updated: row.get(2)?,
        })
    }
}

/// One row returned by [`Store::fts_search`].
#[derive(Debug, Clone, PartialEq)]
pub struct FtsHit {
    /// Repository-relative path of the matching page.
    pub page_id: String,
    /// The title as it was indexed.
    pub title: String,
    /// Body excerpt chosen by FTS5, still in index (segmented) form.
    pub snippet: String,
    /// `-bm25`, so a larger number is a better match.
    pub score: f64,
}

/// Which pages [`Store::fts_search`] may return.
///
/// The patterns are SQL `LIKE` patterns against the page path; the caller owns
/// the escaping, because only the caller knows what a path prefix looks like.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FtsScope<'a> {
    /// Every indexed page.
    All,
    /// Only pages whose path matches the pattern.
    Like(&'a str),
    /// Only pages that match none of the patterns.
    ///
    /// One pattern is not enough to exclude a directory: a page under it lands
    /// at either `<wiki>/<type>/<dir>/` or `<wiki>/<dir>/`, and the caller must
    /// pass both.
    NotLike(&'a [&'a str]),
}

/// One row of the `sources` ledger: a file that exists under `source/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRow {
    /// Path identity key of the source, `/`-separated and NFC-normalised.
    pub key: String,
    /// `blake3:` digest of the file's content.
    pub hash: String,
    /// File length in bytes.
    pub size: i64,
    /// Modification time in seconds since the Unix epoch.
    pub mtime: i64,
    /// Key of the raw markdown this source was converted into, if any.
    pub raw_key: Option<String>,
    /// When the most recent conversion finished, if it has.
    pub converted_at: Option<String>,
}

/// Column order shared by the `raws` reader helpers.
const RAW_COLUMNS: &str = "key, hash, source_key, converted_at";

/// One row of the `raws` ledger: a normalised markdown file under `raw/`.
///
/// `hash` digests the bytes that are actually on disk, frontmatter included, so
/// a hand edit — even one that only changes a line ending — is visible to
/// [`crate::scan::scan_raw`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawRow {
    /// Path identity key of the raw file relative to `raw/`, `/`-separated and
    /// NFC-normalised.
    pub key: String,
    /// `blake3:` digest of the raw file's final bytes.
    pub hash: String,
    /// Key of the source this raw markdown was converted from.
    pub source_key: String,
    /// When the most recent conversion wrote this file.
    pub converted_at: String,
}

impl RawRow {
    /// Reads one `raws` row in [`RAW_COLUMNS`] order.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRow> {
        Ok(RawRow {
            key: row.get(0)?,
            hash: row.get(1)?,
            source_key: row.get(2)?,
            converted_at: row.get(3)?,
        })
    }
}

impl SourceRow {
    /// Reads one `sources` row in [`SOURCE_COLUMNS`] order.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SourceRow> {
        Ok(SourceRow {
            key: row.get(0)?,
            hash: row.get(1)?,
            size: row.get(2)?,
            mtime: row.get(3)?,
            raw_key: row.get(4)?,
            converted_at: row.get(5)?,
        })
    }
}

/// Failure while opening the database, migrating it, or running a query.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// SQLite reported an error.
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A directory that must hold the database file could not be created.
    #[error("cannot prepare {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    /// The filesystem refused to switch the connection to WAL.
    #[error("cannot enable WAL on {}: sqlite kept journal_mode={mode}", path.display())]
    WalUnavailable { path: PathBuf, mode: String },
    /// The file was written by a newer build and cannot be read here.
    #[error(
        "{} has schema version {found}, but this build supports up to {supported}",
        path.display()
    )]
    SchemaTooNew {
        path: PathBuf,
        found: i64,
        supported: i64,
    },
}

/// Result of the store helpers.
pub type Result<T> = std::result::Result<T, StoreError>;

/// Whether the linked SQLite was built with FTS5.
///
/// The probe opens a private in-memory connection, so it answers a question
/// about the build rather than about one repository, and it writes nothing.
/// `doctor` needs that shape: it must be able to say "this binary cannot keep
/// the index" before a `state.db` exists.
pub fn fts5_supported() -> Result<bool> {
    let conn = Connection::open_in_memory()?;
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM pragma_compile_options \
         WHERE compile_options = 'ENABLE_FTS5'",
        [],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

/// A connection to one `state.db` file.
pub struct Store {
    conn: Connection,
    /// Path of the database file, kept for error messages.
    path: PathBuf,
}

impl Store {
    /// Opens `path`, creating its parent directory and the file if needed.
    ///
    /// The connection is switched to WAL, which is required for readers during a
    /// sync run; a filesystem that cannot do WAL is an error rather than a
    /// silent downgrade. Foreign keys are enforced. The schema is not touched —
    /// call [`Store::migrate`] for that.
    pub fn open(path: &Path) -> Result<Store> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                    path: parent.to_path_buf(),
                    source,
                })?;
            }
        }
        let conn = Connection::open(path)?;
        let mode: String =
            conn.pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::WalUnavailable {
                path: path.to_path_buf(),
                mode,
            });
        }
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(Store {
            conn,
            path: path.to_path_buf(),
        })
    }

    /// Brings the schema up to [`SCHEMA_VERSION`], one version at a time.
    ///
    /// Idempotent: a database already at the current version is left untouched,
    /// and every step applies only when the file is still behind it, so a
    /// version-1 database becomes version 2 without touching the v1 tables. A
    /// database from a newer build is refused instead of being written under
    /// assumptions that no longer hold.
    pub fn migrate(&mut self) -> Result<()> {
        let mut version = self.user_version()?;
        if version > SCHEMA_VERSION {
            return Err(StoreError::SchemaTooNew {
                path: self.path.clone(),
                found: version,
                supported: SCHEMA_VERSION,
            });
        }
        if version == SCHEMA_VERSION {
            return Ok(());
        }
        if version < 1 {
            self.apply_migration(MIGRATION_1, 1)?;
            version = 1;
        }
        if version < 2 {
            self.apply_migration(MIGRATION_2, 2)?;
            version = 2;
        }
        if version < 3 {
            self.apply_migration(MIGRATION_3, 3)?;
        }
        if version < 4 {
            self.apply_migration(MIGRATION_4, 4)?;
        }
        if version < 5 {
            self.apply_migration(MIGRATION_5, 5)?;
            version = 5;
        }
        if version < 6 {
            self.apply_migration(MIGRATION_6, 6)?;
        }
        if version < 7 {
            self.apply_migration(MIGRATION_7, 7)?;
        }
        Ok(())
    }

    /// Whether precise file facts certify this converted digest.
    pub fn source_facts_match(
        &self,
        key: &str,
        hash: &str,
        size: i64,
        mtime_ns: i64,
    ) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM source_facts WHERE key=?1 AND hash=?2 AND size=?3 AND mtime_ns=?4)",
            params![key, hash, size, mtime_ns], |row| row.get(0))?)
    }

    pub(crate) fn record_source_facts(
        &mut self,
        key: &str,
        hash: &str,
        size: i64,
        mtime_ns: i64,
    ) -> Result<()> {
        self.conn.execute("INSERT INTO source_facts(key,hash,size,mtime_ns) VALUES(?1,?2,?3,?4) ON CONFLICT(key) DO UPDATE SET hash=excluded.hash,size=excluded.size,mtime_ns=excluded.mtime_ns", params![key,hash,size,mtime_ns])?;
        Ok(())
    }

    /// Reads `PRAGMA user_version`, the schema generation of the file.
    ///
    /// Public because `doctor` reports a database left behind by a newer build
    /// without opening a migration on it.
    pub fn user_version(&self) -> Result<i64> {
        Ok(self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?)
    }

    /// Runs one migration's DDL and stamps its version in the same transaction.
    ///
    /// DDL and the stamp move together, so an interrupted migration leaves the
    /// file at the previous version and the next run retries from there instead
    /// of finding half a schema it believes is current.
    fn apply_migration(&mut self, sql: &str, version: i64) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", version)?;
        tx.commit()?;
        Ok(())
    }

    /// Every source in the ledger, ordered by key.
    pub fn all_sources(&self) -> Result<Vec<SourceRow>> {
        let sql = format!("SELECT {SOURCE_COLUMNS} FROM sources ORDER BY key");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], SourceRow::from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The ledger row for `key`, or `None` when the source is unknown.
    pub fn get_source(&self, key: &str) -> Result<Option<SourceRow>> {
        let sql = format!("SELECT {SOURCE_COLUMNS} FROM sources WHERE key = ?1");
        let row = self
            .conn
            .query_row(&sql, [key], SourceRow::from_row)
            .optional()?;
        Ok(row)
    }

    pub(crate) fn relocate_source_work(
        &mut self,
        from: &str,
        to: &SourceRow,
        old_raw: Option<&str>,
        raw: Option<&RawRow>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        if let Some(raw) = raw {
            tx.execute(
                "UPDATE work_items SET source_key=?1,raw_key=?2,raw_hash=?3 WHERE source_key=?4",
                params![to.key, raw.key, raw.hash, from],
            )?;
            if let Some(old) = old_raw {
                tx.execute("DELETE FROM raws WHERE key=?1", [old])?;
            }
            tx.execute("INSERT INTO raws(key,hash,source_key,converted_at) VALUES(?1,?2,?3,?4) ON CONFLICT(key) DO UPDATE SET hash=excluded.hash,source_key=excluded.source_key,converted_at=excluded.converted_at", params![raw.key,raw.hash,raw.source_key,raw.converted_at])?;
        } else {
            tx.execute("DELETE FROM work_items WHERE source_key=?1", [from])?;
            if let Some(old) = old_raw {
                tx.execute("DELETE FROM raws WHERE key=?1", [old])?;
            }
        }
        tx.execute("DELETE FROM source_facts WHERE key=?1", [from])?;
        tx.execute("DELETE FROM sources WHERE key=?1", [from])?;
        tx.execute("INSERT INTO sources(key,hash,size,mtime,raw_key,converted_at) VALUES(?1,?2,?3,?4,?5,?6)", params![to.key,to.hash,to.size,to.mtime,to.raw_key,to.converted_at])?;
        tx.commit()?;
        Ok(())
    }

    pub fn legacy_work_imported(&self, raw_key: &str) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM legacy_work_imports WHERE raw_key=?1)",
            [raw_key],
            |row| row.get(0),
        )?)
    }

    pub(crate) fn import_work_item(&mut self, item: &WorkItem) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO work_items(source_key,raw_key,raw_hash) VALUES(?1,?2,?3) ON CONFLICT(source_key) DO NOTHING", params![item.source_key,item.raw_key,item.raw_hash])?;
        tx.execute(
            "INSERT OR IGNORE INTO legacy_work_imports(raw_key) VALUES(?1)",
            [&item.raw_key],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn complete_work_item(
        &mut self,
        key: &str,
        raw_key: &str,
        raw_hash: &str,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let removed = tx.execute(
            "DELETE FROM work_items WHERE source_key=?1 AND raw_key=?2 AND raw_hash=?3",
            params![key, raw_key, raw_hash],
        )?;
        tx.commit()?;
        Ok(removed != 0)
    }

    /// All unfinished sources in stable key order.
    pub fn work_items(&self) -> Result<Vec<WorkItem>> {
        let mut stmt = self
            .conn
            .prepare("SELECT source_key,raw_key,raw_hash FROM work_items ORDER BY source_key")?;
        let rows = stmt.query_map([], |row| {
            Ok(WorkItem {
                source_key: row.get(0)?,
                raw_key: row.get(1)?,
                raw_hash: row.get(2)?,
            })
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// Atomically register the exact parsed source and rendered raw, and enqueue it.
    pub(crate) fn register_conversion(
        &mut self,
        source: &SourceRow,
        raw: &RawRow,
        mtime_ns: Option<i64>,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("INSERT INTO sources(key,hash,size,mtime,raw_key,converted_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(key) DO UPDATE SET hash=excluded.hash,size=excluded.size,mtime=excluded.mtime,raw_key=excluded.raw_key,converted_at=excluded.converted_at", params![source.key,source.hash,source.size,source.mtime,source.raw_key,source.converted_at])?;
        tx.execute("INSERT INTO raws(key,hash,source_key,converted_at) VALUES(?1,?2,?3,?4) ON CONFLICT(key) DO UPDATE SET hash=excluded.hash,source_key=excluded.source_key,converted_at=excluded.converted_at", params![raw.key,raw.hash,raw.source_key,raw.converted_at])?;
        tx.execute("INSERT INTO work_items(source_key,raw_key,raw_hash) VALUES(?1,?2,?3) ON CONFLICT(source_key) DO UPDATE SET raw_key=excluded.raw_key,raw_hash=excluded.raw_hash", params![source.key,raw.key,raw.hash])?;
        if let Some(ns) = mtime_ns {
            tx.execute("INSERT INTO source_facts(key,hash,size,mtime_ns) VALUES(?1,?2,?3,?4) ON CONFLICT(key) DO UPDATE SET hash=excluded.hash,size=excluded.size,mtime_ns=excluded.mtime_ns", params![source.key,source.hash,source.size,ns])?;
        } else {
            tx.execute("DELETE FROM source_facts WHERE key=?1", [&source.key])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Inserts `row`, or replaces the existing row with the same key.
    ///
    /// Production code states its intent on [`crate::ledger::Ledger`] rather
    /// than calling this directly; tests seed rows here.
    pub fn upsert_source(&mut self, row: &SourceRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sources(key, hash, size, mtime, raw_key, converted_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(key) DO UPDATE SET \
                 hash = excluded.hash, size = excluded.size, mtime = excluded.mtime, \
                 raw_key = excluded.raw_key, converted_at = excluded.converted_at",
            params![
                row.key,
                row.hash,
                row.size,
                row.mtime,
                row.raw_key,
                row.converted_at
            ],
        )?;
        Ok(())
    }

    /// Removes the source `key`. Deleting an unknown key is not an error.
    pub fn delete_source(&mut self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM sources WHERE key = ?1", [key])?;
        Ok(())
    }

    /// One value of the `sync_state` table, or `None` when the key is unset.
    ///
    /// The table is the engine's scratch memory between rounds: stable streaks,
    /// last-seen raw hashes and the auto-ingest gate live here rather than in
    /// `wiki.toml`, because the CLI never rewrites the human's configuration.
    pub fn sync_state_get(&self, key: &str) -> Result<Option<String>> {
        let value = self
            .conn
            .query_row(
                "SELECT value FROM sync_state WHERE key = ?1",
                [key],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value)
    }

    /// Sets a `sync_state` value, replacing any previous one.
    pub fn sync_state_set(&mut self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sync_state(key, value) VALUES (?1, ?2) \
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    /// Every `sync_state` entry, ordered by key.
    pub fn sync_state_entries(&self) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM sync_state ORDER BY key")?;
        let rows = stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Every raw file in the ledger, ordered by key.
    pub fn all_raws(&self) -> Result<Vec<RawRow>> {
        let sql = format!("SELECT {RAW_COLUMNS} FROM raws ORDER BY key");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], RawRow::from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The ledger row for the raw key `key`, or `None` when it is unknown.
    pub fn get_raw(&self, key: &str) -> Result<Option<RawRow>> {
        let sql = format!("SELECT {RAW_COLUMNS} FROM raws WHERE key = ?1");
        let row = self
            .conn
            .query_row(&sql, [key], RawRow::from_row)
            .optional()?;
        Ok(row)
    }

    /// Inserts `row`, or replaces the existing row with the same key.
    ///
    /// Production code states its intent on [`crate::ledger::Ledger`] rather
    /// than calling this directly; tests seed rows here.
    pub fn upsert_raw(&mut self, row: &RawRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO raws(key, hash, source_key, converted_at) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT(key) DO UPDATE SET \
                 hash = excluded.hash, source_key = excluded.source_key, \
                 converted_at = excluded.converted_at",
            params![row.key, row.hash, row.source_key, row.converted_at],
        )?;
        Ok(())
    }

    /// Every `page_meta` record, ordered by page key.
    pub fn all_page_metas(&self) -> Result<Vec<PageMetaRow>> {
        let sql = format!("SELECT {PAGE_META_COLUMNS} FROM page_meta ORDER BY page_key");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], PageMetaRow::from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// The `page_meta` record for `page_key`, or `None` when never written.
    pub fn get_page_meta(&self, page_key: &str) -> Result<Option<PageMetaRow>> {
        let sql = format!("SELECT {PAGE_META_COLUMNS} FROM page_meta WHERE page_key = ?1");
        let row = self
            .conn
            .query_row(&sql, [page_key], PageMetaRow::from_row)
            .optional()?;
        Ok(row)
    }

    /// Inserts `row`, or replaces the record for the same page key.
    pub fn upsert_page_meta(&mut self, row: &PageMetaRow) -> Result<()> {
        self.conn.execute(
            "INSERT INTO page_meta(page_key, hash, updated) VALUES (?1, ?2, ?3) \
             ON CONFLICT(page_key) DO UPDATE SET \
                 hash = excluded.hash, updated = excluded.updated",
            params![row.page_key, row.hash, row.updated],
        )?;
        Ok(())
    }

    /// Removes the raw row for `key`. Deleting an unknown key is a no-op.
    pub fn delete_raw(&mut self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM raws WHERE key = ?1", [key])?;
        Ok(())
    }

    /// Appends one row to the `versions` ledger and returns its row id.
    pub fn record_version(&mut self, created_at: &str, note: &str, kind: &str) -> Result<i64> {
        self.conn.execute(
            "INSERT INTO versions(created_at, note, kind) VALUES (?1, ?2, ?3)",
            params![created_at, note, kind],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Number of rows in the `versions` ledger.
    pub fn version_count(&self) -> Result<i64> {
        let count: i64 = self
            .conn
            .query_row("SELECT count(*) FROM versions", [], |row| row.get(0))?;
        Ok(count)
    }

    /// Whether the linked SQLite was built with the FTS5 extension.
    ///
    /// The full-text index in the schema is useless without it, and the bundled
    /// build is expected to carry it, so a false here is a hard failure for the
    /// query commands rather than something to work around.
    pub fn fts5_available(&self) -> Result<bool> {
        fts5_supported()
    }

    /// Replaces the whole full-text index with `rows` in one transaction.
    ///
    /// Each row is `(page_id, title, body)`. The delete and the inserts land in
    /// one transaction, so a failure leaves the previous index intact instead of a
    /// half-emptied one.
    pub fn fts_replace_all(&mut self, rows: &[(String, String, String)]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM fts", [])?;
        Self::fts_insert(&tx, rows)?;
        tx.commit()?;
        Ok(())
    }

    /// Applies a delta to the full-text index in one transaction.
    ///
    /// Deletes happen by `page_id`; the upserts follow. A transaction here is
    /// what keeps a crashed run from leaving a page twice-indexed or missing.
    pub fn fts_apply(
        &mut self,
        delete: &[String],
        upsert: &[(String, String, String)],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        for page_id in delete {
            tx.execute("DELETE FROM fts WHERE page_id = ?1", params![page_id])?;
        }
        Self::fts_insert(&tx, upsert)?;
        tx.commit()?;
        Ok(())
    }

    /// Every page id currently in the full-text index, sorted.
    pub fn fts_page_ids(&self) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT page_id FROM fts ORDER BY page_id")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    /// Runs one FTS5 `MATCH` expression, best match first.
    ///
    /// `expression` must already be a valid FTS5 expression: this layer does not
    /// fix syntax, it only filters by scope and limits the result.
    pub fn fts_search(
        &self,
        expression: &str,
        limit: usize,
        scope: FtsScope<'_>,
    ) -> Result<Vec<FtsHit>> {
        let mut sql = String::from(
            "SELECT page_id, title, snippet(fts, 2, '', '', '…', 32), bm25(fts) \
             FROM fts WHERE fts MATCH ?1",
        );
        match scope {
            FtsScope::All => {}
            FtsScope::Like(_) => sql.push_str(" AND page_id LIKE ?3"),
            FtsScope::NotLike(patterns) => {
                for index in 0..patterns.len() {
                    sql.push_str(&format!(" AND page_id NOT LIKE ?{}", index + 3));
                }
            }
        }
        sql.push_str(" ORDER BY bm25(fts), page_id LIMIT ?2");

        let limit = limit as i64;
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = match scope {
            FtsScope::All => stmt
                .query_map(params![expression, limit], fts_hit_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            FtsScope::Like(pattern) => stmt
                .query_map(params![expression, limit, pattern], fts_hit_from_row)?
                .collect::<rusqlite::Result<Vec<_>>>()?,
            FtsScope::NotLike(patterns) => {
                let mut args: Vec<&dyn rusqlite::ToSql> = vec![&expression, &limit];
                for pattern in patterns {
                    args.push(pattern);
                }
                stmt.query_map(rusqlite::params_from_iter(args), fts_hit_from_row)?
                    .collect::<rusqlite::Result<Vec<_>>>()?
            }
        };
        Ok(rows)
    }

    /// Drops one page from the `page_meta` ledger.
    pub fn delete_page_meta(&mut self, page_key: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM page_meta WHERE page_key = ?1",
            params![page_key],
        )?;
        Ok(())
    }

    /// Inserts `(page_id, title, body)` rows into `fts`.
    fn fts_insert(conn: &Connection, rows: &[(String, String, String)]) -> Result<()> {
        let mut stmt = conn.prepare("INSERT INTO fts(page_id, title, body) VALUES (?1, ?2, ?3)")?;
        for (page_id, title, body) in rows {
            stmt.execute(params![page_id, title, body])?;
        }
        Ok(())
    }
}

/// Reads one [`Store::fts_search`] row in select order.
fn fts_hit_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<FtsHit> {
    let bm25: f64 = row.get(3)?;
    Ok(FtsHit {
        page_id: row.get(0)?,
        title: row.get(1)?,
        snippet: row.get(2)?,
        score: -bm25,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// A database exactly as the previous build left it: schema v1 with one
    /// converted source, written without going through [`Store::migrate`].
    fn write_v1_database(path: &Path) {
        let conn = Connection::open(path).unwrap();
        conn.execute_batch(MIGRATION_1).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute(
            "INSERT INTO sources(key, hash, size, mtime, raw_key, converted_at) \
             VALUES ('a.pdf', 'blake3:aa', 10, 1, 'raw/a.md', '2026-10-27T10:00:00Z')",
            [],
        )
        .unwrap();
    }

    fn raw_row(key: &str, hash: &str) -> RawRow {
        RawRow {
            key: String::from(key),
            hash: String::from(hash),
            source_key: String::from("a.pdf"),
            converted_at: String::from("2026-10-27T10:00:00Z"),
        }
    }

    fn user_version(store: &Store) -> i64 {
        store
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap()
    }

    /// Every object SQLite knows about, with the SQL that defines it, sorted.
    ///
    /// Two databases that agree on this agree on their tables, columns,
    /// indexes, triggers and FTS5 shadow tables — everything a migration can
    /// add or drop.
    fn schema_dump(store: &Store) -> Vec<(String, String, Option<String>)> {
        let mut stmt = store
            .conn
            .prepare("SELECT type, name, sql FROM sqlite_master ORDER BY type, name")
            .unwrap();
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .unwrap();
        rows.map(|row| row.unwrap()).collect()
    }

    #[test]
    fn a_database_upgraded_from_v1_ends_with_the_fresh_schema() {
        let d = tempdir().unwrap();

        // The long way: a file written by the v1 build, upgraded step by step.
        let old = d.path().join("old").join("state.db");
        std::fs::create_dir_all(old.parent().unwrap()).unwrap();
        write_v1_database(&old);
        let mut upgraded = Store::open(&old).unwrap();
        upgraded.migrate().unwrap();

        // The short way: an empty file, migrated from version 0.
        let fresh = d.path().join("fresh").join("state.db");
        let mut created = Store::open(&fresh).unwrap();
        created.migrate().unwrap();

        assert_eq!(user_version(&upgraded), SCHEMA_VERSION);
        assert_eq!(user_version(&created), SCHEMA_VERSION);
        assert_eq!(
            schema_dump(&upgraded),
            schema_dump(&created),
            "a migrated database must not differ from a fresh one"
        );
    }

    #[test]
    fn a_v1_database_is_upgraded_in_place() {
        let d = tempdir().unwrap();
        let p = d.path().join("state.db");
        write_v1_database(&p);

        let mut s = Store::open(&p).unwrap();
        assert_eq!(user_version(&s), 1);
        s.migrate().unwrap();
        assert_eq!(user_version(&s), SCHEMA_VERSION);

        // The v1 tables and their rows survive; only `source_pages` is dropped,
        // because nothing ever wrote to it.
        let row = s.get_source("a.pdf").unwrap().unwrap();
        assert_eq!(row.hash, "blake3:aa");
        assert_eq!(row.raw_key.as_deref(), Some("raw/a.md"));
        assert_eq!(row.converted_at.as_deref(), Some("2026-10-27T10:00:00Z"));

        // The new table exists and behaves.
        s.upsert_raw(&raw_row("a.md", "blake3:ab")).unwrap();
        assert_eq!(s.get_raw("a.md").unwrap().unwrap().hash, "blake3:ab");
    }

    #[test]
    fn migrating_a_v1_database_twice_stays_at_v2() {
        let d = tempdir().unwrap();
        let p = d.path().join("state.db");
        write_v1_database(&p);

        let mut s = Store::open(&p).unwrap();
        s.migrate().unwrap();
        s.upsert_raw(&raw_row("a.md", "blake3:ab")).unwrap();
        s.migrate().unwrap();
        assert_eq!(user_version(&s), SCHEMA_VERSION);
        assert_eq!(s.get_source("a.pdf").unwrap().unwrap().hash, "blake3:aa");
        assert_eq!(s.all_raws().unwrap(), vec![raw_row("a.md", "blake3:ab")]);

        // Reopening an already-upgraded file is a no-op as well.
        drop(s);
        let mut s = Store::open(&p).unwrap();
        s.migrate().unwrap();
        assert_eq!(user_version(&s), SCHEMA_VERSION);
        assert_eq!(s.all_raws().unwrap(), vec![raw_row("a.md", "blake3:ab")]);
    }

    /// A row recording one page's content hash at write time.
    fn page_meta(page_key: &str, hash: &str, updated: &str) -> PageMetaRow {
        PageMetaRow {
            page_key: String::from(page_key),
            hash: String::from(hash),
            updated: String::from(updated),
        }
    }

    #[test]
    fn page_meta_is_added_by_migration_v3_and_round_trips() {
        let d = tempdir().unwrap();
        let p = d.path().join("state.db");
        {
            let conn = Connection::open(&p).unwrap();
            conn.execute_batch(MIGRATION_1).unwrap();
            conn.execute_batch(MIGRATION_2).unwrap();
            conn.pragma_update(None, "user_version", 2).unwrap();
        }

        let mut s = Store::open(&p).unwrap();
        s.migrate().unwrap();
        assert_eq!(user_version(&s), SCHEMA_VERSION);

        let first = page_meta("wiki/concept/a.md", "blake3:aa", "2026-10-27");
        s.upsert_page_meta(&first).unwrap();
        assert_eq!(
            s.get_page_meta("wiki/concept/a.md").unwrap(),
            Some(first.clone())
        );
        assert_eq!(s.get_page_meta("wiki/concept/missing.md").unwrap(), None);

        // Upsert replaces the row rather than adding a second one.
        let second = page_meta("wiki/concept/a.md", "blake3:bb", "2026-10-28");
        s.upsert_page_meta(&second).unwrap();
        assert_eq!(s.all_page_metas().unwrap(), vec![second]);
    }

    #[test]
    fn a_database_from_a_newer_build_is_refused() {
        let d = tempdir().unwrap();
        let p = d.path().join("state.db");
        {
            let conn = Connection::open(&p).unwrap();
            conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
                .unwrap();
        }

        let mut s = Store::open(&p).unwrap();
        match s.migrate() {
            Err(StoreError::SchemaTooNew {
                found, supported, ..
            }) => {
                assert_eq!(found, SCHEMA_VERSION + 1);
                assert_eq!(supported, SCHEMA_VERSION);
            }
            other => panic!("expected SchemaTooNew, got {other:?}"),
        }
        // A refused migration must leave the file alone.
        assert_eq!(user_version(&s), SCHEMA_VERSION + 1);
    }
}
