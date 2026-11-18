"""把任意来源变成文本与事实：解码、解析、事实采集与新鲜度判定。

本模块只依赖标准库、util 与 errors；不引用 pages/view/ledger，
因此可以被它们任意一层向下调用。
"""

from __future__ import annotations

import html
import re
import shutil
import subprocess
import zipfile
from html.parser import HTMLParser
from pathlib import Path

from errors import WikiError
from frontmatter import split_frontmatter
from util import body_digest, digest_bytes, same_stamp


MAX_RAW_CHARS = 4_000_000


TEXT_SUFFIXES = {
    ".md", ".markdown", ".txt", ".rst", ".log", ".csv", ".tsv", ".yaml", ".yml",
    ".toml", ".ini", ".cfg", ".json", ".xml", ".sql", ".py", ".rs", ".js", ".ts",
    ".css", ".sh", ".java", ".go", ".rb", ".c", ".h", ".cpp",
}


HTML_SUFFIXES = {".html", ".htm"}


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


def raw_body_digest(path):
    """Digest of one raw copy's body: frontmatter is ledger state, not content."""
    text = Path(path).read_text(encoding="utf-8", errors="replace")
    _, body = split_frontmatter(text)
    return body_digest(body)


def raw_facts(root, row):
    """(state, size, mtime)；state ∈ {"ok","missing","edited"}。"""
    if row is None or not row["raw"]:
        return "missing", 0, 0.0
    path = Path(root) / row["raw"]
    try:
        stat = path.stat()
    except OSError:
        return "missing", 0, 0.0
    if not row["raw_digest"]:
        return "edited", stat.st_size, stat.st_mtime
    if same_stamp(stat.st_size, stat.st_mtime, row["raw_size"], row["raw_mtime"]):
        return "ok", stat.st_size, stat.st_mtime
    try:
        digest = raw_body_digest(path)
    except OSError:
        return "missing", stat.st_size, stat.st_mtime
    if digest == row["raw_digest"]:
        return "ok", stat.st_size, stat.st_mtime
    return "edited", stat.st_size, stat.st_mtime


def ingest_decision(source_changed, raw_state, source_mtime, raw_mtime, force=False):
    """规格 §源与原文判定 的一行 → 本次 ingest 对该来源的动作。"""
    if force:
        # 强制重算只看 mtime：原文不比源旧就绝不覆盖（原文是用户的文件）。
        if raw_state == "missing" or source_mtime > raw_mtime:
            return "rewrite"
        return "parse_keep_raw"
    if source_changed:
        if raw_state != "edited" or source_mtime > raw_mtime:
            return "rewrite"
        return "bookkeep_source"
    if raw_state == "missing":
        return "rewrite"
    if raw_state == "edited":
        return "bookkeep_raw"
    return "skip"


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
