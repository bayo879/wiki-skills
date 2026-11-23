"""`submit` 命令与两条提交路径：route（领取任务的提交）与 page（直接提交页面）。"""

from __future__ import annotations

import json
from datetime import datetime, timezone

from cli.common import (
    ARGS,
    ID_RE,
    emit,
    open_wiki,
    read_payload,
    require_install,
    session_owner,
)
from errors import WikiError
from pages import normalize_links
from util import parse_iso
from validation import check_links, validate_page_payload


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
