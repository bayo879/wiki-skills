"""Read-only topic notes stored under `<root>/话题/`.

A topic is one exploration record: the question, the answer, its evidence and
what still needs digging. Topics are the wiki's exploration layer: Markdown
files written by the agent, not formal knowledge pages. `ingest` skips the
directory, nothing enters the ledger, and `query` reads the files directly so a
topic can be updated without re-ingesting.
"""

from __future__ import annotations

from pathlib import Path

from frontmatter import split_frontmatter

TOPIC_DIR = "话题"
TOPIC_SUFFIX = ".md"
MAX_TOPIC_CHARS = 200_000


def topic_root(root):
    return Path(root) / TOPIC_DIR


def scan(root):
    """Return one record per topic file, including unreadable or empty ones."""
    root = Path(root)
    base = topic_root(root)
    if not base.is_dir():
        return []
    try:
        children = sorted(base.iterdir(), key=lambda path: path.name)
    except OSError:
        return []
    return [
        _entry(root, child)
        for child in children
        if child.is_file() and child.suffix == TOPIC_SUFFIX and not child.name.startswith(".")
    ]


def match(root, terms, limit):
    """Rank valid topics against query terms; reads files, writes nothing."""
    terms = [term for term in terms if term]
    if not terms:
        return []
    scored = []
    for entry in scan(root):
        if not entry["valid"]:
            continue
        haystack = f"{entry['title']}\n{entry['description']}\n{entry['body']}"
        hits = sum(haystack.lower().count(term.lower()) for term in terms)
        if hits == 0:
            continue
        score = hits + 5 * sum(entry["title"].lower().count(term.lower()) for term in terms)
        score += 3 * sum(
            entry["description"].lower().count(term.lower()) for term in terms
        )
        scored.append(
            {
                "name": entry["name"],
                "title": entry["title"],
                "description": entry["description"],
                "status": entry["status"],
                "updated": entry["updated"],
                "path": entry["path"],
                "score": score,
                "snippet": snippet(entry["body"], terms[0]),
            }
        )
    scored.sort(key=lambda item: (-item["score"], item["name"]))
    return scored[:max(1, int(limit))]


def snippet(body, term):
    flat = " ".join(body.split())
    index = flat.lower().find(term.lower())
    if index < 0:
        return flat[:120]
    start = max(0, index - 60)
    end = min(len(flat), index + len(term) + 90)
    prefix = "..." if start > 0 else ""
    suffix = "..." if end < len(flat) else ""
    return prefix + flat[start:end] + suffix


def _entry(root, path):
    root = Path(root)
    record = {
        "name": path.stem,
        "title": path.stem,
        "description": "",
        "status": "",
        "updated": "",
        "path": path.relative_to(root).as_posix(),
        "valid": False,
        "error": "",
        "body": "",
    }
    if path.is_symlink() and not _within(root, path):
        record["error"] = "topic file escapes the workspace root"
        return record
    try:
        text = path.read_text(encoding="utf-8", errors="replace")
    except OSError as error:
        record["error"] = f"cannot read topic: {error}"
        return record
    if len(text) > MAX_TOPIC_CHARS:
        record["error"] = f"topic is larger than {MAX_TOPIC_CHARS} characters"
        return record
    meta, body = split_frontmatter(text)
    body = body.strip()
    title = str(meta.get("name") or meta.get("title") or path.stem).strip() or path.stem
    description = str(meta.get("description") or "").strip()
    if not description:
        description = _first_line(body)
    record.update(
        {
            "title": title,
            "description": description,
            "status": str(meta.get("status") or "").strip(),
            "updated": str(meta.get("updated") or "").strip(),
            "body": body,
            "valid": bool(body),
        }
    )
    if not body:
        record["error"] = "topic body is empty"
    return record


def _first_line(body):
    for line in body.splitlines():
        line = line.strip().lstrip("#").strip()
        if line:
            return line[:120]
    return ""


def _within(root, path):
    try:
        Path(path).resolve().relative_to(Path(root).resolve())
        return True
    except ValueError:
        return False
