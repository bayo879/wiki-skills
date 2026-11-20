"""Sync idempotency, adoption, rename/move and tree uniqueness checks.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import json
import re
import sqlite3
import subprocess
import sys

from support import SCRIPT, check, run

# 引擎模块要等 support 把脚本目录加进 sys.path，因此排在 support 之后。
import wiki


def sync_behaviour(ctx):
    room_view = ctx.root / "view/pages/concept/机房巡检.md"
    # query 现在是只读快照，不再顺带推送观看区；先显式推一次再验证 sync 的幂等性。
    run(ctx.root, "sync")
    room_text = room_view.read_text(encoding="utf-8")
    synced = run(ctx.root, "sync")["data"]
    check(ctx.results, "sync leaves an untouched view alone",
          synced["unchanged"] == ["anti-fraud-restore", "machine-room-inspection"], synced)
    check(ctx.results, "sync reports the six keys",
          set(synced) == {"imported", "skipped", "failed", "unchanged", "written",
                          "warnings"}, synced)

    room_view.write_text(
        room_text.replace("每周巡检一次；记录保留一年。", "每周巡检两次；记录保留两年。"),
        encoding="utf-8",
    )
    synced = run(ctx.root, "sync")["data"]
    check(ctx.results, "sync adopts a hand edited body",
          "machine-room-inspection" in synced["imported"], synced)
    check(ctx.results, "sync writes the canonical text back",
          "每周巡检两次；记录保留两年。" in room_view.read_text(encoding="utf-8"),
          room_view.read_text(encoding="utf-8"))
    page = run(ctx.root, "query", "--page", "machine-room-inspection")["data"]
    check(ctx.results, "the adopted body lands in the ledger",
          "每周巡检两次" in page["body"], page["body"])
    check(ctx.results, "sync keeps the ledger metadata",
          page["page"]["title"] == "机房巡检", page["page"])
    conn = sqlite3.connect(ctx.root / ".wiki/wiki.db")
    facts = conn.execute(
        "SELECT rev, view_path, view_digest FROM page_revs "
        "WHERE page = 'machine-room-inspection' ORDER BY rev DESC LIMIT 1"
    ).fetchone()
    conn.close()
    check(ctx.results, "sync records the published facts",
          facts[0] == 2 and facts[1] == "pages/concept/机房巡检.md"
          and facts[2] == wiki.digest_text(room_view.read_text(encoding="utf-8")), facts)

    again = run(ctx.root, "sync")["data"]
    check(ctx.results, "sync is idempotent and does not freeze the page",
          again["unchanged"] == ["anti-fraud-restore", "machine-room-inspection"], again)

    room_view.write_text(room_view.read_text(encoding="utf-8").replace(
        f"summary: {wiki.yaml_scalar('机房每周巡检一次，巡检记录保留一年。')}",
        f"summary: {wiki.yaml_scalar('改过的摘要')}"), encoding="utf-8")
    refused = run(ctx.root, "sync", expect=1)["data"]
    check(ctx.results, "sync refuses a metadata edit",
          any(item["code"] == "E_META_CHANGED" for item in refused["failed"]), refused)
    check(ctx.results, "a refused page keeps its ledger summary",
          run(ctx.root, "query", "--page", "machine-room-inspection")["data"]["page"]["summary"]
          == "机房每周巡检一次，巡检记录保留一年。")
    check(ctx.results, "a refused page file is left alone",
          "改过的摘要" in room_view.read_text(encoding="utf-8"),
          room_view.read_text(encoding="utf-8"))

    room_view.write_text(room_view.read_text(encoding="utf-8").replace(
        "每周巡检两次；记录保留两年。", "每周巡检三次。"), encoding="utf-8")
    adopted = run(ctx.root, "sync", "--body-only")["data"]
    check(ctx.results, "--body-only adopts the body",
          "machine-room-inspection" in adopted["imported"], adopted)
    page = run(ctx.root, "query", "--page", "machine-room-inspection")["data"]
    check(ctx.results, "--body-only keeps the ledger metadata",
          page["page"]["summary"] == "机房每周巡检一次，巡检记录保留一年。", page["page"])
    check(ctx.results, "--body-only rewrites the canonical text",
          "改过的摘要" not in room_view.read_text(encoding="utf-8")
          and "每周巡检三次" in room_view.read_text(encoding="utf-8"))

    handmade = ctx.root / "view/pages/concept/我的笔记.md"
    handmade.write_text("# 随手记\n\n不属于账本。\n", encoding="utf-8")
    stranger = ctx.root / "view/pages/concept/陌生人.md"
    stranger.write_text("---\nid: no-such-page\n---\n\n## 内容\n\n别人的文件。\n", encoding="utf-8")
    skips = run(ctx.root, "sync")["data"]
    check(ctx.results, "sync skips files without an id",
          {"file": "pages/concept/我的笔记.md", "reason": "no id"} in skips["skipped"],
          skips["skipped"])
    check(ctx.results, "sync skips unknown ids",
          {"file": "pages/concept/陌生人.md", "reason": "unknown id"} in skips["skipped"],
          skips["skipped"])
    check(ctx.results, "skipped files are left untouched",
          handmade.read_text(encoding="utf-8") == "# 随手记\n\n不属于账本。\n"
          and stranger.read_text(encoding="utf-8").startswith("---\nid: no-such-page"))

    conn = sqlite3.connect(ctx.root / ".wiki/wiki.db")
    before_rev = conn.execute(
        "SELECT MAX(rev) FROM page_revs WHERE page = 'machine-room-inspection'"
    ).fetchone()[0]
    conn.close()
    copy_path = ctx.root / "view/pages/concept/机房巡检-副本.md"
    copy_path.write_text(room_view.read_text(encoding="utf-8"), encoding="utf-8")
    duplicate = run(ctx.root, "sync", expect=1)["data"]
    conn = sqlite3.connect(ctx.root / ".wiki/wiki.db")
    after_rev = conn.execute(
        "SELECT MAX(rev) FROM page_revs WHERE page = 'machine-room-inspection'"
    ).fetchone()[0]
    conn.close()
    check(ctx.results, "duplicate ids fail with E_DUPLICATE",
          sorted(item["code"] for item in duplicate["failed"]
                 if item["page"] == "machine-room-inspection")
          == ["E_DUPLICATE", "E_DUPLICATE"], duplicate["failed"])
    check(ctx.results, "duplicates are not adopted",
          "machine-room-inspection" not in duplicate["imported"] and before_rev == after_rev,
          (duplicate["imported"], before_rev, after_rev))
    check(ctx.results, "one failing page does not stop the run",
          "anti-fraud-restore" in duplicate["unchanged"], duplicate)
    human = subprocess.run([sys.executable, str(SCRIPT), "sync"], cwd=ctx.root,
                           capture_output=True, text=True)
    check(ctx.results, "sync prints counts and failure codes",
          human.returncode == 1 and "E_DUPLICATE" in human.stdout, human.stdout)
    copy_path.unlink()

    room_view.write_text(room_view.read_text(encoding="utf-8").replace(
        "巡检周期为每周一次", "巡检周期为每周两次"), encoding="utf-8")
    run(ctx.root, "sync")
    page = run(ctx.root, "query", "--page", "machine-room-inspection")["data"]
    check(ctx.results, "sync parses the evidence block back into meta",
          any(item["source"] == "d.md" and item["quote"] == "巡检周期为每周两次"
              for item in page["page"]["evidence"]), page["page"]["evidence"])
    check(ctx.results, "the evidence text stays out of the body",
          "## 证据" not in page["body"] and "巡检周期为每周两次" not in page["body"],
          page["body"])
    check(ctx.results, "the canonical text keeps one evidence block",
          room_view.read_text(encoding="utf-8").count("## 证据") == 1,
          room_view.read_text(encoding="utf-8"))
    linked = re.search(r"- \[`[^`]+`\]\(([^)]*)\)（`d.md`）：",
                       room_view.read_text(encoding="utf-8"))
    check(ctx.results, "page evidence links to the raw copy and names the source",
          linked is not None
          and (room_view.parent / linked.group(1)).resolve().is_file(),
          room_view.read_text(encoding="utf-8"))

    room_view.write_text(re.sub(
        r"- \[`[^`]+`\]\([^)]*\)（`d.md`）：巡检周期为每周两次",
        "- d.md 巡检周期为每周两次",
        room_view.read_text(encoding="utf-8"),
    ), encoding="utf-8")
    broken = subprocess.run([sys.executable, str(SCRIPT), "sync", "--json"], cwd=ctx.root,
                            capture_output=True, text=True)
    check(ctx.results, "an unparseable evidence block warns",
          broken.returncode == 0 and "warning" in broken.stderr, broken.stderr)
    broken_data = json.loads(broken.stdout)["data"]
    check(ctx.results, "an unparseable evidence block is reported in warnings",
          len(broken_data["warnings"]) == 1
          and broken_data["warnings"][0].startswith("pages/concept/机房巡检.md:")
          and "evidence block does not parse" in broken_data["warnings"][0],
          broken_data["warnings"])
    page = run(ctx.root, "query", "--page", "machine-room-inspection")["data"]
    check(ctx.results, "a broken block keeps the ledger evidence",
          any(item["source"] == "d.md" and item["quote"] == "巡检周期为每周两次"
              for item in page["page"]["evidence"]), page["page"]["evidence"])
    check(ctx.results, "a broken block is adopted as body text",
          "d.md 巡检周期为每周两次" in page["body"], page["body"])

    canonical = room_view.read_text(encoding="utf-8")
    head = canonical.split("\n---\n", 1)[0] + "\n---\n"
    room_view.write_text(head, encoding="utf-8")
    empty = run(ctx.root, "sync", expect=1)["data"]
    check(ctx.results, "an empty body is refused with E_PAYLOAD",
          any(item["code"] == "E_PAYLOAD" for item in empty["failed"]), empty)
    room_view.write_text(canonical, encoding="utf-8")

    rename_draft = ctx.root / ".wiki/drafts/rename.md"
    rename_draft.write_text(
        "---\ntitle: 机房巡检记录\n---\n## 要点\n\n每周巡检三次。\n", encoding="utf-8")
    run(ctx.root, "page", "import", "machine-room-inspection", "--from", str(rename_draft))
    old_path = ctx.root / "view/pages/concept/机房巡检.md"
    new_path = ctx.root / "view/pages/concept/机房巡检记录.md"
    moved = run(ctx.root, "sync")["data"]
    check(ctx.results, "sync publishes a ledger-side title change",
          any(item["page"] == "machine-room-inspection"
              and item["to"] == "pages/concept/机房巡检记录.md" for item in moved["written"])
          and new_path.exists(), moved)
    check(ctx.results, "the old path is gone after the rename",
          not old_path.exists(), old_path.exists())
    check(ctx.results, "the new path carries the new title",
          'title: "机房巡检记录"' in new_path.read_text(encoding="utf-8"),
          new_path.read_text(encoding="utf-8"))
    conn = sqlite3.connect(ctx.root / ".wiki/wiki.db")
    moved_facts = conn.execute(
        "SELECT rev, view_path, view_digest FROM page_revs "
        "WHERE page = 'machine-room-inspection' ORDER BY rev DESC LIMIT 1"
    ).fetchone()
    conn.close()
    check(ctx.results, "the renamed page's facts follow the new path",
          moved_facts[1] == "pages/concept/机房巡检记录.md"
          and moved_facts[2] == wiki.digest_text(new_path.read_text(encoding="utf-8")),
          moved_facts)

    moved_away = ctx.root / "view/pages/concept/临时位置.md"
    new_path.rename(moved_away)
    returned = run(ctx.root, "sync")["data"]
    check(ctx.results, "a page file moved by hand is put back",
          any(item["page"] == "machine-room-inspection"
              and item["from"] == "pages/concept/临时位置.md"
              and item["to"] == "pages/concept/机房巡检记录.md" for item in returned["written"])
          and new_path.exists() and not moved_away.exists(), returned)

    room_before = run(ctx.root, "query", "--page", "machine-room-inspection")["data"]
    linked_import = ctx.root / ".wiki/drafts/linked.md"
    linked_import.write_text(
        "---\ntitle: 机房巡检记录\nlinks:\n  - anti-fraud-restore\n---\n"
        + room_before["body"],
        encoding="utf-8",
    )
    imported_link = run(
        ctx.root, "page", "import", "machine-room-inspection", "--from", str(linked_import)
    )["data"]
    check(ctx.results, "page import accepts a frontmatter link list",
          imported_link["rev"] == room_before["rev"] + 1, imported_link)
    imported_page = run(ctx.root, "query", "--page", "machine-room-inspection")["data"]
    check(ctx.results, "imported links join the page edges",
          "anti-fraud-restore" in imported_page["outgoing"], imported_page["outgoing"])
    imported_view = new_path.read_text(encoding="utf-8")
    check(ctx.results, "imported links reach the view after the next push",
          "[反诈复机](../process/反诈复机.md)" in imported_view, imported_view)

    bad_link = ctx.root / ".wiki/drafts/bad-link.md"
    bad_link.write_text(
        "---\ntitle: 机房巡检记录\nlinks:\n  - no-such-page\n---\n"
        "## 要点\n\nunknown link.\n",
        encoding="utf-8",
    )
    rejected = run(ctx.root, "page", "import", "machine-room-inspection",
                   "--from", str(bad_link), expect=2)
    check(ctx.results, "page import rejects an unknown link",
          rejected["error"]["code"] == "E_LINK_UNRESOLVED", rejected)
    stable = run(ctx.root, "query", "--page", "machine-room-inspection")["data"]
    check(ctx.results, "a rejected import leaves the page untouched",
          stable["rev"] == imported_link["rev"]
          and "anti-fraud-restore" in stable["outgoing"], stable)

    status = run(ctx.root, "status")["data"]
    check(ctx.results, "status counts invalidated tasks without opening them",
          status["tasks"].get("expired", 0) >= 1
          and status["tasks"]["open"] == status["tasks"]["queued"] + status["tasks"]["leased"],
          status["tasks"])

    # 树唯一性不变量：只统计 page 树。submit_route 在关闭 route 任务前先建 page 树，
    # route→page 的交接瞬间同一个 key 会同时挂两行，那是正常交接不是第二套树。
    # 同一次 route 拆页是显式例外，下面单独测。
    conn = sqlite3.connect(ctx.root / ".wiki" / "wiki.db")
    twins = conn.execute(
        "SELECT ts.source_key, COUNT(DISTINCT ts.task_id) AS n "
        "FROM task_sources ts JOIN tasks t ON t.id = ts.task_id "
        "WHERE t.kind = 'page' AND t.state IN ('queued', 'leased') "
        "GROUP BY ts.source_key HAVING n > 1"
    ).fetchall()
    conn.close()
    check(ctx.results, "no source sits in two open page trees outside a route split",
          twins == [], twins)
