"""任务状态机：排队、领取、租约续期与关闭。

账本里的 tasks/task_sources 是唯一事实源；本模块只做读写与状态迁移，
不关心命令怎么排版输出。
"""

from __future__ import annotations

import json
import math
import os
import shutil
import sqlite3
from datetime import datetime, timezone

from errors import WikiError
from tasks import render_brief, task_packet
from util import digest_bytes, dump_meta, now_iso, same_stamp


TASK_LEASE_SECONDS = 900
TASK_OPEN_STATES = ("queued", "leased")
QUERY_BUDGET_CHARS = 1000
QUERY_WORDS_PER_WINDOW = 5
QUERY_LOG = "queries.jsonl"


class TaskStoreMixin:
    """任务行的持久化与迁移；由 Wiki 组合，不自带初始化。"""

    def task_row(self, task_id):
        return self.conn.execute("SELECT * FROM tasks WHERE id = ?", (task_id,)).fetchone()

    def task_dir(self, task_id):
        return self.tasks_dir / task_id

    def clear_task_dir(self, task_id):
        shutil.rmtree(self.task_dir(task_id), ignore_errors=True)

    def cleanup_task_dirs(self):
        """Drop task folders that no longer belong to an open task."""
        if not self.tasks_dir.is_dir():
            return []
        dropped = []
        for entry in sorted(self.tasks_dir.iterdir()):
            if not entry.is_dir():
                continue
            row = self.task_row(entry.name)
            if row and row["state"] in TASK_OPEN_STATES:
                continue
            shutil.rmtree(entry, ignore_errors=True)
            dropped.append(entry.name)
        return dropped

    def query_log_path(self, task_id):
        return self.task_dir(task_id) / QUERY_LOG

    def task_queries(self, task_id):
        """Registered searches of one task, oldest first."""
        path = self.query_log_path(task_id)
        if not path.exists():
            return []
        records = []
        for line in path.read_text(encoding="utf-8").splitlines():
            if not line.strip():
                continue
            try:
                records.append(json.loads(line))
            except json.JSONDecodeError:
                # 日志只服务当前任务，坏行跳过比让任务卡死划算。
                continue
        return records

    def query_words(self, task_id):
        """Topic words registered so far: unique, first-seen order."""
        words = []
        seen = set()
        for record in self.task_queries(task_id):
            for word in record.get("words") or []:
                text = str(word).strip()
                if text and text.lower() not in seen:
                    seen.add(text.lower())
                    words.append(text)
        return words

    def query_budget(self, row):
        """Topic words a task may register: five per 1000 source characters."""
        inputs = json.loads(row["inputs"])
        chars = sum(len(self.source_body(item["key"])) for item in inputs.get("sources", []))
        windows = max(1, math.ceil(chars / QUERY_BUDGET_CHARS))
        return QUERY_WORDS_PER_WINDOW * windows

    def query_view(self, row):
        used = self.query_words(row["id"])
        budget = self.query_budget(row)
        return {
            "budget": budget,
            "used": len(used),
            "remaining": max(0, budget - len(used)),
            "log": f"{self.tasks_dir.name}/{row['id']}/{QUERY_LOG}",
        }

    def append_task_query(self, task_id, words, hits):
        path = self.query_log_path(task_id)
        path.parent.mkdir(parents=True, exist_ok=True)
        record = {"ts": now_iso(), "words": list(words), "hits": hits}
        with path.open("a", encoding="utf-8") as handle:
            handle.write(json.dumps(record, ensure_ascii=False, sort_keys=True) + "\n")

    def materialize_task(self, row):
        """Write the task folder: the brief, its payload skeleton, and work data.

        Re-claiming a task refreshes the brief and the work view but never
        overwrites a payload.json the agent already filled.
        """
        work = self.task_work(row)
        packet = task_packet(row["id"], row["kind"], json.loads(row["inputs"]), work)
        directory = self.task_dir(row["id"])
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "brief.md").write_text(
            render_brief(row["id"], packet["brief"]), encoding="utf-8"
        )
        (directory / "work.json").write_text(
            json.dumps(work, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        skeleton = directory / "payload.json"
        if not skeleton.exists():
            skeleton.write_text(
                json.dumps(packet["template"], ensure_ascii=False, indent=2) + "\n",
                encoding="utf-8",
            )
        return packet

    def open_task_for_page(self, page_id):
        return self.conn.execute(
            "SELECT * FROM tasks WHERE kind = 'page' AND subject = ? AND state IN ('queued', 'leased')",
            (page_id,),
        ).fetchone()

    def open_tasks(self):
        return self.conn.execute(
            "SELECT * FROM tasks WHERE state IN ('queued', 'leased') ORDER BY id"
        ).fetchall()

    def task_counts(self):
        # 队列口径只在这一处定义：open 是 queued 与 leased 之和，expired/accepted 是
        # 终态历史，算在返回里供 status 使用，但不属于未完成的工作。
        rows = self.conn.execute(
            "SELECT kind, state, COUNT(*) AS n FROM tasks "
            "WHERE state IN ('queued', 'leased', 'expired', 'accepted') GROUP BY kind, state"
        ).fetchall()
        counts = {"open": 0, "queued": 0, "leased": 0, "route": 0, "page": 0,
                  "expired": 0, "accepted": 0}
        for row in rows:
            counts[row["state"]] += row["n"]
            if row["state"] in ("queued", "leased"):
                counts["open"] += row["n"]
                counts[row["kind"]] = counts.get(row["kind"], 0) + row["n"]
        return counts

    def source_in_open_task(self, source_key):
        return self.conn.execute(
            """
            SELECT t.id FROM task_sources ts JOIN tasks t ON t.id = ts.task_id
            WHERE ts.source_key = ? AND t.state IN ('queued', 'leased')
            LIMIT 1
            """,
            (source_key,),
        ).fetchone()

    def enqueue_task(self, kind, subject, inputs):
        existing = self.conn.execute(
            "SELECT id FROM tasks WHERE kind = ? AND subject = ? AND state IN ('queued', 'leased')",
            (kind, subject),
        ).fetchone()
        if existing:
            return existing["id"]
        # 树唯一性：一个源最多出现在一套未完成的任务树里。route 任务只是临时持有者，
        # 提交时会把源交给 page 树，所以只有 page 持有者才挡新树。同一次 route 提交
        # 拆出的多个 page 任务共享 route_task，是这条不变量的唯一例外。
        route_task = inputs.get("route_task")
        for item in inputs.get("sources", []):
            holder = self.source_in_open_task(item["key"])
            if not holder:
                continue
            holder_row = self.task_row(holder["id"])
            if holder_row["kind"] != "page":
                continue
            if kind == "page" and holder_row["subject"] == subject:
                continue
            if route_task:
                holder_inputs = json.loads(holder_row["inputs"])
                if holder_inputs.get("route_task") == route_task:
                    continue
            raise WikiError(
                "E_TASK_CONFLICT",
                f"source {item['key']} already belongs to task {holder_row['id']} "
                f"({holder_row['kind']} {holder_row['subject']})",
                "run `next` and `submit` to close that task tree first",
                3,
            )
        task_id = "t_" + os.urandom(4).hex()
        encoded = json.dumps(inputs, ensure_ascii=False, sort_keys=True)
        try:
            with self.conn:
                self.conn.execute(
                    """
                    INSERT INTO tasks (id, kind, subject, state, epoch, inputs, created_at)
                    VALUES (?, ?, ?, 'queued', 0, ?, ?)
                    """,
                    (task_id, kind, subject, encoded, now_iso()),
                )
                for item in inputs.get("sources", []):
                    self.conn.execute(
                        "INSERT OR IGNORE INTO task_sources (task_id, source_key) VALUES (?, ?)",
                        (task_id, item["key"]),
                    )
        except sqlite3.IntegrityError:
            # 另一个进程在检查与插入之间抢到同一页；账本的唯一索引兜底，复用那条任务。
            existing = self.conn.execute(
                "SELECT id FROM tasks WHERE kind = ? AND subject = ? AND state IN ('queued', 'leased')",
                (kind, subject),
            ).fetchone()
            if existing:
                return existing["id"]
            raise
        return task_id

    def attach_page_source(self, task_id, source_key):
        """Add a source, with its full snapshot, to an open page task."""
        row = self.task_row(task_id)
        inputs = json.loads(row["inputs"])
        if any(item["key"] == source_key for item in inputs.get("sources", [])):
            # 已经在树上：幂等返回，绝不刷新既有快照。
            return
        inputs["sources"].append(self.source_snapshot(source_key))
        with self.conn:
            self.conn.execute(
                "UPDATE tasks SET inputs = ? WHERE id = ?",
                (json.dumps(inputs, ensure_ascii=False, sort_keys=True), task_id),
            )
            self.conn.execute(
                "INSERT OR IGNORE INTO task_sources (task_id, source_key) VALUES (?, ?)",
                (task_id, source_key),
            )

    def attach_page_links(self, task_id, links):
        """Union route-decided edges into an open page task (add-only)."""
        row = self.task_row(task_id)
        inputs = json.loads(row["inputs"])
        current = {str(item).strip().lower() for item in inputs.get("links") or []}
        current.update(str(item).strip().lower() for item in links if str(item).strip())
        inputs["links"] = sorted(current)
        with self.conn:
            self.conn.execute(
                "UPDATE tasks SET inputs = ? WHERE id = ?",
                (json.dumps(inputs, ensure_ascii=False, sort_keys=True), task_id),
            )

    def task_drift(self, row):
        """First reason this task's snapshot no longer matches the files, or None when it is fresh."""
        inputs = json.loads(row["inputs"])
        for item in inputs.get("sources", []):
            key = item["key"]
            # 旧版三字段快照（key/digest/raw）没有源指纹，判不出是否漂移：
            # 当作漂移处理，让任务直接过期，不迁移、不猜版本。
            if not {"source_size", "source_mtime", "source_digest"} <= set(item):
                return f"task snapshot for {key} predates source fingerprints"
            source = self.source_row(key)
            if source is None or source["status"] == "missing":
                return f"source {key} vanished"
            # 两段式判定：先比 stat，一致就不读内容；不一致才读字节算摘要，
            # 摘要相同说明只是被 touch，任务仍然有效（规格 §判定表）。
            path = self.scan_root / source["path"]
            try:
                stat = path.stat()
            except OSError:
                return f"source {key} vanished"
            if not same_stamp(stat.st_size, stat.st_mtime,
                              item["source_size"], item["source_mtime"]):
                if digest_bytes(path.read_bytes()) != item["source_digest"]:
                    return f"source {key} changed"
            raw = item.get("raw") or ""
            if not raw:
                continue
            raw_path = self.root / raw
            try:
                raw_stat = raw_path.stat()
            except OSError:
                return f"raw copy {raw} is missing"
            if not same_stamp(raw_stat.st_size, raw_stat.st_mtime,
                              item["raw_size"], item["raw_mtime"]):
                if digest_bytes(raw_path.read_bytes()) != item["raw_digest"]:
                    return f"raw copy {raw} was edited"
        # 页面在任务排队后又前进了一版，任务基于的修订已经过期。
        if inputs.get("mode") == "update" and inputs.get("base_rev") is not None:
            record = self.page_record(inputs["page"])
            if not record or record["rev"] != inputs["base_rev"]:
                return f"page {inputs['page']} moved past revision {inputs['base_rev']}"
        return None

    def pin_open_task_raw(self, key, raw, raw_digest, raw_size, raw_mtime):
        """Point open task snapshots at an adopted raw copy (raw side only)."""
        rows = self.conn.execute(
            "SELECT id, inputs FROM tasks WHERE state IN ('queued', 'leased') "
            "AND id IN (SELECT task_id FROM task_sources WHERE source_key = ?)",
            (key,),
        ).fetchall()
        with self.conn:
            for row in rows:
                inputs = json.loads(row["inputs"])
                changed = False
                for item in inputs.get("sources", []):
                    if item["key"] != key:
                        continue
                    item.update(
                        {
                            "raw": raw,
                            "raw_size": raw_size,
                            "raw_mtime": raw_mtime,
                            "raw_digest": raw_digest,
                        }
                    )
                    changed = True
                if changed:
                    self.conn.execute(
                        "UPDATE tasks SET inputs = ? WHERE id = ?",
                        (json.dumps(inputs, ensure_ascii=False, sort_keys=True), row["id"]),
                    )

    def claim_tasks(self, kind=None, batch=None):
        """Claim queued tasks in tree order.

        A task stays `leased` until it is submitted or released, and an agent
        holds at most what it has not finished: `command_next` refuses to hand
        out anything while a lease is open, so a claim never reissues work the
        agent already has. Returns the claimed rows.
        """
        now = now_iso()
        with self.conn:
            self.conn.execute(
                "UPDATE tasks SET state = 'queued', lease_until = NULL WHERE state = 'leased' AND lease_until < ?",
                (now,),
            )
            # 深度优先：先钉住手头那棵树，一批只在它里面发；树里没活可发
            # （例如全被判漂移）才退回常规顺序。
            active = self._active_page_tree() if kind != "route" else None
            for pinned in (active, None):
                claimed = self._claim_queued(kind, batch, pinned)
                if claimed or pinned is None:
                    break
        return claimed

    def leased_tasks(self):
        """Tasks an agent has taken but not submitted or released yet."""
        rows = self.conn.execute(
            "SELECT rowid, * FROM tasks WHERE state = 'leased'"
        ).fetchall()
        return self._task_order(rows)

    def _claim_queued(self, kind, batch, tree=None):
        sql = "SELECT rowid, * FROM tasks WHERE state = 'queued'"
        params = []
        if kind:
            sql += " AND kind = ?"
            params.append(kind)
        claimed = []
        rows = self._task_order(self.conn.execute(sql, params).fetchall())
        if not kind and self._open_child_task():
            # 深度优先：一棵任务树还有页面没写完，就先不发新的来源。
            rows = [row for row in rows if row["kind"] != "route"]
        rows = self._batch_rows(rows, tree)
        for row in rows:
            if len(claimed) >= batch:
                break
            reason = self.task_drift(row)
            if reason:
                # 失效任务留在账本里作为历史，但不发出去。
                self.expire_task(row["id"], reason)
                continue
            self.conn.execute(
                "UPDATE tasks SET state = 'leased', epoch = epoch + 1, lease_until = ?, attempts = attempts + 1 WHERE id = ?",
                (self._lease_deadline(), row["id"]),
            )
            claimed.append(self.task_row(row["id"]))
        return claimed

    def _active_page_tree(self):
        """Tree of the oldest open page task: the one an agent is working on."""
        rows = self.conn.execute(
            "SELECT rowid, * FROM tasks WHERE kind = 'page' AND state IN ('queued', 'leased')"
        ).fetchall()
        rows = self._task_order(rows)
        return self._tree_key(rows[0]) if rows else None

    def _open_child_task(self):
        """An open page task means its tree has not drained yet."""
        return self.conn.execute(
            "SELECT 1 FROM tasks WHERE kind = 'page' AND state IN ('queued', 'leased') LIMIT 1"
        ).fetchone() is not None

    def _tree_key(self, row):
        """Tree of a task: the route that spawned it, or the task itself."""
        try:
            inputs = json.loads(row["inputs"] or "{}")
        except json.JSONDecodeError:
            inputs = {}
        return inputs.get("route_task") or row["id"]

    def _batch_rows(self, rows, tree=None):
        """Keep one batch inside one tree; route tasks stay independent.

        Page tasks of one tree share a source and their boundaries depend on
        each other, so handing them out piecemeal with another tree in the
        same batch is what let a route collide with its own batch. A batch
        that cannot be filled from this tree stays short on purpose.
        """
        if not rows:
            return rows
        if tree is None and rows[0]["kind"] == "page":
            tree = self._tree_key(rows[0])
        if tree is None:
            return rows
        return [row for row in rows if self._tree_key(row) == tree]

    def _task_order(self, rows):
        """Depth-first claim order: a started tree drains before the next source.

        Page tasks carry `inputs.route_task`, so the parent tree is known
        without adding a column. `rowid` keeps same-second enqueues in insert
        order — the order the route listed its pages in — so sibling pages no
        longer come out in an arbitrary order and links stop failing by luck.
        """

        def key(row):
            try:
                inputs = json.loads(row["inputs"] or "{}")
            except json.JSONDecodeError:
                inputs = {}
            parent = 0 if inputs.get("route_task") else 1
            return (parent, row["created_at"], row["rowid"])

        return sorted(rows, key=key)

    def _lease_deadline(self):
        return datetime.fromtimestamp(
            datetime.now(timezone.utc).timestamp() + TASK_LEASE_SECONDS, timezone.utc
        ).strftime("%Y-%m-%dT%H:%M:%SZ")

    def release_task(self, task_id):
        row = self.task_row(task_id)
        if not row:
            raise WikiError("E_TASK_UNKNOWN", f"no task {task_id}", "run `next` to get a task id", 2)
        with self.conn:
            self.conn.execute(
                "UPDATE tasks SET state = 'queued', lease_until = NULL WHERE id = ?", (task_id,)
            )

    def requeue_task(self, task_id, error=None):
        with self.conn:
            self.conn.execute(
                "UPDATE tasks SET state = 'queued', lease_until = NULL, last_error = ? WHERE id = ?",
                (error, task_id),
            )

    def expire_task(self, task_id, reason):
        """Close an open task as expired, keeping its row for history."""
        with self.conn:
            cursor = self.conn.execute(
                "UPDATE tasks SET state = 'expired', last_error = ?, lease_until = NULL, closed_at = ? "
                "WHERE id = ? AND state IN ('queued', 'leased')",
                (reason, now_iso(), task_id),
            )
        if cursor.rowcount > 0:
            self.clear_task_dir(task_id)
        return cursor.rowcount > 0

    def close_task(self, task_id, payload, epoch=None):
        sql = "UPDATE tasks SET state = 'accepted', payload = ?, lease_until = NULL, closed_at = ? WHERE id = ?"
        params = [json.dumps(payload, ensure_ascii=False, sort_keys=True), now_iso(), task_id]
        if epoch is not None:
            sql += " AND epoch = ? AND state = 'leased'"
            params.append(epoch)
        with self.conn:
            cursor = self.conn.execute(sql, params)
        if epoch is not None and cursor.rowcount == 0:
            raise WikiError("E_TASK_STALE", f"task {task_id} changed before it could be closed",
                            "run `next` again and submit the fresh task", 3)
        if cursor.rowcount > 0:
            self.clear_task_dir(task_id)

    def page_index(self):
        outgoing = {
            row["src"]: row["n"]
            for row in self.conn.execute("SELECT src, COUNT(*) AS n FROM links GROUP BY src")
        }
        incoming = {
            row["dst"]: row["n"]
            for row in self.conn.execute("SELECT dst, COUNT(*) AS n FROM links GROUP BY dst")
        }
        return [
            {
                "id": record["id"],
                "title": record["meta"].get("title"),
                "type": record["meta"].get("type"),
                "summary": record["meta"].get("summary") or "",
                "outgoing": outgoing.get(record["id"], 0),
                "incoming": incoming.get(record["id"], 0),
            }
            for record in self.all_pages()
        ]

    def pending_pages(self, exclude=None):
        """Pages that a queued page task will create but that do not exist yet."""
        rows = self.conn.execute(
            "SELECT id, subject, inputs FROM tasks WHERE kind = 'page' AND state IN ('queued', 'leased')"
        )
        pending = []
        for row in rows:
            if row["id"] == exclude:
                continue
            inputs = json.loads(row["inputs"])
            if inputs.get("mode") == "create" and not self.page_record(row["subject"]):
                pending.append({"id": row["subject"], "title": inputs.get("title"),
                                "type": inputs.get("type")})
        return pending

    def source_view(self, item):
        row = self.source_row(item["key"])
        return {
            "key": item["key"],
            "digest": row["digest"] if row else None,
            "raw": row["raw"] if row else None,
            "status": row["status"] if row else "unknown",
            "chars": len(self.source_body(item["key"])),
        }

    def task_work(self, row):
        inputs = json.loads(row["inputs"])
        work = {
            "sources": [
                {
                    "key": view["key"],
                    "raw": view["raw"],
                    "chars": view["chars"],
                }
                for view in (self.source_view(item) for item in inputs.get("sources", []))
            ],
            "page_index": self.page_index(),
            "pending_pages": self.pending_pages(row["id"]),
            "queries": self.query_view(row),
            "tree": self._tree_key(row),
        }
        if row["kind"] == "page":
            record = self.page_record(inputs["page"])
            if record:
                work["page"] = {
                    "id": record["id"],
                    "rev": record["rev"],
                    "meta": dump_meta(record["meta"]),
                    "body": record["body"],
                }
        return work
