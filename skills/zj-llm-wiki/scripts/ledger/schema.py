"""账本表结构、迁移与配置：SCHEMA_SQL、建库函数、`.wiki` 目录常量与 publish 配置。

`Wiki.ensure` 打开连接后调用这里的建库函数，命令代码从不自己建表；
本模块是叶子，不 import 任何账本 mixin，供 core/sources/raws 共用。"""

from __future__ import annotations

import json
import sqlite3
from datetime import datetime, timezone
from pathlib import Path

import topics
import workflows
from errors import WikiError
from workspace import WIKI_DIR


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
-- 租约归属：只记 state='leased' 的任务，随释放/提交/过期由 sweep_leases 清掉。
CREATE TABLE IF NOT EXISTS task_leases (
    task_id TEXT PRIMARY KEY,
    owner TEXT NOT NULL,
    leased_at TEXT NOT NULL
);
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

TABLES = ("sources", "raws", "page_revs", "tasks", "task_leases", "task_sources", "links")

def create_schema(conn):
    """Create every table this skill version expects; existing rows stay put."""
    with conn:
        conn.executescript(SCHEMA_SQL)

def _stale_tables(conn):
    """Tables that exist but lack the columns this version expects."""
    stale = []
    for table, columns in REQUIRED_COLUMNS:
        present = {row[1] for row in conn.execute(f"PRAGMA table_info({table})")}
        if present and not set(columns) <= present:
            stale.append(table)
    return stale

def _ledger_empty(conn):
    for table in TABLES:
        try:
            row = conn.execute(f"SELECT 1 FROM {table} LIMIT 1").fetchone()
        except sqlite3.OperationalError:
            continue
        if row:
            return False
    return True

def ensure_columns(conn):
    """Refuse a ledger from an older skill; there is no migration on purpose.

    唯一的例外是没有数据的旧表：没有内容可保，直接按当前 schema 重建，不算迁移。
    """
    stale = _stale_tables(conn)
    if not stale:
        return
    if not _ledger_empty(conn):
        raise WikiError(
            "E_STATE",
            f"ledger schema is older than this skill: {', '.join(stale)}",
            "back up .wiki/ if the pages matter, delete it, then run `ingest`; "
            "pages in the view dir have to be re-created by hand",
            3,
        )
    with conn:
        for table in stale:
            conn.execute(f"DROP TABLE IF EXISTS {table}")
    create_schema(conn)

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

RAW_SUBDIR = "raw"

DRAFT_SUBDIR = "drafts"

DB_NAME = "wiki.db"

CONFIG_NAME = "wiki.json"

DEFAULT_CONFIG = {
    # 类型只表示内容形态：三类的判据互斥，没有「装不进就放这里」的兜底类型。
    # 老库的 wiki.json 里可能还留着 "note"，那是遗留配置，引擎继续认（见 tasks.PAGE_SECTIONS）。
    "schema": {"types": ["concept", "entity", "process"]},
    "scan": {
        "ignore": [
            ".git", ".hg", ".svn", ".wiki", "node_modules", "target", "dist",
            "build", "vendor", ".venv", "venv", "__pycache__", ".idea", ".vscode",
        ]
    },
    "ingest": {"max_source_bytes": 50_000_000},
    "publish": {"dir": "wiki", "enabled": True},
}

def load_config(path):
    if not path.exists():
        return json.loads(json.dumps(DEFAULT_CONFIG))
    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise WikiError(
            "E_CONFIG",
            f"cannot read {WIKI_DIR}/{CONFIG_NAME}: {error}",
            "fix the JSON syntax, or delete the file to regenerate defaults",
            2,
        )
    config = json.loads(json.dumps(DEFAULT_CONFIG))
    for section, values in raw.items():
        if section in config and isinstance(values, dict):
            config[section].update(values)
    return config

def publish_dir_name(config):
    """Validated single-segment name of the browsable view directory.

    The directory sits at the wiki root, next to `.wiki/`, so the name must be
    one safe segment: no separators, no dot prefix, no absolute path, and no
    name that already belongs to the engine or another wiki layer.
    """
    section = (config or {}).get("publish") or {}
    value = section.get("dir")
    name = value if isinstance(value, str) else ""
    reason = ""
    if not name.strip():
        reason = "must not be empty"
    elif name in (".", ".."):
        reason = "must name a directory, not a relative reference"
    elif name in (WIKI_DIR, topics.TOPIC_DIR) or name in workflows.WORKFLOW_DIRS:
        reason = "is reserved for the engine or another wiki layer"
    elif Path(name).is_absolute() or name.startswith("\\"):
        reason = "must be relative to the wiki root"
    elif "/" in name or "\\" in name:
        reason = "must be a single directory name, not a path"
    elif name.startswith("."):
        reason = "must not start with a dot"
    if reason:
        raise WikiError(
            "E_CONFIG",
            f"publish.dir {value!r} {reason}",
            "set publish.dir to one directory name such as \"wiki\"",
            2,
        )
    return name

def view_dir(root):
    """Return the configured view directory without creating or opening it."""
    root = Path(root)
    return root / publish_dir_name(load_config(root / WIKI_DIR / CONFIG_NAME))
