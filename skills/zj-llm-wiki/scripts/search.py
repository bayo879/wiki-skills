"""Ranked page search used by `query`.

The scoring is intentionally simple: count query-term hits in title, summary and
body, then weight title hits 5x and summary hits 3x. `query` pairs these page
results with topic and workflow candidates, so the page half lives here and stays
free of command-line state.

Scoring reads the latest revision rows directly instead of going through
`all_pages`: that call resolves links and status per page, which costs several
SQL statements per page and is only needed for the handful of results we show.
"""

from __future__ import annotations

import json
import re


def _latest_rows(wiki):
    """Latest revision of every page, without the per-page links/status lookups."""
    return wiki.conn.execute(
        "SELECT r.page, r.rev, r.meta, r.body FROM page_revs r "
        "WHERE r.rev = (SELECT MAX(x.rev) FROM page_revs x WHERE x.page = r.page) "
        "ORDER BY r.page"
    ).fetchall()


def _measure(title_low, summary_low, haystack_low, term):
    """Return (hits, score) for one lowered term against one lowered page."""
    hits = haystack_low.count(term)
    if hits == 0:
        return 0, 0
    return hits, hits + 5 * title_low.count(term) + 3 * summary_low.count(term)


def _result(wiki, page_id, title, summary, body, score, term):
    """Build one result record, resolving links and status only for shown pages."""
    record = wiki.page_record(page_id)
    meta = record["meta"]
    return {
        "id": page_id,
        "title": title,
        "type": meta.get("type"),
        "status": meta.get("status"),
        "summary": summary,
        "sources": meta.get("sources", []),
        "rev": record["rev"],
        "score": score,
        "snippet": snippet(body, term),
    }


def search_pages(wiki, terms, limit):
    """Return (all matching pages as {id, score}, the top `limit` records)."""
    lowered = [term.lower() for term in terms]
    matched = []
    for row in _latest_rows(wiki):
        meta = json.loads(row["meta"])
        title = str(meta.get("title", ""))
        summary = str(meta.get("summary", ""))
        body = row["body"]
        haystack_low = f"{title}\n{summary}\n{body}".lower()
        title_low = title.lower()
        summary_low = summary.lower()
        hits = 0
        score = 0
        for term in lowered:
            term_hits, term_score = _measure(
                title_low, summary_low, haystack_low, term)
            hits += term_hits
            score += term_score
        if hits == 0:
            continue
        matched.append((score, row["page"], title, summary, body))
    matched.sort(key=lambda item: (-item[0], item[1]))
    results = [
        _result(wiki, item[1], item[2], item[3], item[4], item[0], terms[0])
        for item in matched[:max(1, limit)]
    ]
    return [{"id": item[1], "score": item[0]} for item in matched], results


def search_pages_grouped(wiki, words, limit):
    """Return [(match_count, top records)] aligned with `words`.

    Each word keeps its own ranking, which is why `query --task` never pools
    words; one pass over the ledger avoids rescanning every page per word.
    """
    lowered = [word.lower() for word in words]
    limit = max(1, limit)
    counts = [0] * len(words)
    buckets = [[] for _ in words]
    for row in _latest_rows(wiki):
        meta = json.loads(row["meta"])
        title = str(meta.get("title", ""))
        summary = str(meta.get("summary", ""))
        body = row["body"]
        haystack_low = f"{title}\n{summary}\n{body}".lower()
        title_low = title.lower()
        summary_low = summary.lower()
        for index, term in enumerate(lowered):
            hits, score = _measure(title_low, summary_low, haystack_low, term)
            if hits == 0:
                continue
            counts[index] += 1
            bucket = buckets[index]
            bucket.append((score, row["page"], title, summary, body))
            bucket.sort(key=lambda item: (-item[0], item[1]))
            del bucket[limit:]
    grouped = []
    for index, word in enumerate(words):
        results = [
            _result(wiki, item[1], item[2], item[3], item[4], item[0], word)
            for item in buckets[index]
        ]
        grouped.append((counts[index], results))
    return grouped


def rank_key(item):
    return (-item["score"], item["id"])


def snippet(body, term):
    flat = re.sub(r"\s+", " ", body)
    index = flat.lower().find(term.lower())
    if index < 0:
        return flat[:120]
    start = max(0, index - 60)
    end = min(len(flat), index + len(term) + 90)
    prefix = "..." if start > 0 else ""
    suffix = "..." if end < len(flat) else ""
    return prefix + flat[start:end] + suffix
