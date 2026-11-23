"""任务行的持久化：任务行、目录与 payload、查询预算与视图面。

账本里的 tasks/task_sources 是唯一事实源；本模块只做读写与状态迁移，不关心命令怎么排版输出。
`Wiki` 的组合来源之一；排队见 task_queue，领取与租约见 task_lease。"""

from __future__ import annotations

import json
import math
import shutil

from tasks import render_brief, task_packet
from util import dump_meta, now_iso


TASK_LEASE_SECONDS = 900
TASK_OPEN_STATES = ("queued", "leased")
QUERY_BUDGET_CHARS = 1000
QUERY_WORDS_PER_WINDOW = 5
QUERY_LOG = "queries.jsonl"


class TaskStoreMixin:
    """任务行与查询预算：任务行、目录、payload、查询历史与视图面；由 Wiki 组合。"""

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

    def open_task_for_raw(self, raw_id):
        """Any open task holding one of the raw's mounts; identity is the body."""
        return self.conn.execute(
            """
            SELECT t.id, t.kind, t.subject FROM task_sources ts
            JOIN tasks t ON t.id = ts.task_id
            WHERE t.state IN ('queued', 'leased')
              AND ts.source_key IN (SELECT key FROM sources WHERE raw_id = ?)
            ORDER BY t.created_at, t.id
            LIMIT 1
            """,
            (raw_id,),
        ).fetchone()

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
            "raw_id": row["raw_id"] if row else None,
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
                    "raw_id": view["raw_id"],
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
