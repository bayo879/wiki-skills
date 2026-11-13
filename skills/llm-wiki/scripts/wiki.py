#!/usr/bin/env python3
"""Portable wiki engine.

One directory of source files in, one deterministic `.wiki/` state tree out.
Only the Python standard library is required. The engine owns bookkeeping --
scanning, parsing, hashing, atomic writes, the source ledger, and the link
index. Semantic work -- deciding which pages exist and writing their prose --
belongs to the agent that loads `SKILL.md`.

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
from datetime import date
from html.parser import HTMLParser
from pathlib import Path

VERSION = "0.1.0"
WIKI_DIR = ".wiki"
RAW_SUBDIR = "raw"
PAGES_SUBDIR = "pages"
DB_NAME = "wiki.db"
CONFIG_NAME = "wiki.json"
MAX_RAW_CHARS = 4_000_000

DEFAULT_CONFIG = {
    "schema": {"types": ["concept", "entity", "process", "note"]},
    "scan": {
        "ignore": [
            ".git", ".hg", ".svn", ".wiki", "node_modules", "target", "dist",
            "build", "vendor", ".venv", "venv", "__pycache__", ".idea", ".vscode",
        ]
    },
    "ingest": {"max_source_bytes": 50_000_000, "settle_seconds": 0},
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
    return "\n".join(lines) + body.rstrip() + "\n"


def dump_meta(meta):
    """Frontmatter values as JSON-friendly types for command output."""
    clean = {}
    for key, value in meta.items():
        if isinstance(value, list):
            clean[key] = [str(item) for item in value]
        else:
            clean[key] = str(value)
    return clean


class Wiki:
    """The knowledge base rooted at one directory."""

    def __init__(self, root, scan_root=None):
        self.root = Path(root).resolve()
        self.scan_root = Path(scan_root).resolve() if scan_root else self.root
        self.report_root = self.scan_root
        self.wiki_dir = self.root / WIKI_DIR
        self.raw_dir = self.wiki_dir / RAW_SUBDIR
        self.pages_dir = self.wiki_dir / PAGES_SUBDIR
        self.db_path = self.wiki_dir / DB_NAME
        self.config_path = self.wiki_dir / CONFIG_NAME
        self.conn = None
        self.config = None

    def ensure(self):
        created = []
        for directory in (self.wiki_dir, self.raw_dir, self.pages_dir):
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
                """
            )

    def close(self):
        if self.conn is not None:
            self.conn.close()
            self.conn = None

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
        body = render_page(meta, text)
        write_atomic(target, body)
        return norm_key(target.relative_to(self.root))

    def upsert_source(self, path, digest, size, mtime, raw, parser):
        key = norm_key(path)
        now = date.today().isoformat()
        with self.conn:
            self.conn.execute(
                """
                INSERT INTO sources (key, path, digest, size, mtime, raw, parser, status, updated_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', ?)
                ON CONFLICT(key) DO UPDATE SET
                    digest = excluded.digest,
                    size = excluded.size,
                    mtime = excluded.mtime,
                    raw = excluded.raw,
                    parser = excluded.parser,
                    status = 'pending',
                    updated_at = excluded.updated_at
                """,
                (key, str(path), digest, size, mtime, raw, parser, now),
            )

    def set_source_status(self, key, status):
        with self.conn:
            self.conn.execute(
                "UPDATE sources SET status = ?, updated_at = ? WHERE key = ?",
                (status, date.today().isoformat(), key),
            )

    def pages(self):
        records = []
        if not self.pages_dir.exists():
            return records
        for path in sorted(self.pages_dir.rglob("*.md")):
            text = path.read_text(encoding="utf-8", errors="replace")
            meta, body = split_frontmatter(text)
            records.append(
                {
                    "path": norm_key(path.relative_to(self.root)),
                    "meta": meta,
                    "body": body,
                }
            )
        return records

    def page_by_id(self, page_id):
        for record in self.pages():
            if record["meta"].get("id") == page_id:
                return record
        return None

    def write_page(self, meta, body):
        target = self.pages_dir / meta["type"] / f"{meta['id']}.md"
        for existing in self.pages_dir.rglob(f"{meta['id']}.md"):
            if existing != target and existing.exists():
                existing.unlink()
        target.parent.mkdir(parents=True, exist_ok=True)
        write_atomic(target, render_page(meta, body))
        return norm_key(target.relative_to(self.root))

    def delete_page(self, page_id):
        deleted = []
        for path in list(self.pages_dir.rglob(f"{page_id}.md")):
            path.unlink()
            deleted.append(norm_key(path.relative_to(self.root)))
        return deleted

    def rebuild_links(self):
        with self.conn:
            self.conn.execute("DELETE FROM links")
            for record in self.pages():
                page_id = record["meta"].get("id")
                if not page_id:
                    continue
                targets = sorted(set(LINK_RE.findall(record["body"])))
                for target in targets:
                    self.conn.execute(
                        "INSERT OR IGNORE INTO links (src, dst) VALUES (?, ?)",
                        (page_id, target.strip().lower()),
                    )

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
    data = {
        "created": created,
        "config": norm_key(wiki.config_path.relative_to(wiki.root)),
        "pages": len(wiki.pages()),
        "sources": len(rows),
    }
    emit(
        "init", wiki.root,
        f"wiki ready at {WIKI_DIR}/ ({len(rows)} sources, {data['pages']} pages)",
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
    pending, unchanged, unsupported, oversize, failed = [], [], [], [], []
    restored = []
    for path in files:
        key = norm_key(path.relative_to(wiki.scan_root))
        seen.add(key)
        row = wiki.source_row(key)
        try:
            data = read_bytes(path, limit)
        except WikiError as error:
            oversize.append({"key": key, "message": error.message})
            continue
        except OSError as error:
            failed.append({"key": key, "message": str(error)})
            continue
        digest = digest_bytes(data)
        mtime = path.stat().st_mtime
        if row and row["digest"] != digest and not ARGS.dry_run:
            mark_pages_stale(wiki, key)
        if row and row["digest"] == digest and not force:
            if row["status"] == "missing":
                wiki.set_source_status(key, "done")
                mark_pages_status(wiki, key, "stale", "stable")
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
            continue
        if len(text) > MAX_RAW_CHARS:
            text = text[:MAX_RAW_CHARS]
        if ARGS.dry_run:
            pending.append(
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
        pending.append(
            {
                "key": key,
                "raw": raw_rel,
                "parser": parser,
                "digest": digest[:12],
                "size": len(data),
                "chars": len(text),
            }
        )
    vanished = []
    for row in wiki.all_sources():
        if row["key"] in seen or row["status"] == "missing":
            continue
        vanished.append(row["key"])
        if ARGS.dry_run:
            continue
        wiki.set_source_status(row["key"], "missing")
        mark_pages_stale(wiki, row["key"])
    if ARGS.dry_run:
        data = {
            "dry_run": True,
            "would_ingest": pending,
            "unchanged": unchanged,
            "unsupported": unsupported,
            "oversize": oversize,
            "failed": failed,
            "vanished": vanished,
            "restored": restored,
        }
        emit(
            "ingest", wiki.report_root,
            f"dry run: {len(pending)} new or changed, {len(unchanged)} unchanged, "
            f"{len(unsupported)} unsupported",
            data,
            [f"would ingest: {item['key']}" for item in pending],
        )
        wiki.close()
        return 0
    remaining = [row["key"] for row in wiki.all_sources() if row["status"] == "pending"]
    data = {
        "ingested": pending,
        "unchanged": unchanged,
        "unsupported": unsupported,
        "oversize": oversize,
        "failed": failed,
        "vanished": vanished,
        "restored": restored,
        "pending_total": len(remaining),
        "pending": remaining,
        "next": "read each raw file, decide page ownership, then write pages with `page put` and close sources with `settle`",
        "cli": "scripts/wiki.py",
    }
    emit(
        "ingest", wiki.root,
        f"ingested {len(pending)} sources; {len(remaining)} waiting for curation",
        data,
        [f"pending: {item['key']} -> {item['raw']}" for item in pending],
        ok=not (failed or oversize),
    )
    wiki.close()
    return 1 if (failed or oversize) else 0


def mark_pages_stale(wiki, source_key):
    mark_pages_status(wiki, source_key, "stable", "stale")


def mark_pages_status(wiki, source_key, before, after):
    """Flip the status of pages citing one source, without touching prose."""
    for record in wiki.pages():
        meta = record["meta"]
        if source_key not in meta.get("sources", []):
            continue
        if before is not None and meta.get("status") != before:
            continue
        meta["status"] = after
        wiki.write_page(meta, record["body"])


def command_read():
    wiki, _ = open_wiki()
    ref = ARGS.ref
    path = resolve_ref(wiki, ref)
    text = path.read_text(encoding="utf-8", errors="replace")
    meta = {}
    body = text
    if path.suffix == ".md" and WIKI_DIR in path.parts:
        meta, body = split_frontmatter(text)
    lines = body.splitlines()
    offset = max(1, ARGS.offset)
    limit = max(1, min(ARGS.limit, 2000))
    window = lines[offset - 1: offset - 1 + limit]
    next_offset = offset + len(window) if offset - 1 + len(window) < len(lines) else None
    data = {
        "path": norm_key(path.relative_to(wiki.root)),
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
        if not row:
            raise WikiError("E_NOT_FOUND", f"no source keyed {key}", "run `status` to list sources", 3)
        return wiki.root / row["raw"]
    if ref.startswith("page:"):
        page_id = ref[5:]
        record = wiki.page_by_id(page_id)
        if not record:
            raise WikiError("E_NOT_FOUND", f"no page {page_id}", "run `query` to find the page id", 3)
        return wiki.root / record["path"]
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
    pages = [record for record in wiki.pages() if record["meta"].get("id")]
    if ARGS.page:
        record = wiki.page_by_id(ARGS.page)
        if not record:
            raise WikiError("E_NOT_FOUND", f"no page {ARGS.page}", "search with `query <text>` first", 3)
        outgoing, incoming = wiki.links_for(ARGS.page)
        data = {
            "page": dump_meta(record["meta"]),
            "path": record["path"],
            "outgoing": outgoing,
            "incoming": incoming,
            "body": record["body"],
        }
        emit(
            "query", wiki.root,
            f"page {ARGS.page}: {len(outgoing)} outgoing, {len(incoming)} incoming links",
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
                "id": meta.get("id"),
                "title": title,
                "type": meta.get("type"),
                "status": meta.get("status"),
                "summary": summary,
                "sources": meta.get("sources", []),
                "path": record["path"],
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


def read_body_arg():
    if ARGS.body == "-":
        return sys.stdin.read()
    if ARGS.body:
        return Path(ARGS.body).read_text(encoding="utf-8")
    raise WikiError(
        "E_USAGE",
        "page put needs a body",
        "pass --body <file> or --body - with text on stdin",
        2,
    )


def command_page_put():
    wiki, _ = open_wiki(require_sources=True)
    page_id = ARGS.id.strip().lower()
    if not ID_RE.match(page_id):
        raise WikiError(
            "E_BAD_ID",
            f"page id {page_id!r} must be lowercase letters, digits, dot, dash or underscore",
            "use a stable slug such as `huo-zai-yin-huan`",
            2,
        )
    page_type = ARGS.type
    if page_type not in wiki.config["schema"]["types"]:
        raise WikiError(
            "E_BAD_TYPE",
            f"type {page_type!r} is not in schema.types",
            f"choose one of: {', '.join(wiki.config['schema']['types'])}; edit {WIKI_DIR}/{CONFIG_NAME} to add more",
            2,
        )
    summary = (ARGS.summary or "").strip()
    if not summary:
        raise WikiError("E_BAD_SUMMARY", "summary must not be empty", "one sentence shown in search results", 2)
    sources = []
    for key in ARGS.source or []:
        row = wiki.source_row(key)
        if not row:
            raise WikiError(
                "E_UNKNOWN_SOURCE",
                f"source key {key} is not registered",
                "run `ingest` first; use the exact key it reports",
                2,
            )
        sources.append(key)
    if not sources:
        raise WikiError("E_NO_SOURCE", "a page needs at least one source", "pass --source <key>", 2)
    body = read_body_arg()
    meta = {
        "id": page_id,
        "title": ARGS.title.strip(),
        "type": page_type,
        "summary": summary,
        "sources": sorted(set(sources)),
        "status": ARGS.status,
        "updated": date.today().isoformat(),
    }
    path = wiki.write_page(meta, body)
    wiki.rebuild_links()
    data = {"page": dump_meta(meta), "path": path}
    emit(
        "page put", wiki.root,
        f"wrote {page_id} ({page_type}, {len(meta['sources'])} sources) to {path}",
        data,
        [f"page: {path}"],
    )
    wiki.close()
    return 0


def command_page_delete():
    wiki, _ = open_wiki()
    record = wiki.page_by_id(ARGS.id)
    if not record:
        raise WikiError("E_NOT_FOUND", f"no page {ARGS.id}", "check `query` output", 3)
    if not ARGS.yes:
        raise WikiError(
            "E_CONFIRM",
            "page deletion is irreversible in this tool",
            "re-run with --yes after confirming an external backup exists",
            3,
        )
    deleted = wiki.delete_page(ARGS.id)
    wiki.rebuild_links()
    emit("page delete", wiki.root, f"deleted {ARGS.id}", {"deleted": deleted}, deleted)
    wiki.close()
    return 0


def command_settle():
    wiki, _ = open_wiki(require_sources=True)
    keys = list(ARGS.keys)
    if not keys:
        keys = [row["key"] for row in wiki.all_sources() if row["status"] == "pending"]
    if not keys:
        raise WikiError("E_NOTHING", "no pending sources to settle", "run `status`", 3)
    pages = wiki.pages()
    settled, skipped, blocked = [], [], []
    for key in keys:
        row = wiki.source_row(key)
        if not row:
            blocked.append({"key": key, "reason": "unknown source key"})
            continue
        if ARGS.skip:
            wiki.set_source_status(key, "skipped")
            skipped.append(key)
            continue
        citing = [
            record["meta"]["id"] for record in pages
            if key in record["meta"].get("sources", []) and record["meta"].get("id")
        ]
        reviewed = [
            record["meta"]["id"] for record in pages
            if key in record["meta"].get("sources", [])
            and record["meta"].get("id")
            and record["meta"].get("status") != "stale"
        ]
        if not citing:
            blocked.append(
                {"key": key, "reason": "no page cites this source; write a page or pass --skip"}
            )
            continue
        if not reviewed:
            blocked.append(
                {
                    "key": key,
                    "reason": "every citing page is stale; re-read the changed source and rewrite the page",
                }
            )
            continue
        wiki.set_source_status(key, "done")
        settled.append({"key": key, "pages": sorted(reviewed)})
    remaining = [row["key"] for row in wiki.all_sources() if row["status"] == "pending"]
    data = {"settled": settled, "skipped": skipped, "blocked": blocked, "pending": remaining}
    emit(
        "settle", wiki.root,
        f"settled {len(settled)}, skipped {len(skipped)}, blocked {len(blocked)}; {len(remaining)} pending",
        data,
        [f"settled: {item['key']} -> {', '.join(item['pages'])}" for item in settled]
        + [f"blocked: {item['key']} ({item['reason']})" for item in blocked],
        ok=not blocked,
    )
    wiki.close()
    return 1 if blocked else 0


def command_status():
    wiki, created = open_wiki()
    rows = wiki.all_sources()
    by_status = {}
    for row in rows:
        by_status[row["status"]] = by_status.get(row["status"], 0) + 1
    pages = wiki.pages()
    pending = [row["key"] for row in rows if row["status"] == "pending"]
    data = {
        "created": created,
        "sources": by_status,
        "source_total": len(rows),
        "pages": len(pages),
        "stale_pages": sorted(
            record["meta"]["id"] for record in pages
            if record["meta"].get("status") == "stale" and record["meta"].get("id")
        ),
        "pending": pending,
    }
    emit(
        "status", wiki.root,
        f"{len(rows)} sources ({by_status.get('pending', 0)} pending), {len(pages)} pages",
        data,
        [f"pending: {key}" for key in pending] or ["pending: none"],
    )
    wiki.close()
    return 0


def command_doctor():
    wiki, _ = open_wiki()
    issues, fixed = [], []
    try:
        integrity = wiki.conn.execute("PRAGMA integrity_check").fetchone()[0]
    except sqlite3.DatabaseError as error:
        raise WikiError("E_DB", f"database unreadable: {error}", "restore from backup; do not edit .wiki/wiki.db", 1)
    if integrity != "ok":
        issues.append({"severity": "fail", "code": "L1", "message": f"database integrity: {integrity}"})
    for directory in (wiki.wiki_dir, wiki.raw_dir, wiki.pages_dir):
        if not directory.exists():
            if ARGS.fix:
                directory.mkdir(parents=True, exist_ok=True)
                fixed.append(norm_key(directory.relative_to(wiki.root)))
            else:
                issues.append({"severity": "fail", "code": "L2", "message": f"missing {directory.name}/"})
    seen_ids = {}
    for record in wiki.pages():
        meta = record["meta"]
        page_id = meta.get("id")
        if not page_id:
            issues.append({"severity": "fail", "code": "L3", "message": f"{record['path']} has no id"})
            continue
        if page_id in seen_ids:
            issues.append(
                {"severity": "fail", "code": "L4", "message": f"duplicate page id {page_id}: {seen_ids[page_id]} and {record['path']}"}
            )
        seen_ids[page_id] = record["path"]
        missing_fields = [field for field in ("title", "type", "summary", "sources", "status", "updated")
                          if not meta.get(field)]
        if missing_fields:
            issues.append(
                {"severity": "fail", "code": "L5",
                 "message": f"{page_id} missing {', '.join(missing_fields)}"}
            )
        if meta.get("type") and meta["type"] not in wiki.config["schema"]["types"]:
            issues.append(
                {"severity": "warn", "code": "L6",
                 "message": f"{page_id} type {meta['type']} not in schema.types"}
            )
        for key in meta.get("sources", []) or []:
            if not wiki.source_row(key):
                issues.append(
                    {"severity": "warn", "code": "L7",
                     "message": f"{page_id} cites unregistered source {key}"}
                )
        if not meta.get("sources"):
            issues.append(
                {"severity": "warn", "code": "L8", "message": f"{page_id} has no sources"}
            )
    existing_ids = set(seen_ids)
    broken = []
    for row in wiki.conn.execute("SELECT src, dst FROM links"):
        if row["dst"] not in existing_ids:
            broken.append(f"{row['src']} -> {row['dst']}")
    if broken:
        issues.append(
            {"severity": "warn", "code": "L9", "message": f"broken links: {', '.join(sorted(broken)[:10])}"}
        )
    missing_sources = [row["key"] for row in wiki.all_sources() if row["status"] == "missing"]
    if missing_sources:
        issues.append(
            {"severity": "warn", "code": "L10",
             "message": f"source files vanished: {', '.join(missing_sources[:10])}"}
        )
    if ARGS.fix:
        wiki.rebuild_links()
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


def build_parser():
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--root", help="wiki root directory (default: current directory)")
    common.add_argument("--json", action="store_true", help="machine-readable output")
    parser = argparse.ArgumentParser(
        prog="wiki.py",
        description="Portable wiki engine: ingest sources, store pages, query by text and links.",
    )
    parser.add_argument("--version", action="version", version=f"llm-wiki {VERSION}")
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("init", parents=[common], help="create .wiki/ and report state")
    ingest = sub.add_parser("ingest", parents=[common], help="scan sources, parse them into .wiki/raw/")
    ingest.add_argument("--dry-run", action="store_true")
    ingest.add_argument("--force", action="store_true", help="reparse even when the digest is unchanged")
    read = sub.add_parser("read", parents=[common], help="read a raw or page file by ref")
    read.add_argument("ref")
    read.add_argument("--offset", type=int, default=1)
    read.add_argument("--limit", type=int, default=200)
    query = sub.add_parser("query", parents=[common], help="search pages, or inspect one page's links")
    query.add_argument("text", nargs="?")
    query.add_argument("--page")
    query.add_argument("--limit", type=int, default=8)
    page = sub.add_parser("page", help="write or delete pages")
    page_sub = page.add_subparsers(dest="page_command", required=True)
    put = page_sub.add_parser("put", parents=[common], help="create or replace a page")
    put.add_argument("--id", required=True)
    put.add_argument("--title", required=True)
    put.add_argument("--type", required=True)
    put.add_argument("--summary", required=True)
    put.add_argument("--source", action="append", default=[])
    put.add_argument("--body", required=True, help="file path, or - for stdin")
    put.add_argument("--status", default="stable", choices=["stable", "draft", "stale"])
    delete = page_sub.add_parser("delete", parents=[common], help="delete a page (irreversible)")
    delete.add_argument("id")
    delete.add_argument("--yes", action="store_true")
    settle = sub.add_parser("settle", parents=[common], help="close pending sources after curation")
    settle.add_argument("keys", nargs="*")
    settle.add_argument("--skip", action="store_true")
    sub.add_parser("status", parents=[common], help="pending sources and page counts")
    doctor = sub.add_parser("doctor", parents=[common], help="health checks; --fix repairs mechanical issues")
    doctor.add_argument("--fix", action="store_true")
    return parser


ARGS = None
COMMANDS = {
    "init": command_init,
    "ingest": command_ingest,
    "read": command_read,
    "query": command_query,
    "settle": command_settle,
    "status": command_status,
    "doctor": command_doctor,
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
        handler = command_page_put if ARGS.page_command == "put" else command_page_delete
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
