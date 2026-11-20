"""Raw mounting, orphan links and raw file editing checks.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import shutil
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

from support import SCRIPT, check, payload_file, route_query, run

# 引擎模块要等 support 把脚本目录加进 sys.path，因此排在 support 之后。
import wiki


def raw_mounts(ctx):
    raw_root = Path(tempfile.mkdtemp(prefix="llm-wiki-raw-"))
    try:
        shared = "# 共享正文\n\n同一份内容，两个路径。\n"
        (raw_root / "a.md").write_text(shared, encoding="utf-8")
        (raw_root / "copy").mkdir()
        (raw_root / "copy" / "a.md").write_text(shared, encoding="utf-8")
        run(raw_root, "ingest")
        raw_files = sorted(p.name for p in (raw_root / "wiki/raw").glob("*.md"))
        conn = sqlite3.connect(raw_root / ".wiki" / "wiki.db")
        raws = conn.execute("SELECT id, name, path FROM raws").fetchall()
        mounted = conn.execute("SELECT key, raw_id FROM sources ORDER BY key").fetchall()
        conn.close()
        check(ctx.results, "two sources with one body share one raw",
              len(raw_files) == 1 and len(raws) == 1
              and {row[1] for row in mounted} == {raws[0][0]},
              (raw_files, raws, mounted))
        again = run(raw_root, "ingest")["data"]
        check(ctx.results, "a shared raw is not parsed twice",
              again["ingested"] == []
              and len(list((raw_root / "wiki/raw").glob("*.md"))) == 1,
              again["ingested"])

        # 一份正文一条任务：subject 是 raw id，任务带该正文的全部挂载路径。
        raw_id = raws[0][0]
        conn = sqlite3.connect(raw_root / ".wiki" / "wiki.db")
        route_rows = conn.execute(
            "SELECT id, subject FROM tasks WHERE kind = 'route'"
        ).fetchall()
        mounts = conn.execute(
            "SELECT source_key FROM task_sources WHERE task_id = ? ORDER BY source_key",
            (route_rows[0][0],),
        ).fetchall()
        conn.close()
        check(ctx.results, "one raw queues one route task covering every mount",
              len(route_rows) == 1 and route_rows[0][1] == raw_id
              and [row[0] for row in mounts] == ["a.md", "copy/a.md"],
              (route_rows, mounts))

        # 同一正文再挂一个路径：不算漂移，直接并进已排队的任务。
        (raw_root / "copy" / "b.md").write_text(shared, encoding="utf-8")
        mounted = run(raw_root, "ingest")["data"]
        conn = sqlite3.connect(raw_root / ".wiki" / "wiki.db")
        task_count = conn.execute("SELECT COUNT(*) FROM tasks WHERE kind = 'route'").fetchone()[0]
        mounts = conn.execute(
            "SELECT source_key FROM task_sources WHERE task_id = ? ORDER BY source_key",
            (route_rows[0][0],),
        ).fetchall()
        digests = conn.execute("SELECT raw_digest FROM sources").fetchall()
        conn.close()
        check(ctx.results, "a new mount joins the open task without drift",
              not mounted["queued_tasks"] and task_count == 1
              and [row[0] for row in mounts] == ["a.md", "copy/a.md", "copy/b.md"]
              and {row[0] for row in digests} == {raw_id},
              (mounted, task_count, mounts, digests))

        gone = subprocess.run(
            [sys.executable, str(SCRIPT), "read", "a.md", "--json"],
            cwd=raw_root, capture_output=True, text=True,
        )
        check(ctx.results, "the read command is gone",
              gone.returncode == 2 and "invalid choice" in gone.stderr,
              gone.stderr[-200:])

    finally:
        shutil.rmtree(raw_root, ignore_errors=True)

    # 孤儿页：来源全没了、又没有出入链，ingest 末尾连同观看区文件一起删。
    orphan_root = Path(tempfile.mkdtemp(prefix="llm-wiki-orphan-"))
    try:
        (orphan_root / "a.md").write_text("# 孤儿来源\n\n唯一正文。\n", encoding="utf-8")
        run(orphan_root, "ingest")
        route_task = run(orphan_root, "next", "--kind", "route")["data"]["tasks"][0]
        route_query(orphan_root, route_task, "孤儿")
        run(orphan_root, "submit", route_task["id"], "--payload", payload_file(
            orphan_root, "orphan-route.json",
            {"decisions": [{"source": "a.md", "action": "new", "page": "orphan-page",
                            "title": "孤儿页", "type": "concept", "reason": "唯一来源"}]},
        ))
        page_task = run(orphan_root, "next", "--kind", "page")["data"]["tasks"][0]
        draft = orphan_root / ".wiki/drafts/orphan.md"
        draft.parent.mkdir(parents=True, exist_ok=True)
        draft.write_text("## 结论\n\n唯一正文。\n\n## 要点\n\n- 唯一来源。\n",
                         encoding="utf-8")
        run(orphan_root, "submit", page_task["id"], "--payload", payload_file(
            orphan_root, "orphan-page.json",
            {"page": "orphan-page", "title": "孤儿页", "type": "concept",
             "summary": "一句话摘要。", "links": [],
             "body_file": ".wiki/drafts/orphan.md",
             "evidence": [{"source": "a.md", "quote": "唯一正文。"}],
             "boundaries": {"covered": "这一页", "not_covered": "其他"}},
        ))
        page_view = orphan_root / "wiki/pages/concept/孤儿页.md"
        (orphan_root / "a.md").unlink()
        cleaned = run(orphan_root, "ingest")["data"]
        check(ctx.results, "a page whose sources are all gone is removed at ingest",
              cleaned["orphaned"] == ["orphan-page"] and not page_view.exists(),
              (cleaned, page_view.exists()))
    finally:
        shutil.rmtree(orphan_root, ignore_errors=True)


def orphan_and_merge(ctx):
    linked_root = Path(tempfile.mkdtemp(prefix="llm-wiki-orphan-link-"))
    try:
        (linked_root / "a.md").write_text("# 甲\n\n甲正文。\n", encoding="utf-8")
        (linked_root / "b.md").write_text("# 乙\n\n乙正文。\n", encoding="utf-8")
        run(linked_root, "ingest")
        task_a = run(linked_root, "next", "--kind", "route")["data"]["tasks"][0]
        route_query(linked_root, task_a, "正文")
        run(linked_root, "submit", task_a["id"], "--payload", payload_file(
            linked_root, "link-a.json",
            {"decisions": [{"source": "a.md", "action": "new", "page": "page-a",
                            "title": "甲页", "type": "concept", "reason": "甲"}]},
        ))

        def submit_linked(page_id, key, quote, links):
            page_task = run(linked_root, "next", "--kind", "page")["data"]["tasks"][0]
            draft = linked_root / ".wiki/drafts" / f"{page_id}.md"
            draft.parent.mkdir(parents=True, exist_ok=True)
            draft.write_text(f"## 结论\n\n{quote}\n\n## 要点\n\n- {quote}\n",
                             encoding="utf-8")
            run(linked_root, "submit", page_task["id"], "--payload", payload_file(
                linked_root, f"{page_id}.json",
                {"page": page_id, "title": "甲页" if page_id == "page-a" else "乙页",
                 "type": "concept", "summary": "一句话摘要。", "links": links,
                 "body_file": f".wiki/drafts/{page_id}.md",
                 "evidence": [{"source": key, "quote": quote}],
                 "boundaries": {"covered": "这一页", "not_covered": "其他"}},
            ))

        # 甲先建页，乙再链过去：链接只能指向提交时已存在的页。
        submit_linked("page-a", "a.md", "甲正文。", [])
        task_b = run(linked_root, "next", "--kind", "route")["data"]["tasks"][0]
        route_query(linked_root, task_b, "正文")
        run(linked_root, "submit", task_b["id"], "--payload", payload_file(
            linked_root, "link-b.json",
            {"decisions": [{"source": "b.md", "action": "new", "page": "page-b",
                            "title": "乙页", "type": "concept", "reason": "乙",
                            "links": ["page-a"]}]},
        ))
        submit_linked("page-b", "b.md", "乙正文。", ["page-a"])
        (linked_root / "a.md").unlink()
        (linked_root / "b.md").unlink()
        kept = run(linked_root, "ingest")["data"]
        status = run(linked_root, "status")["data"]
        check(ctx.results, "a linked orphan is only marked stale",
              kept["orphaned"] == [] and status["pages"] == 2
              and set(status["stale_pages"]) == {"page-a", "page-b"},
              (kept, status))
    finally:
        shutil.rmtree(linked_root, ignore_errors=True)

    # 人工排版过的原文：账本换行但不回源覆盖，源本身变了才覆盖。
    edit_root = Path(tempfile.mkdtemp(prefix="llm-wiki-raw-edit-"))
    try:
        (edit_root / "a.md").write_text("# 手改\n\n原始正文。\n", encoding="utf-8")
        run(edit_root, "ingest")
        conn = sqlite3.connect(edit_root / ".wiki" / "wiki.db")
        before = conn.execute(
            "SELECT raw, raw_id, last_parsed_digest FROM sources"
        ).fetchone()
        conn.close()
        raw_path = edit_root / before[0]
        parsed = before[2]
        raw_path.write_text(
            raw_path.read_text(encoding="utf-8").rstrip("\n") + "\n\n用户手改的一句。\n",
            encoding="utf-8",
        )
        adopted = run(edit_root, "ingest")["data"]
        conn = sqlite3.connect(edit_root / ".wiki" / "wiki.db")
        raw_rows = conn.execute("SELECT id, path FROM raws").fetchall()
        after = conn.execute("SELECT raw, raw_id, last_parsed_digest FROM sources").fetchone()
        conn.close()
        edited_body = wiki.raw_body_digest(raw_path)
        check(ctx.results, "a hand-edited raw becomes a new row and keeps its parse stamp",
              len(raw_rows) == 1 and raw_rows[0][0] == edited_body
              and after[1] == edited_body and after[2] == parsed and after[0] == before[0]
              and adopted["vanished"] == [],
              (raw_rows, after, parsed, adopted))
        kept = raw_path.read_text(encoding="utf-8")
        again = run(edit_root, "ingest")["data"]
        check(ctx.results, "the next ingest leaves the hand-edited raw alone",
              raw_path.read_text(encoding="utf-8") == kept and again["ingested"] == [],
              again)
        (edit_root / "a.md").write_text("# 手改\n\n源又改了。\n", encoding="utf-8")
        rewritten = run(edit_root, "ingest")["data"]
        conn = sqlite3.connect(edit_root / ".wiki" / "wiki.db")
        raw_rows = conn.execute("SELECT id, path FROM raws").fetchall()
        current = conn.execute("SELECT raw_id, last_parsed_digest FROM sources").fetchone()
        conn.close()
        check(ctx.results, "a changed source rewrites the raw and reclaims the old row",
              len(raw_rows) == 1 and raw_rows[0][0] == current[0] == current[1]
              and "源又改了" in raw_path.read_text(encoding="utf-8"),
              (raw_rows, current, rewritten))
    finally:
        shutil.rmtree(edit_root, ignore_errors=True)

    # 共享 raw 的手改属于整份正文：所有挂载一起换行，文件名不跟着摘要跳。
    shared_root = Path(tempfile.mkdtemp(prefix="llm-wiki-raw-shared-"))
    try:
        shared_body = "# 共享手改\n\n同一份正文。\n"
        (shared_root / "a.md").write_text(shared_body, encoding="utf-8")
        (shared_root / "copy").mkdir()
        (shared_root / "copy" / "a.md").write_text(shared_body, encoding="utf-8")
        run(shared_root, "ingest")
        conn = sqlite3.connect(shared_root / ".wiki" / "wiki.db")
        raw_rel, _, parsed = conn.execute(
            "SELECT raw, raw_id, last_parsed_digest FROM sources WHERE key = 'a.md'"
        ).fetchone()
        conn.close()
        raw_path = shared_root / raw_rel
        raw_path.write_text(
            raw_path.read_text(encoding="utf-8").rstrip("\n") + "\n\n用户手改的一句。\n",
            encoding="utf-8",
        )
        adopted = run(shared_root, "ingest")["data"]
        conn = sqlite3.connect(shared_root / ".wiki" / "wiki.db")
        raw_rows = conn.execute("SELECT id, path FROM raws").fetchall()
        after = conn.execute(
            "SELECT key, raw, raw_id, last_parsed_digest FROM sources ORDER BY key"
        ).fetchall()
        conn.close()
        edited_body = wiki.raw_body_digest(raw_path)
        check(ctx.results, "a shared hand edit moves every mount and keeps the file",
              len(raw_rows) == 1 and raw_rows[0] == (edited_body, raw_rel)
              and [row[2] for row in after] == [edited_body, edited_body]
              and [row[3] for row in after] == [parsed, parsed]
              and [row[1] for row in after] == [raw_rel, raw_rel]
              and adopted["reclaimed"] == [],
              (raw_rows, after, edited_body, adopted))
        kept = raw_path.read_text(encoding="utf-8")
        again = run(shared_root, "ingest")["data"]
        conn = sqlite3.connect(shared_root / ".wiki" / "wiki.db")
        raw_rows = conn.execute("SELECT id, path FROM raws").fetchall()
        conn.close()
        check(ctx.results, "a shared hand edit stays put on the next ingest",
              again["ingested"] == [] and len(raw_rows) == 1
              and raw_rows[0] == (edited_body, raw_rel)
              and raw_path.read_text(encoding="utf-8") == kept,
              (again, raw_rows))
        raw_path.write_text(
            kept.rstrip("\n") + "\n\n再手改一句。\n", encoding="utf-8"
        )
        run(shared_root, "ingest")
        names = sorted(p.name for p in (shared_root / "wiki/raw").glob("*.md"))
        check(ctx.results, "repeated shared edits keep the raw at its original name",
              names == [raw_path.name], names)
    finally:
        shutil.rmtree(shared_root, ignore_errors=True)

    # 手改成另一份已存在的正文：整份并过去，旧行和旧文件回收。
    merge_root = Path(tempfile.mkdtemp(prefix="llm-wiki-raw-merge-"))
    try:
        (merge_root / "a.md").write_text("# 甲\n\n甲正文。\n", encoding="utf-8")
        (merge_root / "b.md").write_text("# 乙\n\n乙正文。\n", encoding="utf-8")
        (merge_root / "copy").mkdir()
        (merge_root / "copy" / "b.md").write_text("# 乙\n\n乙正文。\n", encoding="utf-8")
        run(merge_root, "ingest")
        conn = sqlite3.connect(merge_root / ".wiki" / "wiki.db")
        a_raw = conn.execute("SELECT raw FROM sources WHERE key = 'a.md'").fetchone()[0]
        b_raw = conn.execute("SELECT raw FROM sources WHERE key = 'b.md'").fetchone()[0]
        conn.close()
        (merge_root / b_raw).write_text(
            (merge_root / a_raw).read_text(encoding="utf-8"), encoding="utf-8"
        )
        merged = run(merge_root, "ingest")["data"]
        conn = sqlite3.connect(merge_root / ".wiki" / "wiki.db")
        raw_rows = conn.execute("SELECT id, path FROM raws").fetchall()
        moved = conn.execute(
            "SELECT key, raw, raw_id FROM sources WHERE key LIKE '%b.md' ORDER BY key"
        ).fetchall()
        conn.close()
        check(ctx.results, "a hand edit equal to another raw merges the whole document",
              len(raw_rows) == 1 and merged["reclaimed"] == [b_raw]
              and [row[1] for row in moved] == [a_raw, a_raw]
              and [row[2] for row in moved] == [raw_rows[0][0], raw_rows[0][0]]
              and not (merge_root / b_raw).exists(),
              (raw_rows, moved, merged))
    finally:
        shutil.rmtree(merge_root, ignore_errors=True)
