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
const SCHEMA_VERSION: i64 = 3;

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

/// Column order shared by the `sources` reader helpers.
const SOURCE_COLUMNS: &str = "key, hash, size, mtime, raw_key, converted_at";

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
        Ok(())
    }

    /// Reads `PRAGMA user_version`, the schema generation of the file.
    fn user_version(&self) -> Result<i64> {
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
        conn.execute(
            "INSERT INTO source_pages(raw_key, page_id) VALUES ('raw/a.md', 'page/a')",
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

    #[test]
    fn a_v1_database_is_upgraded_in_place() {
        let d = tempdir().unwrap();
        let p = d.path().join("state.db");
        write_v1_database(&p);

        let mut s = Store::open(&p).unwrap();
        assert_eq!(user_version(&s), 1);
        s.migrate().unwrap();
        assert_eq!(user_version(&s), SCHEMA_VERSION);

        // The v1 tables and their rows survive: nothing is dropped or rebuilt.
        let row = s.get_source("a.pdf").unwrap().unwrap();
        assert_eq!(row.hash, "blake3:aa");
        assert_eq!(row.raw_key.as_deref(), Some("raw/a.md"));
        assert_eq!(row.converted_at.as_deref(), Some("2026-10-27T10:00:00Z"));
        assert_eq!(s.pages_for_source("raw/a.md").unwrap(), ["page/a"]);

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
        assert_eq!(SCHEMA_VERSION, 3);

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
