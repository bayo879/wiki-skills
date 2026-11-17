"""跨模块共用的小工具：键归一、摘要、时间戳、元数据与原子写入。

单独一层是为了让 ingest、pages、view、ledger 与 cli 都能向下引用，
彼此之间不产生循环导入。这里只放工具，不承载业务流程。
"""

from __future__ import annotations

import hashlib
import os
import tempfile
from datetime import datetime, timezone


def norm_key(path):
    """Repo-relative POSIX key, stable across platforms."""
    return path.as_posix()


def digest_bytes(data):
    return hashlib.blake2b(data, digest_size=16).hexdigest()


def digest_text(text):
    return digest_bytes(text.encode("utf-8", "replace"))


def same_stamp(size, mtime, known_size, known_mtime):
    """First-stage change filter: same byte size and (nearly) the same mtime.

    Hashing only happens when this returns False, so the recorded size/mtime
    pair is the whole cheap path for both the source file and its raw view.
    """
    try:
        return (
            int(size) == int(known_size)
            and abs(float(mtime) - float(known_mtime)) < 1e-6
        )
    except (TypeError, ValueError):
        return False


def now_iso():
    return datetime.now(timezone.utc).strftime("%Y-%m-%dT%H:%M:%SZ")


def parse_iso(value):
    return datetime.strptime(value, "%Y-%m-%dT%H:%M:%SZ").replace(tzinfo=timezone.utc)


def dump_meta(meta):
    """Frontmatter values as JSON-friendly types for command output."""
    clean = {}
    for key, value in meta.items():
        if key == "source_digests":
            continue
        if isinstance(value, list):
            clean[key] = [item if isinstance(item, (dict, list)) else str(item) for item in value]
        elif isinstance(value, dict):
            clean[key] = value
        else:
            clean[key] = str(value)
    return clean


def write_atomic(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    handle, temp_name = tempfile.mkstemp(dir=str(path.parent), prefix=".tmp-", suffix=path.suffix)
    try:
        with os.fdopen(handle, "w", encoding="utf-8") as stream:
            stream.write(text)
        os.replace(temp_name, path)
    except BaseException:
        try:
            os.unlink(temp_name)
        except OSError:
            pass
        raise
