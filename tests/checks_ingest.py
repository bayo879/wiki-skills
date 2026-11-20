"""Ingest, raw mirror editing and legacy raw note checks.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import json
import os
import shutil
import sqlite3
import tempfile
from pathlib import Path

from support import check, payload_file, route_query, run

# 引擎模块要等 support 把脚本目录加进 sys.path，因此排在 support 之后。
import wiki


def legacy_note_flow(ctx):
    """`note` 已从默认类型移除，但老库的 wiki.json 还留着它时必须照旧能用。"""
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-legacy-note-"))
    try:
        run(root, "init")
        config_path = root / ".wiki/wiki.json"
        config = json.loads(config_path.read_text(encoding="utf-8"))
        check(ctx.results, "a fresh ledger does not offer the note type",
              config["schema"]["types"] == ["concept", "entity", "process"], config["schema"])
        config["schema"]["types"].append("note")
        config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")

        (root / "a.md").write_text("# 值班交接\n\n工作日晚八点交接班。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(root, route, "值班")
        run(root, "submit", route["id"], "--payload", payload_file(root, "legacy-route.json", {
            "decisions": [{"source": "a.md", "action": "new", "page": "duty-handover",
                           "title": "值班交接", "type": "note", "reason": "老库留下的类型"}]}))
        page_task = run(root, "next")["data"]["tasks"][0]
        check(ctx.results, "a legacy note page keeps its own skeleton",
              page_task["template"]["type"] == "note"
              and page_task["template"]["body"] == "## 要点\n\n", page_task["template"])
        run(root, "submit", page_task["id"], "--payload", payload_file(root, "legacy-page.json", {
            "page": "duty-handover", "title": "值班交接", "type": "note",
            "summary": "工作日晚八点交接班。", "links": [],
            "body": "## 要点\n\n工作日晚八点交接班。\n",
            "evidence": [{"source": "a.md", "quote": "工作日晚八点交接班"}],
            "boundaries": {"covered": "交接时间", "not_covered": "交接清单"}}))
        run(root, "status")
        page = run(root, "query", "--page", "duty-handover")["data"]
        check(ctx.results, "a legacy note page is queryable and published",
              page["page"]["type"] == "note"
              and (root / "wiki/pages/note/值班交接.md").is_file(), page["page"])
    finally:
        shutil.rmtree(root, ignore_errors=True)


def ingest_and_raw(ctx):
    topic = ctx.root / "topics" / "复机办理.md"
    topic.parent.mkdir()
    topic.write_text(
        "---\n"
        "name: 复机办理\n"
        "description: 停机复机的办理路径与时长\n"
        "status: open\n"
        "updated: 2026-11-16\n"
        "---\n\n"
        "## 主题\n\n停机后如何复机、要多久。\n\n"
        "## 结论\n\n本人持证到厅办理，约两个工作日。\n\n"
        "## 依据\n\n- 页面：anti-fraud-restore — 复机路径与时长\n\n"
        "## 未决问题\n\n异地办理是否一样。\n\n"
        "## 下一步\n\n补异地办理的 raw。\n\n"
        "## 能力候选\n\n无。\n",
        encoding="utf-8",
    )

    data = run(ctx.root, "ingest")["data"]
    check(ctx.results, "ingest queues route tasks", data["pending_total"] == 2, data)
    check(ctx.results, "ingest reports the open queue",
          data["queue"]["open"] == data["queue"]["queued"] + data["queue"]["leased"]
          and data["queue"]["route"] == 2, data.get("queue"))
    check(ctx.results, "ingest skips the workflow directory",
          not any(item["key"].startswith("workflows/") for item in data["ingested"]), data["ingested"])
    check(ctx.results, "ingest skips topic files",
          not any(item["key"].startswith("topics/") for item in data["ingested"]), data["ingested"])
    check(ctx.results, "image classified unsupported", [i["key"] for i in data["unsupported"]] == ["scan.png"], data["unsupported"])

    check(ctx.results, "raw lives under the publish dir",
          (ctx.root / "wiki/raw").is_dir() and not (ctx.root / ".wiki/raw").exists())
    data = run(ctx.root, "ingest")["data"]
    raw_files = sorted(p.name for p in (ctx.root / "wiki/raw").glob("*.md"))
    check(ctx.results, "raw files are written into wiki/raw", len(raw_files) == 2, raw_files)

    # 初筛必须停在 stat：把源文件改成不可读，第二轮 ingest 仍要报 unchanged。
    # 引擎只要读了它，就会拿到 EACCES 而不是 unchanged，这条断言就会失败。
    if os.name == "posix" and os.geteuid() != 0:
        (ctx.root / "a.md").chmod(0o000)
        try:
            blind = run(ctx.root, "ingest")["data"]
        finally:
            (ctx.root / "a.md").chmod(0o644)
        check(ctx.results, "an unchanged source is never opened",
              "a.md" in blind["unchanged"] and blind["failed"] == [], blind)

    # --- Task 3: 两段式初筛、--only、判定表 -------------------------------------
    raw_candidates = sorted((ctx.root / "wiki" / "raw").glob("a*.md"))
    check(ctx.results, "a.md publishes exactly one raw file", len(raw_candidates) == 1, raw_candidates)
    raw_path = raw_candidates[0]

    edited_raw = raw_path.read_text(encoding="utf-8") + "\n补充：可线上复机。\n"
    raw_path.write_text(edited_raw, encoding="utf-8")

    edited = run(ctx.root, "ingest")["data"]
    check(ctx.results, "an edited raw survives ingest",
          raw_path.read_text(encoding="utf-8") == edited_raw, edited["ingested"])
    check(ctx.results, "an edited raw is not re-parsed", edited["ingested"] == [], edited["ingested"])

    conn = sqlite3.connect(ctx.root / ".wiki" / "wiki.db")
    ledger_raw = conn.execute(
        "SELECT raw_digest, raw_size, raw_mtime FROM sources WHERE key = ?", ("a.md",)
    ).fetchone()
    conn.close()
    raw_stat = raw_path.stat()
    raw_digest = wiki.raw_body_digest(raw_path)
    raw_size, raw_mtime = raw_stat.st_size, raw_stat.st_mtime
    check(ctx.results, "the ledger mirrors the edited raw file",
          ledger_raw is not None and ledger_raw[0] == raw_digest and ledger_raw[1] == raw_size
          and abs(ledger_raw[2] - raw_mtime) < 1e-6,
          (ledger_raw, (raw_digest, raw_size, raw_mtime)))

    conn = sqlite3.connect(ctx.root / ".wiki" / "wiki.db")
    before_b = conn.execute(
        "SELECT status, digest, raw_digest FROM sources WHERE key = ?", ("b.md",)
    ).fetchone()
    conn.close()
    forced = run(ctx.root, "ingest", "--force", "--only", "a.md")["data"]
    conn = sqlite3.connect(ctx.root / ".wiki" / "wiki.db")
    after_b = conn.execute(
        "SELECT status, digest, raw_digest FROM sources WHERE key = ?", ("b.md",)
    ).fetchone()
    conn.close()
    check(ctx.results, "--only re-parses just the named source",
          [item["key"] for item in forced["ingested"]] == ["a.md"]
          and forced["vanished"] == [] and before_b is not None and before_b == after_b,
          (forced["ingested"], forced["vanished"], before_b, after_b))
    check(ctx.results, "--force keeps the edited raw",
          raw_path.read_text(encoding="utf-8") == edited_raw, forced["ingested"])
    check(ctx.results, "the reported raw path is the edited file",
          bool(forced["ingested"]) and forced["ingested"][0]["raw"] == f"wiki/raw/{raw_path.name}",
          forced["ingested"])

    # raw 只被 touch、字节没变：账本要吸收磁盘上的 size/mtime，否则每次 ingest
    # 都会因为 same_stamp 不成立而重读重算原文。
    touched_stamp = raw_path.stat().st_mtime + 30
    os.utime(raw_path, (touched_stamp, touched_stamp))
    after_touch = run(ctx.root, "ingest")["data"]
    touched_stat = raw_path.stat()
    touched_digest = wiki.raw_body_digest(raw_path)
    touched_size, touched_mtime = touched_stat.st_size, touched_stat.st_mtime
    conn = sqlite3.connect(ctx.root / ".wiki" / "wiki.db")
    touched_row = conn.execute(
        "SELECT raw_digest, raw_size, raw_mtime FROM sources WHERE key = ?", ("a.md",)
    ).fetchone()
    conn.close()
    again = run(ctx.root, "ingest")["data"]
    check(ctx.results, "a touched raw refreshes the ledger stamps",
          after_touch["ingested"] == [] and touched_row is not None
          and touched_row[0] == touched_digest and touched_row[1] == touched_size
          and abs(touched_row[2] - touched_mtime) < 1e-6 and again["ingested"] == [],
          (after_touch["ingested"], touched_row,
           (touched_digest, touched_size, touched_mtime), again["ingested"]))

    cases = [
        ((False, "ok", 10.0, 10.0), "skip"),
        ((False, "missing", 10.0, 10.0), "rewrite"),
        ((False, "edited", 10.0, 20.0), "bookkeep_raw"),
        ((True, "ok", 10.0, 10.0), "rewrite"),
        ((True, "edited", 20.0, 10.0), "rewrite"),
        ((True, "edited", 10.0, 20.0), "bookkeep_source"),
        ((False, "ok", 10.0, 10.0, True), "parse_keep_raw"),
        ((False, "edited", 10.0, 20.0, True), "parse_keep_raw"),
        ((True, "ok", 20.0, 10.0, True), "rewrite"),
    ]
    check(ctx.results, "the ingest decision table follows the spec",
          [wiki.ingest_decision(*args) for args, _ in cases] == [want for _, want in cases], cases)
