"""`page import`/`page restore`/`page link` 与推断式补链。"""

from __future__ import annotations

import json
import sys

from cli.common import ARGS, emit, open_wiki
from errors import WikiError
from frontmatter import split_frontmatter
from pages import normalize_links
from validation import check_links
from workspace import inside_root


def command_page_import():
    wiki, _ = open_wiki()
    page_id = ARGS.id.strip().lower()
    record = wiki.page_record(page_id)
    if not record:
        raise WikiError("E_NOT_FOUND", f"no page {page_id}", "import only edits existing pages", 3)
    try:
        source = inside_root(
            wiki.root, ARGS.from_path, "page import source",
            "put the edited page under .wiki/drafts/",
        )
        text = source.read_text(encoding="utf-8")
    except OSError as error:
        raise WikiError("E_PAYLOAD", f"cannot read {ARGS.from_path}: {error}", "", 2)
    meta, body = split_frontmatter(text)
    merged = dict(record["meta"])
    for field in ("title", "type", "summary"):
        if meta.get(field):
            merged[field] = str(meta[field])
    if meta.get("sources"):
        merged["sources"] = [str(item) for item in meta["sources"]]
    if meta.get("links"):
        if not isinstance(meta["links"], list):
            raise WikiError(
                "E_PAYLOAD",
                "frontmatter links must be a block list of page ids",
                "write `links:` and then one `  - page-id` line per link",
                2,
            )
        merged["links"] = normalize_links(
            list(merged.get("links") or []) + [str(item) for item in meta["links"]]
        )
        check_links(page_id, merged["links"], wiki.page_ids(), "page import")
    for key in merged["sources"]:
        if not wiki.source_row(key):
            raise WikiError("E_UNKNOWN_SOURCE", f"page cites unregistered source {key}",
                            "run `ingest` first, or fix the source list", 2)
    if not body.strip():
        raise WikiError("E_PAYLOAD", "imported page has no body", "", 2)
    if merged.get("type") not in wiki.config["schema"]["types"]:
        raise WikiError("E_PAYLOAD", f"type {merged.get('type')!r} not in schema.types", "", 2)
    rev = wiki.write_rev(page_id, merged, body, "import")
    emit(
        "page import", wiki.root,
        f"imported {page_id} at rev {rev}",
        {"page": page_id, "rev": rev, "path": wiki.page_ref(wiki.page_record(page_id))},
        [f"page: {page_id} rev {rev}"],
    )
    wiki.close()
    return 0


def command_page_restore():
    wiki, _ = open_wiki()
    page_id = ARGS.id.strip().lower()
    row = wiki.conn.execute(
        "SELECT * FROM page_revs WHERE page = ? AND rev = ?", (page_id, ARGS.rev)
    ).fetchone()
    if not row:
        raise WikiError("E_NOT_FOUND", f"no rev {ARGS.rev} for page {page_id}",
                        "`query --page` shows the current revision", 3)
    meta = json.loads(row["meta"])
    rev = wiki.write_rev(page_id, meta, row["body"], "restore")
    emit(
        "page restore", wiki.root,
        f"restored {page_id} to rev {ARGS.rev} as rev {rev}",
        {"page": page_id, "rev": rev, "restored_from": ARGS.rev},
        [f"page: {page_id} rev {rev}"],
    )
    wiki.close()
    return 0


def command_page_link():
    """Add edges to existing pages; the ledger stays add-only."""
    if ARGS.repair and (ARGS.id or ARGS.targets):
        raise WikiError(
            "E_USAGE", "page link --repair takes no page id or targets",
            "run `page link --repair [--dry-run]`", 2,
        )
    wiki, _ = open_wiki(push=not ARGS.dry_run)
    if ARGS.repair:
        return repair_page_links(wiki)
    page_id = (ARGS.id or "").strip().lower()
    if not page_id:
        raise WikiError(
            "E_USAGE", "page link needs a page id",
            "example: page link refund-process refund-policy", 2,
        )
    record = wiki.page_record(page_id)
    if not record:
        raise WikiError(
            "E_NOT_FOUND", f"no page {page_id}",
            "create the page first; `page link` only edits existing pages", 3,
        )
    targets = [
        target for target in normalize_links(ARGS.targets) if target != page_id
    ]
    if not targets:
        raise WikiError(
            "E_USAGE", "page link needs at least one other page id",
            "example: page link refund-process refund-policy", 2,
        )
    check_links(page_id, targets, wiki.page_ids())
    existing = normalize_links(record["meta"].get("links") or [])
    added = [target for target in targets if target not in existing]
    if ARGS.dry_run:
        emit(
            "page link", wiki.root,
            f"would link {page_id} -> {', '.join(added or targets)}",
            {"mode": "single", "dry_run": True, "page": page_id,
             "rev": record["rev"], "added": added},
            [f"{page_id} -> {target}" for target in added],
        )
        wiki.close()
        return 0
    if not added:
        emit(
            "page link", wiki.root,
            f"{page_id} already links {', '.join(targets)}",
            {"mode": "single", "dry_run": False, "page": page_id,
             "rev": record["rev"], "links": existing, "added": []},
            [],
        )
        wiki.close()
        return 0
    meta = dict(record["meta"])
    meta["links"] = normalize_links(existing + targets)
    rev = wiki.write_rev(page_id, meta, record["body"], "link")
    # The command exists to repair the graph, so publish the new section now
    # instead of waiting for the next command that opens the ledger.
    view_events = wiki.sync_view()
    for line in view_events:
        if line.startswith("warning: "):
            sys.stderr.write(line + "\n")
    emit(
        "page link", wiki.root,
        f"linked {page_id} -> {', '.join(added)} at rev {rev}",
        {"mode": "single", "dry_run": False, "page": page_id, "rev": rev,
         "links": meta["links"], "added": added, "view": view_events},
        [f"{page_id} -> {target}" for target in added],
    )
    wiki.close()
    return 0


def repair_page_links(wiki):
    """Add missing edges between pages that cite the same raw source."""
    groups = {}
    for record in wiki.all_pages():
        for source in record["meta"].get("sources") or []:
            row = wiki.source_row(str(source))
            key = row["raw"] if row and row["raw"] else str(source)
            groups.setdefault(key, []).append(record["id"])
    proposed = set()
    for pages in groups.values():
        pages = sorted(set(pages))
        for index, left in enumerate(pages):
            for right in pages[index + 1:]:
                proposed.add((left, right))
    existing = {
        (row["src"], row["dst"])
        for row in wiki.conn.execute("SELECT src, dst FROM links")
    }
    missing = sorted(
        (left, right) for left, right in proposed
        if (left, right) not in existing and (right, left) not in existing
    )
    by_page = {}
    for left, right in missing:
        by_page.setdefault(left, []).append(right)
    planned = []
    for page_id in sorted(by_page):
        record = wiki.page_record(page_id)
        planned.append({
            "page": page_id,
            "rev": record["rev"],
            "added": normalize_links(by_page[page_id]),
        })
    lines = [
        f"{item['page']} -> {', '.join(item['added'])}" for item in planned
    ]
    base = {"mode": "repair", "rules": ["shared-source"], "edges": len(missing)}
    if ARGS.dry_run:
        data = dict(base, dry_run=True, pages=planned)
        emit(
            "page link", wiki.root,
            f"link repair would add {len(missing)} edges across {len(planned)} pages",
            data, lines,
        )
        wiki.close()
        return 0
    applied = []
    for item in planned:
        page_id = item["page"]
        record = wiki.page_record(page_id)
        meta = dict(record["meta"])
        existing_links = normalize_links(meta.get("links") or [])
        meta["links"] = normalize_links(existing_links + item["added"])
        rev = wiki.write_rev(page_id, meta, record["body"], "link")
        applied.append({"page": page_id, "rev": rev, "added": item["added"]})
    if not applied:
        data = dict(base, dry_run=False, pages=[], view=[])
        emit("page link", wiki.root, "no missing shared-source edges", data, [])
        wiki.close()
        return 0
    view_events = wiki.sync_view()
    for line in view_events:
        if line.startswith("warning: "):
            sys.stderr.write(line + "\n")
    data = dict(base, dry_run=False, pages=applied, view=view_events)
    emit(
        "page link", wiki.root,
        f"link repair added {len(missing)} edges across {len(applied)} pages",
        data, lines,
    )
    wiki.close()
    return 0
