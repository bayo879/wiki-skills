"""Runtime self-check for an installed llm-wiki skill.

Two kinds of checks live here, and they are not interchangeable:

- 安装级：解释器版本、技能文件是否齐全、工作区是否可写。修不了，要人处理。
- 账本级：账本缺失、缺表、缺列、缺配置。引擎在打开账本时自己补齐。

`require_install` 只跑前者；后者只用于 `check` / `doctor` 的诊断输出，任何命令都不会
因为它拒绝执行。The end-to-end test suite lives in tests/selftest.py and is not shipped.
"""

from __future__ import annotations

import os
import sqlite3
import sys
import tempfile
from pathlib import Path

from errors import MANUAL_PATH, WikiError
from ledger import REQUIRED_COLUMNS, TABLES
from runtime import MINIMUM, MINIMUM_TEXT

SCRIPT_FILES = (
    "wiki.py",
    "cli.py",
    "cli_common.py",
    "cli_ingest.py",
    "cli_submit.py",
    "cli_workspace.py",
    "cli_query.py",
    "cli_page.py",
    "cli_workflow.py",
    "ledger.py",
    "pages.py",
    "view.py",
    "task_store.py",
    "ingest.py",
    "util.py",
    "errors.py",
    "frontmatter.py",
    "search.py",
    "workflows.py",
    "topics.py",
    "tasks.py",
    "validation.py",
    "workspace.py",
    "runtime.py",
    "health.py",
)
TEMPLATES = ("concept.md", "entity.md", "process.md")
REFERENCES = ("curation.md", "recovery.md")
BUILTIN_WORKFLOWS = ("topic-exploration", "workflow-review", "workflow-create")

LEDGER_OK = "ok"
LEDGER_REPAIRABLE = "repairable"
LEDGER_BROKEN = "broken"

INSTALL_NEXT = {
    "python": f"install Python {MINIMUM_TEXT}+ and rerun; on Windows run scripts\\bootstrap.ps1",
    "workspace": "run the command from a writable directory inside the workspace",
}


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


def _column_gaps(conn, tables):
    gaps = []
    for table, columns in REQUIRED_COLUMNS:
        if table not in tables:
            continue
        present = {row[1] for row in conn.execute(f"PRAGMA table_info({table})")}
        missing = [name for name in columns if name not in present]
        if missing:
            gaps.append(f"{table}({', '.join(missing)})")
    return gaps


def _has_rows(conn, tables):
    for table in TABLES:
        if table in tables and conn.execute(f"SELECT 1 FROM {table} LIMIT 1").fetchone():
            return True
    return False


def _ledger_probe(workspace, deep):
    """Return (state, detail); state is ok, repairable, or broken.

    Repairable means the next command that opens the ledger can fix it without
    losing anything; broken means it needs a human and a backup.
    """
    wiki_dir = workspace / ".wiki"
    db = wiki_dir / "wiki.db"
    config = wiki_dir / "wiki.json"
    if not wiki_dir.exists():
        return LEDGER_OK, "no wiki yet"
    if not db.is_file():
        if not config.is_file():
            # 只有锁文件、任务目录这类工作区脚手架，没有账本也没有配置：等同于未初始化。
            return LEDGER_OK, "no ledger yet"
        return LEDGER_REPAIRABLE, f"{wiki_dir} has no wiki.db; the next command creates it"
    if not config.is_file():
        return LEDGER_REPAIRABLE, "wiki.json is missing; the next command recreates it"
    conn = None
    try:
        conn = sqlite3.connect(f"{db.resolve().as_uri()}?mode=ro", uri=True)
        if deep:
            integrity = conn.execute("PRAGMA integrity_check").fetchone()[0]
            if integrity != "ok":
                return LEDGER_BROKEN, f"database integrity: {integrity}"
        tables = {
            row[0]
            for row in conn.execute("SELECT name FROM sqlite_master WHERE type = 'table'")
        }
        missing = sorted(set(TABLES) - tables)
        if missing:
            return LEDGER_REPAIRABLE, f"missing tables: {', '.join(missing)}"
        gaps = _column_gaps(conn, tables)
        if gaps:
            if _has_rows(conn, tables):
                return LEDGER_BROKEN, f"schema is older than this skill: {', '.join(gaps)}"
            return LEDGER_REPAIRABLE, f"empty tables with an older schema: {', '.join(gaps)}"
        return LEDGER_OK, "integrity ok" if deep else "readable"
    except sqlite3.DatabaseError as error:
        return LEDGER_BROKEN, f"database unreadable: {error}"
    finally:
        if conn is not None:
            conn.close()


def _ledger_check(workspace, deep):
    state, detail = _ledger_probe(workspace, deep)
    if state == LEDGER_REPAIRABLE:
        return True, f"repairable: {detail}"
    return state == LEDGER_OK, detail


def check_installation(skill_dir, workspace, deep=True, include_ledger=True):
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
    for name in REFERENCES:
        checks.append(
            _check(
                f"reference:{name}",
                lambda name=name: (
                    (skill_dir.parent / "references" / name).is_file(),
                    str(skill_dir.parent / "references" / name),
                ),
            )
        )
    for name in BUILTIN_WORKFLOWS:
        checks.append(
            _check(
                f"workflow:{name}",
                lambda name=name: (
                    (skill_dir.parent / "workflows" / name / "WORKFLOW.md").is_file(),
                    str(skill_dir.parent / "workflows" / name / "WORKFLOW.md"),
                ),
            )
        )
    checks.append(_check("workspace", lambda: _workspace_probe(workspace)))
    if include_ledger:
        checks.append(_check("ledger", lambda: _ledger_check(workspace, deep)))
    return {
        "ok": all(item["ok"] for item in checks),
        "doc": str(MANUAL_PATH),
        "checks": checks,
    }


def _next_for(name):
    if name in INSTALL_NEXT:
        return INSTALL_NEXT[name]
    if name.startswith(("file:", "template:", "workflow:", "reference:")):
        return "reinstall the skill so its files are complete, then rerun"
    return "fix the reported item, then rerun"


def require_install(skill_dir, workspace):
    """Raise E_HEALTH for problems no command can repair by itself."""
    report = check_installation(
        skill_dir, workspace, deep=False, include_ledger=False
    )
    if report["ok"]:
        return report
    failed = next(item for item in report["checks"] if not item["ok"])
    section = "工作区不可写" if failed["name"] == "workspace" else "运行时与安装"
    raise WikiError(
        "E_HEALTH",
        f"self-check failed: {failed['name']}: {failed['detail']}",
        _next_for(failed["name"]),
        1,
        section=section,
    )
