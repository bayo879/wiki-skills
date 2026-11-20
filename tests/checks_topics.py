"""Topic references, legacy topic notes and the topic exploration flow.

Check groups moved out of tests/selftest.py; run the whole suite with
python3 tests/selftest.py.
"""

import shutil
import tempfile
import unicodedata
from pathlib import Path

from support import check, payload_file, route_query, run, run_text
from fixtures import write_topic

# 引擎模块要等 support 把脚本目录加进 sys.path，因此排在 support 之后。
import cli
import topics


def topic_refs_checks(ctx):
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
    check(ctx.results, "topic refs parse four kinds",
          parsed["pages"] == ["anti-fraud-restore"]
          and parsed["raws"] == ["docs/a.md"]
          and parsed["topics"] == ["复机办理"]
          and parsed["workflows"] == ["topic-exploration"], parsed)
    check(ctx.results, "topic refs record malformed lines only for ref-shaped rows",
          parsed["malformed"] == ["- 页面：见下文", "- 页面`refund-policy`"], parsed)
    check(ctx.results, "topic refs ignore prose and other sections",
          "in-conclusion" not in parsed["pages"]
          and parsed["sections"] == 1, parsed)
    two = topics.parse_refs("## 依据\n\n- 页面：`a`\n\n## 依据\n\n- 页面：`b`\n")
    check(ctx.results, "second 依据 section still parses",
          two["sections"] == 2 and two["pages"] == ["a", "b"], two)
    fenced = topics.parse_refs(
        "## 依据\n\n- 页面：`real`\n\n```\n- 页面：`fake`\n## 依据\n```\n")
    check(ctx.results, "topic refs skip fenced code blocks",
          fenced["pages"] == ["real"] and fenced["sections"] == 1
          and not fenced["malformed"], fenced)
    tildes = topics.parse_refs(
        "## 依据\n\n- 页面：`real`\n\n~~~\n- 页面：`fake`\n## 依据\n~~~\n")
    check(ctx.results, "topic refs skip tilde fenced code blocks",
          tildes["pages"] == ["real"] and not tildes["malformed"]
          and tildes["sections"] == 1, tildes)
    nfd = unicodedata.normalize("NFD", "café")
    nfc = unicodedata.normalize("NFC", "café")
    precomposed = topics.parse_refs(f"## 依据\n\n- 话题：`{nfd}`\n")
    check(ctx.results, "topic ref tokens normalize to NFC",
          precomposed["topics"] == [nfc], precomposed)
    invalid = Path(tempfile.mkdtemp(prefix="llm-wiki-topicblank-"))
    try:
        write_topic(invalid / "wiki/topics", "空话题.md", "空话题", "无正文",
                    body="")
        blank = topics.scan(invalid, view_root=invalid / "wiki/topics")[0]
        check(ctx.results, "invalid topic entry keeps an empty refs record",
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
        check(ctx.results, "topic match carries parsed refs",
              matched and matched[0]["refs"]["pages"] == ["page-a"], matched)
        write_topic(root / "wiki/topics", nfd + ".md", nfc, "分解形式", body=body)
        write_topic(root / "topics", nfc + ".md", nfc, "旧根同名", body=body)
        scanned = topics.scan(root, view_root=root / "wiki/topics")
        decomposed = [item for item in scanned if item["name"] == nfc]
        check(ctx.results, "topic names normalize to NFC",
              len(decomposed) == 1 and decomposed[0]["origin"] == "view", scanned)
    finally:
        shutil.rmtree(root, ignore_errors=True)


def topic_refs_flow(ctx):
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
        check(ctx.results, "legacy topic refs parse too",
              legacy["origin"] == "legacy" and legacy["refs"]["pages"] == ["page-a"],
              legacy)
        query = run(root, "query", "复机")["data"]
        hit = [item for item in query["topics"] if item["name"] == "复机办理"][0]
        refs = hit["refs"]
        check(ctx.results, "query topic refs keep validated kinds",
              refs["pages"] == ["page-a"] and refs["raws"] == [raw_token]
              and refs["topics"] == ["复机旁证"]
              and refs["workflows"] == ["topic-exploration"], refs)
        check(ctx.results, "query topic refs put unknown targets in missing",
              refs["missing"] == [{"kind": "page", "target": "ghost-page"}], refs)
        listing = [item for item in query["results"] if item["id"] == "page-a"]
        check(ctx.results, "query results cite topics that reference the page",
              listing and listing[0]["citing_topics"] == ["复机办理", "旧库话题"], listing)
        detail = run(root, "query", "--page", "page-a")["data"]
        check(ctx.results, "query page cites topics",
              detail["citing_topics"] == ["复机办理", "旧库话题"], detail)
        page_text = run_text(root, "query", "--page", "page-a")
        check(ctx.results, "query page prints citing topics line",
              "citing topics: 复机办理, 旧库话题" in page_text.stdout, page_text.stdout)
        cite_index = cli.citing_topics_index({
            "旧库话题": {"pages": ["page-a"], "missing": []},
            "复机办理": {"pages": ["page-a"],
                         "missing": [{"kind": "page", "target": "ghost-page"}]},
            "空引用": {"pages": [], "missing": [
                {"kind": "page", "target": "ghost-page"}]},
        })
        check(ctx.results, "citing topics index sorts and ignores missing",
              cite_index == {"page-a": ["复机办理", "旧库话题"]}, cite_index)
        check(ctx.results, "citing topics line caps at three names plus +N",
              cli.ref_topics_line(["甲", "乙", "丙", "丁", "戊"]) == "甲, 乙, 丙, +2"
              and cli.ref_topics_line(["甲", "乙", "丙"]) == "甲, 乙, 丙"
              and cli.ref_topics_line([]) == "", "ref_topics_line")
        proc = run_text(root, "query", "复机")
        check(ctx.results, "query warns about broken refs without failing",
              proc.returncode == 0
              and "cites unknown page ghost-page" in proc.stderr
              and "malformed ref" in proc.stderr, proc.stderr)

        many = "\n".join(f"- 页面：`ghost-{index}`" for index in range(21))
        write_topic(root / "wiki/topics", "坏链话题.md", "坏链话题", "大量坏链",
                    body="## 依据\n\n" + many + "\n")
        capped = run(root, "query", "坏链")["data"]["topics"][0]["refs"]
        check(ctx.results, "missing refs cap at 20",
              len(capped["missing"]) == 20, len(capped["missing"]))

        raw_dir = (root / raw_rel).parent
        for index in range(21):
            (raw_dir / f"extra-{index}.md").write_text("# 补充\n\n说明。\n", encoding="utf-8")
        extra = "\n".join(f"- 原文：`raw:extra-{index}.md`" for index in range(21))
        write_topic(root / "wiki/topics", "超量话题.md", "超量话题", "超量原文",
                    body="## 依据\n\n" + extra + "\n")
        capped = run(root, "query", "超量")["data"]["topics"][0]["refs"]
        check(ctx.results, "valid refs cap at 20", len(capped["raws"]) == 20, len(capped["raws"]))

        many_pages = "\n".join(
            ["- 页面：`page-a`"]
            + [f"- 页面：`ghost-{index}`" for index in range(24)]
            + [f"- 原文：`raw:extra-{index}.md`" for index in range(5)])
        write_topic(root / "wiki/topics", "混合话题.md", "混合话题", "混合引用",
                    body="## 依据\n\n" + many_pages + "\n")
        capped = run(root, "query", "混合")["data"]["topics"][0]["refs"]
        check(ctx.results, "kept refs cap at 20 across kinds",
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
        check(ctx.results, "valid refs cap at 20 in total",
              sum(len(capped[key]) for key in
                  ("pages", "raws", "topics", "workflows")) == 20
              and len(capped["raws"]) == 10
              and capped["topics"] == [f"引用-{index:02d}" for index in range(10)]
              and capped["missing"] == [], capped)
        capped_text = run_text(root, "query", "封顶")
        capped_warnings = [line for line in capped_text.stderr.splitlines()
                           if "封顶话题" in line]
        check(ctx.results, "truncating refs warns once on stderr",
              len(capped_warnings) == 1
              and "refs truncated to 20" in capped_warnings[0], capped_text.stderr)

        write_topic(root / "wiki/topics", "越界话题.md", "越界话题", "越界原文",
                    body="## 依据\n\n- 原文：`raw:/etc/passwd`\n"
                         "- 原文：`raw:../ok.md`\n\n## 结论\n\n复机越界来源。\n")
        escaped = run(root, "query", "越界")["data"]["topics"][0]["refs"]
        check(ctx.results, "raw refs cannot escape the raw dir",
              escaped["raws"] == []
              and escaped["missing"] == [
                  {"kind": "raw", "target": "/etc/passwd"},
                  {"kind": "raw", "target": "../ok.md"}], escaped)

        write_topic(root / "wiki/topics", "根相对话题.md", "根相对话题", "根相对原文",
                    body="## 依据\n\n"
                         f"- 原文：`raw:{raw_rel}`\n"
                         f"- 原文：`{raw_rel}`\n")
        relative = run(root, "query", "根相对")["data"]["topics"][0]["refs"]
        check(ctx.results, "raw refs accept the workspace-relative sources path",
              relative["raws"] == [raw_rel] and relative["missing"] == [],
              relative)

        write_topic(root / "wiki/topics", "坏字节话题.md", "坏字节话题", "坏字节原文",
                    body="## 依据\n\n- 原文：`raw:bad\x00name.md`\n")
        broken_byte = run(root, "query", "坏字节")["data"]["topics"][0]["refs"]
        check(ctx.results, "a NUL byte in a raw ref goes to missing",
              broken_byte["raws"] == []
              and broken_byte["missing"] == [
                  {"kind": "raw", "target": "bad\x00name.md"}], broken_byte)
    finally:
        shutil.rmtree(root, ignore_errors=True)
