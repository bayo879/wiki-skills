"""One route splitting into several pages.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import shutil
import sqlite3
import tempfile
from pathlib import Path

from support import check, payload_file, route_query, run


def multi_page_split(ctx):
    split_root = Path(tempfile.mkdtemp(prefix="llm-wiki-split-"))
    try:
        (split_root / "talk.md").write_text(
            "# 云网安全讲话\n\n主题日活动启动。\n\n双零行动：变更操作零失误。\n",
            encoding="utf-8",
        )
        run(split_root, "ingest")
        split_route = run(split_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        split_key = split_route["work"]["sources"][0]["key"]
        duplicate = run(
            split_root, "submit", split_route["id"],
            "--payload", payload_file(split_root, "split-dup.json", {"decisions": [
                {"source": split_key, "action": "new", "page": "same-page",
                 "title": "同页", "type": "concept", "reason": "重复"},
                {"source": split_key, "action": "new", "page": "same-page",
                 "title": "同页", "type": "concept", "reason": "重复"},
            ]}),
            expect=2,
        )
        check(ctx.results, "the same source cannot decide one page twice",
              duplicate["error"]["code"] == "E_PAYLOAD", duplicate)
        split_route = run(split_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        mixed = run(
            split_root, "submit", split_route["id"],
            "--payload", payload_file(split_root, "split-mixed.json", {"decisions": [
                {"source": split_key, "action": "skip", "reason": "跳过"},
                {"source": split_key, "action": "new", "page": "mixed-page",
                 "title": "混用", "type": "concept", "reason": "混用"},
            ]}),
            expect=2,
        )
        check(ctx.results, "skip cannot be combined with new or merge",
              mixed["error"]["code"] == "E_PAYLOAD", mixed)
        split_route = run(split_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(split_root, split_route, "主题日", "双零")
        split_submit = run(
            split_root, "submit", split_route["id"],
            "--payload", payload_file(split_root, "split-route.json", {"decisions": [
                {"source": split_key, "action": "new", "page": "theme-day",
                 "title": "主题日活动", "type": "concept", "reason": "活动本身"},
                {"source": split_key, "action": "new", "page": "dual-zero",
                 "title": "双零行动", "type": "concept", "reason": "独立行动"},
            ]}),
        )
        check(ctx.results, "one source can be split into two queued pages",
              len(split_submit["data"]["queued_page_tasks"]) == 2, split_submit)
        split_tasks = run(split_root, "next", "--kind", "page", "--batch", "5")["data"]["tasks"]
        check(ctx.results, "both split page tasks are claimable", len(split_tasks) == 2, split_tasks)
        split_bodies = {
            "theme-day": "## 结论\n\n主题日活动启动。\n\n## 要点\n\n- 主题：云网安全。\n",
            "dual-zero": "## 结论\n\n双零行动启动。\n\n## 要点\n\n- 目标：变更操作零失误。\n",
        }
        for index, split_task in enumerate(split_tasks):
            page_id = split_task["template"]["page"]
            draft = split_root / ".wiki/drafts" / f"{page_id}.md"
            draft.parent.mkdir(parents=True, exist_ok=True)
            draft.write_text(split_bodies[page_id], encoding="utf-8")
            quote = "主题日活动启动。" if page_id == "theme-day" else "变更操作零失误"
            accepted = run(
                split_root, "submit", split_task["id"],
                "--payload", payload_file(split_root, f"{page_id}.json", {
                    "page": page_id,
                    "title": "主题日活动" if page_id == "theme-day" else "双零行动",
                    "type": "concept",
                    "summary": "一句话摘要。",
                    "links": [],
                    "body_file": str(draft.relative_to(split_root)),
                    "evidence": [{"source": split_key, "quote": quote}],
                    "boundaries": {"covered": "这一页", "not_covered": "其他"},
                }),
            )
            check(ctx.results, f"split page {page_id} submits", accepted["data"]["rev"] == 1, accepted)
            conn = sqlite3.connect(split_root / ".wiki" / "wiki.db")
            source_status = conn.execute(
                "SELECT status FROM sources WHERE key = ?", (split_key,)
            ).fetchone()[0]
            conn.close()
            expected = "done" if index == 1 else "pending"
            check(ctx.results, f"the split source is {expected} after page {index + 1}",
                  source_status == expected, source_status)
    finally:
        shutil.rmtree(split_root, ignore_errors=True)
