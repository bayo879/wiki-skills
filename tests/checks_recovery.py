"""Source drift recovery and evidence splitting helpers.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import json
import re
import sqlite3

from support import body_file, check, payload_file, route_query, run

# 引擎模块要等 support 把脚本目录加进 sys.path，因此排在 support 之后。
import wiki
from util import split_provenance


def drift_recovery(ctx):
    (ctx.root / "f.md").write_text("# 复机进度查询\n\n可拨打客服确认进度。\n", encoding="utf-8")
    run(ctx.root, "ingest")
    # 发布会切到 view 之后，旧的 wiki/ 目录成了普通素材，前面的 ingest 会为它排队；
    # 这里一次取够所有路由任务，只留下 f.md 的那条，其余放回队列。
    routes = run(ctx.root, "next", "--kind", "route", "--batch", "50")["data"]["tasks"]
    drift_route = None
    for task in routes:
        if task["work"]["sources"][0]["key"] == "f.md":
            drift_route = task
        else:
            run(ctx.root, "release", task["id"])
    route_query(ctx.root, drift_route, "复机")
    drift_key = drift_route["work"]["sources"][0]["key"]
    run(ctx.root, "submit", drift_route["id"], "--payload", payload_file(ctx.root, "drift-route.json", {"decisions": [
        {"source": drift_key, "action": "new", "page": "restore-progress",
         "title": "复机进度查询", "type": "process", "reason": "独立主题"},
    ]}))
    drift_task = run(ctx.root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]

    # --- Task 4: 快照八字段、领取/提交失效、树唯一性 -------------------------
    # 快照要同时认下"源文件版本"和"原文版本"，缺一侧就判不出原文被改。
    conn = sqlite3.connect(ctx.root / ".wiki" / "wiki.db")
    snap = json.loads(conn.execute(
        "SELECT inputs FROM tasks WHERE id = ?", (drift_task["id"],)
    ).fetchone()[0])["sources"][0]
    conn.close()
    check(ctx.results, "the page task snapshot carries all nine fields",
          set(snap) == {"key", "raw_id", "source_size", "source_mtime", "source_digest",
                        "raw", "raw_size", "raw_mtime", "raw_digest"}, snap)
    check(ctx.results, "the snapshot names the raw the source mounts",
          snap["raw_id"] == snap["raw_digest"], snap)
    check(ctx.results, "the snapshot records both the source and its raw copy",
          snap["raw"] != "" and snap["source_size"] > 0 and snap["raw_size"] > 0
          and len(snap["source_digest"]) == 32 and len(snap["raw_digest"]) == 32, snap)

    # CLI 路径会先被 ingest 判重与 attach_task_source 拦住，所以越界组合只能在
    # 进程内直接调 enqueue_task 才看得到：另一个 page 树想用同一个源必须被拒。
    engine = wiki.Wiki(ctx.root)
    engine.ensure()
    conflict = None
    try:
        engine.enqueue_task("page", "restore-progress-copy", {
            "page": "restore-progress-copy", "mode": "create",
            "sources": [engine.source_snapshot(drift_key)],
        })
    except wiki.WikiError as error:
        conflict = error
    engine.close()
    check(ctx.results, "a second task tree for the same source is refused",
          conflict is not None and conflict.code == "E_TASK_CONFLICT", conflict)
    check(ctx.results, "the conflict names the task that already holds the source",
          conflict is not None and drift_task["id"] in conflict.message,
          getattr(conflict, "message", conflict))

    # 领取侧失效：放回队列后改源、重新 ingest，next 不能再把它发出来。
    run(ctx.root, "release", drift_task["id"])
    (ctx.root / "f.md").write_text("# 复机进度查询\n\n拨打 10086 可查进度。\n", encoding="utf-8")
    run(ctx.root, "ingest")
    after = run(ctx.root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"]
    check(ctx.results, "next never hands out a task whose source moved", after == [], after)
    conn = sqlite3.connect(ctx.root / ".wiki" / "wiki.db")
    expired = conn.execute(
        "SELECT state, last_error, closed_at FROM tasks WHERE id = ?", (drift_task["id"],)
    ).fetchone()
    conn.close()
    check(ctx.results, "an invalidated task keeps its row, reason and close time",
          expired is not None and expired[0] == "expired"
          and expired[1] and expired[2], expired)

    # 失效不是放弃：过期之后来源必须还能重新变成工作项，否则 next 永远空、
    # status 永远提示 "run ingest"，整个库没有任何出口。
    requeued = run(ctx.root, "ingest")["data"]
    conn = sqlite3.connect(ctx.root / ".wiki" / "wiki.db")
    reopened = conn.execute(
        "SELECT COUNT(*) FROM task_sources ts JOIN tasks t ON t.id = ts.task_id "
        "WHERE ts.source_key = ? AND t.state = 'queued'",
        (drift_key,),
    ).fetchone()[0]
    conn.close()
    check(ctx.results, "an expired source is queued again on the next ingest",
          requeued["queued_tasks"] != [] and reopened == 1, (requeued, reopened))
    claimed = run(ctx.root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"]
    check(ctx.results, "the requeued source can be claimed again",
          len(claimed) == 1, claimed)

    # 提交侧失效：任务已被判 expired，submit 仍要报 E_SOURCE_DRIFT，且不能复活它。
    body_file(ctx.root)
    drifted = run(ctx.root, "submit", drift_task["id"], "--payload", payload_file(
        ctx.root, "drift-page.json",
        {**ctx.payload, "page": "restore-progress", "title": "复机进度查询",
         "summary": "可拨打客服确认进度。",
         "evidence": [{"source": drift_key, "quote": "可拨打客服确认进度。"}]}), expect=3)
    check(ctx.results, "a source change after the page task still reports E_SOURCE_DRIFT",
          drifted["error"]["code"] == "E_SOURCE_DRIFT", drifted)
    conn = sqlite3.connect(ctx.root / ".wiki" / "wiki.db")
    revived = conn.execute(
        "SELECT state FROM tasks WHERE id = ?", (drift_task["id"],)
    ).fetchone()[0]
    conn.close()
    check(ctx.results, "a failed submit does not revive the expired task", revived == "expired", revived)

    # --- Task 5: 只有被引用的原文会判 stale -------------------------------
    conn = sqlite3.connect(ctx.root / ".wiki" / "wiki.db")
    raw_d = conn.execute(
        "SELECT raw FROM sources WHERE key = 'd.md'"
    ).fetchone()[0]
    conn.close()
    before = run(ctx.root, "query", "--page", "machine-room-inspection")["data"]["page"]
    check(ctx.results, "the page is stable before its raw copy changes",
          before["status"] == "stable", before)
    raw_text = (ctx.root / raw_d).read_text(encoding="utf-8")
    raw_link = re.search(r"- \[`d.md`\]\(([^)]+)\)", raw_text)
    check(ctx.results, "the raw copy links to its source",
          "<!-- llm-wiki:provenance -->" in raw_text
          and "## 原文来源" in raw_text
          and raw_link is not None
          and ((ctx.root / raw_d).parent / raw_link.group(1)).resolve().is_file(),
          raw_text)
    (ctx.root / raw_d).write_text(
        raw_text.split("\n<!-- llm-wiki:provenance -->")[0] + "\n",
        encoding="utf-8",
    )
    run(ctx.root, "ingest")
    repaired_raw = (ctx.root / raw_d).read_text(encoding="utf-8")
    check(ctx.results, "ingest repairs the provenance footer on an old raw copy",
          "<!-- llm-wiki:provenance -->" in repaired_raw
          and "[`d.md`]" in repaired_raw, repaired_raw)
    (ctx.root / raw_d).write_text(
        (ctx.root / raw_d).read_text(encoding="utf-8") + "\n巡检记录留档。\n",
        encoding="utf-8",
    )
    run(ctx.root, "ingest")
    stale = run(ctx.root, "query", "--page", "machine-room-inspection")["data"]["page"]
    check(ctx.results, "editing a cited raw copy makes the page stale",
          stale["status"] == "stale", stale)
    untouched = run(ctx.root, "query", "--page", "anti-fraud-restore")["data"]["page"]
    check(ctx.results, "editing an unrelated raw copy leaves the page stable",
          untouched["status"] == "stable", untouched)


def split_helpers(ctx):
    check(ctx.results, "split_evidence leaves a plain body alone",
          wiki.split_evidence("## 要点\n\n正文。\n") == ("## 要点\n\n正文。\n", []))
    check(ctx.results, "split_evidence parses the trailing block",
          wiki.split_evidence("## 要点\n\n正文。\n\n## 证据\n\n- `a.md`：引文。\n")
          == ("## 要点\n\n正文。", [{"source": "a.md", "quote": "引文。"}]))
    check(ctx.results, "split_evidence keeps a broken block in the body",
          wiki.split_evidence("## 要点\n\n正文。\n\n## 证据\n\n随手写的。\n")
          == ("## 要点\n\n正文。\n\n## 证据\n\n随手写的。\n", []))
    check(ctx.results, "split_evidence parses rendered evidence links",
          wiki.split_evidence(
              "## 要点\n\n正文。\n\n## 证据\n\n"
              "- [`wiki/raw/a.md`](../../raw/a.md)（`a.md`）：引文。\n"
          ) == ("## 要点\n\n正文。", [{"source": "a.md", "quote": "引文。"}]))
    provenance_body = (
        "正文。\n\n<!-- llm-wiki:provenance -->\n\n## 原文来源\n\n"
        "- [`files/a.pdf`](../../files/a.pdf)\n"
    )
    check(ctx.results, "split_provenance strips generated source links",
          split_provenance(provenance_body)
          == ("正文。", [{"source": "files/a.pdf", "target": "../../files/a.pdf"}]))
    hand_written = "正文。\n\n## 原文来源\n\n手写。\n"
    check(ctx.results, "split_provenance leaves hand-written sections alone",
          split_provenance(hand_written) == (hand_written, []))
