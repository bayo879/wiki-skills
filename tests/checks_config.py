"""Publish configuration, size limits and path boundary checks.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import json
import shutil

from support import check, run


def publish_config(ctx):
    config_path = ctx.root / ".wiki/wiki.json"
    config = json.loads(config_path.read_text(encoding="utf-8"))
    for bad in ("../x", ".wiki", "a/b", "workflows", "", ".", "..", ".hidden", "/abs", "a\\b", "topics"):
        config["publish"]["dir"] = bad
        config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
        escaped = run(ctx.root, "status", expect=2)
        check(ctx.results, f"publish dir {bad} rejected",
              escaped["error"]["code"] == "E_CONFIG", escaped)
    config["publish"]["dir"] = "wiki"
    config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
    config["ingest"]["max_source_bytes"] = 10
    config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
    (ctx.root / "big.md").write_text("x" * 100, encoding="utf-8")
    oversized = run(ctx.root, "ingest", expect=1)["data"]
    check(ctx.results, "oversize classified", "big.md" in {item["key"] for item in oversized["oversize"]}, oversized)
    config["ingest"]["max_source_bytes"] = 50_000_000
    config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")

    config["publish"] = {"dir": "../x", "enabled": False, "topics": True, "workflows": True}
    config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
    disabled = run(ctx.root, "status", expect=2)
    check(ctx.results, "invalid publish dir is rejected even when disabled",
          disabled["error"]["code"] == "E_CONFIG", disabled)

    config["publish"] = {"dir": "view", "enabled": True, "topics": True, "workflows": True}
    config_path.write_text(json.dumps(config, ensure_ascii=False), encoding="utf-8")
    (ctx.root / "view").write_text("占位：同名文件不是目录\n", encoding="utf-8")
    blocked = run(ctx.root, "status", expect=3)
    check(ctx.results, "publish dir that is a file reports E_STATE",
          blocked.get("error", {}).get("code") == "E_STATE", blocked)
    (ctx.root / "view").unlink()
    outside_view = ctx.root.parent / f"{ctx.root.name}-view"
    outside_view.mkdir()
    try:
        (ctx.root / "view").symlink_to(outside_view, target_is_directory=True)
    except (OSError, NotImplementedError):
        pass
    else:
        escaping = run(ctx.root, "status", expect=2)
        check(ctx.results, "publish dir symlink outside the workspace reports E_PATH",
              escaping.get("error", {}).get("code") == "E_PATH", escaping)
        (ctx.root / "view").unlink()
    shutil.rmtree(outside_view, ignore_errors=True)
    seen = run(ctx.root, "status")["data"]
    check(ctx.results, "a legal non-default publish dir is created",
          (ctx.root / "view/raw").is_dir() and (ctx.root / "view/README.md").exists(), seen)
    raw_dir = ctx.root / "view/raw"
    shutil.rmtree(raw_dir)
    raw_dir.write_text("占位：同名文件不是目录\n", encoding="utf-8")
    blocked_raw = run(ctx.root, "status", expect=3)
    check(ctx.results, "publish raw dir that is a file reports E_STATE",
          blocked_raw.get("error", {}).get("code") == "E_STATE", blocked_raw)
    raw_dir.unlink()
    outside_raw = ctx.root.parent / f"{ctx.root.name}-raw"
    outside_raw.mkdir()
    try:
        raw_dir.symlink_to(outside_raw, target_is_directory=True)
    except (OSError, NotImplementedError):
        pass
    else:
        escaping_raw = run(ctx.root, "status", expect=2)
        check(ctx.results, "publish raw symlink outside the workspace reports E_PATH",
              escaping_raw.get("error", {}).get("code") == "E_PATH", escaping_raw)
        raw_dir.unlink()
    shutil.rmtree(outside_raw, ignore_errors=True)
    (ctx.root / "view/note.md").write_text("# 视图笔记\n\n用户放进视图的便签。\n", encoding="utf-8")
    ingested = run(ctx.root, "ingest")["data"]["ingested"]
    check(ctx.results, "the publish dir is skipped by ingest",
          not any(item["key"].startswith("view/") for item in ingested), ingested)
