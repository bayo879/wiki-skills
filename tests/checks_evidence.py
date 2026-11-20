"""Stale evidence and legacy ledger checks.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import json
import shutil
import sqlite3
import tempfile
from pathlib import Path

from support import check, payload_file, route_query, run


def stale_evidence(ctx):
    stale_root = Path(tempfile.mkdtemp(prefix="llm-wiki-old-evidence-"))
    try:
        (stale_root / "a.md").write_text("# 甲\n\n甲句：客服可查进度。\n", encoding="utf-8")
        (stale_root / "b.md").write_text("# 乙\n\n乙句：七天可退。\n", encoding="utf-8")
        run(stale_root, "ingest")
        routes = run(stale_root, "next", "--kind", "route", "--batch", "2")["data"]["tasks"]
        for task in routes:
            route_query(stale_root, task, "退")
        a_task = next(t for t in routes if t["work"]["sources"][0]["key"] == "a.md")
        b_task = next(t for t in routes if t["work"]["sources"][0]["key"] == "b.md")
        run(stale_root, "submit", a_task["id"], "--payload", payload_file(
            stale_root, "old-evidence-route-a.json",
            {"decisions": [{"source": "a.md", "action": "new", "page": "refund",
                            "title": "退款", "type": "process",
                            "reason": "整理退款条件"}]},
        ))
        run(stale_root, "submit", b_task["id"], "--payload", payload_file(
            stale_root, "old-evidence-route-b.json",
            {"decisions": [{"source": "b.md", "action": "merge", "page": "refund",
                            "reason": "补充退款时限"}]},
        ))
        task = run(stale_root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
        page_payload = {
            "page": "refund", "title": "退款", "type": "process",
            "summary": "客服可查进度，七天可退。", "links": [],
            "body": (
                "## 结论\n\n客服可查进度，七天可退。\n\n"
                "## 步骤\n\n联系客服提交退款。\n\n"
                "## 注意事项\n\n以到账时间为准。\n"
            ),
            "evidence": [
                {"source": "a.md", "quote": "客服可查进度"},
                {"source": "b.md", "quote": "七天可退"},
            ],
            "boundaries": {"covered": "退款条件", "not_covered": "到账时间"},
        }
        created = run(stale_root, "submit", task["id"], "--payload",
                      payload_file(stale_root, "old-evidence-page.json", page_payload))
        check(ctx.results, "the fixture page starts with evidence from both sources",
              created["data"]["rev"] == 1, created)
        (stale_root / "b.md").write_text("# 乙\n\n乙句：半月内可退。\n", encoding="utf-8")
        run(stale_root, "ingest")
        route = run(stale_root, "next", "--kind", "route")["data"]["tasks"][0]
        route_query(stale_root, route, "退")
        run(stale_root, "submit", route["id"], "--payload", payload_file(
            stale_root, "old-evidence-route-stale.json",
            {"decisions": [{"source": "b.md", "action": "merge", "page": "refund",
                            "reason": "时限改写，正文要跟着更新"}]},
        ))
        task = run(stale_root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
        failed = run(stale_root, "submit", task["id"], "--payload", payload_file(
            stale_root, "old-evidence-stale.json",
            {**page_payload, "evidence": [{"source": "a.md", "quote": "客服可查进度"}]},
        ), expect=2)
        check(ctx.results, "old evidence is rechecked against the rewritten raw",
              failed.get("error", {}).get("code") == "E_EVIDENCE_NOT_FOUND", failed)
        task = run(stale_root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
        fixed = run(stale_root, "submit", task["id"], "--payload", payload_file(
            stale_root, "old-evidence-fixed.json",
            {**page_payload, "evidence": [
                {"source": "a.md", "quote": "客服可查进度"},
                {"source": "b.md", "quote": "半月内可退"},
            ]},
        ))
        check(ctx.results, "a fresh quote for the changed source clears the block",
              fixed["data"]["rev"] == 2, fixed)
    finally:
        shutil.rmtree(stale_root, ignore_errors=True)

    # 旧版三字段快照（b2af7c9 那代引擎建的库）：没有源指纹，只能整体判漂移。
    # 用独立工作区，避免动到上面 fixture 的任务状态。
    legacy_root = Path(tempfile.mkdtemp(prefix="llm-wiki-legacy-"))
    try:
        (legacy_root / "x.md").write_text("# 旧库来源\n\n正文。\n", encoding="utf-8")
        run(legacy_root, "ingest")
        conn = sqlite3.connect(legacy_root / ".wiki" / "wiki.db")
        legacy_id, digest, raw = conn.execute(
            "SELECT t.id, s.digest, s.raw FROM tasks t, sources s "
            "WHERE s.key = 'x.md' AND t.kind = 'route'"
        ).fetchone()
        conn.execute(
            "UPDATE tasks SET inputs = ? WHERE id = ?",
            (json.dumps({"sources": [{"key": "x.md", "digest": digest, "raw": raw}]}),
             legacy_id),
        )
        conn.commit()
        conn.close()
        run(legacy_root, "status")
        handed = run(legacy_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"]
        check(ctx.results, "a pre-fingerprint task is expired instead of crashing the claim",
              handed == [], handed)
        conn = sqlite3.connect(legacy_root / ".wiki" / "wiki.db")
        row = conn.execute(
            "SELECT state, last_error FROM tasks WHERE id = ?", (legacy_id,)
        ).fetchone()
        conn.close()
        check(ctx.results, "the invalidated legacy task keeps its reason",
              row[0] == "expired" and "predates source fingerprints" in row[1], row)
        legacy_submit = run(
            legacy_root, "submit", legacy_id,
            "--payload", payload_file(legacy_root, "legacy-route.json", {"decisions": []}),
            expect=3,
        )
        check(ctx.results, "submitting a pre-fingerprint task reports E_SOURCE_DRIFT",
              legacy_submit["error"]["code"] == "E_SOURCE_DRIFT", legacy_submit)
    finally:
        shutil.rmtree(legacy_root, ignore_errors=True)
