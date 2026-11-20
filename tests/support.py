"""Shared plumbing for the end-to-end self test.

Holds the repository paths, the subprocess helpers, the check recorder and the
``Ctx`` object that carries state across check groups. Importing this module also
puts the engine's script directory on ``sys.path``, so every ``checks_*`` module
imports ``support`` before it imports the engine itself.

Run the whole suite with ``python3 tests/selftest.py``.
"""

import json
import subprocess
import sys
from dataclasses import dataclass
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills" / "llm-wiki" / "scripts" / "wiki.py"
sys.path.insert(0, str(SCRIPT.parent))

from runtime import require_python  # noqa: E402

require_python()

WIKI_CACHE = ".wiki"


@dataclass
class Ctx:
    """Run state shared by the check groups.

    ``results`` is the single list every check appends to; the remaining fields
    carry values one group hands to a later one.
    """

    root: Path
    results: list
    workflow_main: object = None
    payload: object = None
    c_key: object = None


def check(results, name, condition, detail=""):
    results.append((name, bool(condition), detail))
    if not condition:
        raise AssertionError(f"{name}: {detail}")


def run(root, *args, expect=0, stdin=None, timeout=None):
    proc = subprocess.run(
        [sys.executable, str(SCRIPT), *args, "--json"],
        cwd=root, capture_output=True, text=True, input=stdin, timeout=timeout,
    )
    payload = json.loads(proc.stdout) if proc.stdout.strip() else {}
    allowed = expect if isinstance(expect, tuple) else (expect,)
    if proc.returncode not in allowed:
        raise AssertionError(
            f"{' '.join(args)} exited {proc.returncode}, expected {expect}: {proc.stderr or proc.stdout}"
        )
    return payload


def run_text(root, *args):
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        cwd=root, capture_output=True, text=True)


def payload_file(root, name, data):
    path = Path(root) / WIKI_CACHE / "payloads" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, ensure_ascii=False), encoding="utf-8")
    return str(path)


def route_query(root, task, *words):
    """Register topic words: a route submit without a registered query is refused."""
    return run(root, "query", "--task", task["id"], *(words or ["主题"]))


def body_file(root):
    path = Path(root) / WIKI_CACHE / "drafts" / "page.md"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "## 结论\n\n停机后本人持证到厅复机，约两个工作日。\n\n"
        "## 步骤\n\n本人带身份证到营业厅提交复机申请。\n\n"
        "## 注意事项\n\n异地办理与资费以当地营业厅为准。\n",
        encoding="utf-8",
    )
    return path
