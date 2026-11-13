#!/usr/bin/env python3
"""End-to-end self test. Creates a disposable workspace, never touches real data.

Run:  python3 scripts/selftest.py [--keep]
Exit: 0 all checks passed, 1 a check failed.
"""

import argparse
import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "wiki.py"
CHECKS = []


def check(name, function):
    try:
        function()
    except AssertionError as error:
        CHECKS.append((name, "FAIL", str(error)))
        return False
    except Exception as error:  # noqa: BLE001 - report any harness error as a failure
        CHECKS.append((name, "ERROR", f"{type(error).__name__}: {error}"))
        return False
    CHECKS.append((name, "ok", ""))
    return True


def build_docx(path):
    xml = (
        '<?xml version="1.0" encoding="UTF-8"?>'
        '<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main">'
        "<w:body><w:p><w:r><w:t>采购审批流程：需求部门发起，财务复核，分管领导审批。</w:t>"
        "</w:r></w:p></w:body></w:document>"
    )
    with zipfile.ZipFile(path, "w") as archive:
        archive.writestr("[Content_Types].xml", "<Types/>")
        archive.writestr("word/document.xml", xml)


class Harness:
    def __init__(self, entry, workspace):
        self.entry = entry
        self.workspace = workspace

    def run(self, args, expect=0, cwd=None, timeout=120, input_text=None):
        completed = subprocess.run(
            [sys.executable, str(self.entry), *args],
            cwd=str(cwd or self.workspace),
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
            input=input_text,
        )
        assert completed.returncode == expect, (
            f"expected exit {expect}, got {completed.returncode}: {completed.stdout}{completed.stderr}"
        )
        payload = None
        if "--json" in args:
            try:
                payload = json.loads(completed.stdout)
            except json.JSONDecodeError as error:
                raise AssertionError(f"invalid JSON: {error}: {completed.stdout}") from error
        return completed, payload


def make_workspace(root):
    notes = root / "notes"
    notes.mkdir()
    (notes / "alpha.md").write_text(
        "# 反诈模型要求\n\n禁止混用非涉诈模型。存量模型要自查，新模型从严审核。\n",
        encoding="utf-8",
    )
    (notes / "beta.html").write_text(
        "<html><head><style>p{}</style><script>var x=1;</script></head>"
        "<body><h1>云网安全</h1><p>双零行动：变更操作零失误、云网安全零事故。</p></body></html>",
        encoding="utf-8",
    )
    build_docx(notes / "gamma.docx")
    (notes / "binary.bin").write_bytes(b"\x00\x01\x02\x03")


def snapshot(root):
    records = {}
    for path in sorted(Path(root).rglob("*")):
        if path.is_file():
            records[path.relative_to(root).as_posix()] = hashlib.sha256(path.read_bytes()).hexdigest()
    return records


def run_checks(harness, workspace):
    wiki = workspace / ".wiki"

    def first_ingest():
        _, payload = harness.run(["ingest", "--json"], expect=0)
        data = payload["data"]
        assert len(data["ingested"]) == 3, data["ingested"]
        assert len(data["unsupported"]) == 1, data["unsupported"]
        assert data["pending_total"] == 3, data["pending_total"]

    def read_raw():
        completed, _ = harness.run(["read", "raw:notes/alpha.md", "--limit", "5"])
        assert "反诈模型" in completed.stdout, completed.stdout
        completed, _ = harness.run(["read", "raw:notes/alpha.md", "--offset", "99", "--json"])
        payload = json.loads(completed.stdout)
        assert payload["data"]["content"] == "", payload

    def write_pages():
        body = workspace / ".drafts" / "alpha.md"
        body.parent.mkdir(exist_ok=True)
        body.write_text("双零行动见 [[beta-page]]。\n", encoding="utf-8")
        _, payload = harness.run([
            "page", "put", "--id", "alpha-page", "--title", "反诈模型要求",
            "--type", "concept", "--summary", "禁止混用非涉诈模型。",
            "--source", "notes/alpha.md", "--body", str(body), "--json",
        ])
        assert payload["ok"]
        _, payload = harness.run([
            "page", "put", "--id", "beta-page", "--title", "云网安全",
            "--type", "concept", "--summary", "双零行动的两项指标。",
            "--source", "notes/beta.html", "--body", "-", "--json",
        ], input_text="双零行动：变更操作零失误、云网安全零事故。\n")
        assert payload["ok"]

    def settle():
        _, payload = harness.run(["settle", "--json"], expect=1)
        assert [item["key"] for item in payload["data"]["settled"]] == ["notes/alpha.md", "notes/beta.html"]
        assert payload["data"]["pending"] == ["notes/gamma.docx"], payload["data"]
        assert payload["data"]["blocked"][0]["key"] == "notes/gamma.docx"
        harness.run(["settle", "notes/gamma.docx", "--skip", "--json"])
        _, payload = harness.run(["status", "--json"])
        assert payload["data"]["pending"] == [], payload["data"]

    def query_and_links():
        _, payload = harness.run(["query", "零事故", "--json"])
        assert payload["data"]["results"][0]["id"] == "beta-page", payload["data"]
        _, payload = harness.run(["query", "双零", "--json"])
        assert payload["data"]["returned"] >= 1
        _, payload = harness.run(["query", "--page", "alpha-page", "--json"])
        assert payload["data"]["outgoing"] == ["beta-page"], payload["data"]
        _, payload = harness.run(["query", "--page", "beta-page", "--json"])
        assert payload["data"]["incoming"] == ["alpha-page"], payload["data"]

    def stable_digest():
        before = snapshot(workspace)
        _, payload = harness.run(["ingest", "--json"])
        assert payload["data"]["ingested"] == [], payload["data"]
        assert len(payload["data"]["unchanged"]) == 3
        after = snapshot(workspace)
        assert before == after, "unchanged ingest rewrote files"

    def dry_run_pure():
        before = snapshot(workspace)
        _, payload = harness.run(["ingest", "--dry-run", "--force", "--json"])
        after = snapshot(workspace)
        assert before == after, "--dry-run changed files"
        assert payload["data"]["dry_run"] is True
        fresh = workspace / ".fresh"
        fresh.mkdir()
        (fresh / "note.md").write_text("新来源。\n")
        _, fresh_payload = harness.run([
            "ingest", "--dry-run", "--root", str(fresh), "--json",
        ])
        assert fresh_payload["root"] == str(fresh.resolve()), fresh_payload["root"]
        assert not (fresh / ".wiki").exists(), "dry-run initialized .wiki on a fresh root"
        assert [p.name for p in fresh.iterdir()] == ["note.md"]
        shutil.rmtree(fresh)

    def source_vanishes_and_returns():
        source = workspace / "notes" / "beta.html"
        backup = workspace / "beta.html.bak"
        source.rename(backup)
        _, payload = harness.run(["ingest", "--json"])
        assert payload["data"]["vanished"] == ["notes/beta.html"], payload["data"]
        _, payload = harness.run(["status", "--json"])
        assert payload["data"]["stale_pages"] == ["beta-page"], payload["data"]
        _, payload = harness.run(["doctor", "--json"])
        assert any(item["code"] == "L10" for item in payload["data"]["issues"]), payload["data"]
        backup.rename(source)
        _, payload = harness.run(["ingest", "--json"])
        assert payload["data"]["restored"] == ["notes/beta.html"], payload["data"]
        _, payload = harness.run(["status", "--json"])
        assert payload["data"]["stale_pages"] == [], payload["data"]

    def source_changes():
        (workspace / "notes" / "alpha.md").write_text(
            "# 反诈模型要求\n\n禁止混用非涉诈模型。新增：连续 6 个月未更新要评估效能。\n",
            encoding="utf-8",
        )
        _, payload = harness.run(["ingest", "--json"])
        assert [item["key"] for item in payload["data"]["ingested"]] == ["notes/alpha.md"]
        _, payload = harness.run(["settle", "--json"], expect=1)
        assert [item["key"] for item in payload["data"]["blocked"]] == ["notes/alpha.md"]
        assert payload["ok"] is False, payload
        body = workspace / ".drafts" / "alpha2.md"
        body.write_text("更新后的正文，引用 [[beta-page]]。\n", encoding="utf-8")
        harness.run([
            "page", "put", "--id", "alpha-page", "--title", "反诈模型要求",
            "--type", "concept", "--summary", "禁止混用非涉诈模型。",
            "--source", "notes/alpha.md", "--body", str(body), "--json",
        ])
        harness.run(["settle", "--json"])
        _, payload = harness.run(["status", "--json"])
        assert payload["data"]["pending"] == [], payload["data"]

    def rejected_inputs():
        _, payload = harness.run([
            "page", "put", "--id", "x", "--title", "t", "--type", "nope",
            "--summary", "s", "--source", "notes/alpha.md", "--body", "-", "--json",
        ], expect=2)
        assert payload["error"]["code"] == "E_BAD_TYPE"
        _, payload = harness.run(["page", "delete", "alpha-page", "--json"], expect=3)
        assert payload["error"]["code"] == "E_CONFIRM"
        _, payload = harness.run(["read", "../outside.md", "--json"], expect=2)
        assert payload["error"]["code"] == "E_PATH"

    def doctor_clean():
        _, payload = harness.run(["doctor", "--json"])
        assert payload["data"]["healthy"] is True, payload["data"]

    def portable():
        copy_root = Path(tempfile.mkdtemp(prefix="llm-wiki copy ")) / "技能 目录"
        try:
            shutil.copytree(SCRIPT.parent.parent, copy_root, ignore=shutil.ignore_patterns("__pycache__"))
            entry = copy_root / "scripts" / "wiki.py"
            completed = subprocess.run(
                [sys.executable, str(entry), "status", "--root", str(workspace), "--json"],
                cwd="/",
                capture_output=True,
                text=True,
                check=False,
            )
            assert completed.returncode == 0, completed.stderr
            payload = json.loads(completed.stdout)
            assert payload["data"]["pages"] == 2, payload["data"]
        finally:
            shutil.rmtree(copy_root.parent, ignore_errors=True)

    check("首次 ingest 自动建库并分类格式", first_ingest)
    check("read 支持窗口与越界", read_raw)
    check("建页与 stdin 正文", write_pages)
    check("settle 收口并保留未处理来源", settle)
    check("检索与双向链接", query_and_links)
    check("摘要未变不重写", stable_digest)
    check("dry-run 无副作用", dry_run_pure)
    check("来源消失标记 stale、恢复后复位", source_vanishes_and_returns)
    check("来源变更需重新收口", source_changes)
    check("非法输入返回稳定错误码", rejected_inputs)
    check("doctor 最终健康", doctor_clean)
    check("含空格与中文路径下可搬迁", portable)


def main(argv=None):
    parser = argparse.ArgumentParser()
    parser.add_argument("--keep", action="store_true", help="keep the temporary workspace")
    args = parser.parse_args(argv)
    workspace = Path(tempfile.mkdtemp(prefix="llm-wiki-selftest-"))
    try:
        make_workspace(workspace)
        harness = Harness(SCRIPT, workspace)
        run_checks(harness, workspace)
    finally:
        if args.keep:
            print(f"workspace kept: {workspace}")
        else:
            shutil.rmtree(workspace, ignore_errors=True)
    failures = [item for item in CHECKS if item[1] != "ok"]
    for name, status, detail in CHECKS:
        print(f"{status:5} {name}{': ' + detail if detail else ''}")
    print(f"\n{len(CHECKS) - len(failures)}/{len(CHECKS)} passed")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
