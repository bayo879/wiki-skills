#!/usr/bin/env python3
"""End-to-end self test for the task-driven wiki engine.

Runs a disposable knowledge base in a temp directory: ingest, route decisions,
page submission, view push, import/restore, classification, dry run. Never
touches real data and only uses the standard library.

The checks themselves live in the sibling ``checks_*`` modules; this file only
builds the workspace and runs them in order.

Run:  python3 tests/selftest.py [--keep] [--json]
Exit: 0 all checks passed, 1 a check failed.
"""

import argparse
import json
import shutil
import sys
import tempfile
from pathlib import Path

sys.dont_write_bytecode = True

from support import Ctx  # noqa: E402  (importing support puts the engine on sys.path)
from fixtures import workflow_files  # noqa: E402
import checks_config  # noqa: E402
import checks_evidence  # noqa: E402
import checks_import  # noqa: E402
import checks_ingest  # noqa: E402
import checks_ledger  # noqa: E402
import checks_links  # noqa: E402
import checks_multipage  # noqa: E402
import checks_page_flow  # noqa: E402
import checks_pages  # noqa: E402
import checks_query  # noqa: E402
import checks_raw  # noqa: E402
import checks_recovery  # noqa: E402
import checks_repo  # noqa: E402
import checks_sync  # noqa: E402
import checks_tasks  # noqa: E402
import checks_topics  # noqa: E402
import checks_views  # noqa: E402
import checks_workflows  # noqa: E402


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--keep", action="store_true", help="keep the temp workspace")
    parser.add_argument("--json", action="store_true", help="machine-readable summary")
    args = parser.parse_args(argv)

    root = Path(tempfile.mkdtemp(prefix="llm-wiki-selftest-"))
    ctx = Ctx(root=root, results=[])
    results = ctx.results
    try:
        (root / "a.md").write_text(
            "# 反诈复机\n\n号码被停机后，需要本人持身份证到营业厅办理复机。\n"
            "审批时长通常为两个工作日。\n",
            encoding="utf-8",
        )
        (root / "b.md").write_text("# 无关材料\n\n用来验证 skip 分支。\n", encoding="utf-8")
        (root / "scan.png").write_bytes(b"\x89PNG\r\n\x1a\n")
        ctx.workflow_main = workflow_files(root)

        # 顺序沿用拆分前的位置，PASS 行与旧版逐行一致。
        checks_workflows.workflow_catalogue(ctx)
        checks_views.view_dir_checks(ctx)
        checks_workflows.workflow_view_checks(ctx)
        checks_views.topics_view_checks(ctx)
        checks_topics.topic_refs_checks(ctx)
        checks_views.view_definition_suite(ctx)
        checks_topics.topic_refs_flow(ctx)
        checks_workflows.workflow_contracts(ctx)
        checks_ledger.workspace_and_init(ctx)
        checks_ingest.ingest_and_raw(ctx)
        checks_query.query_candidates(ctx)
        checks_page_flow.page_flow(ctx)
        checks_pages.batch_pages(ctx)
        checks_import.import_and_restore(ctx)
        checks_links.link_commands(ctx)
        checks_config.publish_config(ctx)
        checks_recovery.drift_recovery(ctx)
        checks_recovery.split_helpers(ctx)
        checks_sync.sync_behaviour(ctx)
        checks_multipage.multi_page_split(ctx)
        checks_raw.raw_mounts(ctx)
        checks_raw.orphan_and_merge(ctx)
        checks_evidence.stale_evidence(ctx)
        checks_tasks.task_locks(ctx)
        checks_repo.dry_run(ctx)
        checks_query.retrieval_flow(ctx)
        checks_pages.batch_flow(ctx)
        checks_ingest.legacy_note_flow(ctx)
        checks_links.links_flow(ctx)
        checks_ledger.repair_flow(ctx)
        checks_repo.repo_guards(ctx)
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
