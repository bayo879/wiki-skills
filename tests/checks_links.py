"""Page link, broken link and export checks.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import json
import shutil
import sqlite3
import tempfile
from pathlib import Path

from support import check, payload_file, route_query, run

# 引擎模块要等 support 把脚本目录加进 sys.path，因此排在 support 之后。
import wiki


def links_flow(ctx):
    """Relations are route-decided engine data, not a body section."""
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-links-"))
    try:
        def add_page(source, page, title, sentence, links=None):
            (root / source).write_text(f"# {title}\n\n{sentence}\n", encoding="utf-8")
            run(root, "ingest")
            route = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
            route_query(root, route, "主题")
            key = route["work"]["sources"][0]["key"]
            run(root, "submit", route["id"], "--payload", payload_file(
                root, f"{page}-route.json",
                {"decisions": [{
                    "source": key, "action": "new", "page": page, "title": title,
                    "type": "concept", "links": links or [], "reason": "独立主题",
                }]},
            ))
            task = run(root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
            draft = root / ".wiki" / "drafts" / f"{page}.md"
            draft.parent.mkdir(parents=True, exist_ok=True)
            draft.write_text(
                f"## 结论\n\n{sentence}\n\n## 要点\n\n- {title}。\n",
                encoding="utf-8",
            )
            run(root, "submit", task["id"], "--payload", payload_file(
                root, f"{page}-page.json",
                {"page": page, "title": title, "type": "concept", "summary": sentence,
                 "links": list(task["template"]["links"]),
                 "body_file": str(draft.relative_to(root)),
                 "evidence": [{"source": key, "quote": sentence}],
                 "boundaries": {"covered": title, "not_covered": "其他"}},
            ))
            return task

        add_page("a.md", "page-a", "甲页", "甲的定义。")
        sibling = add_page("b.md", "page-b", "乙页", "乙依赖甲。", links=["page-a"])
        check(ctx.results, "route pre-fills links into the page template",
              sibling["template"]["links"] == ["page-a"], sibling["template"])
        run(root, "sync")

        engine = wiki.Wiki(root)
        engine.config = json.loads((root / ".wiki/wiki.json").read_text(encoding="utf-8"))
        engine.conn = sqlite3.connect(root / ".wiki/wiki.db")
        engine.conn.row_factory = sqlite3.Row
        try:
            page_a = engine.page_record("page-a")
            page_b = engine.page_record("page-b")
            check(ctx.results, "relations are not stored in the body",
                  "## 关联" not in page_a["body"] and "## 关联" not in page_b["body"],
                  (page_a["body"], page_b["body"]))
            check(ctx.results, "the view renders outgoing and incoming edges together",
                  "[乙页](乙页.md)" in engine.view_text(page_a)
                  and "[甲页](甲页.md)" in engine.view_text(page_b),
                  engine.view_text(page_a))
            check(ctx.results, "the view has exactly one relation section",
                  engine.view_text(page_b).count("## 关联") == 1,
                  engine.view_text(page_b))
            outgoing, incoming = engine.links_for("page-b")
            check(ctx.results, "only the newer page stores the edge",
                  outgoing == ["page-a"] and incoming == [], (outgoing, incoming))
            index = {item["id"]: item for item in engine.page_index()}
            check(ctx.results, "page index exposes summaries and relation counts",
                  index["page-a"]["summary"] == "甲的定义。"
                  and index["page-a"]["incoming"] == 1
                  and index["page-b"]["outgoing"] == 1,
                  (index.get("page-a"), index.get("page-b")))
            engine.write_rev(
                "orphan-page",
                {"title": "孤页", "type": "concept", "summary": "没有来源的孤页。",
                 "sources": [], "evidence": [],
                 "boundaries": {"covered": "无", "not_covered": "无"}},
                "## 结论\n\n孤页。\n\n## 要点\n\n无来源。\n",
                "import",
            )
            engine.write_rev(
                "linked-orphan",
                {"title": "无来源但有链接", "type": "concept", "summary": "无来源，但链到 page-a。",
                 "sources": [], "evidence": [], "links": ["page-a"],
                 "boundaries": {"covered": "无", "not_covered": "无"}},
                "## 结论\n\n无来源。\n\n## 要点\n\n有链接。\n",
                "import",
            )
            doctor = run(root, "doctor")["data"]
            check(ctx.results, "doctor removes unprovenanced orphans internally",
                  any("deleted 1 unprovenanced orphan" in item for item in doctor["fixed"])
                  and engine.page_record("orphan-page") is None
                  and engine.page_record("linked-orphan") is not None,
                  doctor)
            check(ctx.results, "doctor still reports linked pages without sources",
                  any(item["code"] == "L7" and "linked-orphan" in item["message"]
                      for item in doctor["issues"]),
                  doctor["issues"])
        finally:
            engine.close()

        # A merge can add an edge without touching the target page's body, and a
        # later update that omits it must keep it (add-only storage).
        (root / "c.md").write_text("# 丙\n\n甲还可以补充。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(root, route, "主题")
        key = route["work"]["sources"][0]["key"]
        run(root, "submit", route["id"], "--payload", payload_file(
            root, "merge-route.json",
            {"decisions": [{
                "source": key, "action": "merge", "page": "page-a",
                "links": ["page-b"], "reason": "补充甲页",
            }]},
        ))
        task = run(root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
        check(ctx.results, "merge carries the existing links into the update template",
              task["template"]["links"] == ["page-b"], task["template"])
        draft = root / ".wiki" / "drafts" / "page-a.md"
        draft.write_text(
            "## 结论\n\n甲的定义。\n\n## 要点\n\n- 补充。\n\n"
            "## 关联\n\n- [[page-b]]\n",
            encoding="utf-8",
        )
        run(root, "submit", task["id"], "--payload", payload_file(
            root, "merge-page.json",
            {"page": "page-a", "title": "甲页", "type": "concept", "summary": "甲的定义。",
             "links": list(task["template"]["links"]),
             "body_file": str(draft.relative_to(root)),
             "evidence": [{"source": key, "quote": "甲还可以补充。"}],
             "boundaries": {"covered": "甲", "not_covered": "其他"}},
        ))
        run(root, "sync")

        engine = wiki.Wiki(root)
        engine.config = json.loads((root / ".wiki/wiki.json").read_text(encoding="utf-8"))
        engine.conn = sqlite3.connect(root / ".wiki/wiki.db")
        engine.conn.row_factory = sqlite3.Row
        try:
            record = engine.page_record("page-a")
            check(ctx.results, "a body relation section is stripped on submit",
                  "## 关联" not in record["body"], record["body"])
            check(ctx.results, "the merge-added edge is rendered",
                  "[乙页](乙页.md)" in engine.view_text(record), engine.view_text(record))
        finally:
            engine.close()

        # Two siblings in one payload: the later one may point at the earlier one,
        # which does not exist yet at route time.
        (root / "d.md").write_text("# 兄弟讲话\n\n同一份讲话。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(root, route, "主题")
        key = route["work"]["sources"][0]["key"]
        run(root, "submit", route["id"], "--payload", payload_file(
            root, "sibling-route.json",
            {"decisions": [
                {"source": key, "action": "new", "page": "page-s1", "title": "兄弟一",
                 "type": "concept", "reason": "第一页"},
                {"source": key, "action": "new", "page": "page-s2", "title": "兄弟二",
                 "type": "concept", "links": ["page-s1"], "reason": "链到第一页"},
            ]},
        ))
        tasks = run(root, "next", "--kind", "page", "--batch", "2")["data"]["tasks"]
        by_page = {task["template"]["page"]: task for task in tasks}
        check(ctx.results, "a sibling may link to a page created by the same payload",
              by_page["page-s2"]["template"]["links"] == ["page-s1"], by_page)
        for page, title in (("page-s1", "兄弟一"), ("page-s2", "兄弟二")):
            task = by_page[page]
            draft = root / ".wiki" / "drafts" / f"{page}.md"
            draft.write_text(
                f"## 结论\n\n{title}。\n\n## 要点\n\n- {title}。\n", encoding="utf-8"
            )
            run(root, "submit", task["id"], "--payload", payload_file(
                root, f"{page}.json",
                {"page": page, "title": title, "type": "concept", "summary": f"{title}。",
                 "links": list(task["template"]["links"]),
                 "body_file": str(draft.relative_to(root)),
                 "evidence": [{"source": key, "quote": "同一份讲话。"}],
                 "boundaries": {"covered": title, "not_covered": "其他"}},
            ))
        run(root, "status")
        engine = wiki.Wiki(root)
        engine.config = json.loads((root / ".wiki/wiki.json").read_text(encoding="utf-8"))
        engine.conn = sqlite3.connect(root / ".wiki/wiki.db")
        engine.conn.row_factory = sqlite3.Row
        try:
            record = engine.page_record("page-s2")
            sibling_text = (root / "wiki" / engine.view_relpath(record)).read_text(encoding="utf-8")
            check(ctx.results, "the sibling view shows its edge",
                  "[兄弟一](兄弟一.md)" in sibling_text, sibling_text)
        finally:
            engine.close()

        # Unknown targets are rejected before any write.
        bad_root = Path(tempfile.mkdtemp(prefix="llm-wiki-links-bad-"))
        try:
            (bad_root / "x.md").write_text("# 未知\n\n未知链接。\n", encoding="utf-8")
            run(bad_root, "ingest")
            route = run(bad_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
            route_query(bad_root, route, "主题")
            key = route["work"]["sources"][0]["key"]
            rejected = run(bad_root, "submit", route["id"], "--payload", payload_file(
                bad_root, "bad-route.json",
                {"decisions": [{
                    "source": key, "action": "new", "page": "page-x", "title": "未知",
                    "type": "concept", "links": ["missing-page"], "reason": "测试",
                }]},
            ), expect=2)
            check(ctx.results, "a link to a page that does not exist is refused",
                  rejected["error"]["code"] == "E_LINK_UNRESOLVED", rejected)
        finally:
            shutil.rmtree(bad_root, ignore_errors=True)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def link_commands(ctx):
    linked = run(ctx.root, "page", "link", "anti-fraud-restore",
                 "machine-room-inspection")["data"]
    check(ctx.results, "page link adds an edge in a new revision",
          linked["rev"] == 6 and linked["added"] == ["machine-room-inspection"], linked)
    linked_page = run(ctx.root, "query", "--page", "anti-fraud-restore")["data"]
    check(ctx.results, "page link is visible in outgoing links",
          linked_page["outgoing"] == ["machine-room-inspection"], linked_page["outgoing"])
    linked_view = (ctx.root / "wiki/pages/process/反诈复机.md").read_text(encoding="utf-8")
    check(ctx.results, "page link pushes the relation section to the view",
          "## 关联" in linked_view and "[机房巡检](../concept/机房巡检.md)" in linked_view,
          linked_view)
    missing_link = run(ctx.root, "page", "link", "anti-fraud-restore",
                       "no-such-page", expect=2)
    check(ctx.results, "page link rejects an unknown target",
          missing_link["error"]["code"] == "E_LINK_UNRESOLVED", missing_link)
    self_link = run(ctx.root, "page", "link", "anti-fraud-restore",
                    "anti-fraud-restore", expect=2)
    check(ctx.results, "page link rejects linking to itself",
          self_link["error"]["code"] == "E_USAGE", self_link)
    blank_link = run(ctx.root, "page", "link", "anti-fraud-restore", " ", expect=2)
    check(ctx.results, "page link rejects blank targets",
          blank_link["error"]["code"] == "E_USAGE", blank_link)
    repeated = run(ctx.root, "page", "link", "anti-fraud-restore",
                   "machine-room-inspection")["data"]
    check(ctx.results, "page link does not rewrite an existing edge",
          repeated["rev"] == 6 and repeated["added"] == [], repeated)

    exported = run(ctx.root, "export", str(ctx.root / ".wiki/export"))["data"]["files"]
    check(ctx.results, "export writes rendered pages", exported, exported)
    exported_page = (ctx.root / ".wiki/export/process/anti-fraud-restore.md").read_text(
        encoding="utf-8")
    check(ctx.results, "export renders real page links",
          "[机房巡检](../concept/machine-room-inspection.md)" in exported_page, exported_page)
    escaped = run(ctx.root, "export", str(ctx.root.parent / "wiki-export"), expect=2)
    check(ctx.results, "export outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)
