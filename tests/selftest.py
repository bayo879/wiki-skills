#!/usr/bin/env python3
"""End-to-end self test for the task-driven wiki engine.

Runs a disposable knowledge base in a temp directory: ingest, route decisions,
page submission, mirror drift, import/restore, classification, dry run. Never
touches real data and only uses the standard library.

Run:  python3 tests/selftest.py [--keep] [--json]
Exit: 0 all checks passed, 1 a check failed.
"""

import argparse
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
from pathlib import Path

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "skills" / "llm-wiki" / "scripts" / "wiki.py"
sys.path.insert(0, str(SCRIPT.parent))

from runtime import require_python

require_python()
import wiki  # noqa: E402  (the engine itself, for table-level checks)

WIKI_CACHE = ".wiki"
CHECKED = []


def check(results, name, condition, detail=""):
    results.append((name, bool(condition), detail))
    if not condition:
        raise AssertionError(f"{name}: {detail}")


def run(root, *args, expect=0, stdin=None):
    proc = subprocess.run(
        [sys.executable, str(SCRIPT), *args, "--json"],
        cwd=root, capture_output=True, text=True, input=stdin,
    )
    payload = json.loads(proc.stdout) if proc.stdout.strip() else {}
    allowed = expect if isinstance(expect, tuple) else (expect,)
    if proc.returncode not in allowed:
        raise AssertionError(
            f"{' '.join(args)} exited {proc.returncode}, expected {expect}: {proc.stderr or proc.stdout}"
        )
    return payload


def payload_file(root, name, data):
    path = Path(root) / WIKI_CACHE / "payloads" / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(data, ensure_ascii=False), encoding="utf-8")
    return str(path)


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


def scenario_files(root):
    base = Path(root) / "场景能力"
    good = base / "account-opening"
    (good / "scripts").mkdir(parents=True)
    (good / "references").mkdir(parents=True)
    main = good / "SCENARIO.md"
    main.write_text(
        "---\n"
        "name: 开户流程\n"
        "description: 新客户开户时核验身份并登记资料\n"
        "triggers:\n"
        "  - 开户\n"
        "  - 新客户\n"
        "---\n"
        "# 开户流程\n\n"
        "1. 核验身份\n"
        "2. 登记资料\n",
        encoding="utf-8",
    )
    (good / "scripts/check.sh").write_text("echo check\n", encoding="utf-8")
    (good / "references/notes.md").write_text("开户材料说明\n", encoding="utf-8")
    broken = base / "broken"
    broken.mkdir()
    (broken / "SCENARIO.md").write_text(
        "---\nname: 坏场景\n---\n\n缺少 description。\n", encoding="utf-8"
    )
    english = Path(root) / "scenario" / "deck-flow"
    english.mkdir(parents=True)
    (english / "SCENARIO.md").write_text(
        "---\n"
        "name: English flow\n"
        "description: Flow stored under the English scenario directory\n"
        "triggers:\n"
        "  - deck\n"
        "---\n"
        "# English flow\n\n"
        "1. draft\n"
        "2. hand off\n",
        encoding="utf-8",
    )
    legacy = base / "deck-flow"
    legacy.mkdir()
    (legacy / "SCENARIO.md").write_text(
        "---\n"
        "name: Legacy deck\n"
        "description: Shadowed copy left behind in the Chinese directory\n"
        "---\n"
        "# Legacy deck\n\n"
        "1. stale\n",
        encoding="utf-8",
    )
    return main


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
        scenario_main = scenario_files(root)

        listing = run(root, "scenario", "list")["data"]
        check(results, "scenario list includes project and builtin entries",
              listing["count"] == 4 and listing["valid"] == 3, listing)
        check(results, "scenario list reports both project roots",
              listing["roots"] == ["scenario", "场景能力"], listing.get("roots"))
        check(results, "scenario commands leave no .wiki", not (root / ".wiki").exists(), listing)
        builtin = run(root, "scenario", "load", "话题探索")["data"]
        check(results, "builtin scenario is loadable without project files",
              builtin["scenario"]["source"] == "builtin"
              and builtin["path"].startswith("内置场景/")
              and "话题文件" in builtin["content"]
              and builtin["resources"] == [], builtin)
        check(results, "builtin scenario id cannot escape", run(root, "scenario", "load", "../x", expect=2)["error"]["code"] == "E_USAGE")
        loaded = run(root, "scenario", "load", "account-opening")["data"]
        check(results, "scenario load returns the whole flow",
              "核验身份" in loaded["content"] and loaded["path"].endswith("SCENARIO.md"), loaded)
        check(results, "scenario load inventories support files",
              {item["kind"] for item in loaded["resources"]} == {"script", "reference"}, loaded["resources"])
        english = run(root, "scenario", "load", "deck-flow")["data"]
        check(results, "scenario under scenario/ is loadable",
              english["path"].startswith("scenario/deck-flow/")
              and english["scenario"]["name"] == "English flow"
              and "hand off" in english["content"], english["path"])
        escaped = run(root, "scenario", "load", "../x", expect=2)
        check(results, "scenario id cannot escape the scenario root",
              escaped["error"]["code"] == "E_USAGE", escaped)

        data = run(root, "check")["data"]
        check(results, "check passes before init", data["ok"], data)

        # A ledger from an older skill is refused, not half-used.
        legacy = root / ".wiki"
        legacy.mkdir()
        legacy_db = sqlite3.connect(legacy / "wiki.db")
        legacy_db.executescript(
            "CREATE TABLE sources (key TEXT PRIMARY KEY, path TEXT, digest TEXT, size INTEGER,"
            " mtime REAL, raw TEXT, parser TEXT, status TEXT, updated_at TEXT);"
        )
        legacy_db.close()
        broken = run(root, "status", expect=3)
        check(results, "missing columns report E_STATE",
              broken.get("error", {}).get("code") == "E_STATE", broken)
        shutil.rmtree(legacy)
        shutil.rmtree(root / "wiki", ignore_errors=True)  # the probe touched the view too

        data = run(root, "init")["data"]
        check(results, "init creates .wiki", (root / ".wiki/wiki.db").exists() and data["sources"] == 0, data)
        check(results, "init creates the publish directory",
              (root / "wiki").is_dir() and not (root / ".wiki/raw").exists(), data)
        check(results, "publish README is written", (root / "wiki/README.md").exists())

        data = run(root, "check")["data"]
        check(results, "check passes after init", data["ok"], data)

        outside_file = root.parent / f"{root.name}-outside.md"
        outside_file.write_text("# 区外\n", encoding="utf-8")
        escaped = run(root, "status", "--root", str(root.parent), expect=2)
        check(results, "root outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)
        escaped = run(root, "read", str(outside_file), expect=2)
        check(results, "read outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)

        topic = root / "话题" / "复机办理.md"
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
        check(results, "ingest skips the scenario directory",
              not any(item["key"].startswith(("场景能力", "scenario")) for item in data["ingested"]), data["ingested"])
        check(results, "ingest skips topic files",
              not any(item["key"].startswith("话题/") for item in data["ingested"]), data["ingested"])
        check(results, "image classified unsupported", [i["key"] for i in data["unsupported"]] == ["scan.png"], data["unsupported"])

        check(results, "raw lives under the publish dir",
              (root / "wiki/raw").is_dir() and not (root / ".wiki/raw").exists())
        data = run(root, "ingest")["data"]
        raw_files = sorted(p.name for p in (root / "wiki/raw").glob("*.md"))
        check(results, "raw files are written into wiki/raw", len(raw_files) == 2, raw_files)

        # --- Task 3: 两段式初筛、--only、判定表 -------------------------------------
        raw_candidates = sorted((root / "wiki" / "raw").glob("a-*.md"))
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
        raw_digest, raw_size, raw_mtime = wiki.file_facts(raw_path)
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
        touched_digest, touched_size, touched_mtime = wiki.file_facts(raw_path)
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
        check(results, "query does not mix in scenario matches",
              "scenarios" not in found and "scenario_matches" not in found, found)
        searched = run(root, "scenario", "search", "开户")["data"]
        check(results, "scenario search finds a project scenario",
              searched["matches"] == 1 and searched["scenarios"][0]["id"] == "account-opening", searched)
        scenario_main.write_text(
            scenario_main.read_text(encoding="utf-8") + "\n补充：支持见证开户。\n",
            encoding="utf-8",
        )
        live = run(root, "scenario", "search", "见证开户")["data"]
        check(results, "scenario edits are searchable without ingest",
              live["matches"] == 1, live)
        invalid = run(root, "scenario", "search", "坏场景")["data"]
        check(results, "invalid scenarios stay out of scenario search",
              invalid["matches"] == 0, invalid)

        topics_hit = run(root, "query", "复机")["data"]
        builtin_hit = run(root, "scenario", "search", "话题")["data"]
        check(results, "scenario search finds the builtin explicitly",
              builtin_hit["matches"] >= 1
              and builtin_hit["scenarios"][0]["id"] == "话题探索"
              and builtin_hit["scenarios"][0]["source"] == "builtin",
              builtin_hit)
        check(results, "query finds the saved topic",
              topics_hit["topic_matches"] == 1
              and topics_hit["topics"][0]["name"] == "复机办理"
              and topics_hit["topics"][0]["read"].startswith("read "),
              topics_hit["topics"])
        check(results, "topic hits stay separate from page hits",
              topics_hit["matches"] == 0, topics_hit)

        tasks = run(root, "next", "--kind", "route", "--batch", "5")["data"]["tasks"]
        check(results, "route claims one task per source", len(tasks) == 2, tasks)
        check(results, "next returns brief and template", all(t.get("brief") and t.get("template") for t in tasks), tasks)
        check(results, "next hides task internals",
              not any(key in tasks[0] for key in ("epoch", "state", "inputs", "lease_until")), tasks[0])
        routes = {task["work"]["sources"][0]["key"]: task for task in tasks}
        check(results, "route template pre-fills the source",
              routes["a.md"]["template"]["decisions"][0]["source"] == "a.md", routes["a.md"]["template"])
        source_key = routes["a.md"]["work"]["sources"][0]["key"]
        raw = run(root, "read", f"raw:{source_key}")["data"]
        check(results, "read returns raw text", "复机" in raw["content"], raw)

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
        mirror = root / ".wiki/pages/process/anti-fraud-restore.md"
        check(results, "page submit writes rev 1", accepted["data"]["rev"] == 1 and mirror.exists(), accepted)

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
        check(results, "the mirror stays alias-free",
              "aliases:" not in mirror.read_text(encoding="utf-8"), mirror)
        engine.close()

        bad = run(root, "submit", task["id"],
                  "--payload", payload_file(root, "again.json", payload), expect=3)
        check(results, "closed task rejects resubmit", bad["error"]["code"] == "E_TASK_STALE", bad)

        status = run(root, "status")["data"]
        check(results, "status counts page and pending", status["pages"] == 1, status)
        found = run(root, "query", "复机")["data"]
        check(results, "query finds the page", found["matches"] == 1, found)
        page = run(root, "read", "page:anti-fraud-restore")["data"]
        check(results, "read page comes from the ledger", "两个工作日" in page["content"], page)
        check(results, "read page hides source digests", "source_digests" not in page["meta"], page["meta"])

        (root / "c.md").write_text("# 反诈复机\n\n补充：可拨打客服确认进度。\n", encoding="utf-8")
        run(root, "ingest")
        route = run(root, "next")["data"]["tasks"][0]
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

        # Task 6: 推送观看区（pages / raw / topics / scenarios / README）----
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
        check(results, "view readme lists the directories",
              all(word in readme for word in ("pages", "raw", "topics", "scenarios")), readme)
        check(results, "view readme names who owns what",
              "由引擎维护" in readme and "归你" in readme and "sync" in readme, readme)
        (root / "wiki/README.md").unlink()
        run(root, "status")
        check(results, "view readme is rewritten on every push",
              (root / "wiki/README.md").exists())

        check(results, "topics are seeded into the view dir",
              (root / "wiki/topics/复机办理.md").exists())
        check(results, "scenarios are seeded into the view dir",
              (root / "wiki/scenarios/account-opening/SCENARIO.md").exists()
              and (root / "wiki/scenarios/account-opening/scripts/check.sh").exists())
        deck = root / "wiki/scenarios/deck-flow/SCENARIO.md"
        check(results, "the scenario copy follows the scenario/ precedence",
              deck.exists()
              and "English flow" in deck.read_text(encoding="utf-8")
              and "Legacy deck" not in deck.read_text(encoding="utf-8"))
        (root / "wiki/topics/复机办理.md").write_text("我改过的话题\n", encoding="utf-8")
        run(root, "status")
        check(results, "user owned topic copies are never overwritten",
              (root / "wiki/topics/复机办理.md").read_text(encoding="utf-8") == "我改过的话题\n")

        page_file.unlink()  # 手改件留在树上会持续告警，也会干扰后面的漂移检查
        run(root, "status")
        check(results, "the hand edit is cleaned up for later blocks",
              page_file.exists() and "手改一段。" not in page_file.read_text(encoding="utf-8"))

        page = run(root, "read", "page:anti-fraud-restore")["data"]
        check(results, "update keeps old and new sources",
              set(page["meta"]["sources"]) == {"a.md", c_key}, page["meta"]["sources"])
        check(results, "update keeps old evidence",
              any(item.get("source") == "a.md" for item in page["meta"]["evidence"]),
              page["meta"]["evidence"])

        (root / "d.md").write_text("# 机房巡检\n\n巡检周期为每周一次。\n", encoding="utf-8")
        (root / "e.md").write_text("# 机房巡检\n\n巡检记录保留一年。\n", encoding="utf-8")
        run(root, "ingest")
        first = run(root, "next")["data"]["tasks"][0]
        first_key = first["work"]["sources"][0]["key"]
        run(root, "submit", first["id"], "--payload", payload_file(
            root, "batch-route-1.json",
            {"decisions": [{"source": first_key, "action": "new", "page": "machine-room-inspection",
                            "title": "机房巡检", "type": "note", "reason": "新主题"}]},
        ))
        second = run(root, "next")["data"]["tasks"][0]
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
            "page": "machine-room-inspection", "title": "机房巡检", "type": "note",
            "summary": "机房每周巡检一次，巡检记录保留一年。",
            "links": [], "body": "## 要点\n\n每周巡检一次；记录保留一年。\n",
            "evidence": [{"source": "d.md", "quote": "巡检周期为每周一次"},
                         {"source": "e.md", "quote": "巡检记录保留一年"}],
            "boundaries": {"covered": "巡检周期与记录保存", "not_covered": "巡检项目清单"},
        }
        run(root, "submit", task["id"], "--payload", payload_file(root, "batch-page.json", batch_payload))
        page = run(root, "read", "page:machine-room-inspection")["data"]
        check(results, "batch page keeps both sources",
              set(page["meta"]["sources"]) == {"d.md", "e.md"}, page["meta"]["sources"])

        mirror.write_text("手改的内容\n", encoding="utf-8")
        run(root, "status")
        drift = list((root / ".wiki/drift").glob("*.md"))
        check(results, "mirror edit kept as drift", drift and "手改" in drift[0].read_text(encoding="utf-8"), drift)
        check(results, "mirror restored from ledger", "两个工作日" in mirror.read_text(encoding="utf-8"), mirror)

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
        check(results, "page restore rolls back content", restored["rev"] == 4 and "两个工作日" in mirror.read_text(encoding="utf-8"), restored)

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
        aliased_page = run(root, "read", "page:anti-fraud-restore")["data"]
        check(results, "imported aliases do not leak into page meta",
              "aliases" not in aliased_page["meta"], aliased_page["meta"])

        exported = run(root, "export", str(root / ".wiki/export"))["data"]["files"]
        check(results, "export writes mirror copies", exported, exported)
        escaped = run(root, "export", str(root.parent / "wiki-export"), expect=2)
        check(results, "export outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)

        config_path = root / ".wiki/wiki.json"
        config = json.loads(config_path.read_text(encoding="utf-8"))
        for bad in ("../x", ".wiki", "a/b", "场景能力", "scenario", "", ".", "..", ".hidden", "/abs", "a\\b", "话题"):
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

        config["publish"] = {"dir": "../x", "enabled": False, "topics": True, "scenarios": True}
        config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
        disabled = run(root, "status", expect=2)
        check(results, "invalid publish dir is rejected even when disabled",
              disabled["error"]["code"] == "E_CONFIG", disabled)

        config["publish"] = {"dir": "view", "enabled": True, "topics": True, "scenarios": True}
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
        check(results, "the page task snapshot carries all eight fields",
              set(snap) == {"key", "source_size", "source_mtime", "source_digest",
                            "raw", "raw_size", "raw_mtime", "raw_digest"}, snap)
        check(results, "the snapshot records both the source and its raw copy",
              snap["raw"] != "" and snap["source_size"] > 0 and snap["raw_size"] > 0
              and len(snap["source_digest"]) == 32 and len(snap["raw_digest"]) == 32, snap)

        # CLI 路径会先被 ingest 判重与 attach_page_source 拦住，所以越界组合只能在
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
        before = run(root, "read", "page:machine-room-inspection")["data"]["meta"]
        check(results, "the page is stable before its raw copy changes",
              before["status"] == "stable", before)
        (root / raw_d).write_text(
            (root / raw_d).read_text(encoding="utf-8") + "\n巡检记录留档。\n",
            encoding="utf-8",
        )
        run(root, "ingest")
        stale = run(root, "read", "page:machine-room-inspection")["data"]["meta"]
        check(results, "editing a cited raw copy makes the page stale",
              stale["status"] == "stale", stale)
        untouched = run(root, "read", "page:anti-fraud-restore")["data"]["meta"]
        check(results, "editing an unrelated raw copy leaves the page stable",
              untouched["status"] == "stable", untouched)

        status = run(root, "status")["data"]
        check(results, "status counts invalidated tasks without opening them",
              status["tasks"].get("expired", 0) >= 1
              and status["tasks"]["open"] == status["tasks"]["queued"] + status["tasks"]["leased"],
              status["tasks"])

        # 树唯一性不变量：只统计 page 树。submit_route 在关闭 route 任务前先建 page 树，
        # route→page 的交接瞬间同一个 key 会同时挂两行，那是正常交接不是第二套树。
        conn = sqlite3.connect(root / ".wiki" / "wiki.db")
        twins = conn.execute(
            "SELECT ts.source_key, COUNT(DISTINCT ts.task_id) AS n "
            "FROM task_sources ts JOIN tasks t ON t.id = ts.task_id "
            "WHERE t.kind = 'page' AND t.state IN ('queued', 'leased') "
            "GROUP BY ts.source_key HAVING n > 1"
        ).fetchall()
        conn.close()
        check(results, "one source never sits in two open page task trees", twins == [], twins)

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

        dry = root / "dry"
        dry.mkdir()
        (dry / "note.md").write_text("# 标题\n\n正文。\n", encoding="utf-8")
        dry_run = subprocess.run(
            [sys.executable, str(SCRIPT), "ingest", "--dry-run", "--json"],
            cwd=dry, capture_output=True, text=True,
        )
        check(results, "dry run leaves no .wiki", dry_run.returncode == 0 and not (dry / ".wiki").exists(), dry_run.stderr)

        override = root / "场景能力" / "话题探索"
        override.mkdir()
        (override / "SCENARIO.md").write_text(
            "---\nname: 话题探索\ndescription: 项目自定义的话题流程\n---\n\n项目版流程。\n",
            encoding="utf-8",
        )
        overridden = run(root, "scenario", "load", "话题探索")["data"]
        check(results, "project scenario overrides the builtin",
              overridden["scenario"]["source"] == "project"
              and "项目版流程" in overridden["content"], overridden)

        healthy = run(root, "doctor", expect=(0, 1))["data"]
        check(results, "doctor runs", isinstance(healthy["issues"], list), healthy)
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
