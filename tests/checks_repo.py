"""Dry run, drained queue, override and repository guard checks.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import re
import subprocess
import sys

from support import ROOT, SCRIPT, check, payload_file, route_query, run

# 引擎模块要等 support 把脚本目录加进 sys.path，因此排在 support 之后。
import wiki
import workflows


def dry_run(ctx):
    dry = ctx.root / "dry"
    dry.mkdir()
    (dry / "note.md").write_text("# 标题\n\n正文。\n", encoding="utf-8")
    dry_run = subprocess.run(
        [sys.executable, str(SCRIPT), "ingest", "--dry-run", "--json"],
        cwd=dry, capture_output=True, text=True,
    )
    check(ctx.results, "dry run leaves no .wiki", dry_run.returncode == 0 and not (dry / ".wiki").exists(), dry_run.stderr)

    # 队列清空后再摄入：queue 快照应显示没有未完成的工作，不再给出"去跑 next"的暗示。
    drained_root = ctx.root / "drained"
    drained_root.mkdir()
    (drained_root / "note.md").write_text("# 唯一来源\n\n正文。\n", encoding="utf-8")
    run(drained_root, "init")
    run(drained_root, "ingest")
    claimed = run(drained_root, "next", "--batch", "10")["data"]["tasks"]
    check(ctx.results, "drained fixture claims one route task", len(claimed) == 1, claimed)
    route_query(drained_root, claimed[0], "唯一来源")
    run(drained_root, "submit", claimed[0]["id"], "--payload", payload_file(
        drained_root, "skip.json",
        {"decisions": [{"source": "note.md", "action": "skip", "reason": "无进展，且无可沉淀的知识"}]}))
    drained = run(drained_root, "ingest")["data"]
    check(ctx.results, "a drained queue reports no open work",
          drained["queue"]["open"] == 0 and drained["queue"]["queued"] == 0
          and drained["queued_tasks"] == [], drained.get("queue"))
    empty = run(drained_root, "next", "--batch", "10")
    check(ctx.results, "next keeps reporting no work when nothing is open",
          empty["data"]["count"] == 0 and empty["summary"] == "no queued tasks", empty)

    override = ctx.root / "workflows" / "topic-exploration"
    override.mkdir()
    (override / "WORKFLOW.md").write_text(
        "---\nname: topic-exploration\ndescription: 项目自定义的话题流程\n---\n\n"
        "## 步骤\n\n1. tool: **检索**：`query`\n",
        encoding="utf-8",
    )
    overridden = run(ctx.root, "workflow", "load", "topic-exploration")["data"]
    check(ctx.results, "project workflow overrides the builtin",
          overridden["workflow"]["source"] == "project"
          and "项目自定义" in overridden["content"], overridden)


def repo_guards(ctx):
    healthy = run(ctx.root, "doctor", expect=(0, 1))["data"]
    check(ctx.results, "doctor runs", isinstance(healthy["issues"], list), healthy)

    wrappers = sorted(
        path.relative_to(ROOT).as_posix()
        for path in (ROOT / "skills").rglob("*")
        if path.is_file() and path.suffix.lower() in {".cmd", ".bat"}
    )
    check(ctx.results, "the skill package ships no .cmd/.bat wrapper", not wrappers, wrappers)

    skill_meta, _ = wiki.split_frontmatter(
        (ROOT / "skills" / "llm-wiki" / "SKILL.md").read_text(encoding="utf-8")
    )
    description = str(skill_meta.get("description") or "").strip()
    check(
        ctx.results,
        "SKILL.md description stays inside the 1024-character metadata budget",
        bool(description) and len(description) <= 1024,
        f"{len(description)} chars",
    )

    project_flows = [entry for entry in workflows.scan(ROOT) if entry["source"] == "project"]
    check(ctx.results, "repo's own project workflows are all valid",
          len(project_flows) >= 4 and all(entry["valid"] for entry in project_flows),
          [entry["id"] for entry in project_flows if not entry["valid"]])
    meeting = workflows.match(ROOT, ["会议", "纪要", "行动项"], 5)
    check(ctx.results, "meeting workflow tops the meeting search with applies",
          bool(meeting) and meeting[0]["id"] == "meeting-actions"
          and bool(meeting[0]["applies"]), [(w["id"], w["score"]) for w in meeting])

    teaching = [
        ROOT / "README.md",
        ROOT / "skills" / "llm-wiki" / "SKILL.md",
        *sorted((ROOT / "docs" / "readme").rglob("*.md")),
        *sorted((ROOT / "docs" / "agent").rglob("*.md")),
        *sorted((ROOT / "skills" / "llm-wiki" / "references").rglob("*.md")),
        *sorted((ROOT / "skills" / "llm-wiki" / "workflows").rglob("*.md")),
        *sorted((ROOT / "workflows").rglob("*.md")),
    ]
    stale = [
        f"{path.relative_to(ROOT).as_posix()}:{number}"
        for path in teaching
        for number, line in enumerate(
            path.read_text(encoding="utf-8").splitlines(), 1
        )
        if re.search(r'\bread\s+["“]', line)
    ]
    check(ctx.results, "no shipped doc teaches the deleted read command", not stale, stale)
