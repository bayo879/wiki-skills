"""llm-wiki 命令行：参数解析、输出格式与各命令的实现。

命令只通过 Wiki 的公开方法读写账本；输出契约见 docs/readme/cli.md，
进度与错误码见 docs/agent/engine.md。
"""

from __future__ import annotations

import argparse
import json
import re
import sqlite3
import sys
from datetime import datetime, timezone
from pathlib import Path

import health
import scenarios
import search
import topics
from errors import WikiError
from frontmatter import split_frontmatter
from ingest import (
    MAX_RAW_CHARS,
    extract_text,
    file_facts,
    ingest_decision,
    raw_facts,
    read_bytes,
)
from ledger import CONFIG_NAME, Wiki
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


VERSION = "0.4.3"
ID_RE = re.compile(r"^[a-z0-9][a-z0-9._-]*$")

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

def open_wiki(require_sources=False, dry_run=False, push=True):
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
                disk_digest, disk_size, disk_mtime = file_facts(wiki.root / raw_rel)
                wiki.upsert_source(
                    path.relative_to(wiki.scan_root), digest, stat.st_size, stat.st_mtime,
                    raw_rel, row["parser"], row["status"],
                    disk_digest, disk_size, disk_mtime,
                    raw_id=raw_id,
                    last_parsed_digest=row["last_parsed_digest"],
                )
                # 采纳用户改动后的原文即账本事实：仍在排队或租用的快照要对齐，
                # 否则同一份原文会被自己的任务判成漂移。
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
        raw_digest, raw_size, raw_mtime = file_facts(wiki.root / raw_rel)
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
        if wiki.source_in_open_task(key) is None:
            queued.append(
                wiki.enqueue_task("route", key, {"sources": [wiki.source_snapshot(key)]})
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
    # 存在，下一轮 ingest 只会走「未变化」分支，不再入队——库会永远空转。这里补一次
    # 对账：pending 且没有未关闭任务的来源重新入队。没有 raw 的来源（超大、解析失败）
    # 不排队，它们本来就没有可提交的内容。
    for row in wiki.all_sources():
        if row["status"] != "pending" or not row["raw"]:
            continue
        if selected is not None and row["key"] not in selected:
            continue
        if wiki.source_in_open_task(row["key"]) is not None:
            continue
        queued.append(
            wiki.enqueue_task("route", row["key"], {"sources": [wiki.source_snapshot(row["key"])]})
        )
    # 最后一件事：没有任何来源指向的旧 raw 行连同文件一起回收，观看区不留孤儿副本。
    reclaimed = wiki.collect_raws()
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
        f"{len(remaining)} pending, open queue {counts['open']}",
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
        "next": "run `next`" if tasks["queued"] else ("wait for leased tasks to expire" if tasks["leased"] else "run `ingest`"),
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
    batch = ARGS.batch or 1
    claimed, resumed = wiki.claim_tasks(kind=ARGS.kind, batch=max(1, batch))
    tasks = []
    lines = []
    for row in claimed + resumed:
        item = wiki.materialize_task(row)
        tasks.append(item)
        lines.append(f"{row['id']}  {item['brief']['goal']}")
    data = {"tasks": tasks, "count": len(tasks)}
    if not tasks:
        emit("next", wiki.root, "no queued tasks", data, [])
        wiki.close()
        return 0
    if len(resumed) == len(tasks):
        summary = (
            f"resuming {len(resumed)} unfinished task(s); keep the payload you already "
            "have and submit"
        )
    elif resumed:
        summary = (
            f"{len(tasks)} task(s) ready ({len(resumed)} unfinished resumed); read each "
            "brief, fill its payload.json, then submit"
        )
    else:
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

def submit_route(wiki, row, payload):
    inputs = json.loads(row["inputs"])
    available = [item["key"] for item in inputs["sources"]]
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
        if key not in available:
            raise WikiError("E_TASK_SCOPE", f"source {key!r} is not in this task",
                            f"task sources: {', '.join(available)}", 3)
        action = decision.get("action")
        if action not in ("new", "merge", "skip"):
            raise WikiError("E_PAYLOAD", f"action {action!r} must be new, merge or skip",
                            "pick one action per source", 2)
        if not str(decision.get("reason") or "").strip():
            raise WikiError("E_PAYLOAD", f"{key}: reason must not be empty",
                            "one sentence explaining the choice", 2)
        seen.add(key)
        if action == "skip":
            skipped.add(key)
            continue
        page_id = str(decision.get("page") or "").strip().lower()
        if not ID_RE.match(page_id):
            raise WikiError("E_BAD_ID", f"{key}: page id {page_id!r} is not a lowercase slug",
                            "use e.g. `filing-process`", 2)
        if (key, page_id) in targets:
            raise WikiError(
                "E_PAYLOAD",
                f"{key}: page {page_id} is decided twice in one payload",
                "one decision per source and page; split a source across different pages",
                2,
            )
        targets.add((key, page_id))
        if action == "new":
            if page_id in new_ids:
                raise WikiError(
                    "E_PAYLOAD",
                    f"page {page_id} is created by more than one decision",
                    "create a page once, then merge the other sources into it", 2,
                )
            new_ids.add(page_id)
    missing = [item for item in available if item not in seen]
    if missing:
        raise WikiError(
            "E_PAYLOAD",
            f"decisions must cover every input source; missing {sorted(missing)}",
            "submit at least one decision per source", 2,
        )
    conflicting = sorted(skipped & {key for key, _ in targets})
    if conflicting:
        raise WikiError(
            "E_PAYLOAD",
            f"source {conflicting[0]}: skip cannot be combined with new or merge",
            "skip a source on its own, or decide its pages without skip", 2,
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
        action = decision["action"]
        if action == "skip":
            if decision.get("links"):
                raise WikiError(
                    "E_PAYLOAD", f"{key}: skip decision cannot carry links",
                    "drop links, or decide the page without skip", 2,
                )
            plan.append({"action": "skip", "key": key})
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
            plan.append({"action": "new", "key": key, "page": page_id,
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
            plan.append({"action": "merge", "key": key, "page": page_id,
                         "rev": record["rev"] if record else None, "links": links})
    # A page created in this same payload must be queued before sources merge
    # into it; otherwise the merge would create an update task for a missing page.
    plan.sort(key=lambda item: 0 if item["action"] == "new" else 1)
    page_tasks = []
    for item in plan:
        key = item["key"]
        if item["action"] == "skip":
            wiki.set_source_status(key, "skipped")
            continue
        entry = wiki.source_snapshot(key)
        existing = wiki.open_task_for_page(item["page"])
        if existing:
            wiki.attach_page_source(existing["id"], key)
            if item["links"]:
                wiki.attach_page_links(existing["id"], item["links"])
            page_tasks.append(existing["id"])
            continue
        inputs = {
            "page": item["page"],
            "base_rev": None if item["action"] == "new" else item["rev"],
            "sources": [entry],
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
        wiki.attach_page_source(task_id, key)
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
    wiki.release_task(ARGS.id)
    emit("release", wiki.root, f"released {ARGS.id}", {"task": ARGS.id}, [])
    wiki.close()
    return 0

def command_read():
    wiki, _ = open_wiki()
    ref = ARGS.ref
    meta = {}
    if ref.startswith("page:"):
        page_id = ref[5:]
        record = wiki.page_record(page_id)
        if not record:
            raise WikiError("E_NOT_FOUND", f"no page {page_id}", "run `query` to find the page id", 3)
        meta = record["meta"]
        body = record["body"]
        location = wiki.page_ref(record)
    else:
        path = resolve_ref(wiki, ref)
        text = path.read_text(encoding="utf-8", errors="replace")
        body = text
        # frontmatter 是文本属性，不是路径属性：原文搬到观看区 raw/ 后，不能再靠 .wiki 判定。
        if path.suffix == ".md" and text.startswith("---"):
            meta, body = split_frontmatter(text)
        location = norm_key(path.relative_to(wiki.root)) if window_safe(wiki.root, path) else str(path)
    lines = body.splitlines()
    offset = max(1, ARGS.offset)
    limit = max(1, min(ARGS.limit, 2000))
    window = lines[offset - 1: offset - 1 + limit]
    next_offset = offset + len(window) if offset - 1 + len(window) < len(lines) else None
    data = {
        "path": location,
        "offset": offset,
        "limit": limit,
        "total_lines": len(lines),
        "next_offset": next_offset,
        "meta": dump_meta(meta),
        "content": "\n".join(window),
    }
    emit(
        "read", wiki.root,
        f"{data['path']} lines {offset}-{offset + len(window) - 1} of {len(lines)}",
        data,
        [line for line in window],
    )
    wiki.close()
    return 0

def resolve_ref(wiki, ref):
    if ref.startswith("raw:"):
        key = ref[4:]
        row = wiki.source_row(key)
        if not row or not row["raw"]:
            raise WikiError(
                "E_NOT_FOUND", f"no raw text for {key}",
                "unsupported or oversize sources keep no raw text; run `status`", 3,
            )
        return wiki.root / row["raw"]
    candidate = (wiki.root / ref).resolve()
    if not window_safe(wiki.root, candidate):
        raise WikiError("E_PATH", "ref escapes the workspace root",
                        "use a path inside the workspace", 2)
    if not candidate.exists():
        raise WikiError("E_NOT_FOUND", f"no file {ref}", "check the path, or use `query` first", 3)
    return candidate

def command_query():
    wiki, _ = open_wiki()
    if ARGS.page:
        record = wiki.page_record(ARGS.page)
        if not record:
            raise WikiError("E_NOT_FOUND", f"no page {ARGS.page}", "search with `query <text>` first", 3)
        outgoing, incoming = wiki.links_for(ARGS.page)
        data = {
            "page": dump_meta(record["meta"]),
            "rev": record["rev"],
            "path": wiki.page_ref(record),
            "outgoing": outgoing,
            "incoming": incoming,
            "body": record["body"],
        }
        emit(
            "query", wiki.root,
            f"page {ARGS.page} rev {record['rev']}: {len(outgoing)} outgoing, {len(incoming)} incoming links",
            data,
            [f"outgoing: {', '.join(outgoing) or '(none)'}",
             f"incoming: {', '.join(incoming) or '(none)'}",
             "",
             record["body"]],
        )
        wiki.close()
        return 0
    terms = [term for term in re.split(r"\s+", (ARGS.text or "").strip()) if term]
    if not terms:
        raise WikiError("E_USAGE", "query needs text or --page", "example: query 反诈 复机", 2)
    scored, results = search.search_pages(wiki, terms, ARGS.limit)
    topic_hits = topics.match(wiki.root, terms, ARGS.limit)
    data = {
        "query": " ".join(terms),
        "matches": len(scored),
        "returned": len(results),
        "results": results,
        "topic_matches": len(topic_hits),
        "topics": topic_hits,
        "boundary": "hit count is a ranked subset, not proof of full coverage",
    }
    sections = []
    pages = [
        f"{item['id']}  [{item['type']}/{item['status']}]  {item['title']}\n    {item['snippet']}"
        for item in results
    ]
    if pages:
        sections.append(pages)
    if topic_hits:
        block = ["topics (read before exploring the same theme again):"]
        for item in topic_hits:
            block.append(f"{item['name']}  [{item['status'] or 'open'}]  {item['description']}")
            block.append(f"    path: {item['path']}  read: {item['read']}")
        sections.append(block)
    lines = []
    for block in sections:
        if lines:
            lines.append("")
        lines.extend(block)
    summary = f"{len(scored)} matching pages"
    if topic_hits:
        summary += f", {len(topic_hits)} matching topics"
    summary += f", showing {len(results)} pages for {' '.join(terms)}"
    emit("query", wiki.root, summary, data, lines)
    wiki.close()
    return 0

def command_scenario():
    workspace = Path.cwd().resolve()
    root = Path(ARGS.root).resolve() if ARGS.root else workspace
    if not window_safe(workspace, root):
        raise WikiError(
            "E_PATH", "scenario root escapes the workspace",
            "run from the workspace, or pass --root inside it", 2,
        )
    if ARGS.scenario_command == "search":
        terms = [term.strip() for term in ARGS.text if term.strip()]
        if not terms:
            raise WikiError(
                "E_USAGE", "scenario search needs text",
                'example: scenario search "开户"', 2,
            )
        hits = scenarios.match(root, terms, ARGS.limit)
        data = {"query": " ".join(terms), "matches": len(hits), "scenarios": hits}
        lines = []
        for item in hits:
            alias = f"（{item['alias_name']}）" if item["alias_name"] else ""
            lines.append(f"{item['id']}{alias}  {item['description']}")
            lines.append(f"    path: {item['path']}  load: {item['load']}")
        emit("scenario search", root, f"{len(hits)} matching scenarios", data, lines)
        return 0
    if ARGS.scenario_command == "list":
        data = scenarios.inventory(root)
        summary = f"{data['valid']} valid, {data['count'] - data['valid']} invalid scenarios"
        lines = []
        for item in data["scenarios"]:
            state = "ok" if item["valid"] else f"invalid: {item['error']}"
            alias = f"（{item['alias_name']}）" if item["alias_name"] else ""
            lines.append(f"{item['id']}{alias}  [{state}]  {item['description']}")
            lines.append(f"    path: {item['path']}  resources: {len(item['resources'])}")
        emit("scenario list", root, summary, data, lines)
        return 0
    data = scenarios.load(root, ARGS.id)
    emit(
        "scenario load", root,
        f"scenario {data['scenario']['id']}: {len(data['resources'])} resources",
        data,
        [f"path: {data['path']}", f"description: {data['scenario']['description']}", "", data["content"]],
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
    for record in wiki.all_pages():
        meta = dict(record["meta"])
        meta["status"] = wiki.computed_status(meta)
        destination = target / meta["type"] / f"{record['id']}.md"
        outgoing, incoming = wiki.links_for(record["id"])
        write_atomic(
            destination,
            render_page(meta, record["body"], links=sorted(set(outgoing) | set(incoming))),
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
    known = {record["id"] for record in pages}
    for record in pages:
        meta = record["meta"]
        if meta.get("status") == "stale":
            issues.append({"severity": "warn", "code": "L3",
                           "message": f"{record['id']} cites changed sources"})
        for key in meta.get("sources") or []:
            if not wiki.source_row(key):
                issues.append({"severity": "warn", "code": "L4",
                               "message": f"{record['id']} cites unregistered source {key}"})
    broken = []
    for row in wiki.conn.execute("SELECT src, dst FROM links"):
        if row["dst"] not in known and row["src"] in known:
            broken.append(f"{row['src']} -> {row['dst']}")
    if broken:
        issues.append({"severity": "warn", "code": "L5",
                       "message": f"broken links: {', '.join(sorted(broken)[:10])}"})
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
    read = sub.add_parser("read", parents=[common], help="read raw text or a page by ref")
    read.add_argument("ref")
    read.add_argument("--offset", type=int, default=1)
    read.add_argument("--limit", type=int, default=200)
    query = sub.add_parser("query", parents=[common], help="search pages and topics, or inspect one page")
    query.add_argument("text", nargs="?")
    query.add_argument("--page")
    query.add_argument("--limit", type=int, default=8)
    scenario = sub.add_parser("scenario", help="inspect scenario capabilities")
    scenario_sub = scenario.add_subparsers(dest="scenario_command", required=True)
    scenario_sub.add_parser("list", parents=[common], help="list scenario capabilities and validity")
    search_p = scenario_sub.add_parser("search", parents=[common], help="explicitly search scenario capabilities")
    search_p.add_argument("text", nargs="*")
    search_p.add_argument("--limit", type=int, default=5)
    load_p = scenario_sub.add_parser("load", parents=[common], help="load one scenario flow and its resources")
    load_p.add_argument("id")
    page = sub.add_parser("page", help="human edits to an existing page")
    page_sub = page.add_subparsers(dest="page_command", required=True)
    import_p = page_sub.add_parser("import", parents=[common], help="adopt a hand-edited page file")
    import_p.add_argument("id")
    import_p.add_argument("--from", dest="from_path", required=True)
    restore = page_sub.add_parser("restore", parents=[common], help="restore an older revision")
    restore.add_argument("id")
    restore.add_argument("--rev", type=int, required=True)
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
    "read": command_read,
    "query": command_query,
    "export": command_export,
    "status": command_status,
    "sync": command_sync,
    "doctor": command_doctor,
    "version": command_version,
}

LOCKED_COMMANDS = frozenset({
    "init", "ingest", "next", "submit", "release", "read", "query",
    "export", "status", "sync", "doctor", "page import", "page restore",
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
        handler = (
            command_page_import if ARGS.page_command == "import" else command_page_restore
        )
    elif command == "scenario":
        command = f"scenario {ARGS.scenario_command}"
        handler = command_scenario
    else:
        handler = COMMANDS[command]
    try:
        # 试运行不得创建 `.wiki/`，此时也没有可争用的写入目标，直接跳过排队。
        dry_only = command == "ingest" and ARGS.dry_run and not (root / WIKI_DIR).exists()
        if command in LOCKED_COMMANDS and not dry_only:
            with workspace_lock(root):
                return handler()
        return handler()
    except WikiError as error:
        return fail(command, root, error)
    except KeyboardInterrupt:
        return 130
