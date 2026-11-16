"""Runtime self-check for an installed llm-wiki skill.

This is deliberately small: it verifies the interpreter, the files an installed
copy must contain, the workspace probe, and the existing ledger when there is
one. The end-to-end test suite lives in tests/selftest.py and is not shipped.
"""

from __future__ import annotations

import os
import sqlite3
import sys
import tempfile
from pathlib import Path

from errors import WikiError
from runtime import MINIMUM, MINIMUM_TEXT

SCRIPT_FILES = (
    "wiki.py",
    "errors.py",
    "tasks.py",
    "validation.py",
    "workspace.py",
    "runtime.py",
    "health.py",
)
TEMPLATES = ("concept.md", "entity.md", "process.md", "note.md")
TABLES = ("sources", "page_revs", "tasks", "task_sources", "links")


def _check(name, fn):
    try:
        ok, detail = fn()
    except Exception as error:  # a broken probe is a failed check, not a crash
        ok, detail = False, f"{type(error).__name__}: {error}"
    return {"name": name, "ok": bool(ok), "detail": str(detail)}


def _workspace_probe(workspace):
    if not workspace.is_dir():
        return False, f"{workspace} is not a directory"
    handle = None
    path = None
    try:
        handle, path = tempfile.mkstemp(prefix=".llm-wiki-check-", dir=workspace)
        os.close(handle)
        handle = None
        os.unlink(path)
        path = None
        return True, "writable"
    except OSError as error:
        return False, f"not writable: {error}"
    finally:
        if handle is not None:
            os.close(handle)
        if path is not None:
            try:
                os.unlink(path)
            except OSError:
                pass


def _ledger_probe(workspace, deep, allow_missing):
    wiki_dir = workspace / ".wiki"
    db = wiki_dir / "wiki.db"
    config = wiki_dir / "wiki.json"
    if not wiki_dir.exists():
        return True, "no wiki yet"
    if not db.is_file():
        if allow_missing:
            return True, "ledger missing; init will create it"
        return False, f"{wiki_dir} exists but wiki.db is missing"
    if not config.is_file():
        if allow_missing:
            return True, "config missing; init will recreate it"
        return False, f"{wiki_dir} exists but wiki.json is missing"
    try:
        conn = sqlite3.connect(f"{db.resolve().as_uri()}?mode=ro", uri=True)
        try:
            if deep:
                integrity = conn.execute("PRAGMA integrity_check").fetchone()[0]
                if integrity != "ok":
                    return False, f"database integrity: {integrity}"
            names = {
                row[0]
                for row in conn.execute(
                    "SELECT name FROM sqlite_master WHERE type = 'table'"
                )
            }
            if not deep:
                conn.execute("SELECT 1 FROM tasks LIMIT 1").fetchone()
        finally:
            conn.close()
    except sqlite3.DatabaseError as error:
        return False, f"database unreadable: {error}"
    missing = sorted(set(TABLES) - names)
    if missing:
        return False, f"missing tables: {', '.join(missing)}"
    return True, "integrity ok" if deep else "readable"


def check_installation(skill_dir, workspace, deep=True, allow_missing=False):
    """Return one report; every check runs even if an earlier one failed."""
    skill_dir = Path(skill_dir)
    workspace = Path(workspace)
    checks = [
        _check(
            "python",
            lambda: (
                sys.version_info[:2] >= MINIMUM,
                f"{sys.version.split()[0]} (need {MINIMUM_TEXT}+)",
            ),
        ),
    ]
    for name in SCRIPT_FILES:
        checks.append(
            _check(
                f"file:{name}",
                lambda name=name: (
                    (skill_dir / name).is_file(),
                    str(skill_dir / name),
                ),
            )
        )
    checks.append(
        _check(
            "file:SKILL.md",
            lambda: (
                (skill_dir.parent / "SKILL.md").is_file(),
                str(skill_dir.parent / "SKILL.md"),
            ),
        )
    )
    for name in TEMPLATES:
        checks.append(
            _check(
                f"template:{name}",
                lambda name=name: (
                    (skill_dir.parent / "templates" / name).is_file(),
                    str(skill_dir.parent / "templates" / name),
                ),
            )
        )
    checks.append(_check("workspace", lambda: _workspace_probe(workspace)))
    checks.append(_check("ledger", lambda: _ledger_probe(workspace, deep, allow_missing)))
    return {
        "ok": all(item["ok"] for item in checks),
        "checks": checks,
    }


def require(skill_dir, workspace, allow_missing=False):
    """Raise E_HEALTH before a state-changing command if any check fails."""
    report = check_installation(skill_dir, workspace, deep=False, allow_missing=allow_missing)
    if report["ok"]:
        return report
    failed = next(item for item in report["checks"] if not item["ok"])
    raise WikiError(
        "E_HEALTH",
        f"self-check failed: {failed['name']}: {failed['detail']}",
        "run `wiki.py check --json`; reinstall the skill if files are missing",
        1,
    )
