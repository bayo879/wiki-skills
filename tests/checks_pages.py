"""Batch page submission and multi-file import checks.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import shutil
import tempfile
from pathlib import Path

from support import check, payload_file, route_query, run


def batch_flow(ctx):
    """A batch stays inside one tree, so two sources never share a batch."""
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-batch-"))
    try:
        (root / "a.md").write_text("# 拆页来源\n\n三个可以分开的主题。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(root, route, "拆页")
        run(root, "submit", route["id"], "--payload", payload_file(root, "batch-a.json", {
            "decisions": [
                {"source": "a.md", "action": "new", "page": "pa1", "title": "甲一",
                 "type": "concept", "reason": "主题一"},
                {"source": "a.md", "action": "new", "page": "pa2", "title": "甲二",
                 "type": "concept", "reason": "主题二"},
                {"source": "a.md", "action": "new", "page": "pa3", "title": "甲三",
                 "type": "concept", "reason": "主题三"},
            ]}))

        (root / "b.md").write_text("# 另一来源\n\n单独一页。\n", encoding="utf-8")
        run(root, "ingest")
        second = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(root, second, "另一来源")
        run(root, "submit", second["id"], "--payload", payload_file(root, "batch-b.json", {
            "decisions": [{"source": "b.md", "action": "new", "page": "pb", "title": "乙页",
                           "type": "concept", "reason": "独立主题"}]}))

        def submit_page(page_id, title, task_id, source, quote):
            draft = root / ".wiki" / "drafts" / f"{page_id}.md"
            draft.parent.mkdir(parents=True, exist_ok=True)
            draft.write_text(
                f"## 结论\n\n{title}的一句话结论。\n\n## 要点\n\n- 一句话。\n",
                encoding="utf-8",
            )
            run(root, "submit", task_id, "--payload", payload_file(
                root, f"{page_id}.json",
                {"page": page_id, "title": title, "type": "concept",
                 "summary": f"{title}的一句话结论。", "links": [],
                 "body_file": str(draft.relative_to(root)),
                 "evidence": [{"source": source, "quote": quote}],
                 "boundaries": {"covered": title, "not_covered": "其他"}},
            ))

        first = run(root, "next", "--batch", "2")["data"]["tasks"]
        check(ctx.results, "a batch smaller than the tree stays inside one tree",
              [task["template"]["page"] for task in first] == ["pa1", "pa2"], first)
        check(ctx.results, "page tasks expose the tree they belong to",
              {task["work"]["tree"] for task in first} == {route["id"]},
              [task["work"].get("tree") for task in first])

        blocked = run(root, "next", "--batch", "2", expect=3)
        check(ctx.results, "a taken batch is reported instead of reissued",
              blocked.get("error", {}).get("code") == "E_TASK_UNSUBMITTED"
              and all(task["id"] in blocked["error"]["message"] for task in first),
              blocked.get("error"))

        for task in first:
            submit_page(task["template"]["page"], task["template"]["title"], task["id"],
                        "a.md", "三个可以分开的主题")

        rest = run(root, "next", "--batch", "2")["data"]["tasks"]
        check(ctx.results, "a short tree emits one task instead of borrowing another tree",
              [task["template"]["page"] for task in rest] == ["pa3"]
              and {task["work"]["tree"] for task in rest} == {route["id"]},
              [(task["template"].get("page"), task["work"].get("tree")) for task in rest])
        submit_page("pa3", rest[0]["template"]["title"], rest[0]["id"],
                    "a.md", "三个可以分开的主题")

        other = run(root, "next", "--batch", "5")["data"]["tasks"]
        check(ctx.results, "a drained tree hands the next tree its turn",
              [task["template"]["page"] for task in other] == ["pb"]
              and {task["work"]["tree"] for task in other} == {second["id"]},
              [(task["template"].get("page"), task["work"].get("tree")) for task in other])
        submit_page("pb", other[0]["template"]["title"], other[0]["id"],
                    "b.md", "单独一页")

        (root / "c.md").write_text("# 第三来源\n\n还没开始。\n", encoding="utf-8")
        run(root, "ingest")
        alone = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        check(ctx.results, "a route task is its own tree",
              "decisions" in alone["template"] and alone["work"]["tree"] == alone["id"],
              {"template": list(alone["template"]), "tree": alone["work"].get("tree")})
    finally:
        shutil.rmtree(root, ignore_errors=True)


def batch_pages(ctx):
    page = run(ctx.root, "query", "--page", "anti-fraud-restore")["data"]
    check(ctx.results, "update keeps old and new sources",
          set(page["page"]["sources"]) == {"a.md", ctx.c_key}, page["page"]["sources"])
    check(ctx.results, "update keeps old evidence",
          any(item.get("source") == "a.md" for item in page["page"]["evidence"]),
          page["page"]["evidence"])

    (ctx.root / "d.md").write_text("# 机房巡检\n\n巡检周期为每周一次。\n", encoding="utf-8")
    (ctx.root / "e.md").write_text("# 机房巡检\n\n巡检记录保留一年。\n", encoding="utf-8")
    run(ctx.root, "ingest")
    first = run(ctx.root, "next", "--kind", "route")["data"]["tasks"][0]
    route_query(ctx.root, first, "机房巡检")
    first_key = first["work"]["sources"][0]["key"]
    run(ctx.root, "submit", first["id"], "--payload", payload_file(
        ctx.root, "batch-route-1.json",
        {"decisions": [{"source": first_key, "action": "new", "page": "machine-room-inspection",
                        "title": "机房巡检", "type": "concept", "reason": "新主题"}]},
    ))
    second = run(ctx.root, "next", "--kind", "route")["data"]["tasks"][0]
    route_query(ctx.root, second, "机房巡检")
    second_key = second["work"]["sources"][0]["key"]
    check(ctx.results, "pending page is visible to the next route",
          any(item["id"] == "machine-room-inspection" for item in second["work"]["pending_pages"]),
          second["work"]["pending_pages"])
    run(ctx.root, "submit", second["id"], "--payload", payload_file(
        ctx.root, "batch-route-2.json",
        {"decisions": [{"source": second_key, "action": "merge", "page": "machine-room-inspection",
                        "reason": "同主题补充"}]},
    ))
    task = run(ctx.root, "next")["data"]["tasks"][0]
    check(ctx.results, "pending merge folds both sources into one page task",
          {item.get("source") for item in task["template"]["evidence"]} == {"d.md", "e.md"},
          task["template"]["evidence"])
    batch_payload = {
        "page": "machine-room-inspection", "title": "机房巡检", "type": "concept",
        "summary": "机房每周巡检一次，巡检记录保留一年。",
        "links": [], "body": "## 结论\n\n机房每周巡检一次，巡检记录保留一年。\n\n## 要点\n\n每周巡检一次；记录保留一年。\n",
        "evidence": [{"source": "d.md", "quote": "巡检周期为每周一次"},
                     {"source": "e.md", "quote": "巡检记录保留一年"}],
        "boundaries": {"covered": "巡检周期与记录保存", "not_covered": "巡检项目清单"},
    }
    run(ctx.root, "submit", task["id"], "--payload", payload_file(ctx.root, "batch-page.json", batch_payload))
    page = run(ctx.root, "query", "--page", "machine-room-inspection")["data"]
    check(ctx.results, "batch page keeps both sources",
          set(page["page"]["sources"]) == {"d.md", "e.md"}, page["page"]["sources"])
