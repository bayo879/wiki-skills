"""任务排队：入队、来源归属、链接预填与来源漂移拦截。

`Wiki` 的组合来源之一；任务行与租约生命周期在 store.py 与 lease.py。"""

from __future__ import annotations

import json
import os
import sqlite3

from errors import WikiError
from ingest import raw_body_digest
from util import digest_bytes, now_iso, same_stamp


class TaskQueueMixin:
    """任务排队与来源归属：入队、漂移判定与原文固定；由 Wiki 组合。"""

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

    def attach_task_source(self, task_id, source_key):
        """Add a source, with its full snapshot, to an open task (route or page)."""
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
                if raw_body_digest(raw_path) != item["raw_digest"]:
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
