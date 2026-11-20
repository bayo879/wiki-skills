"""Ledger bootstrap, workspace health and repair checks.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import os
import shutil
import sqlite3
import tempfile
from pathlib import Path

from support import check, payload_file, route_query, run


def repair_flow(ctx):
    """Repair missing edges from pages that cite the same raw source."""
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-repair-"))
    try:
        (root / "shared.md").write_text(
            "# 共享来源\n\n同一份来源支撑甲、乙两页。\n", encoding="utf-8"
        )
        run(root, "ingest")
        route = run(root, "next", "--kind", "route")["data"]["tasks"][0]
        route_query(root, route, "共享")
        key = route["work"]["sources"][0]["key"]
        run(root, "submit", route["id"], "--payload", payload_file(
            root, "repair-route.json",
            {"decisions": [
                {"source": key, "action": "new", "page": "page-a",
                 "title": "甲页", "type": "concept", "reason": "甲"},
                {"source": key, "action": "new", "page": "page-b",
                 "title": "乙页", "type": "concept", "reason": "乙"},
            ]},
        ))
        tasks = run(root, "next", "--kind", "page", "--batch", "2")["data"]["tasks"]
        check(ctx.results, "repair fixture claims both pages", len(tasks) == 2, tasks)
        for task in tasks:
            page = task["template"]["page"]
            title = task["template"]["title"]
            draft = root / ".wiki/drafts" / f"{page}.md"
            draft.parent.mkdir(parents=True, exist_ok=True)
            sentence = f"{title}的正文。"
            quote = "同一份来源支撑甲、乙两页。"
            draft.write_text(
                f"## 结论\n\n{sentence}\n\n## 要点\n\n- {sentence}\n",
                encoding="utf-8",
            )
            run(root, "submit", task["id"], "--payload", payload_file(
                root, f"{page}-repair.json",
                {"page": page, "title": title, "type": "concept", "summary": sentence,
                 "links": [], "body_file": str(draft.relative_to(root)),
                 "evidence": [{"source": key, "quote": quote}],
                 "boundaries": {"covered": title, "not_covered": "其他"}},
            ))

        before = run(root, "query", "--page", "page-a")["data"]
        check(ctx.results, "repair fixture starts without edges",
              before["outgoing"] == [] and before["incoming"] == [], before)
        dry = run(root, "page", "link", "--repair", "--dry-run")["data"]
        check(ctx.results, "link repair previews shared-source edges",
              dry["dry_run"] is True and dry["edges"] == 1
              and dry["pages"][0]["page"] == "page-a"
              and dry["pages"][0]["added"] == ["page-b"], dry)
        still = run(root, "query", "--page", "page-a")["data"]
        check(ctx.results, "a repair preview writes no revision",
              still["rev"] == before["rev"] and still["outgoing"] == [], still)

        applied = run(root, "page", "link", "--repair")["data"]
        check(ctx.results, "link repair applies shared-source edges",
              applied["edges"] == 1 and applied["pages"][0]["page"] == "page-a"
              and applied["pages"][0]["added"] == ["page-b"], applied)
        page_a = run(root, "query", "--page", "page-a")["data"]
        page_b = run(root, "query", "--page", "page-b")["data"]
        check(ctx.results, "repaired edges point from the first page to the second",
              page_a["outgoing"] == ["page-b"] and page_b["incoming"] == ["page-a"],
              (page_a["outgoing"], page_b["incoming"]))
        view_a = (root / page_a["path"]).read_text(encoding="utf-8")
        view_b = (root / page_b["path"]).read_text(encoding="utf-8")
        check(ctx.results, "a repaired edge renders on both sides of the view",
              "[乙页](乙页.md)" in view_a and "[甲页](甲页.md)" in view_b, (view_a, view_b))

        again = run(root, "page", "link", "--repair")["data"]
        check(ctx.results, "link repair is idempotent",
              again["edges"] == 0 and again["pages"] == [], again)
        bad = run(root, "page", "link", "--repair", "page-a", expect=2)
        check(ctx.results, "link repair rejects positional page ids",
              bad["error"]["code"] == "E_USAGE", bad)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def workspace_and_init(ctx):
    data = run(ctx.root, "check")["data"]
    check(ctx.results, "check passes in a workspace with no ledger", data["ok"], data)

    # 不需要 init：第一条打开账本的命令自己建库，并把补出来的路径列在 repaired
    fresh = Path(tempfile.mkdtemp(prefix="llm-wiki-fresh-"))
    try:
        (fresh / "n.md").write_text("# 新库\n\n直接摄入。\n", encoding="utf-8")
        born = run(fresh, "ingest")
        check(ctx.results, "ingest works without init and reports what it repaired",
              born["ok"] and (fresh / ".wiki/wiki.db").exists() and born["data"].get("repaired"), born)
    finally:
        shutil.rmtree(fresh, ignore_errors=True)

    # 只有配置没有库：普通命令当场建库，并把补出来的库文件列进 repaired
    half = Path(tempfile.mkdtemp(prefix="llm-wiki-half-"))
    try:
        (half / ".wiki").mkdir()
        (half / ".wiki/wiki.json").write_text("{}", encoding="utf-8")
        (half / "h.md").write_text("# 半初始化\n\n直接摄入。\n", encoding="utf-8")
        grew = run(half, "ingest")
        check(ctx.results, "ingest fills a half-initialized workspace",
              grew["ok"] and (half / ".wiki/wiki.db").exists()
              and ".wiki/wiki.db" in grew["data"].get("repaired", []), grew)
    finally:
        shutil.rmtree(half, ignore_errors=True)

    # 工作区不可写：E_HEALTH + 手册小节，不能是 traceback
    locked = Path(tempfile.mkdtemp(prefix="llm-wiki-ro-"))
    try:
        locked.chmod(0o555)
        if not os.access(locked, os.W_OK):
            denied = run(locked, "status", expect=1)
            error = denied.get("error", {})
            check(ctx.results, "an unwritable workspace reports E_HEALTH with the manual section",
                  error.get("code") == "E_HEALTH"
                  and error.get("section") == "工作区不可写"
                  and error.get("doc", "").endswith("/references/recovery.md")
                  and "check" not in error.get("next", ""), denied)
    finally:
        locked.chmod(0o755)
        shutil.rmtree(locked, ignore_errors=True)

    def legacy_ledger(rows):
        legacy = ctx.root / ".wiki"
        legacy.mkdir(exist_ok=True)
        conn = sqlite3.connect(legacy / "wiki.db")
        conn.executescript(
            "DROP TABLE IF EXISTS sources;"
            "CREATE TABLE sources (key TEXT PRIMARY KEY, path TEXT, digest TEXT, size INTEGER,"
            " mtime REAL, raw TEXT, parser TEXT, status TEXT, updated_at TEXT);"
        )
        conn.executemany("INSERT INTO sources (key) VALUES (?)", [(row,) for row in rows])
        conn.commit()
        conn.close()
        return legacy

    # 旧结构但没有数据：按当前结构重建，不算迁移
    legacy_ledger([])
    rebuilt = run(ctx.root, "status")
    columns = [row[1] for row in sqlite3.connect(ctx.root / ".wiki/wiki.db").execute("PRAGMA table_info(sources)")]
    check(ctx.results, "an empty old schema is rebuilt in place",
          rebuilt["ok"] and "raw_digest" in columns, {"run": rebuilt, "columns": columns})

    # 旧结构里有数据：拒绝继续，并指向恢复手册
    legacy_ledger(["old.md"])
    broken = run(ctx.root, "status", expect=3)
    error = broken.get("error", {})
    check(ctx.results, "rows in an old schema report E_STATE with the manual section",
          error.get("code") == "E_STATE" and error.get("section") == "旧账本"
          and error.get("doc", "").endswith("/references/recovery.md"), broken)

    # 账本文件本身坏了：E_DB，不要当成旧结构
    (ctx.root / ".wiki/wiki.db").write_bytes(b"not a database")
    corrupt = run(ctx.root, "status", expect=3)
    error = corrupt.get("error", {})
    check(ctx.results, "a corrupt ledger reports E_DB with the manual section",
          error.get("code") == "E_DB" and error.get("section") == "账本损坏", corrupt)

    shutil.rmtree(ctx.root / ".wiki")
    shutil.rmtree(ctx.root / "wiki", ignore_errors=True)  # the probe touched the view too

    data = run(ctx.root, "init")["data"]
    check(ctx.results, "init creates .wiki", (ctx.root / ".wiki/wiki.db").exists() and data["sources"] == 0, data)
    check(ctx.results, "init creates the publish directory",
          (ctx.root / "wiki").is_dir() and not (ctx.root / ".wiki/raw").exists(), data)
    check(ctx.results, "init creates workflow/topic content dirs",
          (ctx.root / "wiki/workflows").is_dir() and (ctx.root / "wiki/topics").is_dir())
    check(ctx.results, "publish README is written", (ctx.root / "wiki/README.md").exists())

    data = run(ctx.root, "check")["data"]
    check(ctx.results, "check passes after init", data["ok"], data)

    escaped = run(ctx.root, "status", "--root", str(ctx.root.parent), expect=2)
    check(ctx.results, "root outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)
