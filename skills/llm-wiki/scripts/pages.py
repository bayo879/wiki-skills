"""知识页的文本层：文件名、渲染、证据切分与发布失配判定。"""

from __future__ import annotations

import json
import re
from datetime import date

from frontmatter import yaml_scalar
from util import digest_text, norm_key, now_iso


VIEW_NAME_LIMIT = 60

VIEW_NAME_BAD = re.compile(r'[\\/:*?"<>|\x00-\x1f\x7f-\x9f]')

EVIDENCE_HEADING = "## 证据"

RELATION_HEADING = "## 关联"

EVIDENCE_ROW = re.compile(r"^- `([^`]+)`：(.+)$")

RELATION_HEADING_RE = re.compile(r"^##\s+关联\s*$")


def normalize_links(links):
    """Stable page-id list for a relation edge set; order never depends on input."""
    return sorted({str(item).strip().lower() for item in (links or []) if str(item).strip()})


def split_relations(body):
    """Drop every legacy ``## 关联`` section from a body.

    Relational text is engine-owned and rendered from ``meta.links``; a body
    section left by an older revision (or copied back from the view tree) must
    never become a second source of edges.
    """
    lines = body.split("\n")
    kept = []
    skipping = False
    for line in lines:
        if RELATION_HEADING_RE.match(line):
            skipping = True
            continue
        if skipping:
            if line.startswith("## "):
                skipping = False
            else:
                continue
        kept.append(line)
    return "\n".join(kept).rstrip()


def view_name(title, page_id):
    """Filesystem-safe page name for the published view tree."""
    text = VIEW_NAME_BAD.sub("-", str(title or ""))
    text = re.sub(r"\s+", " ", text)
    text = text.strip(" .-")
    if len(text) > VIEW_NAME_LIMIT:
        text = text[:VIEW_NAME_LIMIT].strip(" .-")
    return text or page_id


def render_page(meta, body, aliases=None, links=None):
    """Deterministic page text: frontmatter, body, evidence, then relations.

    Relations are rendered only when ``links`` is passed (the published view);
    the ledger's internal render stays relation-free so ``meta.links`` is the
    single stored edge set.
    """
    body = split_relations(body)
    lines = ["---"]
    for field in ("id", "title", "type", "summary"):
        lines.append(f"{field}: {yaml_scalar(meta[field])}")
        if field == "title" and aliases:
            lines.append(f"aliases: [{', '.join(str(alias) for alias in aliases)}]")
    lines.append("sources:")
    for source in meta["sources"]:
        lines.append(f"  - {yaml_scalar(source)}")
    lines.append(f"status: {yaml_scalar(meta['status'])}")
    lines.append(f"updated: {yaml_scalar(meta['updated'])}")
    lines.append("---")
    lines.append("")
    text = "\n".join(lines) + body.rstrip() + "\n"
    evidence = meta.get("evidence") or []
    if evidence:
        rows = ["", "## 证据", ""]
        for item in evidence:
            rows.append(f"- `{item['source']}`：{item['quote']}")
        text += "\n".join(rows) + "\n"
    if links:
        rows = ["", RELATION_HEADING, ""]
        rows.extend(f"- [[{link}]]" for link in normalize_links(links))
        text += "\n".join(rows) + "\n"
    return text


def split_evidence(body):
    """把已发布正文末尾的 `## 证据` 段切下来（R5）。

    返回 (正文, 行列表)。末尾标题之后只要有一行不是 ``- `source`：quote``，就整段留在
    正文里、返回空列表：调用方沿用账本证据并打 warning。空行忽略。
    """
    lines = body.split("\n")
    start = None
    for index in range(len(lines) - 1, -1, -1):
        if lines[index].strip() == EVIDENCE_HEADING:
            start = index
            break
    if start is None:
        return body, []
    rows = []
    for line in lines[start + 1:]:
        if not line.strip():
            continue
        match = EVIDENCE_ROW.match(line)
        if not match:
            return body, []
        rows.append({"source": match.group(1).strip(), "quote": match.group(2).strip()})
    if not rows:
        return body, []
    return "\n".join(lines[:start]).rstrip(), rows


def meta_mismatch(file_meta, record_meta):
    """观看区手改动了哪些元数据字段；只改正文时返回空列表（R4）。

    id/status/updated/aliases 不参与比较：id 决定归位，后三者是渲染出来的，改了也无害。
    """
    changed = [
        field for field in ("title", "type", "summary")
        if str(file_meta.get(field) or "") != str(record_meta.get(field) or "")
    ]
    if set(file_meta.get("sources") or []) != set(record_meta.get("sources") or []):
        changed.append("sources")
    return changed


class PagesMixin:
    """页面记录、修订与链接索引；由 Wiki 组合，不自带初始化。"""

    def latest_rev_row(self, page_id):
        return self.conn.execute(
            "SELECT * FROM page_revs WHERE page = ? ORDER BY rev DESC LIMIT 1",
            (page_id,),
        ).fetchone()

    def page_record(self, page_id):
        row = self.latest_rev_row(page_id)
        if not row:
            return None
        return self._page_row_to_record(row)

    def _page_row_to_record(self, row):
        """One ledger row as a page record, including the published view facts."""
        meta = json.loads(row["meta"])
        # Older revisions stored edges only in the links index. Read them back
        # once so every later write migrates the page to meta.links naturally.
        meta.setdefault("links", self.links_for(row["page"])[0])
        meta["status"] = self.computed_status(meta)
        return {
            "id": row["page"],
            "rev": row["rev"],
            "meta": meta,
            "body": row["body"],
            "digest": row["digest"],
            "render_digest": row["render_digest"],
            "origin": row["origin"],
            "view_digest": row["view_digest"],
            "view_path": row["view_path"],
        }

    def all_pages(self):
        rows = self.conn.execute(
            """
            SELECT r.* FROM page_revs r
            WHERE r.rev = (SELECT MAX(x.rev) FROM page_revs x WHERE x.page = r.page)
            ORDER BY r.page
            """
        ).fetchall()
        return [self._page_row_to_record(row) for row in rows]

    def page_ids(self):
        return {
            row["page"]
            for row in self.conn.execute("SELECT DISTINCT page FROM page_revs")
        }

    def computed_status(self, meta):
        """A page is stale when any cited source changed or vanished."""
        snapshot = meta.get("source_digests") or {}
        for key in meta.get("sources") or []:
            row = self.source_row(key)
            if row is None or row["status"] in ("missing", "unsupported", "oversize"):
                return "stale"
            if snapshot.get(key) != row["raw_digest"]:
                return "stale"
        return "stable"

    def page_ref(self, record):
        """Where `read`/`query` point at a ledger page: the view copy, or `page:<id>`."""
        if not self.config["publish"]["enabled"]:
            return f"page:{record['id']}"
        target = self.publish_dir / self.view_relpath(record)
        return norm_key(target.relative_to(self.root))

    def view_relpath(self, record):
        """Publish path for one page record, relative to the publish dir."""
        meta = record["meta"]
        page_id = record["id"]
        base = view_name(meta.get("title"), page_id)
        relpath = f"pages/{meta['type']}/{base}.md"
        collision = False
        for other in self.all_pages():
            if other["id"] == page_id:
                continue
            if other["meta"].get("type") != meta.get("type"):
                continue
            if view_name(other["meta"].get("title"), other["id"]) == base:
                collision = True
                break
        if not collision:
            row = self.latest_rev_row(page_id)
            stored = row["view_path"] if row else ""
            collision = (
                (self.publish_dir / relpath).exists() and stored != relpath
            )
        if collision:
            relpath = f"pages/{meta['type']}/{base} ({page_id}).md"
        return relpath

    def view_text(self, record):
        """Page text as published in the view tree: same template, id alias.

        Relations are outgoing and incoming edges mixed together: a reader who
        lands on an older page still sees newer pages that point at it.
        """
        outgoing, incoming = self.links_for(record["id"])
        links = sorted(set(outgoing) | set(incoming))
        return render_page(
            record["meta"], record["body"], aliases=[record["id"]], links=links
        )

    def write_rev(self, page, meta, body, origin):
        record = self.page_record(page)
        rev = (record["rev"] if record else 0) + 1
        body = split_relations(body)
        meta = dict(meta)
        meta["id"] = page
        # Add-only at the storage boundary: no caller (submit, import, restore,
        # sync) can silently drop an edge, because there is no delete command.
        existing = (record or {}).get("meta", {}).get("links") or []
        links = normalize_links(list(meta.get("links") or []) + list(existing))
        meta["links"] = [link for link in links if link != page]
        meta["sources"] = sorted(set(meta.get("sources") or []))
        meta["source_digests"] = {
            key: (self.source_row(key)["raw_digest"] if self.source_row(key) else "")
            for key in meta["sources"]
        }
        meta["updated"] = date.today().isoformat()
        meta["status"] = self.computed_status(meta)
        text = render_page(meta, body)
        render_digest = digest_text(text)
        # R1：view_digest/view_path 是"上一次发布出去的事实"，不是本行文本的摘要。
        # 新行先继承上一版；只有 sync_view 把文件写成功之后才改它。
        view_digest = record["view_digest"] if record else ""
        view_path = record["view_path"] if record else ""
        canonical = json.dumps(
            {
                "title": meta.get("title"),
                "type": meta.get("type"),
                "summary": meta.get("summary"),
                "sources": meta["sources"],
                "links": meta["links"],
                "evidence": meta.get("evidence") or [],
                "boundaries": meta.get("boundaries") or {},
                "body": body,
            },
            sort_keys=True,
            ensure_ascii=False,
        )
        with self.conn:
            self.conn.execute(
                """
                INSERT INTO page_revs
                (page, rev, meta, body, digest, render_digest, view_digest, view_path,
                 origin, accepted_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    page, rev, json.dumps(meta, ensure_ascii=False, sort_keys=True),
                    body, digest_text(canonical), render_digest, view_digest, view_path,
                    origin, now_iso(),
                ),
            )
            self.rebuild_links(page, meta["links"])
        return rev

    def rebuild_links(self, page_id, links):
        with self.conn:
            self.conn.execute("DELETE FROM links WHERE src = ?", (page_id,))
            for target in normalize_links(links):
                if target == page_id:
                    continue
                self.conn.execute(
                    "INSERT OR IGNORE INTO links (src, dst) VALUES (?, ?)",
                    (page_id, target),
                )

    def rebuild_all_links(self):
        # Read the fallback (old rows have no meta.links) before deleting the
        # index, otherwise the snapshot used for the rebuild would be empty.
        records = self.all_pages()
        with self.conn:
            self.conn.execute("DELETE FROM links")
        for record in records:
            self.rebuild_links(record["id"], record["meta"].get("links") or [])

    def links_for(self, page_id):
        outgoing = [
            row["dst"] for row in self.conn.execute(
                "SELECT dst FROM links WHERE src = ? ORDER BY dst", (page_id,)
            )
        ]
        incoming = [
            row["src"] for row in self.conn.execute(
                "SELECT src FROM links WHERE dst = ? ORDER BY src", (page_id,)
            )
        ]
        return outgoing, incoming
