"""llm-wiki 命令行：参数解析、命令分发与锁。

各命令实现在同目录的模块（ingest/submit/workspace/query/page/workflow），共用的小工具在
common.py；输出契约见
docs/readme/cli.md，进度与错误码见 docs/agent/engine.md。"""

from __future__ import annotations

import argparse
import sys
from pathlib import Path

from cli.common import ARGS, VERSION, fail
from cli.ingest import command_ingest
from cli.page import command_page_import, command_page_link, command_page_restore
# tests/selftest.py 直接调用这两个纯函数，保持在入口模块的再导出面。
from cli.query import citing_topics_index, command_query, ref_topics_line
from cli.submit import command_submit
from cli.workflow import command_workflow
from cli.workspace import (
    command_check,
    command_doctor,
    command_export,
    command_init,
    command_next,
    command_release,
    command_status,
    command_sync,
    command_version,
)
from errors import WikiError
from workspace import WIKI_DIR, workspace_lock


def build_parser():
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--root", help="wiki root inside the workspace (default: current directory)")
    common.add_argument("--json", action="store_true", help="machine-readable output")
    common.add_argument(
        "--owner",
        help="session id that owns a claim (default: $LLM_WIKI_OWNER, then $DSH_SESSION_ID)",
    )
    parser = argparse.ArgumentParser(
        prog="wiki.py",
        description="Portable wiki engine: task-driven ingestion, ledger-owned pages.",
    )
    parser.add_argument("--version", action="version", version=f"llm-wiki {VERSION}")
    sub = parser.add_subparsers(dest="command", required=True)

    sub.add_parser("check", parents=[common], help="verify the installed skill and workspace")
    sub.add_parser("init", parents=[common], help="create .wiki/ and report state")
    ingest = sub.add_parser("ingest", parents=[common], help="scan sources and queue route tasks")
    ingest.add_argument("--dry-run", action="store_true")
    ingest.add_argument("--force", action="store_true", help="reparse even when the digest is unchanged")
    ingest.add_argument("--only", action="append", metavar="PATH",
                        help="limit ingest to these sources (repeatable)")
    next_p = sub.add_parser("next", parents=[common], help="claim queued tasks")
    next_p.add_argument("--kind", choices=["route", "page"])
    next_p.add_argument("--batch", type=int, default=0, help="tasks to claim (default: 1)")
    submit = sub.add_parser("submit", parents=[common], help="submit a task payload")
    submit.add_argument("id")
    submit.add_argument("--epoch", type=int, help=argparse.SUPPRESS)
    submit.add_argument("--payload", help="JSON file or - for stdin; defaults to the task folder")
    release = sub.add_parser("release", parents=[common], help="return a task to the queue")
    release.add_argument("id")
    query = sub.add_parser("query", parents=[common], help="search pages, topics and workflows, or inspect one page")
    query.add_argument("text", nargs="*")
    query.add_argument("--page")
    query.add_argument("--task", help="register the search on a task; route tasks must query before deciding")
    query.add_argument("--limit", type=int, default=None)
    workflow = sub.add_parser("workflow", help="inspect executable workflows")
    workflow_sub = workflow.add_subparsers(dest="workflow_command", required=True)
    workflow_sub.add_parser("list", parents=[common], help="list workflows with step counts and validity")
    search_p = workflow_sub.add_parser("search", parents=[common], help="explicitly search workflows")
    search_p.add_argument("text", nargs="*")
    search_p.add_argument("--limit", type=int, default=5)
    load_p = workflow_sub.add_parser("load", parents=[common], help="load one workflow, its typed steps and resources")
    load_p.add_argument("id")
    page = sub.add_parser("page", help="human edits to an existing page")
    page_sub = page.add_subparsers(dest="page_command", required=True)
    import_p = page_sub.add_parser("import", parents=[common], help="adopt a hand-edited page file")
    import_p.add_argument("id")
    import_p.add_argument("--from", dest="from_path", required=True)
    restore = page_sub.add_parser("restore", parents=[common], help="restore an older revision")
    restore.add_argument("id")
    restore.add_argument("--rev", type=int, required=True)
    link = page_sub.add_parser("link", parents=[common],
                               help="add relations to an existing page")
    link.add_argument("id", nargs="?")
    link.add_argument("targets", nargs="*", metavar="target")
    link.add_argument("--repair", action="store_true",
                      help="add edges between pages that cite the same raw source")
    link.add_argument("--dry-run", action="store_true",
                      help="preview changes without writing")
    export = sub.add_parser("export", parents=[common], help="export rendered pages to a directory")
    export.add_argument("dir")
    status = sub.add_parser("status", parents=[common], help="ledger, pages, tasks")
    status.add_argument("--brief", action="store_true")
    sync = sub.add_parser("sync", parents=[common], help="take hand edits back from the publish dir")
    sync.add_argument("--body-only", action="store_true",
                      help="absorb the body and evidence, keep the ledger metadata")
    doctor = sub.add_parser("doctor", parents=[common], help="health checks; --fix repairs mechanical issues")
    doctor.add_argument("--fix", action="store_true")
    sub.add_parser("version", parents=[common], help="print the engine version")
    return parser


COMMANDS = {
    "check": command_check,
    "init": command_init,
    "ingest": command_ingest,
    "next": command_next,
    "submit": command_submit,
    "release": command_release,
    "query": command_query,
    "export": command_export,
    "status": command_status,
    "sync": command_sync,
    "doctor": command_doctor,
    "version": command_version,
}


LOCKED_COMMANDS = frozenset({
    "init", "ingest", "next", "submit", "release", "query",
    "export", "status", "sync", "doctor", "page import", "page restore", "page link",
})


def main(argv=None):
    parser = build_parser()
    # 各命令模块 import 的是同一个 ARGS 对象，这里只更新属性、不重新绑定名字。
    ARGS.__dict__.update(vars(parser.parse_args(argv)))
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
        sys.stderr.reconfigure(encoding="utf-8")
    root = Path(ARGS.root).resolve() if ARGS.root else Path.cwd()
    command = ARGS.command
    if command == "page":
        command = f"page {ARGS.page_command}"
        handler = {
            "import": command_page_import,
            "restore": command_page_restore,
            "link": command_page_link,
        }[ARGS.page_command]
    elif command == "workflow":
        command = f"workflow {ARGS.workflow_command}"
        handler = command_workflow
    else:
        handler = COMMANDS[command]
    try:
        # 试运行不得创建 `.wiki/`，此时也没有可争用的写入目标，直接跳过排队。
        dry_only = command == "ingest" and ARGS.dry_run and not (root / WIKI_DIR).exists()
        locked = command in LOCKED_COMMANDS and not dry_only
        # A plain query is a snapshot read; only `--task` writes the ledger.
        if command == "query" and (not ARGS.task or ARGS.page):
            locked = False
        if locked:
            with workspace_lock(root):
                return handler()
        return handler()
    except WikiError as error:
        return fail(command, root, error)
    except KeyboardInterrupt:
        return 130
