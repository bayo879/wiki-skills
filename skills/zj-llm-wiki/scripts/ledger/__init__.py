"""账本入口：`Wiki` 组合根与对外再导出。

表契约住在 schema.py 的 `SCHEMA_SQL`；`Wiki.ensure` 打开连接后调用那里的建库函数，
命令代码从不自己建表。`Wiki` 由八个 mixin 组合：账本连接（core.py）、来源行
（sources.py）、原文读写（raws.py）、pages、view、任务行（task/store.py）、
排队（task/queue.py）与领取租约（task/lease.py）。

外部仍从本模块 import 配置、建库契约与视图路径，不用知道它们住在哪个文件。
没有迁移是有意为之：与当前版本不符的账本一律以 `E_STATE` 拒绝，由用户删掉
`.wiki/` 后重建。
"""

from __future__ import annotations

from ledger.core import LedgerMixin
from ledger.raws import RawMixin
from ledger.schema import (  # 再导出：外部 import 的入口仍是本模块
    CONFIG_NAME,
    DEFAULT_CONFIG,
    REQUIRED_COLUMNS,
    TABLES,
    load_config,
    publish_dir_name,
    view_dir,
)
from ledger.sources import SourceMixin
from pages import PagesMixin
from task.lease import TaskLeaseMixin
from task.queue import TaskQueueMixin
from task.store import TaskStoreMixin
from view import ViewMixin


class Wiki(LedgerMixin, SourceMixin, RawMixin, PagesMixin, ViewMixin,
           TaskStoreMixin, TaskQueueMixin, TaskLeaseMixin):
    """The knowledge base rooted at one directory."""
