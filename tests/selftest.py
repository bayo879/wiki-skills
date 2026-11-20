#!/usr/bin/env python3
"""End-to-end self test for the task-driven wiki engine.

Runs a disposable knowledge base in a temp directory: ingest, route decisions,
page submission, view push, import/restore, classification, dry run. Never
touches real data and only uses the standard library.

Run:  python3 tests/selftest.py [--keep] [--json]
Exit: 0 all checks passed, 1 a check failed.
"""

import argparse
import json
import os
import re
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills" / "llm-wiki" / "scripts" / "wiki.py"
sys.path.insert(0, str(SCRIPT.parent))

from runtime import require_python

require_python()
import cli  # noqa: E402  (query output helpers)
import ledger  # noqa: E402  (config migration)
import topics  # noqa: E402  (view/legacy topic resolution)
import workflows  # noqa: E402  (frontmatter normalisation, for tag checks)
import wiki  # noqa: E402  (the engine itself, for table-level checks)
from ingest import raw_body_digest  # noqa: E402
from util import split_provenance  # noqa: E402

WIKI_CACHE = ".wiki"
CHECKED = []


def check(results, name, condition, detail=""):
    results.append((name, bool(condition), detail))
    if not condition:
        raise AssertionError(f"{name}: {detail}")


def run(root, *args, expect=0, stdin=None, timeout=None):
    proc = subprocess.run(
        [sys.executable, str(SCRIPT), *args, "--json"],
        cwd=root, capture_output=True, text=True, input=stdin, timeout=timeout,
    )
    payload = json.loads(proc.stdout) if proc.stdout.strip() else {}
    allowed = expect if isinstance(expect, tuple) else (expect,)
    if proc.returncode not in allowed:
        raise AssertionError(
            f"{' '.join(args)} exited {proc.returncode}, expected {expect}: {proc.stderr or proc.stdout}"
        )
    return payload


def run_text(root, *args):
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        cwd=root, capture_output=True, text=True)


def payload_file(root, name, data):
    path = Path(root) / WIKI_CACHE / "payloads" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, ensure_ascii=False), encoding="utf-8")
    return str(path)


def route_query(root, task, *words):
    """Register topic words: a route submit without a registered query is refused."""
    return run(root, "query", "--task", task["id"], *(words or ["主题"]))


def body_file(root):
    path = Path(root) / WIKI_CACHE / "drafts" / "page.md"
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(
        "## 结论\n\n停机后本人持证到厅复机，约两个工作日。\n\n"
        "## 步骤\n\n本人带身份证到营业厅提交复机申请。\n\n"
        "## 注意事项\n\n异地办理与资费以当地营业厅为准。\n",
        encoding="utf-8",
    )
    return path


def view_dir_checks(results):
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-viewdir-"))
    try:
        (root / ".wiki").mkdir()
        (root / ".wiki/wiki.json").write_text(
            json.dumps({"publish": {"dir": "kb"}}), encoding="utf-8")
        check(results, "view_dir reads publish.dir",
              ledger.view_dir(root) == root / "kb" and not (root / "kb").exists())
        (root / ".wiki/wiki.json").write_text("{}", encoding="utf-8")
        check(results, "view_dir defaults to wiki",
              ledger.view_dir(root) == root / "wiki")
    finally:
        shutil.rmtree(root, ignore_errors=True)

    root = Path(tempfile.mkdtemp(prefix="llm-wiki-viewdir-off-"))
    try:
        (root / ".wiki").mkdir()
        (root / ".wiki/wiki.json").write_text(
            json.dumps({"publish": {"enabled": False}}), encoding="utf-8")
        run(root, "status")
        check(results, "disabled publish still creates content dirs",
              (root / "wiki/workflows").is_dir() and (root / "wiki/topics").is_dir())
    finally:
        shutil.rmtree(root, ignore_errors=True)


def write_workflow(base, name, description, body="## 步骤\n\n1. tool: **运行**\n"):
    directory = base / name
    directory.mkdir(parents=True, exist_ok=True)
    (directory / "WORKFLOW.md").write_text(
        f"---\nname: {name}\ndescription: {description}\n---\n\n# {name}\n\n"
        "## 步骤\n\n1. tool: **运行**\n\n"
        f"{body}",
        encoding="utf-8")
    return directory


def workflow_view_checks(results):
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-viewwf-"))
    try:
        view = root / "wiki/workflows"
        legacy = root / "workflows"
        write_workflow(view, "account-opening", "观看区开户", "观看区版本")
        write_workflow(legacy, "account-opening", "旧根开户", "旧版本")
        write_workflow(legacy, "legacy-only", "旧根流程")

        roots = [p.relative_to(root).as_posix()
                 for p in workflows.workflow_roots(root, view_root=view)]
        check(results, "workflow_roots puts view first",
              roots == ["wiki/workflows", "workflows"], roots)

        entries = workflows.scan(root, builtin=False, view_root=view)
        by_id = {item["id"]: item for item in entries}
        check(results, "view workflow wins over legacy",
              by_id["account-opening"]["origin"] == "view"
              and by_id["account-opening"]["path"] == "wiki/workflows/account-opening"
              and by_id["account-opening"]["main"]
              == "wiki/workflows/account-opening/WORKFLOW.md")
        check(results, "legacy conflict is marked",
              by_id["account-opening"]["legacy_conflict"] is True
              and by_id["account-opening"]["legacy_path"] == "workflows/account-opening/WORKFLOW.md")
        check(results, "legacy-only workflow keeps legacy origin",
              by_id["legacy-only"]["origin"] == "legacy")

        inv = workflows.inventory(root, view_root=view)
        check(results, "inventory reports relative roots",
              inv["root"] == "wiki/workflows"
              and inv["roots"] == ["wiki/workflows", "workflows"], inv)

        loaded = workflows.load(root, "account-opening", view_root=view)
        check(results, "load reads view workflow",
              loaded["workflow"]["main"] == "wiki/workflows/account-opening/WORKFLOW.md"
              and loaded["workflow"]["origin"] == "view", loaded["workflow"])

        matched = workflows.match(root, ["观看区"], 5, view_root=view)
        check(results, "workflow search finds view workflow",
              matched and matched[0]["id"] == "account-opening"
              and matched[0]["origin"] == "view", matched)

        write_workflow(view, "workflow-create", "观看区覆盖内置")
        shadowed = workflows.load(root, "workflow-create", view_root=view)
        check(results, "view workflow shadows builtin",
              shadowed["workflow"]["origin"] == "view", shadowed["workflow"])

        builtin = workflows.load(root, "topic-exploration", view_root=view)
        check(results, "builtin workflow keeps builtin origin",
              builtin["workflow"]["origin"] == "builtin", builtin["workflow"])
    finally:
        shutil.rmtree(root, ignore_errors=True)


def write_topic(base, name, title, description, body=None):
    base.mkdir(parents=True, exist_ok=True)
    text = body if body is not None else f"## 结论\n\n{description}\n"
    (base / name).write_text(
        f"---\ntitle: {title}\ndescription: {description}\nstatus: open\n---\n\n"
        f"{text}",
        encoding="utf-8")


def topics_view_checks(results):
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-viewtopic-"))
    try:
        view = root / "wiki/topics"
        legacy = root / "topics"
        write_topic(view, "观看区话题.md", "观看区话题", "观看区版本")
        write_topic(legacy, "观看区话题.md", "旧话题", "旧版本")
        write_topic(legacy, "旧话题.md", "旧话题", "旧版本")

        entries = topics.scan(root, view_root=view)
        by_name = {item["name"]: item for item in entries}
        check(results, "view topic wins over legacy",
              by_name["观看区话题"]["origin"] == "view"
              and by_name["观看区话题"]["path"] == "wiki/topics/观看区话题.md")
        check(results, "legacy-only topic keeps legacy origin",
              by_name["旧话题"]["origin"] == "legacy"
              and by_name["旧话题"]["path"] == "topics/旧话题.md")

        matched = topics.match(root, ["观看区"], 5, view_root=view)
        check(results, "topic search finds view topic",
              matched and matched[0]["name"] == "观看区话题"
              and matched[0]["origin"] == "view", matched)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def topic_refs_checks(results):
    body = (
        "## 依据\n\n"
        "- 页面：`anti-fraud-restore` — 复机路径\n"
        "- 页面: `anti-fraud-restore`\n"
        "- 原文：`raw:docs/a.md`\n"
        "- 话题：`复机办理`\n"
        "- 工作流：`Topic-Exploration`\n"
        "- 页面：见下文\n"
        "- 页面`refund-policy`\n"
        "- 页面已重建\n"
        "- 原文 缺失\n"
        "\n## 结论\n\n- 页面：`in-conclusion`\n"
    )
    parsed = topics.parse_refs(body)
    check(results, "topic refs parse four kinds",
          parsed["pages"] == ["anti-fraud-restore"]
          and parsed["raws"] == ["docs/a.md"]
          and parsed["topics"] == ["复机办理"]
          and parsed["workflows"] == ["topic-exploration"], parsed)
    check(results, "topic refs record malformed lines only for ref-shaped rows",
          parsed["malformed"] == ["- 页面：见下文", "- 页面`refund-policy`"], parsed)
    check(results, "topic refs ignore prose and other sections",
          "in-conclusion" not in parsed["pages"]
          and parsed["sections"] == 1, parsed)
    two = topics.parse_refs("## 依据\n\n- 页面：`a`\n\n## 依据\n\n- 页面：`b`\n")
    check(results, "second 依据 section still parses",
          two["sections"] == 2 and two["pages"] == ["a", "b"], two)
    fenced = topics.parse_refs(
        "## 依据\n\n- 页面：`real`\n\n```\n- 页面：`fake`\n## 依据\n```\n")
    check(results, "topic refs skip fenced code blocks",
          fenced["pages"] == ["real"] and fenced["sections"] == 1
          and not fenced["malformed"], fenced)
    invalid = Path(tempfile.mkdtemp(prefix="llm-wiki-topicblank-"))
    try:
        write_topic(invalid / "wiki/topics", "空话题.md", "空话题", "无正文",
                    body="")
        blank = topics.scan(invalid, view_root=invalid / "wiki/topics")[0]
        check(results, "invalid topic entry keeps an empty refs record",
              blank["valid"] is False and blank["refs"]["sections"] == 0
              and not any(blank["refs"][key] for key in (
                  "pages", "raws", "topics", "workflows", "malformed")),
              blank)
    finally:
        shutil.rmtree(invalid, ignore_errors=True)
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-topicrank-"))
    try:
        write_topic(root / "wiki/topics", "复机办理.md", "复机办理", "复机路径",
                    body="## 依据\n\n- 页面：`page-a`\n\n## 结论\n\n复机需本人到厅。\n")
        matched = topics.match(root, ["复机"], 5, view_root=root / "wiki/topics")
        check(results, "topic match carries parsed refs",
              matched and matched[0]["refs"]["pages"] == ["page-a"], matched)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def view_definition_suite(results):
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-view-defs-"))
    try:
        (root / "source.md").write_text("# 观看区流程\n\n内容\n", encoding="utf-8")
        write_workflow(root / "workflows", "legacy-only", "旧根目录流程")
        run(root, "ingest")
        check(results, "view creates content dirs", (root / "wiki/workflows").is_dir())

        write_workflow(root / "wiki/workflows", "account-opening", "观看区开户", "观看区版本")
        write_workflow(root / "workflows", "account-opening", "旧根开户", "旧版本")
        listing = run(root, "workflow", "list")["data"]
        by_id = {item["id"]: item for item in listing["workflows"]}
        check(results, "workflow list reports view first",
              listing["root"] == "wiki/workflows"
              and listing["roots"] == ["wiki/workflows", "workflows"], listing)
        check(results, "workflow list marks view origin and legacy conflict",
              by_id["account-opening"]["origin"] == "view"
              and by_id["account-opening"]["legacy_conflict"] is True
              and by_id["account-opening"]["legacy_path"]
              == "workflows/account-opening/WORKFLOW.md")
        check(results, "workflow list keeps legacy-only",
              by_id["legacy-only"]["origin"] == "legacy")
        check(results, "workflow id appears once",
              [item["id"] for item in listing["workflows"]].count("account-opening") == 1)

        loaded = run(root, "workflow", "load", "account-opening")["data"]
        check(results, "workflow load reads view",
              loaded["path"] == "wiki/workflows/account-opening/WORKFLOW.md"
              and loaded["workflow"]["origin"] == "view", loaded)
        legacy = run(root, "workflow", "load", "legacy-only")["data"]
        check(results, "workflow load falls back to legacy",
              legacy["path"] == "workflows/legacy-only/WORKFLOW.md"
              and legacy["workflow"]["origin"] == "legacy", legacy)

        proc = run_text(root, "workflow", "list")
        check(results, "workflow list warns about legacy conflict",
              proc.returncode == 0 and "观看区版本生效" in proc.stdout, proc.stdout)

        write_topic(root / "wiki/topics", "观看区话题.md", "观看区话题", "观看区版本")
        write_topic(root / "topics", "观看区话题.md", "旧话题", "旧版本")
        query = run(root, "query", "观看区")["data"]
        topic_hits = [item for item in query["topics"] if item["name"] == "观看区话题"]
        check(results, "query reads view topic once",
              len(topic_hits) == 1
              and topic_hits[0]["path"] == "wiki/topics/观看区话题.md"
              and topic_hits[0]["origin"] == "view", topic_hits)
        check(results, "query reads view workflow",
              any(item["id"] == "account-opening" and item["origin"] == "view"
                  for item in query["workflows"]), query["workflows"])

        before = (root / "wiki/workflows/account-opening/WORKFLOW.md").read_text(encoding="utf-8")
        run(root, "status")
        after = (root / "wiki/workflows/account-opening/WORKFLOW.md").read_text(encoding="utf-8")
        check(results, "sync does not overwrite view workflow", before == after)
    finally:
        shutil.rmtree(root, ignore_errors=True)

    custom = Path(tempfile.mkdtemp(prefix="llm-wiki-view-custom-"))
    try:
        (custom / ".wiki").mkdir()
        (custom / ".wiki/wiki.json").write_text(
            json.dumps({"publish": {"dir": "kb"}}), encoding="utf-8")
        write_workflow(custom / "wiki/workflows", "old-flow", "旧观看区")
        write_workflow(custom / "kb/workflows", "new-flow", "新观看区")
        data = run(custom, "workflow", "list")["data"]
        ids = [item["id"] for item in data["workflows"]]
        check(results, "workflow list uses custom publish.dir",
              data["root"] == "kb/workflows" and "new-flow" in ids and "old-flow" not in ids,
              data)
    finally:
        shutil.rmtree(custom, ignore_errors=True)


def topic_refs_flow(results):
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-topicrefs-"))
    try:
        (root / "shared.md").write_text(
            "# 共享来源\n\n复机与停机都靠这份规则。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next", "--kind", "route")["data"]["tasks"][0]
        route_query(root, route, "复机")
        key = route["work"]["sources"][0]["key"]
        run(root, "submit", route["id"], "--payload", payload_file(
            root, "topicrefs-route.json",
            {"decisions": [{"source": key, "action": "new", "page": "page-a",
                            "title": "甲页", "type": "concept", "reason": "甲"}]},
        ))
        task = run(root, "next", "--kind", "page")["data"]["tasks"][0]
        draft = root / ".wiki/drafts/page-a.md"
        draft.parent.mkdir(parents=True, exist_ok=True)
        draft.write_text(
            "## 结论\n\n复机需本人到厅。\n\n## 要点\n\n- 复机需本人到厅。\n",
            encoding="utf-8")
        run(root, "submit", task["id"], "--payload", payload_file(
            root, "topicrefs-page.json",
            {"page": "page-a", "title": "甲页", "type": "concept",
             "summary": "复机需本人到厅。", "links": [],
             "body_file": str(draft.relative_to(root)),
             "evidence": [{"source": key, "quote": "复机与停机都靠这份规则。"}],
             "boundaries": {"covered": "甲页", "not_covered": "其他"}},
        ))
        raw_rel = run(root, "query", "--page", "page-a")["data"]["sources"][0]["raw"]
        raw_token = raw_rel.split("raw/", 1)[1]

        write_topic(root / "wiki/topics", "复机旁证.md", "复机旁证", "旁证")
        write_topic(
            root / "wiki/topics", "复机办理.md", "复机办理", "复机路径",
            body="## 依据\n\n"
                 "- 页面：`page-a` — 甲页\n"
                 "- 页面：`ghost-page`\n"
                 f"- 原文：`raw:{raw_token}`\n"
                 "- 话题：`复机旁证`\n"
                 "- 工作流：`topic-exploration`\n"
                 "- 页面：见下文\n"
                 "\n## 结论\n\n复机需本人到厅。\n")
        write_topic(root / "topics", "旧库话题.md", "旧库话题", "旧库复机",
                    body="## 依据\n\n- 页面：`page-a`\n\n## 结论\n\n复机旧记录。\n")
        legacy = [item for item in run(root, "query", "旧库")["data"]["topics"]
                  if item["name"] == "旧库话题"][0]
        check(results, "legacy topic refs parse too",
              legacy["origin"] == "legacy" and legacy["refs"]["pages"] == ["page-a"],
              legacy)
        query = run(root, "query", "复机")["data"]
        hit = [item for item in query["topics"] if item["name"] == "复机办理"][0]
        refs = hit["refs"]
        check(results, "query topic refs keep validated kinds",
              refs["pages"] == ["page-a"] and refs["raws"] == [raw_token]
              and refs["topics"] == ["复机旁证"]
              and refs["workflows"] == ["topic-exploration"], refs)
        check(results, "query topic refs put unknown targets in missing",
              refs["missing"] == [{"kind": "page", "target": "ghost-page"}], refs)
        listing = [item for item in query["results"] if item["id"] == "page-a"]
        check(results, "query results cite topics that reference the page",
              listing and listing[0]["citing_topics"] == ["复机办理", "旧库话题"], listing)
        detail = run(root, "query", "--page", "page-a")["data"]
        check(results, "query page cites topics",
              detail["citing_topics"] == ["复机办理", "旧库话题"], detail)
        page_text = run_text(root, "query", "--page", "page-a")
        check(results, "query page prints citing topics line",
              "citing topics: 复机办理, 旧库话题" in page_text.stdout, page_text.stdout)
        cite_index = cli.citing_topics_index({
            "旧库话题": {"pages": ["page-a"], "missing": []},
            "复机办理": {"pages": ["page-a"],
                         "missing": [{"kind": "page", "target": "ghost-page"}]},
            "空引用": {"pages": [], "missing": [
                {"kind": "page", "target": "ghost-page"}]},
        })
        check(results, "citing topics index sorts and ignores missing",
              cite_index == {"page-a": ["复机办理", "旧库话题"]}, cite_index)
        check(results, "citing topics line caps at three names plus +N",
              cli.ref_topics_line(["甲", "乙", "丙", "丁", "戊"]) == "甲, 乙, 丙, +2"
              and cli.ref_topics_line(["甲", "乙", "丙"]) == "甲, 乙, 丙"
              and cli.ref_topics_line([]) == "", "ref_topics_line")
        proc = run_text(root, "query", "复机")
        check(results, "query warns about broken refs without failing",
              proc.returncode == 0
              and "cites unknown page ghost-page" in proc.stderr
              and "malformed ref" in proc.stderr, proc.stderr)

        many = "\n".join(f"- 页面：`ghost-{index}`" for index in range(21))
        write_topic(root / "wiki/topics", "坏链话题.md", "坏链话题", "大量坏链",
                    body="## 依据\n\n" + many + "\n")
        capped = run(root, "query", "坏链")["data"]["topics"][0]["refs"]
        check(results, "missing refs cap at 20",
              len(capped["missing"]) == 20, len(capped["missing"]))

        raw_dir = (root / raw_rel).parent
        for index in range(21):
            (raw_dir / f"extra-{index}.md").write_text("# 补充\n\n说明。\n", encoding="utf-8")
        extra = "\n".join(f"- 原文：`raw:extra-{index}.md`" for index in range(21))
        write_topic(root / "wiki/topics", "超量话题.md", "超量话题", "超量原文",
                    body="## 依据\n\n" + extra + "\n")
        capped = run(root, "query", "超量")["data"]["topics"][0]["refs"]
        check(results, "valid refs cap at 20", len(capped["raws"]) == 20, len(capped["raws"]))

        many_pages = "\n".join(
            ["- 页面：`page-a`"]
            + [f"- 页面：`ghost-{index}`" for index in range(24)]
            + [f"- 原文：`raw:extra-{index}.md`" for index in range(5)])
        write_topic(root / "wiki/topics", "混合话题.md", "混合话题", "混合引用",
                    body="## 依据\n\n" + many_pages + "\n")
        capped = run(root, "query", "混合")["data"]["topics"][0]["refs"]
        check(results, "kept refs cap at 20 across kinds",
              capped["pages"] == ["page-a"] and capped["raws"] == []
              and [item["target"] for item in capped["missing"]]
              == [f"ghost-{index}" for index in range(19)]
              and sum(len(capped[key]) for key in
                      ("pages", "raws", "topics", "workflows"))
              + len(capped["missing"]) == 20, capped)

        for index in range(25):
            write_topic(root / "wiki/topics", f"引用-{index:02d}.md",
                        f"引用{index:02d}", "引用材料")
        ref_lines = "\n".join(
            [f"- 原文：`raw:extra-{index}.md`" for index in range(10)]
            + [f"- 话题：`引用-{index:02d}`" for index in range(25)])
        write_topic(root / "wiki/topics", "封顶话题.md", "封顶话题", "封顶引用",
                    body="## 依据\n\n" + ref_lines + "\n")
        capped = run(root, "query", "封顶")["data"]["topics"][0]["refs"]
        check(results, "valid refs cap at 20 in total",
              sum(len(capped[key]) for key in
                  ("pages", "raws", "topics", "workflows")) == 20
              and len(capped["raws"]) == 10
              and capped["topics"] == [f"引用-{index:02d}" for index in range(10)]
              and capped["missing"] == [], capped)
        capped_text = run_text(root, "query", "封顶")
        capped_warnings = [line for line in capped_text.stderr.splitlines()
                           if "封顶话题" in line]
        check(results, "truncating refs warns once on stderr",
              len(capped_warnings) == 1
              and "refs truncated to 20" in capped_warnings[0], capped_text.stderr)

        write_topic(root / "wiki/topics", "越界话题.md", "越界话题", "越界原文",
                    body="## 依据\n\n- 原文：`raw:/etc/passwd`\n"
                         "- 原文：`raw:../ok.md`\n\n## 结论\n\n复机越界来源。\n")
        escaped = run(root, "query", "越界")["data"]["topics"][0]["refs"]
        check(results, "raw refs cannot escape the raw dir",
              escaped["raws"] == []
              and escaped["missing"] == [
                  {"kind": "raw", "target": "/etc/passwd"},
                  {"kind": "raw", "target": "../ok.md"}], escaped)

        write_topic(root / "wiki/topics", "坏字节话题.md", "坏字节话题", "坏字节原文",
                    body="## 依据\n\n- 原文：`raw:bad\x00name.md`\n")
        broken_byte = run(root, "query", "坏字节")["data"]["topics"][0]["refs"]
        check(results, "a NUL byte in a raw ref goes to missing",
              broken_byte["raws"] == []
              and broken_byte["missing"] == [
                  {"kind": "raw", "target": "bad\x00name.md"}], broken_byte)
    finally:
        shutil.rmtree(root, ignore_errors=True)


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


def repair_flow(results):
    """Repair missing edges from pages that cite the same raw source."""
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-repair-"))
    try:
        (root / "shared.md").write_text(
            "# 共享来源\n\n同一份来源支撑甲、乙两页。\n", encoding="utf-8"
        )
        run(root, "ingest")
        route = run(root, "next", "--kind", "route")["data"]["tasks"][0]
        route_query(root, route, "共享")
        key = route["work"]["sources"][0]["key"]
        run(root, "submit", route["id"], "--payload", payload_file(
            root, "repair-route.json",
            {"decisions": [
                {"source": key, "action": "new", "page": "page-a",
                 "title": "甲页", "type": "concept", "reason": "甲"},
                {"source": key, "action": "new", "page": "page-b",
                 "title": "乙页", "type": "concept", "reason": "乙"},
            ]},
        ))
        tasks = run(root, "next", "--kind", "page", "--batch", "2")["data"]["tasks"]
        check(results, "repair fixture claims both pages", len(tasks) == 2, tasks)
        for task in tasks:
            page = task["template"]["page"]
            title = task["template"]["title"]
            draft = root / ".wiki/drafts" / f"{page}.md"
            draft.parent.mkdir(parents=True, exist_ok=True)
            sentence = f"{title}的正文。"
            quote = "同一份来源支撑甲、乙两页。"
            draft.write_text(
                f"## 结论\n\n{sentence}\n\n## 要点\n\n- {sentence}\n",
                encoding="utf-8",
            )
            run(root, "submit", task["id"], "--payload", payload_file(
                root, f"{page}-repair.json",
                {"page": page, "title": title, "type": "concept", "summary": sentence,
                 "links": [], "body_file": str(draft.relative_to(root)),
                 "evidence": [{"source": key, "quote": quote}],
                 "boundaries": {"covered": title, "not_covered": "其他"}},
            ))

        before = run(root, "query", "--page", "page-a")["data"]
        check(results, "repair fixture starts without edges",
              before["outgoing"] == [] and before["incoming"] == [], before)
        dry = run(root, "page", "link", "--repair", "--dry-run")["data"]
        check(results, "link repair previews shared-source edges",
              dry["dry_run"] is True and dry["edges"] == 1
              and dry["pages"][0]["page"] == "page-a"
              and dry["pages"][0]["added"] == ["page-b"], dry)
        still = run(root, "query", "--page", "page-a")["data"]
        check(results, "a repair preview writes no revision",
              still["rev"] == before["rev"] and still["outgoing"] == [], still)

        applied = run(root, "page", "link", "--repair")["data"]
        check(results, "link repair applies shared-source edges",
              applied["edges"] == 1 and applied["pages"][0]["page"] == "page-a"
              and applied["pages"][0]["added"] == ["page-b"], applied)
        page_a = run(root, "query", "--page", "page-a")["data"]
        page_b = run(root, "query", "--page", "page-b")["data"]
        check(results, "repaired edges point from the first page to the second",
              page_a["outgoing"] == ["page-b"] and page_b["incoming"] == ["page-a"],
              (page_a["outgoing"], page_b["incoming"]))
        view_a = (root / page_a["path"]).read_text(encoding="utf-8")
        view_b = (root / page_b["path"]).read_text(encoding="utf-8")
        check(results, "a repaired edge renders on both sides of the view",
              "[乙页](乙页.md)" in view_a and "[甲页](甲页.md)" in view_b, (view_a, view_b))

        again = run(root, "page", "link", "--repair")["data"]
        check(results, "link repair is idempotent",
              again["edges"] == 0 and again["pages"] == [], again)
        bad = run(root, "page", "link", "--repair", "page-a", expect=2)
        check(results, "link repair rejects positional page ids",
              bad["error"]["code"] == "E_USAGE", bad)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def links_flow(results):
    """Relations are route-decided engine data, not a body section."""
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-links-"))
    try:
        def add_page(source, page, title, sentence, links=None):
            (root / source).write_text(f"# {title}\n\n{sentence}\n", encoding="utf-8")
            run(root, "ingest")
            route = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
            route_query(root, route, "主题")
            key = route["work"]["sources"][0]["key"]
            run(root, "submit", route["id"], "--payload", payload_file(
                root, f"{page}-route.json",
                {"decisions": [{
                    "source": key, "action": "new", "page": page, "title": title,
                    "type": "concept", "links": links or [], "reason": "独立主题",
                }]},
            ))
            task = run(root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
            draft = root / ".wiki" / "drafts" / f"{page}.md"
            draft.parent.mkdir(parents=True, exist_ok=True)
            draft.write_text(
                f"## 结论\n\n{sentence}\n\n## 要点\n\n- {title}。\n",
                encoding="utf-8",
            )
            run(root, "submit", task["id"], "--payload", payload_file(
                root, f"{page}-page.json",
                {"page": page, "title": title, "type": "concept", "summary": sentence,
                 "links": list(task["template"]["links"]),
                 "body_file": str(draft.relative_to(root)),
                 "evidence": [{"source": key, "quote": sentence}],
                 "boundaries": {"covered": title, "not_covered": "其他"}},
            ))
            return task

        add_page("a.md", "page-a", "甲页", "甲的定义。")
        sibling = add_page("b.md", "page-b", "乙页", "乙依赖甲。", links=["page-a"])
        check(results, "route pre-fills links into the page template",
              sibling["template"]["links"] == ["page-a"], sibling["template"])
        run(root, "sync")

        engine = wiki.Wiki(root)
        engine.config = json.loads((root / ".wiki/wiki.json").read_text(encoding="utf-8"))
        engine.conn = sqlite3.connect(root / ".wiki/wiki.db")
        engine.conn.row_factory = sqlite3.Row
        try:
            page_a = engine.page_record("page-a")
            page_b = engine.page_record("page-b")
            check(results, "relations are not stored in the body",
                  "## 关联" not in page_a["body"] and "## 关联" not in page_b["body"],
                  (page_a["body"], page_b["body"]))
            check(results, "the view renders outgoing and incoming edges together",
                  "[乙页](乙页.md)" in engine.view_text(page_a)
                  and "[甲页](甲页.md)" in engine.view_text(page_b),
                  engine.view_text(page_a))
            check(results, "the view has exactly one relation section",
                  engine.view_text(page_b).count("## 关联") == 1,
                  engine.view_text(page_b))
            outgoing, incoming = engine.links_for("page-b")
            check(results, "only the newer page stores the edge",
                  outgoing == ["page-a"] and incoming == [], (outgoing, incoming))
            index = {item["id"]: item for item in engine.page_index()}
            check(results, "page index exposes summaries and relation counts",
                  index["page-a"]["summary"] == "甲的定义。"
                  and index["page-a"]["incoming"] == 1
                  and index["page-b"]["outgoing"] == 1,
                  (index.get("page-a"), index.get("page-b")))
            engine.write_rev(
                "orphan-page",
                {"title": "孤页", "type": "concept", "summary": "没有来源的孤页。",
                 "sources": [], "evidence": [],
                 "boundaries": {"covered": "无", "not_covered": "无"}},
                "## 结论\n\n孤页。\n\n## 要点\n\n无来源。\n",
                "import",
            )
            engine.write_rev(
                "linked-orphan",
                {"title": "无来源但有链接", "type": "concept", "summary": "无来源，但链到 page-a。",
                 "sources": [], "evidence": [], "links": ["page-a"],
                 "boundaries": {"covered": "无", "not_covered": "无"}},
                "## 结论\n\n无来源。\n\n## 要点\n\n有链接。\n",
                "import",
            )
            doctor = run(root, "doctor")["data"]
            check(results, "doctor removes unprovenanced orphans internally",
                  any("deleted 1 unprovenanced orphan" in item for item in doctor["fixed"])
                  and engine.page_record("orphan-page") is None
                  and engine.page_record("linked-orphan") is not None,
                  doctor)
            check(results, "doctor still reports linked pages without sources",
                  any(item["code"] == "L7" and "linked-orphan" in item["message"]
                      for item in doctor["issues"]),
                  doctor["issues"])
        finally:
            engine.close()

        # A merge can add an edge without touching the target page's body, and a
        # later update that omits it must keep it (add-only storage).
        (root / "c.md").write_text("# 丙\n\n甲还可以补充。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(root, route, "主题")
        key = route["work"]["sources"][0]["key"]
        run(root, "submit", route["id"], "--payload", payload_file(
            root, "merge-route.json",
            {"decisions": [{
                "source": key, "action": "merge", "page": "page-a",
                "links": ["page-b"], "reason": "补充甲页",
            }]},
        ))
        task = run(root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
        check(results, "merge carries the existing links into the update template",
              task["template"]["links"] == ["page-b"], task["template"])
        draft = root / ".wiki" / "drafts" / "page-a.md"
        draft.write_text(
            "## 结论\n\n甲的定义。\n\n## 要点\n\n- 补充。\n\n"
            "## 关联\n\n- [[page-b]]\n",
            encoding="utf-8",
        )
        run(root, "submit", task["id"], "--payload", payload_file(
            root, "merge-page.json",
            {"page": "page-a", "title": "甲页", "type": "concept", "summary": "甲的定义。",
             "links": list(task["template"]["links"]),
             "body_file": str(draft.relative_to(root)),
             "evidence": [{"source": key, "quote": "甲还可以补充。"}],
             "boundaries": {"covered": "甲", "not_covered": "其他"}},
        ))
        run(root, "sync")

        engine = wiki.Wiki(root)
        engine.config = json.loads((root / ".wiki/wiki.json").read_text(encoding="utf-8"))
        engine.conn = sqlite3.connect(root / ".wiki/wiki.db")
        engine.conn.row_factory = sqlite3.Row
        try:
            record = engine.page_record("page-a")
            check(results, "a body relation section is stripped on submit",
                  "## 关联" not in record["body"], record["body"])
            check(results, "the merge-added edge is rendered",
                  "[乙页](乙页.md)" in engine.view_text(record), engine.view_text(record))
        finally:
            engine.close()

        # Two siblings in one payload: the later one may point at the earlier one,
        # which does not exist yet at route time.
        (root / "d.md").write_text("# 兄弟讲话\n\n同一份讲话。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(root, route, "主题")
        key = route["work"]["sources"][0]["key"]
        run(root, "submit", route["id"], "--payload", payload_file(
            root, "sibling-route.json",
            {"decisions": [
                {"source": key, "action": "new", "page": "page-s1", "title": "兄弟一",
                 "type": "concept", "reason": "第一页"},
                {"source": key, "action": "new", "page": "page-s2", "title": "兄弟二",
                 "type": "concept", "links": ["page-s1"], "reason": "链到第一页"},
            ]},
        ))
        tasks = run(root, "next", "--kind", "page", "--batch", "2")["data"]["tasks"]
        by_page = {task["template"]["page"]: task for task in tasks}
        check(results, "a sibling may link to a page created by the same payload",
              by_page["page-s2"]["template"]["links"] == ["page-s1"], by_page)
        for page, title in (("page-s1", "兄弟一"), ("page-s2", "兄弟二")):
            task = by_page[page]
            draft = root / ".wiki" / "drafts" / f"{page}.md"
            draft.write_text(
                f"## 结论\n\n{title}。\n\n## 要点\n\n- {title}。\n", encoding="utf-8"
            )
            run(root, "submit", task["id"], "--payload", payload_file(
                root, f"{page}.json",
                {"page": page, "title": title, "type": "concept", "summary": f"{title}。",
                 "links": list(task["template"]["links"]),
                 "body_file": str(draft.relative_to(root)),
                 "evidence": [{"source": key, "quote": "同一份讲话。"}],
                 "boundaries": {"covered": title, "not_covered": "其他"}},
            ))
        run(root, "status")
        engine = wiki.Wiki(root)
        engine.config = json.loads((root / ".wiki/wiki.json").read_text(encoding="utf-8"))
        engine.conn = sqlite3.connect(root / ".wiki/wiki.db")
        engine.conn.row_factory = sqlite3.Row
        try:
            record = engine.page_record("page-s2")
            sibling_text = (root / "wiki" / engine.view_relpath(record)).read_text(encoding="utf-8")
            check(results, "the sibling view shows its edge",
                  "[兄弟一](兄弟一.md)" in sibling_text, sibling_text)
        finally:
            engine.close()

        # Unknown targets are rejected before any write.
        bad_root = Path(tempfile.mkdtemp(prefix="llm-wiki-links-bad-"))
        try:
            (bad_root / "x.md").write_text("# 未知\n\n未知链接。\n", encoding="utf-8")
            run(bad_root, "ingest")
            route = run(bad_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
            route_query(bad_root, route, "主题")
            key = route["work"]["sources"][0]["key"]
            rejected = run(bad_root, "submit", route["id"], "--payload", payload_file(
                bad_root, "bad-route.json",
                {"decisions": [{
                    "source": key, "action": "new", "page": "page-x", "title": "未知",
                    "type": "concept", "links": ["missing-page"], "reason": "测试",
                }]},
            ), expect=2)
            check(results, "a link to a page that does not exist is refused",
                  rejected["error"]["code"] == "E_LINK_UNRESOLVED", rejected)
        finally:
            shutil.rmtree(bad_root, ignore_errors=True)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def retrieval_flow(results):
    """Route work must register its searches, and a started tree drains first."""
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-retrieval-"))
    try:
        (root / "a.md").write_text("# 反诈\n\n复机提醒。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        payload = {"decisions": [{"source": "a.md", "action": "new", "page": "first-page",
                                  "title": "首屏", "type": "concept", "reason": "唯一主题"}]}
        refused = run(root, "submit", route["id"], "--payload",
                      payload_file(root, "no-query.json", payload), expect=3)
        check(results, "a route cannot be decided without a registered query",
              refused["error"]["code"] == "E_NO_QUERY", refused)

        reclaimed = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        view = reclaimed["work"]["queries"]
        check(results, "the route work view exposes the query budget", view["budget"] == 5, view)
        check(results, "the route brief asks for registered searches",
              f"query --task {reclaimed['id']}" in " ".join(reclaimed["brief"]["steps"]),
              reclaimed["brief"]["steps"])

        asked = run(root, "query", "--task", reclaimed["id"], "反诈", "复机", "提醒")["data"]
        check(results, "query --task groups one block per word",
              [group["word"] for group in asked["queries"]] == ["反诈", "复机", "提醒"], asked["queries"])
        check(results, "query --task reports the spent budget",
              asked["budget"]["used"] == 3 and asked["budget"]["remaining"] == 2, asked["budget"])
        check(results, "query --task leaves workflow candidates out",
              "workflow_matches" not in asked and "workflows" not in asked, asked)
        log = (root / ".wiki" / "tasks" / reclaimed["id"] / "queries.jsonl")
        check(results, "every query --task call is logged in the task folder",
              log.exists() and len(log.read_text(encoding="utf-8").splitlines()) == 1, log)

        over = run(root, "query", "--task", reclaimed["id"], "四", "五", "六", expect=3)
        check(results, "the query budget refuses words beyond five per 1000 characters",
              over["error"]["code"] == "E_QUERY_BUDGET", over)
        repeated = run(root, "query", "--task", reclaimed["id"], "反诈")["data"]
        check(results, "repeating a word does not spend the budget",
              repeated["budget"]["used"] == 3, repeated["budget"])

        run(root, "submit", reclaimed["id"], "--payload", payload_file(root, "no-query.json", payload))
        page = run(root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]

        # A started tree drains before the next route task is handed out.
        (root / "b.md").write_text("# 另一主题\n\n别的正文。\n", encoding="utf-8")
        run(root, "ingest")
        blocked = run(root, "next", "--batch", "1", expect=3)
        check(results, "an open page task outranks a freshly queued route task",
              blocked.get("error", {}).get("code") == "E_TASK_UNSUBMITTED"
              and page["id"] in blocked["error"]["message"], blocked.get("error"))
    finally:
        shutil.rmtree(root, ignore_errors=True)

    # A lease belongs to the session that claimed it: a second session stops,
    # it does not take turns on the same queue, and a dead session's expired
    # lease is swept instead of blocking everyone.
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-owner-"))
    try:
        (root / "own.md").write_text("# 归属\n\n云网安全主题日。\n", encoding="utf-8")
        run(root, "ingest")
        claimed = run(root, "next", "--owner", "sess-a")["data"]
        check(results, "next records the claiming session", claimed["owner"] == "sess-a", claimed)
        busy = run(root, "next", "--owner", "sess-b", expect=3)
        check(results, "another session's lease stops next",
              busy["error"]["code"] == "E_INGEST_BUSY"
              and "sess-a" in busy["error"]["message"], busy)
        foreign = run(root, "submit", claimed["tasks"][0]["id"], "--owner", "sess-b", expect=3)
        check(results, "another session cannot submit a foreign lease",
              foreign["error"]["code"] == "E_TASK_FOREIGN", foreign)
        busy_ingest = run(root, "ingest", "--owner", "sess-b", expect=3)
        check(results, "a second session cannot ingest while the queue is taken",
              busy_ingest["error"]["code"] == "E_INGEST_BUSY", busy_ingest)

        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        conn.execute("UPDATE tasks SET lease_until = '2000-01-01T00:00:00Z' WHERE state = 'leased'")
        conn.commit()
        conn.close()
        swept = run(root, "next", "--owner", "sess-c")["data"]
        check(results, "an expired lease is swept, not treated as a live session",
              swept["owner"] == "sess-c" and swept["tasks"], swept)
        released = run(root, "release", swept["tasks"][0]["id"], "--owner", "sess-b")["data"]
        check(results, "release reports the owner it took the task from",
              released["owner"] == "sess-c", released)
        again = run(root, "next", "--owner", "sess-b")["data"]
        check(results, "a released task can be claimed by the next session",
              again["owner"] == "sess-b" and again["tasks"], again)
    finally:
        shutil.rmtree(root, ignore_errors=True)

    # Sibling pages keep the order the route listed them in, so a link to an
    # earlier sibling resolves on the first submit instead of by luck.
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-siblings-"))
    try:
        (root / "talk.md").write_text("# 云网安全讲话\n\n主题日启动。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(root, route, "云网安全")
        run(root, "submit", route["id"], "--payload", payload_file(root, "sibling-order.json", {
            "decisions": [
                {"source": "talk.md", "action": "new", "page": "sib-one", "title": "兄弟一",
                 "type": "concept", "reason": "第一页"},
                {"source": "talk.md", "action": "new", "page": "sib-two", "title": "兄弟二",
                 "type": "concept", "links": ["sib-one"], "reason": "链到第一页"},
            ]}))
        order = [task["template"]["page"] for task in
                 run(root, "next", "--kind", "page", "--batch", "2")["data"]["tasks"]]
        check(results, "sibling page tasks come out in the order the route listed them",
              order == ["sib-one", "sib-two"], order)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def batch_flow(results):
    """A batch stays inside one tree, so two sources never share a batch."""
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-batch-"))
    try:
        (root / "a.md").write_text("# 拆页来源\n\n三个可以分开的主题。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(root, route, "拆页")
        run(root, "submit", route["id"], "--payload", payload_file(root, "batch-a.json", {
            "decisions": [
                {"source": "a.md", "action": "new", "page": "pa1", "title": "甲一",
                 "type": "concept", "reason": "主题一"},
                {"source": "a.md", "action": "new", "page": "pa2", "title": "甲二",
                 "type": "concept", "reason": "主题二"},
                {"source": "a.md", "action": "new", "page": "pa3", "title": "甲三",
                 "type": "concept", "reason": "主题三"},
            ]}))

        (root / "b.md").write_text("# 另一来源\n\n单独一页。\n", encoding="utf-8")
        run(root, "ingest")
        second = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(root, second, "另一来源")
        run(root, "submit", second["id"], "--payload", payload_file(root, "batch-b.json", {
            "decisions": [{"source": "b.md", "action": "new", "page": "pb", "title": "乙页",
                           "type": "concept", "reason": "独立主题"}]}))

        def submit_page(page_id, title, task_id, source, quote):
            draft = root / ".wiki" / "drafts" / f"{page_id}.md"
            draft.parent.mkdir(parents=True, exist_ok=True)
            draft.write_text(
                f"## 结论\n\n{title}的一句话结论。\n\n## 要点\n\n- 一句话。\n",
                encoding="utf-8",
            )
            run(root, "submit", task_id, "--payload", payload_file(
                root, f"{page_id}.json",
                {"page": page_id, "title": title, "type": "concept",
                 "summary": f"{title}的一句话结论。", "links": [],
                 "body_file": str(draft.relative_to(root)),
                 "evidence": [{"source": source, "quote": quote}],
                 "boundaries": {"covered": title, "not_covered": "其他"}},
            ))

        first = run(root, "next", "--batch", "2")["data"]["tasks"]
        check(results, "a batch smaller than the tree stays inside one tree",
              [task["template"]["page"] for task in first] == ["pa1", "pa2"], first)
        check(results, "page tasks expose the tree they belong to",
              {task["work"]["tree"] for task in first} == {route["id"]},
              [task["work"].get("tree") for task in first])

        blocked = run(root, "next", "--batch", "2", expect=3)
        check(results, "a taken batch is reported instead of reissued",
              blocked.get("error", {}).get("code") == "E_TASK_UNSUBMITTED"
              and all(task["id"] in blocked["error"]["message"] for task in first),
              blocked.get("error"))

        for task in first:
            submit_page(task["template"]["page"], task["template"]["title"], task["id"],
                        "a.md", "三个可以分开的主题")

        rest = run(root, "next", "--batch", "2")["data"]["tasks"]
        check(results, "a short tree emits one task instead of borrowing another tree",
              [task["template"]["page"] for task in rest] == ["pa3"]
              and {task["work"]["tree"] for task in rest} == {route["id"]},
              [(task["template"].get("page"), task["work"].get("tree")) for task in rest])
        submit_page("pa3", rest[0]["template"]["title"], rest[0]["id"],
                    "a.md", "三个可以分开的主题")

        other = run(root, "next", "--batch", "5")["data"]["tasks"]
        check(results, "a drained tree hands the next tree its turn",
              [task["template"]["page"] for task in other] == ["pb"]
              and {task["work"]["tree"] for task in other} == {second["id"]},
              [(task["template"].get("page"), task["work"].get("tree")) for task in other])
        submit_page("pb", other[0]["template"]["title"], other[0]["id"],
                    "b.md", "单独一页")

        (root / "c.md").write_text("# 第三来源\n\n还没开始。\n", encoding="utf-8")
        run(root, "ingest")
        alone = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        check(results, "a route task is its own tree",
              "decisions" in alone["template"] and alone["work"]["tree"] == alone["id"],
              {"template": list(alone["template"]), "tree": alone["work"].get("tree")})
    finally:
        shutil.rmtree(root, ignore_errors=True)


def legacy_note_flow(results):
    """`note` 已从默认类型移除，但老库的 wiki.json 还留着它时必须照旧能用。"""
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-legacy-note-"))
    try:
        run(root, "init")
        config_path = root / ".wiki/wiki.json"
        config = json.loads(config_path.read_text(encoding="utf-8"))
        check(results, "a fresh ledger does not offer the note type",
              config["schema"]["types"] == ["concept", "entity", "process"], config["schema"])
        config["schema"]["types"].append("note")
        config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")

        (root / "a.md").write_text("# 值班交接\n\n工作日晚八点交接班。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
        route_query(root, route, "值班")
        run(root, "submit", route["id"], "--payload", payload_file(root, "legacy-route.json", {
            "decisions": [{"source": "a.md", "action": "new", "page": "duty-handover",
                           "title": "值班交接", "type": "note", "reason": "老库留下的类型"}]}))
        page_task = run(root, "next")["data"]["tasks"][0]
        check(results, "a legacy note page keeps its own skeleton",
              page_task["template"]["type"] == "note"
              and page_task["template"]["body"] == "## 要点\n\n", page_task["template"])
        run(root, "submit", page_task["id"], "--payload", payload_file(root, "legacy-page.json", {
            "page": "duty-handover", "title": "值班交接", "type": "note",
            "summary": "工作日晚八点交接班。", "links": [],
            "body": "## 要点\n\n工作日晚八点交接班。\n",
            "evidence": [{"source": "a.md", "quote": "工作日晚八点交接班"}],
            "boundaries": {"covered": "交接时间", "not_covered": "交接清单"}}))
        run(root, "status")
        page = run(root, "query", "--page", "duty-handover")["data"]
        check(results, "a legacy note page is queryable and published",
              page["page"]["type"] == "note"
              and (root / "wiki/pages/note/值班交接.md").is_file(), page["page"])
    finally:
        shutil.rmtree(root, ignore_errors=True)


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--keep", action="store_true", help="keep the temp workspace")
    parser.add_argument("--json", action="store_true", help="machine-readable summary")
    args = parser.parse_args(argv)

    root = Path(tempfile.mkdtemp(prefix="llm-wiki-selftest-"))
    results = []
    try:
        (root / "a.md").write_text(
            "# 反诈复机\n\n号码被停机后，需要本人持身份证到营业厅办理复机。\n"
            "审批时长通常为两个工作日。\n",
            encoding="utf-8",
        )
        (root / "b.md").write_text("# 无关材料\n\n用来验证 skip 分支。\n", encoding="utf-8")
        (root / "scan.png").write_bytes(b"\x89PNG\r\n\x1a\n")
        workflow_main = workflow_files(root)

        listing = run(root, "workflow", "list")["data"]
        check(results, "workflow list includes project and builtin entries",
              listing["count"] == 9 and listing["valid"] == 6 and listing["warned"] == 2, listing)
        check(results, "workflow list reports the workflows/ root only",
              listing["roots"] == ["workflows"] and listing["root"] == "workflows",
              listing.get("roots"))
        check(results, "workflow commands leave no .wiki", not (root / ".wiki").exists(), listing)
        check(results, "workflow tags follow the obsidian rules",
              workflows._tags(["#话题", "见证 开户", "2024", "a/b#c", "话题"])
              == ["话题", "见证", "开户", "a/b", "c"], workflows._tags(["#话题", "见证 开户"]))
        legacy_config = Path(tempfile.mkdtemp(prefix="llm-wiki-config-"))
        try:
            (legacy_config / ".wiki").mkdir()
            config_file = legacy_config / ".wiki" / "wiki.json"
            config_file.write_text('{"publish": {"scenarios": false}}', encoding="utf-8")
            migrated = ledger.load_config(config_file)["publish"]
            check(results, "publish drops seed switches",
                  "topics" not in migrated and "workflows" not in migrated, migrated)
        finally:
            shutil.rmtree(legacy_config, ignore_errors=True)
        view_dir_checks(results)
        workflow_view_checks(results)
        topics_view_checks(results)
        topic_refs_checks(results)
        view_definition_suite(results)
        topic_refs_flow(results)
        parsed, has_section = workflows.parse_steps(
            "## 步骤\n\n1. TOOL：**a**\n2. Human: **b**\n   - tool: 子项\n"
            "3. 没标\n\n## 其他\n\n- tool: 不算\n")
        check(results, "step markers accept a full-width colon and any case",
              has_section and [step["type"] for step in parsed] == ["tool", "human", None], parsed)
        builtin = run(root, "workflow", "load", "topic-exploration")["data"]
        check(results, "builtin workflow is loadable without project files",
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
            entry = run(root, "workflow", "load", name)["data"]
            check(results, f"builtin {name} is loadable and self-contained",
                  entry["workflow"]["source"] == "builtin"
                  and "path" not in entry and "main" not in entry
                  and entry["skill_path"] == f"workflows/{name}/WORKFLOW.md"
                  and entry["workflow"]["skill_path"] == f"workflows/{name}/WORKFLOW.md"
                  and entry["resources"] == []
                  and len(entry["tools"]) >= 1, entry)
        check(results, "builtin workflow id cannot escape",
              run(root, "workflow", "load", "../x", expect=2)["error"]["code"] == "E_USAGE")
        loaded = run(root, "workflow", "load", "account-opening")["data"]
        check(results, "workflow load returns the flow and typed steps",
              "核验身份" in loaded["content"] and loaded["path"].endswith("WORKFLOW.md")
              and loaded["workflow"]["alias_name"] == ""
              and [step["text"] for step in loaded["tools"]] == ["**核验身份**：运行 `scripts/check.sh`"]
              and len(loaded["human_steps"]) == 1 and len(loaded["agent_steps"]) == 1
              and len(loaded["verify_steps"]) == 1 and loaded["warnings"] == [], loaded)
        check(results, "workflow load inventories support files",
              {item["kind"] for item in loaded["resources"]} == {"script", "reference"}, loaded["resources"])
        deck = run(root, "workflow", "load", "deck-flow")["data"]
        check(results, "workflows/ is the only workflow directory",
              deck["path"] == "workflows/deck-flow/WORKFLOW.md"
              and deck["workflow"]["name"] == "Deck flow", deck["path"])
        legacy = run(root, "workflow", "list")["data"]["workflows"]
        scenario_only = next(item for item in legacy if item["id"] == "scenario-only")
        check(results, "a bare SCENARIO.md is not a workflow",
              scenario_only["valid"] is False
              and scenario_only["error"] == "missing WORKFLOW.md", scenario_only)
        partial = run(root, "workflow", "load", "partial")["data"]
        check(results, "an untyped step only warns",
              partial["workflow"]["valid"]
              and [item["code"] for item in partial["warnings"]] == ["W_STEP_UNTYPED"], partial)
        manual = run(root, "workflow", "load", "manual", expect=2)
        check(results, "a typed workflow without tool steps is invalid",
              manual["error"]["code"] == "E_WORKFLOW_INVALID"
              and "process page" in manual["error"]["next"], manual)

        data = run(root, "check")["data"]
        check(results, "check passes in a workspace with no ledger", data["ok"], data)

        # 不需要 init：第一条打开账本的命令自己建库，并把补出来的路径列在 repaired
        fresh = Path(tempfile.mkdtemp(prefix="llm-wiki-fresh-"))
        try:
            (fresh / "n.md").write_text("# 新库\n\n直接摄入。\n", encoding="utf-8")
            born = run(fresh, "ingest")
            check(results, "ingest works without init and reports what it repaired",
                  born["ok"] and (fresh / ".wiki/wiki.db").exists() and born["data"].get("repaired"), born)
        finally:
            shutil.rmtree(fresh, ignore_errors=True)

        # 只有配置没有库：普通命令当场建库，并把补出来的库文件列进 repaired
        half = Path(tempfile.mkdtemp(prefix="llm-wiki-half-"))
        try:
            (half / ".wiki").mkdir()
            (half / ".wiki/wiki.json").write_text("{}", encoding="utf-8")
            (half / "h.md").write_text("# 半初始化\n\n直接摄入。\n", encoding="utf-8")
            grew = run(half, "ingest")
            check(results, "ingest fills a half-initialized workspace",
                  grew["ok"] and (half / ".wiki/wiki.db").exists()
                  and ".wiki/wiki.db" in grew["data"].get("repaired", []), grew)
        finally:
            shutil.rmtree(half, ignore_errors=True)

        # 工作区不可写：E_HEALTH + 手册小节，不能是 traceback
        locked = Path(tempfile.mkdtemp(prefix="llm-wiki-ro-"))
        try:
            locked.chmod(0o555)
            if not os.access(locked, os.W_OK):
                denied = run(locked, "status", expect=1)
                error = denied.get("error", {})
                check(results, "an unwritable workspace reports E_HEALTH with the manual section",
                      error.get("code") == "E_HEALTH"
                      and error.get("section") == "工作区不可写"
                      and error.get("doc", "").endswith("/references/recovery.md")
                      and "check" not in error.get("next", ""), denied)
        finally:
            locked.chmod(0o755)
            shutil.rmtree(locked, ignore_errors=True)

        def legacy_ledger(rows):
            legacy = root / ".wiki"
            legacy.mkdir(exist_ok=True)
            conn = sqlite3.connect(legacy / "wiki.db")
            conn.executescript(
                "DROP TABLE IF EXISTS sources;"
                "CREATE TABLE sources (key TEXT PRIMARY KEY, path TEXT, digest TEXT, size INTEGER,"
                " mtime REAL, raw TEXT, parser TEXT, status TEXT, updated_at TEXT);"
            )
            conn.executemany("INSERT INTO sources (key) VALUES (?)", [(row,) for row in rows])
            conn.commit()
            conn.close()
            return legacy

        # 旧结构但没有数据：按当前结构重建，不算迁移
        legacy_ledger([])
        rebuilt = run(root, "status")
        columns = [row[1] for row in sqlite3.connect(root / ".wiki/wiki.db").execute("PRAGMA table_info(sources)")]
        check(results, "an empty old schema is rebuilt in place",
              rebuilt["ok"] and "raw_digest" in columns, {"run": rebuilt, "columns": columns})

        # 旧结构里有数据：拒绝继续，并指向恢复手册
        legacy_ledger(["old.md"])
        broken = run(root, "status", expect=3)
        error = broken.get("error", {})
        check(results, "rows in an old schema report E_STATE with the manual section",
              error.get("code") == "E_STATE" and error.get("section") == "旧账本"
              and error.get("doc", "").endswith("/references/recovery.md"), broken)

        # 账本文件本身坏了：E_DB，不要当成旧结构
        (root / ".wiki/wiki.db").write_bytes(b"not a database")
        corrupt = run(root, "status", expect=3)
        error = corrupt.get("error", {})
        check(results, "a corrupt ledger reports E_DB with the manual section",
              error.get("code") == "E_DB" and error.get("section") == "账本损坏", corrupt)

        shutil.rmtree(root / ".wiki")
        shutil.rmtree(root / "wiki", ignore_errors=True)  # the probe touched the view too

        data = run(root, "init")["data"]
        check(results, "init creates .wiki", (root / ".wiki/wiki.db").exists() and data["sources"] == 0, data)
        check(results, "init creates the publish directory",
              (root / "wiki").is_dir() and not (root / ".wiki/raw").exists(), data)
        check(results, "init creates workflow/topic content dirs",
              (root / "wiki/workflows").is_dir() and (root / "wiki/topics").is_dir())
        check(results, "publish README is written", (root / "wiki/README.md").exists())

        data = run(root, "check")["data"]
        check(results, "check passes after init", data["ok"], data)

        escaped = run(root, "status", "--root", str(root.parent), expect=2)
        check(results, "root outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)

        topic = root / "topics" / "复机办理.md"
        topic.parent.mkdir()
        topic.write_text(
            "---\n"
            "name: 复机办理\n"
            "description: 停机复机的办理路径与时长\n"
            "status: open\n"
            "updated: 2026-11-16\n"
            "---\n\n"
            "## 主题\n\n停机后如何复机、要多久。\n\n"
            "## 结论\n\n本人持证到厅办理，约两个工作日。\n\n"
            "## 依据\n\n- 页面：anti-fraud-restore — 复机路径与时长\n\n"
            "## 未决问题\n\n异地办理是否一样。\n\n"
            "## 下一步\n\n补异地办理的 raw。\n\n"
            "## 能力候选\n\n无。\n",
            encoding="utf-8",
        )

        data = run(root, "ingest")["data"]
        check(results, "ingest queues route tasks", data["pending_total"] == 2, data)
        check(results, "ingest reports the open queue",
              data["queue"]["open"] == data["queue"]["queued"] + data["queue"]["leased"]
              and data["queue"]["route"] == 2, data.get("queue"))
        check(results, "ingest skips the workflow directory",
              not any(item["key"].startswith("workflows/") for item in data["ingested"]), data["ingested"])
        check(results, "ingest skips topic files",
              not any(item["key"].startswith("topics/") for item in data["ingested"]), data["ingested"])
        check(results, "image classified unsupported", [i["key"] for i in data["unsupported"]] == ["scan.png"], data["unsupported"])

        check(results, "raw lives under the publish dir",
              (root / "wiki/raw").is_dir() and not (root / ".wiki/raw").exists())
        data = run(root, "ingest")["data"]
        raw_files = sorted(p.name for p in (root / "wiki/raw").glob("*.md"))
        check(results, "raw files are written into wiki/raw", len(raw_files) == 2, raw_files)

        # 初筛必须停在 stat：把源文件改成不可读，第二轮 ingest 仍要报 unchanged。
        # 引擎只要读了它，就会拿到 EACCES 而不是 unchanged，这条断言就会失败。
        if os.name == "posix" and os.geteuid() != 0:
            (root / "a.md").chmod(0o000)
            try:
                blind = run(root, "ingest")["data"]
            finally:
                (root / "a.md").chmod(0o644)
            check(results, "an unchanged source is never opened",
                  "a.md" in blind["unchanged"] and blind["failed"] == [], blind)

        # --- Task 3: 两段式初筛、--only、判定表 -------------------------------------
        raw_candidates = sorted((root / "wiki" / "raw").glob("a*.md"))
        check(results, "a.md publishes exactly one raw file", len(raw_candidates) == 1, raw_candidates)
        raw_path = raw_candidates[0]

        edited_raw = raw_path.read_text(encoding="utf-8") + "\n补充：可线上复机。\n"
        raw_path.write_text(edited_raw, encoding="utf-8")

        edited = run(root, "ingest")["data"]
        check(results, "an edited raw survives ingest",
              raw_path.read_text(encoding="utf-8") == edited_raw, edited["ingested"])
        check(results, "an edited raw is not re-parsed", edited["ingested"] == [], edited["ingested"])

        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        ledger_raw = conn.execute(
            "SELECT raw_digest, raw_size, raw_mtime FROM sources WHERE key = ?", ("a.md",)
        ).fetchone()
        conn.close()
        raw_stat = raw_path.stat()
        raw_digest = wiki.raw_body_digest(raw_path)
        raw_size, raw_mtime = raw_stat.st_size, raw_stat.st_mtime
        check(results, "the ledger mirrors the edited raw file",
              ledger_raw is not None and ledger_raw[0] == raw_digest and ledger_raw[1] == raw_size
              and abs(ledger_raw[2] - raw_mtime) < 1e-6,
              (ledger_raw, (raw_digest, raw_size, raw_mtime)))

        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        before_b = conn.execute(
            "SELECT status, digest, raw_digest FROM sources WHERE key = ?", ("b.md",)
        ).fetchone()
        conn.close()
        forced = run(root, "ingest", "--force", "--only", "a.md")["data"]
        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        after_b = conn.execute(
            "SELECT status, digest, raw_digest FROM sources WHERE key = ?", ("b.md",)
        ).fetchone()
        conn.close()
        check(results, "--only re-parses just the named source",
              [item["key"] for item in forced["ingested"]] == ["a.md"]
              and forced["vanished"] == [] and before_b is not None and before_b == after_b,
              (forced["ingested"], forced["vanished"], before_b, after_b))
        check(results, "--force keeps the edited raw",
              raw_path.read_text(encoding="utf-8") == edited_raw, forced["ingested"])
        check(results, "the reported raw path is the edited file",
              bool(forced["ingested"]) and forced["ingested"][0]["raw"] == f"wiki/raw/{raw_path.name}",
              forced["ingested"])

        # raw 只被 touch、字节没变：账本要吸收磁盘上的 size/mtime，否则每次 ingest
        # 都会因为 same_stamp 不成立而重读重算原文。
        touched_stamp = raw_path.stat().st_mtime + 30
        os.utime(raw_path, (touched_stamp, touched_stamp))
        after_touch = run(root, "ingest")["data"]
        touched_stat = raw_path.stat()
        touched_digest = wiki.raw_body_digest(raw_path)
        touched_size, touched_mtime = touched_stat.st_size, touched_stat.st_mtime
        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        touched_row = conn.execute(
            "SELECT raw_digest, raw_size, raw_mtime FROM sources WHERE key = ?", ("a.md",)
        ).fetchone()
        conn.close()
        again = run(root, "ingest")["data"]
        check(results, "a touched raw refreshes the ledger stamps",
              after_touch["ingested"] == [] and touched_row is not None
              and touched_row[0] == touched_digest and touched_row[1] == touched_size
              and abs(touched_row[2] - touched_mtime) < 1e-6 and again["ingested"] == [],
              (after_touch["ingested"], touched_row,
               (touched_digest, touched_size, touched_mtime), again["ingested"]))

        cases = [
            ((False, "ok", 10.0, 10.0), "skip"),
            ((False, "missing", 10.0, 10.0), "rewrite"),
            ((False, "edited", 10.0, 20.0), "bookkeep_raw"),
            ((True, "ok", 10.0, 10.0), "rewrite"),
            ((True, "edited", 20.0, 10.0), "rewrite"),
            ((True, "edited", 10.0, 20.0), "bookkeep_source"),
            ((False, "ok", 10.0, 10.0, True), "parse_keep_raw"),
            ((False, "edited", 10.0, 20.0, True), "parse_keep_raw"),
            ((True, "ok", 20.0, 10.0, True), "rewrite"),
        ]
        check(results, "the ingest decision table follows the spec",
              [wiki.ingest_decision(*args) for args, _ in cases] == [want for _, want in cases], cases)

        found = run(root, "query", "开户")["data"]
        check(results, "query carries workflow candidates without loading them",
              found.get("workflow_matches") == 1
              and found.get("workflows", [{}])[0].get("id") == "account-opening"
              and found.get("workflows", [{}])[0].get("applies") == "新客户首次开户，且需要核验身份。"
              and found.get("workflows", [{}])[0].get("load") == 'workflow load "account-opening"',
              found)
        searched = run(root, "workflow", "search", "开户")["data"]
        check(results, "workflow search finds a project workflow",
              searched["matches"] == 1 and searched["workflows"][0]["id"] == "account-opening"
              and searched["workflows"][0]["tools"] == 1
              and searched["workflows"][0].get("applies") == "新客户首次开户，且需要核验身份。",
              searched)
        workflow_main.write_text(
            workflow_main.read_text(encoding="utf-8") + "\n补充：支持见证开户。\n",
            encoding="utf-8",
        )
        live = run(root, "workflow", "search", "见证开户")["data"]
        check(results, "workflow edits are searchable without ingest",
              live["matches"] == 1, live)
        invalid = run(root, "workflow", "search", "坏流程")["data"]
        check(results, "invalid workflows stay out of workflow search",
              invalid["matches"] == 0 and invalid.get("message") == "无可用workflow", invalid)

        topics_hit = run(root, "query", "复机")["data"]
        builtin_hit = run(root, "workflow", "search", "话题")["data"]
        check(results, "workflow search finds the builtin explicitly",
              builtin_hit["matches"] >= 1
              and builtin_hit["workflows"][0]["id"] == "topic-exploration"
              and builtin_hit["workflows"][0]["source"] == "builtin",
              builtin_hit)
        check(results, "query finds the saved topic",
              topics_hit["topic_matches"] == 1
              and topics_hit["topics"][0]["name"] == "复机办理"
              and topics_hit["topics"][0]["path"].startswith("topics/"),
              topics_hit["topics"])
        check(results, "topic hits stay separate from page hits",
              topics_hit["matches"] == 0, topics_hit)

        tasks = run(root, "next", "--kind", "route", "--batch", "5")["data"]["tasks"]
        check(results, "route claims one task per source", len(tasks) == 2, tasks)
        check(results, "next returns brief and template", all(t.get("brief") and t.get("template") for t in tasks), tasks)
        check(results, "next hides task internals",
              not any(key in tasks[0] for key in ("epoch", "state", "inputs", "lease_until")), tasks[0])
        for task in tasks:
            route_query(root, task, "复机")
        routes = {task["work"]["sources"][0]["key"]: task for task in tasks}
        check(results, "route template pre-fills the source",
              routes["a.md"]["template"]["decisions"][0]["source"] == "a.md", routes["a.md"]["template"])
        source_key = routes["a.md"]["work"]["sources"][0]["key"]
        raw_rel = routes["a.md"]["work"]["sources"][0]["raw"]
        raw_meta, raw_body = wiki.split_frontmatter(
            (root / raw_rel).read_text(encoding="utf-8")
        )
        check(results, "the view tree carries the parsed raw text",
              "复机" in raw_body and raw_rel.startswith("wiki/raw/"), raw_rel)
        check(results, "raw frontmatter names the raw object and its mounts",
              raw_meta.get("type") == "raw"
              and "a.md" in json.dumps(raw_meta, ensure_ascii=False), raw_meta)

        front_meta, front_body = wiki.split_frontmatter(
            "---\ntitle: 手工标题\n---\n\n# 正文\n\n内容。\n"
        )
        check(results, "frontmatter split keys on the first line, not on .wiki",
              front_meta.get("title") == "手工标题" and "title: 手工标题" not in front_body,
              front_meta)

        outside_payload = root.parent / f"{root.name}-payload.json"
        outside_payload.write_text("{}", encoding="utf-8")
        escaped = run(
            root, "submit", routes["b.md"]["id"],
            "--payload", str(outside_payload), expect=2,
        )
        check(results, "payload outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)
        outside_payload.unlink()
        merged = run(
            root, "submit", routes["b.md"]["id"],
            "--payload", payload_file(root, "skip.json",
                                      {"decisions": [{"source": "b.md", "action": "skip",
                                                      "reason": "与主题无关"}]}),
        )
        check(results, "skip decision accepted", merged["ok"], merged)

        submitted = run(
            root, "submit", routes["a.md"]["id"],
            "--payload", payload_file(root, "route.json", {"decisions": [
                {"source": source_key, "action": "new", "page": "anti-fraud-restore",
                 "title": "反诈复机", "type": "process", "reason": "完整的复机办理路径"},
            ]}),
        )
        check(results, "route new queues a page task", submitted["data"]["queued_page_tasks"], submitted)

        body_file(root)
        task = run(root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
        check(results, "page template fixes the section skeleton",
              "## 结论" in task["template"]["body"] and "## 步骤" in task["template"]["body"], task["template"])
        payload = {
            "page": "anti-fraud-restore",
            "title": "反诈复机",
            "type": "process",
            "summary": "停机后本人持证到厅复机，约两个工作日。",
            "links": [],
            "body_file": ".wiki/drafts/page.md",
            "evidence": [{"source": source_key, "quote": "需要本人持身份证到营业厅办理复机"}],
            "boundaries": {"covered": "停机后的复机路径", "not_covered": "异地办理与资费"},
        }
        outside_body = root.parent / f"{root.name}-body.md"
        outside_body.write_text("## 区外草稿\n", encoding="utf-8")
        escaped = run(
            root, "submit", task["id"],
            "--payload", payload_file(root, "body-escape.json",
                                      {**payload, "body_file": str(outside_body)}),
            expect=2,
        )
        check(results, "body_file outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)
        outside_body.unlink()
        task = run(root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
        accepted = run(root, "submit", task["id"],
                       "--payload", payload_file(root, "page.json", payload))
        check(results, "page submit writes rev 1", accepted["data"]["rev"] == 1, accepted)
        # 镜像已删除：页面正文只存在账本与观看区两处。
        check(results, "page submit writes no mirror copy",
              not (root / ".wiki/pages").exists(), root / ".wiki/pages")

        # Task 5: 观看区文件名与别名只从标题生成，先把纯函数钉住。
        check(results, "view_name strips path separators",
              wiki.view_name("复机/办理:2026", "p-x") == "复机-办理-2026",
              wiki.view_name("复机/办理:2026", "p-x"))
        check(results, "view_name collapses whitespace",
              wiki.view_name("复机\u3000 办理", "p-x") == "复机 办理",
              wiki.view_name("复机\u3000 办理", "p-x"))
        check(results, "view_name truncates long titles",
              len(wiki.view_name("复" * 200, "p-x")) == 60,
              len(wiki.view_name("复" * 200, "p-x")))
        check(results, "view_name falls back to the id when only separators remain",
              wiki.view_name("///", "p-x") == "p-x",
              wiki.view_name("///", "p-x"))
        check(results, "view_name falls back to the id",
              wiki.view_name("  ..  ", "p-x") == "p-x",
              wiki.view_name("  ..  ", "p-x"))

        # Task 5: 观看区事实只落在账本里；这里不调用 ensure()，不碰观看区文件（写文件是 Task 6）。
        engine = wiki.Wiki(root)
        engine.config = json.loads((root / ".wiki/wiki.json").read_text(encoding="utf-8"))
        engine.conn = sqlite3.connect(root / ".wiki/wiki.db")
        engine.conn.row_factory = sqlite3.Row
        record = engine.page_record("anti-fraud-restore")
        published = engine.view_text(record)
        check(results, "view path follows the chinese title",
              engine.view_relpath(record) == "pages/process/反诈复机.md",
              engine.view_relpath(record))
        check(results, "view text aliases the id right after the title",
              f"title: {wiki.yaml_scalar(record['meta']['title'])}\n"
              f"aliases: [anti-fraud-restore]" in published,
              published)
        # R1（Task 6 取代 Task 5 的 R5）：view_digest/view_path 是"上一次发布出去的事实"，
        # 不是本行文本的摘要；必须先写盘、成功之后才回写事实，所以新页两列为空。
        stored = engine.latest_rev_row("anti-fraud-restore")
        check(results, "a fresh revision has no published facts yet",
              stored["view_path"] == "" and stored["view_digest"] == "",
              (stored["view_path"], stored["view_digest"]))
        check(results, "the ledger render stays alias-free",
              "aliases:" not in wiki.render_page(record["meta"], record["body"]),
              wiki.render_page(record["meta"], record["body"]))
        engine.close()

        bad = run(root, "submit", task["id"],
                  "--payload", payload_file(root, "again.json", payload), expect=3)
        check(results, "closed task rejects resubmit", bad["error"]["code"] == "E_TASK_STALE", bad)

        status = run(root, "status")["data"]
        check(results, "status counts page and pending", status["pages"] == 1, status)
        found = run(root, "query", "复机")["data"]
        check(results, "query finds the page", found["matches"] == 1, found)
        page = run(root, "query", "--page", "anti-fraud-restore")["data"]
        check(results, "query --page reads the ledger", "两个工作日" in page["body"], page)
        check(results, "query --page hides source digests", "source_digests" not in page["page"], page["page"])
        raw_paths = {item["key"]: item["raw"] for item in page["sources"]}
        check(results, "query --page renders the raw path of every source",
              bool(raw_paths) and all(
                  raw and (root / raw).is_file() for raw in raw_paths.values()),
              page["sources"])

        (root / "c.md").write_text("# 反诈复机\n\n补充：可拨打客服确认进度。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next", "--kind", "route")["data"]["tasks"][0]
        route_query(root, route, "复机")
        c_key = route["work"]["sources"][0]["key"]
        check(results, "route brief requires reason for every decision",
              "每条 decision 都必须写 reason" in " ".join(route["brief"]["steps"]), route["brief"])
        run(root, "submit", route["id"], "--payload", payload_file(
            root, "update-route.json",
            {"decisions": [{"source": c_key, "action": "merge", "page": "anti-fraud-restore",
                            "reason": "补充客服确认方式"}]},
        ))
        task = run(root, "next")["data"]["tasks"][0]
        check(results, "update template keeps old evidence",
              any(item.get("source") == "a.md" and item.get("quote")
                  for item in task["template"]["evidence"]), task["template"]["evidence"])
        update_body = (
            "## 结论\n\n停机后本人持证到厅复机，约两个工作日。\n\n"
            "## 步骤\n\n本人带身份证到营业厅提交复机申请。\n\n"
            "## 注意事项\n\n可拨打客服确认进度。\n"
        )
        update_payload = {
            "page": "anti-fraud-restore", "title": "反诈复机", "type": "process",
            "summary": "停机后本人持证到厅复机，约两个工作日；可拨打客服确认进度。",
            "links": [], "body": update_body,
            "evidence": [{"source": c_key, "quote": "可拨打客服确认进度"}],
            "boundaries": {"covered": "停机后的复机路径", "not_covered": "异地办理与资费"},
        }
        run(root, "submit", task["id"], "--payload", payload_file(root, "update-page.json", update_payload))

        # Task 6: 推送观看区（pages / raw / topics / workflows / README）----
        page_file = root / "wiki/pages/process/反诈复机.md"
        check(results, "the first revision was published with its own text",
              page_file.exists()
              and update_body.strip() not in page_file.read_text(encoding="utf-8"))

        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        carried = conn.execute(
            "SELECT rev, view_path, view_digest FROM page_revs "
            "WHERE page = 'anti-fraud-restore' ORDER BY rev DESC LIMIT 1"
        ).fetchone()
        conn.close()
        check(results, "a new revision carries the previous published facts",
              carried[0] == 2
              and carried[1] == "pages/process/反诈复机.md"
              and carried[2] == wiki.digest_text(page_file.read_text(encoding="utf-8")),
              carried)

        run(root, "status")  # ensure() → sync_view()：推送 rev 2
        published_text = page_file.read_text(encoding="utf-8")
        check(results, "sync_view publishes the newest revision",
              update_body.strip() in published_text)
        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        facts = conn.execute(
            "SELECT view_path, view_digest FROM page_revs "
            "WHERE page = 'anti-fraud-restore' ORDER BY rev DESC LIMIT 1"
        ).fetchone()
        conn.close()
        clean_digest = wiki.digest_text(published_text)
        check(results, "published facts describe the file on disk",
              facts[0] == "pages/process/反诈复机.md" and facts[1] == clean_digest,
              (facts[0], facts[1], clean_digest))

        (root / "wiki/笔记.md").write_text("我自己的笔记\n", encoding="utf-8")
        (root / "wiki/.obsidian").mkdir(exist_ok=True)
        (root / "wiki/.obsidian/app.json").write_text("{}\n", encoding="utf-8")
        (root / "wiki/raw/用户笔记.md").write_text("# 我的原文笔记\n", encoding="utf-8")

        page_file.unlink()
        run(root, "status")
        check(results, "a deleted published page is restored",
              page_file.exists() and update_body.strip() in page_file.read_text(encoding="utf-8"))

        page_file.write_text(published_text + "\n手改一段。\n", encoding="utf-8")
        hand_digest = wiki.digest_text(page_file.read_text(encoding="utf-8"))
        run(root, "status")
        check(results, "a hand edited published page is not overwritten",
              "手改一段。" in page_file.read_text(encoding="utf-8"))
        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        after_hand = conn.execute(
            "SELECT view_digest FROM page_revs WHERE page = 'anti-fraud-restore' "
            "ORDER BY rev DESC LIMIT 1"
        ).fetchone()[0]
        conn.close()
        check(results, "a hand edit leaves the published facts untouched",
              after_hand == clean_digest and after_hand != hand_digest,
              (after_hand, clean_digest, hand_digest))

        check(results, "user files in the view dir are untouched",
              (root / "wiki/笔记.md").read_text(encoding="utf-8") == "我自己的笔记\n"
              and (root / "wiki/.obsidian/app.json").exists()
              and (root / "wiki/raw/用户笔记.md").exists())

        readme = (root / "wiki/README.md").read_text(encoding="utf-8")
        check(results, "view readme lists directories and ownership",
              all(name in readme for name in ("pages/", "raw/", "workflows/", "topics/", "runs/"))
              and "由引擎维护" in readme and "项目内容" in readme
              and "首次投放" not in readme
              and "工作流副本" not in readme and "话题副本" not in readme,
              readme)
        (root / "wiki/README.md").unlink()
        run(root, "status")
        check(results, "view readme is rewritten on every push",
              (root / "wiki/README.md").exists())

        check(results, "view content dirs exist",
              (root / "wiki/workflows").is_dir() and (root / "wiki/topics").is_dir())
        check(results, "view does not seed root workflows",
              not (root / "wiki/workflows/account-opening").exists())
        check(results, "view does not seed root topics",
              not (root / "wiki/topics/复机办理.md").exists())

        page_file.unlink()  # 手改件留在树上会持续告警，也会干扰后面的漂移检查
        run(root, "status")
        check(results, "the hand edit is cleaned up for later blocks",
              page_file.exists() and "手改一段。" not in page_file.read_text(encoding="utf-8"))

        page = run(root, "query", "--page", "anti-fraud-restore")["data"]
        check(results, "update keeps old and new sources",
              set(page["page"]["sources"]) == {"a.md", c_key}, page["page"]["sources"])
        check(results, "update keeps old evidence",
              any(item.get("source") == "a.md" for item in page["page"]["evidence"]),
              page["page"]["evidence"])

        (root / "d.md").write_text("# 机房巡检\n\n巡检周期为每周一次。\n", encoding="utf-8")
        (root / "e.md").write_text("# 机房巡检\n\n巡检记录保留一年。\n", encoding="utf-8")
        run(root, "ingest")
        first = run(root, "next", "--kind", "route")["data"]["tasks"][0]
        route_query(root, first, "机房巡检")
        first_key = first["work"]["sources"][0]["key"]
        run(root, "submit", first["id"], "--payload", payload_file(
            root, "batch-route-1.json",
            {"decisions": [{"source": first_key, "action": "new", "page": "machine-room-inspection",
                            "title": "机房巡检", "type": "concept", "reason": "新主题"}]},
        ))
        second = run(root, "next", "--kind", "route")["data"]["tasks"][0]
        route_query(root, second, "机房巡检")
        second_key = second["work"]["sources"][0]["key"]
        check(results, "pending page is visible to the next route",
              any(item["id"] == "machine-room-inspection" for item in second["work"]["pending_pages"]),
              second["work"]["pending_pages"])
        run(root, "submit", second["id"], "--payload", payload_file(
            root, "batch-route-2.json",
            {"decisions": [{"source": second_key, "action": "merge", "page": "machine-room-inspection",
                            "reason": "同主题补充"}]},
        ))
        task = run(root, "next")["data"]["tasks"][0]
        check(results, "pending merge folds both sources into one page task",
              {item.get("source") for item in task["template"]["evidence"]} == {"d.md", "e.md"},
              task["template"]["evidence"])
        batch_payload = {
            "page": "machine-room-inspection", "title": "机房巡检", "type": "concept",
            "summary": "机房每周巡检一次，巡检记录保留一年。",
            "links": [], "body": "## 结论\n\n机房每周巡检一次，巡检记录保留一年。\n\n## 要点\n\n每周巡检一次；记录保留一年。\n",
            "evidence": [{"source": "d.md", "quote": "巡检周期为每周一次"},
                         {"source": "e.md", "quote": "巡检记录保留一年"}],
            "boundaries": {"covered": "巡检周期与记录保存", "not_covered": "巡检项目清单"},
        }
        run(root, "submit", task["id"], "--payload", payload_file(root, "batch-page.json", batch_payload))
        page = run(root, "query", "--page", "machine-room-inspection")["data"]
        check(results, "batch page keeps both sources",
              set(page["page"]["sources"]) == {"d.md", "e.md"}, page["page"]["sources"])

        edited = root / ".wiki/drafts/edited.md"
        edited.write_text("---\ntitle: 反诈复机\n---\n## 结论\n\n人工补充：可拨打客服确认进度。\n", encoding="utf-8")
        imported = run(root, "page", "import", "anti-fraud-restore", "--from", str(edited))["data"]
        check(results, "page import adds a revision", imported["rev"] == 3, imported)
        outside_edit = root.parent / f"{root.name}-edit.md"
        outside_edit.write_text("## 区外\n", encoding="utf-8")
        escaped = run(root, "page", "import", "anti-fraud-restore",
                      "--from", str(outside_edit), expect=2)
        check(results, "import outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)
        outside_edit.unlink()
        restored = run(root, "page", "restore", "anti-fraud-restore", "--rev", "1")["data"]
        restored_page = run(root, "query", "--page", "anti-fraud-restore")["data"]
        check(results, "page restore rolls back content",
              restored["rev"] == 4 and "两个工作日" in restored_page["body"], restored)

        aliased = root / ".wiki/drafts/aliased.md"
        aliased.write_text(
            "---\ntitle: 反诈复机\naliases: [anti-fraud-restore]\n---\n"
            "## 结论\n\n导入时 aliases 不应进入账本。\n",
            encoding="utf-8",
        )
        imported_alias = run(
            root, "page", "import", "anti-fraud-restore", "--from", str(aliased)
        )["data"]
        check(results, "page import accepts an aliased source file",
              imported_alias["rev"] == 5, imported_alias)
        aliased_page = run(root, "query", "--page", "anti-fraud-restore")["data"]
        check(results, "imported aliases do not leak into page meta",
              "aliases" not in aliased_page["page"], aliased_page["page"])

        linked = run(root, "page", "link", "anti-fraud-restore",
                     "machine-room-inspection")["data"]
        check(results, "page link adds an edge in a new revision",
              linked["rev"] == 6 and linked["added"] == ["machine-room-inspection"], linked)
        linked_page = run(root, "query", "--page", "anti-fraud-restore")["data"]
        check(results, "page link is visible in outgoing links",
              linked_page["outgoing"] == ["machine-room-inspection"], linked_page["outgoing"])
        linked_view = (root / "wiki/pages/process/反诈复机.md").read_text(encoding="utf-8")
        check(results, "page link pushes the relation section to the view",
              "## 关联" in linked_view and "[机房巡检](../concept/机房巡检.md)" in linked_view,
              linked_view)
        missing_link = run(root, "page", "link", "anti-fraud-restore",
                           "no-such-page", expect=2)
        check(results, "page link rejects an unknown target",
              missing_link["error"]["code"] == "E_LINK_UNRESOLVED", missing_link)
        self_link = run(root, "page", "link", "anti-fraud-restore",
                        "anti-fraud-restore", expect=2)
        check(results, "page link rejects linking to itself",
              self_link["error"]["code"] == "E_USAGE", self_link)
        blank_link = run(root, "page", "link", "anti-fraud-restore", " ", expect=2)
        check(results, "page link rejects blank targets",
              blank_link["error"]["code"] == "E_USAGE", blank_link)
        repeated = run(root, "page", "link", "anti-fraud-restore",
                       "machine-room-inspection")["data"]
        check(results, "page link does not rewrite an existing edge",
              repeated["rev"] == 6 and repeated["added"] == [], repeated)

        exported = run(root, "export", str(root / ".wiki/export"))["data"]["files"]
        check(results, "export writes rendered pages", exported, exported)
        exported_page = (root / ".wiki/export/process/anti-fraud-restore.md").read_text(
            encoding="utf-8")
        check(results, "export renders real page links",
              "[机房巡检](../concept/machine-room-inspection.md)" in exported_page, exported_page)
        escaped = run(root, "export", str(root.parent / "wiki-export"), expect=2)
        check(results, "export outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)

        config_path = root / ".wiki/wiki.json"
        config = json.loads(config_path.read_text(encoding="utf-8"))
        for bad in ("../x", ".wiki", "a/b", "workflows", "", ".", "..", ".hidden", "/abs", "a\\b", "topics"):
            config["publish"]["dir"] = bad
            config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
            escaped = run(root, "status", expect=2)
            check(results, f"publish dir {bad} rejected",
                  escaped["error"]["code"] == "E_CONFIG", escaped)
        config["publish"]["dir"] = "wiki"
        config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
        config["ingest"]["max_source_bytes"] = 10
        config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
        (root / "big.md").write_text("x" * 100, encoding="utf-8")
        oversized = run(root, "ingest", expect=1)["data"]
        check(results, "oversize classified", "big.md" in {item["key"] for item in oversized["oversize"]}, oversized)
        config["ingest"]["max_source_bytes"] = 50_000_000
        config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")

        config["publish"] = {"dir": "../x", "enabled": False, "topics": True, "workflows": True}
        config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
        disabled = run(root, "status", expect=2)
        check(results, "invalid publish dir is rejected even when disabled",
              disabled["error"]["code"] == "E_CONFIG", disabled)

        config["publish"] = {"dir": "view", "enabled": True, "topics": True, "workflows": True}
        config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
        (root / "view").write_text("占位：同名文件不是目录\n", encoding="utf-8")
        blocked = run(root, "status", expect=3)
        check(results, "publish dir that is a file reports E_STATE",
              blocked.get("error", {}).get("code") == "E_STATE", blocked)
        (root / "view").unlink()
        outside_view = root.parent / f"{root.name}-view"
        outside_view.mkdir()
        try:
            (root / "view").symlink_to(outside_view, target_is_directory=True)
        except (OSError, NotImplementedError):
            pass
        else:
            escaping = run(root, "status", expect=2)
            check(results, "publish dir symlink outside the workspace reports E_PATH",
                  escaping.get("error", {}).get("code") == "E_PATH", escaping)
            (root / "view").unlink()
        shutil.rmtree(outside_view, ignore_errors=True)
        seen = run(root, "status")["data"]
        check(results, "a legal non-default publish dir is created",
              (root / "view/raw").is_dir() and (root / "view/README.md").exists(), seen)
        raw_dir = root / "view/raw"
        shutil.rmtree(raw_dir)
        raw_dir.write_text("占位：同名文件不是目录\n", encoding="utf-8")
        blocked_raw = run(root, "status", expect=3)
        check(results, "publish raw dir that is a file reports E_STATE",
              blocked_raw.get("error", {}).get("code") == "E_STATE", blocked_raw)
        raw_dir.unlink()
        outside_raw = root.parent / f"{root.name}-raw"
        outside_raw.mkdir()
        try:
            raw_dir.symlink_to(outside_raw, target_is_directory=True)
        except (OSError, NotImplementedError):
            pass
        else:
            escaping_raw = run(root, "status", expect=2)
            check(results, "publish raw symlink outside the workspace reports E_PATH",
                  escaping_raw.get("error", {}).get("code") == "E_PATH", escaping_raw)
            raw_dir.unlink()
        shutil.rmtree(outside_raw, ignore_errors=True)
        (root / "view/note.md").write_text("# 视图笔记\n\n用户放进视图的便签。\n", encoding="utf-8")
        ingested = run(root, "ingest")["data"]["ingested"]
        check(results, "the publish dir is skipped by ingest",
              not any(item["key"].startswith("view/") for item in ingested), ingested)

        # 回归护栏：任务排队后源文件又变了，submit 必须报 E_SOURCE_DRIFT（退出码 3）。
        (root / "f.md").write_text("# 复机进度查询\n\n可拨打客服确认进度。\n", encoding="utf-8")
        run(root, "ingest")
        # 发布会切到 view 之后，旧的 wiki/ 目录成了普通素材，前面的 ingest 会为它排队；
        # 这里一次取够所有路由任务，只留下 f.md 的那条，其余放回队列。
        routes = run(root, "next", "--kind", "route", "--batch", "50")["data"]["tasks"]
        drift_route = None
        for task in routes:
            if task["work"]["sources"][0]["key"] == "f.md":
                drift_route = task
            else:
                run(root, "release", task["id"])
        route_query(root, drift_route, "复机")
        drift_key = drift_route["work"]["sources"][0]["key"]
        run(root, "submit", drift_route["id"], "--payload", payload_file(root, "drift-route.json", {"decisions": [
            {"source": drift_key, "action": "new", "page": "restore-progress",
             "title": "复机进度查询", "type": "process", "reason": "独立主题"},
        ]}))
        drift_task = run(root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]

        # --- Task 4: 快照八字段、领取/提交失效、树唯一性 -------------------------
        # 快照要同时认下"源文件版本"和"原文版本"，缺一侧就判不出原文被改。
        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        snap = json.loads(conn.execute(
            "SELECT inputs FROM tasks WHERE id = ?", (drift_task["id"],)
        ).fetchone()[0])["sources"][0]
        conn.close()
        check(results, "the page task snapshot carries all nine fields",
              set(snap) == {"key", "raw_id", "source_size", "source_mtime", "source_digest",
                            "raw", "raw_size", "raw_mtime", "raw_digest"}, snap)
        check(results, "the snapshot names the raw the source mounts",
              snap["raw_id"] == snap["raw_digest"], snap)
        check(results, "the snapshot records both the source and its raw copy",
              snap["raw"] != "" and snap["source_size"] > 0 and snap["raw_size"] > 0
              and len(snap["source_digest"]) == 32 and len(snap["raw_digest"]) == 32, snap)

        # CLI 路径会先被 ingest 判重与 attach_task_source 拦住，所以越界组合只能在
        # 进程内直接调 enqueue_task 才看得到：另一个 page 树想用同一个源必须被拒。
        engine = wiki.Wiki(root)
        engine.ensure()
        conflict = None
        try:
            engine.enqueue_task("page", "restore-progress-copy", {
                "page": "restore-progress-copy", "mode": "create",
                "sources": [engine.source_snapshot(drift_key)],
            })
        except wiki.WikiError as error:
            conflict = error
        engine.close()
        check(results, "a second task tree for the same source is refused",
              conflict is not None and conflict.code == "E_TASK_CONFLICT", conflict)
        check(results, "the conflict names the task that already holds the source",
              conflict is not None and drift_task["id"] in conflict.message,
              getattr(conflict, "message", conflict))

        # 领取侧失效：放回队列后改源、重新 ingest，next 不能再把它发出来。
        run(root, "release", drift_task["id"])
        (root / "f.md").write_text("# 复机进度查询\n\n拨打 10086 可查进度。\n", encoding="utf-8")
        run(root, "ingest")
        after = run(root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"]
        check(results, "next never hands out a task whose source moved", after == [], after)
        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        expired = conn.execute(
            "SELECT state, last_error, closed_at FROM tasks WHERE id = ?", (drift_task["id"],)
        ).fetchone()
        conn.close()
        check(results, "an invalidated task keeps its row, reason and close time",
              expired is not None and expired[0] == "expired"
              and expired[1] and expired[2], expired)

        # 失效不是放弃：过期之后来源必须还能重新变成工作项，否则 next 永远空、
        # status 永远提示 "run ingest"，整个库没有任何出口。
        requeued = run(root, "ingest")["data"]
        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        reopened = conn.execute(
            "SELECT COUNT(*) FROM task_sources ts JOIN tasks t ON t.id = ts.task_id "
            "WHERE ts.source_key = ? AND t.state = 'queued'",
            (drift_key,),
        ).fetchone()[0]
        conn.close()
        check(results, "an expired source is queued again on the next ingest",
              requeued["queued_tasks"] != [] and reopened == 1, (requeued, reopened))
        claimed = run(root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"]
        check(results, "the requeued source can be claimed again",
              len(claimed) == 1, claimed)

        # 提交侧失效：任务已被判 expired，submit 仍要报 E_SOURCE_DRIFT，且不能复活它。
        body_file(root)
        drifted = run(root, "submit", drift_task["id"], "--payload", payload_file(
            root, "drift-page.json",
            {**payload, "page": "restore-progress", "title": "复机进度查询",
             "summary": "可拨打客服确认进度。",
             "evidence": [{"source": drift_key, "quote": "可拨打客服确认进度。"}]}), expect=3)
        check(results, "a source change after the page task still reports E_SOURCE_DRIFT",
              drifted["error"]["code"] == "E_SOURCE_DRIFT", drifted)
        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        revived = conn.execute(
            "SELECT state FROM tasks WHERE id = ?", (drift_task["id"],)
        ).fetchone()[0]
        conn.close()
        check(results, "a failed submit does not revive the expired task", revived == "expired", revived)

        # --- Task 5: 只有被引用的原文会判 stale -------------------------------
        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        raw_d = conn.execute(
            "SELECT raw FROM sources WHERE key = 'd.md'"
        ).fetchone()[0]
        conn.close()
        before = run(root, "query", "--page", "machine-room-inspection")["data"]["page"]
        check(results, "the page is stable before its raw copy changes",
              before["status"] == "stable", before)
        raw_text = (root / raw_d).read_text(encoding="utf-8")
        raw_link = re.search(r"- \[`d.md`\]\(([^)]+)\)", raw_text)
        check(results, "the raw copy links to its source",
              "<!-- llm-wiki:provenance -->" in raw_text
              and "## 原文来源" in raw_text
              and raw_link is not None
              and ((root / raw_d).parent / raw_link.group(1)).resolve().is_file(),
              raw_text)
        (root / raw_d).write_text(
            raw_text.split("\n<!-- llm-wiki:provenance -->")[0] + "\n",
            encoding="utf-8",
        )
        run(root, "ingest")
        repaired_raw = (root / raw_d).read_text(encoding="utf-8")
        check(results, "ingest repairs the provenance footer on an old raw copy",
              "<!-- llm-wiki:provenance -->" in repaired_raw
              and "[`d.md`]" in repaired_raw, repaired_raw)
        (root / raw_d).write_text(
            (root / raw_d).read_text(encoding="utf-8") + "\n巡检记录留档。\n",
            encoding="utf-8",
        )
        run(root, "ingest")
        stale = run(root, "query", "--page", "machine-room-inspection")["data"]["page"]
        check(results, "editing a cited raw copy makes the page stale",
              stale["status"] == "stale", stale)
        untouched = run(root, "query", "--page", "anti-fraud-restore")["data"]["page"]
        check(results, "editing an unrelated raw copy leaves the page stable",
              untouched["status"] == "stable", untouched)

        # --- Task 7: sync 反向回收（观看区手改回到账本）-----------------------
        check(results, "split_evidence leaves a plain body alone",
              wiki.split_evidence("## 要点\n\n正文。\n") == ("## 要点\n\n正文。\n", []))
        check(results, "split_evidence parses the trailing block",
              wiki.split_evidence("## 要点\n\n正文。\n\n## 证据\n\n- `a.md`：引文。\n")
              == ("## 要点\n\n正文。", [{"source": "a.md", "quote": "引文。"}]))
        check(results, "split_evidence keeps a broken block in the body",
              wiki.split_evidence("## 要点\n\n正文。\n\n## 证据\n\n随手写的。\n")
              == ("## 要点\n\n正文。\n\n## 证据\n\n随手写的。\n", []))
        check(results, "split_evidence parses rendered evidence links",
              wiki.split_evidence(
                  "## 要点\n\n正文。\n\n## 证据\n\n"
                  "- [`wiki/raw/a.md`](../../raw/a.md)（`a.md`）：引文。\n"
              ) == ("## 要点\n\n正文。", [{"source": "a.md", "quote": "引文。"}]))
        provenance_body = (
            "正文。\n\n<!-- llm-wiki:provenance -->\n\n## 原文来源\n\n"
            "- [`files/a.pdf`](../../files/a.pdf)\n"
        )
        check(results, "split_provenance strips generated source links",
              split_provenance(provenance_body)
              == ("正文。", [{"source": "files/a.pdf", "target": "../../files/a.pdf"}]))
        hand_written = "正文。\n\n## 原文来源\n\n手写。\n"
        check(results, "split_provenance leaves hand-written sections alone",
              split_provenance(hand_written) == (hand_written, []))

        room_view = root / "view/pages/concept/机房巡检.md"
        # query 现在是只读快照，不再顺带推送观看区；先显式推一次再验证 sync 的幂等性。
        run(root, "sync")
        room_text = room_view.read_text(encoding="utf-8")
        synced = run(root, "sync")["data"]
        check(results, "sync leaves an untouched view alone",
              synced["unchanged"] == ["anti-fraud-restore", "machine-room-inspection"], synced)
        check(results, "sync reports the six keys",
              set(synced) == {"imported", "skipped", "failed", "unchanged", "written",
                              "warnings"}, synced)

        room_view.write_text(
            room_text.replace("每周巡检一次；记录保留一年。", "每周巡检两次；记录保留两年。"),
            encoding="utf-8",
        )
        synced = run(root, "sync")["data"]
        check(results, "sync adopts a hand edited body",
              "machine-room-inspection" in synced["imported"], synced)
        check(results, "sync writes the canonical text back",
              "每周巡检两次；记录保留两年。" in room_view.read_text(encoding="utf-8"),
              room_view.read_text(encoding="utf-8"))
        page = run(root, "query", "--page", "machine-room-inspection")["data"]
        check(results, "the adopted body lands in the ledger",
              "每周巡检两次" in page["body"], page["body"])
        check(results, "sync keeps the ledger metadata",
              page["page"]["title"] == "机房巡检", page["page"])
        conn = sqlite3.connect(root / ".wiki/wiki.db")
        facts = conn.execute(
            "SELECT rev, view_path, view_digest FROM page_revs "
            "WHERE page = 'machine-room-inspection' ORDER BY rev DESC LIMIT 1"
        ).fetchone()
        conn.close()
        check(results, "sync records the published facts",
              facts[0] == 2 and facts[1] == "pages/concept/机房巡检.md"
              and facts[2] == wiki.digest_text(room_view.read_text(encoding="utf-8")), facts)

        again = run(root, "sync")["data"]
        check(results, "sync is idempotent and does not freeze the page",
              again["unchanged"] == ["anti-fraud-restore", "machine-room-inspection"], again)

        room_view.write_text(room_view.read_text(encoding="utf-8").replace(
            f"summary: {wiki.yaml_scalar('机房每周巡检一次，巡检记录保留一年。')}",
            f"summary: {wiki.yaml_scalar('改过的摘要')}"), encoding="utf-8")
        refused = run(root, "sync", expect=1)["data"]
        check(results, "sync refuses a metadata edit",
              any(item["code"] == "E_META_CHANGED" for item in refused["failed"]), refused)
        check(results, "a refused page keeps its ledger summary",
              run(root, "query", "--page", "machine-room-inspection")["data"]["page"]["summary"]
              == "机房每周巡检一次，巡检记录保留一年。")
        check(results, "a refused page file is left alone",
              "改过的摘要" in room_view.read_text(encoding="utf-8"),
              room_view.read_text(encoding="utf-8"))

        room_view.write_text(room_view.read_text(encoding="utf-8").replace(
            "每周巡检两次；记录保留两年。", "每周巡检三次。"), encoding="utf-8")
        adopted = run(root, "sync", "--body-only")["data"]
        check(results, "--body-only adopts the body",
              "machine-room-inspection" in adopted["imported"], adopted)
        page = run(root, "query", "--page", "machine-room-inspection")["data"]
        check(results, "--body-only keeps the ledger metadata",
              page["page"]["summary"] == "机房每周巡检一次，巡检记录保留一年。", page["page"])
        check(results, "--body-only rewrites the canonical text",
              "改过的摘要" not in room_view.read_text(encoding="utf-8")
              and "每周巡检三次" in room_view.read_text(encoding="utf-8"))

        handmade = root / "view/pages/concept/我的笔记.md"
        handmade.write_text("# 随手记\n\n不属于账本。\n", encoding="utf-8")
        stranger = root / "view/pages/concept/陌生人.md"
        stranger.write_text("---\nid: no-such-page\n---\n\n## 内容\n\n别人的文件。\n", encoding="utf-8")
        skips = run(root, "sync")["data"]
        check(results, "sync skips files without an id",
              {"file": "pages/concept/我的笔记.md", "reason": "no id"} in skips["skipped"],
              skips["skipped"])
        check(results, "sync skips unknown ids",
              {"file": "pages/concept/陌生人.md", "reason": "unknown id"} in skips["skipped"],
              skips["skipped"])
        check(results, "skipped files are left untouched",
              handmade.read_text(encoding="utf-8") == "# 随手记\n\n不属于账本。\n"
              and stranger.read_text(encoding="utf-8").startswith("---\nid: no-such-page"))

        conn = sqlite3.connect(root / ".wiki/wiki.db")
        before_rev = conn.execute(
            "SELECT MAX(rev) FROM page_revs WHERE page = 'machine-room-inspection'"
        ).fetchone()[0]
        conn.close()
        copy_path = root / "view/pages/concept/机房巡检-副本.md"
        copy_path.write_text(room_view.read_text(encoding="utf-8"), encoding="utf-8")
        duplicate = run(root, "sync", expect=1)["data"]
        conn = sqlite3.connect(root / ".wiki/wiki.db")
        after_rev = conn.execute(
            "SELECT MAX(rev) FROM page_revs WHERE page = 'machine-room-inspection'"
        ).fetchone()[0]
        conn.close()
        check(results, "duplicate ids fail with E_DUPLICATE",
              sorted(item["code"] for item in duplicate["failed"]
                     if item["page"] == "machine-room-inspection")
              == ["E_DUPLICATE", "E_DUPLICATE"], duplicate["failed"])
        check(results, "duplicates are not adopted",
              "machine-room-inspection" not in duplicate["imported"] and before_rev == after_rev,
              (duplicate["imported"], before_rev, after_rev))
        check(results, "one failing page does not stop the run",
              "anti-fraud-restore" in duplicate["unchanged"], duplicate)
        human = subprocess.run([sys.executable, str(SCRIPT), "sync"], cwd=root,
                               capture_output=True, text=True)
        check(results, "sync prints counts and failure codes",
              human.returncode == 1 and "E_DUPLICATE" in human.stdout, human.stdout)
        copy_path.unlink()

        room_view.write_text(room_view.read_text(encoding="utf-8").replace(
            "巡检周期为每周一次", "巡检周期为每周两次"), encoding="utf-8")
        run(root, "sync")
        page = run(root, "query", "--page", "machine-room-inspection")["data"]
        check(results, "sync parses the evidence block back into meta",
              any(item["source"] == "d.md" and item["quote"] == "巡检周期为每周两次"
                  for item in page["page"]["evidence"]), page["page"]["evidence"])
        check(results, "the evidence text stays out of the body",
              "## 证据" not in page["body"] and "巡检周期为每周两次" not in page["body"],
              page["body"])
        check(results, "the canonical text keeps one evidence block",
              room_view.read_text(encoding="utf-8").count("## 证据") == 1,
              room_view.read_text(encoding="utf-8"))
        linked = re.search(r"- \[`[^`]+`\]\(([^)]*)\)（`d.md`）：",
                           room_view.read_text(encoding="utf-8"))
        check(results, "page evidence links to the raw copy and names the source",
              linked is not None
              and (room_view.parent / linked.group(1)).resolve().is_file(),
              room_view.read_text(encoding="utf-8"))

        room_view.write_text(re.sub(
            r"- \[`[^`]+`\]\([^)]*\)（`d.md`）：巡检周期为每周两次",
            "- d.md 巡检周期为每周两次",
            room_view.read_text(encoding="utf-8"),
        ), encoding="utf-8")
        broken = subprocess.run([sys.executable, str(SCRIPT), "sync", "--json"], cwd=root,
                                capture_output=True, text=True)
        check(results, "an unparseable evidence block warns",
              broken.returncode == 0 and "warning" in broken.stderr, broken.stderr)
        broken_data = json.loads(broken.stdout)["data"]
        check(results, "an unparseable evidence block is reported in warnings",
              len(broken_data["warnings"]) == 1
              and broken_data["warnings"][0].startswith("pages/concept/机房巡检.md:")
              and "evidence block does not parse" in broken_data["warnings"][0],
              broken_data["warnings"])
        page = run(root, "query", "--page", "machine-room-inspection")["data"]
        check(results, "a broken block keeps the ledger evidence",
              any(item["source"] == "d.md" and item["quote"] == "巡检周期为每周两次"
                  for item in page["page"]["evidence"]), page["page"]["evidence"])
        check(results, "a broken block is adopted as body text",
              "d.md 巡检周期为每周两次" in page["body"], page["body"])

        canonical = room_view.read_text(encoding="utf-8")
        head = canonical.split("\n---\n", 1)[0] + "\n---\n"
        room_view.write_text(head, encoding="utf-8")
        empty = run(root, "sync", expect=1)["data"]
        check(results, "an empty body is refused with E_PAYLOAD",
              any(item["code"] == "E_PAYLOAD" for item in empty["failed"]), empty)
        room_view.write_text(canonical, encoding="utf-8")

        rename_draft = root / ".wiki/drafts/rename.md"
        rename_draft.write_text(
            "---\ntitle: 机房巡检记录\n---\n## 要点\n\n每周巡检三次。\n", encoding="utf-8")
        run(root, "page", "import", "machine-room-inspection", "--from", str(rename_draft))
        old_path = root / "view/pages/concept/机房巡检.md"
        new_path = root / "view/pages/concept/机房巡检记录.md"
        moved = run(root, "sync")["data"]
        check(results, "sync publishes a ledger-side title change",
              any(item["page"] == "machine-room-inspection"
                  and item["to"] == "pages/concept/机房巡检记录.md" for item in moved["written"])
              and new_path.exists(), moved)
        check(results, "the old path is gone after the rename",
              not old_path.exists(), old_path.exists())
        check(results, "the new path carries the new title",
              'title: "机房巡检记录"' in new_path.read_text(encoding="utf-8"),
              new_path.read_text(encoding="utf-8"))
        conn = sqlite3.connect(root / ".wiki/wiki.db")
        moved_facts = conn.execute(
            "SELECT rev, view_path, view_digest FROM page_revs "
            "WHERE page = 'machine-room-inspection' ORDER BY rev DESC LIMIT 1"
        ).fetchone()
        conn.close()
        check(results, "the renamed page's facts follow the new path",
              moved_facts[1] == "pages/concept/机房巡检记录.md"
              and moved_facts[2] == wiki.digest_text(new_path.read_text(encoding="utf-8")),
              moved_facts)

        moved_away = root / "view/pages/concept/临时位置.md"
        new_path.rename(moved_away)
        returned = run(root, "sync")["data"]
        check(results, "a page file moved by hand is put back",
              any(item["page"] == "machine-room-inspection"
                  and item["from"] == "pages/concept/临时位置.md"
                  and item["to"] == "pages/concept/机房巡检记录.md" for item in returned["written"])
              and new_path.exists() and not moved_away.exists(), returned)

        room_before = run(root, "query", "--page", "machine-room-inspection")["data"]
        linked_import = root / ".wiki/drafts/linked.md"
        linked_import.write_text(
            "---\ntitle: 机房巡检记录\nlinks:\n  - anti-fraud-restore\n---\n"
            + room_before["body"],
            encoding="utf-8",
        )
        imported_link = run(
            root, "page", "import", "machine-room-inspection", "--from", str(linked_import)
        )["data"]
        check(results, "page import accepts a frontmatter link list",
              imported_link["rev"] == room_before["rev"] + 1, imported_link)
        imported_page = run(root, "query", "--page", "machine-room-inspection")["data"]
        check(results, "imported links join the page edges",
              "anti-fraud-restore" in imported_page["outgoing"], imported_page["outgoing"])
        imported_view = new_path.read_text(encoding="utf-8")
        check(results, "imported links reach the view after the next push",
              "[反诈复机](../process/反诈复机.md)" in imported_view, imported_view)

        bad_link = root / ".wiki/drafts/bad-link.md"
        bad_link.write_text(
            "---\ntitle: 机房巡检记录\nlinks:\n  - no-such-page\n---\n"
            "## 要点\n\nunknown link.\n",
            encoding="utf-8",
        )
        rejected = run(root, "page", "import", "machine-room-inspection",
                       "--from", str(bad_link), expect=2)
        check(results, "page import rejects an unknown link",
              rejected["error"]["code"] == "E_LINK_UNRESOLVED", rejected)
        stable = run(root, "query", "--page", "machine-room-inspection")["data"]
        check(results, "a rejected import leaves the page untouched",
              stable["rev"] == imported_link["rev"]
              and "anti-fraud-restore" in stable["outgoing"], stable)

        status = run(root, "status")["data"]
        check(results, "status counts invalidated tasks without opening them",
              status["tasks"].get("expired", 0) >= 1
              and status["tasks"]["open"] == status["tasks"]["queued"] + status["tasks"]["leased"],
              status["tasks"])

        # 树唯一性不变量：只统计 page 树。submit_route 在关闭 route 任务前先建 page 树，
        # route→page 的交接瞬间同一个 key 会同时挂两行，那是正常交接不是第二套树。
        # 同一次 route 拆页是显式例外，下面单独测。
        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        twins = conn.execute(
            "SELECT ts.source_key, COUNT(DISTINCT ts.task_id) AS n "
            "FROM task_sources ts JOIN tasks t ON t.id = ts.task_id "
            "WHERE t.kind = 'page' AND t.state IN ('queued', 'leased') "
            "GROUP BY ts.source_key HAVING n > 1"
        ).fetchall()
        conn.close()
        check(results, "no source sits in two open page trees outside a route split",
              twins == [], twins)

        # 同一来源拆多页：同一次 route 提交的两条 new 决定共享 route_task，
        # 是树唯一性的唯一例外；两个 page 任务都能领取、提交。
        split_root = Path(tempfile.mkdtemp(prefix="llm-wiki-split-"))
        try:
            (split_root / "talk.md").write_text(
                "# 云网安全讲话\n\n主题日活动启动。\n\n双零行动：变更操作零失误。\n",
                encoding="utf-8",
            )
            run(split_root, "ingest")
            split_route = run(split_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
            split_key = split_route["work"]["sources"][0]["key"]
            duplicate = run(
                split_root, "submit", split_route["id"],
                "--payload", payload_file(split_root, "split-dup.json", {"decisions": [
                    {"source": split_key, "action": "new", "page": "same-page",
                     "title": "同页", "type": "concept", "reason": "重复"},
                    {"source": split_key, "action": "new", "page": "same-page",
                     "title": "同页", "type": "concept", "reason": "重复"},
                ]}),
                expect=2,
            )
            check(results, "the same source cannot decide one page twice",
                  duplicate["error"]["code"] == "E_PAYLOAD", duplicate)
            split_route = run(split_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
            mixed = run(
                split_root, "submit", split_route["id"],
                "--payload", payload_file(split_root, "split-mixed.json", {"decisions": [
                    {"source": split_key, "action": "skip", "reason": "跳过"},
                    {"source": split_key, "action": "new", "page": "mixed-page",
                     "title": "混用", "type": "concept", "reason": "混用"},
                ]}),
                expect=2,
            )
            check(results, "skip cannot be combined with new or merge",
                  mixed["error"]["code"] == "E_PAYLOAD", mixed)
            split_route = run(split_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
            route_query(split_root, split_route, "主题日", "双零")
            split_submit = run(
                split_root, "submit", split_route["id"],
                "--payload", payload_file(split_root, "split-route.json", {"decisions": [
                    {"source": split_key, "action": "new", "page": "theme-day",
                     "title": "主题日活动", "type": "concept", "reason": "活动本身"},
                    {"source": split_key, "action": "new", "page": "dual-zero",
                     "title": "双零行动", "type": "concept", "reason": "独立行动"},
                ]}),
            )
            check(results, "one source can be split into two queued pages",
                  len(split_submit["data"]["queued_page_tasks"]) == 2, split_submit)
            split_tasks = run(split_root, "next", "--kind", "page", "--batch", "5")["data"]["tasks"]
            check(results, "both split page tasks are claimable", len(split_tasks) == 2, split_tasks)
            split_bodies = {
                "theme-day": "## 结论\n\n主题日活动启动。\n\n## 要点\n\n- 主题：云网安全。\n",
                "dual-zero": "## 结论\n\n双零行动启动。\n\n## 要点\n\n- 目标：变更操作零失误。\n",
            }
            for index, split_task in enumerate(split_tasks):
                page_id = split_task["template"]["page"]
                draft = split_root / ".wiki/drafts" / f"{page_id}.md"
                draft.parent.mkdir(parents=True, exist_ok=True)
                draft.write_text(split_bodies[page_id], encoding="utf-8")
                quote = "主题日活动启动。" if page_id == "theme-day" else "变更操作零失误"
                accepted = run(
                    split_root, "submit", split_task["id"],
                    "--payload", payload_file(split_root, f"{page_id}.json", {
                        "page": page_id,
                        "title": "主题日活动" if page_id == "theme-day" else "双零行动",
                        "type": "concept",
                        "summary": "一句话摘要。",
                        "links": [],
                        "body_file": str(draft.relative_to(split_root)),
                        "evidence": [{"source": split_key, "quote": quote}],
                        "boundaries": {"covered": "这一页", "not_covered": "其他"},
                    }),
                )
                check(results, f"split page {page_id} submits", accepted["data"]["rev"] == 1, accepted)
                conn = sqlite3.connect(split_root / ".wiki" / "wiki.db")
                source_status = conn.execute(
                    "SELECT status FROM sources WHERE key = ?", (split_key,)
                ).fetchone()[0]
                conn.close()
                expected = "done" if index == 1 else "pending"
                check(results, f"the split source is {expected} after page {index + 1}",
                      source_status == expected, source_status)
        finally:
            shutil.rmtree(split_root, ignore_errors=True)

        # raw 是内容寻址的一等对象：同一份正文只落一份，人工排版另起一行。
        raw_root = Path(tempfile.mkdtemp(prefix="llm-wiki-raw-"))
        try:
            shared = "# 共享正文\n\n同一份内容，两个路径。\n"
            (raw_root / "a.md").write_text(shared, encoding="utf-8")
            (raw_root / "copy").mkdir()
            (raw_root / "copy" / "a.md").write_text(shared, encoding="utf-8")
            run(raw_root, "ingest")
            raw_files = sorted(p.name for p in (raw_root / "wiki/raw").glob("*.md"))
            conn = sqlite3.connect(raw_root / ".wiki" / "wiki.db")
            raws = conn.execute("SELECT id, name, path FROM raws").fetchall()
            mounted = conn.execute("SELECT key, raw_id FROM sources ORDER BY key").fetchall()
            conn.close()
            check(results, "two sources with one body share one raw",
                  len(raw_files) == 1 and len(raws) == 1
                  and {row[1] for row in mounted} == {raws[0][0]},
                  (raw_files, raws, mounted))
            again = run(raw_root, "ingest")["data"]
            check(results, "a shared raw is not parsed twice",
                  again["ingested"] == []
                  and len(list((raw_root / "wiki/raw").glob("*.md"))) == 1,
                  again["ingested"])

            # 一份正文一条任务：subject 是 raw id，任务带该正文的全部挂载路径。
            raw_id = raws[0][0]
            conn = sqlite3.connect(raw_root / ".wiki" / "wiki.db")
            route_rows = conn.execute(
                "SELECT id, subject FROM tasks WHERE kind = 'route'"
            ).fetchall()
            mounts = conn.execute(
                "SELECT source_key FROM task_sources WHERE task_id = ? ORDER BY source_key",
                (route_rows[0][0],),
            ).fetchall()
            conn.close()
            check(results, "one raw queues one route task covering every mount",
                  len(route_rows) == 1 and route_rows[0][1] == raw_id
                  and [row[0] for row in mounts] == ["a.md", "copy/a.md"],
                  (route_rows, mounts))

            # 同一正文再挂一个路径：不算漂移，直接并进已排队的任务。
            (raw_root / "copy" / "b.md").write_text(shared, encoding="utf-8")
            mounted = run(raw_root, "ingest")["data"]
            conn = sqlite3.connect(raw_root / ".wiki" / "wiki.db")
            task_count = conn.execute("SELECT COUNT(*) FROM tasks WHERE kind = 'route'").fetchone()[0]
            mounts = conn.execute(
                "SELECT source_key FROM task_sources WHERE task_id = ? ORDER BY source_key",
                (route_rows[0][0],),
            ).fetchall()
            digests = conn.execute("SELECT raw_digest FROM sources").fetchall()
            conn.close()
            check(results, "a new mount joins the open task without drift",
                  not mounted["queued_tasks"] and task_count == 1
                  and [row[0] for row in mounts] == ["a.md", "copy/a.md", "copy/b.md"]
                  and {row[0] for row in digests} == {raw_id},
                  (mounted, task_count, mounts, digests))

            gone = subprocess.run(
                [sys.executable, str(SCRIPT), "read", "a.md", "--json"],
                cwd=raw_root, capture_output=True, text=True,
            )
            check(results, "the read command is gone",
                  gone.returncode == 2 and "invalid choice" in gone.stderr,
                  gone.stderr[-200:])

        finally:
            shutil.rmtree(raw_root, ignore_errors=True)

        # 孤儿页：来源全没了、又没有出入链，ingest 末尾连同观看区文件一起删。
        orphan_root = Path(tempfile.mkdtemp(prefix="llm-wiki-orphan-"))
        try:
            (orphan_root / "a.md").write_text("# 孤儿来源\n\n唯一正文。\n", encoding="utf-8")
            run(orphan_root, "ingest")
            route_task = run(orphan_root, "next", "--kind", "route")["data"]["tasks"][0]
            route_query(orphan_root, route_task, "孤儿")
            run(orphan_root, "submit", route_task["id"], "--payload", payload_file(
                orphan_root, "orphan-route.json",
                {"decisions": [{"source": "a.md", "action": "new", "page": "orphan-page",
                                "title": "孤儿页", "type": "concept", "reason": "唯一来源"}]},
            ))
            page_task = run(orphan_root, "next", "--kind", "page")["data"]["tasks"][0]
            draft = orphan_root / ".wiki/drafts/orphan.md"
            draft.parent.mkdir(parents=True, exist_ok=True)
            draft.write_text("## 结论\n\n唯一正文。\n\n## 要点\n\n- 唯一来源。\n",
                             encoding="utf-8")
            run(orphan_root, "submit", page_task["id"], "--payload", payload_file(
                orphan_root, "orphan-page.json",
                {"page": "orphan-page", "title": "孤儿页", "type": "concept",
                 "summary": "一句话摘要。", "links": [],
                 "body_file": ".wiki/drafts/orphan.md",
                 "evidence": [{"source": "a.md", "quote": "唯一正文。"}],
                 "boundaries": {"covered": "这一页", "not_covered": "其他"}},
            ))
            page_view = orphan_root / "wiki/pages/concept/孤儿页.md"
            (orphan_root / "a.md").unlink()
            cleaned = run(orphan_root, "ingest")["data"]
            check(results, "a page whose sources are all gone is removed at ingest",
                  cleaned["orphaned"] == ["orphan-page"] and not page_view.exists(),
                  (cleaned, page_view.exists()))
        finally:
            shutil.rmtree(orphan_root, ignore_errors=True)

        # 有链的孤儿只标 stale：删了会让其他页的链接悬空。
        linked_root = Path(tempfile.mkdtemp(prefix="llm-wiki-orphan-link-"))
        try:
            (linked_root / "a.md").write_text("# 甲\n\n甲正文。\n", encoding="utf-8")
            (linked_root / "b.md").write_text("# 乙\n\n乙正文。\n", encoding="utf-8")
            run(linked_root, "ingest")
            task_a = run(linked_root, "next", "--kind", "route")["data"]["tasks"][0]
            route_query(linked_root, task_a, "正文")
            run(linked_root, "submit", task_a["id"], "--payload", payload_file(
                linked_root, "link-a.json",
                {"decisions": [{"source": "a.md", "action": "new", "page": "page-a",
                                "title": "甲页", "type": "concept", "reason": "甲"}]},
            ))

            def submit_linked(page_id, key, quote, links):
                page_task = run(linked_root, "next", "--kind", "page")["data"]["tasks"][0]
                draft = linked_root / ".wiki/drafts" / f"{page_id}.md"
                draft.parent.mkdir(parents=True, exist_ok=True)
                draft.write_text(f"## 结论\n\n{quote}\n\n## 要点\n\n- {quote}\n",
                                 encoding="utf-8")
                run(linked_root, "submit", page_task["id"], "--payload", payload_file(
                    linked_root, f"{page_id}.json",
                    {"page": page_id, "title": "甲页" if page_id == "page-a" else "乙页",
                     "type": "concept", "summary": "一句话摘要。", "links": links,
                     "body_file": f".wiki/drafts/{page_id}.md",
                     "evidence": [{"source": key, "quote": quote}],
                     "boundaries": {"covered": "这一页", "not_covered": "其他"}},
                ))

            # 甲先建页，乙再链过去：链接只能指向提交时已存在的页。
            submit_linked("page-a", "a.md", "甲正文。", [])
            task_b = run(linked_root, "next", "--kind", "route")["data"]["tasks"][0]
            route_query(linked_root, task_b, "正文")
            run(linked_root, "submit", task_b["id"], "--payload", payload_file(
                linked_root, "link-b.json",
                {"decisions": [{"source": "b.md", "action": "new", "page": "page-b",
                                "title": "乙页", "type": "concept", "reason": "乙",
                                "links": ["page-a"]}]},
            ))
            submit_linked("page-b", "b.md", "乙正文。", ["page-a"])
            (linked_root / "a.md").unlink()
            (linked_root / "b.md").unlink()
            kept = run(linked_root, "ingest")["data"]
            status = run(linked_root, "status")["data"]
            check(results, "a linked orphan is only marked stale",
                  kept["orphaned"] == [] and status["pages"] == 2
                  and set(status["stale_pages"]) == {"page-a", "page-b"},
                  (kept, status))
        finally:
            shutil.rmtree(linked_root, ignore_errors=True)

        # 人工排版过的原文：账本换行但不回源覆盖，源本身变了才覆盖。
        edit_root = Path(tempfile.mkdtemp(prefix="llm-wiki-raw-edit-"))
        try:
            (edit_root / "a.md").write_text("# 手改\n\n原始正文。\n", encoding="utf-8")
            run(edit_root, "ingest")
            conn = sqlite3.connect(edit_root / ".wiki" / "wiki.db")
            before = conn.execute(
                "SELECT raw, raw_id, last_parsed_digest FROM sources"
            ).fetchone()
            conn.close()
            raw_path = edit_root / before[0]
            parsed = before[2]
            raw_path.write_text(
                raw_path.read_text(encoding="utf-8").rstrip("\n") + "\n\n用户手改的一句。\n",
                encoding="utf-8",
            )
            adopted = run(edit_root, "ingest")["data"]
            conn = sqlite3.connect(edit_root / ".wiki" / "wiki.db")
            raw_rows = conn.execute("SELECT id, path FROM raws").fetchall()
            after = conn.execute("SELECT raw, raw_id, last_parsed_digest FROM sources").fetchone()
            conn.close()
            edited_body = wiki.raw_body_digest(raw_path)
            check(results, "a hand-edited raw becomes a new row and keeps its parse stamp",
                  len(raw_rows) == 1 and raw_rows[0][0] == edited_body
                  and after[1] == edited_body and after[2] == parsed and after[0] == before[0]
                  and adopted["vanished"] == [],
                  (raw_rows, after, parsed, adopted))
            kept = raw_path.read_text(encoding="utf-8")
            again = run(edit_root, "ingest")["data"]
            check(results, "the next ingest leaves the hand-edited raw alone",
                  raw_path.read_text(encoding="utf-8") == kept and again["ingested"] == [],
                  again)
            (edit_root / "a.md").write_text("# 手改\n\n源又改了。\n", encoding="utf-8")
            rewritten = run(edit_root, "ingest")["data"]
            conn = sqlite3.connect(edit_root / ".wiki" / "wiki.db")
            raw_rows = conn.execute("SELECT id, path FROM raws").fetchall()
            current = conn.execute("SELECT raw_id, last_parsed_digest FROM sources").fetchone()
            conn.close()
            check(results, "a changed source rewrites the raw and reclaims the old row",
                  len(raw_rows) == 1 and raw_rows[0][0] == current[0] == current[1]
                  and "源又改了" in raw_path.read_text(encoding="utf-8"),
                  (raw_rows, current, rewritten))
        finally:
            shutil.rmtree(edit_root, ignore_errors=True)

        # 共享 raw 的手改属于整份正文：所有挂载一起换行，文件名不跟着摘要跳。
        shared_root = Path(tempfile.mkdtemp(prefix="llm-wiki-raw-shared-"))
        try:
            shared_body = "# 共享手改\n\n同一份正文。\n"
            (shared_root / "a.md").write_text(shared_body, encoding="utf-8")
            (shared_root / "copy").mkdir()
            (shared_root / "copy" / "a.md").write_text(shared_body, encoding="utf-8")
            run(shared_root, "ingest")
            conn = sqlite3.connect(shared_root / ".wiki" / "wiki.db")
            raw_rel, _, parsed = conn.execute(
                "SELECT raw, raw_id, last_parsed_digest FROM sources WHERE key = 'a.md'"
            ).fetchone()
            conn.close()
            raw_path = shared_root / raw_rel
            raw_path.write_text(
                raw_path.read_text(encoding="utf-8").rstrip("\n") + "\n\n用户手改的一句。\n",
                encoding="utf-8",
            )
            adopted = run(shared_root, "ingest")["data"]
            conn = sqlite3.connect(shared_root / ".wiki" / "wiki.db")
            raw_rows = conn.execute("SELECT id, path FROM raws").fetchall()
            after = conn.execute(
                "SELECT key, raw, raw_id, last_parsed_digest FROM sources ORDER BY key"
            ).fetchall()
            conn.close()
            edited_body = wiki.raw_body_digest(raw_path)
            check(results, "a shared hand edit moves every mount and keeps the file",
                  len(raw_rows) == 1 and raw_rows[0] == (edited_body, raw_rel)
                  and [row[2] for row in after] == [edited_body, edited_body]
                  and [row[3] for row in after] == [parsed, parsed]
                  and [row[1] for row in after] == [raw_rel, raw_rel]
                  and adopted["reclaimed"] == [],
                  (raw_rows, after, edited_body, adopted))
            kept = raw_path.read_text(encoding="utf-8")
            again = run(shared_root, "ingest")["data"]
            conn = sqlite3.connect(shared_root / ".wiki" / "wiki.db")
            raw_rows = conn.execute("SELECT id, path FROM raws").fetchall()
            conn.close()
            check(results, "a shared hand edit stays put on the next ingest",
                  again["ingested"] == [] and len(raw_rows) == 1
                  and raw_rows[0] == (edited_body, raw_rel)
                  and raw_path.read_text(encoding="utf-8") == kept,
                  (again, raw_rows))
            raw_path.write_text(
                kept.rstrip("\n") + "\n\n再手改一句。\n", encoding="utf-8"
            )
            run(shared_root, "ingest")
            names = sorted(p.name for p in (shared_root / "wiki/raw").glob("*.md"))
            check(results, "repeated shared edits keep the raw at its original name",
                  names == [raw_path.name], names)
        finally:
            shutil.rmtree(shared_root, ignore_errors=True)

        # 手改成另一份已存在的正文：整份并过去，旧行和旧文件回收。
        merge_root = Path(tempfile.mkdtemp(prefix="llm-wiki-raw-merge-"))
        try:
            (merge_root / "a.md").write_text("# 甲\n\n甲正文。\n", encoding="utf-8")
            (merge_root / "b.md").write_text("# 乙\n\n乙正文。\n", encoding="utf-8")
            (merge_root / "copy").mkdir()
            (merge_root / "copy" / "b.md").write_text("# 乙\n\n乙正文。\n", encoding="utf-8")
            run(merge_root, "ingest")
            conn = sqlite3.connect(merge_root / ".wiki" / "wiki.db")
            a_raw = conn.execute("SELECT raw FROM sources WHERE key = 'a.md'").fetchone()[0]
            b_raw = conn.execute("SELECT raw FROM sources WHERE key = 'b.md'").fetchone()[0]
            conn.close()
            (merge_root / b_raw).write_text(
                (merge_root / a_raw).read_text(encoding="utf-8"), encoding="utf-8"
            )
            merged = run(merge_root, "ingest")["data"]
            conn = sqlite3.connect(merge_root / ".wiki" / "wiki.db")
            raw_rows = conn.execute("SELECT id, path FROM raws").fetchall()
            moved = conn.execute(
                "SELECT key, raw, raw_id FROM sources WHERE key LIKE '%b.md' ORDER BY key"
            ).fetchall()
            conn.close()
            check(results, "a hand edit equal to another raw merges the whole document",
                  len(raw_rows) == 1 and merged["reclaimed"] == [b_raw]
                  and [row[1] for row in moved] == [a_raw, a_raw]
                  and [row[2] for row in moved] == [raw_rows[0][0], raw_rows[0][0]]
                  and not (merge_root / b_raw).exists(),
                  (raw_rows, moved, merged))
        finally:
            shutil.rmtree(merge_root, ignore_errors=True)

        # 旧证据复核：来源改写后，重写页面的旧引文必须在当前 raw 里还找得到。
        stale_root = Path(tempfile.mkdtemp(prefix="llm-wiki-old-evidence-"))
        try:
            (stale_root / "a.md").write_text("# 甲\n\n甲句：客服可查进度。\n", encoding="utf-8")
            (stale_root / "b.md").write_text("# 乙\n\n乙句：七天可退。\n", encoding="utf-8")
            run(stale_root, "ingest")
            routes = run(stale_root, "next", "--kind", "route", "--batch", "2")["data"]["tasks"]
            for task in routes:
                route_query(stale_root, task, "退")
            a_task = next(t for t in routes if t["work"]["sources"][0]["key"] == "a.md")
            b_task = next(t for t in routes if t["work"]["sources"][0]["key"] == "b.md")
            run(stale_root, "submit", a_task["id"], "--payload", payload_file(
                stale_root, "old-evidence-route-a.json",
                {"decisions": [{"source": "a.md", "action": "new", "page": "refund",
                                "title": "退款", "type": "process",
                                "reason": "整理退款条件"}]},
            ))
            run(stale_root, "submit", b_task["id"], "--payload", payload_file(
                stale_root, "old-evidence-route-b.json",
                {"decisions": [{"source": "b.md", "action": "merge", "page": "refund",
                                "reason": "补充退款时限"}]},
            ))
            task = run(stale_root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
            page_payload = {
                "page": "refund", "title": "退款", "type": "process",
                "summary": "客服可查进度，七天可退。", "links": [],
                "body": (
                    "## 结论\n\n客服可查进度，七天可退。\n\n"
                    "## 步骤\n\n联系客服提交退款。\n\n"
                    "## 注意事项\n\n以到账时间为准。\n"
                ),
                "evidence": [
                    {"source": "a.md", "quote": "客服可查进度"},
                    {"source": "b.md", "quote": "七天可退"},
                ],
                "boundaries": {"covered": "退款条件", "not_covered": "到账时间"},
            }
            created = run(stale_root, "submit", task["id"], "--payload",
                          payload_file(stale_root, "old-evidence-page.json", page_payload))
            check(results, "the fixture page starts with evidence from both sources",
                  created["data"]["rev"] == 1, created)
            (stale_root / "b.md").write_text("# 乙\n\n乙句：半月内可退。\n", encoding="utf-8")
            run(stale_root, "ingest")
            route = run(stale_root, "next", "--kind", "route")["data"]["tasks"][0]
            route_query(stale_root, route, "退")
            run(stale_root, "submit", route["id"], "--payload", payload_file(
                stale_root, "old-evidence-route-stale.json",
                {"decisions": [{"source": "b.md", "action": "merge", "page": "refund",
                                "reason": "时限改写，正文要跟着更新"}]},
            ))
            task = run(stale_root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
            failed = run(stale_root, "submit", task["id"], "--payload", payload_file(
                stale_root, "old-evidence-stale.json",
                {**page_payload, "evidence": [{"source": "a.md", "quote": "客服可查进度"}]},
            ), expect=2)
            check(results, "old evidence is rechecked against the rewritten raw",
                  failed.get("error", {}).get("code") == "E_EVIDENCE_NOT_FOUND", failed)
            task = run(stale_root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
            fixed = run(stale_root, "submit", task["id"], "--payload", payload_file(
                stale_root, "old-evidence-fixed.json",
                {**page_payload, "evidence": [
                    {"source": "a.md", "quote": "客服可查进度"},
                    {"source": "b.md", "quote": "半月内可退"},
                ]},
            ))
            check(results, "a fresh quote for the changed source clears the block",
                  fixed["data"]["rev"] == 2, fixed)
        finally:
            shutil.rmtree(stale_root, ignore_errors=True)

        # 旧版三字段快照（b2af7c9 那代引擎建的库）：没有源指纹，只能整体判漂移。
        # 用独立工作区，避免动到上面 fixture 的任务状态。
        legacy_root = Path(tempfile.mkdtemp(prefix="llm-wiki-legacy-"))
        try:
            (legacy_root / "x.md").write_text("# 旧库来源\n\n正文。\n", encoding="utf-8")
            run(legacy_root, "ingest")
            conn = sqlite3.connect(legacy_root / ".wiki" / "wiki.db")
            legacy_id, digest, raw = conn.execute(
                "SELECT t.id, s.digest, s.raw FROM tasks t, sources s "
                "WHERE s.key = 'x.md' AND t.kind = 'route'"
            ).fetchone()
            conn.execute(
                "UPDATE tasks SET inputs = ? WHERE id = ?",
                (json.dumps({"sources": [{"key": "x.md", "digest": digest, "raw": raw}]}),
                 legacy_id),
            )
            conn.commit()
            conn.close()
            run(legacy_root, "status")
            handed = run(legacy_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"]
            check(results, "a pre-fingerprint task is expired instead of crashing the claim",
                  handed == [], handed)
            conn = sqlite3.connect(legacy_root / ".wiki" / "wiki.db")
            row = conn.execute(
                "SELECT state, last_error FROM tasks WHERE id = ?", (legacy_id,)
            ).fetchone()
            conn.close()
            check(results, "the invalidated legacy task keeps its reason",
                  row[0] == "expired" and "predates source fingerprints" in row[1], row)
            legacy_submit = run(
                legacy_root, "submit", legacy_id,
                "--payload", payload_file(legacy_root, "legacy-route.json", {"decisions": []}),
                expect=3,
            )
            check(results, "submitting a pre-fingerprint task reports E_SOURCE_DRIFT",
                  legacy_submit["error"]["code"] == "E_SOURCE_DRIFT", legacy_submit)
        finally:
            shutil.rmtree(legacy_root, ignore_errors=True)

        # 任务目录只是任务的素材：next 物化，submit 默认读它，关闭即删。
        task_root = Path(tempfile.mkdtemp(prefix="llm-wiki-taskdir-"))
        try:
            (task_root / "a.md").write_text("# 任务目录\n\n正文一句话。\n", encoding="utf-8")
            run(task_root, "ingest")
            handed = run(task_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
            folder = task_root / ".wiki" / "tasks" / handed["id"]
            names = sorted(path.name for path in folder.iterdir())
            check(results, "next materializes the task folder",
                  names == ["brief.md", "payload.json", "work.json"], names)
            route_query(task_root, handed, "任务目录")
            (folder / "payload.json").write_text(json.dumps({"decisions": [
                {"source": handed["work"]["sources"][0]["key"], "action": "skip",
                 "reason": "与主题无关"}]}, ensure_ascii=False), encoding="utf-8")
            accepted = run(task_root, "submit", handed["id"])
            check(results, "submit reads the task payload by default", accepted["ok"], accepted)
            check(results, "a closed task drops its folder", not folder.exists(), folder)

            (task_root / "b.md").write_text("# 第二个来源\n\n正文两句话。\n", encoding="utf-8")
            run(task_root, "ingest")
            route = run(task_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
            route_query(task_root, route, "第二个来源")
            route_folder = task_root / ".wiki" / "tasks" / route["id"]
            (route_folder / "payload.json").write_text(json.dumps({"decisions": [
                {"source": route["work"]["sources"][0]["key"], "action": "new",
                 "page": "task-dir-page", "title": "任务目录页", "type": "concept",
                 "reason": "唯一来源"}]}, ensure_ascii=False), encoding="utf-8")
            run(task_root, "submit", route["id"])
            page_task = run(task_root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
            page_folder = task_root / ".wiki" / "tasks" / page_task["id"]
            filled = page_folder / "payload.json"
            original = filled.read_text(encoding="utf-8")
            run(task_root, "release", page_task["id"])
            check(results, "release keeps the filled payload",
                  filled.read_text(encoding="utf-8") == original, filled)
            reclaimed = run(task_root, "next", "--kind", "page", "--batch", "1")["data"]["tasks"][0]
            check(results, "a re-claimed task keeps the agent's payload",
                  reclaimed["id"] == page_task["id"]
                  and filled.read_text(encoding="utf-8") == original, reclaimed["id"])

            # 领取后不提交、租约未过期时再 next：报出未提交的任务，不重发也不续租。
            conn = sqlite3.connect(task_root / ".wiki" / "wiki.db")
            before = conn.execute(
                "SELECT epoch, attempts, lease_until FROM tasks WHERE id = ?", (page_task["id"],)
            ).fetchone()
            conn.close()
            blocked = run(task_root, "next", "--kind", "page", "--batch", "1", expect=3)
            error = blocked.get("error", {})
            check(results, "next reports the open task instead of reissuing it",
                  error.get("code") == "E_TASK_UNSUBMITTED"
                  and page_task["id"] in error.get("message", "")
                  and page_task["id"] in error.get("next", "")
                  and filled.read_text(encoding="utf-8") == original, blocked)
            conn = sqlite3.connect(task_root / ".wiki" / "wiki.db")
            after = conn.execute(
                "SELECT epoch, attempts, lease_until FROM tasks WHERE id = ?", (page_task["id"],)
            ).fetchone()
            conn.close()
            check(results, "a refused next does not renew the lease or bump the epoch",
                  before == after, (before, after))

            # 同一页面只允许一个未关闭的 page 任务：第二个来源挂上去，不新增第二条。
            run(task_root, "release", page_task["id"])
            (task_root / "c.md").write_text("# 第三个来源\n\n正文三句话。\n", encoding="utf-8")
            run(task_root, "ingest")
            second = run(task_root, "next", "--kind", "route", "--batch", "1")["data"]["tasks"][0]
            route_query(task_root, second, "第三个来源")
            second_folder = task_root / ".wiki" / "tasks" / second["id"]
            (second_folder / "payload.json").write_text(json.dumps({"decisions": [
                {"source": second["work"]["sources"][0]["key"], "action": "new",
                 "page": "task-dir-page", "title": "任务目录页", "type": "concept",
                 "reason": "同一页补来源"}]}, ensure_ascii=False), encoding="utf-8")
            attached = run(task_root, "submit", second["id"])
            conn = sqlite3.connect(task_root / ".wiki" / "wiki.db")
            open_pages = conn.execute(
                "SELECT id FROM tasks WHERE kind = 'page' AND state IN ('queued', 'leased')"
            ).fetchall()
            mounted = conn.execute(
                "SELECT COUNT(*) FROM task_sources WHERE task_id = ?", (page_task["id"],)
            ).fetchone()[0]
            conn.close()
            check(results, "a second route attaches to the open page task",
                  attached["ok"] and len(open_pages) == 1 and mounted == 2,
                  (attached, open_pages, mounted))

            stale = task_root / ".wiki" / "tasks" / "t_deadbeef"
            stale.mkdir()
            run(task_root, "ingest")
            check(results, "next reaps folders of closed tasks", not stale.exists(), stale)

            # 写命令排队：另一进程占着 `lock` 时等 5 秒后报 E_BUSY；陈旧的锁被接管。
            lock = task_root / ".wiki" / "lock"
            holder = subprocess.Popen(
                [sys.executable, "-c",
                 "import os,sys,time; p=sys.argv[1];"
                 " fd=os.open(p, os.O_CREAT|os.O_EXCL|os.O_WRONLY, 0o644);"
                 " os.write(fd, str(os.getpid()).encode()); os.close(fd); time.sleep(9)",
                 str(lock)]
            )
            try:
                time.sleep(1.0)
                busy = run(task_root, "status", expect=4)
            finally:
                holder.wait()
            check(results, "a held lock returns E_BUSY",
                  busy.get("error", {}).get("code") == "E_BUSY", busy)
            lock.write_text("999999", encoding="utf-8")
            taken = run(task_root, "status")
            check(results, "a stale lock is taken over",
                  taken["ok"] and not lock.exists(), (taken, lock))

            # 异常锁文件不能让加锁命令挂住：目录、FIFO、不可删的陈旧锁都要快速报 E_STATE。
            def abnormal_lock(name, prepare):
                case = Path(tempfile.mkdtemp(prefix="llm-wiki-lock-"))
                try:
                    (case / ".wiki").mkdir()
                    prepare(case / ".wiki" / "lock")
                    started = time.monotonic()
                    try:
                        denied = run(case, "status", expect=3, timeout=10)
                    except subprocess.TimeoutExpired:
                        check(results, name, False, "command did not return within 10s")
                    elapsed = time.monotonic() - started
                    check(results, name,
                          elapsed < 8 and denied.get("error", {}).get("code") == "E_STATE",
                          (elapsed, denied))
                finally:
                    shutil.rmtree(case, ignore_errors=True)

            abnormal_lock("a directory lock fails with E_STATE instead of hanging",
                          lambda path: path.mkdir())
            if hasattr(os, "mkfifo"):
                abnormal_lock("a FIFO lock fails with E_STATE instead of blocking",
                              lambda path: os.mkfifo(path))

            # 锁文件本身不可删：同样要报错，不能反复判陈旧后 continue。
            unremovable = Path(tempfile.mkdtemp(prefix="llm-wiki-lock-ro-"))
            try:
                (unremovable / ".wiki").mkdir()
                (unremovable / ".wiki" / "lock").write_text("999999", encoding="utf-8")
                (unremovable / ".wiki").chmod(0o555)
                if not os.access(unremovable / ".wiki", os.W_OK):
                    denied = run(unremovable, "status", expect=3, timeout=10)
                    check(results, "an unremovable stale lock fails with E_STATE",
                          denied.get("error", {}).get("code") == "E_STATE", denied)
            finally:
                (unremovable / ".wiki").chmod(0o755)
                shutil.rmtree(unremovable, ignore_errors=True)

            # `.wiki` 存在但不可写：创建锁失败也要变成 E_HEALTH，而不是 traceback。
            unwritable = Path(tempfile.mkdtemp(prefix="llm-wiki-wiki-ro-"))
            try:
                (unwritable / ".wiki").mkdir()
                (unwritable / ".wiki").chmod(0o555)
                if not os.access(unwritable / ".wiki", os.W_OK):
                    denied = run(unwritable, "status", expect=1, timeout=10)
                    check(results, "an unwritable .wiki fails with E_HEALTH",
                          denied.get("error", {}).get("code") == "E_HEALTH", denied)
            finally:
                (unwritable / ".wiki").chmod(0o755)
                shutil.rmtree(unwritable, ignore_errors=True)

            # 普通 query 是只读快照：不取锁、不建目录、不推观看区。
            lock.mkdir()
            try:
                found = run(task_root, "query", "测试")
                check(results, "a plain query ignores the workspace lock",
                      found["ok"] and lock.is_dir(), (found, lock))
            finally:
                lock.rmdir()

            lock.mkdir()
            try:
                denied = run(task_root, "query", "--task", "t_missing", expect=3, timeout=10)
                check(results, "query --task still takes the workspace lock",
                      denied.get("error", {}).get("code") == "E_STATE", denied)
            finally:
                lock.rmdir()

            empty = Path(tempfile.mkdtemp(prefix="llm-wiki-query-empty-"))
            try:
                denied = run(empty, "query", "hello", expect=3)
                check(results, "query on an uninitialized workspace reports E_STATE",
                      denied.get("error", {}).get("code") == "E_STATE"
                      and not (empty / ".wiki").exists(), denied)
            finally:
                shutil.rmtree(empty, ignore_errors=True)

            legacy = Path(tempfile.mkdtemp(prefix="llm-wiki-query-legacy-"))
            try:
                (legacy / ".wiki").mkdir()
                conn = sqlite3.connect(legacy / ".wiki" / "wiki.db")
                conn.execute(
                    "CREATE TABLE page_revs (page TEXT, rev INTEGER, meta TEXT, "
                    "body TEXT, digest TEXT, render_digest TEXT, origin TEXT, "
                    "accepted_at TEXT)"
                )
                conn.commit()
                conn.close()
                denied = run(legacy, "query", "hello", expect=3)
                check(results, "query refuses a ledger it cannot migrate read-only",
                      denied.get("error", {}).get("code") == "E_STATE", denied)
            finally:
                shutil.rmtree(legacy, ignore_errors=True)

            snapshot = Path(tempfile.mkdtemp(prefix="llm-wiki-query-snapshot-"))
            try:
                run(snapshot, "init")
                readme = snapshot / "wiki" / "README.md"
                readme.parent.mkdir(parents=True, exist_ok=True)
                readme.write_text("hand edited\n", encoding="utf-8")
                found = run(snapshot, "query", "hello")
                check(results, "query does not sync the view",
                      found["ok"] and readme.read_text(encoding="utf-8") == "hand edited\n",
                      (found, readme))
            finally:
                shutil.rmtree(snapshot, ignore_errors=True)

            readonly = Path(tempfile.mkdtemp(prefix="llm-wiki-query-ro-"))
            try:
                run(readonly, "init")
                (readonly / ".wiki").chmod(0o555)
                if not os.access(readonly / ".wiki", os.W_OK):
                    found = run(readonly, "query", "hello", timeout=10)
                    check(results, "query reads a read-only workspace",
                          found["ok"], found)
            finally:
                (readonly / ".wiki").chmod(0o755)
                shutil.rmtree(readonly, ignore_errors=True)
        finally:
            shutil.rmtree(task_root, ignore_errors=True)

        dry = root / "dry"
        dry.mkdir()
        (dry / "note.md").write_text("# 标题\n\n正文。\n", encoding="utf-8")
        dry_run = subprocess.run(
            [sys.executable, str(SCRIPT), "ingest", "--dry-run", "--json"],
            cwd=dry, capture_output=True, text=True,
        )
        check(results, "dry run leaves no .wiki", dry_run.returncode == 0 and not (dry / ".wiki").exists(), dry_run.stderr)

        # 队列清空后再摄入：queue 快照应显示没有未完成的工作，不再给出"去跑 next"的暗示。
        drained_root = root / "drained"
        drained_root.mkdir()
        (drained_root / "note.md").write_text("# 唯一来源\n\n正文。\n", encoding="utf-8")
        run(drained_root, "init")
        run(drained_root, "ingest")
        claimed = run(drained_root, "next", "--batch", "10")["data"]["tasks"]
        check(results, "drained fixture claims one route task", len(claimed) == 1, claimed)
        route_query(drained_root, claimed[0], "唯一来源")
        run(drained_root, "submit", claimed[0]["id"], "--payload", payload_file(
            drained_root, "skip.json",
            {"decisions": [{"source": "note.md", "action": "skip", "reason": "无进展，且无可沉淀的知识"}]}))
        drained = run(drained_root, "ingest")["data"]
        check(results, "a drained queue reports no open work",
              drained["queue"]["open"] == 0 and drained["queue"]["queued"] == 0
              and drained["queued_tasks"] == [], drained.get("queue"))
        empty = run(drained_root, "next", "--batch", "10")
        check(results, "next keeps reporting no work when nothing is open",
              empty["data"]["count"] == 0 and empty["summary"] == "no queued tasks", empty)

        override = root / "workflows" / "topic-exploration"
        override.mkdir()
        (override / "WORKFLOW.md").write_text(
            "---\nname: topic-exploration\ndescription: 项目自定义的话题流程\n---\n\n"
            "## 步骤\n\n1. tool: **检索**：`query`\n",
            encoding="utf-8",
        )
        overridden = run(root, "workflow", "load", "topic-exploration")["data"]
        check(results, "project workflow overrides the builtin",
              overridden["workflow"]["source"] == "project"
              and "项目自定义" in overridden["content"], overridden)

        retrieval_flow(results)
        batch_flow(results)
        legacy_note_flow(results)
        links_flow(results)
        repair_flow(results)

        healthy = run(root, "doctor", expect=(0, 1))["data"]
        check(results, "doctor runs", isinstance(healthy["issues"], list), healthy)

        wrappers = sorted(
            path.relative_to(ROOT).as_posix()
            for path in (ROOT / "skills").rglob("*")
            if path.is_file() and path.suffix.lower() in {".cmd", ".bat"}
        )
        check(results, "the skill package ships no .cmd/.bat wrapper", not wrappers, wrappers)

        skill_meta, _ = wiki.split_frontmatter(
            (ROOT / "skills" / "llm-wiki" / "SKILL.md").read_text(encoding="utf-8")
        )
        description = str(skill_meta.get("description") or "").strip()
        check(
            results,
            "SKILL.md description stays inside the 1024-character metadata budget",
            bool(description) and len(description) <= 1024,
            f"{len(description)} chars",
        )

        project_flows = [entry for entry in workflows.scan(ROOT) if entry["source"] == "project"]
        check(results, "repo's own project workflows are all valid",
              len(project_flows) >= 4 and all(entry["valid"] for entry in project_flows),
              [entry["id"] for entry in project_flows if not entry["valid"]])
        meeting = workflows.match(ROOT, ["会议", "纪要", "行动项"], 5)
        check(results, "meeting workflow tops the meeting search with applies",
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
        check(results, "no shipped doc teaches the deleted read command", not stale, stale)
    except AssertionError as error:
        results.append(("failed", False, str(error)))
    finally:
        if args.keep:
            print(f"workspace kept at {root}", file=sys.stderr)
        else:
            shutil.rmtree(root, ignore_errors=True)

    failures = [item for item in results if not item[1]]
    if args.json:
        print(json.dumps({"ok": not failures, "checks": len(results), "failed": len(failures),
                          "results": [{"name": name, "ok": ok, "detail": str(detail)}
                                      for name, ok, detail in results]}, ensure_ascii=False))
    else:
        for name, ok, detail in results:
            print(f"{'PASS' if ok else 'FAIL'}  {name}")
            if not ok:
                print(f"      {detail}")
        print(f"\n{len(results) - len(failures)}/{len(results)} checks passed")
    return 0 if not failures else 1


if __name__ == "__main__":
    sys.exit(main())
