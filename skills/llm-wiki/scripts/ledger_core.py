"""账本连接与目录布局：打开/只读打开、建表与发布目录、关闭。

`Wiki` 的组合来源之一；来源行与原文读写分别在 ledger_sources 与 ledger_raws。"""

from __future__ import annotations

import json
import sqlite3
import sys
from pathlib import Path

import topics
from errors import WikiError
from ledger_schema import (
    CONFIG_NAME,
    DB_NAME,
    DEFAULT_CONFIG,
    DRAFT_SUBDIR,
    RAW_SUBDIR,
    REQUIRED_COLUMNS,
    create_schema,
    ensure_columns,
    ensure_open_page_index,
    load_config,
    publish_dir_name,
)
from tasks import TASK_DIR
from util import norm_key
from workspace import WIKI_DIR, window_safe


class LedgerMixin:
    """账本连接与目录布局；由 Wiki 组合，不自带业务判断。"""

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

    def open_readonly(self):
        """Open an existing ledger without creating, migrating or publishing.

        `query` is a snapshot read: it must not create `.wiki/`, repair the
        schema or push the view. Requiring an existing database also keeps a
        misspelled `--root` from silently looking like an empty wiki.
        """
        if not self.db_path.is_file():
            raise WikiError(
                "E_STATE",
                "workspace is not initialized",
                "run `ingest` first; query only reads an existing ledger",
                3,
                section="工作区未初始化",
            )
        self.config = load_config(self.config_path)
        publish_dir_name(self.config)
        self.raw_dir = self.publish_raw_dir
        try:
            self.conn = sqlite3.connect(f"{self.db_path.as_uri()}?mode=ro", uri=True)
            self.conn.row_factory = sqlite3.Row
            self.conn.execute("PRAGMA query_only = ON")
            self.conn.execute("PRAGMA busy_timeout = 5000")
            missing = set()
            for table, columns in REQUIRED_COLUMNS:
                present = {
                    row[1]
                    for row in self.conn.execute(f"PRAGMA table_info({table})")
                }
                missing.update(
                    f"{table}.{column}" for column in columns if column not in present
                )
            for table in ("sources", "page_revs", "links"):
                if self.conn.execute(
                    "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
                    (table,),
                ).fetchone() is None:
                    missing.add(table)
        except sqlite3.DatabaseError as error:
            conn, self.conn = self.conn, None
            if conn is not None:
                conn.close()
            raise WikiError(
                "E_DB",
                f"ledger cannot be opened: {error}",
                "run `status` or `ingest` to repair the ledger, or restore it from backup",
                3,
            ) from error
        if missing:
            self.conn.close()
            self.conn = None
            raise WikiError(
                "E_STATE",
                f"ledger schema is out of date: missing {', '.join(sorted(missing))}",
                "run `status` or `ingest` to migrate the ledger",
                3,
                section="旧账本",
            )
        return self

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
        # 工作流定义和话题是项目内容，目录必须始终存在；runs/ 按需创建。
        view = self.publish_dir
        for name in ("workflows", topics.TOPIC_DIR):
            target = view / name
            if not target.exists():
                target.mkdir(parents=True, exist_ok=True)
                created.append(target.relative_to(self.root).as_posix())
        # 目录本身始终存在（raw/ 是引擎数据，位置不随开关变化）；剩下的事情
        # 归 sync_view，开关为假时它直接返回。
        if not self.config["publish"]["enabled"]:
            return

    def close(self):
        if self.conn is not None:
            self.conn.close()
            self.conn = None
