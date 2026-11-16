"""Read-only scenario capabilities stored under `<root>/场景能力/`.

A scenario is shaped like a skill:

    场景能力/<id>/SCENARIO.md      flow + frontmatter (name, description, triggers)
    场景能力/<id>/scripts/         executable helpers
    场景能力/<id>/references/      notes read on demand

Scenarios are not knowledge pages: `ingest` skips the directory, nothing is
written to `.wiki/`, and `query` reads the files directly. This keeps a flow
editable without re-ingesting it, while `scenario load` still returns the full
flow plus an inventory of the support files.

The skill also ships built-in scenarios under `<skill>/scenarios/`. They are
read-only and always available, and a project scenario with the same id takes
precedence, so a project can override the default flow. Built-in flows are
self-contained: they list no resources and their `path` values are display
labels rather than workspace paths.
"""

from __future__ import annotations

import os
import re
from pathlib import Path

from errors import WikiError
from frontmatter import split_frontmatter

SCENARIO_DIR = "场景能力"
SCENARIO_FILE = "SCENARIO.md"
MAX_SCENARIO_CHARS = 200_000
BUILTIN_DIR = "scenarios"
BUILTIN_LABEL = "内置场景"
SKILL_ROOT = Path(__file__).resolve().parent.parent


def scenario_root(root):
    return Path(root) / SCENARIO_DIR


def builtin_root():
    return SKILL_ROOT / BUILTIN_DIR


def inventory(root):
    """List every scenario directory, valid or not."""
    entries = scan(root)
    return {
        "root": SCENARIO_DIR,
        "count": len(entries),
        "valid": sum(1 for entry in entries if entry["valid"]),
        "scenarios": [_public(entry) for entry in entries],
    }


def scan(root, builtin=True):
    """Return full internal records for project scenarios, then built-ins.

    A project scenario shadows a built-in one with the same id.
    """
    root = Path(root)
    entries = [
        _entry(root, child)
        for child in _children(scenario_root(root))
    ]
    if not builtin:
        return entries
    seen = {entry["id"] for entry in entries}
    base = builtin_root()
    for child in _children(base):
        if child.name in seen:
            continue
        entries.append(_builtin_entry(base, child))
    return entries


def _children(base):
    if not base.is_dir():
        return []
    try:
        return sorted(
            (
                child
                for child in base.iterdir()
                if child.is_dir() and not child.name.startswith(".")
            ),
            key=lambda path: path.name,
        )
    except OSError:
        return []


def load(root, scenario_id):
    """Return one scenario's flow, metadata and resource inventory."""
    scenario_id = _valid_id(scenario_id)
    root = Path(root)
    entry = next(
        (item for item in scan(root) if item["id"] == scenario_id),
        None,
    )
    if entry is None:
        raise WikiError(
            "E_NOT_FOUND",
            f"no scenario {scenario_id}",
            "run `scenario list` to see available scenarios",
            3,
        )
    if not entry["valid"]:
        raise WikiError(
            "E_SCENARIO_INVALID",
            f"scenario {scenario_id} is invalid: {entry['error']}",
            "run `scenario list` and fix the scenario directory",
            2,
        )
    return {
        "scenario": _public(entry),
        "path": entry["main"],
        "content": entry["text"],
        "meta": entry["meta"],
        "resources": entry["resources"],
    }


def match(root, terms, limit, builtin=True):
    """Rank valid scenarios against search terms; reads only files."""
    terms = [term for term in terms if term]
    if not terms:
        return []
    scored = []
    for entry in scan(root, builtin=builtin):
        if not entry["valid"]:
            continue
        title = entry["name"]
        description = entry["description"]
        triggers = " ".join(entry["triggers"])
        haystack = f"{title}\n{description}\n{triggers}\n{entry['body']}"
        hits = sum(haystack.lower().count(term.lower()) for term in terms)
        if hits == 0:
            continue
        score = hits + 5 * sum(title.lower().count(term.lower()) for term in terms)
        score += 3 * sum(
            description.lower().count(term.lower()) + triggers.lower().count(term.lower())
            for term in terms
        )
        scored.append(
            {
                "id": entry["id"],
                "source": entry["source"],
                "name": title,
                "description": description,
                "triggers": entry["triggers"],
                "path": entry["path"],
                "main": entry["main"],
                "load": f'scenario load "{entry["id"]}"',
                "score": score,
                "resources": len(entry["resources"]),
            }
        )
    scored.sort(key=lambda item: (-item["score"], item["id"]))
    return scored[:max(1, int(limit))]


def _builtin_entry(base, directory):
    entry = _entry(base, directory, source="builtin")
    entry["path"] = f"{BUILTIN_LABEL}/{entry['path']}"
    entry["main"] = f"{BUILTIN_LABEL}/{entry['main']}"
    entry["resources"] = []
    return entry


def _entry(root, directory, source="project"):
    root = Path(root)
    record = {
        "id": directory.name,
        "source": source,
        "name": directory.name,
        "description": "",
        "triggers": [],
        "path": directory.relative_to(root).as_posix(),
        "main": (directory / SCENARIO_FILE).relative_to(root).as_posix(),
        "valid": False,
        "error": "",
        "resources": [],
    }
    if not _within(root, directory):
        record["error"] = "scenario directory escapes the workspace root"
        return record
    record["resources"] = _resources(directory, root)
    main = directory / SCENARIO_FILE
    if main.is_symlink() and not _within(root, main):
        record["error"] = f"{SCENARIO_FILE} escapes the workspace root"
        return record
    if not main.is_file():
        record["error"] = f"missing {SCENARIO_FILE}"
        return record
    try:
        text = main.read_text(encoding="utf-8", errors="replace")
    except OSError as error:
        record["error"] = f"cannot read {SCENARIO_FILE}: {error}"
        return record
    if len(text) > MAX_SCENARIO_CHARS:
        record["error"] = f"{SCENARIO_FILE} is larger than {MAX_SCENARIO_CHARS} characters"
        return record
    meta, body = split_frontmatter(text)
    body = body.strip()
    name = str(meta.get("name") or directory.name).strip() or directory.name
    description = str(meta.get("description") or "").strip()
    record.update(
        {
            "name": name,
            "description": description,
            "triggers": _triggers(meta.get("triggers")),
            "meta": meta,
            "text": text,
            "body": body,
        }
    )
    if not description:
        record["error"] = "frontmatter description is required"
    elif not body:
        record["error"] = f"{SCENARIO_FILE} body is empty"
    else:
        record["valid"] = True
    return record


def _public(entry):
    return {
        "id": entry["id"],
        "source": entry["source"],
        "name": entry["name"],
        "description": entry["description"],
        "triggers": entry["triggers"],
        "path": entry["path"],
        "main": entry["main"],
        "valid": entry["valid"],
        "error": entry["error"],
        "resources": entry["resources"],
    }


def _triggers(value):
    raw = value if isinstance(value, list) else [value] if value else []
    triggers = []
    for item in raw:
        for part in re.split(r"[,，、;；]+", str(item)):
            part = part.strip()
            if part and part not in triggers:
                triggers.append(part)
    return triggers


def _resources(directory, root):
    items = []
    for current, dirnames, filenames in os.walk(directory):
        dirnames[:] = sorted(name for name in dirnames if not name.startswith("."))
        for filename in sorted(filenames):
            if filename.startswith("."):
                continue
            path = Path(current) / filename
            if path.parent == directory and path.name == SCENARIO_FILE:
                continue
            if path.is_symlink() and not _within(root, path):
                continue
            try:
                size = path.stat().st_size
            except OSError:
                continue
            items.append(
                {
                    "path": path.relative_to(root).as_posix(),
                    "kind": _kind(path, directory),
                    "size": size,
                }
            )
    items.sort(key=lambda item: item["path"])
    return items


def _kind(path, directory):
    parts = path.relative_to(directory).parts
    if parts and parts[0] == "scripts":
        return "script"
    if parts and parts[0] == "references":
        return "reference"
    return "other"


def _valid_id(value):
    text = str(value or "").strip()
    if not text or text.startswith(".") or text != os.path.basename(text):
        raise WikiError(
            "E_USAGE",
            f"invalid scenario id {value!r}",
            "run `scenario list` and pass a directory name",
            2,
        )
    if any(char in text for char in ("/", "\\", "\x00")):
        raise WikiError(
            "E_USAGE",
            f"invalid scenario id {value!r}",
            "run `scenario list` and pass a directory name",
            2,
        )
    return text


def _within(root, path):
    try:
        Path(path).resolve().relative_to(Path(root).resolve())
        return True
    except ValueError:
        return False
