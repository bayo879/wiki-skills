"""Frontmatter parsing shared by ledger pages and scenario flows.

The parser is deliberately small: scalar keys plus `- item` lists, which is all
the engine and `SCENARIO.md` need. Page rendering and scenario loading must read
the same syntax, so both import this module instead of keeping their own copy.
"""

from __future__ import annotations

import json
import re


def split_frontmatter(text):
    """Split a document into (metadata dict, body). Tolerant of missing fields."""
    if not text.startswith("---"):
        return {}, text
    lines = text.splitlines()
    if lines[0].strip() != "---":
        return {}, text
    meta = {}
    end = None
    current_list = None
    for index in range(1, len(lines)):
        line = lines[index]
        if line.strip() == "---":
            end = index
            break
        item = re.match(r"^([A-Za-z_][\w-]*):\s*(.*)$", line)
        if item:
            key, value = item.group(1), item.group(2).strip()
            current_list = None
            if value == "":
                meta[key] = []
                current_list = key
            else:
                meta[key] = unquote_scalar(value)
            continue
        entry = re.match(r"^\s+-\s*(.*)$", line)
        if entry and current_list:
            meta[current_list].append(unquote_scalar(entry.group(1).strip()))
    if end is None:
        return {}, text
    return meta, "\n".join(lines[end + 1:]).lstrip("\n")


def unquote_scalar(value):
    value = value.strip()
    if len(value) >= 2 and value[0] == value[-1] and value[0] in "\"'":
        try:
            return json.loads(value) if value[0] == '"' else value[1:-1]
        except json.JSONDecodeError:
            return value[1:-1]
    return value


def yaml_scalar(value):
    return json.dumps(str(value), ensure_ascii=False)
