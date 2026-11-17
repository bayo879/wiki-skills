"""Ledger schema for llm-wiki.

The table contract lives here so `wiki.py` no longer grows a second DDL site
whenever a column or table is added. `Wiki.ensure` opens the connection and
calls into this module; command code never builds its own schema.

There is no migration on purpose: a ledger that does not match this version is
refused with `E_STATE`, and the workspace is rebuilt from scratch.
"""

from __future__ import annotations

import sqlite3
from datetime import datetime, timezone

from errors import WikiError

SCHEMA_SQL = """
CREATE TABLE IF NOT EXISTS raws (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    path TEXT NOT NULL,
    parser TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime REAL NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS sources (
    key TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    digest TEXT NOT NULL,
    size INTEGER NOT NULL,
    mtime REAL NOT NULL,
    raw_digest TEXT NOT NULL,
    raw_size INTEGER NOT NULL,
    raw_mtime REAL NOT NULL,
    raw TEXT NOT NULL,
    parser TEXT NOT NULL,
    status TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    raw_id TEXT NOT NULL DEFAULT '',
    last_parsed_digest TEXT NOT NULL DEFAULT ''
);
CREATE TABLE IF NOT EXISTS links (
    src TEXT NOT NULL,
    dst TEXT NOT NULL,
    UNIQUE(src, dst)
);
CREATE INDEX IF NOT EXISTS links_dst ON links(dst);
CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    subject TEXT NOT NULL,
    -- queued | leased | expired | accepted; rows are never deleted
    state TEXT NOT NULL,
    epoch INTEGER NOT NULL DEFAULT 0,
    inputs TEXT NOT NULL,
    payload TEXT,
    last_error TEXT,
    attempts INTEGER NOT NULL DEFAULT 0,
    lease_until TEXT,
    created_at TEXT NOT NULL,
    closed_at TEXT
);
CREATE INDEX IF NOT EXISTS tasks_state ON tasks(state, kind);
CREATE TABLE IF NOT EXISTS task_sources (
    task_id TEXT NOT NULL,
    source_key TEXT NOT NULL,
    PRIMARY KEY (task_id, source_key)
);
CREATE INDEX IF NOT EXISTS task_sources_key ON task_sources(source_key);
CREATE TABLE IF NOT EXISTS page_revs (
    page TEXT NOT NULL,
    rev INTEGER NOT NULL,
    meta TEXT NOT NULL,
    body TEXT NOT NULL,
    digest TEXT NOT NULL,
    render_digest TEXT NOT NULL,
    view_digest TEXT NOT NULL,
    view_path TEXT NOT NULL,
    origin TEXT NOT NULL,
    accepted_at TEXT NOT NULL,
    PRIMARY KEY (page, rev)
);
"""

REQUIRED_COLUMNS = (
    ("sources", ("raw_digest", "raw_size", "raw_mtime", "raw_id", "last_parsed_digest")),
    ("raws", ("id", "name", "path", "parser", "size", "mtime", "updated_at")),
    ("page_revs", ("view_digest", "view_path")),
)


def create_schema(conn):
    """Create every table this skill version expects; existing rows stay put."""
    with conn:
        conn.executescript(SCHEMA_SQL)


def ensure_columns(conn):
    """Refuse a ledger from an older skill; there is no migration on purpose."""
    for table, columns in REQUIRED_COLUMNS:
        present = {
            row["name"] for row in conn.execute(f"PRAGMA table_info({table})")
        }
        if not set(columns) <= present:
            raise WikiError(
                "E_STATE",
                "ledger schema is older than this skill",
                "delete .wiki/ then run `init` and `ingest`",
                3,
            )


OPEN_PAGE_INDEX = """
CREATE UNIQUE INDEX IF NOT EXISTS tasks_open_page ON tasks(subject)
    WHERE kind = 'page' AND state IN ('queued', 'leased')
"""


def ensure_open_page_index(conn):
    """One open page task per page, enforced by the ledger itself.

    A ledger written by an older or racing process may already hold two open
    page tasks for one page. Keep the earliest and expire the rest instead of
    refusing the workspace: the losing task's sources move to the keeper.
    """
    try:
        with conn:
            conn.executescript(OPEN_PAGE_INDEX)
        return
    except sqlite3.IntegrityError:
        pass
    rows = conn.execute(
        "SELECT id, subject FROM tasks WHERE kind = 'page' AND state IN ('queued', 'leased') "
        "ORDER BY created_at, id"
    ).fetchall()
    keeper = {}
    stamp = datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")
    with conn:
        for row in rows:
            first = keeper.setdefault(row["subject"], row["id"])
            if first == row["id"]:
                continue
            conn.execute(
                "UPDATE tasks SET state = 'expired', last_error = ?, lease_until = NULL, "
                "closed_at = ? WHERE id = ?",
                (f"duplicate open page task for {row['subject']}", stamp, row["id"]),
            )
            conn.execute(
                "INSERT OR IGNORE INTO task_sources (task_id, source_key) "
                "SELECT ?, source_key FROM task_sources WHERE task_id = ?",
                (first, row["id"]),
            )
            conn.execute("DELETE FROM task_sources WHERE task_id = ?", (row["id"],))
        conn.executescript(OPEN_PAGE_INDEX)
