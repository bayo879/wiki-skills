"""步骤契约：解析 WORKFLOW.md 的 `## 步骤` 与 `## 适用` 段。

只做文本解析，不碰文件与账本：`parse_steps` 取第一个 `## 步骤` 段里的顶层列表项，
`_contract` 按四类标记给出 warnings/errors/hint，`_applies` 原样取出 `## 适用` 段。
工作流目录的扫描、加载与条目组装在 workflows.py。
"""

from __future__ import annotations

import re


STEP_TYPES = ("tool", "human", "agent", "verify")
# The typed-step contract ships in 0.5.0. Old files stay valid with a warning
# until 0.6.0; flip this constant and drop the warning branch at that release.
UNTYPED_ALLOWED = True


STEP_HEADING = re.compile(r"^##\s+步骤\s*$")
APPLIES_HEADING = re.compile(r"^##\s+适用\s*$")
HEADING2 = re.compile(r"^##\s")
LIST_ITEM = re.compile(r"^( {0,3})([-*+]|\d+[.)])\s+(.*)$")
MARKER = re.compile(r"^(tool|human|agent|verify)\s*[:：]\s*(.*)$", re.IGNORECASE)
FENCE = re.compile(r"^(```|~~~)")


def _applies(text):
    """Return the `## 适用` section verbatim, or an empty string."""
    lines = []
    in_section = False
    for line in text.splitlines():
        stripped = line.strip()
        if APPLIES_HEADING.match(stripped):
            in_section = True
            continue
        if in_section and HEADING2.match(stripped):
            break
        if in_section:
            lines.append(line)
    return "\n".join(lines).strip()


def parse_steps(text):
    """Parse the first `## 步骤` section into typed steps.

    Only the shallowest list items count as steps: sub-bullets and fenced code
    are ignored. A step is typed when its item text starts with `tool:`,
    `human:`, `agent:` or `verify:` (full-width colon and any case accepted).

    Returns `(steps, has_section)`; each step is
    `{"line": int, "type": str|None, "text": str}` and `text` excludes the
    marker when one is present.
    """
    lines = text.splitlines()
    in_section = False
    in_fence = False
    items = []
    for index, line in enumerate(lines, 1):
        if FENCE.match(line.strip()):
            in_fence = not in_fence
            continue
        if in_fence:
            continue
        if not in_section:
            if STEP_HEADING.match(line):
                in_section = True
            continue
        if HEADING2.match(line):
            break
        found = LIST_ITEM.match(line)
        if found:
            items.append((len(found.group(1)), index, found.group(3).strip()))
    if not in_section:
        return [], False
    if not items:
        return [], True
    base = min(indent for indent, _, _ in items)
    steps = []
    for indent, line, raw in items:
        if indent != base:
            continue
        marker = MARKER.match(raw)
        if marker:
            steps.append(
                {
                    "line": line,
                    "type": marker.group(1).lower(),
                    "text": marker.group(2).strip(),
                }
            )
        else:
            steps.append({"line": line, "type": None, "text": raw})
    return steps, True


def _contract(steps, has_section):
    """Return `(warnings, errors, hint)` for the typed-step contract."""
    warnings = []
    errors = []
    hint = ""
    if not has_section:
        if UNTYPED_ALLOWED:
            warnings.append(
                _warning(
                    "W_STEPS_MISSING",
                    0,
                    "",
                    "missing ## 步骤 section; add one with typed steps (0.6.0 will require it)",
                )
            )
        else:
            errors.append("missing ## 步骤 section")
            hint = "add a ## 步骤 section whose steps start with tool:/human:/agent:/verify:"
        return warnings, errors, hint
    if not steps:
        if UNTYPED_ALLOWED:
            warnings.append(
                _warning(
                    "W_STEPS_EMPTY",
                    0,
                    "",
                    "## 步骤 has no list items; add typed steps (0.6.0 will require it)",
                )
            )
        else:
            errors.append("## 步骤 has no list items")
            hint = "add list items under ## 步骤 and start each with tool:/human:/agent:/verify:"
        return warnings, errors, hint
    typed = [step for step in steps if step["type"]]
    if not typed:
        if UNTYPED_ALLOWED:
            warnings.append(
                _warning(
                    "W_STEPS_UNTYPED",
                    steps[0]["line"],
                    steps[0]["text"],
                    "no step is typed; prefix each step with tool:, human:, agent: or verify: "
                    "(0.6.0 will require it)",
                )
            )
        else:
            errors.append("no step is typed")
            hint = "prefix each step with tool:, human:, agent: or verify:"
        return warnings, errors, hint
    for step in steps:
        if step["type"] is None:
            warnings.append(
                _warning(
                    "W_STEP_UNTYPED",
                    step["line"],
                    step["text"],
                    "step is not typed; prefix it with tool:, human:, agent: or verify:",
                )
            )
        elif not step["text"]:
            warnings.append(
                _warning(
                    "W_STEP_EMPTY",
                    step["line"],
                    "",
                    f"{step['type']}: step has no content",
                )
            )
    if not any(step["type"] == "tool" for step in typed):
        errors.append("no tool: step; a pure manual procedure belongs on a process page")
        hint = "write the procedure as a process page, or add the tool steps it calls"
    return warnings, errors, hint


def _warning(code, line, text, message):
    return {"code": code, "line": line, "text": text, "message": message}
