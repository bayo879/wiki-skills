"""观看区：页面在发布树里的命名、README 与推送后的吸收。"""

from __future__ import annotations

import sys

import scenarios
from errors import WikiError
from pages import EVIDENCE_HEADING, meta_mismatch, split_evidence
from util import digest_text, write_atomic


def render_view_readme(wiki, records=None):
    """Directory guide for the view tree; rewritten on every push."""
    publish = wiki.publish_dir
    if records is None:
        records = wiki.all_pages()
    by_type = {}
    for record in records:
        kind = str(record["meta"].get("type") or "unknown")
        by_type[kind] = by_type.get(kind, 0) + 1
    raw_count = sum(1 for path in wiki.publish_raw_dir.rglob("*") if path.is_file())
    topic_count = sum(1 for path in (publish / "topics").rglob("*.md") if path.is_file())
    scenario_count = sum(
        1 for path in (publish / "scenarios").glob("*")
        if (path / scenarios.SCENARIO_FILE).is_file()
    )
    lines = [
        "# Wiki 视图",
        "",
        "pages/ 由引擎维护：按账本推送。raw/ 是原文副本，手改会被 ingest 采纳为账本事实。",
        "topics/ 与 scenarios/ 首次投放后归你：引擎不再覆盖、不会删除。",
        "pages/ 里手改过的文件不会被覆盖，只给告警；运行 sync 把它收进账本。",
        "其他文件（包括 .obsidian/）不会被删除。",
        "",
        "## 目录",
        "",
        f"- `pages/`：知识页，共 {len(records)} 篇。",
    ]
    for kind in sorted(by_type):
        lines.append(f"  - `{kind}/`：{by_type[kind]} 篇")
    lines.extend([
        f"- `raw/`：原文视图，共 {raw_count} 个文件。",
        f"- `topics/`：话题副本，共 {topic_count} 篇。",
        f"- `scenarios/`：场景副本，共 {scenario_count} 个。",
        "",
        "## 说明",
        "",
        "- 原文只留一份：删掉 `raw/` 里的文件后 `ingest` 会用源文件重建；改坏后删掉再 `ingest`。源文件内容改变且比 raw 新时 `ingest` 会主动重写；只改 mtime 不会触发。",
        "- `topics/`、`scenarios/` 是你自己的成果，改动与新增都不会被引擎同步或删除。",
    ])
    return "\n".join(lines) + "\n"


def sync_view_file(wiki, item, target, body_only, warnings=None):
    """回收一个已定位的文件；返回 "unchanged" / "written" / "imported"。

    拒绝时抛 WikiError，由 command_sync 记入 failed 后继续（R7）。
    """
    page_id = item["page"]
    record = item["record"]
    target_path = wiki.publish_dir / target
    canonical = wiki.view_text(record)
    if item["text"] == canonical:
        if item["path"] == target_path:
            if record["view_path"] != target or record["view_digest"] != digest_text(canonical):
                wiki.set_view_facts(page_id, record["rev"], target, digest_text(canonical))
            return "unchanged"
        write_atomic(target_path, canonical)
        item["path"].unlink()
        wiki.set_view_facts(page_id, record["rev"], target, digest_text(canonical))
        return "written"
    if record["view_digest"] and digest_text(item["text"]) == record["view_digest"]:
        # 这份字节正是上一次发布出去的（账本已前进，如账本侧改名）：不是手改（R11）。
        write_atomic(target_path, canonical)
        if item["path"] != target_path:
            item["path"].unlink()
        wiki.set_view_facts(page_id, record["rev"], target, digest_text(canonical))
        return "written"
    changed = meta_mismatch(item["meta"], record["meta"])
    if changed and not body_only:
        raise WikiError(
            "E_META_CHANGED",
            f"view copy changed {', '.join(changed)}；保留账本元数据、只收正文用 sync --body-only；确实要改元数据用 page import",
            "", 1,
        )
    body, evidence = split_evidence(item["body"])
    if not evidence:
        if EVIDENCE_HEADING in item["body"]:
            message = (f"{item['file']}: evidence block does not parse; "
                       "kept the ledger's evidence and adopted the text as body")
            sys.stderr.write(f"warning: {message}\n")
            if warnings is not None:
                warnings.append(message)
        evidence = list(record["meta"].get("evidence") or [])
    if not body.strip():
        raise WikiError("E_PAYLOAD", "the view copy has no body", "", 1)
    for entry in evidence:
        if not wiki.source_row(entry["source"]):
            raise WikiError(
                "E_UNKNOWN_SOURCE",
                f"evidence cites unregistered source {entry['source']}",
                "run `ingest` first, or fix the evidence block", 1,
            )
    merged = dict(record["meta"])
    merged["evidence"] = evidence
    if merged.get("type") not in wiki.config["schema"]["types"]:
        raise WikiError("E_PAYLOAD", f"type {merged.get('type')!r} not in schema.types", "", 1)
    rev = wiki.write_rev(page_id, merged, body, "sync")
    fresh = wiki.page_record(page_id)
    text = wiki.view_text(fresh)
    write_atomic(target_path, text)
    if item["path"] != target_path:
        item["path"].unlink()
    wiki.set_view_facts(page_id, rev, target, digest_text(text))
    return "imported"
