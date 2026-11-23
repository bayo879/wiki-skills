# 引擎内部

对象：`skills/llm-wiki/scripts/`。`wiki.py` 只是入口（命令行转发与兼容再导出）。成族的模块收在
三个目录里：`cli/`（`__init__.py` 是入口与再导出，`common.py` 是共用层，其余按命令族分）、
`ledger/`（`__init__.py` 是组合根与再导出，另有 `schema.py`、`core.py`、`sources.py`、`raws.py`）、
`task/`（`store.py`、`queue.py`、`lease.py`）；其余按职责分在 `pages.py`、`view.py`、
`ingest.py`、`util.py` 等根级文件；单文件都不超过 1000 行 / 2 万字符。公共错误在 `errors.py`，
frontmatter 解析在 `frontmatter.py`，页面检索在 `search.py`，可执行工作流在
`workflows.py` 与 `workflow_steps.py`，话题沉淀在 `topics.py`，运行时基线在 `runtime.py`，安装自检在 `health.py`，
工作区边界在 `workspace.py`，任务信息面在 `tasks.py`，页面载荷校验在 `validation.py`。
本文件只记录改代码时容易踩的内部约定，命令与字段以 `docs/readme/cli.md` 为准。

## 布局

| 区段 | 位置 | 说明 |
| --- | --- | --- |
| 入口与兼容面 | `wiki.py` | 调 `runtime.require_python()`、`__main__` 转发到 `cli.main()`；再导出 `tests/selftest.py` 用到的函数，不承载逻辑 |
| 命令层 | `cli/` 的 `__init__.py`、`ingest.py`、`submit.py`、`workspace.py`、`query.py`、`page.py`、`workflow.py` | 各文件装一族的 `command_*`；只做参数解析、调用 `Wiki`、组装输出。`__init__.py` 只留 `main`、`COMMANDS`、`build_parser` 与再导出；跨文件共享的只有 `common.py` |
| 命令共用层 | `cli/common.py` 的 `ARGS`、`emit`、`fail`、`open_wiki`、`require_install` | `ARGS` 是一个共享的 `Namespace` 对象，`main` 只更新它的属性，命令模块 import 到的是同一个对象 |
| 常量与配置 | `ledger/schema.py` 的目录常量、`DEFAULT_CONFIG`、`load_config`（`ledger/__init__.py` 再导出） | `TASK_LEASE_SECONDS=900` 在 `task/store.py`；正文/摘要上限在 `validation.py` |
| 错误与输出 | `errors.py` 的 `WikiError`；`emit`、`fail` 在 `cli/common.py` | 错误码 + `next` + 退出码；所有命令走同一信封 |
| 工具 | `util.py` | `norm_key`、`digest_bytes`/`digest_text`、`same_stamp`、`now_iso`/`parse_iso`、`dump_meta`、`write_atomic` |
| 工作区边界 | `workspace.py` | `WIKI_DIR`、`window_safe`、`inside_root`、`dry_store`；文件参数一律先过 `inside_root` |
| 摄入与解析 | `ingest.py` | `extract_text`/`pdf_to_text`/`docx_to_text`/`html_to_text`、`decode_bytes`、`raw_facts`、`raw_body_digest`、`ingest_decision`；纯函数，不碰数据库；超限在 `read_bytes` 抛 |
| frontmatter | `frontmatter.py` | `split_frontmatter`、`unquote_scalar`、`yaml_scalar`；页面渲染与工作流加载共用一套语法 |
| 页面层 | `pages.py` 的 `PagesMixin` | `page_record`、`write_rev`、`rebuild_links`、`links_for`、`view_text`、`page_ids` |
| 页面渲染 | `pages.py` 的 `render_page` | 依赖 `frontmatter.yaml_scalar`；`view_path`/`raw_of` 只在写观看区时传入，证据行链到 raw、raw 末尾追加原文来源段；改动一侧要同步另一侧 |
| 页面检索 | `search.py` | `search_pages`、`rank_key`、`snippet`；只读 `Wiki`，不碰命令状态 |
| 观看区与发布 | `view.py` 的 `sync_view`、`sync_view_file`、`render_view_readme`；`command_sync` 在 `cli/workspace.py` | 账本 → `<view>/pages`；`raw/` 由摄入维护；`workflows/`、`topics/`、`runs/` 是项目内容，引擎不覆盖、不删除；手改只告警，`sync` 回收 |
| 任务行 | `task/store.py` 的 `TaskStoreMixin` | 任务行、任务目录与 payload、查询预算与视图面；任务常量（`TASK_OPEN_STATES`、`TASK_LEASE_SECONDS`、`QUERY_*`）住在这个文件，其他任务模块从这里取 |
| 任务排队 | `task/queue.py` 的 `TaskQueueMixin` | 入队、来源归属、链接预填、来源漂移判定与原文固定 |
| 任务领取与租约 | `task/lease.py` 的 `TaskLeaseMixin` | 领取、租约归属（`task_leases`、`sweep_leases`、`lease_owner`）、树与批的排序、释放/过期/关闭；并发走同一条 CAS 约定 |
| 任务信息面 | `tasks.py` | `brief`（做什么/怎么做/提交后的影响）、`template`（固定骨架）、章节骨架；不读写数据库 |
| 工作流能力 | `workflows.py` | `scan`/`inventory`/`match`/`load` 与条目组装（`_entry`/`_public`/`_resources`/`_tags`）；先扫 `<view>/workflows/`，再扫旧根 `workflows/` 兜底，最后合并技能内置工作流；条目带 `origin`（`view`/`legacy`/`builtin`）与 `legacy_conflict`；只读文件，不写 `.wiki/`，不生成任务 |
| 步骤契约 | `workflow_steps.py` | `parse_steps`/`_contract`/`_applies` 解析 `## 步骤`、`## 适用` 与四类标记判定；纯文本函数，不碰文件与账本；`UNTYPED_ALLOWED` 也在这里 |
| 话题沉淀 | `topics.py` | `scan`/`match`/`rank`/`parse_refs`（`match` 是 `rank(scan(...))` 的薄包装）；先扫 `<view>/topics/*.md`，再扫旧根 `topics/` 兜底；条目带 `origin`；只读文件，不写 `.wiki/`、不排任务；检索打分同 `search.py` |
| 页面校验 | `validation.py` | 页载荷字段、章节、evidence 逐字校验、来源并集与旧证据保留并复核；纯函数，只读 `Wiki` |
| 账本结构 | `ledger/schema.py` | `SCHEMA_SQL`、`create_schema`、`ensure_columns`、`ensure_open_page_index`；表与索引契约只在这里定义，`Wiki.ensure` 调用。叶子模块，不 import 任何 mixin |
| 账本连接 | `ledger/core.py` 的 `LedgerMixin` | `__init__`/`ensure`/`close`、发布目录创建、推送入口 |
| 来源行 | `ledger/sources.py` 的 `SourceMixin` | `scan_files`、来源快照、`upsert_source`/`set_source_status`、`source_body` |
| 原文（raw） | `ledger/raws.py` 的 `RawMixin` | raw 命名、挂载、手改采纳、脚注刷新与孤儿回收 |
| 账本组合根 | `ledger/__init__.py` | 只做组合与再导出：`Wiki` 由 `LedgerMixin`、`SourceMixin`、`RawMixin`、`PagesMixin`、`ViewMixin`、`TaskStoreMixin`、`TaskQueueMixin`、`TaskLeaseMixin` 组合，方法名与签名不变，外部只认 `Wiki` |
| 安装自检 | `health.py` | 解释器、技能文件与模板、工作区可写、账本可读；`check` 只读深检，命令入口只跑安装级轻检（`require_install`） |

## 表

- `raws(id, name, path, parser, size, mtime, updated_at)`：`id` 是正文 sha256，一份正文一行
- `sources(key, path, digest, size, mtime, raw_digest, raw_size, raw_mtime, raw, parser,
  status, updated_at, raw_id, last_parsed_digest)`
- `links(src, dst)`
- `tasks(id, kind, subject, state, epoch, inputs, payload, last_error, attempts,
  lease_until, created_at, closed_at)`
- `task_sources(task_id, source_key)`
- `page_revs(page, rev, meta, body, digest, render_digest, view_digest, view_path, origin,
  accepted_at)`

表结构每版固定，不写迁移：`ledger.ensure_columns` 遇到有数据的旧结构直接 `E_STATE` 拒绝；
旧表全空时按当前 `SCHEMA_SQL` 重建那几张表（没有内容可保，不算迁移）。

## 不变量

存储、页面、raw、任务与并发的存储不变量单列一页：[引擎不变量](engine-invariants.md)。
改账本、页面、raw、任务或锁之前先读那一页，`第 N 条` 的编号都指那里。

## 锁的分层现状（2026-11-20）

普通 `query`（文本检索、`--page`）已按只读快照落地：`Wiki.open_readonly` 用
`mode=ro` + `query_only` 打开账本，不执行 `ensure(push=True)`/`sync_view`，不创建
`.wiki/lock`，也不创建 `.wiki/`；`.wiki/wiki.db` 不存在或 schema 缺列时报 `E_STATE`。
它不进入第 18 条的整命令工作区锁；`query --task` 仍因写 `queries.jsonl` 保留锁。

更长期的方向尚未实现：任务租约带进程身份、心跳与 epoch fencing，互斥粒度缩到任务树/
共享资源，观看区推送收敛为单写者。第 18 条仍是写命令的实际行为，决策见
`docs/agent/development-decisions.md`。

## 数据方向

```text
源文件 → scan_files → extract_text → <view>/raw/<key>-<digest>.md（含原文来源段）+ sources 行
                                        ↓ enqueue route 任务
route 载荷 → 校验 → 页面任务（new/merge）或来源结算（skip）
page 载荷 / page link / page import / page link --repair → 校验 → page_revs + links → 推送
→ <view>/pages 副本

<view>/workflows/<id>/WORKFLOW.md ──query/scan──→ 工作流命中（只读，不落盘）
                                        └─workflow load→ 全文 + 分类型步骤 + resources
<root>/workflows/<id>/WORKFLOW.md ──legacy 兜底──┘

<view>/topics/*.md ──query/scan──→ topics 命中（只读，不落盘）
<root>/topics/*.md ──legacy 兜底──┘
```

`raw` 文件名取来源 basename，同名按 `-2`、`-3` 顺延，不跟正文摘要走；`sources.raw` 存相对
路径。一份正文一行 `raws`：手改按整份正文采纳，挂载它的 `sources` 一起换到新摘要，文件留在
原路径。raw 末尾的 `## 原文来源` 段是渲染层生成的导航，不是原文：`split_provenance` 在
`body_digest`/`raw_body_digest` 前剥掉它，所以补链接、换挂载路径都不会让引用页 stale；
`ingest` 末尾的 `refresh_raw_provenance` 只给缺段的旧 raw 补一次，已有段的文件不重排。

`links` 是 `page_revs.meta` 里的页面 id 数组，并镜像到 `links` 表；`write_rev` 按 meta 重建，
不再从 body 的 `[[...]]` 或 `## 关联` 段解析。观看区由 `links_for` 取出链与入链，渲染成一个
`## 关联` 段，每行是 `[目标页标题](相对路径)`（账本只存 id，标题和路径由 `render_page`
的 `page_of`/`view_path` 现算）；正文里的该段在提交与推送时剥掉。`page link` 与
`page import` 的 `links` 走同一套 `check_links`/`write_rev`；`page link` 成功后立即
`sync_view`，不等下一个命令。

`page link --repair` 是唯一的推断式补链：把 `meta.sources` 经 `sources.raw` 归并到同一条
原始正文，同组页两两配对，
已有任一方向就跳过，每对只加小 id → 大 id 一条；`--dry-run` 不写修订。它只补缺边，不改变
已有方向，也不把正文 `[[...]]` 变成第二套边。

`sync` 回收观看区时先剥掉关系段，再解析末尾证据段：关系段渲染在证据之后，若先解析证据，
带关联的页面会把证据块当正文收进来、下次推送再追加一份。重复的证据段也会在回收时一并
剥掉，只保留最后一份。证据行兼容旧式 ``- `source`：quote`` 和新式
``- [`raw`](target)（`source`）：quote``；后者从括号里的 code span 取 source key，链接本身
不参与账本。

观看区 `pages/` 是账本的唯一页面落盘副本（按标题命名，`view_digest` 记录上次推送的
字节）；手改过的副本不覆盖，`sync` 收成新修订后才恢复推送。`sync` 只认
`<view>/pages/**/*.md`。

`next` 只输出 `{id, brief, template, work}`；`epoch`、`lease_until`、`attempts` 留在账本里。
`submit` 的 `--epoch` 只是兼容旧脚本的可选项，正常路径不要求。任务包由 `tasks.py` 生成，
改任务设计时只改这一处，不要在 `SKILL.md` 里补流程说明。

## 改动检查表

- 新增或拆分 `scripts/` 模块：同步本文件布局表与 `health.py` 的 `SCRIPT_FILES`；
  `wiki.py` 只保留入口与兼容再导出。
- 改字段或校验：同步 `docs/readme/cli.md`、`skills/llm-wiki/scripts/tasks.py`、
  `skills/llm-wiki/scripts/validation.py`、`skills/llm-wiki/scripts/health.py`、`SKILL.md`、
  `tests/selftest.py`。
- 改工作流：同步 `docs/readme/cli.md`、`docs/readme/workflows.md`、`SKILL.md`、
  `skills/llm-wiki/scripts/health.py` 的 `SCRIPT_FILES` 与 `BUILTIN_WORKFLOWS`、
  `tests/selftest.py`；保持 query 只返回候选、显式 `workflow search` 与 query 共用 `match()`、
  `load` 只读、内置可覆盖。
  改进步骤契约时同时改 `workflow_steps.py` 的 `parse_steps`、`_contract`、selftest 与
  `workflows.md` 的判定表。
- 改话题：同步 `docs/readme/cli.md`、`docs/readme/topics.md`、`SKILL.md`、
  `skills/llm-wiki/workflows/topic-exploration/WORKFLOW.md`、`scripts/health.py` 的
  `SCRIPT_FILES`、`tests/selftest.py`；保持 `ingest` 跳过 `topics/`、`query` 只带话题。
- 改观看区或发布：同步 `docs/readme/cli.md`、`README.md`、
  `docs/readme/design-principles.md`、`SKILL.md`、`tests/selftest.py`；保持 `sync` 只回收、
  推送只发生在命令入口，且观看区目录名始终跳过扫描。
- 改表结构：同步 `ledger.REQUIRED_COLUMNS`、`ledger.TABLES` 与本文件的表清单；有数据的旧库
  由 `ledger.ensure_columns` 直接 `E_STATE` 拒绝，全空的旧表按新结构重建，不写迁移。
- 改渲染：`render_page` 与 `split_frontmatter` 必须能往返；跑 `tests/selftest.py` 的
  import/restore 用例。
- 改任务：并发前提是单写者；不要为了"更快"去掉提交前的状态与租约检查。
