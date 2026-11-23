"""原文（raw）读写：命名、挂载、手改采纳与来源脚注刷新。

`Wiki` 的组合来源之一；来源行读写见 sources.py。"""

from __future__ import annotations

import re
from datetime import date

from frontmatter import split_frontmatter
from pages import render_page
from util import body_digest, norm_key, split_provenance, write_atomic


class RawMixin:
    """原文（raw）：命名、挂载、手改采纳与脚注刷新；由 Wiki 组合。"""

    def _raw_name_taken(self, name, raw_id):
        row = self.conn.execute(
            "SELECT id FROM raws WHERE name = ?", (name,)
        ).fetchone()
        return row is not None and row["id"] != raw_id

    def _raw_name(self, key, raw_id):
        """First-seen basename of the source key, unique among raw rows."""
        base = key.rsplit("/", 1)[-1]
        base = re.sub(r"\.[A-Za-z0-9]+$", "", base)
        base = re.sub(r"[^\w.-]+", "-", base, flags=re.UNICODE).strip("-._") or "source"
        base = base[:80]
        candidate, ordinal = base, 2
        while self._raw_name_taken(candidate, raw_id):
            candidate = f"{base}-{ordinal}"
            ordinal += 1
        return candidate

    def raw_mounts(self, raw_id):
        return [
            row["key"] for row in self.conn.execute(
                "SELECT key FROM sources WHERE raw_id = ? ORDER BY key", (raw_id,)
            )
        ]

    def attach_raw(self, key, text, parser, title):
        """Store one parsed body as a raw copy; return (path, id).

        The body digest is the identity: the same text always lands on one row
        and one file, so a hundred copies of a document leave one raw copy
        mounted by many sources. When the previous raw of this key had no other
        mount, the row is replaced in place and keeps its filename.
        """
        raw_id = body_digest(text)
        today = date.today().isoformat()
        row = self.conn.execute("SELECT * FROM raws WHERE id = ?", (raw_id,)).fetchone()
        if row is not None:
            name, rel = row["name"], row["path"]
        else:
            current = self.source_row(key)
            previous = None
            if current is not None and current["raw_id"]:
                previous = self.conn.execute(
                    "SELECT * FROM raws WHERE id = ?", (current["raw_id"],)
                ).fetchone()
            if previous is not None and self.raw_mounts(previous["id"]) == [key]:
                # 只有这一个源，换内容就是原地换行：文件名跟着这份内容继续用。
                name, rel = previous["name"], previous["path"]
                with self.conn:
                    self.conn.execute("DELETE FROM raws WHERE id = ?", (previous["id"],))
            else:
                name = self._raw_name(key, raw_id)
                rel = norm_key(
                    (self.publish_raw_dir / f"{name}.md").relative_to(self.root)
                )
        mounts = self.raw_mounts(raw_id)
        if key not in mounts:
            mounts = sorted(mounts + [key])
        meta = {
            "id": raw_id,
            "title": title,
            "type": "raw",
            "summary": f"parsed from {', '.join(mounts)} by {parser}",
            "sources": mounts,
            "status": "raw",
            "updated": today,
        }
        target = self.root / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        write_atomic(target, render_page(meta, text, view_path=rel))
        size, mtime = target.stat().st_size, target.stat().st_mtime
        with self.conn:
            if row is None:
                self.conn.execute(
                    """
                    INSERT INTO raws (id, name, path, parser, size, mtime, updated_at)
                    VALUES (?, ?, ?, ?, ?, ?, ?)
                    """,
                    (raw_id, name, rel, parser, size, mtime, today),
                )
            else:
                self.conn.execute(
                    "UPDATE raws SET size = ?, mtime = ?, updated_at = ? WHERE id = ?",
                    (size, mtime, today, raw_id),
                )
            # 一份 raw 可能被多个源挂着：文件的 size/mtime 变了，每个挂载源
            # 记的原文事实都要一起对齐，否则下轮 ingest 会把共享者误判成「人工改过」。
            self.conn.execute(
                "UPDATE sources SET raw_digest = ?, raw_size = ?, raw_mtime = ? "
                "WHERE raw_id = ?",
                (raw_id, size, mtime, raw_id),
            )
        return rel, raw_id

    def refresh_raw_provenance(self):
        """Give every published raw copy a current page → raw → source footer.

        Raw files are owned by ingest, not sync.  Existing copies written before
        the footer existed are repaired here; the body digest ignores the footer,
        so this is display-only and never makes a citing page stale.
        """
        if not self.config["publish"]["enabled"]:
            return 0
        today = date.today().isoformat()
        changed = 0
        for row in self.conn.execute("SELECT id, path FROM raws ORDER BY id").fetchall():
            target = self.root / row["path"]
            try:
                text = target.read_text(encoding="utf-8", errors="replace")
            except OSError:
                continue
            # 已有脚注就不再重排：用户可能在后记里手写了内容，ingest 不能改原文。
            if "<!-- llm-wiki:provenance -->" in text:
                continue
            meta, body = split_frontmatter(text)
            if not meta.get("id") or not meta.get("title"):
                continue
            body, _ = split_provenance(body)
            mounts = sorted(
                item["key"] for item in self.conn.execute(
                    "SELECT key FROM sources WHERE raw_id = ? AND raw <> ''", (row["id"],)
                ).fetchall()
            )
            meta["sources"] = mounts
            desired = render_page(meta, body, view_path=row["path"])
            if desired == text:
                continue
            write_atomic(target, desired)
            stat = target.stat()
            with self.conn:
                self.conn.execute(
                    "UPDATE raws SET size = ?, mtime = ?, updated_at = ? WHERE id = ?",
                    (stat.st_size, stat.st_mtime, today, row["id"]),
                )
                self.conn.execute(
                    "UPDATE sources SET raw_size = ?, raw_mtime = ?, updated_at = ? "
                    "WHERE raw_id = ?",
                    (stat.st_size, stat.st_mtime, today, row["id"]),
                )
            changed += 1
        return changed

    def adopt_raw_edit(self, key, parser):
        """Adopt a hand-edited raw for every source path that mounts it.

        A raw row is one body however many source keys share it, so a hand edit
        belongs to the whole document: all mounts move to the new digest in one
        step, and the file keeps its name and path. The user's own bytes stay
        exactly as written — the ledger is the fact source, and the frontmatter
        inside a hand-edited raw is whatever the parse wrote back then.
        """
        row = self.source_row(key)
        if row is None or not row["raw"]:
            return "", ""
        old_id = row["raw_id"]
        target = self.root / row["raw"]
        try:
            _, body = split_frontmatter(target.read_text(encoding="utf-8", errors="replace"))
        except OSError:
            return row["raw"], old_id
        body, _ = split_provenance(body)
        raw_id = body_digest(body)
        if raw_id == old_id:
            return row["raw"], raw_id
        # 空 raw_id 只可能是没有原文的来源，不能拿它反查挂载，否则会把别人一起改掉。
        mounts = self.raw_mounts(old_id) if old_id else [key]
        today = date.today().isoformat()
        known = self.conn.execute(
            "SELECT * FROM raws WHERE id = ?", (raw_id,)
        ).fetchone()
        previous = None
        if known is not None:
            # 同样的正文已经在别处落过一份：整份文档并过去，旧行没有挂载后由
            # collect_raws 连同旧文件一起回收。
            rel = known["path"]
            known_file = self.root / rel
            try:
                stat = known_file.stat()
                size, mtime = stat.st_size, stat.st_mtime
            except OSError:
                size, mtime = int(known["size"]), float(known["mtime"])
        else:
            if old_id:
                previous = self.conn.execute(
                    "SELECT * FROM raws WHERE id = ?", (old_id,)
                ).fetchone()
            stat = target.stat()
            if previous is not None:
                name, rel = previous["name"], previous["path"]
            else:
                name = self._raw_name(key, raw_id)
                rel = norm_key((self.publish_raw_dir / f"{name}.md").relative_to(self.root))
            size, mtime = stat.st_size, stat.st_mtime
        with self.conn:
            if known is None:
                if previous is not None:
                    self.conn.execute("DELETE FROM raws WHERE id = ?", (old_id,))
                self.conn.execute(
                    """
                    INSERT INTO raws (id, name, path, parser, size, mtime, updated_at)
                    VALUES (?, ?, ?, ?, ?, ?, ?)
                    """,
                    (raw_id, name, rel, parser, size, mtime, today),
                )
            marks = ",".join("?" for _ in mounts)
            self.conn.execute(
                f"UPDATE sources SET raw = ?, raw_id = ?, raw_digest = ?, raw_size = ?, "
                f"raw_mtime = ?, updated_at = ? WHERE key IN ({marks})",
                (rel, raw_id, raw_id, size, mtime, today, *mounts),
            )
        # 采纳后的原文就是账本事实：仍在排队或租用的任务快照要一起对齐，
        # 否则同一份原文会被自己的任务判成漂移。
        for mount in mounts:
            self.pin_open_task_raw(mount, rel, raw_id, size, mtime)
        return rel, raw_id

    def collect_raws(self):
        """Drop raw rows no source points at, and their files."""
        dropped = [
            row for row in self.conn.execute(
                """
                SELECT * FROM raws
                WHERE id NOT IN (SELECT raw_id FROM sources WHERE raw_id <> '')
                """
            )
        ]
        for row in dropped:
            with self.conn:
                self.conn.execute("DELETE FROM raws WHERE id = ?", (row["id"],))
            target = self.root / row["path"]
            if target.exists():
                target.unlink()
        return [row["path"] for row in dropped]
