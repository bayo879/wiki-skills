#!/usr/bin/env python3
"""Portable wiki engine.

One directory of source files in, one deterministic `.wiki/` state tree out.
Only the Python standard library is required.

The engine owns the ledger: scanning, parsing, hashing, task state, validation,
page revisions, and the automatic page mirror. Semantic work -- deciding which
pages exist, their boundaries, their prose, and which quotes support them --
belongs to the agent, which submits JSON payloads and never writes `.wiki/`.

Commands are documented by `wiki.py --help` and in `../references/cli.md`.
"""

from __future__ import annotations

import argparse
import atexit
import hashlib
import html
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import zipfile
from datetime import date, datetime, timezone
from html.parser import HTMLParser
from pathlib import Path

VERSION = "0.2.0"
WIKI_DIR = ".wiki"
RAW_SUBDIR = "raw"
PAGES_SUBDIR = "pages"
DRIFT_SUBDIR = "drift"
DB_NAME = "wiki.db"
CONFIG_NAME = "wiki.json"
MAX_RAW_CHARS = 4_000_000
BODY_MAX_CHARS = 60_000
SUMMARY_MAX_CHARS = 200
TASK_LEASE_SECONDS = 900
DEFAULT_BATCH = 20
TASK_OPEN_STATES = ("queued", "leased")

DEFAULT_CONFIG = {
    "schema": {"types": ["concept", "entity", "process", "note"]},
    "scan": {
        "ignore": [
            ".git", ".hg", ".svn", ".wiki", "node_modules", "target", "dist",
            "build", "vendor", ".venv", "venv", "__pycache__", ".idea", ".vscode",
        ]
    },
    "ingest": {"max_source_bytes": 50_000_000, "batch": DEFAULT_BATCH},
}

TEXT_SUFFIXES = {
    ".md", ".markdown", ".txt", ".rst", ".log", ".csv", ".tsv", ".yaml", ".yml",
    ".toml", ".ini", ".cfg", ".json", ".xml", ".sql", ".py", ".rs", ".js", ".ts",
    ".css", ".sh", ".java", ".go", ".rb", ".c", ".h", ".cpp",
}
HTML_SUFFIXES = {".html", ".htm"}
ID_RE = re.compile(r"^[a-z0-9][a-z0-9._-]*$")
LINK_RE = re.compile(r"\[\[([^\]|#]+)(?:#[^\]|]*)?(?:\|[^\]]*)?\]\]")
TITLE_RE = re.compile(r"^#\s+(.+)$", re.MULTILINE)


class WikiError(Exception):
    """A failure with a stable code, an explanation, and a next action."""

    def __init__(self, code, message, next_step="", exit_code=1):
        super().__init__(message)
        self.code = code
        self.message = message
        self.next_step = next_step
        self.exit_code = exit_code


class _TextExtractor(HTMLParser):
    """Turn HTML into readable text: keep block boundaries, drop scripts."""

    BLOCK_TAGS = {
        "p", "div", "br", "li", "tr", "h1", "h2", "h3", "h4", "h5", "h6",
        "section", "article", "header", "footer", "table", "blockquote", "pre",
    }
    SKIP_TAGS = {"script", "style", "noscript", "template"}

    def __init__(self):
        super().__init__(convert_charrefs=True)
        self.parts = []
        self.depth = 0

    def handle_starttag(self, tag, attrs):
        if tag in self.SKIP_TAGS:
            self.depth += 1
        elif tag in self.BLOCK_TAGS:
            self.parts.append("\n")
        elif tag == "td" or tag == "th":
            self.parts.append("\t")

    def handle_endtag(self, tag):
        if tag in self.SKIP_TAGS and self.depth > 0:
            self.depth -= 1
        elif tag in self.BLOCK_TAGS:
            self.parts.append("\n")

    def handle_data(self, data):
        if self.depth == 0:
            self.parts.append(data)


def norm_key(path):
    """Repo-relative POSIX key, stable across platforms."""
    return path.as_posix()


def digest_bytes(data):
    return hashlib.blake2b(data, digest_size=16).hexdigest()


def digest_text(text):
    return digest_bytes(text.encode("utf-8", "replace"))


def now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_iso(value):
    return datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)


def read_bytes(path, limit):
    size = path.stat().st_size
    if limit and size > limit:
        raise WikiError(
            "E_SOURCE_TOO_LARGE",
            f"{path.name} is {size} bytes, above ingest.max_source_bytes={limit}",
            "raise ingest.max_source_bytes, or move this file outside the wiki root",
            2,
        )
    return path.read_bytes()


def decode_bytes(data):
    for encoding in ("utf-8", "utf-8-sig", "gb18030", "latin-1"):
        try:
            return data.decode(encoding), encoding
        except UnicodeDecodeError:
            continue
    return data.decode("utf-8", "replace"), "utf-8"


def html_to_text(data):
    _, encoding = decode_bytes(data)
    parser = _TextExtractor()
    parser.feed(data.decode(encoding, "replace"))
    parser.close()
    return re.sub(r"\n{3,}", "\n\n", "".join(parser.parts)).strip()


def docx_to_text(path):
    with zipfile.ZipFile(path) as archive:
        xml = archive.read("word/document.xml").decode("utf-8", "replace")
    xml = xml.replace("</w:p>", "\n").replace("<w:tab/>", "\t")
    return html.unescape(re.sub(r"<[^>]+>", "", xml)).strip()


def pdf_to_text(path):
    """Best effort, in order: pdftotext, then pypdf. No silent fake success."""
    tool = shutil.which("pdftotext")
    if tool:
        try:
            completed = subprocess.run(
                [tool, "-layout", "-q", str(path), "-"],
                capture_output=True,
                timeout=180,
                check=False,
            )
        except (subprocess.TimeoutExpired, OSError):
            completed = None
        if completed is not None and completed.returncode == 0:
            text, _ = decode_bytes(completed.stdout)
            if text.strip():
                return text, "pdftotext"
    try:
        from pypdf import PdfReader  # optional dependency
    except ImportError:
        PdfReader = None
    if PdfReader is not None:
        reader = PdfReader(str(path))
        pages = [page.extract_text() or "" for page in reader.pages]
        text = "\n\n".join(pages).strip()
        if text:
            return text, "pypdf"
    return "", ""


def extract_text(path, data):
    """Return (text, parser_name). Empty text means the file is unsupported."""
    suffix = path.suffix.lower()
    if suffix == ".pdf":
        text, parser = pdf_to_text(path)
        return text or "", parser
    if suffix == ".docx":
        return docx_to_text(path), "docx"
    if suffix in HTML_SUFFIXES:
        return html_to_text(data), "html"
    if suffix in TEXT_SUFFIXES or not suffix:
        text, _ = decode_bytes(data)
        return text, "text"
    return "", ""


def split_frontmatter(text):
    """Split a page into (metadata dict, body). Tolerant of missing fields."""
    if not text.startswith("---"):
        return {}, text
    lines = text.splitlines()
    if lines[0].strip() != "---":
        return {}, text
    meta = {}
    end = None
    current_list = None
    for index in range(1, len(lines)):
        line = lines[index]
        if line.strip() == "---":
            end = index
            break
        item = re.match(r"^([A-Za-z_][\w-]*):\s*(.*)$", line)
        if item:
            key, value = item.group(1), item.group(2).strip()
            current_list = None
            if value == "":
                meta[key] = []
                current_list = key
            else:
                meta[key] = unquote_scalar(value)
            continue
        entry = re.match(r"^\s+-\s*(.*)$", line)
        if entry and current_list:
            meta[current_list].append(unquote_scalar(entry.group(1).strip()))
    if end is None:
        return {}, text
    return meta, "\n".join(lines[end + 1:]).lstrip("\n")


def unquote_scalar(value):
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        try:
            return json.loads(value) if value[0] == '"' else value[1:-1]
        except json.JSONDecodeError:
            return value[1:-1]
    return value


def yaml_scalar(value):
    return json.dumps(str(value), ensure_ascii=False)


def render_page(meta, body):
    """Deterministic mirror text: frontmatter, body, then the evidence block."""
    lines = ["---"]
    for field in ("id", "title", "type", "summary"):
        lines.append(f"{field}: {yaml_scalar(meta[field])}")
    lines.append("sources:")
    for source in meta["sources"]:
        lines.append(f"  - {yaml_scalar(source)}")
    lines.append(f"status: {yaml_scalar(meta['status'])}")
    lines.append(f"updated: {yaml_scalar(meta['updated'])}")
    lines.append("---")
    lines.append("")
    text = "\n".join(lines) + body.rstrip() + "\n"
    evidence = meta.get("evidence") or []
    if evidence:
        rows = ["", "## 证据", ""]
        for item in evidence:
            rows.append(f"- `{item['source']}`：{item['quote']}")
        text += "\n".join(rows) + "\n"
    return text


def dump_meta(meta):
    """Frontmatter values as JSON-friendly types for command output."""
    clean = {}
    for key, value in meta.items():
        if isinstance(value, list):
            clean[key] = [item if isinstance(item, (dict, list)) else str(item) for item in value]
        elif isinstance(value, dict):
            clean[key] = value
        else:
            clean[key] = str(value)
    return clean


def key2file(key):
    """Readable, filesystem-safe raw filename for an arbitrary source key."""
    base = key.rsplit("/", 1)[-1]
    base = re.sub(r"\.[A-Za-z0-9]+$", "", base)
    safe = re.sub(r"[^\w.-]+", "-", base, flags=re.UNICODE).strip("-._")
    return f"{safe[:80] or 'source'}-{digest_text(key)[:8]}"


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


def write_atomic(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    handle, temp_name = tempfile.mkstemp(dir=str(path.parent), prefix=".tmp-", suffix=path.suffix)
    try:
        with os.fdopen(handle, "w", encoding="utf-8") as stream:
            stream.write(text)
        os.replace(temp_name, path)
    except BaseException:
        try:
            os.unlink(temp_name)
        except OSError:
            pass
        raise


class Wiki:
    """The knowledge base rooted at one directory."""

    def __init__(self, root, scan_root=None):
        self.root = Path(root).resolve()
        self.scan_root = Path(scan_root).resolve() if scan_root else self.root
        self.report_root = self.scan_root
        self.wiki_dir = self.root / WIKI_DIR
        self.raw_dir = self.wiki_dir / RAW_SUBDIR
        self.pages_dir = self.wiki_dir / PAGES_SUBDIR
        self.drift_dir = self.wiki_dir / DRIFT_SUBDIR
        self.db_path = self.wiki_dir / DB_NAME
        self.config_path = self.wiki_dir / CONFIG_NAME
        self.conn = None
        self.config = None

    def ensure(self):
        created = []
        for directory in (self.wiki_dir, self.raw_dir, self.pages_dir, self.drift_dir):
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
        if self.conn is None:
            self.conn = sqlite3.connect(self.db_path)
            self.conn.row_factory = sqlite3.Row
            self.conn.execute("PRAGMA busy_timeout = 5000")
            self._create_schema()
            self.migrate_legacy_pages()
        self.ensure_mirror()
        return created

    def _create_schema(self):
        with self.conn:
            self.conn.executescript(
                """
                CREATE TABLE IF NOT EXISTS sources (
                    key TEXT PRIMARY KEY,
                    path TEXT NOT NULL,
                    digest TEXT NOT NULL,
                    size INTEGER NOT NULL,
                    mtime REAL NOT NULL,
                    raw TEXT NOT NULL,
                    parser TEXT NOT NULL,
                    status TEXT NOT NULL,
                    updated_at TEXT NOT NULL
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
                    origin TEXT NOT NULL,
                    accepted_at TEXT NOT NULL,
                    PRIMARY KEY (page, rev)
                );
                CREATE TABLE IF NOT EXISTS drift (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    page TEXT NOT NULL,
                    path TEXT NOT NULL,
                    digest TEXT NOT NULL,
                    found_at TEXT NOT NULL,
                    note TEXT
                );
                """
            )

    def close(self):
        if self.conn is not None:
            self.conn.close()
            self.conn = None

    # ---------------------------------------------------------------- sources

    def scan_files(self):
        ignores = set(self.config["scan"]["ignore"])
        found = []
        for current, dirnames, filenames in os.walk(self.scan_root):
            dirnames[:] = sorted(
                name for name in dirnames
                if name not in ignores and not name.startswith(".")
            )
            for name in sorted(filenames):
                if name.startswith("."):
                    continue
                found.append(Path(current) / name)
        return found

    def source_row(self, key):
        return self.conn.execute(
            "SELECT * FROM sources WHERE key = ?", (key,)
        ).fetchone()

    def all_sources(self):
        return self.conn.execute("SELECT * FROM sources ORDER BY key").fetchall()

    def write_raw(self, key, text, meta):
        target = self.raw_dir / f"{key2file(key)}.md"
        target.parent.mkdir(parents=True, exist_ok=True)
        write_atomic(target, render_page(meta, text))
        return norm_key(target.relative_to(self.root))

    def upsert_source(self, path, digest, size, mtime, raw, parser, status="pending"):
        key = norm_key(path)
        now = date.today().isoformat()
        with self.conn:
            self.conn.execute(
                """
                INSERT INTO sources (key, path, digest, size, mtime, raw, parser, status, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(key) DO UPDATE SET
                    digest = excluded.digest,
                    size = excluded.size,
                    mtime = excluded.mtime,
                    raw = excluded.raw,
                    parser = excluded.parser,
                    status = excluded.status,
                    updated_at = excluded.updated_at
                """,
                (key, str(path), digest, size, mtime, raw, parser, status, now),
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
        meta = json.loads(row["meta"])
        meta["status"] = self.computed_status(meta)
        return {
            "id": page_id,
            "rev": row["rev"],
            "meta": meta,
            "body": row["body"],
            "digest": row["digest"],
            "render_digest": row["render_digest"],
            "origin": row["origin"],
        }

    def all_pages(self):
        rows = self.conn.execute(
            """
            SELECT r.* FROM page_revs r
            WHERE r.rev = (SELECT MAX(x.rev) FROM page_revs x WHERE x.page = r.page)
            ORDER BY r.page
            """
        ).fetchall()
        records = []
        for row in rows:
            meta = json.loads(row["meta"])
            meta["status"] = self.computed_status(meta)
            records.append(
                {
                    "id": row["page"],
                    "rev": row["rev"],
                    "meta": meta,
                    "body": row["body"],
                    "digest": row["digest"],
                    "render_digest": row["render_digest"],
                    "origin": row["origin"],
                }
            )
        return records

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
            if snapshot.get(key) != row["digest"]:
                return "stale"
        return "stable"

    def page_path(self, meta):
        return self.pages_dir / meta["type"] / f"{meta['id']}.md"

    def write_rev(self, page, meta, body, origin):
        record = self.page_record(page)
        rev = (record["rev"] if record else 0) + 1
        meta = dict(meta)
        meta["id"] = page
        meta["sources"] = sorted(set(meta.get("sources") or []))
        meta["source_digests"] = {
            key: (self.source_row(key)["digest"] if self.source_row(key) else "")
            for key in meta["sources"]
        }
        meta["updated"] = date.today().isoformat()
        meta["status"] = self.computed_status(meta)
        text = render_page(meta, body)
        render_digest = digest_text(text)
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
        target = self.page_path(meta)
        with self.conn:
            self.conn.execute(
                """
                INSERT INTO page_revs (page, rev, meta, body, digest, render_digest, origin, accepted_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    page, rev, json.dumps(meta, ensure_ascii=False, sort_keys=True),
                    body, digest_text(canonical), render_digest, origin, now_iso(),
                ),
            )
            write_atomic(target, text)
            for path in list(self.pages_dir.rglob(f"{page}.md")):
                if path != target:
                    path.unlink()
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

    # ----------------------------------------------------------------- mirror

    def migrate_legacy_pages(self):
        """Adopt pages written by the old file-first version as revision 1."""
        count = self.conn.execute("SELECT COUNT(*) FROM page_revs").fetchone()[0]
        if count or not self.pages_dir.exists():
            return
        for path in sorted(self.pages_dir.rglob("*.md")):
            text = path.read_text(encoding="utf-8", errors="replace")
            meta, body = split_frontmatter(text)
            page_id = str(meta.get("id") or path.stem)
            sources = [str(item) for item in (meta.get("sources") or [])]
            adopted = {
                "id": page_id,
                "title": str(meta.get("title") or page_id),
                "type": str(meta.get("type") or "note"),
                "summary": str(meta.get("summary") or ""),
                "sources": sorted(set(sources)),
                "source_digests": {
                    key: (self.source_row(key)["digest"] if self.source_row(key) else "")
                    for key in set(sources)
                },
                "evidence": [],
                "boundaries": {},
                "status": "stable",
                "updated": str(meta.get("updated") or date.today().isoformat()),
            }
            canonical = json.dumps(
                {"title": adopted["title"], "type": adopted["type"],
                 "summary": adopted["summary"], "sources": adopted["sources"],
                 "evidence": [], "boundaries": {}, "body": body},
                sort_keys=True, ensure_ascii=False,
            )
            with self.conn:
                self.conn.execute(
                    """
                    INSERT OR IGNORE INTO page_revs
                    (page, rev, meta, body, digest, render_digest, origin, accepted_at)
                    VALUES (?, 1, ?, ?, ?, ?, 'external', ?)
                    """,
                    (
                        page_id,
                        json.dumps(adopted, ensure_ascii=False, sort_keys=True),
                        body, digest_text(canonical), digest_bytes(text.encode("utf-8")),
                        now_iso(),
                    ),
                )
        self.rebuild_all_links()

    def ensure_mirror(self):
        """Render every page from the ledger; keep external edits in drift/."""
        events = []
        self.pages_dir.mkdir(parents=True, exist_ok=True)
        records = self.all_pages()
        known = set()
        for record in records:
            page_id = record["id"]
            known.add(page_id)
            meta = dict(record["meta"])
            meta["status"] = self.computed_status(meta)
            text = render_page(meta, record["body"])
            expected = digest_text(text)
            target = self.page_path(meta)
            current = next(
                (path for path in self.pages_dir.rglob(f"{page_id}.md") if path.exists()),
                None,
            )
            if current is None:
                write_atomic(target, text)
                events.append({"page": page_id, "action": "written", "path": norm_key(target.relative_to(self.root))})
                continue
            actual = digest_bytes(current.read_bytes())
            if actual == expected:
                if current != target:
                    write_atomic(target, text)
                    current.unlink()
                continue
            if actual != record["render_digest"]:
                saved = self.record_drift(page_id, current, actual, "page mirror edited outside the engine")
                events.append({"page": page_id, "action": "drift", "path": saved})
            write_atomic(target, text)
            if current != target and current.exists():
                current.unlink()
            events.append({"page": page_id, "action": "restored", "path": norm_key(target.relative_to(self.root))})
        for path in sorted(self.pages_dir.rglob("*.md")):
            if path.stem in known:
                continue
            saved = self.record_drift(path.stem, path, digest_bytes(path.read_bytes()), "page file without a ledger revision")
            path.unlink()
            events.append({"page": path.stem, "action": "orphan", "path": saved})
        return events

    def record_drift(self, page, path, digest, note):
        stamp = datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%SZ")
        destination = self.drift_dir / f"{page or 'unknown'}.{stamp}.md"
        counter = 1
        while destination.exists():
            destination = self.drift_dir / f"{page or 'unknown'}.{stamp}-{counter}.md"
            counter += 1
        shutil.copy2(path, destination)
        with self.conn:
            self.conn.execute(
                "INSERT INTO drift (page, path, digest, found_at, note) VALUES (?, ?, ?, ?, ?)",
                (page, norm_key(destination.relative_to(self.root)), digest, now_iso(), note),
            )
        return norm_key(destination.relative_to(self.root))

    # ------------------------------------------------------------------ tasks

    def task_row(self, task_id):
        return self.conn.execute("SELECT * FROM tasks WHERE id = ?", (task_id,)).fetchone()

    def open_task_for_page(self, page_id):
        return self.conn.execute(
            "SELECT * FROM tasks WHERE kind = 'page' AND subject = ? AND state IN ('queued', 'leased')",
            (page_id,),
        ).fetchone()

    def open_tasks(self):
        return self.conn.execute(
            "SELECT * FROM tasks WHERE state IN ('queued', 'leased') ORDER BY id"
        ).fetchall()

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
        task_id = "t_" + os.urandom(4).hex()
        with self.conn:
            self.conn.execute(
                """
                INSERT INTO tasks (id, kind, subject, state, epoch, inputs, created_at)
                VALUES (?, ?, ?, 'queued', 0, ?, ?)
                """,
                (task_id, kind, subject, json.dumps(inputs, ensure_ascii=False, sort_keys=True), now_iso()),
            )
            for item in inputs.get("sources", []):
                self.conn.execute(
                    "INSERT OR IGNORE INTO task_sources (task_id, source_key) VALUES (?, ?)",
                    (task_id, item["key"]),
                )
        return task_id

    def attach_page_source(self, task_id, source_key):
        """Add a source to a queued page task, refreshing digests of all cited sources."""
        row = self.task_row(task_id)
        inputs = json.loads(row["inputs"])
        source = self.source_row(source_key)
        inputs["sources"] = [item for item in inputs["sources"] if item["key"] != source_key]
        inputs["sources"].append(
            {"key": source_key, "digest": source["digest"], "raw": source["raw"]}
        )
        self.refresh_source_digests(inputs)
        with self.conn:
            self.conn.execute(
                "UPDATE tasks SET inputs = ? WHERE id = ?",
                (json.dumps(inputs, ensure_ascii=False, sort_keys=True), task_id),
            )
            self.conn.execute(
                "INSERT OR IGNORE INTO task_sources (task_id, source_key) VALUES (?, ?)",
                (task_id, source_key),
            )

    def refresh_source_digests(self, inputs):
        """Point a task at the current raw text after a source was reparsed."""
        changed = False
        for item in inputs.get("sources", []):
            source = self.source_row(item["key"])
            if source and source["digest"] != item["digest"]:
                item["digest"] = source["digest"]
                item["raw"] = source["raw"]
                changed = True
        return changed

    def claim_tasks(self, kind=None, batch=None):
        now = now_iso()
        with self.conn:
            self.conn.execute(
                "UPDATE tasks SET state = 'queued', lease_until = NULL WHERE state = 'leased' AND lease_until < ?",
                (now,),
            )
            sql = "SELECT * FROM tasks WHERE state = 'queued'"
            params = []
            if kind:
                sql += " AND kind = ?"
                params.append(kind)
            sql += " ORDER BY CASE kind WHEN 'route' THEN 0 ELSE 1 END, created_at, id LIMIT ?"
            params.append(batch)
            rows = self.conn.execute(sql, params).fetchall()
            for row in rows:
                self.conn.execute(
                    "UPDATE tasks SET state = 'leased', epoch = epoch + 1, lease_until = ?, attempts = attempts + 1 WHERE id = ?",
                    (self._lease_deadline(), row["id"]),
                )
                if row["kind"] == "page":
                    inputs = json.loads(row["inputs"])
                    if self.refresh_source_digests(inputs):
                        self.conn.execute(
                            "UPDATE tasks SET inputs = ? WHERE id = ?",
                            (json.dumps(inputs, ensure_ascii=False, sort_keys=True), row["id"]),
                        )
        return [self.task_row(row["id"]) for row in rows]

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

    def close_task(self, task_id, payload):
        with self.conn:
            self.conn.execute(
                "UPDATE tasks SET state = 'accepted', payload = ?, lease_until = NULL, closed_at = ? WHERE id = ?",
                (json.dumps(payload, ensure_ascii=False, sort_keys=True), now_iso(), task_id),
            )

    def page_index(self):
        return [
            {"id": record["id"], "title": record["meta"].get("title"), "type": record["meta"].get("type")}
            for record in self.all_pages()
        ]

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
            "sources": [self.source_view(item) for item in inputs.get("sources", [])],
            "page_index": self.page_index(),
        }
        if row["kind"] == "page":
            record = self.page_record(inputs["page"])
            work["mode"] = inputs.get("mode")
            work["base_rev"] = inputs.get("base_rev")
            if record:
                work["page"] = {
                    "id": record["id"],
                    "rev": record["rev"],
                    "meta": dump_meta(record["meta"]),
                    "body": record["body"],
                }
        return work


def task_json(row):
    return {
        "id": row["id"],
        "kind": row["kind"],
        "subject": row["subject"],
        "state": row["state"],
        "epoch": row["epoch"],
        "attempts": row["attempts"],
        "lease_until": row["lease_until"],
        "last_error": row["last_error"],
        "inputs": json.loads(row["inputs"]),
    }


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


def open_wiki(require_sources=False, dry_run=False):
    root = Path(ARGS.root).resolve() if ARGS.root else Path.cwd()
    if dry_run and not (root / WIKI_DIR / CONFIG_NAME).exists():
        # A dry run must not create `.wiki/` in a directory the user is only
        # inspecting, so plan against a throwaway store that scans the real root.
        ephemeral = Path(tempfile.mkdtemp(prefix="llm-wiki-dry-"))
        atexit.register(shutil.rmtree, ephemeral, ignore_errors=True)
        wiki = Wiki(ephemeral, scan_root=root)
    else:
        wiki = Wiki(root)
    created = wiki.ensure()
    if require_sources and not wiki.all_sources():
        raise WikiError(
            "E_EMPTY",
            "no sources have been ingested yet",
            "run `ingest` first so the engine has raw material to work from",
            3,
        )
    return wiki, created


def command_init():
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
    wiki, _ = open_wiki(dry_run=ARGS.dry_run)
    limit = int(wiki.config["ingest"].get("max_source_bytes", 50_000_000))
    force = ARGS.force
    files = wiki.scan_files()
    seen = set()
    fresh, unchanged, unsupported, oversize, failed, queued = [], [], [], [], [], []
    restored = []
    for path in files:
        key = norm_key(path.relative_to(wiki.scan_root))
        seen.add(key)
        row = wiki.source_row(key)
        try:
            data = read_bytes(path, limit)
        except WikiError as error:
            oversize.append({"key": key, "message": error.message})
            if not ARGS.dry_run:
                wiki.upsert_source(
                    path.relative_to(wiki.scan_root),
                    digest_text(key + str(path.stat().st_size)),
                    path.stat().st_size,
                    path.stat().st_mtime,
                    "", "", "oversize",
                )
            continue
        except OSError as error:
            failed.append({"key": key, "message": str(error)})
            continue
        digest = digest_bytes(data)
        mtime = path.stat().st_mtime
        if row and row["digest"] == digest and not force:
            if row["status"] == "missing":
                wiki.set_source_status(key, "pending" if not _cites(wiki, key) else "done")
                restored.append(key)
                continue
            if row["status"] != "missing":
                unchanged.append(key)
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
                    path.relative_to(wiki.scan_root), digest, len(data), mtime, "", "", "unsupported"
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
        meta = {
            "id": key2file(key),
            "title": path.name,
            "type": "raw",
            "summary": f"parsed from {key} by {parser}",
            "sources": [key],
            "status": "raw",
            "updated": date.today().isoformat(),
        }
        raw_rel = wiki.write_raw(key, text, meta)
        wiki.upsert_source(path.relative_to(wiki.scan_root), digest, len(data), mtime, raw_rel, parser)
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
        if not wiki.source_in_open_task(key):
            queued.append(
                wiki.enqueue_task(
                    "route",
                    key,
                    {"sources": [{"key": key, "digest": digest, "raw": raw_rel}]},
                )
            )
    vanished = []
    for row in wiki.all_sources():
        if row["key"] in seen or row["status"] == "missing":
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
    remaining = [row["key"] for row in wiki.all_sources() if row["status"] == "pending"]
    data = {
        "ingested": fresh,
        "unchanged": unchanged,
        "unsupported": unsupported,
        "oversize": oversize,
        "failed": failed,
        "vanished": vanished,
        "restored": restored,
        "queued_tasks": queued,
        "pending_total": len(remaining),
        "pending": remaining,
        "next": "run `next` to claim route tasks",
    }
    emit(
        "ingest", wiki.root,
        f"ingested {len(fresh)} sources, queued {len(queued)} route tasks; {len(remaining)} pending",
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
    open_rows = wiki.conn.execute(
        "SELECT kind, state, COUNT(*) AS n FROM tasks WHERE state IN ('queued', 'leased') GROUP BY kind, state"
    ).fetchall()
    tasks = {"queued": 0, "leased": 0, "route": 0, "page": 0}
    for row in open_rows:
        tasks[row["state"]] = tasks.get(row["state"], 0) + row["n"]
        tasks[row["kind"]] = tasks.get(row["kind"], 0) + row["n"]
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
        "tasks": {"open": tasks["queued"] + tasks["leased"], "queued": tasks["queued"],
                  "leased": tasks["leased"], "route": tasks["route"], "page": tasks["page"]},
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
    wiki, _ = open_wiki()
    batch = ARGS.batch or int(wiki.config["ingest"].get("batch", DEFAULT_BATCH))
    rows = wiki.claim_tasks(kind=ARGS.kind, batch=max(1, batch))
    tasks = []
    lines = []
    for row in rows:
        item = task_json(row)
        item["work"] = wiki.task_work(row)
        tasks.append(item)
        lines.append(f"{row['id']}  {row['kind']:<5}  {row['subject']}  (attempt {row['attempts']})")
    data = {"tasks": tasks, "count": len(tasks), "lease_seconds": TASK_LEASE_SECONDS}
    if not tasks:
        emit("next", wiki.root, "no queued tasks", data, [])
        wiki.close()
        return 0
    emit(
        "next", wiki.root,
        f"claimed {len(tasks)} task(s); submit with `submit <id> --epoch <n> --payload <file|->`",
        data,
        lines,
    )
    wiki.close()
    return 0


def read_payload():
    if ARGS.payload == "-":
        raw = sys.stdin.read()
    else:
        try:
            raw = Path(ARGS.payload).read_text(encoding="utf-8")
        except OSError as error:
            raise WikiError("E_PAYLOAD", f"cannot read payload: {error}", "pass a readable JSON file or -", 2)
    try:
        return json.loads(raw)
    except json.JSONDecodeError as error:
        raise WikiError(
            "E_PAYLOAD", f"payload is not valid JSON: {error}",
            "emit one JSON object; `next --json` shows each task's inputs", 2,
        )


def command_submit():
    wiki, _ = open_wiki()
    task_id = ARGS.id
    row = wiki.task_row(task_id)
    if not row:
        raise WikiError("E_TASK_UNKNOWN", f"no task {task_id}", "run `next` to get a task id", 2)
    if row["state"] == "accepted":
        raise WikiError("E_TASK_STALE", f"task {task_id} is already accepted",
                        "run `next` for new work", 3)
    if row["state"] != "leased":
        raise WikiError("E_TASK_STALE", f"task {task_id} is {row['state']}, not leased",
                        "run `next` to claim it again", 3)
    if row["epoch"] != ARGS.epoch:
        raise WikiError(
            "E_TASK_STALE",
            f"task {task_id} epoch is {row['epoch']}, payload used {ARGS.epoch}",
            "re-read `next --json` and resubmit with the current epoch", 3,
        )
    if row["lease_until"] and parse_iso(row["lease_until"]) < datetime.now(timezone.utc):
        wiki.requeue_task(task_id, "lease expired")
        raise WikiError("E_TASK_STALE", f"lease on {task_id} expired",
                        "run `next` to claim it again", 3)
    payload = read_payload()
    try:
        if row["kind"] == "route":
            result = submit_route(wiki, row, payload)
        else:
            result = submit_page(wiki, row, payload)
    except WikiError as error:
        wiki.requeue_task(task_id, f"{error.code}: {error.message}")
        raise
    emit(
        "submit", wiki.root,
        result["summary"],
        {
            "task": task_id,
            "kind": result["kind"],
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
            "route payload must be {\"decisions\": [...]} with one decision per input source",
            "see references/cli.md for the route schema",
            2,
        )
    seen = []
    for decision in decisions:
        if not isinstance(decision, dict):
            raise WikiError("E_PAYLOAD", "each decision must be an object", "", 2)
        key = decision.get("source")
        if key not in available:
            raise WikiError("E_TASK_SCOPE", f"source {key!r} is not in this task",
                            f"task sources: {', '.join(available)}", 3)
        seen.append(key)
        action = decision.get("action")
        if action not in ("new", "merge", "skip"):
            raise WikiError("E_PAYLOAD", f"action {action!r} must be new, merge or skip",
                            "pick one action per source", 2)
        if not str(decision.get("reason") or "").strip():
            raise WikiError("E_PAYLOAD", f"{key}: reason must not be empty",
                            "one sentence explaining the choice", 2)
    if sorted(seen) != sorted(available) or len(seen) != len(available):
        raise WikiError(
            "E_PAYLOAD",
            f"decisions must cover each input source exactly once; got {sorted(seen)} for {sorted(available)}",
            "submit one decision per source", 2,
        )
    # Validate every decision before touching the ledger, so a bad payload
    # never leaves a half-applied route behind.
    plan = []
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
            if not record:
                raise WikiError(
                    "E_PAGE_UNKNOWN",
                    f"{key}: page {page_id} does not exist",
                    "use action new, or check `next --json` page_index", 3,
                )
            plan.append({"action": "merge", "key": key, "page": page_id, "rev": record["rev"]})
    page_tasks = []
    for item in plan:
        key = item["key"]
        if item["action"] == "skip":
            wiki.set_source_status(key, "skipped")
            continue
        source = wiki.source_row(key)
        entry = {"key": key, "digest": source["digest"], "raw": source["raw"]}
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
            inputs.update({"mode": "create", "title": item["title"], "type": item["type"]})
        else:
            inputs["mode"] = "update"
        page_tasks.append(wiki.enqueue_task("page", item["page"], inputs))
    wiki.close_task(row["id"], payload)
    return {
        "kind": "route",
        "summary": f"route {row['id']} accepted; {len(set(page_tasks))} page task(s) queued",
        "sources": available,
        "queued_page_tasks": sorted(set(page_tasks)),
        "lines": [f"page task: {task_id}" for task_id in sorted(set(page_tasks))],
    }


def submit_page(wiki, row, payload):
    inputs = json.loads(row["inputs"])
    page_id = inputs["page"]
    sources = inputs["sources"]
    for item in sources:
        current = wiki.source_row(item["key"])
        if current is None or current["status"] == "missing":
            raise WikiError("E_SOURCE_DRIFT", f"source {item['key']} vanished",
                            "re-run `ingest`, then `next`", 3)
        if current["digest"] != item["digest"]:
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
                f"page {page_id} is at rev {record['rev']}, task was based on rev {inputs.get('base_rev')}",
                "take a fresh page task and rewrite against the current revision", 3,
            )
    meta, body, evidence, boundaries = validate_page_payload(wiki, row, payload, sources)
    rev = wiki.write_rev(page_id, meta, body, "submit")
    for item in sources:
        wiki.set_source_status(item["key"], "done")
    wiki.close_task(row["id"], payload)
    return {
        "kind": "page",
        "page": page_id,
        "rev": rev,
        "sources": [item["key"] for item in sources],
        "summary": f"page {page_id} accepted at rev {rev} ({len(sources)} sources)",
        "lines": [f"page: {page_id} rev {rev}"],
    }


def validate_page_payload(wiki, row, payload, sources):
    inputs = json.loads(row["inputs"])
    page_id = inputs["page"]
    if not isinstance(payload, dict):
        raise WikiError("E_PAYLOAD", "page payload must be a JSON object", "", 2)
    if str(payload.get("page") or "").strip().lower() != page_id:
        raise WikiError("E_PAYLOAD", f"payload.page must be {page_id!r}",
                        "the task subject is the only page you may write", 2)
    title = str(payload.get("title") or "").strip()
    if not title or len(title) > 120:
        raise WikiError("E_PAYLOAD", "title must be 1-120 characters", "", 2)
    page_type = str(payload.get("type") or "").strip()
    if page_type not in wiki.config["schema"]["types"]:
        raise WikiError("E_PAYLOAD", f"type {page_type!r} not in schema.types",
                        f"choose one of: {', '.join(wiki.config['schema']['types'])}", 2)
    summary = str(payload.get("summary") or "").strip()
    if not summary:
        raise WikiError("E_PAYLOAD", "summary must not be empty", "", 2)
    if len(summary) > SUMMARY_MAX_CHARS:
        raise WikiError("E_PAYLOAD", f"summary is {len(summary)} chars, max {SUMMARY_MAX_CHARS}",
                        "move detail into the body", 2)
    links = payload.get("links") or []
    if not isinstance(links, list) or any(not isinstance(item, str) for item in links):
        raise WikiError("E_PAYLOAD", "links must be a list of page ids", "", 2)
    known = wiki.page_ids()
    for link in links:
        if link.strip().lower() == page_id:
            continue
        if link.strip().lower() not in known:
            raise WikiError("E_LINK_UNRESOLVED", f"link {link!r} points at no page",
                            "create that page first, or drop the link", 2)
    body = payload.get("body")
    if body is None and payload.get("body_file"):
        candidate = Path(str(payload["body_file"]))
        if not candidate.is_absolute():
            candidate = Path.cwd() / candidate
        try:
            body = candidate.read_text(encoding="utf-8")
        except OSError as error:
            raise WikiError("E_PAYLOAD", f"cannot read body_file: {error}", "", 2)
    if not isinstance(body, str) or not body.strip():
        raise WikiError("E_PAYLOAD", "body must be non-empty text (or use body_file)", "", 2)
    if body.lstrip().startswith("---"):
        raise WikiError("E_PAYLOAD", "body must not start with frontmatter",
                        "send only the markdown sections; the engine renders frontmatter", 2)
    if len(body) > BODY_MAX_CHARS:
        raise WikiError("E_PAYLOAD", f"body is {len(body)} chars, max {BODY_MAX_CHARS}",
                        "split the page or move detail into linked pages", 2)
    evidence = payload.get("evidence")
    if not isinstance(evidence, list) or not evidence:
        raise WikiError("E_PAYLOAD", "evidence must be a non-empty list of {source, quote}",
                        "quote at least one sentence from the task sources", 2)
    available = {item["key"] for item in sources}
    for item in evidence:
        if not isinstance(item, dict):
            raise WikiError("E_PAYLOAD", "each evidence item must be an object", "", 2)
        key = item.get("source")
        quote = str(item.get("quote") or "")
        if key not in available:
            raise WikiError(
                "E_TASK_SCOPE", f"evidence source {key!r} is not in this task",
                f"task sources: {', '.join(sorted(available))}", 3,
            )
        if not quote.strip():
            raise WikiError("E_PAYLOAD", f"{key}: quote must not be empty", "", 2)
        raw = re.sub(r"\s+", "", wiki.source_body(key))
        if re.sub(r"\s+", "", quote) not in raw:
            raise WikiError(
                "E_EVIDENCE_NOT_FOUND",
                f"quote not found in {key}: {quote[:60]!r}",
                "re-read the raw source and copy an exact sentence", 2,
            )
    boundaries = payload.get("boundaries")
    if not isinstance(boundaries, dict) or not str(boundaries.get("covered") or "").strip() \
            or not str(boundaries.get("not_covered") or "").strip():
        raise WikiError("E_PAYLOAD", "boundaries.covered and boundaries.not_covered are required",
                        "state what the page covers and what it deliberately leaves out", 2)
    meta = {
        "id": page_id,
        "title": title,
        "type": page_type,
        "summary": summary,
        "sources": sorted(available),
        "evidence": evidence,
        "boundaries": boundaries,
    }
    cleaned_links = sorted({link.strip().lower() for link in links if link.strip()})
    if cleaned_links:
        trailer = "\n\n## 关联\n" + "\n".join(f"- [[{link}]]" for link in cleaned_links)
        body = body.rstrip() + trailer
    return meta, body, evidence, boundaries


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
        path = wiki.page_path(meta)
    else:
        path = resolve_ref(wiki, ref)
        text = path.read_text(encoding="utf-8", errors="replace")
        body = text
        if path.suffix == ".md" and WIKI_DIR in path.parts:
            meta, body = split_frontmatter(text)
    lines = body.splitlines()
    offset = max(1, ARGS.offset)
    limit = max(1, min(ARGS.limit, 2000))
    window = lines[offset - 1: offset - 1 + limit]
    next_offset = offset + len(window) if offset - 1 + len(window) < len(lines) else None
    data = {
        "path": norm_key(path.relative_to(wiki.root)) if window_safe(wiki.root, path) else str(path),
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
        raise WikiError("E_PATH", "ref escapes the wiki root", "use a repo-relative path", 2)
    if not candidate.exists():
        raise WikiError("E_NOT_FOUND", f"no file {ref}", "check the path, or use `query` first", 3)
    return candidate


def window_safe(root, candidate):
    try:
        candidate.relative_to(root)
        return True
    except ValueError:
        return False


def command_query():
    wiki, _ = open_wiki()
    pages = wiki.all_pages()
    if ARGS.page:
        record = wiki.page_record(ARGS.page)
        if not record:
            raise WikiError("E_NOT_FOUND", f"no page {ARGS.page}", "search with `query <text>` first", 3)
        outgoing, incoming = wiki.links_for(ARGS.page)
        data = {
            "page": dump_meta(record["meta"]),
            "rev": record["rev"],
            "path": norm_key(wiki.page_path(record["meta"]).relative_to(wiki.root)),
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
    scored = []
    for record in pages:
        meta = record["meta"]
        title = str(meta.get("title", ""))
        summary = str(meta.get("summary", ""))
        body = record["body"]
        haystack = f"{title}\n{summary}\n{body}"
        hits = sum(haystack.lower().count(term.lower()) for term in terms)
        if hits == 0:
            continue
        score = hits + 5 * sum(title.lower().count(t.lower()) for t in terms)
        score += 3 * sum(summary.lower().count(t.lower()) for t in terms)
        scored.append(
            {
                "id": record["id"],
                "title": title,
                "type": meta.get("type"),
                "status": meta.get("status"),
                "summary": summary,
                "sources": meta.get("sources", []),
                "rev": record["rev"],
                "score": score,
                "snippet": snippet(body, terms[0]),
            }
        )
    scored.sort(key=rank_key)
    limit = max(1, ARGS.limit)
    results = scored[:limit]
    data = {
        "query": " ".join(terms),
        "matches": len(scored),
        "returned": len(results),
        "results": results,
        "boundary": "hit count is a ranked subset, not proof of full coverage",
    }
    emit(
        "query", wiki.root,
        f"{len(scored)} matching pages, showing {len(results)} for {' '.join(terms)}",
        data,
        [f"{item['id']}  [{item['type']}/{item['status']}]  {item['title']}\n    {item['snippet']}"
         for item in results],
    )
    wiki.close()
    return 0


def rank_key(item):
    return (-item["score"], item["id"])


def snippet(body, term):
    flat = re.sub(r"\s+", " ", body)
    index = flat.lower().find(term.lower())
    if index < 0:
        return flat[:120]
    start = max(0, index - 60)
    end = min(len(flat), index + len(term) + 90)
    prefix = "..." if start > 0 else ""
    suffix = "..." if end < len(flat) else ""
    return prefix + flat[start:end] + suffix


def command_page_import():
    wiki, _ = open_wiki()
    page_id = ARGS.id.strip().lower()
    record = wiki.page_record(page_id)
    if not record:
        raise WikiError("E_NOT_FOUND", f"no page {page_id}", "import only edits existing pages", 3)
    try:
        text = Path(ARGS.from_path).read_text(encoding="utf-8")
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
        {"page": page_id, "rev": rev, "path": norm_key(wiki.page_path(merged).relative_to(wiki.root))},
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


def command_export():
    wiki, _ = open_wiki()
    target = Path(ARGS.dir).resolve()
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
    events = wiki.ensure_mirror()
    for event in events:
        if event["action"] in ("drift", "orphan"):
            issues.append({"severity": "warn", "code": "L2",
                           "message": f"mirror drift kept at {event['path']} ({event['page']})"})
        elif ARGS.fix:
            fixed.append(f"{event['action']}: {event['page']}")
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
    common.add_argument("--root", help="wiki root directory (default: current directory)")
    common.add_argument("--json", action="store_true", help="machine-readable output")
    parser = argparse.ArgumentParser(
        prog="wiki.py",
        description="Portable wiki engine: task-driven ingestion, ledger-owned pages.",
    )
    parser.add_argument("--version", action="version", version=f"llm-wiki {VERSION}")
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("init", parents=[common], help="create .wiki/ and report state")
    ingest = sub.add_parser("ingest", parents=[common], help="scan sources and queue route tasks")
    ingest.add_argument("--dry-run", action="store_true")
    ingest.add_argument("--force", action="store_true", help="reparse even when the digest is unchanged")
    next_p = sub.add_parser("next", parents=[common], help="claim queued tasks")
    next_p.add_argument("--kind", choices=["route", "page"])
    next_p.add_argument("--batch", type=int, default=0, help="tasks to claim (default: ingest.batch)")
    submit = sub.add_parser("submit", parents=[common], help="submit a task payload")
    submit.add_argument("id")
    submit.add_argument("--epoch", type=int, required=True)
    submit.add_argument("--payload", required=True, help="JSON file, or - for stdin")
    release = sub.add_parser("release", parents=[common], help="return a task to the queue")
    release.add_argument("id")
    read = sub.add_parser("read", parents=[common], help="read raw text or a page by ref")
    read.add_argument("ref")
    read.add_argument("--offset", type=int, default=1)
    read.add_argument("--limit", type=int, default=200)
    query = sub.add_parser("query", parents=[common], help="search pages, or inspect one page")
    query.add_argument("text", nargs="?")
    query.add_argument("--page")
    query.add_argument("--limit", type=int, default=8)
    page = sub.add_parser("page", help="human edits to an existing page")
    page_sub = page.add_subparsers(dest="page_command", required=True)
    import_p = page_sub.add_parser("import", parents=[common], help="adopt a hand-edited page file")
    import_p.add_argument("id")
    import_p.add_argument("--from", dest="from_path", required=True)
    restore = page_sub.add_parser("restore", parents=[common], help="restore an older revision")
    restore.add_argument("id")
    restore.add_argument("--rev", type=int, required=True)
    export = sub.add_parser("export", parents=[common], help="export mirrored pages to a directory")
    export.add_argument("dir")
    status = sub.add_parser("status", parents=[common], help="ledger, pages, tasks")
    status.add_argument("--brief", action="store_true")
    doctor = sub.add_parser("doctor", parents=[common], help="health checks; --fix repairs mechanical issues")
    doctor.add_argument("--fix", action="store_true")
    sub.add_parser("version", parents=[common], help="print the engine version")
    return parser


ARGS = None
COMMANDS = {
    "init": command_init,
    "ingest": command_ingest,
    "next": command_next,
    "submit": command_submit,
    "release": command_release,
    "read": command_read,
    "query": command_query,
    "export": command_export,
    "status": command_status,
    "doctor": command_doctor,
    "version": command_version,
}


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
    else:
        handler = COMMANDS[command]
    try:
        return handler()
    except WikiError as error:
        return fail(command, root, error)
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main())
