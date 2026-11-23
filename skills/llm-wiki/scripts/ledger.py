"""账本入口：`Wiki` 组合根与对外再导出。

表契约住在 ledger_schema.SCHEMA_SQL；`Wiki.ensure` 打开连接后调用那里的建库函数，
命令代码从不自己建表。`Wiki` 由八个 mixin 组合：账本连接（ledger_core）、来源行
（ledger_sources）、原文读写（ledger_raws）、pages、view、任务行（task_store）、
排队（task_queue）与领取租约（task_lease）。

外部仍从本模块 import 配置、建库契约与视图路径，不用知道它们住在哪个文件。
没有迁移是有意为之：与当前版本不符的账本一律以 `E_STATE` 拒绝，由用户删掉
`.wiki/` 后重建。
"""

from __future__ import annotations

from ledger_core import LedgerMixin
from ledger_raws import RawMixin
from ledger_schema import (  # 再导出：外部 import 的入口仍是本模块
    CONFIG_NAME,
    DEFAULT_CONFIG,
    REQUIRED_COLUMNS,
    TABLES,
    load_config,
    publish_dir_name,
    view_dir,
)
from ledger_sources import SourceMixin
from pages import PagesMixin
from task_lease import TaskLeaseMixin
from task_queue import TaskQueueMixin
from task_store import TaskStoreMixin
from view import ViewMixin


class Wiki(LedgerMixin, SourceMixin, RawMixin, PagesMixin, ViewMixin,
           TaskStoreMixin, TaskQueueMixin, TaskLeaseMixin):
    """The knowledge base rooted at one directory."""
