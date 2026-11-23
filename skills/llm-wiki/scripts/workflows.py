"""Read-only executable workflows stored under `<view>/workflows/`.

A workflow is shaped like a skill:

    <view>/workflows/<id>/WORKFLOW.md     flow + frontmatter
    <view>/workflows/<id>/scripts/        executable helpers
    <view>/workflows/<id>/references/     notes read on demand

The legacy `<root>/workflows/` directory is read only as a fallback.

A workflow is executable: every step in its `## 步骤` section carries one of
`tool:`, `human:`, `agent:` or `verify:`. The engine reads those markers so
`workflow load` can tell an agent which steps call a tool, which need a human,
which it decides itself, and which verify an earlier result. A workflow with
typed steps but no `tool:` step is a manual procedure: it is invalid here and
belongs on a `process` page. Files written before the contract existed are
still readable with a warning; `UNTYPED_ALLOWED` is flipped off in 0.6.0.

Workflows are not knowledge pages: `ingest` skips the directory, nothing is
written to `.wiki/`, and `query` reads the files directly. This keeps a flow
editable without re-ingesting it, while `workflow load` still returns the full
flow plus an inventory of the support files.

The skill also ships built-in workflows under `<skill>/workflows/`. They are
read-only and always available, and a project workflow with the same id takes
precedence, so a project can override the default flow. Built-in flows are
self-contained: they list no resources and report `skill_path` (relative to
the skill's own `workflows/` directory) instead of the workspace-relative
`path`/`main` a project workflow carries.
"""

from __future__ import annotations

import os
import re
from pathlib import Path

from errors import WikiError
from frontmatter import split_frontmatter
from workflow_steps import (
    UNTYPED_ALLOWED,
    _applies,
    _contract,
    parse_steps,
)

WORKFLOW_DIR = "workflows"
WORKFLOW_FILE = "WORKFLOW.md"
# Callers that reserve the workflow directory (ingest skip, publish.dir) read
# this instead of hardcoding the name.
WORKFLOW_DIRS = (WORKFLOW_DIR,)
MAX_WORKFLOW_CHARS = 200_000
BUILTIN_DIR = "workflows"

SKILL_ROOT = Path(__file__).resolve().parent.parent


def workflow_root(root, view_root=None):
    if view_root is not None:
        return Path(view_root)
    return Path(root) / WORKFLOW_DIR


def workflow_roots(root, view_root=None):
    """Existing project workflow directories, view first, then legacy root."""
    bases = []
    if view_root is not None:
        bases.append(Path(view_root))
    bases.append(Path(root) / WORKFLOW_DIR)
    roots = []
    for base in bases:
        if base.is_dir() and base not in roots:
            roots.append(base)
    return roots


def builtin_root():
    return SKILL_ROOT / BUILTIN_DIR


def inventory(root, view_root=None):
    """List every workflow directory, valid or not."""
    root = Path(root)
    entries = scan(root, view_root=view_root)
    paths = workflow_roots(root, view_root=view_root)
    roots = [path.relative_to(root).as_posix() for path in paths]
    if roots:
        first = roots[0]
    elif view_root is not None:
        first = Path(view_root).relative_to(root).as_posix()
    else:
        first = WORKFLOW_DIR
    return {
        "root": first,
        "roots": roots,
        "count": len(entries),
        "valid": sum(1 for entry in entries if entry["valid"]),
        "warned": sum(1 for entry in entries if entry["warnings"]),
        "workflows": [_public(entry) for entry in entries],
    }


def scan(root, builtin=True, view_root=None):
    """Return project workflows (view, legacy root), then built-ins.

    The view wins over the legacy root and the legacy root wins over a built-in
    with the same id. Invalid directories still occupy their id.
    """
    root = Path(root)
    entries = []
    seen = set()
    if view_root is not None:
        for child in _children(Path(view_root)):
            if child.name in seen:
                continue
            seen.add(child.name)
            entries.append(_entry(
                root, child, origin="view",
                legacy_path=_legacy_conflict(root, child),
            ))
    for child in _children(root / WORKFLOW_DIR):
        if child.name in seen:
            continue
        seen.add(child.name)
        entries.append(_entry(root, child, origin="legacy"))
    if not builtin:
        return entries
    seen.update(entry["id"] for entry in entries)
    base = builtin_root()
    for child in _children(base):
        if child.name in seen:
            continue
        entries.append(_builtin_entry(base, child))
    return entries


def _legacy_conflict(root, child):
    """Return the legacy WORKFLOW path when both definitions exist and differ."""
    view_main = child / WORKFLOW_FILE
    legacy_main = Path(root) / WORKFLOW_DIR / child.name / WORKFLOW_FILE
    if not view_main.is_file() or not legacy_main.is_file():
        return None
    try:
        if view_main.read_text(encoding="utf-8", errors="replace") == legacy_main.read_text(
            encoding="utf-8", errors="replace"
        ):
            return None
    except OSError:
        return None
    return f"{WORKFLOW_DIR}/{child.name}/{WORKFLOW_FILE}"


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


def load(root, workflow_id, view_root=None):
    """Return one workflow's flow, metadata, step inventory and resources."""
    workflow_id = _valid_id(workflow_id)
    root = Path(root)
    entry = next(
        (item for item in scan(root, view_root=view_root) if item["id"] == workflow_id),
        None,
    )
    if entry is None:
        raise WikiError(
            "E_NOT_FOUND",
            f"no workflow {workflow_id}",
            "run `workflow list` to see available workflows",
            3,
        )
    if not entry["valid"]:
        raise WikiError(
            "E_WORKFLOW_INVALID",
            f"workflow {workflow_id} is invalid: {entry['error']}",
            entry["hint"] or "run `workflow list` and fix the workflow directory",
            2,
        )
    where = (
        {"skill_path": entry["skill_path"]}
        if entry["source"] == "builtin"
        else {"path": entry["main"]}
    )
    return {
        "workflow": _public(entry),
        **where,
        "content": entry["text"],
        "meta": entry["meta"],
        "resources": entry["resources"],
        "tools": entry["tools"],
        "human_steps": entry["human_steps"],
        "agent_steps": entry["agent_steps"],
        "verify_steps": entry["verify_steps"],
        "warnings": entry["warnings"],
    }


def match(root, terms, limit, builtin=True, view_root=None):
    """Rank valid workflows against search terms; reads only files."""
    terms = [term for term in terms if term]
    if not terms:
        return []
    scored = []
    for entry in scan(root, builtin=builtin, view_root=view_root):
        if not entry["valid"]:
            continue
        fields = (
            (entry["name"], 4),
            (entry["alias_name"], 4),
            (entry["description"], 2),
            (" ".join(entry["tags"]), 6),
            (entry["body"], 1),
        )
        score = sum(
            weight * sum(text.lower().count(term.lower()) for term in terms)
            for text, weight in fields
        )
        if score == 0:
            continue
        scored.append(
            {
                "id": entry["id"],
                "source": entry["source"],
                "origin": entry["origin"],
                "name": entry["name"],
                "alias_name": entry["alias_name"],
                "description": entry["description"],
                "tags": entry["tags"],
                **_location(entry),
                "load": f'workflow load "{entry["id"]}"',
                "score": score,
                "applies": _applies(entry["body"]),
                "resources": len(entry["resources"]),
                "tools": len(entry["tools"]),
                "human_steps": len(entry["human_steps"]),
                "verify_steps": len(entry["verify_steps"]),
                "warnings": len(entry["warnings"]),
            }
        )
    scored.sort(key=lambda item: (-item["score"], item["id"]))
    return scored[:max(1, int(limit))]


def _builtin_entry(base, directory):
    entry = _entry(base, directory, source="builtin", origin="builtin")
    entry["skill_path"] = f"{BUILTIN_DIR}/{entry.pop('main')}"
    entry.pop("path")
    entry["resources"] = []
    return entry


def _entry(root, directory, source="project", origin="view", legacy_path=None):
    root = Path(root)
    record = {
        "id": directory.name,
        "source": source,
        "origin": origin,
        "name": directory.name,
        "alias_name": "",
        "description": "",
        "tags": [],
        "path": directory.relative_to(root).as_posix(),
        "main": (directory / WORKFLOW_FILE).relative_to(root).as_posix(),
        "valid": False,
        "error": "",
        "hint": "",
        "resources": [],
        "steps": [],
        "tools": [],
        "human_steps": [],
        "agent_steps": [],
        "verify_steps": [],
        "warnings": [],
    }
    if legacy_path:
        record["legacy_path"] = legacy_path
        record["legacy_conflict"] = True
    if not _within(root, directory):
        record["error"] = "workflow directory escapes the workspace root"
        return record
    record["resources"] = _resources(directory, root)
    main = directory / WORKFLOW_FILE
    record["main"] = main.relative_to(root).as_posix()
    if main.is_symlink() and not _within(root, main):
        record["error"] = f"{main.name} escapes the workspace root"
        return record
    if not main.is_file():
        record["error"] = f"missing {WORKFLOW_FILE}"
        return record
    try:
        text = main.read_text(encoding="utf-8", errors="replace")
    except OSError as error:
        record["error"] = f"cannot read {main.name}: {error}"
        return record
    if len(text) > MAX_WORKFLOW_CHARS:
        record["error"] = f"{main.name} is larger than {MAX_WORKFLOW_CHARS} characters"
        return record
    meta, body = split_frontmatter(text)
    body = body.strip()
    name = str(meta.get("name") or directory.name).strip() or directory.name
    alias_name = str(meta.get("alias_name") or "").strip()
    description = str(meta.get("description") or "").strip()
    steps, has_section = parse_steps(body)
    record.update(
        {
            "name": name,
            "alias_name": alias_name,
            "description": description,
            "tags": _tags(meta.get("tags")),
            "meta": meta,
            "text": text,
            "body": body,
            "steps": steps,
            "tools": _steps_of(steps, "tool"),
            "human_steps": _steps_of(steps, "human"),
            "agent_steps": _steps_of(steps, "agent"),
            "verify_steps": _steps_of(steps, "verify"),
        }
    )
    warnings, errors, hint = _contract(steps, has_section)
    record["warnings"] = warnings
    if not description:
        record["error"] = "frontmatter description is required"
    elif not body:
        record["error"] = f"{main.name} body is empty"
    elif errors:
        record["error"] = errors[0]
        record["hint"] = hint
    else:
        record["valid"] = True
    return record


def _steps_of(steps, kind):
    return [
        {"line": step["line"], "text": step["text"]}
        for step in steps
        if step["type"] == kind
    ]


def _location(entry):
    """Return the flow's location, keyed by the base its path is relative to."""
    if entry["source"] == "builtin":
        return {"skill_path": entry["skill_path"]}
    return {"path": entry["path"], "main": entry["main"]}


def _public(entry):
    item = {
        "id": entry["id"],
        "source": entry["source"],
        "origin": entry["origin"],
        "name": entry["name"],
        "alias_name": entry["alias_name"],
        "description": entry["description"],
        "tags": entry["tags"],
        **_location(entry),
        "valid": entry["valid"],
        "error": entry["error"],
        "resources": entry["resources"],
        "tools": len(entry["tools"]),
        "human_steps": len(entry["human_steps"]),
        "agent_steps": len(entry["agent_steps"]),
        "verify_steps": len(entry["verify_steps"]),
        "warnings": entry["warnings"],
    }
    if entry.get("legacy_conflict"):
        item["legacy_path"] = entry["legacy_path"]
        item["legacy_conflict"] = True
    return item


def _tags(value):
    raw = value if isinstance(value, list) else [value] if value else []
    tags = []
    seen = set()
    for item in raw:
        for part in re.split(r"[,，、;；#\s]+", str(item)):
            tag = _tag(part)
            if not tag or tag.lower() in seen:
                continue
            seen.add(tag.lower())
            tags.append(tag)
    return tags


def _tag(part):
    """Normalise one fragment to an Obsidian tag: no '#', no spaces, nesting via '/'."""
    chars = []
    for char in str(part).strip():
        if char == "#":
            continue
        if char.isspace():
            chars.append("-")
        elif char.isalnum() or char in "_-/":
            chars.append(char)
    tag = re.sub(r"-{2,}", "-", "".join(chars))
    tag = re.sub(r"/{2,}", "/", tag).strip("-/")
    return "" if not tag or tag.isdigit() else tag


def _resources(directory, root):
    items = []
    for current, dirnames, filenames in os.walk(directory):
        dirnames[:] = sorted(name for name in dirnames if not name.startswith("."))
        for filename in sorted(filenames):
            if filename.startswith("."):
                continue
            path = Path(current) / filename
            if path.parent == directory and path.name == WORKFLOW_FILE:
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
            f"invalid workflow id {value!r}",
            "run `workflow list` and pass a directory name",
            2,
        )
    if any(char in text for char in ("/", "\\", "\x00")):
        raise WikiError(
            "E_USAGE",
            f"invalid workflow id {value!r}",
            "run `workflow list` and pass a directory name",
            2,
        )
    return text


def _within(root, path):
    try:
        Path(path).resolve().relative_to(Path(root).resolve())
        return True
    except ValueError:
        return False
