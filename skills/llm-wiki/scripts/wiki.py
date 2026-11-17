#!/usr/bin/env python3
"""Portable wiki engine.

One directory of source files in, one deterministic `.wiki/` state tree out.
Only the Python standard library is required.

The engine owns the ledger: scanning, parsing, hashing, task state, validation,
page revisions, and the push into the publish view. Semantic work -- deciding
which pages exist, their boundaries, their prose, and which quotes support them
-- belongs to the agent, which submits JSON payloads and never writes `.wiki/`.

Commands are documented by `wiki.py --help` and in `docs/readme/cli.md`.
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import sqlite3
import sys
from datetime import date, datetime, timezone
from pathlib import Path

sys.dont_write_bytecode = True  # never write __pycache__ into a global skill
from runtime import require_python
require_python()

import health
import ledger
import scenarios
import search
import topics
from errors import WikiError
from frontmatter import split_frontmatter, yaml_scalar
from tasks import TASK_DIR, render_brief, task_dir_relative, task_packet
from validation import validate_page_payload
from workspace import WIKI_DIR, dry_store, inside_root, window_safe, workspace_lock
from ingest import (
    MAX_RAW_CHARS,
    extract_text,
    file_facts,
    ingest_decision,
    raw_facts,
    read_bytes,
)
from pages import LINK_RE, meta_mismatch, render_page, split_evidence, view_name
from util import (
    digest_bytes,
    digest_text,
    dump_meta,
    norm_key,
    now_iso,
    parse_iso,
    same_stamp,
    write_atomic,
)
from view import render_view_readme, sync_view_file

VERSION = "0.4.1"
RAW_SUBDIR = "raw"
DRAFT_SUBDIR = "drafts"
DB_NAME = "wiki.db"
CONFIG_NAME = "wiki.json"
TASK_LEASE_SECONDS = 900
TASK_OPEN_STATES = ("queued", "leased")

DEFAULT_CONFIG = {
    "schema": {"types": ["concept", "entity", "process", "note"]},
    "scan": {
        "ignore": [
            ".git", ".hg", ".svn", ".wiki", "node_modules", "target", "dist",
            "build", "vendor", ".venv", "venv", "__pycache__", ".idea", ".vscode",
        ]
    },
    "ingest": {"max_source_bytes": 50_000_000},
    "publish": {"dir": "wiki", "enabled": True, "topics": True, "scenarios": True},
}

ID_RE = re.compile(r"^[a-z0-9][a-z0-9._-]*$")
TITLE_RE = re.compile(r"^#\s+(.+)$", re.MULTILINE)


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


class Wiki:
    """The knowledge base rooted at one directory."""

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
            self.conn = sqlite3.connect(self.db_path)
            self.conn.row_factory = sqlite3.Row
            self.conn.execute("PRAGMA busy_timeout = 5000")
            ledger.create_schema(self.conn)
            ledger.ensure_columns(self.conn)
            ledger.ensure_open_page_index(self.conn)
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

    # ---------------------------------------------------------------- sources

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

    # ------------------------------------------------------------------ pages

    def latest_rev_row(self, page_id):
        return self.conn.execute(
            "SELECT * FROM page_revs WHERE page = ? ORDER BY rev DESC LIMIT 1",
            (page_id,),
        ).fetchone()

    def page_record(self, page_id):
        row = self.latest_rev_row(page_id)
        if not row:
            return None
        return self._page_row_to_record(row)

    def _page_row_to_record(self, row):
        """One ledger row as a page record, including the published view facts."""
        meta = json.loads(row["meta"])
        meta["status"] = self.computed_status(meta)
        return {
            "id": row["page"],
            "rev": row["rev"],
            "meta": meta,
            "body": row["body"],
            "digest": row["digest"],
            "render_digest": row["render_digest"],
            "origin": row["origin"],
            "view_digest": row["view_digest"],
            "view_path": row["view_path"],
        }

    def all_pages(self):
        rows = self.conn.execute(
            """
            SELECT r.* FROM page_revs r
            WHERE r.rev = (SELECT MAX(x.rev) FROM page_revs x WHERE x.page = r.page)
            ORDER BY r.page
            """
        ).fetchall()
        return [self._page_row_to_record(row) for row in rows]

    def page_ids(self):
        return {
            row["page"]
            for row in self.conn.execute("SELECT DISTINCT page FROM page_revs")
        }

    def computed_status(self, meta):
        """A page is stale when any cited source changed or vanished."""
        snapshot = meta.get("source_digests") or {}
        for key in meta.get("sources") or []:
            row = self.source_row(key)
            if row is None or row["status"] in ("missing", "unsupported", "oversize"):
                return "stale"
            if snapshot.get(key) != row["raw_digest"]:
                return "stale"
        return "stable"

    def page_ref(self, record):
        """Where `read`/`query` point at a ledger page: the view copy, or `page:<id>`."""
        if not self.config["publish"]["enabled"]:
            return f"page:{record['id']}"
        target = self.publish_dir / self.view_relpath(record)
        return norm_key(target.relative_to(self.root))

    def view_relpath(self, record):
        """Publish path for one page record, relative to the publish dir."""
        meta = record["meta"]
        page_id = record["id"]
        base = view_name(meta.get("title"), page_id)
        relpath = f"pages/{meta['type']}/{base}.md"
        collision = False
        for other in self.all_pages():
            if other["id"] == page_id:
                continue
            if other["meta"].get("type") != meta.get("type"):
                continue
            if view_name(other["meta"].get("title"), other["id"]) == base:
                collision = True
                break
        if not collision:
            row = self.latest_rev_row(page_id)
            stored = row["view_path"] if row else ""
            collision = (
                (self.publish_dir / relpath).exists() and stored != relpath
            )
        if collision:
            relpath = f"pages/{meta['type']}/{base} ({page_id}).md"
        return relpath

    def view_text(self, record):
        """Page text as published in the view tree: same template, id alias."""
        return render_page(record["meta"], record["body"], aliases=[record["id"]])

    def write_rev(self, page, meta, body, origin):
        record = self.page_record(page)
        rev = (record["rev"] if record else 0) + 1
        meta = dict(meta)
        meta["id"] = page
        meta["sources"] = sorted(set(meta.get("sources") or []))
        meta["source_digests"] = {
            key: (self.source_row(key)["raw_digest"] if self.source_row(key) else "")
            for key in meta["sources"]
        }
        meta["updated"] = date.today().isoformat()
        meta["status"] = self.computed_status(meta)
        text = render_page(meta, body)
        render_digest = digest_text(text)
        # R1：view_digest/view_path 是"上一次发布出去的事实"，不是本行文本的摘要。
        # 新行先继承上一版；只有 sync_view 把文件写成功之后才改它。
        view_digest = record["view_digest"] if record else ""
        view_path = record["view_path"] if record else ""
        canonical = json.dumps(
            {
                "title": meta.get("title"),
                "type": meta.get("type"),
                "summary": meta.get("summary"),
                "sources": meta["sources"],
                "evidence": meta.get("evidence") or [],
                "boundaries": meta.get("boundaries") or {},
                "body": body,
            },
            sort_keys=True,
            ensure_ascii=False,
        )
        with self.conn:
            self.conn.execute(
                """
                INSERT INTO page_revs
                (page, rev, meta, body, digest, render_digest, view_digest, view_path,
                 origin, accepted_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    page, rev, json.dumps(meta, ensure_ascii=False, sort_keys=True),
                    body, digest_text(canonical), render_digest, view_digest, view_path,
                    origin, now_iso(),
                ),
            )
            self.rebuild_links(page, body)
        return rev

    def rebuild_links(self, page_id, body):
        with self.conn:
            self.conn.execute("DELETE FROM links WHERE src = ?", (page_id,))
            for target in sorted(set(LINK_RE.findall(body))):
                self.conn.execute(
                    "INSERT OR IGNORE INTO links (src, dst) VALUES (?, ?)",
                    (page_id, target.strip().lower()),
                )

    def rebuild_all_links(self):
        with self.conn:
            self.conn.execute("DELETE FROM links")
        for record in self.all_pages():
            self.rebuild_links(record["id"], record["body"])

    def links_for(self, page_id):
        outgoing = [
            row["dst"] for row in self.conn.execute(
                "SELECT dst FROM links WHERE src = ? ORDER BY dst", (page_id,)
            )
        ]
        incoming = [
            row["src"] for row in self.conn.execute(
                "SELECT src FROM links WHERE dst = ? ORDER BY src", (page_id,)
            )
        ]
        return outgoing, incoming

    # ------------------------------------------------------------------- view

    def sync_view(self):
        """Push the ledger into the view tree; return one line per action."""
        if not self.config["publish"]["enabled"]:
            return []
        events = []
        # raw/ 由 ingest 维护：这里只保证目录存在，绝不写、删其中任何文件。
        self.publish_raw_dir.mkdir(parents=True, exist_ok=True)
        records = self.all_pages()
        self._sync_view_pages(events, records)
        publish = self.config["publish"]
        if publish.get("topics", True):
            self._seed_view_topics(events)
        if publish.get("scenarios", True):
            self._seed_view_scenarios(events)
        write_atomic(self.publish_dir / "README.md", render_view_readme(self, records))
        events.append("README.md rewritten")
        return events

    def _view_targets(self, records):
        """Publish path per page for one snapshot; same rules as view_relpath."""
        shared = {}
        for record in records:
            key = (record["meta"].get("type"),
                   view_name(record["meta"].get("title"), record["id"]))
            shared[key] = shared.get(key, 0) + 1
        targets = {}
        for record in records:
            meta = record["meta"]
            base = view_name(meta.get("title"), record["id"])
            relpath = f"pages/{meta['type']}/{base}.md"
            taken = shared[(meta.get("type"), base)] > 1 or (
                (self.publish_dir / relpath).exists() and record["view_path"] != relpath
            )
            if taken:
                relpath = f"pages/{meta['type']}/{base} ({record['id']}).md"
            targets[record["id"]] = relpath
        return targets

    def _sync_view_pages(self, events, records):
        """Publish every page; a hand-edited copy is skipped, never overwritten."""
        base = self.publish_dir.resolve()
        targets = self._view_targets(records)
        for record in records:
            target = targets[record["id"]]
            desired = self.view_text(record)
            target_digest = digest_text(desired)
            published = record["view_path"]
            known = record["view_digest"]
            target_path = self.publish_dir / target
            old_path = self.publish_dir / published if published else None
            if not window_safe(base, target_path.resolve()) or (
                    old_path is not None and not window_safe(base, old_path.resolve())):
                events.append(f"warning: skip {record['id']}: view path escapes the publish dir")
                continue
            old_text = old_path.read_text(encoding="utf-8") if old_path and old_path.is_file() else None
            target_text = target_path.read_text(encoding="utf-8") if target_path.is_file() else None

            if old_text is not None and digest_text(old_text) != known:
                # P 被手改。唯一例外：Q == P 且磁盘恰好已是 T —— 那不是手改，是我们上次
                # 写完还没来得及回写事实（崩溃窗口），直接补事实，不覆盖任何东西。
                if target == published and target_text == desired:
                    self._publish_facts(record, target, target_digest, events)
                else:
                    events.append(
                        f"warning: {record['id']} was edited by hand at {published}; "
                        "published copy left alone (run `sync` to absorb it)"
                    )
                continue

            if target_text != desired:  # 已等于 T 时只补事实
                write_atomic(target_path, desired)
                events.append(f"{target} written")
            if published and published != target and old_text is not None \
                    and digest_text(old_text) == known:
                old_path.unlink()
                events.append(f"{published} removed")
            self._publish_facts(record, target, target_digest, events)

    def _publish_facts(self, record, view_path, view_digest, events):
        if record["view_path"] == view_path and record["view_digest"] == view_digest:
            return
        self.set_view_facts(record["id"], record["rev"], view_path, view_digest)
        record["view_path"] = view_path
        record["view_digest"] = view_digest
        events.append(f"{record['id']} facts updated")

    def set_view_facts(self, page_id, rev, view_path, view_digest):
        """Record what is now published for one revision (R1)."""
        self.conn.execute(
            "UPDATE page_revs SET view_path = ?, view_digest = ? "
            "WHERE page = ? AND rev = ?",
            (view_path, view_digest, page_id, rev),
        )
        self.conn.commit()

    def _seed_view_topics(self, events):
        """Copy each topic once; afterwards the copy belongs to the user."""
        destination = self.publish_dir / "topics"
        for entry in topics.scan(self.root):
            source = self.root / entry["path"]
            target = destination / Path(entry["path"]).name
            if target.exists():
                continue
            if not window_safe(self.root, source.resolve()):
                events.append(f"warning: skip topic {entry['path']}: source escapes the workspace")
                continue
            destination.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
            events.append(f"{norm_key(target.relative_to(self.root))} seeded")

    def _seed_view_scenarios(self, events):
        """Copy each project scenario once; built-in scenarios never enter the view."""
        destination = self.publish_dir / "scenarios"
        for entry in scenarios.scan(self.root, builtin=False):
            source = self.root / entry["path"]
            target = destination / entry["id"]
            if target.exists() or not source.is_dir():
                continue
            if not window_safe(self.root, source.resolve()):
                events.append(f"warning: skip scenario {entry['id']}: source escapes the workspace")
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(source, target)
            events.append(f"{norm_key(target.relative_to(self.root))} seeded")

    # ------------------------------------------------------------------ tasks

    def task_row(self, task_id):
        return self.conn.execute("SELECT * FROM tasks WHERE id = ?", (task_id,)).fetchone()

    def task_dir(self, task_id):
        return self.tasks_dir / task_id

    def clear_task_dir(self, task_id):
        shutil.rmtree(self.task_dir(task_id), ignore_errors=True)

    def cleanup_task_dirs(self):
        """Drop task folders that no longer belong to an open task."""
        if not self.tasks_dir.is_dir():
            return []
        dropped = []
        for entry in sorted(self.tasks_dir.iterdir()):
            if not entry.is_dir():
                continue
            row = self.task_row(entry.name)
            if row and row["state"] in TASK_OPEN_STATES:
                continue
            shutil.rmtree(entry, ignore_errors=True)
            dropped.append(entry.name)
        return dropped

    def materialize_task(self, row):
        """Write the task folder: the brief, its payload skeleton, and work data.

        Re-claiming a task refreshes the brief and the work view but never
        overwrites a payload.json the agent already filled.
        """
        work = self.task_work(row)
        packet = task_packet(row["id"], row["kind"], json.loads(row["inputs"]), work)
        directory = self.task_dir(row["id"])
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "brief.md").write_text(
            render_brief(row["id"], packet["brief"]), encoding="utf-8"
        )
        (directory / "work.json").write_text(
            json.dumps(work, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        skeleton = directory / "payload.json"
        if not skeleton.exists():
            skeleton.write_text(
                json.dumps(packet["template"], ensure_ascii=False, indent=2) + "\n",
                encoding="utf-8",
            )
        return packet

    def open_task_for_page(self, page_id):
        return self.conn.execute(
            "SELECT * FROM tasks WHERE kind = 'page' AND subject = ? AND state IN ('queued', 'leased')",
            (page_id,),
        ).fetchone()

    def open_tasks(self):
        return self.conn.execute(
            "SELECT * FROM tasks WHERE state IN ('queued', 'leased') ORDER BY id"
        ).fetchall()

    def task_counts(self):
        # 队列口径只在这一处定义：open 是 queued 与 leased 之和，expired/accepted 是
        # 终态历史，算在返回里供 status 使用，但不属于未完成的工作。
        rows = self.conn.execute(
            "SELECT kind, state, COUNT(*) AS n FROM tasks "
            "WHERE state IN ('queued', 'leased', 'expired', 'accepted') GROUP BY kind, state"
        ).fetchall()
        counts = {"open": 0, "queued": 0, "leased": 0, "route": 0, "page": 0,
                  "expired": 0, "accepted": 0}
        for row in rows:
            counts[row["state"]] += row["n"]
            if row["state"] in ("queued", "leased"):
                counts["open"] += row["n"]
                counts[row["kind"]] = counts.get(row["kind"], 0) + row["n"]
        return counts

    def source_in_open_task(self, source_key):
        return self.conn.execute(
            """
            SELECT t.id FROM task_sources ts JOIN tasks t ON t.id = ts.task_id
            WHERE ts.source_key = ? AND t.state IN ('queued', 'leased')
            LIMIT 1
            """,
            (source_key,),
        ).fetchone()

    def enqueue_task(self, kind, subject, inputs):
        existing = self.conn.execute(
            "SELECT id FROM tasks WHERE kind = ? AND subject = ? AND state IN ('queued', 'leased')",
            (kind, subject),
        ).fetchone()
        if existing:
            return existing["id"]
        # 树唯一性：一个源最多出现在一套未完成的任务树里。route 任务只是临时持有者，
        # 提交时会把源交给 page 树，所以只有 page 持有者才挡新树。同一次 route 提交
        # 拆出的多个 page 任务共享 route_task，是这条不变量的唯一例外。
        route_task = inputs.get("route_task")
        for item in inputs.get("sources", []):
            holder = self.source_in_open_task(item["key"])
            if not holder:
                continue
            holder_row = self.task_row(holder["id"])
            if holder_row["kind"] != "page":
                continue
            if kind == "page" and holder_row["subject"] == subject:
                continue
            if route_task:
                holder_inputs = json.loads(holder_row["inputs"])
                if holder_inputs.get("route_task") == route_task:
                    continue
            raise WikiError(
                "E_TASK_CONFLICT",
                f"source {item['key']} already belongs to task {holder_row['id']} "
                f"({holder_row['kind']} {holder_row['subject']})",
                "run `next` and `submit` to close that task tree first",
                3,
            )
        task_id = "t_" + os.urandom(4).hex()
        encoded = json.dumps(inputs, ensure_ascii=False, sort_keys=True)
        try:
            with self.conn:
                self.conn.execute(
                    """
                    INSERT INTO tasks (id, kind, subject, state, epoch, inputs, created_at)
                    VALUES (?, ?, ?, 'queued', 0, ?, ?)
                    """,
                    (task_id, kind, subject, encoded, now_iso()),
                )
                for item in inputs.get("sources", []):
                    self.conn.execute(
                        "INSERT OR IGNORE INTO task_sources (task_id, source_key) VALUES (?, ?)",
                        (task_id, item["key"]),
                    )
        except sqlite3.IntegrityError:
            # 另一个进程在检查与插入之间抢到同一页；账本的唯一索引兜底，复用那条任务。
            existing = self.conn.execute(
                "SELECT id FROM tasks WHERE kind = ? AND subject = ? AND state IN ('queued', 'leased')",
                (kind, subject),
            ).fetchone()
            if existing:
                return existing["id"]
            raise
        return task_id

    def attach_page_source(self, task_id, source_key):
        """Add a source, with its full snapshot, to an open page task."""
        row = self.task_row(task_id)
        inputs = json.loads(row["inputs"])
        if any(item["key"] == source_key for item in inputs.get("sources", [])):
            # 已经在树上：幂等返回，绝不刷新既有快照。
            return
        inputs["sources"].append(self.source_snapshot(source_key))
        with self.conn:
            self.conn.execute(
                "UPDATE tasks SET inputs = ? WHERE id = ?",
                (json.dumps(inputs, ensure_ascii=False, sort_keys=True), task_id),
            )
            self.conn.execute(
                "INSERT OR IGNORE INTO task_sources (task_id, source_key) VALUES (?, ?)",
                (task_id, source_key),
            )

    def task_drift(self, row):
        """First reason this task's snapshot no longer matches the files, or None when it is fresh."""
        inputs = json.loads(row["inputs"])
        for item in inputs.get("sources", []):
            key = item["key"]
            # 旧版三字段快照（key/digest/raw）没有源指纹，判不出是否漂移：
            # 当作漂移处理，让任务直接过期，不迁移、不猜版本。
            if not {"source_size", "source_mtime", "source_digest"} <= set(item):
                return f"task snapshot for {key} predates source fingerprints"
            source = self.source_row(key)
            if source is None or source["status"] == "missing":
                return f"source {key} vanished"
            # 两段式判定：先比 stat，一致就不读内容；不一致才读字节算摘要，
            # 摘要相同说明只是被 touch，任务仍然有效（规格 §判定表）。
            path = self.scan_root / source["path"]
            try:
                stat = path.stat()
            except OSError:
                return f"source {key} vanished"
            if not same_stamp(stat.st_size, stat.st_mtime,
                              item["source_size"], item["source_mtime"]):
                if digest_bytes(path.read_bytes()) != item["source_digest"]:
                    return f"source {key} changed"
            raw = item.get("raw") or ""
            if not raw:
                continue
            raw_path = self.root / raw
            try:
                raw_stat = raw_path.stat()
            except OSError:
                return f"raw copy {raw} is missing"
            if not same_stamp(raw_stat.st_size, raw_stat.st_mtime,
                              item["raw_size"], item["raw_mtime"]):
                if digest_bytes(raw_path.read_bytes()) != item["raw_digest"]:
                    return f"raw copy {raw} was edited"
        # 页面在任务排队后又前进了一版，任务基于的修订已经过期。
        if inputs.get("mode") == "update" and inputs.get("base_rev") is not None:
            record = self.page_record(inputs["page"])
            if not record or record["rev"] != inputs["base_rev"]:
                return f"page {inputs['page']} moved past revision {inputs['base_rev']}"
        return None

    def pin_open_task_raw(self, key, raw, raw_digest, raw_size, raw_mtime):
        """Point open task snapshots at an adopted raw copy (raw side only)."""
        rows = self.conn.execute(
            "SELECT id, inputs FROM tasks WHERE state IN ('queued', 'leased') "
            "AND id IN (SELECT task_id FROM task_sources WHERE source_key = ?)",
            (key,),
        ).fetchall()
        with self.conn:
            for row in rows:
                inputs = json.loads(row["inputs"])
                changed = False
                for item in inputs.get("sources", []):
                    if item["key"] != key:
                        continue
                    item.update(
                        {
                            "raw": raw,
                            "raw_size": raw_size,
                            "raw_mtime": raw_mtime,
                            "raw_digest": raw_digest,
                        }
                    )
                    changed = True
                if changed:
                    self.conn.execute(
                        "UPDATE tasks SET inputs = ? WHERE id = ?",
                        (json.dumps(inputs, ensure_ascii=False, sort_keys=True), row["id"]),
                    )

    def claim_tasks(self, kind=None, batch=None):
        """Claim queued tasks, then reissue still-leased ones to fill the batch.

        A reissued task keeps its task folder, so an agent that lost its turn
        continues the payload it already filled instead of stopping until the
        lease expires. Returns `(claimed, resumed)`.
        """
        now = now_iso()
        with self.conn:
            self.conn.execute(
                "UPDATE tasks SET state = 'queued', lease_until = NULL WHERE state = 'leased' AND lease_until < ?",
                (now,),
            )
            claimed = self._claim_queued(kind, batch)
            resumed = self._resume_leased(
                kind, max(0, batch - len(claimed)), {row["id"] for row in claimed}
            )
        return claimed, resumed

    def _claim_queued(self, kind, batch):
        sql = "SELECT * FROM tasks WHERE state = 'queued'"
        params = []
        if kind:
            sql += " AND kind = ?"
            params.append(kind)
        sql += " ORDER BY CASE kind WHEN 'route' THEN 0 ELSE 1 END, created_at, id LIMIT ?"
        params.append(batch)
        claimed = []
        for row in self.conn.execute(sql, params).fetchall():
            reason = self.task_drift(row)
            if reason:
                # 失效任务留在账本里作为历史，但不发出去。
                self.expire_task(row["id"], reason)
                continue
            self.conn.execute(
                "UPDATE tasks SET state = 'leased', epoch = epoch + 1, lease_until = ?, attempts = attempts + 1 WHERE id = ?",
                (self._lease_deadline(), row["id"]),
            )
            claimed.append(self.task_row(row["id"]))
        return claimed

    def _resume_leased(self, kind, limit, skip):
        """Reissue open leases in claim order, expiring drifted tasks.

        `skip` holds the ids claimed a moment ago in this same call; they are
        leased again already and must not come back twice.
        """
        if limit <= 0:
            return []
        sql = "SELECT * FROM tasks WHERE state = 'leased'"
        params = []
        if kind:
            sql += " AND kind = ?"
            params.append(kind)
        sql += " ORDER BY CASE kind WHEN 'route' THEN 0 ELSE 1 END, created_at, id"
        resumed = []
        for row in self.conn.execute(sql, params).fetchall():
            if len(resumed) >= limit:
                break
            if row["id"] in skip:
                continue
            reason = self.task_drift(row)
            if reason:
                self.expire_task(row["id"], reason)
                continue
            self.conn.execute(
                "UPDATE tasks SET state = 'leased', epoch = epoch + 1, lease_until = ?, attempts = attempts + 1 WHERE id = ?",
                (self._lease_deadline(), row["id"]),
            )
            resumed.append(self.task_row(row["id"]))
        return resumed

    def _lease_deadline(self):
        return datetime.fromtimestamp(
            datetime.now(timezone.utc).timestamp() + TASK_LEASE_SECONDS, timezone.utc
        ).strftime("%Y-%m-%dT%H:%M:%SZ")

    def release_task(self, task_id):
        row = self.task_row(task_id)
        if not row:
            raise WikiError("E_TASK_UNKNOWN", f"no task {task_id}", "run `next` to get a task id", 2)
        with self.conn:
            self.conn.execute(
                "UPDATE tasks SET state = 'queued', lease_until = NULL WHERE id = ?", (task_id,)
            )

    def requeue_task(self, task_id, error=None):
        with self.conn:
            self.conn.execute(
                "UPDATE tasks SET state = 'queued', lease_until = NULL, last_error = ? WHERE id = ?",
                (error, task_id),
            )

    def expire_task(self, task_id, reason):
        """Close an open task as expired, keeping its row for history."""
        with self.conn:
            cursor = self.conn.execute(
                "UPDATE tasks SET state = 'expired', last_error = ?, lease_until = NULL, closed_at = ? "
                "WHERE id = ? AND state IN ('queued', 'leased')",
                (reason, now_iso(), task_id),
            )
        if cursor.rowcount > 0:
            self.clear_task_dir(task_id)
        return cursor.rowcount > 0

    def close_task(self, task_id, payload, epoch=None):
        sql = "UPDATE tasks SET state = 'accepted', payload = ?, lease_until = NULL, closed_at = ? WHERE id = ?"
        params = [json.dumps(payload, ensure_ascii=False, sort_keys=True), now_iso(), task_id]
        if epoch is not None:
            sql += " AND epoch = ? AND state = 'leased'"
            params.append(epoch)
        with self.conn:
            cursor = self.conn.execute(sql, params)
        if epoch is not None and cursor.rowcount == 0:
            raise WikiError("E_TASK_STALE", f"task {task_id} changed before it could be closed",
                            "run `next` again and submit the fresh task", 3)
        if cursor.rowcount > 0:
            self.clear_task_dir(task_id)

    def page_index(self):
        return [
            {"id": record["id"], "title": record["meta"].get("title"), "type": record["meta"].get("type")}
            for record in self.all_pages()
        ]

    def pending_pages(self, exclude=None):
        """Pages that a queued page task will create but that do not exist yet."""
        rows = self.conn.execute(
            "SELECT id, subject, inputs FROM tasks WHERE kind = 'page' AND state IN ('queued', 'leased')"
        )
        pending = []
        for row in rows:
            if row["id"] == exclude:
                continue
            inputs = json.loads(row["inputs"])
            if inputs.get("mode") == "create" and not self.page_record(row["subject"]):
                pending.append({"id": row["subject"], "title": inputs.get("title"),
                                "type": inputs.get("type")})
        return pending

    def source_view(self, item):
        row = self.source_row(item["key"])
        return {
            "key": item["key"],
            "digest": row["digest"] if row else None,
            "raw": row["raw"] if row else None,
            "status": row["status"] if row else "unknown",
            "chars": len(self.source_body(item["key"])),
        }

    def task_work(self, row):
        inputs = json.loads(row["inputs"])
        work = {
            "sources": [
                {
                    "key": view["key"],
                    "raw": view["raw"],
                    "chars": view["chars"],
                }
                for view in (self.source_view(item) for item in inputs.get("sources", []))
            ],
            "page_index": self.page_index(),
            "pending_pages": self.pending_pages(row["id"]),
        }
        if row["kind"] == "page":
            record = self.page_record(inputs["page"])
            if record:
                work["page"] = {
                    "id": record["id"],
                    "rev": record["rev"],
                    "meta": dump_meta(record["meta"]),
                    "body": record["body"],
                }
        return work


def emit(cmd, root, summary, data, human_lines, ok=True):
    if ARGS.json:
        payload = {
            "ok": ok,
            "cmd": cmd,
            "version": VERSION,
            "root": str(root),
            "summary": summary,
            "data": data,
        }
        sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
        return
    if summary:
        sys.stdout.write(summary + "\n")
    for line in human_lines:
        sys.stdout.write(line + "\n")


def fail(cmd, root, error):
    if ARGS.json:
        payload = {
            "ok": False,
            "cmd": cmd,
            "version": VERSION,
            "root": str(root),
            "error": {
                "code": error.code,
                "message": error.message,
                "next": error.next_step,
            },
        }
        sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
    else:
        sys.stderr.write(f"[{error.code}] {error.message}\n")
        if error.next_step:
            sys.stderr.write(f"next: {error.next_step}\n")
    return error.exit_code


def open_wiki(require_sources=False, dry_run=False, push=True):
    # The workspace is the directory the command runs in. A wiki may live in a
    # subdirectory, but nothing may point above the workspace.
    workspace = Path.cwd().resolve()
    root = Path(ARGS.root).resolve() if ARGS.root else workspace
    if not window_safe(workspace, root):
        raise WikiError(
            "E_PATH",
            f"wiki root {root} is outside the workspace {workspace}",
            "run the command from the workspace, or pass a subdirectory inside it",
            2,
        )
    if dry_run and not (root / WIKI_DIR / CONFIG_NAME).exists():
        # A dry run must not create `.wiki/` in a directory the user is only
        # inspecting, so plan against a throwaway store under `.wiki/tmp/`.
        wiki = Wiki(dry_store(root), scan_root=root)
    else:
        wiki = Wiki(root)
    created = wiki.ensure(push=push)
    if require_sources and not wiki.all_sources():
        raise WikiError(
            "E_EMPTY",
            "no sources have been ingested yet",
            "run `ingest` first so the engine has raw material to work from",
            3,
        )
    return wiki, created


def require_health(allow_missing=False):
    health.require(Path(__file__).resolve().parent, Path.cwd(), allow_missing=allow_missing)


def command_check():
    report = health.check_installation(Path(__file__).resolve().parent, Path.cwd())
    lines = [
        f"{'ok  ' if item['ok'] else 'fail'} {item['name']}: {item['detail']}"
        for item in report["checks"]
    ]
    emit(
        "check", Path.cwd(),
        "self-check passed" if report["ok"] else "self-check failed",
        report,
        lines,
        ok=report["ok"],
    )
    return 0 if report["ok"] else 1


def command_init():
    require_health(allow_missing=True)
    wiki, created = open_wiki()
    rows = wiki.all_sources()
    pages = wiki.all_pages()
    data = {
        "created": created,
        "config": norm_key(wiki.config_path.relative_to(wiki.root)),
        "pages": len(pages),
        "sources": len(rows),
        "open_tasks": len(wiki.open_tasks()),
    }
    emit(
        "init", wiki.root,
        f"wiki ready at {WIKI_DIR}/ ({len(rows)} sources, {len(pages)} pages)",
        data,
        [f"created: {', '.join(created) if created else '(already present)'}"],
    )
    wiki.close()
    return 0


def command_ingest():
    require_health()
    wiki, _ = open_wiki(dry_run=ARGS.dry_run)
    wiki.cleanup_task_dirs()
    limit = int(wiki.config["ingest"].get("max_source_bytes", 50_000_000))
    force = ARGS.force
    selected = {norm_key(Path(value)) for value in ARGS.only} if ARGS.only else None
    files = [
        path for path in wiki.scan_files()
        if selected is None or norm_key(path.relative_to(wiki.scan_root)) in selected
    ]
    seen = set()
    fresh, unchanged, unsupported, oversize, failed, queued = [], [], [], [], [], []
    restored = []
    for path in files:
        key = norm_key(path.relative_to(wiki.scan_root))
        seen.add(key)
        row = wiki.source_row(key)
        try:
            stat = path.stat()
        except OSError as error:
            failed.append({"key": key, "message": str(error)})
            continue
        # 第一段初筛：只有 stat 与账本不符、强制重算、或原文缺失时才读源文件。
        # 其余情形（skip / bookkeep_raw）一次都不读，也不重算摘要。
        stamp_changed = row is None or not same_stamp(
            stat.st_size, stat.st_mtime, row["size"], row["mtime"]
        )
        if row is None:
            raw_state, raw_size, raw_mtime = "missing", 0, 0.0
        else:
            raw_state, raw_size, raw_mtime = raw_facts(wiki.root, row)
        digest = row["digest"] if row is not None else ""
        changed = False
        data = None
        if stamp_changed or force or raw_state == "missing":
            try:
                data = read_bytes(path, limit)
            except WikiError as error:
                oversize.append({"key": key, "message": error.message})
                if not ARGS.dry_run:
                    wiki.upsert_source(
                        path.relative_to(wiki.scan_root),
                        digest_text(key + str(stat.st_size)),
                        stat.st_size,
                        stat.st_mtime,
                        "", "", "oversize", "", 0, 0.0,
                    )
                continue
            except OSError as error:
                failed.append({"key": key, "message": str(error)})
                continue
            digest = digest_bytes(data)
            changed = row is None or row["digest"] != digest
        action = ingest_decision(changed, raw_state, stat.st_mtime, raw_mtime, force=force)
        if action in ("skip", "bookkeep_raw", "bookkeep_source"):
            # 不解析、不写原文：只把账本对齐到磁盘上的事实。
            if action == "skip" and row["status"] == "missing":
                wiki.set_source_status(key, "pending" if not _cites(wiki, key) else "done")
                restored.append(key)
                continue
            unchanged.append(key)
            if action == "skip" and not stamp_changed and same_stamp(
                raw_size, raw_mtime, row["raw_size"], row["raw_mtime"]
            ):
                continue
            if not ARGS.dry_run:
                raw_rel = row["raw"]
                raw_id = row["raw_id"]
                if action == "bookkeep_raw" and raw_rel:
                    # 人工排版过的原文：正文变了就换一行，last_parsed_digest 留旧值，
                    # 下一轮 ingest 据此认出「源没变、原文是人改的」，不回源覆盖。
                    raw_rel, raw_id = wiki.adopt_raw_edit(key, row["parser"])
                disk_digest, disk_size, disk_mtime = file_facts(wiki.root / raw_rel)
                wiki.upsert_source(
                    path.relative_to(wiki.scan_root), digest, stat.st_size, stat.st_mtime,
                    raw_rel, row["parser"], row["status"],
                    disk_digest, disk_size, disk_mtime,
                    raw_id=raw_id,
                    last_parsed_digest=row["last_parsed_digest"],
                )
                # 采纳用户改动后的原文即账本事实：仍在排队或租用的快照要对齐，
                # 否则同一份原文会被自己的任务判成漂移。
                if raw_rel:
                    wiki.pin_open_task_raw(
                        key, raw_rel, disk_digest, disk_size, disk_mtime
                    )
            continue
        text, parser = extract_text(path, data)
        if not text.strip():
            unsupported.append(
                {
                    "key": key,
                    "suffix": path.suffix.lower() or "(none)",
                    "reason": "no text backend produced content",
                    "hint": "install pdftotext or pypdf for PDFs; images need external OCR",
                }
            )
            if not ARGS.dry_run:
                wiki.upsert_source(
                    path.relative_to(wiki.scan_root), digest, len(data), stat.st_mtime,
                    "", "", "unsupported", "", 0, 0.0,
                )
            continue
        if len(text) > MAX_RAW_CHARS:
            text = text[:MAX_RAW_CHARS]
        if ARGS.dry_run:
            fresh.append(
                {
                    "key": key,
                    "raw": None,
                    "parser": parser,
                    "digest": digest[:12],
                    "size": len(data),
                    "chars": len(text),
                }
            )
            continue
        if action == "rewrite":
            raw_rel, raw_id = wiki.attach_raw(key, text, parser, path.name)
            last_parsed = raw_id
        else:
            # parse_keep_raw：保留用户那份原文，只刷新账本里的原文事实。
            raw_rel = row["raw"]
            raw_id = row["raw_id"]
            last_parsed = row["last_parsed_digest"]
        raw_digest, raw_size, raw_mtime = file_facts(wiki.root / raw_rel)
        wiki.upsert_source(path.relative_to(wiki.scan_root), digest, len(data), stat.st_mtime,
                           raw_rel, parser, "pending", raw_digest, raw_size, raw_mtime,
                           raw_id=raw_id, last_parsed_digest=last_parsed)
        fresh.append(
            {
                "key": key,
                "raw": raw_rel,
                "parser": parser,
                "digest": digest[:12],
                "size": len(data),
                "chars": len(text),
            }
        )
        if wiki.source_in_open_task(key) is None:
            queued.append(
                wiki.enqueue_task("route", key, {"sources": [wiki.source_snapshot(key)]})
            )
    vanished = []
    for row in wiki.all_sources():
        if row["key"] in seen or row["status"] == "missing":
            continue
        # --only 只看指定来源，未参与本轮扫描的来源不能被当成消失。
        if selected is not None and row["key"] not in selected:
            continue
        vanished.append(row["key"])
        if ARGS.dry_run:
            continue
        wiki.set_source_status(row["key"], "missing")
    if ARGS.dry_run:
        data = {
            "dry_run": True,
            "would_ingest": fresh,
            "unchanged": unchanged,
            "unsupported": unsupported,
            "oversize": oversize,
            "failed": failed,
            "vanished": vanished,
            "restored": restored,
        }
        emit(
            "ingest", wiki.report_root,
            f"dry run: {len(fresh)} new or changed, {len(unchanged)} unchanged, "
            f"{len(unsupported)} unsupported",
            data,
            [f"would ingest: {item['key']}" for item in fresh],
        )
        wiki.close()
        return 0
    # 失效不是放弃：任务因来源漂移被判 expired 后，来源仍是 pending，但旧任务已经不
    # 存在，下一轮 ingest 只会走「未变化」分支，不再入队——库会永远空转。这里补一次
    # 对账：pending 且没有未关闭任务的来源重新入队。没有 raw 的来源（超大、解析失败）
    # 不排队，它们本来就没有可提交的内容。
    for row in wiki.all_sources():
        if row["status"] != "pending" or not row["raw"]:
            continue
        if selected is not None and row["key"] not in selected:
            continue
        if wiki.source_in_open_task(row["key"]) is not None:
            continue
        queued.append(
            wiki.enqueue_task("route", row["key"], {"sources": [wiki.source_snapshot(row["key"])]})
        )
    # 最后一件事：没有任何来源指向的旧 raw 行连同文件一起回收，观看区不留孤儿副本。
    reclaimed = wiki.collect_raws()
    remaining = [row["key"] for row in wiki.all_sources() if row["status"] == "pending"]
    counts = wiki.task_counts()
    data = {
        "ingested": fresh,
        "unchanged": unchanged,
        "unsupported": unsupported,
        "oversize": oversize,
        "failed": failed,
        "vanished": vanished,
        "restored": restored,
        "queued_tasks": queued,
        "reclaimed": reclaimed,
        "pending_total": len(remaining),
        "pending": remaining,
        "queue": {
            "open": counts["open"], "queued": counts["queued"], "leased": counts["leased"],
            "route": counts["route"], "page": counts["page"],
        },
    }
    emit(
        "ingest", wiki.root,
        f"ingested {len(fresh)} sources, queued {len(queued)} route tasks; "
        f"{len(remaining)} pending, open queue {counts['open']}",
        data,
        [f"queued: {item}" for item in queued],
        ok=not (failed or oversize),
    )
    wiki.close()
    return 1 if (failed or oversize) else 0


def _cites(wiki, key):
    return any(key in record["meta"].get("sources", []) for record in wiki.all_pages())


def command_status():
    wiki, created = open_wiki()
    rows = wiki.all_sources()
    by_status = {}
    for row in rows:
        by_status[row["status"]] = by_status.get(row["status"], 0) + 1
    pages = wiki.all_pages()
    tasks = wiki.task_counts()
    stale = sorted(
        record["id"] for record in pages if record["meta"].get("status") == "stale"
    )
    pending = [row["key"] for row in rows if row["status"] == "pending"]
    if ARGS.brief:
        line = (
            f"{len(rows)} sources ({len(pending)} pending, {by_status.get('unsupported', 0)} unsupported, "
            f"{by_status.get('oversize', 0)} oversize), {len(pages)} pages ({len(stale)} stale), "
            f"{tasks['route']} route + {tasks['page']} page tasks open"
        )
        emit("status", wiki.root, line, {"brief": line}, [])
        wiki.close()
        return 0
    data = {
        "created": created,
        "sources": by_status,
        "source_total": len(rows),
        "pages": len(pages),
        "stale_pages": stale,
        "pending": pending,
        "tasks": {"open": tasks["open"], "queued": tasks["queued"],
                  "leased": tasks["leased"], "route": tasks["route"], "page": tasks["page"],
                  "expired": tasks["expired"], "accepted": tasks["accepted"]},
        "next": "run `next`" if tasks["queued"] else ("wait for leased tasks to expire" if tasks["leased"] else "run `ingest`"),
    }
    emit(
        "status", wiki.root,
        f"{len(rows)} sources ({len(pending)} pending), {len(pages)} pages, "
        f"{tasks['route']} route + {tasks['page']} page tasks",
        data,
        [f"pending: {key}" for key in pending] or ["pending: none"],
    )
    wiki.close()
    return 0


def command_next():
    require_health()
    wiki, _ = open_wiki()
    wiki.cleanup_task_dirs()
    batch = ARGS.batch or 1
    claimed, resumed = wiki.claim_tasks(kind=ARGS.kind, batch=max(1, batch))
    tasks = []
    lines = []
    for row in claimed + resumed:
        item = wiki.materialize_task(row)
        tasks.append(item)
        lines.append(f"{row['id']}  {item['brief']['goal']}")
    data = {"tasks": tasks, "count": len(tasks)}
    if not tasks:
        emit("next", wiki.root, "no queued tasks", data, [])
        wiki.close()
        return 0
    if len(resumed) == len(tasks):
        summary = (
            f"resuming {len(resumed)} unfinished task(s); keep the payload you already "
            "have and submit"
        )
    elif resumed:
        summary = (
            f"{len(tasks)} task(s) ready ({len(resumed)} unfinished resumed); read each "
            "brief, fill its payload.json, then submit"
        )
    else:
        summary = (
            f"{len(tasks)} task(s) ready; read each brief, fill its payload.json, then submit"
        )
    emit("next", wiki.root, summary, data, lines)
    wiki.close()
    return 0


def read_payload(wiki, row):
    """Read the submit payload: the task folder by default, an explicit path, or stdin."""
    if ARGS.payload == "-":
        raw = sys.stdin.read()
    else:
        if ARGS.payload:
            candidate = inside_root(
                wiki.root, ARGS.payload, "payload",
                "put payloads inside the workspace, or pass - for stdin",
            )
        else:
            candidate = wiki.task_dir(row["id"]) / "payload.json"
            if not candidate.exists():
                # 目录丢失（旧任务或被手工删除）时按当前任务重建，再读回骨架。
                wiki.materialize_task(row)
        try:
            raw = candidate.read_text(encoding="utf-8")
        except OSError as error:
            raise WikiError(
                "E_PAYLOAD", f"cannot read payload: {error}",
                f"fill {task_dir_relative(row['id'])}/payload.json, or pass --payload", 2,
            )
    try:
        return json.loads(raw)
    except json.JSONDecodeError as error:
        raise WikiError(
            "E_PAYLOAD", f"payload is not valid JSON: {error}",
            f"edit {task_dir_relative(row['id'])}/payload.json; `next --json` also returns the template", 2,
        )


def command_submit():
    require_health()
    wiki, _ = open_wiki()
    task_id = ARGS.id
    row = wiki.task_row(task_id)
    if not row:
        raise WikiError("E_TASK_UNKNOWN", f"no task {task_id}", "run `next` to get a task id", 2)
    if row["state"] == "accepted":
        raise WikiError("E_TASK_STALE", f"task {task_id} is already accepted",
                        "run `next` for new work", 3)
    # 领取阶段判出的漂移在 submit 时也报来源漂移，而不是笼统的任务过期。
    if row["state"] == "expired":
        raise WikiError(
            "E_SOURCE_DRIFT",
            f"task {task_id} was invalidated: {row['last_error'] or 'its source changed'}",
            "re-run `ingest`, then `next` for fresh work", 3,
        )
    if row["state"] != "leased":
        raise WikiError("E_TASK_STALE", f"task {task_id} is no longer ready",
                        "run `next` again to get fresh work", 3)
    if ARGS.epoch is not None and row["epoch"] != ARGS.epoch:
        raise WikiError(
            "E_TASK_STALE",
            f"task {task_id} changed after it was claimed",
            "run `next` again and submit the fresh task", 3,
        )
    if row["lease_until"] and parse_iso(row["lease_until"]) < datetime.now(timezone.utc):
        wiki.requeue_task(task_id, "lease expired")
        raise WikiError("E_TASK_STALE", f"task {task_id} expired before submit",
                        "run `next` again to get fresh work", 3)
    payload = read_payload(wiki, row)
    try:
        if row["kind"] == "route":
            result = submit_route(wiki, row, payload)
        else:
            result = submit_page(wiki, row, payload)
    except WikiError as error:
        # 只有仍在租约里的任务才回队；expired 是终态，不能被回队复活。
        fresh = wiki.task_row(task_id)
        if fresh and fresh["state"] == "leased":
            wiki.requeue_task(task_id, f"{error.code}: {error.message}")
        raise
    emit(
        "submit", wiki.root,
        result["summary"],
        {
            "task": task_id,
            "page": result.get("page"),
            "rev": result.get("rev"),
            "sources": result.get("sources", []),
            "queued_page_tasks": result.get("queued_page_tasks", []),
        },
        result.get("lines", []),
    )
    wiki.close()
    return 0


def submit_route(wiki, row, payload):
    inputs = json.loads(row["inputs"])
    available = [item["key"] for item in inputs["sources"]]
    decisions = payload.get("decisions") if isinstance(payload, dict) else None
    if not isinstance(decisions, list) or not decisions:
        raise WikiError(
            "E_PAYLOAD",
            "route payload must be {\"decisions\": [...]} with at least one decision per input source",
            "use the task template: at least one decision per input source",
            2,
        )
    seen = set()
    targets = set()
    new_ids = set()
    skipped = set()
    for decision in decisions:
        if not isinstance(decision, dict):
            raise WikiError("E_PAYLOAD", "each decision must be an object", "", 2)
        key = decision.get("source")
        if key not in available:
            raise WikiError("E_TASK_SCOPE", f"source {key!r} is not in this task",
                            f"task sources: {', '.join(available)}", 3)
        action = decision.get("action")
        if action not in ("new", "merge", "skip"):
            raise WikiError("E_PAYLOAD", f"action {action!r} must be new, merge or skip",
                            "pick one action per source", 2)
        if not str(decision.get("reason") or "").strip():
            raise WikiError("E_PAYLOAD", f"{key}: reason must not be empty",
                            "one sentence explaining the choice", 2)
        seen.add(key)
        if action == "skip":
            skipped.add(key)
            continue
        page_id = str(decision.get("page") or "").strip().lower()
        if not ID_RE.match(page_id):
            raise WikiError("E_BAD_ID", f"{key}: page id {page_id!r} is not a lowercase slug",
                            "use e.g. `filing-process`", 2)
        if (key, page_id) in targets:
            raise WikiError(
                "E_PAYLOAD",
                f"{key}: page {page_id} is decided twice in one payload",
                "one decision per source and page; split a source across different pages",
                2,
            )
        targets.add((key, page_id))
        if action == "new":
            if page_id in new_ids:
                raise WikiError(
                    "E_PAYLOAD",
                    f"page {page_id} is created by more than one decision",
                    "create a page once, then merge the other sources into it", 2,
                )
            new_ids.add(page_id)
    missing = [item for item in available if item not in seen]
    if missing:
        raise WikiError(
            "E_PAYLOAD",
            f"decisions must cover every input source; missing {sorted(missing)}",
            "submit at least one decision per source", 2,
        )
    conflicting = sorted(skipped & {key for key, _ in targets})
    if conflicting:
        raise WikiError(
            "E_PAYLOAD",
            f"source {conflicting[0]}: skip cannot be combined with new or merge",
            "skip a source on its own, or decide its pages without skip", 2,
        )
    # Validate every decision before touching the ledger, so a bad payload
    # never leaves a half-applied route behind.
    plan = []
    new_pages = {
        str(decision.get("page") or "").strip().lower()
        for decision in decisions if decision.get("action") == "new"
    }
    for decision in decisions:
        key = decision["source"]
        action = decision["action"]
        if action == "skip":
            plan.append({"action": "skip", "key": key})
            continue
        page_id = str(decision.get("page") or "").strip().lower()
        if not ID_RE.match(page_id):
            raise WikiError("E_BAD_ID", f"{key}: page id {page_id!r} is not a lowercase slug",
                            "use e.g. `filing-process`", 2)
        record = wiki.page_record(page_id)
        existing = wiki.open_task_for_page(page_id)
        if existing and existing["state"] == "leased":
            raise WikiError(
                "E_STATE", f"{key}: page {page_id} is being written by another task",
                "retry this route after that page task is submitted", 3,
            )
        if action == "new":
            if record:
                raise WikiError(
                    "E_PAGE_CONFLICT",
                    f"{key}: page {page_id} already exists",
                    "use action merge, or pick a different page id", 3,
                )
            title = str(decision.get("title") or "").strip()
            page_type = str(decision.get("type") or "").strip()
            if not title:
                raise WikiError("E_PAYLOAD", f"{key}: title is required for a new page", "", 2)
            if page_type not in wiki.config["schema"]["types"]:
                raise WikiError(
                    "E_PAYLOAD", f"{key}: type {page_type!r} not in schema.types",
                    f"choose one of: {', '.join(wiki.config['schema']['types'])}", 2,
                )
            plan.append({"action": "new", "key": key, "page": page_id,
                         "title": title, "type": page_type})
        else:
            pending_create = bool(
                existing and json.loads(existing["inputs"]).get("mode") == "create"
            )
            if not record and not pending_create and page_id not in new_pages:
                raise WikiError(
                    "E_PAGE_UNKNOWN",
                    f"{key}: page {page_id} does not exist",
                    "use action new, or check `next --json` page_index and pending_pages", 3,
                )
            plan.append({"action": "merge", "key": key, "page": page_id,
                         "rev": record["rev"] if record else None})
    # A page created in this same payload must be queued before sources merge
    # into it; otherwise the merge would create an update task for a missing page.
    plan.sort(key=lambda item: 0 if item["action"] == "new" else 1)
    page_tasks = []
    for item in plan:
        key = item["key"]
        if item["action"] == "skip":
            wiki.set_source_status(key, "skipped")
            continue
        entry = wiki.source_snapshot(key)
        existing = wiki.open_task_for_page(item["page"])
        if existing:
            wiki.attach_page_source(existing["id"], key)
            page_tasks.append(existing["id"])
            continue
        inputs = {
            "page": item["page"],
            "base_rev": None if item["action"] == "new" else item["rev"],
            "sources": [entry],
        }
        if item["action"] == "new":
            inputs.update({"mode": "create", "title": item["title"], "type": item["type"],
                           "route_task": row["id"]})
        else:
            inputs["mode"] = "update"
            inputs["route_task"] = row["id"]
        task_id = wiki.enqueue_task("page", item["page"], inputs)
        # 唯一索引可能把并发插入收敛到同一条任务，补挂本次来源（幂等）。
        wiki.attach_page_source(task_id, key)
        page_tasks.append(task_id)
    wiki.close_task(row["id"], payload, row["epoch"])
    return {
        "kind": "route",
        "summary": f"route {row['id']} accepted; {len(set(page_tasks))} page task(s) queued",
        "sources": available,
        "queued_page_tasks": sorted(set(page_tasks)),
        "lines": [f"page task: {task_id}" for task_id in sorted(set(page_tasks))],
    }


def submit_page(wiki, row, payload):
    inputs = json.loads(row["inputs"])
    reason = wiki.task_drift(row)
    if reason:
        # 提交时才发现失效：置 expired 并报来源漂移，不回队。
        wiki.expire_task(row["id"], reason)
        raise WikiError(
            "E_SOURCE_DRIFT",
            f"task {row['id']} no longer matches its source snapshot: {reason}",
            "re-run `ingest`, then `next` for fresh work",
            3,
        )
    page_id = inputs["page"]
    sources = inputs["sources"]
    for item in sources:
        current = wiki.source_row(item["key"])
        if current is None or current["status"] == "missing":
            raise WikiError("E_SOURCE_DRIFT", f"source {item['key']} vanished",
                            "re-run `ingest`, then `next`", 3)
        if current["digest"] != item["source_digest"]:
            raise WikiError(
                "E_SOURCE_DRIFT", f"source {item['key']} changed after the task was queued",
                "re-read the source and take a fresh task", 3,
            )
    record = wiki.page_record(page_id)
    if inputs.get("mode") == "create" and record:
        raise WikiError("E_PAGE_CONFLICT", f"page {page_id} already exists",
                        "take the update task instead", 3)
    if inputs.get("mode") == "update":
        if not record:
            raise WikiError("E_PAGE_GONE", f"page {page_id} disappeared",
                            "run `next` again to rebuild the task", 3)
        if record["rev"] != inputs.get("base_rev"):
            raise WikiError(
                "E_PAGE_CONFLICT",
                f"page {page_id} is now at rev {record['rev']}, the task started from rev {inputs.get('base_rev')}",
                "run `next` again and rewrite against the current revision", 3,
            )
    meta, body, evidence, boundaries = validate_page_payload(wiki, row, payload, sources)
    rev = wiki.write_rev(page_id, meta, body, "submit")
    wiki.close_task(row["id"], payload, row["epoch"])
    # 一个来源可能拆到多页：最后一个相关 page 任务关闭后才算写完。
    for item in sources:
        if wiki.source_in_open_task(item["key"]) is None:
            wiki.set_source_status(item["key"], "done")
    return {
        "kind": "page",
        "page": page_id,
        "rev": rev,
        "sources": meta["sources"],
        "summary": f"page {page_id} accepted at rev {rev} ({len(meta['sources'])} sources)",
        "lines": [f"page: {page_id} rev {rev}"],
    }


def command_release():
    wiki, _ = open_wiki()
    row = wiki.task_row(ARGS.id)
    if not row:
        raise WikiError("E_TASK_UNKNOWN", f"no task {ARGS.id}", "run `next`", 2)
    wiki.release_task(ARGS.id)
    emit("release", wiki.root, f"released {ARGS.id}", {"task": ARGS.id}, [])
    wiki.close()
    return 0


def command_read():
    wiki, _ = open_wiki()
    ref = ARGS.ref
    meta = {}
    if ref.startswith("page:"):
        page_id = ref[5:]
        record = wiki.page_record(page_id)
        if not record:
            raise WikiError("E_NOT_FOUND", f"no page {page_id}", "run `query` to find the page id", 3)
        meta = record["meta"]
        body = record["body"]
        location = wiki.page_ref(record)
    else:
        path = resolve_ref(wiki, ref)
        text = path.read_text(encoding="utf-8", errors="replace")
        body = text
        # frontmatter 是文本属性，不是路径属性：原文搬到观看区 raw/ 后，不能再靠 .wiki 判定。
        if path.suffix == ".md" and text.startswith("---"):
            meta, body = split_frontmatter(text)
        location = norm_key(path.relative_to(wiki.root)) if window_safe(wiki.root, path) else str(path)
    lines = body.splitlines()
    offset = max(1, ARGS.offset)
    limit = max(1, min(ARGS.limit, 2000))
    window = lines[offset - 1: offset - 1 + limit]
    next_offset = offset + len(window) if offset - 1 + len(window) < len(lines) else None
    data = {
        "path": location,
        "offset": offset,
        "limit": limit,
        "total_lines": len(lines),
        "next_offset": next_offset,
        "meta": dump_meta(meta),
        "content": "\n".join(window),
    }
    emit(
        "read", wiki.root,
        f"{data['path']} lines {offset}-{offset + len(window) - 1} of {len(lines)}",
        data,
        [line for line in window],
    )
    wiki.close()
    return 0


def resolve_ref(wiki, ref):
    if ref.startswith("raw:"):
        key = ref[4:]
        row = wiki.source_row(key)
        if not row or not row["raw"]:
            raise WikiError(
                "E_NOT_FOUND", f"no raw text for {key}",
                "unsupported or oversize sources keep no raw text; run `status`", 3,
            )
        return wiki.root / row["raw"]
    candidate = (wiki.root / ref).resolve()
    if not window_safe(wiki.root, candidate):
        raise WikiError("E_PATH", "ref escapes the workspace root",
                        "use a path inside the workspace", 2)
    if not candidate.exists():
        raise WikiError("E_NOT_FOUND", f"no file {ref}", "check the path, or use `query` first", 3)
    return candidate


def command_query():
    wiki, _ = open_wiki()
    if ARGS.page:
        record = wiki.page_record(ARGS.page)
        if not record:
            raise WikiError("E_NOT_FOUND", f"no page {ARGS.page}", "search with `query <text>` first", 3)
        outgoing, incoming = wiki.links_for(ARGS.page)
        data = {
            "page": dump_meta(record["meta"]),
            "rev": record["rev"],
            "path": wiki.page_ref(record),
            "outgoing": outgoing,
            "incoming": incoming,
            "body": record["body"],
        }
        emit(
            "query", wiki.root,
            f"page {ARGS.page} rev {record['rev']}: {len(outgoing)} outgoing, {len(incoming)} incoming links",
            data,
            [f"outgoing: {', '.join(outgoing) or '(none)'}",
             f"incoming: {', '.join(incoming) or '(none)'}",
             "",
             record["body"]],
        )
        wiki.close()
        return 0
    terms = [term for term in re.split(r"\s+", (ARGS.text or "").strip()) if term]
    if not terms:
        raise WikiError("E_USAGE", "query needs text or --page", "example: query 反诈 复机", 2)
    scored, results = search.search_pages(wiki, terms, ARGS.limit)
    topic_hits = topics.match(wiki.root, terms, ARGS.limit)
    data = {
        "query": " ".join(terms),
        "matches": len(scored),
        "returned": len(results),
        "results": results,
        "topic_matches": len(topic_hits),
        "topics": topic_hits,
        "boundary": "hit count is a ranked subset, not proof of full coverage",
    }
    sections = []
    pages = [
        f"{item['id']}  [{item['type']}/{item['status']}]  {item['title']}\n    {item['snippet']}"
        for item in results
    ]
    if pages:
        sections.append(pages)
    if topic_hits:
        block = ["topics (read before exploring the same theme again):"]
        for item in topic_hits:
            block.append(f"{item['name']}  [{item['status'] or 'open'}]  {item['description']}")
            block.append(f"    path: {item['path']}  read: {item['read']}")
        sections.append(block)
    lines = []
    for block in sections:
        if lines:
            lines.append("")
        lines.extend(block)
    summary = f"{len(scored)} matching pages"
    if topic_hits:
        summary += f", {len(topic_hits)} matching topics"
    summary += f", showing {len(results)} pages for {' '.join(terms)}"
    emit("query", wiki.root, summary, data, lines)
    wiki.close()
    return 0


def command_scenario():
    workspace = Path.cwd().resolve()
    root = Path(ARGS.root).resolve() if ARGS.root else workspace
    if not window_safe(workspace, root):
        raise WikiError(
            "E_PATH", "scenario root escapes the workspace",
            "run from the workspace, or pass --root inside it", 2,
        )
    if ARGS.scenario_command == "search":
        terms = [term.strip() for term in ARGS.text if term.strip()]
        if not terms:
            raise WikiError(
                "E_USAGE", "scenario search needs text",
                'example: scenario search "开户"', 2,
            )
        hits = scenarios.match(root, terms, ARGS.limit)
        data = {"query": " ".join(terms), "matches": len(hits), "scenarios": hits}
        lines = []
        for item in hits:
            alias = f"（{item['alias_name']}）" if item["alias_name"] else ""
            lines.append(f"{item['id']}{alias}  {item['description']}")
            lines.append(f"    path: {item['path']}  load: {item['load']}")
        emit("scenario search", root, f"{len(hits)} matching scenarios", data, lines)
        return 0
    if ARGS.scenario_command == "list":
        data = scenarios.inventory(root)
        summary = f"{data['valid']} valid, {data['count'] - data['valid']} invalid scenarios"
        lines = []
        for item in data["scenarios"]:
            state = "ok" if item["valid"] else f"invalid: {item['error']}"
            alias = f"（{item['alias_name']}）" if item["alias_name"] else ""
            lines.append(f"{item['id']}{alias}  [{state}]  {item['description']}")
            lines.append(f"    path: {item['path']}  resources: {len(item['resources'])}")
        emit("scenario list", root, summary, data, lines)
        return 0
    data = scenarios.load(root, ARGS.id)
    emit(
        "scenario load", root,
        f"scenario {data['scenario']['id']}: {len(data['resources'])} resources",
        data,
        [f"path: {data['path']}", f"description: {data['scenario']['description']}", "", data["content"]],
    )
    return 0


def command_page_import():
    wiki, _ = open_wiki()
    page_id = ARGS.id.strip().lower()
    record = wiki.page_record(page_id)
    if not record:
        raise WikiError("E_NOT_FOUND", f"no page {page_id}", "import only edits existing pages", 3)
    try:
        source = inside_root(
            wiki.root, ARGS.from_path, "page import source",
            "put the edited page under .wiki/drafts/",
        )
        text = source.read_text(encoding="utf-8")
    except OSError as error:
        raise WikiError("E_PAYLOAD", f"cannot read {ARGS.from_path}: {error}", "", 2)
    meta, body = split_frontmatter(text)
    merged = dict(record["meta"])
    for field in ("title", "type", "summary"):
        if meta.get(field):
            merged[field] = str(meta[field])
    if meta.get("sources"):
        merged["sources"] = [str(item) for item in meta["sources"]]
    for key in merged["sources"]:
        if not wiki.source_row(key):
            raise WikiError("E_UNKNOWN_SOURCE", f"page cites unregistered source {key}",
                            "run `ingest` first, or fix the source list", 2)
    if not body.strip():
        raise WikiError("E_PAYLOAD", "imported page has no body", "", 2)
    if merged.get("type") not in wiki.config["schema"]["types"]:
        raise WikiError("E_PAYLOAD", f"type {merged.get('type')!r} not in schema.types", "", 2)
    rev = wiki.write_rev(page_id, merged, body, "import")
    emit(
        "page import", wiki.root,
        f"imported {page_id} at rev {rev}",
        {"page": page_id, "rev": rev, "path": wiki.page_ref(wiki.page_record(page_id))},
        [f"page: {page_id} rev {rev}"],
    )
    wiki.close()
    return 0


def command_page_restore():
    wiki, _ = open_wiki()
    page_id = ARGS.id.strip().lower()
    row = wiki.conn.execute(
        "SELECT * FROM page_revs WHERE page = ? AND rev = ?", (page_id, ARGS.rev)
    ).fetchone()
    if not row:
        raise WikiError("E_NOT_FOUND", f"no rev {ARGS.rev} for page {page_id}",
                        "`query --page` shows the current revision", 3)
    meta = json.loads(row["meta"])
    rev = wiki.write_rev(page_id, meta, row["body"], "restore")
    emit(
        "page restore", wiki.root,
        f"restored {page_id} to rev {ARGS.rev} as rev {rev}",
        {"page": page_id, "rev": rev, "restored_from": ARGS.rev},
        [f"page: {page_id} rev {rev}"],
    )
    wiki.close()
    return 0


def command_sync():
    """Take hand edits back from the view dir (R1-R9)."""
    # R11：先看磁盘，再让 ensure 的推送动手；否则挪走的文件会被重建出第二个副本。
    wiki, _ = open_wiki(push=False)
    pages_dir = wiki.publish_dir / "pages"
    base = wiki.publish_dir.resolve()
    imported, unchanged, written = [], [], []
    skipped, failed, warnings = [], [], []
    candidates = []
    paths = sorted(pages_dir.rglob("*.md")) if pages_dir.is_dir() else []
    for path in paths:
        rel = norm_key(path.relative_to(wiki.publish_dir))
        if not window_safe(base, path.resolve()):
            skipped.append({"file": rel, "reason": "escapes the publish dir"})
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as error:
            failed.append({"file": rel, "page": "", "code": "E_IO", "message": str(error)})
            continue
        meta, body = split_frontmatter(text)
        page_id = str(meta.get("id") or "").strip().lower()
        if not page_id:
            skipped.append({"file": rel, "reason": "no id"})
            continue
        record = wiki.page_record(page_id)
        if not record:
            skipped.append({"file": rel, "reason": "unknown id"})
            continue
        candidates.append({
            "file": rel, "path": path, "text": text, "meta": meta,
            "body": body, "page": page_id, "record": record,
        })
    groups = {}
    for item in candidates:
        groups.setdefault(item["page"], []).append(item)
    targets = wiki._view_targets(wiki.all_pages())
    for page_id in sorted(groups):
        items = groups[page_id]
        if len(items) > 1:
            names = ", ".join(sorted(item["file"] for item in items))
            for item in items:
                failed.append({
                    "file": item["file"], "page": page_id, "code": "E_DUPLICATE",
                    "message": f"id {page_id} is used by {len(items)} files: {names}",
                })
            continue
        item = items[0]
        try:
            bucket = sync_view_file(wiki, item, targets[page_id], ARGS.body_only,
                                    warnings=warnings)
        except (WikiError, OSError, UnicodeDecodeError) as error:
            if isinstance(error, WikiError):
                code, message = error.code, error.message
            else:
                code, message = "E_IO", str(error)
            failed.append({"file": item["file"], "page": page_id,
                           "code": code, "message": message})
            continue
        if bucket == "imported":
            imported.append(page_id)
        elif bucket == "written":
            written.append({"page": page_id, "from": item["file"], "to": targets[page_id]})
        else:
            unchanged.append(page_id)
    imported.sort()
    unchanged.sort()
    skipped.sort(key=lambda item: item["file"])
    failed.sort(key=lambda item: item["file"])
    written.sort(key=lambda item: (item["page"], item["from"]))
    data = {"imported": imported, "skipped": skipped, "failed": failed,
            "unchanged": unchanged, "written": written,
            "warnings": sorted(warnings)}
    summary = (f"sync: {len(imported)} imported, {len(skipped)} skipped, "
               f"{len(failed)} failed, {len(unchanged)} unchanged, {len(written)} written")
    lines = [f"imported: {page_id}" for page_id in imported]
    lines += [f"written: {item['page']} {item['from']} -> {item['to']}" for item in written]
    lines += [f"failed: {item['file']} [{item['code']}] {item['message']}" for item in failed]
    emit("sync", wiki.root, summary, data, lines, ok=not failed)
    wiki.close()
    return 1 if failed else 0


def command_export():
    wiki, _ = open_wiki()
    target = inside_root(
        wiki.root, ARGS.dir, "export target",
        "export into a directory inside the workspace, e.g. .wiki/export",
    )
    written = []
    for record in wiki.all_pages():
        meta = dict(record["meta"])
        meta["status"] = wiki.computed_status(meta)
        destination = target / meta["type"] / f"{record['id']}.md"
        write_atomic(destination, render_page(meta, record["body"]))
        written.append(norm_key(destination.relative_to(target)))
    emit(
        "export", wiki.root,
        f"exported {len(written)} pages to {target}",
        {"target": str(target), "files": written},
        written,
    )
    wiki.close()
    return 0


def command_doctor():
    wiki, _ = open_wiki()
    issues, fixed = [], []
    try:
        integrity = wiki.conn.execute("PRAGMA integrity_check").fetchone()[0]
    except sqlite3.DatabaseError as error:
        raise WikiError("E_DB", f"database unreadable: {error}",
                        "restore from backup; do not edit .wiki/wiki.db", 1)
    if integrity != "ok":
        issues.append({"severity": "fail", "code": "L1", "message": f"database integrity: {integrity}"})
    pages = wiki.all_pages()
    known = {record["id"] for record in pages}
    for record in pages:
        meta = record["meta"]
        if meta.get("status") == "stale":
            issues.append({"severity": "warn", "code": "L3",
                           "message": f"{record['id']} cites changed sources"})
        for key in meta.get("sources") or []:
            if not wiki.source_row(key):
                issues.append({"severity": "warn", "code": "L4",
                               "message": f"{record['id']} cites unregistered source {key}"})
    broken = []
    for row in wiki.conn.execute("SELECT src, dst FROM links"):
        if row["dst"] not in known and row["src"] in known:
            broken.append(f"{row['src']} -> {row['dst']}")
    if broken:
        issues.append({"severity": "warn", "code": "L5",
                       "message": f"broken links: {', '.join(sorted(broken)[:10])}"})
    stuck = wiki.conn.execute(
        "SELECT id FROM tasks WHERE state = 'leased' AND lease_until < ?", (now_iso(),)
    ).fetchall()
    if stuck:
        issues.append({"severity": "warn", "code": "L6",
                       "message": f"expired leases: {', '.join(row['id'] for row in stuck)}"})
    if ARGS.fix:
        wiki.rebuild_all_links()
        fixed.append("links rebuilt")
    data = {"issues": issues, "fixed": fixed, "healthy": not issues}
    emit(
        "doctor", wiki.root,
        f"{len(issues)} issues, {len(fixed)} repairs" if (issues or fixed) else "healthy",
        data,
        [f"{item['severity']}: {item['code']} {item['message']}" for item in issues],
    )
    wiki.close()
    return 1 if any(item["severity"] == "fail" for item in issues) else 0


def command_version():
    print(f"llm-wiki {VERSION}")
    return 0


def build_parser():
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--root", help="wiki root inside the workspace (default: current directory)")
    common.add_argument("--json", action="store_true", help="machine-readable output")
    parser = argparse.ArgumentParser(
        prog="wiki.py",
        description="Portable wiki engine: task-driven ingestion, ledger-owned pages.",
    )
    parser.add_argument("--version", action="version", version=f"llm-wiki {VERSION}")
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("check", parents=[common], help="verify the installed skill and workspace")
    sub.add_parser("init", parents=[common], help="create .wiki/ and report state")
    ingest = sub.add_parser("ingest", parents=[common], help="scan sources and queue route tasks")
    ingest.add_argument("--dry-run", action="store_true")
    ingest.add_argument("--force", action="store_true", help="reparse even when the digest is unchanged")
    ingest.add_argument("--only", action="append", metavar="PATH",
                        help="limit ingest to these sources (repeatable)")
    next_p = sub.add_parser("next", parents=[common], help="claim queued tasks")
    next_p.add_argument("--kind", choices=["route", "page"])
    next_p.add_argument("--batch", type=int, default=0, help="tasks to claim (default: 1)")
    submit = sub.add_parser("submit", parents=[common], help="submit a task payload")
    submit.add_argument("id")
    submit.add_argument("--epoch", type=int, help=argparse.SUPPRESS)
    submit.add_argument("--payload", help="JSON file or - for stdin; defaults to the task folder")
    release = sub.add_parser("release", parents=[common], help="return a task to the queue")
    release.add_argument("id")
    read = sub.add_parser("read", parents=[common], help="read raw text or a page by ref")
    read.add_argument("ref")
    read.add_argument("--offset", type=int, default=1)
    read.add_argument("--limit", type=int, default=200)
    query = sub.add_parser("query", parents=[common], help="search pages and topics, or inspect one page")
    query.add_argument("text", nargs="?")
    query.add_argument("--page")
    query.add_argument("--limit", type=int, default=8)
    scenario = sub.add_parser("scenario", help="inspect scenario capabilities")
    scenario_sub = scenario.add_subparsers(dest="scenario_command", required=True)
    scenario_sub.add_parser("list", parents=[common], help="list scenario capabilities and validity")
    search_p = scenario_sub.add_parser("search", parents=[common], help="explicitly search scenario capabilities")
    search_p.add_argument("text", nargs="*")
    search_p.add_argument("--limit", type=int, default=5)
    load_p = scenario_sub.add_parser("load", parents=[common], help="load one scenario flow and its resources")
    load_p.add_argument("id")
    page = sub.add_parser("page", help="human edits to an existing page")
    page_sub = page.add_subparsers(dest="page_command", required=True)
    import_p = page_sub.add_parser("import", parents=[common], help="adopt a hand-edited page file")
    import_p.add_argument("id")
    import_p.add_argument("--from", dest="from_path", required=True)
    restore = page_sub.add_parser("restore", parents=[common], help="restore an older revision")
    restore.add_argument("id")
    restore.add_argument("--rev", type=int, required=True)
    export = sub.add_parser("export", parents=[common], help="export rendered pages to a directory")
    export.add_argument("dir")
    status = sub.add_parser("status", parents=[common], help="ledger, pages, tasks")
    status.add_argument("--brief", action="store_true")
    sync = sub.add_parser("sync", parents=[common], help="take hand edits back from the publish dir")
    sync.add_argument("--body-only", action="store_true",
                      help="absorb the body and evidence, keep the ledger metadata")
    doctor = sub.add_parser("doctor", parents=[common], help="health checks; --fix repairs mechanical issues")
    doctor.add_argument("--fix", action="store_true")
    sub.add_parser("version", parents=[common], help="print the engine version")
    return parser


ARGS = None
COMMANDS = {
    "check": command_check,
    "init": command_init,
    "ingest": command_ingest,
    "next": command_next,
    "submit": command_submit,
    "release": command_release,
    "read": command_read,
    "query": command_query,
    "export": command_export,
    "status": command_status,
    "sync": command_sync,
    "doctor": command_doctor,
    "version": command_version,
}

# 会打开账本或改观看区的命令都排队执行；check/version/scenario 只读包与场景目录。
LOCKED_COMMANDS = frozenset({
    "init", "ingest", "next", "submit", "release", "read", "query",
    "export", "status", "sync", "doctor", "page import", "page restore",
})


def main(argv=None):
    global ARGS
    parser = build_parser()
    ARGS = parser.parse_args(argv)
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
        sys.stderr.reconfigure(encoding="utf-8")
    root = Path(ARGS.root).resolve() if ARGS.root else Path.cwd()
    command = ARGS.command
    if command == "page":
        command = f"page {ARGS.page_command}"
        handler = (
            command_page_import if ARGS.page_command == "import" else command_page_restore
        )
    elif command == "scenario":
        command = f"scenario {ARGS.scenario_command}"
        handler = command_scenario
    else:
        handler = COMMANDS[command]
    try:
        # 试运行不得创建 `.wiki/`，此时也没有可争用的写入目标，直接跳过排队。
        dry_only = command == "ingest" and ARGS.dry_run and not (root / WIKI_DIR).exists()
        if command in LOCKED_COMMANDS and not dry_only:
            with workspace_lock(root):
                return handler()
        return handler()
    except WikiError as error:
        return fail(command, root, error)
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main())