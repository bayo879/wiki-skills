"""`workflow list/search/load` 命令。"""

from __future__ import annotations

import workflows
from pathlib import Path

from cli_common import ARGS, emit
from errors import WikiError
from ledger import view_dir
from workspace import window_safe


def command_workflow():
    workspace = Path.cwd().resolve()
    root = Path(ARGS.root).resolve() if ARGS.root else workspace
    if not window_safe(workspace, root):
        raise WikiError(
            "E_PATH", "workflow root escapes the workspace",
            "run from the workspace, or pass --root inside it", 2,
        )
    view_root = view_dir(root) / workflows.WORKFLOW_DIR
    if ARGS.workflow_command == "search":
        terms = [term.strip() for term in ARGS.text if term.strip()]
        if not terms:
            raise WikiError(
                "E_USAGE", "workflow search needs text",
                'example: workflow search "开户"', 2,
            )
        limit = min(5, max(1, ARGS.limit))
        hits = workflows.match(root, terms, limit, view_root=view_root)
        data = {"query": " ".join(terms), "matches": len(hits), "workflows": hits}
        if not hits:
            data["message"] = "无可用workflow"
        lines = []
        for item in hits:
            alias = f"（{item['alias_name']}）" if item["alias_name"] else ""
            lines.append(f"{item['id']}{alias}  {item['description']}")
            where = (
                f"skill_path: {item['skill_path']}"
                if item["source"] == "builtin"
                else f"path: {item['path']}"
                + ("  [legacy]" if item["origin"] == "legacy" else "")
            )
            lines.append(
                f"    {where}  tools: {item['tools']}"
                f"  human: {item['human_steps']}  load: {item['load']}"
            )
            lines.append(f"    applies: {item['applies'] or '(none)'}")
        if not lines:
            lines.append("无可用workflow")
        emit("workflow search", root, f"{len(hits)} matching workflows", data, lines)
        return 0
    if ARGS.workflow_command == "list":
        data = workflows.inventory(root, view_root=view_root)
        summary = f"{data['valid']} valid, {data['count'] - data['valid']} invalid workflows"
        if data["warned"]:
            summary += f", {data['warned']} with warnings"
        lines = []
        for item in data["workflows"]:
            state = "ok" if item["valid"] else f"invalid: {item['error']}"
            alias = f"（{item['alias_name']}）" if item["alias_name"] else ""
            lines.append(f"{item['id']}{alias}  [{state}]  {item['description']}")
            where = (
                f"skill_path: {item['skill_path']}"
                if item["source"] == "builtin"
                else f"path: {item['path']}"
                + ("  [legacy]" if item["origin"] == "legacy" else "")
            )
            detail = (
                f"    {where}  resources: {len(item['resources'])}"
                f"  tools: {item['tools']}  human: {item['human_steps']}"
            )
            if item["warnings"]:
                detail += f"  warnings: {len(item['warnings'])}"
            lines.append(detail)
            for warning in item["warnings"]:
                where = f"line {warning['line']}: " if warning["line"] else ""
                lines.append(f"    ! {where}{warning['message']}")
            if item.get("legacy_conflict"):
                lines.append(
                    f"    ! 观看区版本生效，旧根目录版本未加载：{item['legacy_path']}"
                )
        emit("workflow list", root, summary, data, lines)
        return 0
    data = workflows.load(root, ARGS.id, view_root=view_root)
    record = data["workflow"]
    emit(
        "workflow load", root,
        f"workflow {record['id']}: {len(data['resources'])} resources, "
        f"{len(data['tools'])} tool steps, {len(data['human_steps'])} human steps",
        data,
        [
            (
                f"skill_path: {data['skill_path']}"
                if record["source"] == "builtin"
                else f"path: {data['path']}"
                + ("  [legacy]" if record["origin"] == "legacy" else "")
            ),
            f"description: {record['description']}",
            f"steps: {len(data['tools'])} tool, {len(data['human_steps'])} human, "
            f"{len(data['agent_steps'])} agent, {len(data['verify_steps'])} verify",
            "",
            data["content"],
        ],
    )
    return 0
