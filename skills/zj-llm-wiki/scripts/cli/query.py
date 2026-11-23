"""`query` 命令与话题引用工具：检索页面、话题与工作流候选。"""

from __future__ import annotations

import re
import search
import sys
import topics
import workflows
from pathlib import Path

from cli.common import ARGS, emit, open_wiki, session_owner
from errors import WikiError
from util import dump_meta


def query_task(wiki, words):
    """Register one search per topic word on a task, then show the hits.

    Words keep separate rankings on purpose: pooling them ranks a page that
    repeats a common word above the page that answers one specific word, and
    the agent reads the grouped output to judge similarity itself. The ledger is
    still read only once, so the per-word groups cost one scan in total.
    """
    row = wiki.task_row(ARGS.task)
    if not row:
        raise WikiError("E_TASK_UNKNOWN", f"no task {ARGS.task}",
                        "run `next` to get a task id", 2)
    if row["state"] != "leased":
        raise WikiError(
            "E_TASK_STALE",
            f"task {ARGS.task} is {row['state']}, not leased",
            "run `next` to claim it again, then query", 3,
        )
    held_by = wiki.lease_owner(ARGS.task)
    if held_by and held_by != session_owner():
        raise WikiError(
            "E_TASK_FOREIGN",
            f"task {ARGS.task} is leased to session {held_by}",
            "only the session that claimed a task can query on it; release it or run `next` here", 3,
        )
    if not words:
        raise WikiError("E_USAGE", "query --task needs topic words",
                        "example: query --task t_xxx 反诈 复机", 2)
    registered = wiki.query_words(ARGS.task)
    budget = wiki.query_budget(row)
    fresh = []
    seen = {word.lower() for word in registered}
    for word in words:
        if word.lower() in seen:
            continue
        seen.add(word.lower())
        fresh.append(word)
    if len(registered) + len(fresh) > budget:
        raise WikiError(
            "E_QUERY_BUDGET",
            f"task {ARGS.task} allows {budget} topic words: {len(registered)} used, "
            f"{len(fresh)} new would exceed it",
            "drop near-duplicates and keep the words that separate pages; "
            "the budget is 5 words per 1000 source characters",
            3,
        )
    limit = ARGS.limit or 5
    groups = []
    hits = {}
    for word, (matches, results) in zip(
            words, search.search_pages_grouped(wiki, words, limit)):
        groups.append({
            "word": word,
            "matches": matches,
            "returned": len(results),
            "results": results,
        })
        hits[word] = [item["id"] for item in results]
    wiki.append_task_query(ARGS.task, words, hits)
    view = wiki.query_view(row)
    sections = []
    for group in groups:
        block = [f"{group['word']}  ({group['matches']} matches, showing {group['returned']})"]
        if group["results"]:
            block.extend(
                f"{item['id']}  [{item['type']}/{item['status']}]  {item['title']}\n    {item['snippet']}"
                for item in group["results"]
            )
        else:
            block.append("  (no page matched: judge from page_index and pending_pages)")
        sections.append(block)
    sections.append([
        f"topic words {view['used']}/{view['budget']} used, log: {view['log']}",
        "a truncated hit list is not proof that no similar page exists",
    ])
    lines = []
    for block in sections:
        if lines:
            lines.append("")
        lines.extend(block)
    data = {
        "task": ARGS.task,
        "queries": groups,
        "budget": view,
        "boundary": "hit count is a ranked subset, not proof of full coverage",
    }
    summary = (
        f"{len(groups)} topic word(s) searched for task {ARGS.task}, "
        f"{view['used']}/{view['budget']} words used"
    )
    emit("query", wiki.root, summary, data, lines)
    wiki.close()
    return 0


TOPIC_REF_LIMIT = 20


TOPIC_REF_WARNINGS = 3


def resolve_topic_refs(wiki, entries):
    """Validate parsed topic refs; return ({topic: refs}, warnings)."""
    names = {entry["name"] for entry in entries}
    pages = wiki.page_ids()
    workflows_seen = {
        item["id"] for item in workflows.scan(
            wiki.root, view_root=wiki.publish_dir / workflows.WORKFLOW_DIR)
    }
    raw_base = wiki.publish_raw_dir
    try:
        raw_prefix = raw_base.relative_to(wiki.root).as_posix()
    except ValueError:
        raw_prefix = ""
    kind_order = (("pages", "page"), ("raws", "raw"),
                  ("topics", "topic"), ("workflows", "workflow"))
    resolved, warnings = {}, []
    for entry in entries:
        if not entry["valid"]:
            # `_entry` only parses refs for a topic that has a body.
            continue
        parsed = entry["refs"]
        refs = {"pages": [], "raws": [], "topics": [], "workflows": [], "missing": []}
        notes = [
            f"warning: topic {entry['name']} malformed ref: {line.strip()}"
            for line in parsed["malformed"]
        ]
        ordered = [
            (key, kind, value)
            for key, kind in kind_order
            for value in parsed[key]
        ]
        dropped = len(ordered) > TOPIC_REF_LIMIT
        for key, kind, value in ordered[:TOPIC_REF_LIMIT]:
            if kind == "raw":
                known = _raw_ref_ok(raw_base, value, raw_prefix)
            elif kind == "page":
                known = value in pages
            elif kind == "topic":
                known = value in names
            else:
                known = value in workflows_seen
            if known:
                refs[key].append(value)
            else:
                refs["missing"].append({"kind": kind, "target": value})
                notes.append(
                    f"warning: topic {entry['name']} cites unknown {kind} {value}")
        if parsed["sections"] > 1:
            notes.append(
                f"warning: topic {entry['name']} has {parsed['sections']} 依据 sections")
        if dropped:
            notes.append(
                f"warning: topic {entry['name']} refs truncated to {TOPIC_REF_LIMIT}")
        resolved[entry["name"]] = refs
        warnings.extend(notes[:TOPIC_REF_WARNINGS])
    return resolved, warnings


def _raw_ref_ok(raw_base, value, raw_prefix=""):
    """A raw ref must stay inside the published raw tree and exist.

    `query --page` prints `sources[].raw` as a workspace-relative path
    (`wiki/raw/x.md`); accept that form as well as the bare `raw/` one.
    """
    text = value
    if raw_prefix and (text == raw_prefix or text.startswith(raw_prefix + "/")):
        text = text[len(raw_prefix):].lstrip("/")
    path = Path(text)
    if not text or path.is_absolute() or ".." in path.parts:
        return False
    try:
        return (raw_base / path).is_file()
    except (OSError, ValueError):
        return False


def citing_topics_index(refs_by_topic):
    """{page id: sorted topic names} from validated page refs only."""
    index = {}
    for name, refs in refs_by_topic.items():
        for page_id in refs["pages"]:
            index.setdefault(page_id, []).append(name)
    return {page_id: sorted(names) for page_id, names in index.items()}


def ref_topics_line(names):
    """`甲, 乙, 丙, +2`; empty list renders as an empty string."""
    if not names:
        return ""
    shown = ", ".join(names[:3])
    if len(names) > 3:
        shown += f", +{len(names) - 3}"
    return shown


def command_query():
    words = [
        word
        for chunk in (ARGS.text or [])
        for word in re.split(r"\s+", chunk.strip())
        if word
    ]
    if ARGS.task and not ARGS.page:
        wiki, _ = open_wiki()
        return query_task(wiki, words)
    wiki, _ = open_wiki(readonly=True)
    if ARGS.page:
        record = wiki.page_record(ARGS.page)
        if not record:
            raise WikiError("E_NOT_FOUND", f"no page {ARGS.page}", "search with `query <text>` first", 3)
        outgoing, incoming = wiki.links_for(ARGS.page)
        entries = topics.scan(wiki.root, view_root=wiki.publish_dir / topics.TOPIC_DIR)
        refs_by_topic, ref_warnings = resolve_topic_refs(wiki, entries)
        for line in ref_warnings:
            sys.stderr.write(line + "\n")
        cited = citing_topics_index(refs_by_topic).get(ARGS.page, [])
        citing_line = ref_topics_line(cited)
        # 源文件路径是账本事实：来源只存 key，副本路径按 raw_id 查 raws 表渲染出来，
        # 调用方（agent）不用再按来源名猜文件名。
        sources = []
        for key in record["meta"]["sources"]:
            row = wiki.source_row(key)
            raw = row["raw"] if row else ""
            sources.append({"key": key, "raw": raw or None})
        data = {
            "page": dump_meta(record["meta"]),
            "rev": record["rev"],
            "path": wiki.page_ref(record),
            "sources": sources,
            "outgoing": outgoing,
            "incoming": incoming,
            "citing_topics": cited,
            "body": record["body"],
        }
        emit(
            "query", wiki.root,
            f"page {ARGS.page} rev {record['rev']}: {len(outgoing)} outgoing, {len(incoming)} incoming links",
            data,
            [f"outgoing: {', '.join(outgoing) or '(none)'}",
             f"incoming: {', '.join(incoming) or '(none)'}",
             *([f"citing topics: {citing_line}"] if citing_line else []),
             f"raw: {', '.join(item['raw'] for item in sources if item['raw']) or '(none)'}",
             "",
             record["body"]],
        )
        wiki.close()
        return 0
    terms = words
    if not terms:
        raise WikiError("E_USAGE", "query needs text, --page or --task",
                        "example: query 反诈 复机", 2)
    limit = ARGS.limit or 8
    scored, results = search.search_pages(wiki, terms, limit)
    entries = topics.scan(wiki.root, view_root=wiki.publish_dir / topics.TOPIC_DIR)
    topic_hits = topics.rank(entries, terms, limit)
    refs_by_topic, ref_warnings = resolve_topic_refs(wiki, entries)
    for line in ref_warnings:
        sys.stderr.write(line + "\n")
    for item in topic_hits:
        item["refs"] = refs_by_topic[item["name"]]
    workflow_hits = workflows.match(
        wiki.root, terms, 5, view_root=wiki.publish_dir / workflows.WORKFLOW_DIR)
    cited = citing_topics_index(refs_by_topic)
    for item in results:
        item["citing_topics"] = cited.get(item["id"], [])
    data = {
        "query": " ".join(terms),
        "matches": len(scored),
        "returned": len(results),
        "results": results,
        "topic_matches": len(topic_hits),
        "topics": topic_hits,
        "workflow_matches": len(workflow_hits),
        "workflows": workflow_hits,
        "boundary": "hit count is a ranked subset, not proof of full coverage",
    }
    sections = []
    if workflow_hits:
        block = ["workflows (check applies before loading):"]
        for item in workflow_hits:
            alias = f"（{item['alias_name']}）" if item["alias_name"] else ""
            block.append(f"{item['id']}{alias}  {item['description']}")
            block.append(f"    applies: {item['applies'] or '(none)'}")
            block.append(f"    load: {item['load']}")
        sections.append(block)
    pages = []
    for item in results:
        entry = (
            f"{item['id']}  [{item['type']}/{item['status']}]  "
            f"{item['title']}\n    {item['snippet']}"
        )
        cite = ref_topics_line(item.get("citing_topics") or [])
        if cite:
            entry += f"\n    citing topics: {cite}"
        pages.append(entry)
    if pages:
        sections.append(pages)
    if topic_hits:
        block = ["topics (read before exploring the same theme again):"]
        for item in topic_hits:
            block.append(f"{item['name']}  [{item['status'] or 'open'}]  {item['description']}")
            block.append(f"    path: {item['path']}")
            refs = item["refs"]
            packed = [f"page:{value}" for value in refs["pages"]]
            packed += [f"raw:{value}" for value in refs["raws"]]
            packed += [f"topic:{value}" for value in refs["topics"]]
            packed += [f"workflow:{value}" for value in refs["workflows"]]
            if packed:
                block.append(f"    refs: {', '.join(packed)}")
            if refs["missing"]:
                block.append("    missing: " + ", ".join(
                    f"{miss['kind']}:{miss['target']}" for miss in refs["missing"]))
        sections.append(block)
    lines = []
    for block in sections:
        if lines:
            lines.append("")
        lines.extend(block)
    summary = f"{len(scored)} matching pages"
    if topic_hits:
        summary += f", {len(topic_hits)} matching topics"
    if workflow_hits:
        summary += f", {len(workflow_hits)} workflow candidates"
    summary += f", showing {len(results)} pages for {' '.join(terms)}"
    emit("query", wiki.root, summary, data, lines)
    wiki.close()
    return 0
