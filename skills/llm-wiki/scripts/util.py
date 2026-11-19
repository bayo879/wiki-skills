"""跨模块共用的小工具：键归一、摘要、时间戳、元数据与原子写入。

单独一层是为了让 ingest、pages、view、ledger 与 cli 都能向下引用，
彼此之间不产生循环导入。这里只放工具，不承载业务流程。
"""

from __future__ import annotations

import hashlib
import os
import re
import tempfile
from datetime import datetime, timezone


PROVENANCE_MARKER = "<!-- llm-wiki:provenance -->"

PROVENANCE_HEADING = "## 原文来源"

PROVENANCE_ROW = re.compile(r"^- \[`([^`]+)`\]\(([^)\s]+)\)$")


def norm_key(path):
    """Repo-relative POSIX key, stable across platforms."""
    return path.as_posix()


def digest_bytes(data):
    return hashlib.blake2b(data, digest_size=16).hexdigest()


def digest_text(text):
    return digest_bytes(text.encode("utf-8", "replace"))


def body_digest(text):
    """摘要只取正文：观看区渲染时会去掉前导空行、尾随空白并统一换行。

    原文的身份是正文，不是文件字节。文件前言记的是挂载关系与展示属性，
    由账本字段决定，重写它（例如多挂一个同正文来源）不能算作原文被改过。
    """
    text = text.replace("\r\n", "\n").replace("\r", "\n")
    return digest_text(text.lstrip("\n").rstrip())


def split_provenance(body):
    """剥掉 raw 末尾由引擎生成的“原文来源”段，返回 (正文, 链接)。

    这段只服务观看区导航，不是原文内容；摘要在比较 raw 身份与手改时必须
    先剥掉它，否则给旧文件补链接会把整篇原文误判成已编辑。只有同时满足
    标记、标题与逐行链接格式才剥离；手写的其他内容原样保留。
    """
    lines = body.replace("\r\n", "\n").replace("\r", "\n").split("\n")
    start = None
    for index in range(len(lines) - 1, -1, -1):
        if lines[index].strip() == PROVENANCE_MARKER:
            start = index
            break
    if start is None:
        return body, []
    rest = lines[start + 1:]
    index = 0
    while index < len(rest) and not rest[index].strip():
        index += 1
    if index >= len(rest) or rest[index].strip() != PROVENANCE_HEADING:
        return body, []
    links = []
    for line in rest[index + 1:]:
        if not line.strip():
            continue
        match = PROVENANCE_ROW.match(line)
        if not match:
            return body, []
        links.append({"source": match.group(1).strip(), "target": match.group(2).strip()})
    return "\n".join(lines[:start]).rstrip(), links


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
