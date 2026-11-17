"""Page payload validation.

Kept out of wiki.py so the engine's command surface stays under its size
budget; these rules are pure functions over ledger records the engine passes in.
"""

from __future__ import annotations

import json
import re

from errors import WikiError
from pages import normalize_links, split_relations
from tasks import missing_sections
from workspace import inside_root

BODY_MAX_CHARS = 60_000
SUMMARY_MAX_CHARS = 200


def check_links(page_id, links, known, context=""):
    """Every edge must point at a page that exists at submit time."""
    prefix = f"{context}: " if context else ""
    for link in links:
        if link == page_id:
            continue
        if link not in known:
            raise WikiError(
                "E_LINK_UNRESOLVED",
                f"{prefix}link {link!r} points at no page",
                "create that page first, or drop the link", 2,
            )


def validate_page_payload(wiki, row, payload, sources):
    inputs = json.loads(row["inputs"])
    page_id = inputs["page"]
    record = wiki.page_record(page_id) if inputs.get("mode") == "update" else None
    old_meta = (record or {}).get("meta") or {}
    old_sources = set(old_meta.get("sources") or [])
    if not isinstance(payload, dict):
        raise WikiError("E_PAYLOAD", "page payload must be a JSON object", "", 2)
    if str(payload.get("page") or "").strip().lower() != page_id:
        raise WikiError("E_PAYLOAD", f"payload.page must be {page_id!r}",
                        "the task subject is the only page you may write", 2)
    title = str(payload.get("title") or "").strip()
    if not title or len(title) > 120:
        raise WikiError("E_PAYLOAD", "title must be 1-120 characters", "", 2)
    page_type = str(payload.get("type") or "").strip()
    if page_type not in wiki.config["schema"]["types"]:
        raise WikiError("E_PAYLOAD", f"type {page_type!r} not in schema.types",
                        f"choose one of: {', '.join(wiki.config['schema']['types'])}", 2)
    summary = str(payload.get("summary") or "").strip()
    if not summary:
        raise WikiError("E_PAYLOAD", "summary must not be empty", "", 2)
    if len(summary) > SUMMARY_MAX_CHARS:
        raise WikiError("E_PAYLOAD", f"summary is {len(summary)} chars, max {SUMMARY_MAX_CHARS}",
                        "move detail into the body", 2)
    raw_links = payload.get("links") or []
    if not isinstance(raw_links, list) or any(not isinstance(item, str) for item in raw_links):
        raise WikiError("E_PAYLOAD", "links must be a list of page ids", "", 2)
    links = normalize_links(raw_links)
    known = wiki.page_ids()
    check_links(page_id, links, known)
    body = payload.get("body")
    if body is None and payload.get("body_file"):
        candidate = inside_root(
            wiki.root, payload["body_file"], "body_file",
            "put drafts under .wiki/drafts/",
        )
        try:
            body = candidate.read_text(encoding="utf-8")
        except OSError as error:
            raise WikiError("E_PAYLOAD", f"cannot read body_file: {error}", "", 2)
    if not isinstance(body, str) or not body.strip():
        raise WikiError("E_PAYLOAD", "body must be non-empty text (or use body_file)", "", 2)
    if body.lstrip().startswith("---"):
        raise WikiError("E_PAYLOAD", "body must not start with frontmatter",
                        "send only the markdown sections; the engine renders frontmatter", 2)
    # Relations are engine-owned. An older template (or a copied view file) may
    # still carry the section; drop it instead of storing a second edge source.
    body = split_relations(body)
    if inputs.get("mode") == "create":
        missing = missing_sections(body, page_type)
        if missing:
            raise WikiError(
                "E_PAYLOAD",
                f"body is missing required section(s): {', '.join(missing)}",
                "start from the task template.body and fill it; do not rename or drop sections", 2,
            )
    if len(body) > BODY_MAX_CHARS:
        raise WikiError("E_PAYLOAD", f"body is {len(body)} chars, max {BODY_MAX_CHARS}",
                        "split the page or move detail into linked pages", 2)
    evidence = payload.get("evidence")
    if not isinstance(evidence, list) or not evidence:
        raise WikiError("E_PAYLOAD", "evidence must be a non-empty list of {source, quote}",
                        "quote at least one sentence from the task sources", 2)
    available = {item["key"] for item in sources} | old_sources
    for item in evidence:
        if not isinstance(item, dict):
            raise WikiError("E_PAYLOAD", "each evidence item must be an object", "", 2)
        key = item.get("source")
        quote = str(item.get("quote") or "")
        if key not in available:
            raise WikiError(
                "E_TASK_SCOPE", f"evidence source {key!r} is not in this task",
                f"task sources: {', '.join(sorted(available))}", 3,
            )
        if not quote.strip():
            raise WikiError("E_PAYLOAD", f"{key}: quote must not be empty", "", 2)
        raw = re.sub(r"\s+", "", wiki.source_body(key))
        if re.sub(r"\s+", "", quote) not in raw:
            raise WikiError(
                "E_EVIDENCE_NOT_FOUND",
                f"quote not found in {key}: {quote[:60]!r}",
                "re-read the raw source and copy an exact sentence", 2,
            )
    if old_meta:
        cited = {item.get("source") for item in evidence}
        evidence.extend(
            item for item in old_meta.get("evidence") or []
            if isinstance(item, dict) and item.get("source") not in cited
        )
    boundaries = payload.get("boundaries")
    if not isinstance(boundaries, dict) or not str(boundaries.get("covered") or "").strip() \
            or not str(boundaries.get("not_covered") or "").strip():
        raise WikiError("E_PAYLOAD", "boundaries.covered and boundaries.not_covered are required",
                        "state what the page covers and what it deliberately leaves out", 2)
    old_links = (old_meta.get("links") or []) if old_meta else []
    merged_links = normalize_links(list(links) + list(old_links))
    meta = {
        "id": page_id,
        "title": title,
        "type": page_type,
        "summary": summary,
        "sources": sorted(available),
        "links": [link for link in merged_links if link != page_id],
        "evidence": evidence,
        "boundaries": boundaries,
    }
    return meta, body, evidence, boundaries
