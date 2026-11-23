"""工作区级命令：init/check/status/next/release/sync/export/doctor/version。"""

from __future__ import annotations

import health
import sqlite3
from pathlib import Path

from cli.common import (
    ARGS,
    SCRIPTS_DIR,
    VERSION,
    emit,
    open_wiki,
    require_install,
    session_owner,
)
from errors import WikiError
from frontmatter import split_frontmatter
from pages import render_page
from util import norm_key, now_iso, write_atomic
from view import sync_view_file
from workspace import WIKI_DIR, inside_root, window_safe


def command_check():
    report = health.check_installation(SCRIPTS_DIR, Path.cwd())
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
