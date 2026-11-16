"""Agent-facing task packets.

The engine owns the flow: it decides which task exists, what it may touch, and
what happens after a submit. This module only turns a claimed task into three
things the agent needs:

- a brief: what to do, how to do it, and what the submit changes;
- a pre-filled template: the structure the agent is allowed to fill;
- nothing about leases, epochs, retries, or task state.

Keeping the packet here also keeps the engine's main module under its size
budget while making the task-design rules easy to review in one place.
"""

from __future__ import annotations

import re

DEFAULT_TYPE = "concept"
PAGE_SECTIONS = {
    "concept": ("结论", "要点"),
    "entity": ("结论", "关键信息"),
    "process": ("结论", "步骤", "注意事项"),
    "note": ("要点",),
}
HEADING_RE = re.compile(r"^##\s+(.+?)\s*$", re.MULTILINE)


def sections_for(page_type):
    """Fixed section order for a page type; code decides, the agent fills."""
    return PAGE_SECTIONS.get(page_type, PAGE_SECTIONS[DEFAULT_TYPE])


def skeleton_body(page_type):
    """The body skeleton a create task starts from."""
    return "".join(f"## {name}\n\n" for name in sections_for(page_type))


def missing_sections(body, page_type):
    """Return the required sections absent or out of order in ``body``."""
    present = [match.group(1).strip() for match in HEADING_RE.finditer(body)]
    cursor = 0
    missing = []
    for name in sections_for(page_type):
        try:
            cursor = present.index(name, cursor) + 1
        except ValueError:
            missing.append(name)
    return missing


def submit_step(task_id):
    path = f".wiki/payloads/{task_id}.json"
    return f"把填好的 template 存到 {path}，运行 `submit {task_id} --payload {path}`"


def route_template(sources):
    return {
        "decisions": [
            {
                "source": item["key"],
                "action": "",
                "page": "",
                "title": "",
                "type": "",
                "reason": "",
            }
            for item in sources
        ]
    }


def route_brief(task_id, sources):
    keys = "、".join(item["key"] for item in sources)
    first = sources[0]["key"] if sources else ""
    return {
        "goal": f"判断来源 {keys} 应归入哪一页知识，或跳过。",
        "steps": [
            f"读原文：运行 `read \"raw:{first}\" --offset 1 --limit 120 --json`。",
            "在 work.page_index 里找同主题的已有页面；已有页面能覆盖时优先 merge。",
            "填 template.decisions[0]：action 选 new/merge/skip；"
            "new 还要给 page、title、type，merge 要给 page，skip 要给 reason。",
        ],
        "submit": submit_step(task_id),
        "effect": "提交后引擎自动创建或合并页面写作任务；skip 只留记录，不建页。之后重新运行 `next` 取下一个任务。",
    }


def page_template(inputs, record):
    page_id = inputs["page"]
    if record:
        meta = record.get("meta") or {}
        page_type = meta.get("type") or DEFAULT_TYPE
        return {
            "page": page_id,
            "title": meta.get("title") or page_id,
            "type": page_type,
            "summary": meta.get("summary") or "",
            "body": record.get("body") or skeleton_body(page_type),
            "links": [],
            "evidence": [{"source": item["key"], "quote": ""} for item in inputs["sources"]],
            "boundaries": meta.get("boundaries") or {"covered": "", "not_covered": ""},
        }
    page_type = inputs.get("type") or DEFAULT_TYPE
    return {
        "page": page_id,
        "title": inputs.get("title") or page_id,
        "type": page_type,
        "summary": "",
        "body": skeleton_body(page_type),
        "links": [],
        "evidence": [{"source": item["key"], "quote": ""} for item in inputs["sources"]],
        "boundaries": {"covered": "", "not_covered": ""},
    }


def page_brief(task_id, inputs, record):
    page_id = inputs["page"]
    action = "更新" if record else "新建"
    return {
        "goal": f"{action}页面 {page_id} 的正文。",
        "steps": [
            "先读 work.sources 里每份 raw 原文；只写原文支持的事实。",
            "按 template.body 已有的章节填写，不要改章节名，也不要删章节。",
            "给 template.evidence 的每条来源填一句逐字 quote；"
            "数字、日期、文号、责任主体都要能回到原文。",
            "links 只填 work.page_index 里已有的页面 id；"
            "boundaries 写清这一页覆盖什么、不覆盖什么。",
        ],
        "submit": submit_step(task_id),
        "effect": "提交后引擎写入新修订并更新 .wiki/pages 镜像；"
        "页面立即可 query/read，来源后续变化会标记待更新。之后重新运行 `next` 取下一个任务。",
    }


def task_packet(task_id, kind, inputs, work):
    """Assemble the only task shape the agent sees."""
    if kind == "route":
        return {
            "id": task_id,
            "brief": route_brief(task_id, work["sources"]),
            "template": route_template(work["sources"]),
            "work": work,
        }
    record = work.get("page")
    return {
        "id": task_id,
        "brief": page_brief(task_id, inputs, record),
        "template": page_template(inputs, record),
        "work": work,
    }
