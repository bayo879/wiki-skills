"""知识页的文本层：文件名、渲染、证据切分与发布失配判定。"""

from __future__ import annotations

import re

from frontmatter import yaml_scalar
from util import digest_text


VIEW_NAME_LIMIT = 60


VIEW_NAME_BAD = re.compile(r'[\\/:*?"<>|\x00-\x1f\x7f-\x9f]')


EVIDENCE_HEADING = "## 证据"


EVIDENCE_ROW = re.compile(r"^- `([^`]+)`：(.+)$")


LINK_RE = re.compile(r"\[\[([^\]|#]+)(?:#[^\]|]*)?(?:\|[^\]]*)?\]\]")


def view_name(title, page_id):
    """Filesystem-safe page name for the published view tree."""
    text = VIEW_NAME_BAD.sub("-", str(title or ""))
    text = re.sub(r"\s+", " ", text)
    text = text.strip(" .-")
    if len(text) > VIEW_NAME_LIMIT:
        text = text[:VIEW_NAME_LIMIT].strip(" .-")
    return text or page_id


def render_page(meta, body, aliases=None):
    """Deterministic page text: frontmatter, body, then the evidence block."""
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
