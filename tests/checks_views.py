"""View directory, view definition and topic listing checks.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import json
import shutil
import tempfile
from pathlib import Path

from support import check, run, run_text
from fixtures import write_topic, write_workflow

# 引擎模块要等 support 把脚本目录加进 sys.path，因此排在 support 之后。
import ledger
import topics


def view_dir_checks(ctx):
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-viewdir-"))
    try:
        (root / ".wiki").mkdir()
        (root / ".wiki/wiki.json").write_text(
            json.dumps({"publish": {"dir": "kb"}}), encoding="utf-8")
        check(ctx.results, "view_dir reads publish.dir",
              ledger.view_dir(root) == root / "kb" and not (root / "kb").exists())
        (root / ".wiki/wiki.json").write_text("{}", encoding="utf-8")
        check(ctx.results, "view_dir defaults to wiki",
              ledger.view_dir(root) == root / "wiki")
    finally:
        shutil.rmtree(root, ignore_errors=True)

    root = Path(tempfile.mkdtemp(prefix="llm-wiki-viewdir-off-"))
    try:
        (root / ".wiki").mkdir()
        (root / ".wiki/wiki.json").write_text(
            json.dumps({"publish": {"enabled": False}}), encoding="utf-8")
        run(root, "status")
        check(ctx.results, "disabled publish still creates content dirs",
              (root / "wiki/workflows").is_dir() and (root / "wiki/topics").is_dir())
    finally:
        shutil.rmtree(root, ignore_errors=True)


def topics_view_checks(ctx):
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-viewtopic-"))
    try:
        view = root / "wiki/topics"
        legacy = root / "topics"
        write_topic(view, "观看区话题.md", "观看区话题", "观看区版本")
        write_topic(legacy, "观看区话题.md", "旧话题", "旧版本")
        write_topic(legacy, "旧话题.md", "旧话题", "旧版本")

        entries = topics.scan(root, view_root=view)
        by_name = {item["name"]: item for item in entries}
        check(ctx.results, "view topic wins over legacy",
              by_name["观看区话题"]["origin"] == "view"
              and by_name["观看区话题"]["path"] == "wiki/topics/观看区话题.md")
        check(ctx.results, "legacy-only topic keeps legacy origin",
              by_name["旧话题"]["origin"] == "legacy"
              and by_name["旧话题"]["path"] == "topics/旧话题.md")

        matched = topics.match(root, ["观看区"], 5, view_root=view)
        check(ctx.results, "topic search finds view topic",
              matched and matched[0]["name"] == "观看区话题"
              and matched[0]["origin"] == "view", matched)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def view_definition_suite(ctx):
    root = Path(tempfile.mkdtemp(prefix="llm-wiki-view-defs-"))
    try:
        (root / "source.md").write_text("# 观看区流程\n\n内容\n", encoding="utf-8")
        write_workflow(root / "workflows", "legacy-only", "旧根目录流程")
        run(root, "ingest")
        check(ctx.results, "view creates content dirs", (root / "wiki/workflows").is_dir())

        write_workflow(root / "wiki/workflows", "account-opening", "观看区开户", "观看区版本")
        write_workflow(root / "workflows", "account-opening", "旧根开户", "旧版本")
        listing = run(root, "workflow", "list")["data"]
        by_id = {item["id"]: item for item in listing["workflows"]}
        check(ctx.results, "workflow list reports view first",
              listing["root"] == "wiki/workflows"
              and listing["roots"] == ["wiki/workflows", "workflows"], listing)
        check(ctx.results, "workflow list marks view origin and legacy conflict",
              by_id["account-opening"]["origin"] == "view"
              and by_id["account-opening"]["legacy_conflict"] is True
              and by_id["account-opening"]["legacy_path"]
              == "workflows/account-opening/WORKFLOW.md")
        check(ctx.results, "workflow list keeps legacy-only",
              by_id["legacy-only"]["origin"] == "legacy")
        check(ctx.results, "workflow id appears once",
              [item["id"] for item in listing["workflows"]].count("account-opening") == 1)

        loaded = run(root, "workflow", "load", "account-opening")["data"]
        check(ctx.results, "workflow load reads view",
              loaded["path"] == "wiki/workflows/account-opening/WORKFLOW.md"
              and loaded["workflow"]["origin"] == "view", loaded)
        legacy = run(root, "workflow", "load", "legacy-only")["data"]
        check(ctx.results, "workflow load falls back to legacy",
              legacy["path"] == "workflows/legacy-only/WORKFLOW.md"
              and legacy["workflow"]["origin"] == "legacy", legacy)

        proc = run_text(root, "workflow", "list")
        check(ctx.results, "workflow list warns about legacy conflict",
              proc.returncode == 0 and "观看区版本生效" in proc.stdout, proc.stdout)

        write_topic(root / "wiki/topics", "观看区话题.md", "观看区话题", "观看区版本")
        write_topic(root / "topics", "观看区话题.md", "旧话题", "旧版本")
        query = run(root, "query", "观看区")["data"]
        topic_hits = [item for item in query["topics"] if item["name"] == "观看区话题"]
        check(ctx.results, "query reads view topic once",
              len(topic_hits) == 1
              and topic_hits[0]["path"] == "wiki/topics/观看区话题.md"
              and topic_hits[0]["origin"] == "view", topic_hits)
        check(ctx.results, "query reads view workflow",
              any(item["id"] == "account-opening" and item["origin"] == "view"
                  for item in query["workflows"]), query["workflows"])

        before = (root / "wiki/workflows/account-opening/WORKFLOW.md").read_text(encoding="utf-8")
        run(root, "status")
        after = (root / "wiki/workflows/account-opening/WORKFLOW.md").read_text(encoding="utf-8")
        check(ctx.results, "sync does not overwrite view workflow", before == after)
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
        check(ctx.results, "workflow list uses custom publish.dir",
              data["root"] == "kb/workflows" and "new-flow" in ids and "old-flow" not in ids,
              data)
    finally:
        shutil.rmtree(custom, ignore_errors=True)
