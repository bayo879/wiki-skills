#!/usr/bin/env python3
"""llm-wiki 的入口与导入面。

命令实现在 cli/ 里按命令族分文件，Wiki 类在 ledger/__init__.py 里由八个 mixin 组合。本模块保留
`python3 wiki.py <command>` 这个调用方式，并再导出 tests/selftest.py
在表一级检查用到的那几个名字；其他代码应直接 import 对应模块。
"""

from __future__ import annotations

import sys

sys.dont_write_bytecode = True  # never write __pycache__ into a global skill
from runtime import require_python

require_python()

from errors import WikiError
from frontmatter import split_frontmatter, yaml_scalar
from ingest import ingest_decision, raw_body_digest
from ledger import DEFAULT_CONFIG, Wiki, load_config, publish_dir_name
from pages import render_page, split_evidence, view_name
from util import digest_text

__all__ = [
    "DEFAULT_CONFIG",
    "Wiki",
    "WikiError",
    "digest_text",
    "ingest_decision",
    "load_config",
    "publish_dir_name",
    "raw_body_digest",
    "render_page",
    "split_evidence",
    "split_frontmatter",
    "view_name",
    "yaml_scalar",
]


if __name__ == "__main__":
    import cli

    sys.exit(cli.main())
