"""Shared temp-workspace fixtures: sample workflows and topics.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

from pathlib import Path


def write_workflow(base, name, description, body="## 步骤\n\n1. tool: **运行**\n"):
    directory = base / name
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "WORKFLOW.md").write_text(
        f"---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n\n"
        "## 步骤\n\n1. tool: **运行**\n\n"
        f"{body}",
        encoding="utf-8")
    return directory


def write_topic(base, name, title, description, body=None):
    base.mkdir(parents=True, exist_ok=True)
    text = body if body is not None else f"## 结论\n\n{description}\n"
    (base / name).write_text(
        f"---\ntitle: {title}\ndescription: {description}\nstatus: open\n---\n\n"
        f"{text}",
        encoding="utf-8")


def workflow_files(root):
    base = Path(root) / "workflows"
    good = base / "account-opening"
    (good / "scripts").mkdir(parents=True)
    (good / "references").mkdir(parents=True)
    main = good / "WORKFLOW.md"
    main.write_text(
        "---\n"
        "name: 开户流程\n"
        "description: 新客户开户时核验身份并登记资料\n"
        "tags:\n"
        "  - 开户\n"
        "  - 新客户\n"
        "---\n"
        "# 开户流程\n\n"
        "## 适用\n\n"
        "新客户首次开户，且需要核验身份。\n\n"
        "## 步骤\n\n"
        "1. tool: **核验身份**：运行 `scripts/check.sh`\n"
        "2. human: **收资料**：请客户签字确认\n"
        "3. agent: **登记资料**：写入客户档案\n"
        "4. verify: **核对**：检查资料齐全\n"
        "   - tool: 缩进子项不是步骤\n",
        encoding="utf-8",
    )
    (good / "scripts/check.sh").write_text("echo check\n", encoding="utf-8")
    (good / "references/notes.md").write_text("开户材料说明\n", encoding="utf-8")
    broken = base / "broken"
    broken.mkdir()
    (broken / "WORKFLOW.md").write_text(
        "---\nname: 坏流程\n---\n\n缺少 description。\n", encoding="utf-8"
    )
    manual = base / "manual"
    manual.mkdir()
    (manual / "WORKFLOW.md").write_text(
        "---\nname: 手工流程\ndescription: 只有人工步骤\n---\n\n"
        "## 步骤\n\n1. human: **签字**\n2. agent: **归档**\n",
        encoding="utf-8",
    )
    partial = base / "partial"
    partial.mkdir()
    (partial / "WORKFLOW.md").write_text(
        "---\nname: 混写流程\ndescription: 有一步没标类型\n---\n\n"
        "## 步骤\n\n1. tool: **取证**\n2. 复核\n",
        encoding="utf-8",
    )
    deck = base / "deck-flow"
    deck.mkdir()
    (deck / "WORKFLOW.md").write_text(
        "---\nname: Deck flow\ndescription: Flow stored under the workflow directory\n---\n\n"
        "## 步骤\n\n1. tool: **draft**\n2. human: **hand off**\n",
        encoding="utf-8",
    )
    scenario_only = base / "scenario-only"
    scenario_only.mkdir()
    (scenario_only / "SCENARIO.md").write_text(
        "---\nname: 旧流程\ndescription: 只剩旧文件名\n---\n\n"
        "# 旧流程\n\n## 步骤\n\n1. tool: **旧步骤**\n",
        encoding="utf-8",
    )
    return main
