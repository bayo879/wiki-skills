# `wiki.py` 超过 2000 行，需要按职责拆分

## 问题

`skills/llm-wiki/scripts/wiki.py` 在 2026-11-16 是 2715 行（`wc -l` 输出），超过仓库
`AGENTS.md` 的单文件 2000 行警戒；本次发布/观看区改动开始时（`60c1e5b`）它已经是 1937
行，之后继续增长。文件同时承担：配置与 schema、工作区与扫描、文本解析、来源与修订、镜像、
观看区推送、任务状态机、以及全部命令层。

证据：

- 类 `Wiki` 从 558 行到 1544 行，约 990 行，混了连接、建表、来源、修订、镜像、观看区与
  任务。
- `command_*` 从 `command_check`（1618）到 `command_version`（2600），约 980 行，参数解析、
  业务编排与输出组装交织。
- 顶部已拆出 `errors.py`、`frontmatter.py`、`search.py`、`scenarios.py`、`topics.py`、
  `runtime.py`、`health.py`、`workspace.py`、`tasks.py`、`validation.py`，但核心仍在
  `wiki.py`。
- `docs/agent/engine.md` 的行数声明本次刚改为 2715，说明体量已经成为需要持续维护的
  文档事实，而不是一次性偏差。

## 影响

- 新增字段或命令要在 2700 行里找位置，观看区、镜像和任务逻辑容易继续互相依赖。
- 评审时难以把「账本改动」与「视图渲染改动」分开看，回归范围说不清。
- 测试只有整体端到端入口 `tests/selftest.py`，拆分前无法按模块单独验证。
- 单文件越大，`docs/agent/engine.md` 的布局表越容易过时；本次已经出现过期行数。

当前没有功能缺陷；本问题只关乎可维护性与评审成本。

## 候选方案

未定案。可选：按职责拆成 5 个模块 + 1 个 CLI 入口，`wiki.py` 保留为入口与兼容导入面。
先做「只搬不改」的机械拆分：命令输出、错误码、schema 与文件布局都不变。

拆分边界（行号为 2026-11-16 的 `wiki.py`）：

| 模块 | 职责 | 现有位置/符号 |
| --- | --- | --- |
| `ledger.py` | 连接、建表、配置加载、扫描根、来源行、`Wiki` 基础方法 | `Wiki.__init__`/`ensure`/`ensure_publish`/`_create_schema`/`ensure_columns`/`close`/`scan_files` 与来源方法（558–856） |
| `ingest.py` | 字节读取、文本抽取、摄入判定与 `ingest` 命令 | `read_bytes`…`extract_text`（197–280）、`ingest_decision`（171）、`command_ingest`（1656–1850） |
| `pages.py` | 页面记录、修订写入、渲染、链接 | `page_record`、`page_ref`、`view_relpath`/`view_text`/`write_rev`、`rebuild_links`（857–1026，镜像与 drift 已于 2026-11-17 删除） |
| `task_store.py` | 任务入队、快照、漂移、领取、过期、关闭 | `enqueue_task`、`attach_page_source`、`task_drift`、`pin_open_task_raw`、`claim_tasks`、`release_task`、`requeue_task`、`expire_task`、`close_task`、`open_task_for_page`、`source_in_open_task`（1262–1544） |
| `view.py` | 观看区命名、推送、主题/场景投放、`sync` | `view_name`、`render_view_readme`、`render_page`、`sync_view_file`、`sync_view`、`_sync_view_pages`、`_seed_view_topics`、`_seed_view_scenarios`、`command_sync`（283–466、1136–1261、2439–2520） |
| `cli.py` | `emit`/`open_wiki`、参数解析、其余命令 | `emit`（1546 起）、`open_wiki`（1585–1611）、`require_health`/`command_check`（1614–1632）、`build_parser`/`main`、`command_read`/`query`/`status`/`export`/`doctor`/`scenario`/`page` |

拆分缝：

- `Wiki` 是唯一持有连接的对象；`pages.py`、`task_store.py`、`view.py` 通过 `Wiki` 实例
  或纯函数接收依赖，不各自打开数据库。
- `render_page(meta, body, aliases)` 保持纯函数；`view.py` 只依赖账本对象与文件系统。
- `task_store.py` 不导入命令层；命令层只组装参数、调用存储与渲染、输出信封。
- 模块间只走显式参数、返回值与 `Wiki` 属性，不引入全局单例或循环导入。
- 拆分顺序建议：先 `pages.py` 与 `view.py`（渲染边界最清楚），再 `task_store.py`，最后
  拆 `cli.py` 与 `ledger.py` 的剩余部分。

## 待确认与验收条件

- [ ] 确认按上表拆分，并确认 `wiki.py` 的兼容导入面（是否保留 `from wiki import Wiki`）。
- [ ] 每一步只搬代码、不改行为；每一步后 `python3 tests/selftest.py` 全绿（当前 175/175）。
- [ ] 拆完后单文件都不超过 2000 行，`docs/agent/engine.md` 的行数与布局表同步更新。
- [ ] 命令输出、错误码、`docs/readme/cli.md` 的契约与 `.wiki/` 文件布局保持不变。
- [ ] 若拆分中发现职责交叉（例如观看区读取修订的私有方法），先记录再动，不顺手改契约。
