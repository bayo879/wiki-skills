"""任务领取与租约：领取、租约清扫、批与树的排序、释放与关闭。

`Wiki` 的组合来源之一；入队见 queue.py。"""

from __future__ import annotations

import json
from datetime import datetime, timezone

from errors import WikiError
from task.store import TASK_LEASE_SECONDS
from util import now_iso


class TaskLeaseMixin:
    """任务领取与租约：领取、清扫、批与树、释放与关闭；由 Wiki 组合。"""

    def claim_tasks(self, kind=None, batch=None, owner=""):
        """Claim queued tasks in tree order.

        A task stays `leased` until it is submitted or released, and an agent
        holds at most what it has not finished: `command_next` refuses to hand
        out anything while a lease is open, so a claim never reissues work the
        agent already has. The claim also records `owner` (the calling
        session), which is what lets `command_next` tell "my own open task"
        from "another session is already ingesting" — the second case stops
        the caller instead of letting sessions take turns on one queue.
        Returns the claimed rows.
        """
        self.sweep_leases()
        with self.conn:
            # 深度优先：先钉住手头那棵树，一批只在它里面发；树里没活可发
            # （例如全被判漂移）才退回常规顺序。
            active = self._active_page_tree() if kind != "route" else None
            for pinned in (active, None):
                claimed = self._claim_queued(kind, batch, pinned, owner)
                if claimed or pinned is None:
                    break
        return claimed

    def sweep_leases(self, now=None):
        """Return timed-out leases to the queue and drop owners of closed tasks.

        A session that dies mid-task leaves a lease behind. `next` calls this
        before refusing, so a dead session cannot block the queue past
        `TASK_LEASE_SECONDS`; the owner row of any task that is no longer
        `leased` is dropped here too, which keeps `task_leases` from growing
        historical rows for work that is already finished.
        """
        now = now or now_iso()
        with self.conn:
            self.conn.execute(
                "UPDATE tasks SET state = 'queued', lease_until = NULL "
                "WHERE state = 'leased' AND lease_until < ?",
                (now,),
            )
            self.conn.execute(
                "DELETE FROM task_leases "
                "WHERE task_id NOT IN (SELECT id FROM tasks WHERE state = 'leased')"
            )

    def lease_owner(self, task_id):
        """Session that holds this task, or '' when it was claimed before
        owners were recorded or by a session with no id."""
        row = self.conn.execute(
            "SELECT owner FROM task_leases WHERE task_id = ?", (task_id,)
        ).fetchone()
        return row["owner"] if row else ""

    def foreign_leases(self, owner):
        """Open leases held by another session, as [(task_id, owner)].

        Leases with no recorded owner are left out on purpose: an old or
        unattributed claim must stay finishable by anyone rather than block
        every session behind an owner nobody can name.
        """
        return [
            (task_id, held_by)
            for task_id, held_by in (
                (row["id"], self.lease_owner(row["id"])) for row in self.leased_tasks()
            )
            if held_by and held_by != owner
        ]

    def leased_tasks(self):
        """Tasks an agent has taken but not submitted or released yet."""
        rows = self.conn.execute(
            "SELECT rowid, * FROM tasks WHERE state = 'leased'"
        ).fetchall()
        return self._task_order(rows)

    def _claim_queued(self, kind, batch, tree=None, owner=""):
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
            if owner:
                # 没带会话 id 的调用不写归属：报错时照旧只说“有任务没交”，
                # 不会把别的会话的租约错认成自己的。
                self.conn.execute(
                    "INSERT OR REPLACE INTO task_leases(task_id, owner, leased_at) VALUES (?, ?, ?)",
                    (row["id"], owner, now_iso()),
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
