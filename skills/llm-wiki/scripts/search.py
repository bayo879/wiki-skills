"""Ranked page search used by `query`.

The scoring is intentionally simple: count query-term hits in title, summary and
body, then weight title hits 5x and summary hits 3x. `query` combines these page
results with workflow matches, so the page half lives here and stays free of
command-line state.
"""

from __future__ import annotations

import re


def search_pages(wiki, terms, limit):
    """Return (all scored page records, the top `limit` records)."""
    scored = []
    for record in wiki.all_pages():
        meta = record["meta"]
        title = str(meta.get("title", ""))
        summary = str(meta.get("summary", ""))
        body = record["body"]
        haystack = f"{title}\n{summary}\n{body}"
        hits = sum(haystack.lower().count(term.lower()) for term in terms)
        if hits == 0:
            continue
        score = hits + 5 * sum(title.lower().count(term.lower()) for term in terms)
        score += 3 * sum(summary.lower().count(term.lower()) for term in terms)
        scored.append(
            {
                "id": record["id"],
                "title": title,
                "type": meta.get("type"),
                "status": meta.get("status"),
                "summary": summary,
                "sources": meta.get("sources", []),
                "rev": record["rev"],
                "score": score,
                "snippet": snippet(body, terms[0]),
            }
        )
    scored.sort(key=rank_key)
    return scored, scored[:max(1, limit)]


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
