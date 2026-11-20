"""Query routing, workflow candidates and retrieval checks.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import shutil
import sqlite3
import tempfile
from pathlib import Path

from support import check, payload_file, route_query, run


def retrieval_flow(ctx):
    """Route work must register its searches, and a started tree drains first."""
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-retrieval-"))
    try:
        (root / "a.md").write_text("# 反诈\n\n复机提醒。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        payload = {"decisions": [{"source": "a.md", "action": "new", "page": "first-page",
                                  "title": "首屏", "type": "concept", "reason": "唯一主题"}]}
        refused = run(root, "submit", route["id"], "--payload",
                      payload_file(root, "no-query.json", payload), expect=3)
        check(ctx.results, "a route cannot be decided without a registered query",
              refused["error"]["code"] == "E_NO_QUERY", refused)

        reclaimed = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        view = reclaimed["work"]["queries"]
        check(ctx.results, "the route work view exposes the query budget", view["budget"] == 5, view)
        check(ctx.results, "the route brief asks for registered searches",
              f"query --task {reclaimed['id']}" in " ".join(reclaimed["brief"]["steps"]),
              reclaimed["brief"]["steps"])

        asked = run(root, "query", "--task", reclaimed["id"], "反诈", "复机", "提醒")["data"]
        check(ctx.results, "query --task groups one block per word",
              [group["word"] for group in asked["queries"]] == ["反诈", "复机", "提醒"], asked["queries"])
        check(ctx.results, "query --task reports the spent budget",
              asked["budget"]["used"] == 3 and asked["budget"]["remaining"] == 2, asked["budget"])
        check(ctx.results, "query --task leaves workflow candidates out",
              "workflow_matches" not in asked and "workflows" not in asked, asked)
        log = (root / ".wiki" / "tasks" / reclaimed["id"] / "queries.jsonl")
        check(ctx.results, "every query --task call is logged in the task folder",
              log.exists() and len(log.read_text(encoding="utf-8").splitlines()) == 1, log)

        over = run(root, "query", "--task", reclaimed["id"], "四", "五", "六", expect=3)
        check(ctx.results, "the query budget refuses words beyond five per 1000 characters",
              over["error"]["code"] == "E_QUERY_BUDGET", over)
        repeated = run(root, "query", "--task", reclaimed["id"], "反诈")["data"]
        check(ctx.results, "repeating a word does not spend the budget",
              repeated["budget"]["used"] == 3, repeated["budget"])

        run(root, "submit", reclaimed["id"], "--payload", payload_file(root, "no-query.json", payload))
        page = run(root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]

        # A started tree drains before the next route task is handed out.
        (root / "b.md").write_text("# 另一主题\n\n别的正文。\n", encoding="utf-8")
        run(root, "ingest")
        blocked = run(root, "next", "--batch", "1", expect=3)
        check(ctx.results, "an open page task outranks a freshly queued route task",
              blocked.get("error", {}).get("code") == "E_TASK_UNSUBMITTED"
              and page["id"] in blocked["error"]["message"], blocked.get("error"))
    finally:
        shutil.rmtree(root, ignore_errors=True)

    # A lease belongs to the session that claimed it: a second session stops,
    # it does not take turns on the same queue, and a dead session's expired
    # lease is swept instead of blocking everyone.
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-owner-"))
    try:
        (root / "own.md").write_text("# 归属\n\n云网安全主题日。\n", encoding="utf-8")
        run(root, "ingest")
        claimed = run(root, "next", "--owner", "sess-a")["data"]
        check(ctx.results, "next records the claiming session", claimed["owner"] == "sess-a", claimed)
        busy = run(root, "next", "--owner", "sess-b", expect=3)
        check(ctx.results, "another session's lease stops next",
              busy["error"]["code"] == "E_INGEST_BUSY"
              and "sess-a" in busy["error"]["message"], busy)
        foreign = run(root, "submit", claimed["tasks"][0]["id"], "--owner", "sess-b", expect=3)
        check(ctx.results, "another session cannot submit a foreign lease",
              foreign["error"]["code"] == "E_TASK_FOREIGN", foreign)
        busy_ingest = run(root, "ingest", "--owner", "sess-b", expect=3)
        check(ctx.results, "a second session cannot ingest while the queue is taken",
              busy_ingest["error"]["code"] == "E_INGEST_BUSY", busy_ingest)

        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        conn.execute("UPDATE tasks SET lease_until = '2000-01-01T00:00:00Z' WHERE state = 'leased'")
        conn.commit()
        conn.close()
        swept = run(root, "next", "--owner", "sess-c")["data"]
        check(ctx.results, "an expired lease is swept, not treated as a live session",
              swept["owner"] == "sess-c" and swept["tasks"], swept)
        released = run(root, "release", swept["tasks"][0]["id"], "--owner", "sess-b")["data"]
        check(ctx.results, "release reports the owner it took the task from",
              released["owner"] == "sess-c", released)
        again = run(root, "next", "--owner", "sess-b")["data"]
        check(ctx.results, "a released task can be claimed by the next session",
              again["owner"] == "sess-b" and again["tasks"], again)
    finally:
        shutil.rmtree(root, ignore_errors=True)

    # Sibling pages keep the order the route listed them in, so a link to an
    # earlier sibling resolves on the first submit instead of by luck.
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-siblings-"))
    try:
        (root / "talk.md").write_text("# 云网安全讲话\n\n主题日启动。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(root, route, "云网安全")
        run(root, "submit", route["id"], "--payload", payload_file(root, "sibling-order.json", {
            "decisions": [
                {"source": "talk.md", "action": "new", "page": "sib-one", "title": "兄弟一",
                 "type": "concept", "reason": "第一页"},
                {"source": "talk.md", "action": "new", "page": "sib-two", "title": "兄弟二",
                 "type": "concept", "links": ["sib-one"], "reason": "链到第一页"},
            ]}))
        order = [task["template"]["page"] for task in
                 run(root, "next", "--kind", "page", "--batch", "2")["data"]["tasks"]]
        check(ctx.results, "sibling page tasks come out in the order the route listed them",
              order == ["sib-one", "sib-two"], order)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def query_candidates(ctx):
    found = run(ctx.root, "query", "开户")["data"]
    check(ctx.results, "query carries workflow candidates without loading them",
          found.get("workflow_matches") == 1
          and found.get("workflows", [{}])[0].get("id") == "account-opening"
          and found.get("workflows", [{}])[0].get("applies") == "新客户首次开户，且需要核验身份。"
          and found.get("workflows", [{}])[0].get("load") == 'workflow load "account-opening"',
          found)
    searched = run(ctx.root, "workflow", "search", "开户")["data"]
    check(ctx.results, "workflow search finds a project workflow",
          searched["matches"] == 1 and searched["workflows"][0]["id"] == "account-opening"
          and searched["workflows"][0]["tools"] == 1
          and searched["workflows"][0].get("applies") == "新客户首次开户，且需要核验身份。",
          searched)
    ctx.workflow_main.write_text(
        ctx.workflow_main.read_text(encoding="utf-8") + "\n补充：支持见证开户。\n",
        encoding="utf-8",
    )
    live = run(ctx.root, "workflow", "search", "见证开户")["data"]
    check(ctx.results, "workflow edits are searchable without ingest",
          live["matches"] == 1, live)
    invalid = run(ctx.root, "workflow", "search", "坏流程")["data"]
    check(ctx.results, "invalid workflows stay out of workflow search",
          invalid["matches"] == 0 and invalid.get("message") == "无可用workflow", invalid)

    topics_hit = run(ctx.root, "query", "复机")["data"]
    builtin_hit = run(ctx.root, "workflow", "search", "话题")["data"]
    check(ctx.results, "workflow search finds the builtin explicitly",
          builtin_hit["matches"] >= 1
          and builtin_hit["workflows"][0]["id"] == "topic-exploration"
          and builtin_hit["workflows"][0]["source"] == "builtin",
          builtin_hit)
    check(ctx.results, "query finds the saved topic",
          topics_hit["topic_matches"] == 1
          and topics_hit["topics"][0]["name"] == "复机办理"
          and topics_hit["topics"][0]["path"].startswith("topics/"),
          topics_hit["topics"])
    check(ctx.results, "topic hits stay separate from page hits",
          topics_hit["matches"] == 0, topics_hit)
