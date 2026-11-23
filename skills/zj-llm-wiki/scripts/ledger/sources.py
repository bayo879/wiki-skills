"""来源行的读写：扫描来源、来源快照、写入与状态更新。

`Wiki` 的组合来源之一；原文（raw）读写见 raws.py。"""

from __future__ import annotations

import os
from datetime import date
from pathlib import Path

import topics
import workflows
from errors import WikiError
from frontmatter import split_frontmatter
from ingest import raw_facts
from ledger.schema import publish_dir_name
from util import norm_key
from workspace import window_safe


class SourceMixin:
    """来源行：扫描、快照、写入与状态；由 Wiki 组合。"""

    def scan_files(self):
        ignores = set(self.config["scan"]["ignore"])
        root_skips = {
            topics.TOPIC_DIR, publish_dir_name(self.config), *workflows.WORKFLOW_DIRS,
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
            "raw_id": row["raw_id"] or "",
            "source_size": int(row["size"]),
            "source_mtime": float(row["mtime"]),
            "source_digest": row["digest"],
            "raw": raw,
            "raw_size": raw_size,
            "raw_mtime": raw_mtime,
            "raw_digest": raw_digest,
        }

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
