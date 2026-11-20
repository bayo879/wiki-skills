"""llm-wiki 命令行：参数解析、输出格式与各命令的实现。

命令只通过 Wiki 的公开方法读写账本；输出契约见 docs/readme/cli.md，
进度与错误码见 docs/agent/engine.md。
"""

from __future__ import annotations

import argparse
import json
import os
import re
import sqlite3
import sys
from datetime import datetime, timezone
from pathlib import Path

import health
import workflows
import search
import topics
from errors import WikiError
from frontmatter import split_frontmatter
from ingest import (
    MAX_RAW_CHARS,
    extract_text,
    ingest_decision,
    raw_facts,
    read_bytes,
)
from ledger import CONFIG_NAME, Wiki, view_dir
from pages import normalize_links, render_page
from tasks import task_dir_relative
from util import (
    digest_bytes,
    digest_text,
    dump_meta,
    norm_key,
    now_iso,
    parse_iso,
    same_stamp,
    write_atomic,
)
from validation import check_links, validate_page_payload
from view import sync_view_file
from workspace import (
    WIKI_DIR,
    dry_store,
    inside_root,
    window_safe,
    workspace_lock,
)


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

def command_check():
    report = health.check_installation(Path(__file__).resolve().parent, Path.cwd())
    lines = [
        f"{'ok  ' if item['ok'] else 'fail'} {item['name']}: {item['detail']}"
        for item in report["checks"]
    ]
    emit(
        "check", Path.cwd(),
        "self-check passed" if report["ok"] else "self-check failed",
        report,
        lines,
        ok=report["ok"],
    )
    return 0 if report["ok"] else 1

def command_init():
    require_install()
    wiki, created = open_wiki()
    rows = wiki.all_sources()
    pages = wiki.all_pages()
    data = {
        "config": norm_key(wiki.config_path.relative_to(wiki.root)),
        "pages": len(pages),
        "sources": len(rows),
        "open_tasks": len(wiki.open_tasks()),
    }
    emit(
        "init", wiki.root,
        f"wiki ready at {WIKI_DIR}/ ({len(rows)} sources, {len(pages)} pages)",
        data,
        [f"created: {', '.join(created) if created else '(already present)'}"],
    )
    wiki.close()
    return 0

def command_ingest():
    require_install()
    wiki, _ = open_wiki(dry_run=ARGS.dry_run)
    # 别的会话正在摄入时连排队都停：多个执行体一起改任务目录正是载荷被写串的入口。
    if not ARGS.dry_run:
        foreign = wiki.foreign_leases(session_owner())
        if foreign:
            task_id, held_by = foreign[0]
            raise WikiError(
                "E_INGEST_BUSY",
                f"session {held_by} is already ingesting task {task_id}; stop, do not retry",
                "another session is ingesting this workspace: do not ingest, queue or claim "
                "here, and do not retry — wait for it to submit, or have the user decide who "
                "takes over", 3,
            )
    wiki.cleanup_task_dirs()
    limit = int(wiki.config["ingest"].get("max_source_bytes", 50_000_000))
    force = ARGS.force
    selected = {norm_key(Path(value)) for value in ARGS.only} if ARGS.only else None
    files = [
        path for path in wiki.scan_files()
        if selected is None or norm_key(path.relative_to(wiki.scan_root)) in selected
    ]
    seen = set()
    fresh, unchanged, unsupported, oversize, failed, queued = [], [], [], [], [], []
    restored = []
    for path in files:
        key = norm_key(path.relative_to(wiki.scan_root))
        seen.add(key)
        row = wiki.source_row(key)
        try:
            stat = path.stat()
        except OSError as error:
            failed.append({"key": key, "message": str(error)})
            continue
        # 第一段初筛：只有 stat 与账本不符、强制重算、或原文缺失时才读源文件。
        # 其余情形（skip / bookkeep_raw）一次都不读，也不重算摘要。
        stamp_changed = row is None or not same_stamp(
            stat.st_size, stat.st_mtime, row["size"], row["mtime"]
        )
        if row is None:
            raw_state, raw_size, raw_mtime = "missing", 0, 0.0
        else:
            raw_state, raw_size, raw_mtime = raw_facts(wiki.root, row)
        digest = row["digest"] if row is not None else ""
        changed = False
        data = None
        if stamp_changed or force or raw_state == "missing":
            try:
                data = read_bytes(path, limit)
            except WikiError as error:
                oversize.append({"key": key, "message": error.message})
                if not ARGS.dry_run:
                    wiki.upsert_source(
                        path.relative_to(wiki.scan_root),
                        digest_text(key + str(stat.st_size)),
                        stat.st_size,
                        stat.st_mtime,
                        "", "", "oversize", "", 0, 0.0,
                    )
                continue
            except OSError as error:
                failed.append({"key": key, "message": str(error)})
                continue
            digest = digest_bytes(data)
            changed = row is None or row["digest"] != digest
        action = ingest_decision(changed, raw_state, stat.st_mtime, raw_mtime, force=force)
        if action in ("skip", "bookkeep_raw", "bookkeep_source"):
            # 不解析、不写原文：只把账本对齐到磁盘上的事实。
            if action == "skip" and row["status"] == "missing":
                wiki.set_source_status(key, "pending" if not _cites(wiki, key) else "done")
                restored.append(key)
                continue
            unchanged.append(key)
            if action == "skip" and not stamp_changed and same_stamp(
                raw_size, raw_mtime, row["raw_size"], row["raw_mtime"]
            ):
                continue
            if not ARGS.dry_run:
                raw_rel = row["raw"]
                raw_id = row["raw_id"]
                if action == "bookkeep_raw" and raw_rel:
                    # 人工排版过的原文：正文变了就换一行，last_parsed_digest 留旧值，
                    # 下一轮 ingest 据此认出「源没变、原文是人改的」，不回源覆盖。
                    raw_rel, raw_id = wiki.adopt_raw_edit(key, row["parser"])
                raw_stat = (wiki.root / raw_rel).stat()
                disk_digest = raw_id
                disk_size, disk_mtime = raw_stat.st_size, raw_stat.st_mtime
                wiki.upsert_source(
                    path.relative_to(wiki.scan_root), digest, stat.st_size, stat.st_mtime,
                    raw_rel, row["parser"], row["status"],
                    disk_digest, disk_size, disk_mtime,
                    raw_id=raw_id,
                    last_parsed_digest=row["last_parsed_digest"],
                )
                # 账本跟着磁盘上的原文走：当前源的快照要对齐；共享 raw 的其他挂载
                # 由 adopt_raw_edit 一并处理。
                if raw_rel:
                    wiki.pin_open_task_raw(
                        key, raw_rel, disk_digest, disk_size, disk_mtime
                    )
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
            if not ARGS.dry_run:
                wiki.upsert_source(
                    path.relative_to(wiki.scan_root), digest, len(data), stat.st_mtime,
                    "", "", "unsupported", "", 0, 0.0,
                )
            continue
        if len(text) > MAX_RAW_CHARS:
            text = text[:MAX_RAW_CHARS]
        if ARGS.dry_run:
            fresh.append(
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
        if action == "rewrite":
            raw_rel, raw_id = wiki.attach_raw(key, text, parser, path.name)
            last_parsed = raw_id
        else:
            # parse_keep_raw：保留用户那份原文，只刷新账本里的原文事实。
            raw_rel = row["raw"]
            raw_id = row["raw_id"]
            last_parsed = row["last_parsed_digest"]
        raw_stat = (wiki.root / raw_rel).stat()
        raw_digest = raw_id
        raw_size, raw_mtime = raw_stat.st_size, raw_stat.st_mtime
        wiki.upsert_source(path.relative_to(wiki.scan_root), digest, len(data), stat.st_mtime,
                           raw_rel, parser, "pending", raw_digest, raw_size, raw_mtime,
                           raw_id=raw_id, last_parsed_digest=last_parsed)
        fresh.append(
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
        # --only 只看指定来源，未参与本轮扫描的来源不能被当成消失。
        if selected is not None and row["key"] not in selected:
            continue
        vanished.append(row["key"])
        if ARGS.dry_run:
            continue
        wiki.set_source_status(row["key"], "missing")
    if ARGS.dry_run:
        data = {
            "dry_run": True,
            "would_ingest": fresh,
            "unchanged": unchanged,
            "unsupported": unsupported,
            "oversize": oversize,
            "failed": failed,
            "vanished": vanished,
            "restored": restored,
        }
        emit(
            "ingest", wiki.report_root,
            f"dry run: {len(fresh)} new or changed, {len(unchanged)} unchanged, "
            f"{len(unsupported)} unsupported",
            data,
            [f"would ingest: {item['key']}" for item in fresh],
        )
        wiki.close()
        return 0
    # 失效不是放弃：任务因来源漂移被判 expired 后，来源仍是 pending，但旧任务已经不
    # 存在，下一轮 ingest 只会走「未变化」分支，不再入队——库会永远空转。这里统一入队：
    # 先按 raw 归并（同一份正文只排一个 route 任务，任务带它的全部挂载路径），再给
    # 每个 raw 入队。没有 raw 的来源（超大、解析失败）不排队，它们本来就没有可提交的内容。
    by_raw = {}
    for row in wiki.all_sources():
        if row["status"] != "pending" or not row["raw"]:
            continue
        if selected is not None and row["key"] not in selected:
            continue
        holder = wiki.open_task_for_raw(row["raw_id"])
        if holder is not None:
            # 这份正文已经在别人的树上：新路径挂上去即可，不另排一棵树。
            wiki.attach_task_source(holder["id"], row["key"])
            continue
        by_raw.setdefault(row["raw_id"], []).append(row["key"])
    for raw_id, keys in by_raw.items():
        queued.append(
            wiki.enqueue_task(
                "route", raw_id,
                {"sources": [wiki.source_snapshot(key) for key in sorted(keys)]},
            )
        )
    # 没有任何来源指向的旧 raw 行连同文件一起回收，观看区不留孤儿副本。
    reclaimed = wiki.collect_raws()
    # 旧版 raw 没有“原文来源”脚注；ingest 是 raw/ 的维护者，顺手补齐。
    refreshed_raws = wiki.refresh_raw_provenance()
    # 孤儿页：来源全没了、也没有出入链的页面没有保留价值，连同观看区副本一起删除；
    # 还有链的页面只由 computed_status 标 stale，等链断了再清。
    orphaned = []
    for record in wiki.removable_pages():
        if wiki.delete_page(record["id"]):
            orphaned.append(record["id"])
    remaining = [row["key"] for row in wiki.all_sources() if row["status"] == "pending"]
    counts = wiki.task_counts()
    data = {
        "ingested": fresh,
        "unchanged": unchanged,
        "unsupported": unsupported,
        "oversize": oversize,
        "failed": failed,
        "vanished": vanished,
        "restored": restored,
        "queued_tasks": queued,
        "reclaimed": reclaimed,
        "raw_links": refreshed_raws,
        "orphaned": orphaned,
        "pending_total": len(remaining),
        "pending": remaining,
        "queue": {
            "open": counts["open"], "queued": counts["queued"], "leased": counts["leased"],
            "route": counts["route"], "page": counts["page"],
        },
    }
    emit(
        "ingest", wiki.root,
        f"ingested {len(fresh)} sources, queued {len(queued)} route tasks; "
        f"{len(remaining)} pending, {len(orphaned)} orphan page(s) removed, "
        f"open queue {counts['open']}",
        data,
        [f"queued: {item}" for item in queued],
        ok=not (failed or oversize),
    )
    wiki.close()
    return 1 if (failed or oversize) else 0

def _cites(wiki, key):
    return any(key in record["meta"].get("sources", []) for record in wiki.all_pages())

def command_status():
    wiki, created = open_wiki()
    rows = wiki.all_sources()
    by_status = {}
    for row in rows:
        by_status[row["status"]] = by_status.get(row["status"], 0) + 1
    pages = wiki.all_pages()
    # 超时租约在 status 里一并收掉：agent 常先看 status 再决定要不要动手。
    wiki.sweep_leases()
    tasks = wiki.task_counts()
    stale = sorted(
        record["id"] for record in pages if record["meta"].get("status") == "stale"
    )
    pending = [row["key"] for row in rows if row["status"] == "pending"]
    if ARGS.brief:
        line = (
            f"{len(rows)} sources ({len(pending)} pending, {by_status.get('unsupported', 0)} unsupported, "
            f"{by_status.get('oversize', 0)} oversize), {len(pages)} pages ({len(stale)} stale), "
            f"{tasks['route']} route + {tasks['page']} page tasks open"
        )
        emit("status", wiki.root, line, {"brief": line}, [])
        wiki.close()
        return 0
    owner = session_owner()
    leases = [
        {"task": row["id"], "owner": wiki.lease_owner(row["id"])}
        for row in wiki.leased_tasks()
    ]
    foreign = sorted({item["owner"] for item in leases if item["owner"] and item["owner"] != owner})
    data = {
        "created": created,
        "sources": by_status,
        "source_total": len(rows),
        "pages": len(pages),
        "stale_pages": stale,
        "pending": pending,
        "tasks": {"open": tasks["open"], "queued": tasks["queued"],
                  "leased": tasks["leased"], "route": tasks["route"], "page": tasks["page"],
                  "expired": tasks["expired"], "accepted": tasks["accepted"]},
        "leases": leases,
        "next": (f"stop: {', '.join(foreign)} is already ingesting" if foreign
                 else "submit the open task first" if tasks["leased"]
                 else "run `next`" if tasks["queued"] else "run `ingest`"),
    }
    emit(
        "status", wiki.root,
        f"{len(rows)} sources ({len(pending)} pending), {len(pages)} pages, "
        f"{tasks['route']} route + {tasks['page']} page tasks",
        data,
        [f"pending: {key}" for key in pending] or ["pending: none"],
    )
    wiki.close()
    return 0

def command_next():
    require_install()
    wiki, _ = open_wiki()
    wiki.cleanup_task_dirs()
    # 先收掉超时的租约：死掉的会话不能靠一条还挂着的 lease 把队列堵死。
    wiki.sweep_leases()
    owner = session_owner()
    open_tasks = wiki.leased_tasks()
    if open_tasks:
        holders = [(row["id"], wiki.lease_owner(row["id"])) for row in open_tasks]
        foreign = wiki.foreign_leases(owner)
        if foreign:
            task_id, held_by = foreign[0]
            raise WikiError(
                "E_INGEST_BUSY",
                f"session {held_by} is already ingesting task {task_id}; stop, do not retry",
                "another session owns the queue: do not claim, do not retry, and do not run "
                "`next` again until it finishes — only its owner submits or releases it. "
                "Tell the user another session is ingesting instead of working around it",
                3,
            )
        ids = ", ".join(task_id for task_id, _ in holders)
        first = open_tasks[0]
        raise WikiError(
            "E_TASK_UNSUBMITTED",
            f"task {ids} is taken but not submitted",
            f"finish it first: submit {first['id']} --payload "
            f"{wiki.task_dir(first['id']) / 'payload.json'}, or release {first['id']} "
            "to return it to the queue",
            3,
        )
    batch = ARGS.batch or 1
    tasks = []
    lines = []
    for row in wiki.claim_tasks(kind=ARGS.kind, batch=max(1, batch), owner=owner):
        item = wiki.materialize_task(row)
        tasks.append(item)
        lines.append(f"{row['id']}  {item['brief']['goal']}")
    data = {"tasks": tasks, "count": len(tasks), "owner": owner}
    if not tasks:
        emit("next", wiki.root, "no queued tasks", data, [])
        wiki.close()
        return 0
    summary = (
        f"{len(tasks)} task(s) ready; read each brief, fill its payload.json, then submit"
    )
    emit("next", wiki.root, summary, data, lines)
    wiki.close()
    return 0

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

def command_submit():
    require_install()
    wiki, _ = open_wiki()
    task_id = ARGS.id
    row = wiki.task_row(task_id)
    if not row:
        raise WikiError("E_TASK_UNKNOWN", f"no task {task_id}", "run `next` to get a task id", 2)
    if row["state"] == "accepted":
        raise WikiError("E_TASK_STALE", f"task {task_id} is already accepted",
                        "run `next` for new work", 3)
    # 领取阶段判出的漂移在 submit 时也报来源漂移，而不是笼统的任务过期。
    if row["state"] == "expired":
        raise WikiError(
            "E_SOURCE_DRIFT",
            f"task {task_id} was invalidated: {row['last_error'] or 'its source changed'}",
            "re-run `ingest`, then `next` for fresh work", 3,
        )
    if row["state"] != "leased":
        raise WikiError("E_TASK_STALE", f"task {task_id} is no longer ready",
                        "run `next` again to get fresh work", 3)
    held_by = wiki.lease_owner(task_id)
    if held_by and held_by != session_owner():
        # 别的会话领走的任务不在这里交：两个会话共用任务目录正是载荷被写串的入口。
        raise WikiError(
            "E_TASK_FOREIGN",
            f"task {task_id} is leased to session {held_by}",
            "submit only the task this session claimed; ask that session to submit it, or "
            f"release {task_id} and run `next` here to claim it again", 3,
        )
    if ARGS.epoch is not None and row["epoch"] != ARGS.epoch:
        raise WikiError(
            "E_TASK_STALE",
            f"task {task_id} changed after it was claimed",
            "run `next` again and submit the fresh task", 3,
        )
    if row["lease_until"] and parse_iso(row["lease_until"]) < datetime.now(timezone.utc):
        wiki.requeue_task(task_id, "lease expired")
        raise WikiError("E_TASK_STALE", f"task {task_id} expired before submit",
                        "run `next` again to get fresh work", 3)
    payload = read_payload(wiki, row)
    try:
        if row["kind"] == "route":
            result = submit_route(wiki, row, payload)
        else:
            result = submit_page(wiki, row, payload)
    except WikiError as error:
        # 只有仍在租约里的任务才回队；expired 是终态，不能被回队复活。
        fresh = wiki.task_row(task_id)
        if fresh and fresh["state"] == "leased":
            wiki.requeue_task(task_id, f"{error.code}: {error.message}")
        raise
    emit(
        "submit", wiki.root,
        result["summary"],
        {
            "task": task_id,
            "page": result.get("page"),
            "rev": result.get("rev"),
            "sources": result.get("sources", []),
            "queued_page_tasks": result.get("queued_page_tasks", []),
        },
        result.get("lines", []),
    )
    wiki.close()
    return 0

def _raw_mounts(sources):
    """Group task sources by raw: one decision covers every path of the same body."""
    raw_of = {}
    mounts = {}
    for item in sources:
        raw_id = item.get("raw_id") or item["key"]
        raw_of[item["key"]] = raw_id
        mounts.setdefault(raw_id, []).append(item["key"])
    return raw_of, mounts


def submit_route(wiki, row, payload):
    inputs = json.loads(row["inputs"])
    available = [item["key"] for item in inputs["sources"]]
    raw_of, mounts = _raw_mounts(inputs["sources"])
    decisions = payload.get("decisions") if isinstance(payload, dict) else None
    if not isinstance(decisions, list) or not decisions:
        raise WikiError(
            "E_PAYLOAD",
            "route payload must be {\"decisions\": [...]} with at least one decision per input source",
            "use the task template: at least one decision per input source",
            2,
        )
    seen = set()
    targets = set()
    new_ids = set()
    skipped = set()
    for decision in decisions:
        if not isinstance(decision, dict):
            raise WikiError("E_PAYLOAD", "each decision must be an object", "", 2)
        key = decision.get("source")
        if key not in raw_of:
            raise WikiError("E_TASK_SCOPE", f"source {key!r} is not in this task",
                            f"task sources: {', '.join(available)}", 3)
        raw_id = raw_of[key]
        action = decision.get("action")
        if action not in ("new", "merge", "skip"):
            raise WikiError("E_PAYLOAD", f"action {action!r} must be new, merge or skip",
                            "pick one action per source", 2)
        if not str(decision.get("reason") or "").strip():
            raise WikiError("E_PAYLOAD", f"{key}: reason must not be empty",
                            "one sentence explaining the choice", 2)
        seen.add(raw_id)
        if action == "skip":
            skipped.add(raw_id)
            continue
        page_id = str(decision.get("page") or "").strip().lower()
        if not ID_RE.match(page_id):
            raise WikiError("E_BAD_ID", f"{key}: page id {page_id!r} is not a lowercase slug",
                            "use e.g. `filing-process`", 2)
        if (raw_id, page_id) in targets:
            raise WikiError(
                "E_PAYLOAD",
                f"{key}: page {page_id} is decided twice for one raw",
                "one decision per raw and page; split a raw across different pages",
                2,
            )
        targets.add((raw_id, page_id))
        if action == "new":
            if page_id in new_ids:
                raise WikiError(
                    "E_PAYLOAD",
                    f"page {page_id} is created by more than one decision",
                    "create a page once, then merge the other sources into it", 2,
                )
            new_ids.add(page_id)
    missing = [
        key for key in available if raw_of[key] not in seen
    ]
    if missing:
        raise WikiError(
            "E_PAYLOAD",
            f"decisions must cover every input raw; missing {sorted(missing)}",
            "submit at least one decision per raw (any one of its paths)",
            2,
        )
    conflicting = sorted(skipped & {raw_id for raw_id, _ in targets})
    if conflicting:
        raise WikiError(
            "E_PAYLOAD",
            f"source {mounts[conflicting[0]][0]}: skip cannot be combined with new or merge",
            "skip a source on its own, or decide its pages without skip", 2,
        )
    words = wiki.query_words(row["id"])
    if not words:
        raise WikiError(
            "E_NO_QUERY",
            f"route {row['id']} has no registered query",
            f"register the topic words first: query --task {row['id']} 词1 词2",
            3,
        )
    if len(words) > wiki.query_budget(row):
        raise WikiError(
            "E_QUERY_BUDGET",
            f"route {row['id']} registered {len(words)} topic words",
            "keep the words that separate pages; the new words are already logged",
            3,
        )
    # Validate every decision before touching the ledger, so a bad payload
    # never leaves a half-applied route behind.
    plan = []
    new_pages = {
        str(decision.get("page") or "").strip().lower()
        for decision in decisions if decision.get("action") == "new"
    }
    for decision in decisions:
        key = decision["source"]
        raw_id = raw_of[key]
        action = decision["action"]
        if action == "skip":
            if decision.get("links"):
                raise WikiError(
                    "E_PAYLOAD", f"{key}: skip decision cannot carry links",
                    "drop links, or decide the page without skip", 2,
                )
            plan.append({"action": "skip", "key": key, "raw": raw_id})
            continue
        page_id = str(decision.get("page") or "").strip().lower()
        if not ID_RE.match(page_id):
            raise WikiError("E_BAD_ID", f"{key}: page id {page_id!r} is not a lowercase slug",
                            "use e.g. `filing-process`", 2)
        raw_links = decision.get("links") or []
        if not isinstance(raw_links, list) or any(not isinstance(item, str) for item in raw_links):
            raise WikiError("E_PAYLOAD", f"{key}: links must be a list of page ids", "", 2)
        links = normalize_links(raw_links)
        # Siblings created by this same payload are legal targets; they may not
        # exist yet, and submitting in the wrong order is retried later.
        check_links(page_id, links, wiki.page_ids() | new_pages, context=key)
        record = wiki.page_record(page_id)
        existing = wiki.open_task_for_page(page_id)
        if existing and existing["state"] == "leased":
            raise WikiError(
                "E_STATE", f"{key}: page {page_id} is being written by another task",
                "retry this route after that page task is submitted", 3,
            )
        if action == "new":
            if record:
                raise WikiError(
                    "E_PAGE_CONFLICT",
                    f"{key}: page {page_id} already exists",
                    "use action merge, or pick a different page id", 3,
                )
            title = str(decision.get("title") or "").strip()
            page_type = str(decision.get("type") or "").strip()
            if not title:
                raise WikiError("E_PAYLOAD", f"{key}: title is required for a new page", "", 2)
            if page_type not in wiki.config["schema"]["types"]:
                raise WikiError(
                    "E_PAYLOAD", f"{key}: type {page_type!r} not in schema.types",
                    f"choose one of: {', '.join(wiki.config['schema']['types'])}", 2,
                )
            plan.append({"action": "new", "key": key, "raw": raw_id, "page": page_id,
                         "title": title, "type": page_type, "links": links})
        else:
            pending_create = bool(
                existing and json.loads(existing["inputs"]).get("mode") == "create"
            )
            if not record and not pending_create and page_id not in new_pages:
                raise WikiError(
                    "E_PAGE_UNKNOWN",
                    f"{key}: page {page_id} does not exist",
                    "use action new, or check `next --json` page_index and pending_pages", 3,
                )
            plan.append({"action": "merge", "key": key, "raw": raw_id, "page": page_id,
                         "rev": record["rev"] if record else None, "links": links})
    # A page created in this same payload must be queued before sources merge
    # into it; otherwise the merge would create an update task for a missing page.
    plan.sort(key=lambda item: 0 if item["action"] == "new" else 1)
    page_tasks = []
    for item in plan:
        # 一条 decision 覆盖这份正文的全部挂载路径：页面来源、skip 状态都按 raw 落。
        keys = mounts[item["raw"]]
        if item["action"] == "skip":
            for key in keys:
                mount = wiki.source_row(key)
                if mount is not None and mount["status"] == "pending":
                    wiki.set_source_status(key, "skipped")
            continue
        entries = [wiki.source_snapshot(key) for key in keys]
        existing = wiki.open_task_for_page(item["page"])
        if existing:
            for entry in entries:
                wiki.attach_task_source(existing["id"], entry["key"])
            if item["links"]:
                wiki.attach_page_links(existing["id"], item["links"])
            page_tasks.append(existing["id"])
            continue
        inputs = {
            "page": item["page"],
            "base_rev": None if item["action"] == "new" else item["rev"],
            "sources": entries,
            "links": item["links"],
        }
        if item["action"] == "new":
            inputs.update({"mode": "create", "title": item["title"], "type": item["type"],
                           "route_task": row["id"]})
        else:
            inputs["mode"] = "update"
            inputs["route_task"] = row["id"]
        task_id = wiki.enqueue_task("page", item["page"], inputs)
        # 唯一索引可能把并发插入收敛到同一条任务，补挂本次来源（幂等）。
        for entry in entries:
            wiki.attach_task_source(task_id, entry["key"])
        page_tasks.append(task_id)
    wiki.close_task(row["id"], payload, row["epoch"])
    return {
        "kind": "route",
        "summary": f"route {row['id']} accepted; {len(set(page_tasks))} page task(s) queued",
        "sources": available,
        "queued_page_tasks": sorted(set(page_tasks)),
        "lines": [f"page task: {task_id}" for task_id in sorted(set(page_tasks))],
    }

def submit_page(wiki, row, payload):
    inputs = json.loads(row["inputs"])
    reason = wiki.task_drift(row)
    if reason:
        # 提交时才发现失效：置 expired 并报来源漂移，不回队。
        wiki.expire_task(row["id"], reason)
        raise WikiError(
            "E_SOURCE_DRIFT",
            f"task {row['id']} no longer matches its source snapshot: {reason}",
            "re-run `ingest`, then `next` for fresh work",
            3,
        )
    page_id = inputs["page"]
    sources = inputs["sources"]
    for item in sources:
        current = wiki.source_row(item["key"])
        if current is None or current["status"] == "missing":
            raise WikiError("E_SOURCE_DRIFT", f"source {item['key']} vanished",
                            "re-run `ingest`, then `next`", 3)
        if current["digest"] != item["source_digest"]:
            raise WikiError(
                "E_SOURCE_DRIFT", f"source {item['key']} changed after the task was queued",
                "re-read the source and take a fresh task", 3,
            )
    record = wiki.page_record(page_id)
    if inputs.get("mode") == "create" and record:
        raise WikiError("E_PAGE_CONFLICT", f"page {page_id} already exists",
                        "take the update task instead", 3)
    if inputs.get("mode") == "update":
        if not record:
            raise WikiError("E_PAGE_GONE", f"page {page_id} disappeared",
                            "run `next` again to rebuild the task", 3)
        if record["rev"] != inputs.get("base_rev"):
            raise WikiError(
                "E_PAGE_CONFLICT",
                f"page {page_id} is now at rev {record['rev']}, the task started from rev {inputs.get('base_rev')}",
                "run `next` again and rewrite against the current revision", 3,
            )
    meta, body, evidence, boundaries = validate_page_payload(wiki, row, payload, sources)
    rev = wiki.write_rev(page_id, meta, body, "submit")
    wiki.close_task(row["id"], payload, row["epoch"])
    # 一个来源可能拆到多页：最后一个相关 page 任务关闭后才算写完。
    for item in sources:
        if wiki.source_in_open_task(item["key"]) is None:
            wiki.set_source_status(item["key"], "done")
    return {
        "kind": "page",
        "page": page_id,
        "rev": rev,
        "sources": meta["sources"],
        "summary": f"page {page_id} accepted at rev {rev} ({len(meta['sources'])} sources)",
        "lines": [f"page: {page_id} rev {rev}"],
    }

def command_release():
    wiki, _ = open_wiki()
    row = wiki.task_row(ARGS.id)
    if not row:
        raise WikiError("E_TASK_UNKNOWN", f"no task {ARGS.id}", "run `next`", 2)
    held_by = wiki.lease_owner(ARGS.id)
    wiki.release_task(ARGS.id)
    summary = f"released {ARGS.id}"
    if held_by:
        # 释放是运维口子，谁的租约都能放回队列，但要说清放掉的是谁的。
        summary += f" (was leased to {held_by})"
    emit("release", wiki.root, summary, {"task": ARGS.id, "owner": held_by}, [])
    wiki.close()
    return 0

def query_task(wiki, words):
    """Register one search per topic word on a task, then show the hits.

    Words are searched one by one on purpose: pooling them ranks a page that
    repeats a common word above the page that answers one specific word, and
    the agent reads the grouped output to judge similarity itself.
    """
    row = wiki.task_row(ARGS.task)
    if not row:
        raise WikiError("E_TASK_UNKNOWN", f"no task {ARGS.task}",
                        "run `next` to get a task id", 2)
    if row["state"] != "leased":
        raise WikiError(
            "E_TASK_STALE",
            f"task {ARGS.task} is {row['state']}, not leased",
            "run `next` to claim it again, then query", 3,
        )
    held_by = wiki.lease_owner(ARGS.task)
    if held_by and held_by != session_owner():
        raise WikiError(
            "E_TASK_FOREIGN",
            f"task {ARGS.task} is leased to session {held_by}",
            "only the session that claimed a task can query on it; release it or run `next` here", 3,
        )
    if not words:
        raise WikiError("E_USAGE", "query --task needs topic words",
                        "example: query --task t_xxx 反诈 复机", 2)
    registered = wiki.query_words(ARGS.task)
    budget = wiki.query_budget(row)
    fresh = []
    seen = {word.lower() for word in registered}
    for word in words:
        if word.lower() in seen:
            continue
        seen.add(word.lower())
        fresh.append(word)
    if len(registered) + len(fresh) > budget:
        raise WikiError(
            "E_QUERY_BUDGET",
            f"task {ARGS.task} allows {budget} topic words: {len(registered)} used, "
            f"{len(fresh)} new would exceed it",
            "drop near-duplicates and keep the words that separate pages; "
            "the budget is 5 words per 1000 source characters",
            3,
        )
    limit = ARGS.limit or 5
    groups = []
    hits = {}
    for word in words:
        scored, results = search.search_pages(wiki, [word], limit)
        groups.append({
            "word": word,
            "matches": len(scored),
            "returned": len(results),
            "results": results,
        })
        hits[word] = [item["id"] for item in results]
    wiki.append_task_query(ARGS.task, words, hits)
    view = wiki.query_view(row)
    sections = []
    for group in groups:
        block = [f"{group['word']}  ({group['matches']} matches, showing {group['returned']})"]
        if group["results"]:
            block.extend(
                f"{item['id']}  [{item['type']}/{item['status']}]  {item['title']}\n    {item['snippet']}"
                for item in group["results"]
            )
        else:
            block.append("  (no page matched: judge from page_index and pending_pages)")
        sections.append(block)
    sections.append([
        f"topic words {view['used']}/{view['budget']} used, log: {view['log']}",
        "a truncated hit list is not proof that no similar page exists",
    ])
    lines = []
    for block in sections:
        if lines:
            lines.append("")
        lines.extend(block)
    data = {
        "task": ARGS.task,
        "queries": groups,
        "budget": view,
        "boundary": "hit count is a ranked subset, not proof of full coverage",
    }
    summary = (
        f"{len(groups)} topic word(s) searched for task {ARGS.task}, "
        f"{view['used']}/{view['budget']} words used"
    )
    emit("query", wiki.root, summary, data, lines)
    wiki.close()
    return 0


TOPIC_REF_LIMIT = 20
TOPIC_REF_WARNINGS = 3


def resolve_topic_refs(wiki, entries):
    """Validate parsed topic refs; return ({topic: refs}, warnings)."""
    names = {entry["name"] for entry in entries}
    pages = wiki.page_ids()
    workflows_seen = {
        item["id"] for item in workflows.scan(
            wiki.root, view_root=wiki.publish_dir / workflows.WORKFLOW_DIR)
    }
    raw_base = wiki.publish_raw_dir
    try:
        raw_prefix = raw_base.relative_to(wiki.root).as_posix()
    except ValueError:
        raw_prefix = ""
    kind_order = (("pages", "page"), ("raws", "raw"),
                  ("topics", "topic"), ("workflows", "workflow"))
    resolved, warnings = {}, []
    for entry in entries:
        if not entry["valid"]:
            # `_entry` only parses refs for a topic that has a body.
            continue
        parsed = entry["refs"]
        refs = {"pages": [], "raws": [], "topics": [], "workflows": [], "missing": []}
        notes = [
            f"warning: topic {entry['name']} malformed ref: {line.strip()}"
            for line in parsed["malformed"]
        ]
        ordered = [
            (key, kind, value)
            for key, kind in kind_order
            for value in parsed[key]
        ]
        dropped = len(ordered) > TOPIC_REF_LIMIT
        for key, kind, value in ordered[:TOPIC_REF_LIMIT]:
            if kind == "raw":
                known = _raw_ref_ok(raw_base, value, raw_prefix)
            elif kind == "page":
                known = value in pages
            elif kind == "topic":
                known = value in names
            else:
                known = value in workflows_seen
            if known:
                refs[key].append(value)
            else:
                refs["missing"].append({"kind": kind, "target": value})
                notes.append(
                    f"warning: topic {entry['name']} cites unknown {kind} {value}")
        if parsed["sections"] > 1:
            notes.append(
                f"warning: topic {entry['name']} has {parsed['sections']} 依据 sections")
        if dropped:
            notes.append(
                f"warning: topic {entry['name']} refs truncated to {TOPIC_REF_LIMIT}")
        resolved[entry["name"]] = refs
        warnings.extend(notes[:TOPIC_REF_WARNINGS])
    return resolved, warnings


def _raw_ref_ok(raw_base, value, raw_prefix=""):
    """A raw ref must stay inside the published raw tree and exist.

    `query --page` prints `sources[].raw` as a workspace-relative path
    (`wiki/raw/x.md`); accept that form as well as the bare `raw/` one.
    """
    text = value
    if raw_prefix and (text == raw_prefix or text.startswith(raw_prefix + "/")):
        text = text[len(raw_prefix):].lstrip("/")
    path = Path(text)
    if not text or path.is_absolute() or ".." in path.parts:
        return False
    try:
        return (raw_base / path).is_file()
    except (OSError, ValueError):
        return False


def citing_topics_index(refs_by_topic):
    """{page id: sorted topic names} from validated page refs only."""
    index = {}
    for name, refs in refs_by_topic.items():
        for page_id in refs["pages"]:
            index.setdefault(page_id, []).append(name)
    return {page_id: sorted(names) for page_id, names in index.items()}


def ref_topics_line(names):
    """`甲, 乙, 丙, +2`; empty list renders as an empty string."""
    if not names:
        return ""
    shown = ", ".join(names[:3])
    if len(names) > 3:
        shown += f", +{len(names) - 3}"
    return shown


def command_query():
    words = [
        word
        for chunk in (ARGS.text or [])
        for word in re.split(r"\s+", chunk.strip())
        if word
    ]
    if ARGS.task and not ARGS.page:
        wiki, _ = open_wiki()
        return query_task(wiki, words)
    wiki, _ = open_wiki(readonly=True)
    if ARGS.page:
        record = wiki.page_record(ARGS.page)
        if not record:
            raise WikiError("E_NOT_FOUND", f"no page {ARGS.page}", "search with `query <text>` first", 3)
        outgoing, incoming = wiki.links_for(ARGS.page)
        entries = topics.scan(wiki.root, view_root=wiki.publish_dir / topics.TOPIC_DIR)
        refs_by_topic, ref_warnings = resolve_topic_refs(wiki, entries)
        for line in ref_warnings:
            sys.stderr.write(line + "\n")
        cited = citing_topics_index(refs_by_topic).get(ARGS.page, [])
        citing_line = ref_topics_line(cited)
        # 源文件路径是账本事实：来源只存 key，副本路径按 raw_id 查 raws 表渲染出来，
        # 调用方（agent）不用再按来源名猜文件名。
        sources = []
        for key in record["meta"]["sources"]:
            row = wiki.source_row(key)
            raw = row["raw"] if row else ""
            sources.append({"key": key, "raw": raw or None})
        data = {
            "page": dump_meta(record["meta"]),
            "rev": record["rev"],
            "path": wiki.page_ref(record),
            "sources": sources,
            "outgoing": outgoing,
            "incoming": incoming,
            "citing_topics": cited,
            "body": record["body"],
        }
        emit(
            "query", wiki.root,
            f"page {ARGS.page} rev {record['rev']}: {len(outgoing)} outgoing, {len(incoming)} incoming links",
            data,
            [f"outgoing: {', '.join(outgoing) or '(none)'}",
             f"incoming: {', '.join(incoming) or '(none)'}",
             *([f"citing topics: {citing_line}"] if citing_line else []),
             f"raw: {', '.join(item['raw'] for item in sources if item['raw']) or '(none)'}",
             "",
             record["body"]],
        )
        wiki.close()
        return 0
    terms = words
    if not terms:
        raise WikiError("E_USAGE", "query needs text, --page or --task",
                        "example: query 反诈 复机", 2)
    limit = ARGS.limit or 8
    scored, results = search.search_pages(wiki, terms, limit)
    entries = topics.scan(wiki.root, view_root=wiki.publish_dir / topics.TOPIC_DIR)
    topic_hits = topics.rank(entries, terms, limit)
    refs_by_topic, ref_warnings = resolve_topic_refs(wiki, entries)
    for line in ref_warnings:
        sys.stderr.write(line + "\n")
    for item in topic_hits:
        item["refs"] = refs_by_topic[item["name"]]
    workflow_hits = workflows.match(
        wiki.root, terms, 5, view_root=wiki.publish_dir / workflows.WORKFLOW_DIR)
    cited = citing_topics_index(refs_by_topic)
    for item in results:
        item["citing_topics"] = cited.get(item["id"], [])
    data = {
        "query": " ".join(terms),
        "matches": len(scored),
        "returned": len(results),
        "results": results,
        "topic_matches": len(topic_hits),
        "topics": topic_hits,
        "workflow_matches": len(workflow_hits),
        "workflows": workflow_hits,
        "boundary": "hit count is a ranked subset, not proof of full coverage",
    }
    sections = []
    if workflow_hits:
        block = ["workflows (check applies before loading):"]
        for item in workflow_hits:
            alias = f"（{item['alias_name']}）" if item["alias_name"] else ""
            block.append(f"{item['id']}{alias}  {item['description']}")
            block.append(f"    applies: {item['applies'] or '(none)'}")
            block.append(f"    load: {item['load']}")
        sections.append(block)
    pages = []
    for item in results:
        entry = (
            f"{item['id']}  [{item['type']}/{item['status']}]  "
            f"{item['title']}\n    {item['snippet']}"
        )
        cite = ref_topics_line(item.get("citing_topics") or [])
        if cite:
            entry += f"\n    citing topics: {cite}"
        pages.append(entry)
    if pages:
        sections.append(pages)
    if topic_hits:
        block = ["topics (read before exploring the same theme again):"]
        for item in topic_hits:
            block.append(f"{item['name']}  [{item['status'] or 'open'}]  {item['description']}")
            block.append(f"    path: {item['path']}")
            refs = item["refs"]
            packed = [f"page:{value}" for value in refs["pages"]]
            packed += [f"raw:{value}" for value in refs["raws"]]
            packed += [f"topic:{value}" for value in refs["topics"]]
            packed += [f"workflow:{value}" for value in refs["workflows"]]
            if packed:
                block.append(f"    refs: {', '.join(packed)}")
            if refs["missing"]:
                block.append("    missing: " + ", ".join(
                    f"{miss['kind']}:{miss['target']}" for miss in refs["missing"]))
        sections.append(block)
    lines = []
    for block in sections:
        if lines:
            lines.append("")
        lines.extend(block)
    summary = f"{len(scored)} matching pages"
    if topic_hits:
        summary += f", {len(topic_hits)} matching topics"
    if workflow_hits:
        summary += f", {len(workflow_hits)} workflow candidates"
    summary += f", showing {len(results)} pages for {' '.join(terms)}"
    emit("query", wiki.root, summary, data, lines)
    wiki.close()
    return 0

def command_workflow():
    workspace = Path.cwd().resolve()
    root = Path(ARGS.root).resolve() if ARGS.root else workspace
    if not window_safe(workspace, root):
        raise WikiError(
            "E_PATH", "workflow root escapes the workspace",
            "run from the workspace, or pass --root inside it", 2,
        )
    view_root = view_dir(root) / workflows.WORKFLOW_DIR
    if ARGS.workflow_command == "search":
        terms = [term.strip() for term in ARGS.text if term.strip()]
        if not terms:
            raise WikiError(
                "E_USAGE", "workflow search needs text",
                'example: workflow search "开户"', 2,
            )
        limit = min(5, max(1, ARGS.limit))
        hits = workflows.match(root, terms, limit, view_root=view_root)
        data = {"query": " ".join(terms), "matches": len(hits), "workflows": hits}
        if not hits:
            data["message"] = "无可用workflow"
        lines = []
        for item in hits:
            alias = f"（{item['alias_name']}）" if item["alias_name"] else ""
            lines.append(f"{item['id']}{alias}  {item['description']}")
            where = (
                f"skill_path: {item['skill_path']}"
                if item["source"] == "builtin"
                else f"path: {item['path']}"
                + ("  [legacy]" if item["origin"] == "legacy" else "")
            )
            lines.append(
                f"    {where}  tools: {item['tools']}"
                f"  human: {item['human_steps']}  load: {item['load']}"
            )
            lines.append(f"    applies: {item['applies'] or '(none)'}")
        if not lines:
            lines.append("无可用workflow")
        emit("workflow search", root, f"{len(hits)} matching workflows", data, lines)
        return 0
    if ARGS.workflow_command == "list":
        data = workflows.inventory(root, view_root=view_root)
        summary = f"{data['valid']} valid, {data['count'] - data['valid']} invalid workflows"
        if data["warned"]:
            summary += f", {data['warned']} with warnings"
        lines = []
        for item in data["workflows"]:
            state = "ok" if item["valid"] else f"invalid: {item['error']}"
            alias = f"（{item['alias_name']}）" if item["alias_name"] else ""
            lines.append(f"{item['id']}{alias}  [{state}]  {item['description']}")
            where = (
                f"skill_path: {item['skill_path']}"
                if item["source"] == "builtin"
                else f"path: {item['path']}"
                + ("  [legacy]" if item["origin"] == "legacy" else "")
            )
            detail = (
                f"    {where}  resources: {len(item['resources'])}"
                f"  tools: {item['tools']}  human: {item['human_steps']}"
            )
            if item["warnings"]:
                detail += f"  warnings: {len(item['warnings'])}"
            lines.append(detail)
            for warning in item["warnings"]:
                where = f"line {warning['line']}: " if warning["line"] else ""
                lines.append(f"    ! {where}{warning['message']}")
            if item.get("legacy_conflict"):
                lines.append(
                    f"    ! 观看区版本生效，旧根目录版本未加载：{item['legacy_path']}"
                )
        emit("workflow list", root, summary, data, lines)
        return 0
    data = workflows.load(root, ARGS.id, view_root=view_root)
    record = data["workflow"]
    emit(
        "workflow load", root,
        f"workflow {record['id']}: {len(data['resources'])} resources, "
        f"{len(data['tools'])} tool steps, {len(data['human_steps'])} human steps",
        data,
        [
            (
                f"skill_path: {data['skill_path']}"
                if record["source"] == "builtin"
                else f"path: {data['path']}"
                + ("  [legacy]" if record["origin"] == "legacy" else "")
            ),
            f"description: {record['description']}",
            f"steps: {len(data['tools'])} tool, {len(data['human_steps'])} human, "
            f"{len(data['agent_steps'])} agent, {len(data['verify_steps'])} verify",
            "",
            data["content"],
        ],
    )
    return 0


def command_page_import():
    wiki, _ = open_wiki()
    page_id = ARGS.id.strip().lower()
    record = wiki.page_record(page_id)
    if not record:
        raise WikiError("E_NOT_FOUND", f"no page {page_id}", "import only edits existing pages", 3)
    try:
        source = inside_root(
            wiki.root, ARGS.from_path, "page import source",
            "put the edited page under .wiki/drafts/",
        )
        text = source.read_text(encoding="utf-8")
    except OSError as error:
        raise WikiError("E_PAYLOAD", f"cannot read {ARGS.from_path}: {error}", "", 2)
    meta, body = split_frontmatter(text)
    merged = dict(record["meta"])
    for field in ("title", "type", "summary"):
        if meta.get(field):
            merged[field] = str(meta[field])
    if meta.get("sources"):
        merged["sources"] = [str(item) for item in meta["sources"]]
    if meta.get("links"):
        if not isinstance(meta["links"], list):
            raise WikiError(
                "E_PAYLOAD",
                "frontmatter links must be a block list of page ids",
                "write `links:` and then one `  - page-id` line per link",
                2,
            )
        merged["links"] = normalize_links(
            list(merged.get("links") or []) + [str(item) for item in meta["links"]]
        )
        check_links(page_id, merged["links"], wiki.page_ids(), "page import")
    for key in merged["sources"]:
        if not wiki.source_row(key):
            raise WikiError("E_UNKNOWN_SOURCE", f"page cites unregistered source {key}",
                            "run `ingest` first, or fix the source list", 2)
    if not body.strip():
        raise WikiError("E_PAYLOAD", "imported page has no body", "", 2)
    if merged.get("type") not in wiki.config["schema"]["types"]:
        raise WikiError("E_PAYLOAD", f"type {merged.get('type')!r} not in schema.types", "", 2)
    rev = wiki.write_rev(page_id, merged, body, "import")
    emit(
        "page import", wiki.root,
        f"imported {page_id} at rev {rev}",
        {"page": page_id, "rev": rev, "path": wiki.page_ref(wiki.page_record(page_id))},
        [f"page: {page_id} rev {rev}"],
    )
    wiki.close()
    return 0

def command_page_restore():
    wiki, _ = open_wiki()
    page_id = ARGS.id.strip().lower()
    row = wiki.conn.execute(
        "SELECT * FROM page_revs WHERE page = ? AND rev = ?", (page_id, ARGS.rev)
    ).fetchone()
    if not row:
        raise WikiError("E_NOT_FOUND", f"no rev {ARGS.rev} for page {page_id}",
                        "`query --page` shows the current revision", 3)
    meta = json.loads(row["meta"])
    rev = wiki.write_rev(page_id, meta, row["body"], "restore")
    emit(
        "page restore", wiki.root,
        f"restored {page_id} to rev {ARGS.rev} as rev {rev}",
        {"page": page_id, "rev": rev, "restored_from": ARGS.rev},
        [f"page: {page_id} rev {rev}"],
    )
    wiki.close()
    return 0

def command_page_link():
    """Add edges to existing pages; the ledger stays add-only."""
    if ARGS.repair and (ARGS.id or ARGS.targets):
        raise WikiError(
            "E_USAGE", "page link --repair takes no page id or targets",
            "run `page link --repair [--dry-run]`", 2,
        )
    wiki, _ = open_wiki(push=not ARGS.dry_run)
    if ARGS.repair:
        return repair_page_links(wiki)
    page_id = (ARGS.id or "").strip().lower()
    if not page_id:
        raise WikiError(
            "E_USAGE", "page link needs a page id",
            "example: page link refund-process refund-policy", 2,
        )
    record = wiki.page_record(page_id)
    if not record:
        raise WikiError(
            "E_NOT_FOUND", f"no page {page_id}",
            "create the page first; `page link` only edits existing pages", 3,
        )
    targets = [
        target for target in normalize_links(ARGS.targets) if target != page_id
    ]
    if not targets:
        raise WikiError(
            "E_USAGE", "page link needs at least one other page id",
            "example: page link refund-process refund-policy", 2,
        )
    check_links(page_id, targets, wiki.page_ids())
    existing = normalize_links(record["meta"].get("links") or [])
    added = [target for target in targets if target not in existing]
    if ARGS.dry_run:
        emit(
            "page link", wiki.root,
            f"would link {page_id} -> {', '.join(added or targets)}",
            {"mode": "single", "dry_run": True, "page": page_id,
             "rev": record["rev"], "added": added},
            [f"{page_id} -> {target}" for target in added],
        )
        wiki.close()
        return 0
    if not added:
        emit(
            "page link", wiki.root,
            f"{page_id} already links {', '.join(targets)}",
            {"mode": "single", "dry_run": False, "page": page_id,
             "rev": record["rev"], "links": existing, "added": []},
            [],
        )
        wiki.close()
        return 0
    meta = dict(record["meta"])
    meta["links"] = normalize_links(existing + targets)
    rev = wiki.write_rev(page_id, meta, record["body"], "link")
    # The command exists to repair the graph, so publish the new section now
    # instead of waiting for the next command that opens the ledger.
    view_events = wiki.sync_view()
    for line in view_events:
        if line.startswith("warning: "):
            sys.stderr.write(line + "\n")
    emit(
        "page link", wiki.root,
        f"linked {page_id} -> {', '.join(added)} at rev {rev}",
        {"mode": "single", "dry_run": False, "page": page_id, "rev": rev,
         "links": meta["links"], "added": added, "view": view_events},
        [f"{page_id} -> {target}" for target in added],
    )
    wiki.close()
    return 0


def repair_page_links(wiki):
    """Add missing edges between pages that cite the same raw source."""
    groups = {}
    for record in wiki.all_pages():
        for source in record["meta"].get("sources") or []:
            row = wiki.source_row(str(source))
            key = row["raw"] if row and row["raw"] else str(source)
            groups.setdefault(key, []).append(record["id"])
    proposed = set()
    for pages in groups.values():
        pages = sorted(set(pages))
        for index, left in enumerate(pages):
            for right in pages[index + 1:]:
                proposed.add((left, right))
    existing = {
        (row["src"], row["dst"])
        for row in wiki.conn.execute("SELECT src, dst FROM links")
    }
    missing = sorted(
        (left, right) for left, right in proposed
        if (left, right) not in existing and (right, left) not in existing
    )
    by_page = {}
    for left, right in missing:
        by_page.setdefault(left, []).append(right)
    planned = []
    for page_id in sorted(by_page):
        record = wiki.page_record(page_id)
        planned.append({
            "page": page_id,
            "rev": record["rev"],
            "added": normalize_links(by_page[page_id]),
        })
    lines = [
        f"{item['page']} -> {', '.join(item['added'])}" for item in planned
    ]
    base = {"mode": "repair", "rules": ["shared-source"], "edges": len(missing)}
    if ARGS.dry_run:
        data = dict(base, dry_run=True, pages=planned)
        emit(
            "page link", wiki.root,
            f"link repair would add {len(missing)} edges across {len(planned)} pages",
            data, lines,
        )
        wiki.close()
        return 0
    applied = []
    for item in planned:
        page_id = item["page"]
        record = wiki.page_record(page_id)
        meta = dict(record["meta"])
        existing_links = normalize_links(meta.get("links") or [])
        meta["links"] = normalize_links(existing_links + item["added"])
        rev = wiki.write_rev(page_id, meta, record["body"], "link")
        applied.append({"page": page_id, "rev": rev, "added": item["added"]})
    if not applied:
        data = dict(base, dry_run=False, pages=[], view=[])
        emit("page link", wiki.root, "no missing shared-source edges", data, [])
        wiki.close()
        return 0
    view_events = wiki.sync_view()
    for line in view_events:
        if line.startswith("warning: "):
            sys.stderr.write(line + "\n")
    data = dict(base, dry_run=False, pages=applied, view=view_events)
    emit(
        "page link", wiki.root,
        f"link repair added {len(missing)} edges across {len(applied)} pages",
        data, lines,
    )
    wiki.close()
    return 0


def command_sync():
    """Take hand edits back from the view dir (R1-R9)."""
    # R11：先看磁盘，再让 ensure 的推送动手；否则挪走的文件会被重建出第二个副本。
    wiki, _ = open_wiki(push=False)
    pages_dir = wiki.publish_dir / "pages"
    base = wiki.publish_dir.resolve()
    imported, unchanged, written = [], [], []
    skipped, failed, warnings = [], [], []
    candidates = []
    paths = sorted(pages_dir.rglob("*.md")) if pages_dir.is_dir() else []
    for path in paths:
        rel = norm_key(path.relative_to(wiki.publish_dir))
        if not window_safe(base, path.resolve()):
            skipped.append({"file": rel, "reason": "escapes the publish dir"})
            continue
        try:
            text = path.read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError) as error:
            failed.append({"file": rel, "page": "", "code": "E_IO", "message": str(error)})
            continue
        meta, body = split_frontmatter(text)
        page_id = str(meta.get("id") or "").strip().lower()
        if not page_id:
            skipped.append({"file": rel, "reason": "no id"})
            continue
        record = wiki.page_record(page_id)
        if not record:
            skipped.append({"file": rel, "reason": "unknown id"})
            continue
        candidates.append({
            "file": rel, "path": path, "text": text, "meta": meta,
            "body": body, "page": page_id, "record": record,
        })
    groups = {}
    for item in candidates:
        groups.setdefault(item["page"], []).append(item)
    targets = wiki._view_targets(wiki.all_pages())
    for page_id in sorted(groups):
        items = groups[page_id]
        if len(items) > 1:
            names = ", ".join(sorted(item["file"] for item in items))
            for item in items:
                failed.append({
                    "file": item["file"], "page": page_id, "code": "E_DUPLICATE",
                    "message": f"id {page_id} is used by {len(items)} files: {names}",
                })
            continue
        item = items[0]
        try:
            bucket = sync_view_file(wiki, item, targets[page_id], ARGS.body_only,
                                    warnings=warnings)
        except (WikiError, OSError, UnicodeDecodeError) as error:
            if isinstance(error, WikiError):
                code, message = error.code, error.message
            else:
                code, message = "E_IO", str(error)
            failed.append({"file": item["file"], "page": page_id,
                           "code": code, "message": message})
            continue
        if bucket == "imported":
            imported.append(page_id)
        elif bucket == "written":
            written.append({"page": page_id, "from": item["file"], "to": targets[page_id]})
        else:
            unchanged.append(page_id)
    imported.sort()
    unchanged.sort()
    skipped.sort(key=lambda item: item["file"])
    failed.sort(key=lambda item: item["file"])
    written.sort(key=lambda item: (item["page"], item["from"]))
    data = {"imported": imported, "skipped": skipped, "failed": failed,
            "unchanged": unchanged, "written": written,
            "warnings": sorted(warnings)}
    summary = (f"sync: {len(imported)} imported, {len(skipped)} skipped, "
               f"{len(failed)} failed, {len(unchanged)} unchanged, {len(written)} written")
    lines = [f"imported: {page_id}" for page_id in imported]
    lines += [f"written: {item['page']} {item['from']} -> {item['to']}" for item in written]
    lines += [f"failed: {item['file']} [{item['code']}] {item['message']}" for item in failed]
    emit("sync", wiki.root, summary, data, lines, ok=not failed)
    wiki.close()
    return 1 if failed else 0

def command_export():
    wiki, _ = open_wiki()
    target = inside_root(
        wiki.root, ARGS.dir, "export target",
        "export into a directory inside the workspace, e.g. .wiki/export",
    )
    written = []
    records = wiki.all_pages()
    page_of = {
        record["id"]: (
            record["meta"].get("title") or record["id"],
            f"{record['meta']['type']}/{record['id']}.md",
        )
        for record in records
    }
    for record in records:
        meta = dict(record["meta"])
        meta["status"] = wiki.computed_status(meta)
        relative = f"{meta['type']}/{record['id']}.md"
        destination = target / relative
        outgoing, incoming = wiki.links_for(record["id"])
        write_atomic(
            destination,
            render_page(
                meta, record["body"], links=sorted(set(outgoing) | set(incoming)),
                view_path=relative, page_of=page_of,
            ),
        )
        written.append(norm_key(destination.relative_to(target)))
    emit(
        "export", wiki.root,
        f"exported {len(written)} pages to {target}",
        {"target": str(target), "files": written},
        written,
    )
    wiki.close()
    return 0

def command_doctor():
    wiki, _ = open_wiki()
    issues, fixed = [], []
    try:
        integrity = wiki.conn.execute("PRAGMA integrity_check").fetchone()[0]
    except sqlite3.DatabaseError as error:
        raise WikiError("E_DB", f"database unreadable: {error}",
                        "restore from backup; do not edit .wiki/wiki.db", 1)
    if integrity != "ok":
        issues.append({"severity": "fail", "code": "L1", "message": f"database integrity: {integrity}"})
    pages = wiki.all_pages()
    for record in pages:
        meta = record["meta"]
        if meta.get("status") == "stale":
            issues.append({"severity": "warn", "code": "L3",
                           "message": f"{record['id']} cites changed sources"})
        for key in meta.get("sources") or []:
            if not wiki.source_row(key):
                issues.append({"severity": "warn", "code": "L4",
                               "message": f"{record['id']} cites unregistered source {key}"})
    removable = wiki.removable_pages()
    unprovenanced = {record["id"]: record["id"] for record in wiki.unprovenanced_pages()}
    for record in removable:
        unprovenanced[record["id"]] += " (orphan, removable)"
    deleted = []
    for record in removable:
        if wiki.delete_page(record["id"]):
            deleted.append(record["id"])
            unprovenanced.pop(record["id"], None)
    if deleted:
        fixed.append(f"deleted {len(deleted)} unprovenanced orphan page(s)")
    if unprovenanced:
        issues.append({"severity": "warn", "code": "L7",
                       "message": "pages without usable sources: "
                                  + ", ".join(sorted(unprovenanced.values())[:10])})
    stuck = wiki.conn.execute(
        "SELECT id FROM tasks WHERE state = 'leased' AND lease_until < ?", (now_iso(),)
    ).fetchall()
    if stuck:
        issues.append({"severity": "warn", "code": "L6",
                       "message": f"expired leases: {', '.join(row['id'] for row in stuck)}"})
    if ARGS.fix:
        wiki.rebuild_all_links()
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

def command_version():
    print(f"llm-wiki {VERSION}")
    return 0

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

ARGS = None

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
