"""Page drafting and submission flow: route, skip, update, view push.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import json
import sqlite3

from support import body_file, check, payload_file, route_query, run

# 引擎模块要等 support 把脚本目录加进 sys.path，因此排在 support 之后。
import wiki


def page_flow(ctx):
    tasks = run(ctx.root, "next", "--kind", "route", "--batch", "5")["data"]["tasks"]
    check(ctx.results, "route claims one task per source", len(tasks) == 2, tasks)
    check(ctx.results, "next returns brief and template", all(t.get("brief") and t.get("template") for t in tasks), tasks)
    check(ctx.results, "next hides task internals",
          not any(key in tasks[0] for key in ("epoch", "state", "inputs", "lease_until")), tasks[0])
    for task in tasks:
        route_query(ctx.root, task, "复机")
    routes = {task["work"]["sources"][0]["key"]: task for task in tasks}
    check(ctx.results, "route template pre-fills the source",
          routes["a.md"]["template"]["decisions"][0]["source"] == "a.md", routes["a.md"]["template"])
    source_key = routes["a.md"]["work"]["sources"][0]["key"]
    raw_rel = routes["a.md"]["work"]["sources"][0]["raw"]
    raw_meta, raw_body = wiki.split_frontmatter(
        (ctx.root / raw_rel).read_text(encoding="utf-8")
    )
    check(ctx.results, "the view tree carries the parsed raw text",
          "复机" in raw_body and raw_rel.startswith("wiki/raw/"), raw_rel)
    check(ctx.results, "raw frontmatter names the raw object and its mounts",
          raw_meta.get("type") == "raw"
          and "a.md" in json.dumps(raw_meta, ensure_ascii=False), raw_meta)

    front_meta, front_body = wiki.split_frontmatter(
        "---\ntitle: 手工标题\n---\n\n# 正文\n\n内容。\n"
    )
    check(ctx.results, "frontmatter split keys on the first line, not on .wiki",
          front_meta.get("title") == "手工标题" and "title: 手工标题" not in front_body,
          front_meta)

    outside_payload = ctx.root.parent / f"{ctx.root.name}-payload.json"
    outside_payload.write_text("{}", encoding="utf-8")
    escaped = run(
        ctx.root, "submit", routes["b.md"]["id"],
        "--payload", str(outside_payload), expect=2,
    )
    check(ctx.results, "payload outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)
    outside_payload.unlink()
    merged = run(
        ctx.root, "submit", routes["b.md"]["id"],
        "--payload", payload_file(ctx.root, "skip.json",
                                  {"decisions": [{"source": "b.md", "action": "skip",
                                                  "reason": "与主题无关"}]}),
    )
    check(ctx.results, "skip decision accepted", merged["ok"], merged)

    submitted = run(
        ctx.root, "submit", routes["a.md"]["id"],
        "--payload", payload_file(ctx.root, "route.json", {"decisions": [
            {"source": source_key, "action": "new", "page": "anti-fraud-restore",
             "title": "反诈复机", "type": "process", "reason": "完整的复机办理路径"},
        ]}),
    )
    check(ctx.results, "route new queues a page task", submitted["data"]["queued_page_tasks"], submitted)

    body_file(ctx.root)
    task = run(ctx.root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
    check(ctx.results, "page template fixes the section skeleton",
          "## 结论" in task["template"]["body"] and "## 步骤" in task["template"]["body"], task["template"])
    ctx.payload = {
        "page": "anti-fraud-restore",
        "title": "反诈复机",
        "type": "process",
        "summary": "停机后本人持证到厅复机，约两个工作日。",
        "links": [],
        "body_file": ".wiki/drafts/page.md",
        "evidence": [{"source": source_key, "quote": "需要本人持身份证到营业厅办理复机"}],
        "boundaries": {"covered": "停机后的复机路径", "not_covered": "异地办理与资费"},
    }
    outside_body = ctx.root.parent / f"{ctx.root.name}-body.md"
    outside_body.write_text("## 区外草稿\n", encoding="utf-8")
    escaped = run(
        ctx.root, "submit", task["id"],
        "--payload", payload_file(ctx.root, "body-escape.json",
                                  {**ctx.payload, "body_file": str(outside_body)}),
        expect=2,
    )
    check(ctx.results, "body_file outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)
    outside_body.unlink()
    task = run(ctx.root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
    accepted = run(ctx.root, "submit", task["id"],
                   "--payload", payload_file(ctx.root, "page.json", ctx.payload))
    check(ctx.results, "page submit writes rev 1", accepted["data"]["rev"] == 1, accepted)
    # 镜像已删除：页面正文只存在账本与观看区两处。
    check(ctx.results, "page submit writes no mirror copy",
          not (ctx.root / ".wiki/pages").exists(), ctx.root / ".wiki/pages")

    # Task 5: 观看区文件名与别名只从标题生成，先把纯函数钉住。
    check(ctx.results, "view_name strips path separators",
          wiki.view_name("复机/办理:2026", "p-x") == "复机-办理-2026",
          wiki.view_name("复机/办理:2026", "p-x"))
    check(ctx.results, "view_name collapses whitespace",
          wiki.view_name("复机\u3000 办理", "p-x") == "复机 办理",
          wiki.view_name("复机\u3000 办理", "p-x"))
    check(ctx.results, "view_name truncates long titles",
          len(wiki.view_name("复" * 200, "p-x")) == 60,
          len(wiki.view_name("复" * 200, "p-x")))
    check(ctx.results, "view_name falls back to the id when only separators remain",
          wiki.view_name("///", "p-x") == "p-x",
          wiki.view_name("///", "p-x"))
    check(ctx.results, "view_name falls back to the id",
          wiki.view_name("  ..  ", "p-x") == "p-x",
          wiki.view_name("  ..  ", "p-x"))

    # Task 5: 观看区事实只落在账本里；这里不调用 ensure()，不碰观看区文件（写文件是 Task 6）。
    engine = wiki.Wiki(ctx.root)
    engine.config = json.loads((ctx.root / ".wiki/wiki.json").read_text(encoding="utf-8"))
    engine.conn = sqlite3.connect(ctx.root / ".wiki/wiki.db")
    engine.conn.row_factory = sqlite3.Row
    record = engine.page_record("anti-fraud-restore")
    published = engine.view_text(record)
    check(ctx.results, "view path follows the chinese title",
          engine.view_relpath(record) == "pages/process/反诈复机.md",
          engine.view_relpath(record))
    check(ctx.results, "view text aliases the id right after the title",
          f"title: {wiki.yaml_scalar(record['meta']['title'])}\n"
          f"aliases: [anti-fraud-restore]" in published,
          published)
    # R1（Task 6 取代 Task 5 的 R5）：view_digest/view_path 是"上一次发布出去的事实"，
    # 不是本行文本的摘要；必须先写盘、成功之后才回写事实，所以新页两列为空。
    stored = engine.latest_rev_row("anti-fraud-restore")
    check(ctx.results, "a fresh revision has no published facts yet",
          stored["view_path"] == "" and stored["view_digest"] == "",
          (stored["view_path"], stored["view_digest"]))
    check(ctx.results, "the ledger render stays alias-free",
          "aliases:" not in wiki.render_page(record["meta"], record["body"]),
          wiki.render_page(record["meta"], record["body"]))
    engine.close()

    bad = run(ctx.root, "submit", task["id"],
              "--payload", payload_file(ctx.root, "again.json", ctx.payload), expect=3)
    check(ctx.results, "closed task rejects resubmit", bad["error"]["code"] == "E_TASK_STALE", bad)

    status = run(ctx.root, "status")["data"]
    check(ctx.results, "status counts page and pending", status["pages"] == 1, status)
    found = run(ctx.root, "query", "复机")["data"]
    check(ctx.results, "query finds the page", found["matches"] == 1, found)
    page = run(ctx.root, "query", "--page", "anti-fraud-restore")["data"]
    check(ctx.results, "query --page reads the ledger", "两个工作日" in page["body"], page)
    check(ctx.results, "query --page hides source digests", "source_digests" not in page["page"], page["page"])
    raw_paths = {item["key"]: item["raw"] for item in page["sources"]}
    check(ctx.results, "query --page renders the raw path of every source",
          bool(raw_paths) and all(
              raw and (ctx.root / raw).is_file() for raw in raw_paths.values()),
          page["sources"])

    (ctx.root / "c.md").write_text("# 反诈复机\n\n补充：可拨打客服确认进度。\n", encoding="utf-8")
    run(ctx.root, "ingest")
    route = run(ctx.root, "next", "--kind", "route")["data"]["tasks"][0]
    route_query(ctx.root, route, "复机")
    ctx.c_key = route["work"]["sources"][0]["key"]
    check(ctx.results, "route brief requires reason for every decision",
          "每条 decision 都必须写 reason" in " ".join(route["brief"]["steps"]), route["brief"])
    run(ctx.root, "submit", route["id"], "--payload", payload_file(
        ctx.root, "update-route.json",
        {"decisions": [{"source": ctx.c_key, "action": "merge", "page": "anti-fraud-restore",
                        "reason": "补充客服确认方式"}]},
    ))
    task = run(ctx.root, "next")["data"]["tasks"][0]
    check(ctx.results, "update template keeps old evidence",
          any(item.get("source") == "a.md" and item.get("quote")
              for item in task["template"]["evidence"]), task["template"]["evidence"])
    update_body = (
        "## 结论\n\n停机后本人持证到厅复机，约两个工作日。\n\n"
        "## 步骤\n\n本人带身份证到营业厅提交复机申请。\n\n"
        "## 注意事项\n\n可拨打客服确认进度。\n"
    )
    update_payload = {
        "page": "anti-fraud-restore", "title": "反诈复机", "type": "process",
        "summary": "停机后本人持证到厅复机，约两个工作日；可拨打客服确认进度。",
        "links": [], "body": update_body,
        "evidence": [{"source": ctx.c_key, "quote": "可拨打客服确认进度"}],
        "boundaries": {"covered": "停机后的复机路径", "not_covered": "异地办理与资费"},
    }
    run(ctx.root, "submit", task["id"], "--payload", payload_file(ctx.root, "update-page.json", update_payload))

    # Task 6: 推送观看区（pages / raw / topics / workflows / README）----
    page_file = ctx.root / "wiki/pages/process/反诈复机.md"
    check(ctx.results, "the first revision was published with its own text",
          page_file.exists()
          and update_body.strip() not in page_file.read_text(encoding="utf-8"))

    conn = sqlite3.connect(ctx.root / ".wiki" / "wiki.db")
    carried = conn.execute(
        "SELECT rev, view_path, view_digest FROM page_revs "
        "WHERE page = 'anti-fraud-restore' ORDER BY rev DESC LIMIT 1"
    ).fetchone()
    conn.close()
    check(ctx.results, "a new revision carries the previous published facts",
          carried[0] == 2
          and carried[1] == "pages/process/反诈复机.md"
          and carried[2] == wiki.digest_text(page_file.read_text(encoding="utf-8")),
          carried)

    run(ctx.root, "status")  # ensure() → sync_view()：推送 rev 2
    published_text = page_file.read_text(encoding="utf-8")
    check(ctx.results, "sync_view publishes the newest revision",
          update_body.strip() in published_text)
    conn = sqlite3.connect(ctx.root / ".wiki" / "wiki.db")
    facts = conn.execute(
        "SELECT view_path, view_digest FROM page_revs "
        "WHERE page = 'anti-fraud-restore' ORDER BY rev DESC LIMIT 1"
    ).fetchone()
    conn.close()
    clean_digest = wiki.digest_text(published_text)
    check(ctx.results, "published facts describe the file on disk",
          facts[0] == "pages/process/反诈复机.md" and facts[1] == clean_digest,
          (facts[0], facts[1], clean_digest))

    (ctx.root / "wiki/笔记.md").write_text("我自己的笔记\n", encoding="utf-8")
    (ctx.root / "wiki/.obsidian").mkdir(exist_ok=True)
    (ctx.root / "wiki/.obsidian/app.json").write_text("{}\n", encoding="utf-8")
    (ctx.root / "wiki/raw/用户笔记.md").write_text("# 我的原文笔记\n", encoding="utf-8")

    page_file.unlink()
    run(ctx.root, "status")
    check(ctx.results, "a deleted published page is restored",
          page_file.exists() and update_body.strip() in page_file.read_text(encoding="utf-8"))

    page_file.write_text(published_text + "\n手改一段。\n", encoding="utf-8")
    hand_digest = wiki.digest_text(page_file.read_text(encoding="utf-8"))
    run(ctx.root, "status")
    check(ctx.results, "a hand edited published page is not overwritten",
          "手改一段。" in page_file.read_text(encoding="utf-8"))
    conn = sqlite3.connect(ctx.root / ".wiki" / "wiki.db")
    after_hand = conn.execute(
        "SELECT view_digest FROM page_revs WHERE page = 'anti-fraud-restore' "
        "ORDER BY rev DESC LIMIT 1"
    ).fetchone()[0]
    conn.close()
    check(ctx.results, "a hand edit leaves the published facts untouched",
          after_hand == clean_digest and after_hand != hand_digest,
          (after_hand, clean_digest, hand_digest))

    check(ctx.results, "user files in the view dir are untouched",
          (ctx.root / "wiki/笔记.md").read_text(encoding="utf-8") == "我自己的笔记\n"
          and (ctx.root / "wiki/.obsidian/app.json").exists()
          and (ctx.root / "wiki/raw/用户笔记.md").exists())

    readme = (ctx.root / "wiki/README.md").read_text(encoding="utf-8")
    check(ctx.results, "view readme lists directories and ownership",
          all(name in readme for name in ("pages/", "raw/", "workflows/", "topics/", "runs/"))
          and "由引擎维护" in readme and "项目内容" in readme
          and "首次投放" not in readme
          and "工作流副本" not in readme and "话题副本" not in readme,
          readme)
    (ctx.root / "wiki/README.md").unlink()
    run(ctx.root, "status")
    check(ctx.results, "view readme is rewritten on every push",
          (ctx.root / "wiki/README.md").exists())

    check(ctx.results, "view content dirs exist",
          (ctx.root / "wiki/workflows").is_dir() and (ctx.root / "wiki/topics").is_dir())
    check(ctx.results, "view does not seed root workflows",
          not (ctx.root / "wiki/workflows/account-opening").exists())
    check(ctx.results, "view does not seed root topics",
          not (ctx.root / "wiki/topics/复机办理.md").exists())

    page_file.unlink()  # 手改件留在树上会持续告警，也会干扰后面的漂移检查
    run(ctx.root, "status")
    check(ctx.results, "the hand edit is cleaned up for later blocks",
          page_file.exists() and "手改一段。" not in page_file.read_text(encoding="utf-8"))
