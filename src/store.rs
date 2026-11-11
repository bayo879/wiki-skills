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
pub const SCHEMA_VERSION: i64 = 12;

/// The routing task: `route` becomes a task kind of its own.
///
/// Every root owns at most one `route` child, which is the only entry point for
/// assigning a source to pages; the partial unique index makes a second one
/// impossible even if a caller tries. The table is rebuilt because SQLite
/// cannot alter a CHECK constraint.
const MIGRATION_12: &str = r#"
ALTER TABLE tasks RENAME TO tasks_v11;
CREATE TABLE tasks(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_id INTEGER REFERENCES tasks(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('convert','breakdown','page','route','reconcile')),
    status TEXT NOT NULL CHECK(status IN ('pending','active','failed','blocked')),
    source_key TEXT,
    raw_hash TEXT,
    target_path TEXT,
    allowed_json TEXT,
    payload_json TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
INSERT INTO tasks(id,parent_id,kind,status,source_key,raw_hash,target_path,allowed_json,payload_json,attempts,last_error,created_at,updated_at)
    SELECT id,parent_id,kind,status,source_key,raw_hash,target_path,allowed_json,payload_json,attempts,last_error,created_at,updated_at FROM tasks_v11;
DROP TABLE tasks_v11;
CREATE UNIQUE INDEX tasks_one_root_per_source ON tasks(source_key) WHERE kind='breakdown';
CREATE UNIQUE INDEX tasks_one_convert_per_source ON tasks(source_key) WHERE kind='convert';
CREATE UNIQUE INDEX tasks_one_route_per_root ON tasks(parent_id) WHERE kind='route';
"#;

/// The relation index: who points at whom, and which pages cite which raw.
///
/// Every row here is derived from page frontmatter and bodies; it is rebuilt by
/// `query::refresh` and `doctor --fix` and never edited by hand. `page_links`
/// keeps unresolved literals as rows with a NULL `to_page`, so a dent in the
/// wiki is data, not a missing row. `relation_meta.generation` counts changes so
/// a reader can tell whether the snapshot it saw is still current.
const MIGRATION_11: &str = r#"
CREATE TABLE relation_meta(
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
INSERT INTO relation_meta(key, value) VALUES ('generation', '1');
CREATE TABLE page_nodes(
    page_key TEXT PRIMARY KEY,
    page_id TEXT NOT NULL,
    page_type TEXT NOT NULL,
    title TEXT NOT NULL,
    summary TEXT NOT NULL,
    status TEXT NOT NULL,
    content_hash TEXT NOT NULL
);
CREATE INDEX page_nodes_page_id ON page_nodes(page_id);
CREATE TABLE page_links(
    from_page TEXT NOT NULL,
    target_literal TEXT NOT NULL,
    to_page TEXT,
    resolution TEXT NOT NULL CHECK(resolution IN ('resolved','missing','ambiguous')),
    PRIMARY KEY(from_page, target_literal)
);
CREATE INDEX page_links_to_page ON page_links(to_page);
CREATE TABLE page_source_refs(
    page_key TEXT NOT NULL,
    raw_ref TEXT NOT NULL,
    raw_key TEXT NOT NULL,
    fragment TEXT,
    PRIMARY KEY(page_key, raw_ref)
);
CREATE INDEX page_source_refs_raw_key ON page_source_refs(raw_key);
"#;

/// The task tree: what the current ingest round still owes, and the page
/// hashes the batch started from.
///
/// A row exists exactly while its task is unfinished; closing a task deletes
/// it. `task_baseline` is one shared snapshot of `pages/` (`root_id = 0`),
/// advanced after every merge: one machine and one author means one "what was
/// here" per repository, not one per root.
/// `reconcile` joins the task kinds: an authored page changed outside the task
/// tree, and someone has to decide whether the change stands or the source is
/// regenerated. SQLite cannot alter a CHECK constraint, so the table is rebuilt.
const MIGRATION_10: &str = r#"
ALTER TABLE tasks RENAME TO tasks_v9;
CREATE TABLE tasks(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_id INTEGER REFERENCES tasks(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('convert','breakdown','page','reconcile')),
    status TEXT NOT NULL CHECK(status IN ('pending','active','failed','blocked')),
    source_key TEXT,
    raw_hash TEXT,
    target_path TEXT,
    allowed_json TEXT,
    payload_json TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
INSERT INTO tasks(id,parent_id,kind,status,source_key,raw_hash,target_path,allowed_json,payload_json,attempts,last_error,created_at,updated_at)
    SELECT id,parent_id,kind,status,source_key,raw_hash,target_path,allowed_json,payload_json,attempts,last_error,created_at,updated_at FROM tasks_v9;
DROP TABLE tasks_v9;
CREATE UNIQUE INDEX tasks_one_root_per_source ON tasks(source_key) WHERE kind='breakdown';
CREATE UNIQUE INDEX tasks_one_convert_per_source ON tasks(source_key) WHERE kind='convert';
"#;

const MIGRATION_9: &str = r#"
CREATE TABLE tasks(
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    parent_id INTEGER REFERENCES tasks(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK(kind IN ('convert','breakdown','page')),
    status TEXT NOT NULL CHECK(status IN ('pending','active','failed','blocked')),
    source_key TEXT,
    raw_hash TEXT,
    target_path TEXT,
    allowed_json TEXT,
    payload_json TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE UNIQUE INDEX tasks_one_root_per_source ON tasks(source_key) WHERE kind='breakdown';
CREATE UNIQUE INDEX tasks_one_convert_per_source ON tasks(source_key) WHERE kind='convert';
CREATE TABLE task_baseline(
    root_id INTEGER NOT NULL,
    path TEXT NOT NULL,
    hash TEXT NOT NULL,
    PRIMARY KEY(root_id,path)
);
"#;

/// The failure note for a source that has never converted.
///
/// `sources` and `work_items` only get a row once a conversion succeeds, so a
/// file that keeps failing leaves nothing behind to remember it by. This table
/// is that memory: it exists only so the next round can put a known-bad file at
/// the back of the queue instead of letting it hold the budget forever.
const MIGRATION_8: &str = r#"
CREATE TABLE source_attempts(
    key TEXT PRIMARY KEY,
    attempted_at TEXT NOT NULL,
    error TEXT NOT NULL
);
"#;

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

/// One row of the task tree.
///
/// `kind` is `convert` (source → raw markdown, internal), `breakdown` (one root
/// per source, the settlement container), `route` (the root's single source
/// assignment step) or `page` (one leaf, one target page). A row lives only
/// while the task is unfinished; closing deletes it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct TaskRow {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub kind: String,
    pub status: String,
    pub source_key: Option<String>,
    pub raw_hash: Option<String>,
    pub target_path: Option<String>,
    pub allowed_json: Option<String>,
    pub payload_json: Option<String>,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

/// A task about to be inserted. Timestamps are passed in by the caller.
#[derive(Debug, Clone, Default)]
pub struct NewTask {
    pub parent_id: Option<i64>,
    pub kind: String,
    pub status: String,
    pub source_key: Option<String>,
    pub raw_hash: Option<String>,
    pub target_path: Option<String>,
    pub allowed_json: Option<String>,
    pub payload_json: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl NewTask {
    /// A pending task of `kind` stamped at `now` (RFC 3339).
    pub fn pending(kind: &str, now: &str) -> NewTask {
        NewTask {
            kind: String::from(kind),
            status: String::from("pending"),
            created_at: String::from(now),
            updated_at: String::from(now),
            ..NewTask::default()
        }
    }
}

impl TaskRow {
    /// Reads one row in `tasks` column order.
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskRow> {
        Ok(TaskRow {
            id: row.get(0)?,
            parent_id: row.get(1)?,
            kind: row.get(2)?,
            status: row.get(3)?,
            source_key: row.get(4)?,
            raw_hash: row.get(5)?,
            target_path: row.get(6)?,
            allowed_json: row.get(7)?,
            payload_json: row.get(8)?,
            attempts: row.get(9)?,
            last_error: row.get(10)?,
            created_at: row.get(11)?,
            updated_at: row.get(12)?,
        })
    }
}

/// The last failed conversion of a source, kept only to order the queue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub key: String,
    /// RFC 3339, from the round that failed.
    pub attempted_at: String,
    /// The error code and message a human would have seen.
    pub error: String,
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

/// One indexed page: the facts a relation answer needs without rereading it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationNode {
    pub page_key: String,
    pub page_id: String,
    pub page_type: String,
    pub title: String,
    pub summary: String,
    pub status: String,
    /// `page_meta.hash` at the time this row was written.
    pub content_hash: String,
}

/// One outgoing link, resolved or not.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationLink {
    pub from_page: String,
    pub target_literal: String,
    pub to_page: Option<String>,
    /// `resolved`, `missing` or `ambiguous`; see [`crate::relation::Resolution`].
    pub resolution: String,
}

/// One `sources:` entry, split into a raw file and an optional fragment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationSource {
    pub page_key: String,
    pub raw_ref: String,
    pub raw_key: String,
    pub fragment: Option<String>,
}

/// Column order shared by the `page_nodes` reader helpers.
const RELATION_NODE_COLUMNS: &str =
    "page_key, page_id, page_type, title, summary, status, content_hash";

impl RelationNode {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RelationNode> {
        Ok(RelationNode {
            page_key: row.get(0)?,
            page_id: row.get(1)?,
            page_type: row.get(2)?,
            title: row.get(3)?,
            summary: row.get(4)?,
            status: row.get(5)?,
            content_hash: row.get(6)?,
        })
    }
}

impl RelationLink {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RelationLink> {
        Ok(RelationLink {
            from_page: row.get(0)?,
            target_literal: row.get(1)?,
            to_page: row.get(2)?,
            resolution: row.get(3)?,
        })
    }
}

/// Everything one `query::refresh` wants to change, committed as one batch.
#[derive(Debug, Default)]
pub struct RefreshWrite {
    pub fts_deletes: Vec<String>,
    pub fts_upserts: Vec<(String, String, String)>,
    pub meta_deletes: Vec<String>,
    pub metas: Vec<PageMetaRow>,
    /// Pages that lost their relation rows (deleted or unreadable).
    pub relation_deletes: Vec<String>,
    /// Pages whose relation rows are replaced in this batch.
    pub relation_pages: Vec<PageRelations>,
    /// Replaces the whole relation index when the page set or an alias changed.
    pub full_relations: Option<Vec<PageRelations>>,
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

/// One page's relation rows: the node, its outgoing links and its sources.
pub type PageRelations = (RelationNode, Vec<RelationLink>, Vec<RelationSource>);

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
        // One file, two processes: `ingest` converts while a `query` refreshes
        // the index, and WAL only lets readers through. A writer that finds the
        // write lock taken would otherwise fail at once with `SQLITE_BUSY`,
        // which reads as a broken database. Waiting a few seconds lets the short
        // index writes queue; a long conversion still ends as a clean error.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
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
        if version < 8 {
            self.apply_migration(MIGRATION_8, 8)?;
        }
        if version < 9 {
            self.apply_migration(MIGRATION_9, 9)?;
        }
        if version < 10 {
            self.apply_migration(MIGRATION_10, 10)?;
        }
        if version < 11 {
            self.apply_migration(MIGRATION_11, 11)?;
        }
        if version < 12 {
            self.apply_migration(MIGRATION_12, 12)?;
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

    /// Remembers that `key` failed to convert at `attempted_at`.
    ///
    /// This is bookkeeping for the queue order, not a status: a source is only
    /// ever "pending" or "converted". Overwriting the previous note is the
    /// point — every failure pushes the file further back.
    pub(crate) fn record_attempt(
        &mut self,
        key: &str,
        attempted_at: &str,
        error: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO source_attempts(key,attempted_at,error) VALUES(?1,?2,?3) ON CONFLICT(key) DO UPDATE SET attempted_at=excluded.attempted_at,error=excluded.error",
            params![key, attempted_at, error],
        )?;
        Ok(())
    }

    /// Forgets a failure note. A source that disappears before it ever
    /// converted has no `sources` row to delete the note through.
    pub(crate) fn delete_attempt(&mut self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM source_attempts WHERE key=?1", [key])?;
        Ok(())
    }

    /// Every source that failed to convert, in key order.
    pub fn attempts(&self) -> Result<Vec<Attempt>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key,attempted_at,error FROM source_attempts ORDER BY key")?;
        let rows = stmt.query_map([], |row| {
            Ok(Attempt {
                key: row.get(0)?,
                attempted_at: row.get(1)?,
                error: row.get(2)?,
            })
        })?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
    }

    /// Records one conversion failure as a `convert` task row.
    ///
    /// The row is the durable failure counter: `attempts` rises by one per
    /// failure and the third failure parks it in `failed`. Returns the task id
    /// and whether this call parked it, so the round can say so out loud.
    pub(crate) fn record_convert_failure(
        &mut self,
        key: &str,
        error: &str,
        now: &str,
    ) -> Result<(i64, bool)> {
        self.conn.execute(
            "INSERT INTO tasks(parent_id,kind,status,source_key,attempts,last_error,created_at,updated_at) \
             VALUES(NULL,'convert','pending',?1,1,?2,?3,?3) \
             ON CONFLICT(source_key) WHERE kind='convert' DO UPDATE SET \
               attempts=attempts+1, \
               last_error=?2, \
               status=CASE WHEN attempts+1>=3 THEN 'failed' ELSE 'pending' END, \
               updated_at=?3",
            params![key, error, now],
        )?;
        let id: i64 = self.conn.query_row(
            "SELECT id FROM tasks WHERE kind='convert' AND source_key=?1",
            [key],
            |row| row.get(0),
        )?;
        let attempts: i64 =
            self.conn
                .query_row("SELECT attempts FROM tasks WHERE id=?1", [id], |row| {
                    row.get(0)
                })?;
        Ok((id, attempts >= 3))
    }

    /// Removes the `convert` row for a source that produced a raw file again.
    pub(crate) fn clear_convert_failure(&mut self, key: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM tasks WHERE kind='convert' AND source_key=?1",
            [key],
        )?;
        Ok(())
    }

    /// Sources parked in `failed` by three consecutive conversion failures.
    pub fn failed_converts(&self) -> Result<Vec<TaskRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,parent_id,kind,status,source_key,raw_hash,target_path,allowed_json,payload_json,attempts,last_error,created_at,updated_at \
             FROM tasks WHERE kind='convert' AND status='failed' ORDER BY source_key",
        )?;
        let rows = stmt.query_map([], TaskRow::from_row)?;
        let mut items = Vec::new();
        for row in rows {
            items.push(row?);
        }
        Ok(items)
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

    /// Inserts one task and returns its id.
    pub(crate) fn insert_task(&mut self, t: &NewTask) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let id = Self::insert_task_row(&tx, t)?;
        tx.commit()?;
        Ok(id)
    }

    /// Inserts one task inside an existing transaction and returns its id.
    fn insert_task_row(tx: &rusqlite::Transaction<'_>, t: &NewTask) -> Result<i64> {
        tx.execute(
            "INSERT INTO tasks(parent_id,kind,status,source_key,raw_hash,target_path,allowed_json,payload_json,created_at,updated_at) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            params![
                t.parent_id,
                t.kind,
                t.status,
                t.source_key,
                t.raw_hash,
                t.target_path,
                t.allowed_json,
                t.payload_json,
                t.created_at,
                t.updated_at
            ],
        )?;
        Ok(tx.last_insert_rowid())
    }

    /// Creates a root and its single `route` child in one transaction.
    ///
    /// The route always hangs off the new root; callers do not have to fill in
    /// `parent_id`. Either both rows exist or neither does, so a round never
    /// finds a root with nothing to deliver.
    pub(crate) fn create_root_with_route(
        &mut self,
        root: &NewTask,
        route: &NewTask,
    ) -> Result<(i64, i64)> {
        let tx = self.conn.transaction()?;
        let root_id = Self::insert_task_row(&tx, root)?;
        let route = NewTask {
            parent_id: Some(root_id),
            ..route.clone()
        };
        let route_id = Self::insert_task_row(&tx, &route)?;
        tx.commit()?;
        Ok((root_id, route_id))
    }

    /// Closes a `route`: saves the root plan, creates its pages, deletes the route.
    ///
    /// One transaction for all three steps, so a page row the schema rejects
    /// leaves the route in place and the payload unconsumed. Page tasks are
    /// parented to `root_id` regardless of what the caller passed.
    pub(crate) fn commit_route(
        &mut self,
        root_id: i64,
        root_payload: &str,
        pages: &[NewTask],
        route_id: i64,
        now: &str,
    ) -> Result<Vec<i64>> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE tasks SET payload_json=?1,updated_at=?2 WHERE id=?3",
            params![root_payload, now, root_id],
        )?;
        let mut page_ids = Vec::with_capacity(pages.len());
        for page in pages {
            let page = NewTask {
                parent_id: Some(root_id),
                ..page.clone()
            };
            page_ids.push(Self::insert_task_row(&tx, &page)?);
        }
        tx.execute("DELETE FROM tasks WHERE id=?1", [route_id])?;
        tx.commit()?;
        Ok(page_ids)
    }

    /// Closes a `page`: saves the root plan and deletes the page in one transaction.
    pub(crate) fn commit_page(
        &mut self,
        root_id: i64,
        page_id: i64,
        root_payload: &str,
        now: &str,
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE tasks SET payload_json=?1,updated_at=?2 WHERE id=?3",
            params![root_payload, now, root_id],
        )?;
        tx.execute("DELETE FROM tasks WHERE id=?1", [page_id])?;
        tx.commit()?;
        Ok(())
    }

    /// Rebuilds a root's plan: saves it, drops every child, creates a fresh route.
    ///
    /// Written pages stay where they are; only the task tree is rebuilt, which
    /// is why the page rows and their staging are not touched here.
    pub(crate) fn replan_root(
        &mut self,
        root_id: i64,
        root_payload: &str,
        route: &NewTask,
        now: &str,
    ) -> Result<i64> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE tasks SET payload_json=?1,updated_at=?2 WHERE id=?3",
            params![root_payload, now, root_id],
        )?;
        tx.execute("DELETE FROM tasks WHERE parent_id=?1", [root_id])?;
        let route = NewTask {
            parent_id: Some(root_id),
            ..route.clone()
        };
        let route_id = Self::insert_task_row(&tx, &route)?;
        tx.commit()?;
        Ok(route_id)
    }

    /// Parks a task as `blocked`, counts the attempt and records why.
    pub(crate) fn block_task(&mut self, id: i64, error: &str, now: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE tasks SET status='blocked',attempts=attempts+1,last_error=?1,updated_at=?2 WHERE id=?3",
            params![error, now, id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /// Every task, ordered by id (roots and leaves interleaved in creation order).
    pub fn tasks(&self) -> Result<Vec<TaskRow>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,parent_id,kind,status,source_key,raw_hash,target_path,allowed_json,payload_json,attempts,last_error,created_at,updated_at FROM tasks ORDER BY id",
        )?;
        let rows = stmt.query_map([], TaskRow::from_row)?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// One task by id, or `None` when it is already closed.
    pub fn task(&self, id: i64) -> Result<Option<TaskRow>> {
        let row = self
            .conn
            .query_row(
                "SELECT id,parent_id,kind,status,source_key,raw_hash,target_path,allowed_json,payload_json,attempts,last_error,created_at,updated_at FROM tasks WHERE id=?1",
                [id],
                TaskRow::from_row,
            )
            .optional()?;
        Ok(row)
    }

    /// Moves a task to `status` and stamps it.
    pub(crate) fn set_task_status(&mut self, id: i64, status: &str, now: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET status=?1,updated_at=?2 WHERE id=?3",
            params![status, now, id],
        )?;
        Ok(())
    }

    /// Re-anchors a root to the source's current raw digest and counts a replan.
    pub(crate) fn update_task_source(
        &mut self,
        id: i64,
        raw_hash: Option<&str>,
        attempts: i64,
        now: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET raw_hash=?1,attempts=?2,updated_at=?3 WHERE id=?4",
            params![raw_hash, attempts, now, id],
        )?;
        Ok(())
    }

    /// Stores the payload a leaf was closed with, and the page set it was allowed.
    pub(crate) fn set_task_payload(
        &mut self,
        id: i64,
        payload_json: &str,
        allowed_json: Option<&str>,
        now: &str,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET payload_json=?1,allowed_json=COALESCE(?2,allowed_json),updated_at=?3 WHERE id=?4",
            params![payload_json, allowed_json, now, id],
        )?;
        Ok(())
    }

    /// Counts one failure and parks the task as `failed`.
    pub(crate) fn fail_task(&mut self, id: i64, error: &str, now: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET attempts=attempts+1,status='failed',last_error=?1,updated_at=?2 WHERE id=?3",
            params![error, now, id],
        )?;
        Ok(())
    }

    /// Clears a failure and returns the task to `pending`.
    pub(crate) fn retry_task(&mut self, id: i64, now: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET status='pending',attempts=0,last_error=NULL,updated_at=?1 WHERE id=?2",
            params![now, id],
        )?;
        Ok(())
    }

    /// Deletes one task. Children and baseline rows cascade with it.
    pub(crate) fn delete_task(&mut self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM tasks WHERE id=?1", [id])?;
        Ok(())
    }

    /// The page hashes a root started from, ordered by path.
    pub fn baseline(&self, root_id: i64) -> Result<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT path,hash FROM task_baseline WHERE root_id=?1 ORDER BY path")?;
        let rows = stmt.query_map([root_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Drops one baseline wholesale, so it can be re-snapshotted.
    pub(crate) fn clear_baseline(&mut self, root_id: i64) -> Result<()> {
        self.conn
            .execute("DELETE FROM task_baseline WHERE root_id=?1", [root_id])?;
        Ok(())
    }

    /// Drops one page from a root's baseline, after the page went away and the
    /// removal is accepted as a real edit rather than a task's business.
    pub(crate) fn delete_baseline_path(&mut self, root_id: i64, path: &str) -> Result<()> {
        self.conn.execute(
            "DELETE FROM task_baseline WHERE root_id=?1 AND path=?2",
            params![root_id, path],
        )?;
        Ok(())
    }

    /// Records one page's hash in a root's baseline.
    pub(crate) fn set_baseline(&mut self, root_id: i64, path: &str, hash: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO task_baseline(root_id,path,hash) VALUES(?1,?2,?3) ON CONFLICT(root_id,path) DO UPDATE SET hash=excluded.hash",
            params![root_id, path, hash],
        )?;
        Ok(())
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
        tx.execute("DELETE FROM source_attempts WHERE key=?1", [&source.key])?;
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

    /// Removes everything the ledger knows about the source `key`: its row, the
    /// pending work item it left behind, and the cached file facts. Deleting an
    /// unknown key is not an error.
    ///
    /// The three rows are one fact about one source, so they go together: a
    /// source forgotten while its work item stayed behind would resurrect a
    /// pending task for a file that no longer exists.
    pub fn delete_source(&mut self, key: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM sources WHERE key = ?1", [key])?;
        tx.execute("DELETE FROM work_items WHERE source_key = ?1", [key])?;
        tx.execute("DELETE FROM source_facts WHERE key = ?1", [key])?;
        tx.execute("DELETE FROM source_attempts WHERE key = ?1", [key])?;
        tx.commit()?;
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

    /// Replaces everything the index knows about one page.
    ///
    /// The node, its outgoing links and its source references are written in one
    /// transaction, and `relation_meta.generation` advances only when the rows
    /// actually changed. Dropping a link row is how a removed `[[...]]`
    /// disappears; [`Store::delete_page_relations`] handles the page itself.
    pub fn replace_page_relations(
        &mut self,
        node: &RelationNode,
        links: &[RelationLink],
        sources: &[RelationSource],
    ) -> Result<()> {
        let tx = self.conn.transaction()?;
        if Self::write_page_relations(&tx, node, links, sources)? {
            Self::bump_generation_tx(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Removes one page from the index and leaves its inbound links unresolved.
    ///
    /// The links are not deleted: they belong to other pages, which still claim
    /// the relation. They become `missing`, which is what the reader needs to
    /// see, and the page no longer answers an incoming query.
    pub fn delete_page_relations(&mut self, page_key: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        if Self::delete_page_relations_tx(&tx, page_key)? {
            Self::bump_generation_tx(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Writes one refresh's FTS, `page_meta` and relation changes together.
    ///
    /// `refresh` collects everything first and commits once, so a failure leaves
    /// the last good index in place instead of a half-updated one. A full
    /// relation rebuild replaces all three tables; otherwise only the named
    /// pages are rewritten.
    pub fn apply_refresh(&mut self, write: &RefreshWrite) -> Result<()> {
        let tx = self.conn.transaction()?;
        for page_id in &write.fts_deletes {
            tx.execute("DELETE FROM fts WHERE page_id = ?1", [page_id])?;
        }
        Self::fts_insert(&tx, &write.fts_upserts)?;
        for page_key in &write.meta_deletes {
            tx.execute("DELETE FROM page_meta WHERE page_key = ?1", [page_key])?;
        }
        for meta in &write.metas {
            tx.execute(
                "INSERT INTO page_meta(page_key, hash, updated) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(page_key) DO UPDATE SET \
                     hash = excluded.hash, updated = excluded.updated",
                params![meta.page_key, meta.hash, meta.updated],
            )?;
        }

        let mut relation_changed = false;
        match &write.full_relations {
            Some(pages) => {
                tx.execute("DELETE FROM page_links", [])?;
                tx.execute("DELETE FROM page_source_refs", [])?;
                tx.execute("DELETE FROM page_nodes", [])?;
                for (node, links, sources) in pages {
                    Self::write_page_relations(&tx, node, links, sources)?;
                }
                relation_changed = true;
            }
            None => {
                for page_key in &write.relation_deletes {
                    relation_changed |= Self::delete_page_relations_tx(&tx, page_key)?;
                }
                for (node, links, sources) in &write.relation_pages {
                    relation_changed |= Self::write_page_relations(&tx, node, links, sources)?;
                }
            }
        }
        if relation_changed {
            Self::bump_generation_tx(&tx)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Every indexed page, ordered by key.
    pub fn all_relation_nodes(&self) -> Result<Vec<RelationNode>> {
        let sql = format!("SELECT {RELATION_NODE_COLUMNS} FROM page_nodes ORDER BY page_key");
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map([], RelationNode::from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    /// Outgoing links of one page, ordered by literal.
    pub fn relation_links_from(&self, page_key: &str) -> Result<Vec<RelationLink>> {
        Self::relation_links_from_tx(&self.conn, page_key)
    }

    /// Incoming links of one page, ordered by source page.
    pub fn relation_links_to(&self, page_key: &str) -> Result<Vec<RelationLink>> {
        Self::relation_links_to_tx(&self.conn, page_key)
    }

    /// Source references of one page, ordered by raw reference.
    pub fn relation_sources(&self, page_key: &str) -> Result<Vec<RelationSource>> {
        Self::relation_sources_tx(&self.conn, page_key)
    }

    /// Pages citing `raw_key`, sorted and deduplicated.
    pub fn pages_citing(&self, raw_key: &str) -> Result<Vec<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT DISTINCT page_key FROM page_source_refs WHERE raw_key = ?1 \
             ORDER BY page_key",
        )?;
        let rows = stmt.query_map([raw_key], |row| row.get(0))?;
        Ok(rows.collect::<rusqlite::Result<Vec<String>>>()?)
    }

    /// The current relation index generation.
    pub fn relation_generation(&self) -> Result<i64> {
        let value: String = self.conn.query_row(
            "SELECT value FROM relation_meta WHERE key = 'generation'",
            [],
            |row| row.get(0),
        )?;
        Ok(value.parse().unwrap_or(0))
    }

    /// Advances the relation generation and returns the new value.
    pub fn bump_relation_generation(&mut self) -> Result<i64> {
        let tx = self.conn.transaction()?;
        let generation = Self::bump_generation_tx(&tx)?;
        tx.commit()?;
        Ok(generation)
    }

    /// Writes one page's rows; returns whether anything differed.
    fn write_page_relations(
        tx: &rusqlite::Transaction<'_>,
        node: &RelationNode,
        links: &[RelationLink],
        sources: &[RelationSource],
    ) -> Result<bool> {
        let mut links = links.to_vec();
        links.sort_by(|a, b| a.target_literal.cmp(&b.target_literal));
        links.dedup_by(|a, b| a.target_literal == b.target_literal);
        let mut sources = sources.to_vec();
        sources.sort_by(|a, b| a.raw_ref.cmp(&b.raw_ref));
        sources.dedup_by(|a, b| a.raw_ref == b.raw_ref);

        let old_node = Self::relation_node(tx, &node.page_key)?;
        let old_links = Self::relation_links_from_tx(tx, &node.page_key)?;
        let old_sources = Self::relation_sources_tx(tx, &node.page_key)?;
        if old_node.as_ref() == Some(node) && old_links == links && old_sources == sources {
            return Ok(false);
        }

        tx.execute(
            "DELETE FROM page_links WHERE from_page = ?1",
            [&node.page_key],
        )?;
        tx.execute(
            "DELETE FROM page_source_refs WHERE page_key = ?1",
            [&node.page_key],
        )?;
        tx.execute(
            "INSERT INTO page_nodes(page_key,page_id,page_type,title,summary,status,content_hash) \
             VALUES(?1,?2,?3,?4,?5,?6,?7) \
             ON CONFLICT(page_key) DO UPDATE SET page_id=excluded.page_id, \
             page_type=excluded.page_type, title=excluded.title, summary=excluded.summary, \
             status=excluded.status, content_hash=excluded.content_hash",
            params![
                node.page_key,
                node.page_id,
                node.page_type,
                node.title,
                node.summary,
                node.status,
                node.content_hash
            ],
        )?;
        for link in &links {
            tx.execute(
                "INSERT INTO page_links(from_page,target_literal,to_page,resolution) \
                 VALUES(?1,?2,?3,?4)",
                params![
                    link.from_page,
                    link.target_literal,
                    link.to_page,
                    link.resolution
                ],
            )?;
        }
        for source in &sources {
            tx.execute(
                "INSERT INTO page_source_refs(page_key,raw_ref,raw_key,fragment) \
                 VALUES(?1,?2,?3,?4)",
                params![
                    source.page_key,
                    source.raw_ref,
                    source.raw_key,
                    source.fragment
                ],
            )?;
        }
        Ok(true)
    }

    fn relation_node(conn: &rusqlite::Connection, page_key: &str) -> Result<Option<RelationNode>> {
        let sql = format!("SELECT {RELATION_NODE_COLUMNS} FROM page_nodes WHERE page_key = ?1");
        Ok(conn
            .query_row(&sql, [page_key], RelationNode::from_row)
            .optional()?)
    }

    fn relation_links_from_tx(
        conn: &rusqlite::Connection,
        page_key: &str,
    ) -> Result<Vec<RelationLink>> {
        let mut stmt = conn.prepare(
            "SELECT from_page,target_literal,to_page,resolution FROM page_links \
             WHERE from_page = ?1 ORDER BY target_literal",
        )?;
        let rows = stmt.query_map([page_key], RelationLink::from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn relation_links_to_tx(
        conn: &rusqlite::Connection,
        page_key: &str,
    ) -> Result<Vec<RelationLink>> {
        let mut stmt = conn.prepare(
            "SELECT from_page,target_literal,to_page,resolution FROM page_links \
             WHERE to_page = ?1 ORDER BY from_page, target_literal",
        )?;
        let rows = stmt.query_map([page_key], RelationLink::from_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn relation_sources_tx(
        conn: &rusqlite::Connection,
        page_key: &str,
    ) -> Result<Vec<RelationSource>> {
        let mut stmt = conn.prepare(
            "SELECT page_key,raw_ref,raw_key,fragment FROM page_source_refs \
             WHERE page_key = ?1 ORDER BY raw_ref",
        )?;
        let rows = stmt.query_map([page_key], |row| {
            Ok(RelationSource {
                page_key: row.get(0)?,
                raw_ref: row.get(1)?,
                raw_key: row.get(2)?,
                fragment: row.get(3)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn delete_page_relations_tx(tx: &rusqlite::Transaction<'_>, page_key: &str) -> Result<bool> {
        let node = Self::relation_node(tx, page_key)?;
        let links = Self::relation_links_from_tx(tx, page_key)?;
        let sources = Self::relation_sources_tx(tx, page_key)?;
        tx.execute("DELETE FROM page_links WHERE from_page = ?1", [page_key])?;
        tx.execute(
            "DELETE FROM page_source_refs WHERE page_key = ?1",
            [page_key],
        )?;
        tx.execute("DELETE FROM page_nodes WHERE page_key = ?1", [page_key])?;
        let inbound = tx.execute(
            "UPDATE page_links SET to_page = NULL, resolution = 'missing' WHERE to_page = ?1",
            [page_key],
        )?;
        Ok(node.is_some() || !links.is_empty() || !sources.is_empty() || inbound > 0)
    }

    fn bump_generation_tx(tx: &rusqlite::Transaction<'_>) -> Result<i64> {
        tx.execute(
            "UPDATE relation_meta SET value = CAST(value AS INTEGER) + 1 \
             WHERE key = 'generation'",
            [],
        )?;
        let value: String = tx.query_row(
            "SELECT value FROM relation_meta WHERE key = 'generation'",
            [],
            |row| row.get(0),
        )?;
        Ok(value.parse().unwrap_or(0))
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

    #[test]
    fn a_failure_note_lives_until_the_source_converts_or_goes_away() {
        let d = tempdir().unwrap();
        let mut s = Store::open(&d.path().join("state.db")).unwrap();
        s.migrate().unwrap();

        s.record_attempt("a.pdf", "2026-11-09T10:00:00Z", "E_PARSE: broken")
            .unwrap();
        assert_eq!(s.attempts().unwrap().len(), 1);

        // Success ends the note: the source is now on the queue, not at the
        // back of it.
        let source = SourceRow {
            key: String::from("a.pdf"),
            hash: String::from("blake3:aa"),
            size: 10,
            mtime: 1,
            raw_key: Some(String::from("raw/a.md")),
            converted_at: Some(String::from("2026-11-10T10:00:00Z")),
        };
        s.register_conversion(&source, &raw_row("raw/a.md", "blake3:bb"), None)
            .unwrap();
        assert!(
            s.attempts().unwrap().is_empty(),
            "a converted source must not stay in the failure order"
        );

        // A note for a source that disappears goes with it.
        s.record_attempt("b.pdf", "2026-11-09T10:00:00Z", "E_PARSE: broken")
            .unwrap();
        s.delete_source("b.pdf").unwrap();
        assert!(s.attempts().unwrap().is_empty());
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
    fn schema_12_allows_one_route_per_root() {
        let d = tempdir().unwrap();
        let mut s = Store::open(&d.path().join("state.db")).unwrap();
        s.migrate().unwrap();
        let now = "2026-11-11T00:00:00Z";
        let mut root = NewTask::pending("breakdown", now);
        root.source_key = Some("a.md".into());
        let mut route = NewTask::pending("route", now);
        route.source_key = Some("a.md".into());
        let (_, route_id) = s.create_root_with_route(&root, &route).unwrap();

        let mut second = NewTask::pending("route", now);
        second.parent_id = s.task(route_id).unwrap().unwrap().parent_id;
        second.source_key = Some("a.md".into());
        assert!(s.insert_task(&second).is_err());
    }

    #[test]
    fn commit_route_rolls_back_when_a_page_row_is_invalid() {
        let d = tempdir().unwrap();
        let mut s = Store::open(&d.path().join("state.db")).unwrap();
        s.migrate().unwrap();
        let now = "2026-11-11T00:00:00Z";
        let mut root = NewTask::pending("breakdown", now);
        root.source_key = Some("a.md".into());
        let mut route = NewTask::pending("route", now);
        route.source_key = Some("a.md".into());
        let (root_id, route_id) = s.create_root_with_route(&root, &route).unwrap();

        let mut page = NewTask::pending("page", now);
        page.status = "not-a-status".into();
        assert!(s
            .commit_route(
                root_id,
                "{\"schema\":\"root-plan-v1\"}",
                &[page],
                route_id,
                now
            )
            .is_err());
        assert!(
            s.task(route_id).unwrap().is_some(),
            "the route must survive a failed commit"
        );
        assert_eq!(
            s.task(root_id).unwrap().unwrap().payload_json,
            None,
            "the root payload must roll back with the failed commit"
        );
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
