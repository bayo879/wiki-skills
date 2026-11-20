"""Workflow catalogue, contract and view-tag checks.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import shutil
import tempfile
from pathlib import Path

from support import check, run
from fixtures import write_workflow

# 引擎模块要等 support 把脚本目录加进 sys.path，因此排在 support 之后。
import ledger
import workflows


def workflow_catalogue(ctx):
    listing = run(ctx.root, "workflow", "list")["data"]
    check(ctx.results, "workflow list includes project and builtin entries",
          listing["count"] == 9 and listing["valid"] == 6 and listing["warned"] == 2, listing)
    check(ctx.results, "workflow list reports the workflows/ root only",
          listing["roots"] == ["workflows"] and listing["root"] == "workflows",
          listing.get("roots"))
    check(ctx.results, "workflow commands leave no .wiki", not (ctx.root / ".wiki").exists(), listing)
    check(ctx.results, "workflow tags follow the obsidian rules",
          workflows._tags(["#话题", "见证 开户", "2024", "a/b#c", "话题"])
          == ["话题", "见证", "开户", "a/b", "c"], workflows._tags(["#话题", "见证 开户"]))
    legacy_config = Path(tempfile.mkdtemp(prefix="llm-wiki-config-"))
    try:
        (legacy_config / ".wiki").mkdir()
        config_file = legacy_config / ".wiki" / "wiki.json"
        config_file.write_text('{"publish": {"scenarios": false}}', encoding="utf-8")
        migrated = ledger.load_config(config_file)["publish"]
        check(ctx.results, "publish drops seed switches",
              "topics" not in migrated and "workflows" not in migrated, migrated)
    finally:
        shutil.rmtree(legacy_config, ignore_errors=True)


def workflow_view_checks(ctx):
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-viewwf-"))
    try:
        view = root / "wiki/workflows"
        legacy = root / "workflows"
        write_workflow(view, "account-opening", "观看区开户", "观看区版本")
        write_workflow(legacy, "account-opening", "旧根开户", "旧版本")
        write_workflow(legacy, "legacy-only", "旧根流程")

        roots = [p.relative_to(root).as_posix()
                 for p in workflows.workflow_roots(root, view_root=view)]
        check(ctx.results, "workflow_roots puts view first",
              roots == ["wiki/workflows", "workflows"], roots)

        entries = workflows.scan(root, builtin=False, view_root=view)
        by_id = {item["id"]: item for item in entries}
        check(ctx.results, "view workflow wins over legacy",
              by_id["account-opening"]["origin"] == "view"
              and by_id["account-opening"]["path"] == "wiki/workflows/account-opening"
              and by_id["account-opening"]["main"]
              == "wiki/workflows/account-opening/WORKFLOW.md")
        check(ctx.results, "legacy conflict is marked",
              by_id["account-opening"]["legacy_conflict"] is True
              and by_id["account-opening"]["legacy_path"] == "workflows/account-opening/WORKFLOW.md")
        check(ctx.results, "legacy-only workflow keeps legacy origin",
              by_id["legacy-only"]["origin"] == "legacy")

        inv = workflows.inventory(root, view_root=view)
        check(ctx.results, "inventory reports relative roots",
              inv["root"] == "wiki/workflows"
              and inv["roots"] == ["wiki/workflows", "workflows"], inv)

        loaded = workflows.load(root, "account-opening", view_root=view)
        check(ctx.results, "load reads view workflow",
              loaded["workflow"]["main"] == "wiki/workflows/account-opening/WORKFLOW.md"
              and loaded["workflow"]["origin"] == "view", loaded["workflow"])

        matched = workflows.match(root, ["观看区"], 5, view_root=view)
        check(ctx.results, "workflow search finds view workflow",
              matched and matched[0]["id"] == "account-opening"
              and matched[0]["origin"] == "view", matched)

        write_workflow(view, "workflow-create", "观看区覆盖内置")
        shadowed = workflows.load(root, "workflow-create", view_root=view)
        check(ctx.results, "view workflow shadows builtin",
              shadowed["workflow"]["origin"] == "view", shadowed["workflow"])

        builtin = workflows.load(root, "topic-exploration", view_root=view)
        check(ctx.results, "builtin workflow keeps builtin origin",
              builtin["workflow"]["origin"] == "builtin", builtin["workflow"])
    finally:
        shutil.rmtree(root, ignore_errors=True)


def workflow_contracts(ctx):
    parsed, has_section = workflows.parse_steps(
        "## 步骤\n\n1. TOOL：**a**\n2. Human: **b**\n   - tool: 子项\n"
        "3. 没标\n\n## 其他\n\n- tool: 不算\n")
    check(ctx.results, "step markers accept a full-width colon and any case",
          has_section and [step["type"] for step in parsed] == ["tool", "human", None], parsed)
    builtin = run(ctx.root, "workflow", "load", "topic-exploration")["data"]
    check(ctx.results, "builtin workflow is loadable without project files",
          builtin["workflow"]["source"] == "builtin"
          and "path" not in builtin and "main" not in builtin
          and builtin["skill_path"] == "workflows/topic-exploration/WORKFLOW.md"
          and builtin["workflow"]["skill_path"] == "workflows/topic-exploration/WORKFLOW.md"
          and "话题文件" in builtin["content"]
          and builtin["workflow"]["name"] == "topic-exploration"
          and builtin["workflow"]["alias_name"] == "话题探索"
          and len(builtin["tools"]) == 4 and len(builtin["human_steps"]) == 1
          and builtin["resources"] == [], builtin)
    for name in ("workflow-review", "workflow-create"):
        entry = run(ctx.root, "workflow", "load", name)["data"]
        check(ctx.results, f"builtin {name} is loadable and self-contained",
              entry["workflow"]["source"] == "builtin"
              and "path" not in entry and "main" not in entry
              and entry["skill_path"] == f"workflows/{name}/WORKFLOW.md"
              and entry["workflow"]["skill_path"] == f"workflows/{name}/WORKFLOW.md"
              and entry["resources"] == []
              and len(entry["tools"]) >= 1, entry)
    check(ctx.results, "builtin workflow id cannot escape",
          run(ctx.root, "workflow", "load", "../x", expect=2)["error"]["code"] == "E_USAGE")
    loaded = run(ctx.root, "workflow", "load", "account-opening")["data"]
    check(ctx.results, "workflow load returns the flow and typed steps",
          "核验身份" in loaded["content"] and loaded["path"].endswith("WORKFLOW.md")
          and loaded["workflow"]["alias_name"] == ""
          and [step["text"] for step in loaded["tools"]] == ["**核验身份**：运行 `scripts/check.sh`"]
          and len(loaded["human_steps"]) == 1 and len(loaded["agent_steps"]) == 1
          and len(loaded["verify_steps"]) == 1 and loaded["warnings"] == [], loaded)
    check(ctx.results, "workflow load inventories support files",
          {item["kind"] for item in loaded["resources"]} == {"script", "reference"}, loaded["resources"])
    deck = run(ctx.root, "workflow", "load", "deck-flow")["data"]
    check(ctx.results, "workflows/ is the only workflow directory",
          deck["path"] == "workflows/deck-flow/WORKFLOW.md"
          and deck["workflow"]["name"] == "Deck flow", deck["path"])
    legacy = run(ctx.root, "workflow", "list")["data"]["workflows"]
    scenario_only = next(item for item in legacy if item["id"] == "scenario-only")
    check(ctx.results, "a bare SCENARIO.md is not a workflow",
          scenario_only["valid"] is False
          and scenario_only["error"] == "missing WORKFLOW.md", scenario_only)
    partial = run(ctx.root, "workflow", "load", "partial")["data"]
    check(ctx.results, "an untyped step only warns",
          partial["workflow"]["valid"]
          and [item["code"] for item in partial["warnings"]] == ["W_STEP_UNTYPED"], partial)
    manual = run(ctx.root, "workflow", "load", "manual", expect=2)
    check(ctx.results, "a typed workflow without tool steps is invalid",
          manual["error"]["code"] == "E_WORKFLOW_INVALID"
          and "process page" in manual["error"]["next"], manual)
