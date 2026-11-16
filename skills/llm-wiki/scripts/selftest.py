#!/usr/bin/env python3
"""End-to-end self test for the task-driven wiki engine.

Runs a disposable knowledge base in a temp directory: ingest, route decisions,
page submission, mirror drift, import/restore, classification, dry run. Never
touches real data and only uses the standard library.

Run:  python3 scripts/selftest.py [--keep] [--json]
Exit: 0 all checks passed, 1 a check failed.
"""

import argparse
import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "wiki.py"
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

        data = run(root, "init")["data"]
        check(results, "init creates .wiki", (root / ".wiki/wiki.db").exists() and data["sources"] == 0, data)

        outside_file = root.parent / f"{root.name}-outside.md"
        outside_file.write_text("# 区外\n", encoding="utf-8")
        escaped = run(root, "status", "--root", str(root.parent), expect=2)
        check(results, "root outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)
        escaped = run(root, "read", str(outside_file), expect=2)
        check(results, "read outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)

        data = run(root, "ingest")["data"]
        check(results, "ingest queues route tasks", data["pending_total"] == 2, data)
        check(results, "image classified unsupported", [i["key"] for i in data["unsupported"]] == ["scan.png"], data["unsupported"])

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

        bad = run(root, "submit", task["id"],
                  "--payload", payload_file(root, "again.json", payload), expect=3)
        check(results, "closed task rejects resubmit", bad["error"]["code"] == "E_TASK_STALE", bad)

        status = run(root, "status")["data"]
        check(results, "status counts page and pending", status["pages"] == 1, status)
        found = run(root, "query", "复机")["data"]
        check(results, "query finds the page", found["matches"] == 1, found)
        page = run(root, "read", "page:anti-fraud-restore")["data"]
        check(results, "read page comes from the ledger", "两个工作日" in page["content"], page)

        mirror.write_text("手改的内容\n", encoding="utf-8")
        run(root, "status")
        drift = list((root / ".wiki/drift").glob("*.md"))
        check(results, "mirror edit kept as drift", drift and "手改" in drift[0].read_text(encoding="utf-8"), drift)
        check(results, "mirror restored from ledger", "两个工作日" in mirror.read_text(encoding="utf-8"), mirror)

        edited = root / ".wiki/drafts/edited.md"
        edited.write_text("---\ntitle: 反诈复机\n---\n## 结论\n\n人工补充：可拨打客服确认进度。\n", encoding="utf-8")
        imported = run(root, "page", "import", "anti-fraud-restore", "--from", str(edited))["data"]
        check(results, "page import adds a revision", imported["rev"] == 2, imported)
        outside_edit = root.parent / f"{root.name}-edit.md"
        outside_edit.write_text("## 区外\n", encoding="utf-8")
        escaped = run(root, "page", "import", "anti-fraud-restore",
                      "--from", str(outside_edit), expect=2)
        check(results, "import outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)
        outside_edit.unlink()
        restored = run(root, "page", "restore", "anti-fraud-restore", "--rev", "1")["data"]
        check(results, "page restore rolls back content", restored["rev"] == 3 and "两个工作日" in mirror.read_text(encoding="utf-8"), restored)

        exported = run(root, "export", str(root / ".wiki/export"))["data"]["files"]
        check(results, "export writes mirror copies", exported, exported)
        escaped = run(root, "export", str(root.parent / "wiki-export"), expect=2)
        check(results, "export outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)

        config_path = root / ".wiki/wiki.json"
        config = json.loads(config_path.read_text(encoding="utf-8"))
        config["ingest"]["max_source_bytes"] = 10
        config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
        (root / "big.md").write_text("x" * 100, encoding="utf-8")
        oversized = run(root, "ingest", expect=1)["data"]
        check(results, "oversize classified", "big.md" in {item["key"] for item in oversized["oversize"]}, oversized)
        config["ingest"]["max_source_bytes"] = 50_000_000
        config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")

        dry = root / "dry"
        dry.mkdir()
        (dry / "note.md").write_text("# 标题\n\n正文。\n", encoding="utf-8")
        dry_run = subprocess.run(
            [sys.executable, str(SCRIPT), "ingest", "--dry-run", "--json"],
            cwd=dry, capture_output=True, text=True,
        )
        check(results, "dry run leaves no .wiki", dry_run.returncode == 0 and not (dry / ".wiki").exists(), dry_run.stderr)

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
