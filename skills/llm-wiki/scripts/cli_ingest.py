"""`ingest` 命令：读来源、写 raw、更新账本与观看区。"""

from __future__ import annotations

from pathlib import Path

from cli_common import (
    ARGS,
    emit,
    open_wiki,
    require_install,
    session_owner,
)
from errors import WikiError
from ingest import (
    MAX_RAW_CHARS,
    extract_text,
    ingest_decision,
    raw_facts,
    read_bytes,
)
from util import digest_bytes, digest_text, norm_key, same_stamp


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
