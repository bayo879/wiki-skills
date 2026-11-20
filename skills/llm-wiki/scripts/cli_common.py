"""各命令共用的小工具：参数信封（ARGS）、输出（emit/fail）、开账本与安装校验。

命令实现在 cli_ingest/cli_submit/cli_workspace/cli_query/cli_page/cli_workflow，
参数解析与分发在 cli.py。"""

from __future__ import annotations

import argparse
import health
import json
import os
import re
import sys
from pathlib import Path

from errors import WikiError
from ledger import CONFIG_NAME, Wiki
from tasks import task_dir_relative
from workspace import WIKI_DIR, dry_store, inside_root, window_safe


VERSION = "0.5.3"


ID_RE = re.compile(r"^[a-z0-9][a-z0-9._-]*$")


# 领取任务的会话身份：显式参数 > 用户导出 > 宿主注入的会话 id。
OWNER_ENV = ("LLM_WIKI_OWNER", "DSH_SESSION_ID")


def session_owner():
    """Which session a claim belongs to.

    DSH exports `DSH_SESSION_ID` for every session, so a subagent is a
    different owner without any setup; a plain terminal exports
    `LLM_WIKI_OWNER` once or passes `--owner` on each command. An empty
    result means "unattributed": the claim still works, but the queue cannot
    tell this session from another one, so `next` keeps the old wording
    instead of guessing.
    """
    candidates = [getattr(ARGS, "owner", "")] + [os.environ.get(name) for name in OWNER_ENV]
    for value in candidates:
        value = (value or "").strip()
        if value:
            return value
    return ""


# 当前命令打开账本时补齐的东西；emit 把它作为 repaired 输出。
ARGS = argparse.Namespace()
REPAIRED = []


def emit(cmd, root, summary, data, human_lines, ok=True):
    if ARGS.json:
        if REPAIRED:
            data = dict(data, repaired=list(REPAIRED))
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
            "error": error.as_dict(),
        }
        sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
    else:
        sys.stderr.write(f"[{error.code}] {error.message}\n")
        if error.next_step:
            sys.stderr.write(f"next: {error.next_step}\n")
    return error.exit_code


def open_wiki(require_sources=False, dry_run=False, push=True, readonly=False):
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
    if readonly:
        return Wiki(root).open_readonly(), []
    if dry_run and not (root / WIKI_DIR / CONFIG_NAME).exists():
        # A dry run must not create `.wiki/` in a directory the user is only
        # inspecting, so plan against a throwaway store under `.wiki/tmp/`.
        wiki = Wiki(dry_store(root), scan_root=root)
    else:
        wiki = Wiki(root)
    created = wiki.ensure(push=push)
    REPAIRED[:] = created
    if require_sources and not wiki.all_sources():
        raise WikiError(
            "E_EMPTY",
            "no sources have been ingested yet",
            "run `ingest` first so the engine has raw material to work from",
            3,
        )
    return wiki, created


def require_install():
    health.require_install(Path(__file__).resolve().parent, Path.cwd())


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
