"""账本 schema 与 Wiki 组合根。

表契约住在 SCHEMA_SQL；`Wiki.ensure` 打开连接后调用本模块的建库函数，
命令代码从不自己建表。`Wiki` 由 LedgerMixin（本模块）加上 pages/view/task
三个 mixin 组合而成；命令行入口在 cli.py。

没有迁移是有意为之：与当前版本不符的账本一律以 `E_STATE` 拒绝，
由用户删掉 `.wiki/` 后重建。
"""

from __future__ import annotations

import json
import os
import re
import sqlite3
import sys
from datetime import date, datetime, timezone
from pathlib import Path

import scenarios
import topics
from errors import WikiError
from frontmatter import split_frontmatter
from ingest import raw_facts
from pages import PagesMixin, render_page
from tasks import TASK_DIR
from task_store import TaskStoreMixin
from util import digest_bytes, digest_text, norm_key, write_atomic
from view import ViewMixin
from workspace import WIKI_DIR, window_safe

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

TABLES = ("sources", "raws", "page_revs", "tasks", "task_sources", "links")


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
    "publish": {"dir": "wiki", "enabled": True, "topics": True, "scenarios": True},
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
    elif name in (WIKI_DIR, topics.TOPIC_DIR) or name in scenarios.SCENARIO_DIRS:
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


class LedgerMixin:
    """账本连接、来源行与原文读写；由 Wiki 组合，不自带业务判断。"""

    def __init__(self, root, scan_root=None):
        self.root = Path(root).resolve()
        self.scan_root = Path(scan_root).resolve() if scan_root else self.root
        self.report_root = self.scan_root
        self.wiki_dir = self.root / WIKI_DIR
        # Placeholder until ensure() reads the config, which then repoints it at
        # the publish tree: raw text never lives in `.wiki/` any more.
        self.raw_dir = self.wiki_dir / RAW_SUBDIR
        self.tasks_dir = self.wiki_dir / TASK_DIR
        self.draft_dir = self.wiki_dir / DRAFT_SUBDIR
        self.db_path = self.wiki_dir / DB_NAME
        self.config_path = self.wiki_dir / CONFIG_NAME
        self.conn = None
        self.config = None
        self.view_events = []

    @property
    def publish_dir(self):
        """Browsable view directory at the wiki root, named by `publish.dir`."""
        return self.root / publish_dir_name(self.config)

    @property
    def publish_raw_dir(self):
        return self.publish_dir / RAW_SUBDIR

    def ensure(self, push=True):
        created = []
        for directory in (self.wiki_dir, self.tasks_dir, self.draft_dir):
            if not directory.exists():
                directory.mkdir(parents=True)
                created.append(norm_key(directory.relative_to(self.root)))
        if not self.config_path.exists():
            self.config_path.write_text(
                json.dumps(DEFAULT_CONFIG, ensure_ascii=False, indent=2) + "\n",
                encoding="utf-8",
            )
            created.append(norm_key(self.config_path.relative_to(self.root)))
        self.config = load_config(self.config_path)
        publish_dir_name(self.config)  # an invalid dir is rejected even when disabled
        # Raw text is engine data: the publish switch never moves it.
        self.raw_dir = self.publish_raw_dir
        self.ensure_publish(created)
        if self.conn is None:
            fresh_db = not self.db_path.exists()
            try:
                self.conn = sqlite3.connect(self.db_path)
                self.conn.row_factory = sqlite3.Row
                self.conn.execute("PRAGMA busy_timeout = 5000")
                create_schema(self.conn)
                ensure_columns(self.conn)
                ensure_open_page_index(self.conn)
            except sqlite3.DatabaseError as error:
                conn, self.conn = self.conn, None
                if conn is not None:
                    conn.close()
                raise WikiError(
                    "E_DB",
                    f"ledger cannot be opened: {error}",
                    "stop writing; restore the whole .wiki/ from a backup",
                    3,
                )
            if fresh_db:
                created.append(norm_key(self.db_path.relative_to(self.root)))
        # R11：sync 要先看磁盘再让推送动手，否则挪走的文件会被重建出第二个副本。
        if push:
            self.view_events = self.sync_view()
            for line in self.view_events:
                if line.startswith("warning: "):
                    sys.stderr.write(line + "\n")
        else:
            self.view_events = []
        return created

    def ensure_publish(self, created):
        """Create the view tree; the switch only stops the page-side output."""
        name = publish_dir_name(self.config)
        for directory in (self.root / name, self.publish_raw_dir):
            shown = norm_key(directory.relative_to(self.root))
            if not window_safe(self.root, directory.resolve()):
                raise WikiError(
                    "E_PATH",
                    f"publish dir {shown} resolves outside the workspace root",
                    "keep publish.dir inside the workspace, or remove the symlink",
                    2,
                )
            if not directory.is_dir() and (directory.exists() or directory.is_symlink()):
                raise WikiError(
                    "E_STATE",
                    f"publish dir {shown} exists but is not a directory",
                    "move it aside, or set publish.dir to another name",
                    3,
                )
            if not directory.exists():
                directory.mkdir(parents=True)
                created.append(shown)
        # 目录本身始终存在（raw/ 是引擎数据，位置不随开关变化）；剩下的事情
        # 归 sync_view，开关为假时它直接返回。
        if not self.config["publish"]["enabled"]:
            return

    def close(self):
        if self.conn is not None:
            self.conn.close()
            self.conn = None

    def scan_files(self):
        ignores = set(self.config["scan"]["ignore"])
        root_skips = {
            topics.TOPIC_DIR, publish_dir_name(self.config), *scenarios.SCENARIO_DIRS,
        }
        found = []
        for current, dirnames, filenames in os.walk(self.scan_root):
            at_scan_root = Path(current) == Path(self.scan_root)
            dirnames[:] = sorted(
                name for name in dirnames
                if name not in ignores and not name.startswith(".")
                and not (at_scan_root and name in root_skips)
            )
            for name in sorted(filenames):
                if name.startswith("."):
                    continue
                candidate = Path(current) / name
                if candidate.is_symlink() and not window_safe(self.scan_root, candidate.resolve()):
                    continue
                found.append(candidate)
        return found

    def source_row(self, key):
        return self.conn.execute(
            "SELECT * FROM sources WHERE key = ?", (key,)
        ).fetchone()

    def all_sources(self):
        return self.conn.execute("SELECT * FROM sources ORDER BY key").fetchall()

    def source_raw_state(self, row):
        """规格 §判定表：raw 侧状态 ok / missing / edited。"""
        return raw_facts(self.root, row)[0]

    def source_snapshot(self, key):
        """The source version a task accepts: source side plus raw side (规格 §任务类型与快照)."""
        row = self.source_row(key)
        if row is None:
            raise WikiError("E_NOT_FOUND", f"source {key} is not in the ledger",
                            "run `ingest`, then `next`", 3)
        # 原文侧以 raw 文件的实际存在性为准：没有副本时三项都记空，
        # 任务就不会因为一份从未存在过的原文而失效。
        if self.source_raw_state(row) == "missing":
            raw, raw_size, raw_mtime, raw_digest = "", 0, 0.0, ""
        else:
            raw = row["raw"] or ""
            raw_size = int(row["raw_size"])
            raw_mtime = float(row["raw_mtime"])
            raw_digest = row["raw_digest"] or ""
        return {
            "key": key,
            "source_size": int(row["size"]),
            "source_mtime": float(row["mtime"]),
            "source_digest": row["digest"],
            "raw": raw,
            "raw_size": raw_size,
            "raw_mtime": raw_mtime,
            "raw_digest": raw_digest,
        }

    def _raw_name_taken(self, name, raw_id):
        row = self.conn.execute(
            "SELECT id FROM raws WHERE name = ?", (name,)
        ).fetchone()
        return row is not None and row["id"] != raw_id

    def _raw_name(self, key, raw_id):
        """First-seen basename of the source key, unique among raw rows."""
        base = key.rsplit("/", 1)[-1]
        base = re.sub(r"\.[A-Za-z0-9]+$", "", base)
        base = re.sub(r"[^\w.-]+", "-", base, flags=re.UNICODE).strip("-._") or "source"
        base = base[:80]
        candidate, ordinal = base, 2
        while self._raw_name_taken(candidate, raw_id):
            candidate = f"{base}-{ordinal}"
            ordinal += 1
        return candidate

    def raw_mounts(self, raw_id):
        return [
            row["key"] for row in self.conn.execute(
                "SELECT key FROM sources WHERE raw_id = ? ORDER BY key", (raw_id,)
            )
        ]

    def attach_raw(self, key, text, parser, title):
        """Store one parsed body as a raw copy; return (path, id).

        The body digest is the identity: the same text always lands on one row
        and one file, so a hundred copies of a document leave one raw copy
        mounted by many sources. When the previous raw of this key had no other
        mount, the row is replaced in place and keeps its filename.
        """
        raw_id = digest_text(text)
        today = date.today().isoformat()
        row = self.conn.execute("SELECT * FROM raws WHERE id = ?", (raw_id,)).fetchone()
        if row is not None:
            name, rel = row["name"], row["path"]
        else:
            current = self.source_row(key)
            previous = None
            if current is not None and current["raw_id"]:
                previous = self.conn.execute(
                    "SELECT * FROM raws WHERE id = ?", (current["raw_id"],)
                ).fetchone()
            if previous is not None and self.raw_mounts(previous["id"]) == [key]:
                # 只有这一个源，换内容就是原地换行：文件名跟着这份内容继续用。
                name, rel = previous["name"], previous["path"]
                with self.conn:
                    self.conn.execute("DELETE FROM raws WHERE id = ?", (previous["id"],))
            else:
                name = self._raw_name(key, raw_id)
                rel = norm_key(
                    (self.publish_raw_dir / f"{name}.md").relative_to(self.root)
                )
        mounts = self.raw_mounts(raw_id)
        if key not in mounts:
            mounts = sorted(mounts + [key])
        meta = {
            "id": raw_id,
            "title": title,
            "type": "raw",
            "summary": f"parsed from {', '.join(mounts)} by {parser}",
            "sources": mounts,
            "status": "raw",
            "updated": today,
        }
        target = self.root / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        write_atomic(target, render_page(meta, text))
        size, mtime = target.stat().st_size, target.stat().st_mtime
        with self.conn:
            if row is None:
                self.conn.execute(
                    """
                    INSERT INTO raws (id, name, path, parser, size, mtime, updated_at)
                    VALUES (?, ?, ?, ?, ?, ?, ?)
                    """,
                    (raw_id, name, rel, parser, size, mtime, today),
                )
            else:
                self.conn.execute(
                    "UPDATE raws SET size = ?, mtime = ?, updated_at = ? WHERE id = ?",
                    (size, mtime, today, raw_id),
                )
            # 一份 raw 可能被多个源挂着：文件的 size/mtime 变了，每个挂载源
            # 记的原文事实都要一起对齐，否则下轮 ingest 会把共享者误判成「人工改过」。
            self.conn.execute(
                "UPDATE sources SET raw_digest = ?, raw_size = ?, raw_mtime = ? "
                "WHERE raw_id = ?",
                (digest_bytes(target.read_bytes()), size, mtime, raw_id),
            )
        return rel, raw_id

    def adopt_raw_edit(self, key, parser):
        """Re-key a hand-edited raw in place; the file itself is left untouched.

        The body digest is the ledger identity, so a reformatted copy becomes a
        new row. The user's own bytes stay exactly as written — the ledger is
        the fact source, the frontmatter inside a hand-edited raw is whatever
        the parse wrote back then.
        """
        row = self.source_row(key)
        if row is None or not row["raw"]:
            return "", ""
        target = self.root / row["raw"]
        try:
            _, body = split_frontmatter(target.read_text(encoding="utf-8", errors="replace"))
        except OSError:
            return row["raw"], row["raw_id"]
        raw_id = digest_text(body)
        if raw_id == row["raw_id"]:
            return row["raw"], raw_id
        known = self.conn.execute(
            "SELECT * FROM raws WHERE id = ?", (raw_id,)
        ).fetchone()
        if known is not None:
            # 同样的正文已经在别处落过一份：直接挂过去，不再造第二份。
            return known["path"], raw_id
        if self.raw_mounts(row["raw_id"]) != [key]:
            # 这份原文还被别的源共享：不能就地改行，另存一份给这个源。
            return self.attach_raw(key, body, parser, target.name)
        previous = self.conn.execute(
            "SELECT * FROM raws WHERE id = ?", (row["raw_id"],)
        ).fetchone()
        stat = target.stat()
        with self.conn:
            if previous is not None:
                self.conn.execute("DELETE FROM raws WHERE id = ?", (row["raw_id"],))
                name, rel, updated = previous["name"], previous["path"], date.today().isoformat()
            else:
                name = self._raw_name(key, raw_id)
                rel = norm_key((self.publish_raw_dir / f"{name}.md").relative_to(self.root))
                updated = date.today().isoformat()
            self.conn.execute(
                """
                INSERT INTO raws (id, name, path, parser, size, mtime, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                """,
                (raw_id, name, rel, parser, stat.st_size, stat.st_mtime, updated),
            )
        return rel, raw_id

    def collect_raws(self):
        """Drop raw rows no source points at, and their files."""
        dropped = [
            row for row in self.conn.execute(
                """
                SELECT * FROM raws
                WHERE id NOT IN (SELECT raw_id FROM sources WHERE raw_id <> '')
                """
            )
        ]
        for row in dropped:
            with self.conn:
                self.conn.execute("DELETE FROM raws WHERE id = ?", (row["id"],))
            target = self.root / row["path"]
            if target.exists():
                target.unlink()
        return [row["path"] for row in dropped]

    def upsert_source(self, path, digest, size, mtime, raw, parser, status,
                      raw_digest, raw_size, raw_mtime, raw_id="",
                      last_parsed_digest=""):
        key = norm_key(path)
        now = date.today().isoformat()
        with self.conn:
            self.conn.execute(
                """
                INSERT INTO sources (key, path, digest, size, mtime, raw_digest, raw_size,
                                     raw_mtime, raw, parser, status, updated_at, raw_id,
                                     last_parsed_digest)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(key) DO UPDATE SET
                    digest = excluded.digest,
                    size = excluded.size,
                    mtime = excluded.mtime,
                    raw_digest = excluded.raw_digest,
                    raw_size = excluded.raw_size,
                    raw_mtime = excluded.raw_mtime,
                    raw = excluded.raw,
                    parser = excluded.parser,
                    status = excluded.status,
                    updated_at = excluded.updated_at,
                    raw_id = excluded.raw_id,
                    last_parsed_digest = excluded.last_parsed_digest
                """,
                (key, str(path), digest, size, mtime, raw_digest, raw_size, raw_mtime,
                 raw, parser, status, now, raw_id, last_parsed_digest),
            )

    def set_source_status(self, key, status):
        with self.conn:
            self.conn.execute(
                "UPDATE sources SET status = ?, updated_at = ? WHERE key = ?",
                (status, date.today().isoformat(), key),
            )

    def source_body(self, key):
        """Parsed text of one source, without its raw frontmatter."""
        row = self.source_row(key)
        if not row or not row["raw"]:
            return ""
        path = self.root / row["raw"]
        if not path.exists():
            return ""
        _, body = split_frontmatter(path.read_text(encoding="utf-8", errors="replace"))
        return body


class Wiki(LedgerMixin, PagesMixin, ViewMixin, TaskStoreMixin):
    """The knowledge base rooted at one directory."""
