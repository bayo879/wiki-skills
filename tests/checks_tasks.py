"""Task directory, lock and lease checks.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

from support import check, route_query, run

# 引擎模块要等 support 把脚本目录加进 sys.path，因此排在 support 之后。
import workspace


def task_locks(ctx):
    task_root = Path(tempfile.mkdtemp(prefix="llm-wiki-taskdir-"))
    try:
        (task_root / "a.md").write_text("# 任务目录\n\n正文一句话。\n", encoding="utf-8")
        run(task_root, "ingest")
        handed = run(task_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        folder = task_root / ".wiki" / "tasks" / handed["id"]
        names = sorted(path.name for path in folder.iterdir())
        check(ctx.results, "next materializes the task folder",
              names == ["brief.md", "payload.json", "work.json"], names)
        route_query(task_root, handed, "任务目录")
        (folder / "payload.json").write_text(json.dumps({"decisions": [
            {"source": handed["work"]["sources"][0]["key"], "action": "skip",
             "reason": "与主题无关"}]}, ensure_ascii=False), encoding="utf-8")
        accepted = run(task_root, "submit", handed["id"])
        check(ctx.results, "submit reads the task payload by default", accepted["ok"], accepted)
        check(ctx.results, "a closed task drops its folder", not folder.exists(), folder)

        (task_root / "b.md").write_text("# 第二个来源\n\n正文两句话。\n", encoding="utf-8")
        run(task_root, "ingest")
        route = run(task_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(task_root, route, "第二个来源")
        route_folder = task_root / ".wiki" / "tasks" / route["id"]
        (route_folder / "payload.json").write_text(json.dumps({"decisions": [
            {"source": route["work"]["sources"][0]["key"], "action": "new",
             "page": "task-dir-page", "title": "任务目录页", "type": "concept",
             "reason": "唯一来源"}]}, ensure_ascii=False), encoding="utf-8")
        run(task_root, "submit", route["id"])
        page_task = run(task_root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
        page_folder = task_root / ".wiki" / "tasks" / page_task["id"]
        filled = page_folder / "payload.json"
        original = filled.read_text(encoding="utf-8")
        run(task_root, "release", page_task["id"])
        check(ctx.results, "release keeps the filled payload",
              filled.read_text(encoding="utf-8") == original, filled)
        reclaimed = run(task_root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
        check(ctx.results, "a re-claimed task keeps the agent's payload",
              reclaimed["id"] == page_task["id"]
              and filled.read_text(encoding="utf-8") == original, reclaimed["id"])

        # 领取后不提交、租约未过期时再 next：报出未提交的任务，不重发也不续租。
        conn = sqlite3.connect(task_root / ".wiki" / "wiki.db")
        before = conn.execute(
            "SELECT epoch, attempts, lease_until FROM tasks WHERE id = ?", (page_task["id"],)
        ).fetchone()
        conn.close()
        blocked = run(task_root, "next", "--kind", "page", "--batch", "1", expect=3)
        error = blocked.get("error", {})
        check(ctx.results, "next reports the open task instead of reissuing it",
              error.get("code") == "E_TASK_UNSUBMITTED"
              and page_task["id"] in error.get("message", "")
              and page_task["id"] in error.get("next", "")
              and filled.read_text(encoding="utf-8") == original, blocked)
        conn = sqlite3.connect(task_root / ".wiki" / "wiki.db")
        after = conn.execute(
            "SELECT epoch, attempts, lease_until FROM tasks WHERE id = ?", (page_task["id"],)
        ).fetchone()
        conn.close()
        check(ctx.results, "a refused next does not renew the lease or bump the epoch",
              before == after, (before, after))

        # 同一页面只允许一个未关闭的 page 任务：第二个来源挂上去，不新增第二条。
        run(task_root, "release", page_task["id"])
        (task_root / "c.md").write_text("# 第三个来源\n\n正文三句话。\n", encoding="utf-8")
        run(task_root, "ingest")
        second = run(task_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(task_root, second, "第三个来源")
        second_folder = task_root / ".wiki" / "tasks" / second["id"]
        (second_folder / "payload.json").write_text(json.dumps({"decisions": [
            {"source": second["work"]["sources"][0]["key"], "action": "new",
             "page": "task-dir-page", "title": "任务目录页", "type": "concept",
             "reason": "同一页补来源"}]}, ensure_ascii=False), encoding="utf-8")
        attached = run(task_root, "submit", second["id"])
        conn = sqlite3.connect(task_root / ".wiki" / "wiki.db")
        open_pages = conn.execute(
            "SELECT id FROM tasks WHERE kind = 'page' AND state IN ('queued', 'leased')"
        ).fetchall()
        mounted = conn.execute(
            "SELECT COUNT(*) FROM task_sources WHERE task_id = ?", (page_task["id"],)
        ).fetchone()[0]
        conn.close()
        check(ctx.results, "a second route attaches to the open page task",
              attached["ok"] and len(open_pages) == 1 and mounted == 2,
              (attached, open_pages, mounted))

        stale = task_root / ".wiki" / "tasks" / "t_deadbeef"
        stale.mkdir()
        run(task_root, "ingest")
        check(ctx.results, "next reaps folders of closed tasks", not stale.exists(), stale)

        # 写命令排队：另一进程占着 `lock` 时等 5 秒后报 E_BUSY；陈旧的锁被接管。
        lock = task_root / ".wiki" / "lock"
        holder = subprocess.Popen(
            [sys.executable, "-c",
             "import os,sys,time; p=sys.argv[1];"
             " fd=os.open(p, os.O_CREAT|os.O_EXCL|os.O_WRONLY, 0o644);"
             " os.write(fd, str(os.getpid()).encode()); os.close(fd); time.sleep(9)",
             str(lock)]
        )
        try:
            time.sleep(1.0)
            busy = run(task_root, "status", expect=4)
        finally:
            holder.wait()
        check(ctx.results, "a held lock returns E_BUSY",
              busy.get("error", {}).get("code") == "E_BUSY", busy)
        lock.write_text("999999", encoding="utf-8")
        taken = run(task_root, "status")
        check(ctx.results, "a stale lock is taken over",
              taken["ok"] and not lock.exists(), (taken, lock))

        # 空内容的新鲜锁不等于陈旧锁：创建即带 pid，但旧版本或没有硬链接的
        # 文件系统可能留下空锁，宽限期内必须排队，不能删掉别人的锁。
        lock.write_bytes(b"")
        started = time.monotonic()
        empty_busy = run(task_root, "status", expect=4, timeout=20)
        check(ctx.results, "an empty fresh lock returns E_BUSY instead of being taken over",
              empty_busy.get("error", {}).get("code") == "E_BUSY"
              and lock.exists() and time.monotonic() - started >= 4.5,
              (empty_busy, lock.exists()))
        # 过了宽限期还是空锁：按陈旧接管，不能把工作区永久卡住。
        aged = time.time() - (workspace.LOCK_EMPTY_GRACE_SECONDS + 5)
        os.utime(lock, (aged, aged))
        aged_taken = run(task_root, "status")
        check(ctx.results, "an empty lock past the grace period is taken over",
              aged_taken["ok"] and not lock.exists(), (aged_taken, lock.exists()))

        # 锁出现时已带持有者记录：持锁期间内容就是自己的 pid，也不留临时名。
        with workspace.workspace_lock(task_root):
            owner = lock.read_text(encoding="utf-8").split()
            residue = [p.name for p in lock.parent.glob("lock.*")]
        check(ctx.results, "a fresh lock carries its owner pid and leaves no temp name",
              owner and owner[0] == str(os.getpid()) and not residue,
              (owner, residue))

        # 异常锁文件不能让加锁命令挂住：目录、FIFO、不可删的陈旧锁都要快速报 E_STATE。
        def abnormal_lock(name, prepare):
            case = Path(tempfile.mkdtemp(prefix="llm-wiki-lock-"))
            try:
                (case / ".wiki").mkdir()
                prepare(case / ".wiki" / "lock")
                started = time.monotonic()
                try:
                    denied = run(case, "status", expect=3, timeout=10)
                except subprocess.TimeoutExpired:
                    check(ctx.results, name, False, "command did not return within 10s")
                elapsed = time.monotonic() - started
                check(ctx.results, name,
                      elapsed < 8 and denied.get("error", {}).get("code") == "E_STATE",
                      (elapsed, denied))
            finally:
                shutil.rmtree(case, ignore_errors=True)

        abnormal_lock("a directory lock fails with E_STATE instead of hanging",
                      lambda path: path.mkdir())
        if hasattr(os, "mkfifo"):
            abnormal_lock("a FIFO lock fails with E_STATE instead of blocking",
                          lambda path: os.mkfifo(path))

        # 锁文件本身不可删：同样要报错，不能反复判陈旧后 continue。
        unremovable = Path(tempfile.mkdtemp(prefix="llm-wiki-lock-ro-"))
        try:
            (unremovable / ".wiki").mkdir()
            (unremovable / ".wiki" / "lock").write_text("999999", encoding="utf-8")
            (unremovable / ".wiki").chmod(0o555)
            if not os.access(unremovable / ".wiki", os.W_OK):
                denied = run(unremovable, "status", expect=3, timeout=10)
                check(ctx.results, "an unremovable stale lock fails with E_STATE",
                      denied.get("error", {}).get("code") == "E_STATE", denied)
        finally:
            (unremovable / ".wiki").chmod(0o755)
            shutil.rmtree(unremovable, ignore_errors=True)

        # `.wiki` 存在但不可写：创建锁失败也要变成 E_HEALTH，而不是 traceback。
        unwritable = Path(tempfile.mkdtemp(prefix="llm-wiki-wiki-ro-"))
        try:
            (unwritable / ".wiki").mkdir()
            (unwritable / ".wiki").chmod(0o555)
            if not os.access(unwritable / ".wiki", os.W_OK):
                denied = run(unwritable, "status", expect=1, timeout=10)
                check(ctx.results, "an unwritable .wiki fails with E_HEALTH",
                      denied.get("error", {}).get("code") == "E_HEALTH", denied)
        finally:
            (unwritable / ".wiki").chmod(0o755)
            shutil.rmtree(unwritable, ignore_errors=True)

        # 普通 query 是只读快照：不取锁、不建目录、不推观看区。
        lock.mkdir()
        try:
            found = run(task_root, "query", "测试")
            check(ctx.results, "a plain query ignores the workspace lock",
                  found["ok"] and lock.is_dir(), (found, lock))
        finally:
            lock.rmdir()

        lock.mkdir()
        try:
            denied = run(task_root, "query", "--task", "t_missing", expect=3, timeout=10)
            check(ctx.results, "query --task still takes the workspace lock",
                  denied.get("error", {}).get("code") == "E_STATE", denied)
        finally:
            lock.rmdir()

        empty = Path(tempfile.mkdtemp(prefix="llm-wiki-query-empty-"))
        try:
            denied = run(empty, "query", "hello", expect=3)
            check(ctx.results, "query on an uninitialized workspace reports E_STATE",
                  denied.get("error", {}).get("code") == "E_STATE"
                  and not (empty / ".wiki").exists(), denied)
        finally:
            shutil.rmtree(empty, ignore_errors=True)

        legacy = Path(tempfile.mkdtemp(prefix="llm-wiki-query-legacy-"))
        try:
            (legacy / ".wiki").mkdir()
            conn = sqlite3.connect(legacy / ".wiki" / "wiki.db")
            conn.execute(
                "CREATE TABLE page_revs (page TEXT, rev INTEGER, meta TEXT, "
                "body TEXT, digest TEXT, render_digest TEXT, origin TEXT, "
                "accepted_at TEXT)"
            )
            conn.commit()
            conn.close()
            denied = run(legacy, "query", "hello", expect=3)
            check(ctx.results, "query refuses a ledger it cannot migrate read-only",
                  denied.get("error", {}).get("code") == "E_STATE", denied)
        finally:
            shutil.rmtree(legacy, ignore_errors=True)

        snapshot = Path(tempfile.mkdtemp(prefix="llm-wiki-query-snapshot-"))
        try:
            run(snapshot, "init")
            readme = snapshot / "wiki" / "README.md"
            readme.parent.mkdir(parents=True, exist_ok=True)
            readme.write_text("hand edited\n", encoding="utf-8")
            found = run(snapshot, "query", "hello")
            check(ctx.results, "query does not sync the view",
                  found["ok"] and readme.read_text(encoding="utf-8") == "hand edited\n",
                  (found, readme))
        finally:
            shutil.rmtree(snapshot, ignore_errors=True)

        readonly = Path(tempfile.mkdtemp(prefix="llm-wiki-query-ro-"))
        try:
            run(readonly, "init")
            (readonly / ".wiki").chmod(0o555)
            if not os.access(readonly / ".wiki", os.W_OK):
                found = run(readonly, "query", "hello", timeout=10)
                check(ctx.results, "query reads a read-only workspace",
                      found["ok"], found)
        finally:
            (readonly / ".wiki").chmod(0o755)
            shutil.rmtree(readonly, ignore_errors=True)
    finally:
        shutil.rmtree(task_root, ignore_errors=True)
