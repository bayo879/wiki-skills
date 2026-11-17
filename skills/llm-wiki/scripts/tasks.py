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


TASK_DIR = "tasks"


def task_dir_relative(task_id):
    """Where a leased task keeps its brief, payload skeleton, and work data."""
    return f".wiki/{TASK_DIR}/{task_id}"


def submit_step(task_id):
    return f"把填好的 payload.json 留在本目录，运行 `submit {task_id}`"


def render_brief(task_id, brief):
    """The agent-facing brief as Markdown, next to the payload it fills."""
    lines = [
        f"# {brief['goal']}",
        "",
        f"本目录 `{task_dir_relative(task_id)}/`：`payload.json` 是本次提交内容，"
        "`work.json` 是来源与页面索引。",
        "",
        "## 步骤",
        "",
    ]
    for index, step in enumerate(brief["steps"], 1):
        lines.append(f"{index}. {step}")
    lines += ["", "## 提交", "", brief["submit"], "", "## 提交后", "", brief["effect"], ""]
    return "\n".join(lines)


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
        "goal": f"判断来源 {keys} 应归入哪些页面，或跳过；一份来源可拆到多页。",
        "steps": [
            f"读原文：运行 `read \"raw:{first}\" --offset 1 --limit 120 --json`。",
            "在 `work.json` 的 `page_index` 里找同主题的已有页面；已有页面能覆盖时优先"
            " merge。`pending_pages` 是已排队、尚未写入的页面，同主题也可以 merge 到它的 id。",
            "填 `payload.json` 的 `decisions[]`：每个来源至少一条 decision，action 选 new/merge/skip；"
            "一份文件含多个可独立检索的主题时拆成多条 decision、指向不同 page；"
            "每条 decision 都必须写 reason；new 还要给 page、title、type，merge 要给 page。",
        ],
        "submit": submit_step(task_id),
        "effect": "提交后引擎自动创建或合并页面写作任务；skip 只留记录，不建页。之后重新运行 `next` 取下一个任务。",
    }


def page_template(inputs, record):
    page_id = inputs["page"]
    if record:
        meta = record.get("meta") or {}
        page_type = meta.get("type") or DEFAULT_TYPE
        evidence = [dict(item) for item in (meta.get("evidence") or []) if isinstance(item, dict)]
        cited = {item.get("source") for item in evidence}
        evidence.extend(
            {"source": item["key"], "quote": ""}
            for item in inputs["sources"] if item["key"] not in cited
        )
        return {
            "page": page_id,
            "title": meta.get("title") or page_id,
            "type": page_type,
            "summary": meta.get("summary") or "",
            "body": record.get("body") or skeleton_body(page_type),
            "links": [],
            "evidence": evidence,
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
            "先读 `work.json` 的 `sources` 里每份 raw 原文；只写原文支持的事实。",
            "按 `payload.json` 的 `body` 已有章节填写，不要改章节名，也不要删章节。",
            "`summary` 写 1-200 字的一句话；更新已有页面时，`evidence` 里旧来源的"
            "证据要保留，不要删。",
            "给 `evidence` 的每条来源填一到两句逐字 `quote`；"
            "数字、日期、文号、责任主体都要能回到原文。",
            "`links` 只填 `work.json` 里 `page_index` 已有的页面 id；"
            "`boundaries` 写清这一页覆盖什么、不覆盖什么。",
        ],
        "submit": submit_step(task_id),
        "effect": "提交后引擎写入新修订并按账本推送到观看区；"
        "旧来源和旧证据会保留，本任务的新来源会加入来源列表。"
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
