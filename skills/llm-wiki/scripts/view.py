"""观看区：页面在发布树里的命名、README 与推送后的吸收。"""

from __future__ import annotations

import shutil
import sys
from pathlib import Path

import workflows
import topics
from errors import WikiError
from pages import EVIDENCE_HEADING, meta_mismatch, split_evidence, split_relations, view_name
from util import digest_text, norm_key, write_atomic
from workspace import window_safe


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
    workflow_count = sum(
        1 for path in (publish / "workflows").glob("*")
        if (path / workflows.WORKFLOW_FILE).is_file()
    )
    lines = [
        "# Wiki 视图",
        "",
        "pages/ 由引擎维护：按账本推送。raw/ 是原文副本，手改会被 ingest 采纳为账本事实。",
        "topics/ 与 workflows/ 首次投放后归你：引擎不再覆盖、不会删除。",
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
        f"- `workflows/`：工作流副本，共 {workflow_count} 个。",
    ])
    lines.extend([
        "",
        "## 说明",
        "",
        "- 原文只留一份：删掉 `raw/` 里的文件后 `ingest` 会用源文件重建；改坏后删掉再 `ingest`。源文件内容改变且比 raw 新时 `ingest` 会主动重写；只改 mtime 不会触发。",
        "- `topics/`、`workflows/` 是你自己的成果，改动与新增都不会被引擎同步或删除。",
    ])
    return "\n".join(lines) + "\n"


def sync_view_file(wiki, item, target, body_only, warnings=None):
    """回收一个已定位的文件；返回 "unchanged" / "written" / "imported"。

    拒绝时抛 WikiError，由 command_sync 记入 failed 后继续（R7）。
    """
    page_id = item["page"]
    record = item["record"]
    target_path = wiki.publish_dir / target
    canonical = wiki.view_text(record, wiki.view_repo_path(target))
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
    # Relations are engine-owned and render after the evidence block, so drop
    # them before looking for a trailing evidence block. A view written by an
    # older revision can also carry repeated evidence sections; strip every
    # valid trailing one and keep the newest (R5).
    body = split_relations(item["body"])
    body, evidence = split_evidence(body)
    while EVIDENCE_HEADING in body:
        stripped, _ = split_evidence(body)
        if stripped == body:
            break
        body = stripped
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
    text = wiki.view_text(fresh, wiki.view_repo_path(target))
    write_atomic(target_path, text)
    if item["path"] != target_path:
        item["path"].unlink()
    wiki.set_view_facts(page_id, rev, target, digest_text(text))
    return "imported"


class ViewMixin:
    """把账本推成观看区，并把用户的改动吸收回账本；由 Wiki 组合。"""

    def sync_view(self):
        """Push the ledger into the view tree; return one line per action."""
        if not self.config["publish"]["enabled"]:
            return []
        events = []
        # raw/ 由 ingest 维护：这里只保证目录存在，绝不写、删其中任何文件。
        self.publish_raw_dir.mkdir(parents=True, exist_ok=True)
        records = self.all_pages()
        self._sync_view_pages(events, records)
        publish = self.config["publish"]
        if publish.get("topics", True):
            self._seed_view_topics(events)
        if publish.get("workflows", True):
            self._seed_view_workflows(events)
        write_atomic(self.publish_dir / "README.md", render_view_readme(self, records))
        events.append("README.md rewritten")
        return events

    def _view_targets(self, records):
        """Publish path per page for one snapshot; same rules as view_relpath."""
        shared = {}
        for record in records:
            key = (record["meta"].get("type"),
                   view_name(record["meta"].get("title"), record["id"]))
            shared[key] = shared.get(key, 0) + 1
        targets = {}
        for record in records:
            meta = record["meta"]
            base = view_name(meta.get("title"), record["id"])
            relpath = f"pages/{meta['type']}/{base}.md"
            taken = shared[(meta.get("type"), base)] > 1 or (
                (self.publish_dir / relpath).exists() and record["view_path"] != relpath
            )
            if taken:
                relpath = f"pages/{meta['type']}/{base} ({record['id']}).md"
            targets[record["id"]] = relpath
        return targets

    def _sync_view_pages(self, events, records):
        """Publish every page; a hand-edited copy is skipped, never overwritten."""
        base = self.publish_dir.resolve()
        targets = self._view_targets(records)
        for record in records:
            target = targets[record["id"]]
            desired = self.view_text(record, self.view_repo_path(target))
            target_digest = digest_text(desired)
            published = record["view_path"]
            known = record["view_digest"]
            target_path = self.publish_dir / target
            old_path = self.publish_dir / published if published else None
            if not window_safe(base, target_path.resolve()) or (
                    old_path is not None and not window_safe(base, old_path.resolve())):
                events.append(f"warning: skip {record['id']}: view path escapes the publish dir")
                continue
            old_text = old_path.read_text(encoding="utf-8") if old_path and old_path.is_file() else None
            target_text = target_path.read_text(encoding="utf-8") if target_path.is_file() else None

            if old_text is not None and digest_text(old_text) != known:
                # P 被手改。唯一例外：Q == P 且磁盘恰好已是 T —— 那不是手改，是我们上次
                # 写完还没来得及回写事实（崩溃窗口），直接补事实，不覆盖任何东西。
                if target == published and target_text == desired:
                    self._publish_facts(record, target, target_digest, events)
                else:
                    events.append(
                        f"warning: {record['id']} was edited by hand at {published}; "
                        "published copy left alone (run `sync` to absorb it)"
                    )
                continue

            if target_text != desired:  # 已等于 T 时只补事实
                write_atomic(target_path, desired)
                events.append(f"{target} written")
            if published and published != target and old_text is not None \
                    and digest_text(old_text) == known:
                old_path.unlink()
                events.append(f"{published} removed")
            self._publish_facts(record, target, target_digest, events)

    def _publish_facts(self, record, view_path, view_digest, events):
        if record["view_path"] == view_path and record["view_digest"] == view_digest:
            return
        self.set_view_facts(record["id"], record["rev"], view_path, view_digest)
        record["view_path"] = view_path
        record["view_digest"] = view_digest
        events.append(f"{record['id']} facts updated")

    def set_view_facts(self, page_id, rev, view_path, view_digest):
        """Record what is now published for one revision (R1)."""
        self.conn.execute(
            "UPDATE page_revs SET view_path = ?, view_digest = ? "
            "WHERE page = ? AND rev = ?",
            (view_path, view_digest, page_id, rev),
        )
        self.conn.commit()

    def _seed_view_topics(self, events):
        """Copy each topic once; afterwards the copy belongs to the user."""
        destination = self.publish_dir / "topics"
        for entry in topics.scan(self.root):
            source = self.root / entry["path"]
            target = destination / Path(entry["path"]).name
            if target.exists():
                continue
            if not window_safe(self.root, source.resolve()):
                events.append(f"warning: skip topic {entry['path']}: source escapes the workspace")
                continue
            destination.mkdir(parents=True, exist_ok=True)
            shutil.copy2(source, target)
            events.append(f"{norm_key(target.relative_to(self.root))} seeded")

    def _seed_view_workflows(self, events):
        """Copy each project workflow once; built-in workflows never enter the view."""
        destination = self.publish_dir / "workflows"
        for entry in workflows.scan(self.root, builtin=False):
            source = self.root / entry["path"]
            target = destination / entry["id"]
            if target.exists() or not source.is_dir():
                continue
            if not window_safe(self.root, source.resolve()):
                events.append(f"warning: skip workflow {entry['id']}: source escapes the workspace")
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copytree(source, target)
            events.append(f"{norm_key(target.relative_to(self.root))} seeded")
