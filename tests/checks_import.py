"""Page import, restore and alias checks.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

from support import check, run


def import_and_restore(ctx):
    edited = ctx.root / ".wiki/drafts/edited.md"
    edited.write_text("---\ntitle: 反诈复机\n---\n## 结论\n\n人工补充：可拨打客服确认进度。\n", encoding="utf-8")
    imported = run(ctx.root, "page", "import", "anti-fraud-restore", "--from", str(edited))["data"]
    check(ctx.results, "page import adds a revision", imported["rev"] == 3, imported)
    outside_edit = ctx.root.parent / f"{ctx.root.name}-edit.md"
    outside_edit.write_text("## 区外\n", encoding="utf-8")
    escaped = run(ctx.root, "page", "import", "anti-fraud-restore",
                  "--from", str(outside_edit), expect=2)
    check(ctx.results, "import outside workspace rejected", escaped["error"]["code"] == "E_PATH", escaped)
    outside_edit.unlink()
    restored = run(ctx.root, "page", "restore", "anti-fraud-restore", "--rev", "1")["data"]
    restored_page = run(ctx.root, "query", "--page", "anti-fraud-restore")["data"]
    check(ctx.results, "page restore rolls back content",
          restored["rev"] == 4 and "两个工作日" in restored_page["body"], restored)

    aliased = ctx.root / ".wiki/drafts/aliased.md"
    aliased.write_text(
        "---\ntitle: 反诈复机\naliases: [anti-fraud-restore]\n---\n"
        "## 结论\n\n导入时 aliases 不应进入账本。\n",
        encoding="utf-8",
    )
    imported_alias = run(
        ctx.root, "page", "import", "anti-fraud-restore", "--from", str(aliased)
    )["data"]
    check(ctx.results, "page import accepts an aliased source file",
          imported_alias["rev"] == 5, imported_alias)
    aliased_page = run(ctx.root, "query", "--page", "anti-fraud-restore")["data"]
    check(ctx.results, "imported aliases do not leak into page meta",
          "aliases" not in aliased_page["page"], aliased_page["page"])
