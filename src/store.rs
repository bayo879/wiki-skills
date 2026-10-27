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
const SCHEMA_VERSION: i64 = 1;

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
CREATE TABLE source_pages(
    raw_key TEXT NOT NULL,
    page_id TEXT NOT NULL,
    PRIMARY KEY(raw_key, page_id)
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

/// Column order shared by the `sources` reader helpers.
const SOURCE_COLUMNS: &str = "key, hash, size, mtime, raw_key, converted_at";

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

    /// Brings the schema up to [`SCHEMA_VERSION`].
    ///
    /// Idempotent: a database already at the current version is left untouched.
    /// A database from a newer build is refused instead of being written under
    /// assumptions that no longer hold.
    pub fn migrate(&mut self) -> Result<()> {
        let version: i64 = self
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))?;
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
        // DDL and the version stamp move together, so an interrupted migration
        // leaves a version-0 file that the next run can retry from scratch.
        let tx = self.conn.transaction()?;
        tx.execute_batch(MIGRATION_1)?;
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
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

    /// Inserts `row`, or replaces the existing row with the same key.
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

    /// Page ids generated from the raw markdown `raw_key`, ordered by page id.
    pub fn pages_for_source(&self, raw_key: &str) -> Result<Vec<String>> {
        let mut stmt = self
            .conn
            .prepare("SELECT page_id FROM source_pages WHERE raw_key = ?1 ORDER BY page_id")?;
        let rows = stmt.query_map([raw_key], |row| row.get(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Replaces the page ids recorded for `raw_key`.
    ///
    /// Re-ingesting a raw source can produce a different page set, so this
    /// writes the whole set instead of merging: pages that are gone from the new
    /// set are removed from the mapping (the pages themselves are handled by the
    /// ingest transaction, not here).
    pub fn set_source_pages(&mut self, raw_key: &str, pages: &[String]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM source_pages WHERE raw_key = ?1", [raw_key])?;
        {
            let mut insert =
                tx.prepare("INSERT INTO source_pages(raw_key, page_id) VALUES (?1, ?2)")?;
            for page in pages {
                insert.execute(params![raw_key, page])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Whether the linked SQLite was built with the FTS5 extension.
    ///
    /// The full-text index in the schema is useless without it, and the bundled
    /// build is expected to carry it, so a false here is a hard failure for the
    /// query commands rather than something to work around.
    pub fn fts5_available(&self) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT count(*) FROM pragma_compile_options \
             WHERE compile_options = 'ENABLE_FTS5'",
            [],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }
}
