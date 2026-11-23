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

from pages import normalize_links, split_relations

DEFAULT_TYPE = "concept"
# 类型只决定章节骨架，由代码给、agent 填。`note` 已从默认 schema.types 移除，
# 但老库的 wiki.json 可能还留着它：保留这一条，让那些页面继续能读、能改，
# 不被悄悄套上 concept 的章节。
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


def raw_groups(sources):
    """Group task sources by raw: one decision covers every path of the same body."""
    groups = {}
    for item in sources:
        groups.setdefault(item.get("raw_id") or item["key"], []).append(item["key"])
    return list(groups.values())


def route_template(sources):
    return {
        "decisions": [
            {
                "source": group[0],
                "action": "",
                "page": "",
                "title": "",
                "type": "",
                "links": [],
                "reason": "",
            }
            for group in raw_groups(sources)
        ]
    }


def route_brief(task_id, sources, queries=None):
    groups = raw_groups(sources)
    keys = "、".join(group[0] for group in groups)
    chars = sum(item.get("chars") or 0 for item in sources)
    view = queries or {}
    used = view.get("used") or 0
    budget = view.get("budget") or 5
    mount_list = "；".join("、".join(group) for group in groups) or "无"
    return {
        "goal": f"读来源 {keys} 的全文，判断应归入哪些页面、或跳过；"
                "同一份正文只决定一次，决定覆盖它的全部路径。",
        "steps": [
            "读原文：`work.json` 的 `sources[].raw` 是观看区里的文件路径，直接打开读全文再抽词；"
            "长文分段读完，不要只看开头就定案。",
            f"抽主题词并登记：`query --task {task_id} 词1 词2 词3`。一次可给多个词，每个词独立"
            "检索、各返 5 条，可以分多次登记；配额按正文长度算，"
            f"当前已用 {used}/{budget} 个词（{chars} 字，5 个/1000 字，至少 5 个）。"
            "不登记查询就提交会报 `E_NO_QUERY`。词不必与原文逐字一致，只要能把主题分开。",
            "逐词看命中页的 title、summary 与出入链数；有同主题候选时用 `query --page <id>` 读"
            "covered/not_covered 再决定 merge。命中被截断时换词重查，不要把「没搜到」当成"
            "「没有相似页」。`pending_pages` 里已排队、尚未写入的页面同样可以 merge。",
            "填 `payload.json` 的 `decisions[]`：每份正文至少一条 decision，`source` 填它的任一"
            f"挂载路径（{mount_list}），这次决定覆盖该正文的全部路径；action 选 new/merge/skip；"
            "同一份正文含多个可独立检索的主题时拆成多条 decision、指向不同 page；"
            "每条 decision 都必须写 reason；new 还要给 page、title、type，merge 要给 page。"
            "类型只表示形态：要求与规则用 concept，组织/系统/人物用 entity，可复用的办理步骤用"
            "process；拿不准用 concept。同一主题的要求与步骤写同一页，不要拆两页；"
            "写不出一句「是什么/谁/怎么办」的 summary 就 skip，不要为了不丢内容硬建页。",
            "`links` 必须逐条判断：这页写完后，读者下一步最该看的 2–5 个页面；只填已存在页，"
            "或同一载荷里 new 的兄弟页；确实没有相关页就留空数组，不要为了凑数乱链。"
            "兄弟页要等被链页面先提交；顺序错会报 `E_LINK_UNRESOLVED`，重新领取后按顺序重试。",
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
        links = normalize_links(
            list(meta.get("links") or []) + list(inputs.get("links") or [])
        )
        return {
            "page": page_id,
            "title": meta.get("title") or page_id,
            "type": page_type,
            "summary": meta.get("summary") or "",
            "body": split_relations(record.get("body") or skeleton_body(page_type)),
            "links": links,
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
        "links": normalize_links(inputs.get("links") or []),
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
            "`links` 由 route 预填；写正文时再判断一次读者下一步该看哪页（2–5 个），只可新增"
            "已存在或本批已提交的页面 id。更新页会保留旧链接，只能新增；确实没有相关页就留空。"
            "被链页面还没提交时先提交它，再重试本任务。"
            "`boundaries` 写清这一页覆盖什么、不覆盖什么。",
        ],
        "submit": submit_step(task_id),
        "effect": "提交后引擎写入新修订并按账本推送到观看区；"
        "旧来源和旧证据会保留，本任务的新来源会加入来源列表。"
        "页面立即可 `query --page` 读到，来源后续变化会标记待更新。之后重新运行 `next` 取下一个任务。",
    }


def task_packet(task_id, kind, inputs, work):
    """Assemble the only task shape the agent sees."""
    if kind == "route":
        return {
            "id": task_id,
            "brief": route_brief(task_id, work["sources"], work.get("queries")),
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
