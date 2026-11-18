# 引擎内部

对象：`skills/llm-wiki/scripts/`。`wiki.py` 只是入口（命令行转发与兼容再导出），命令层在
`cli.py`，账本表结构与 `Wiki` 组合根在 `ledger.py`，其余按职责分在 `pages.py`、`view.py`、
`task_store.py`、`ingest.py`、`util.py`；单文件都不超过 2000 行。公共错误在 `errors.py`，
frontmatter 解析在 `frontmatter.py`，页面检索在 `search.py`，可执行工作流在
`workflows.py`，话题沉淀在 `topics.py`，运行时基线在 `runtime.py`，安装自检在 `health.py`，
工作区边界在 `workspace.py`，任务信息面在 `tasks.py`，页面载荷校验在 `validation.py`。
本文件只记录改代码时容易踩的内部约定，命令与字段以 `docs/readme/cli.md` 为准。

## 布局

| 区段 | 位置 | 说明 |
| --- | --- | --- |
| 入口与兼容面 | `wiki.py` | 调 `runtime.require_python()`、`__main__` 转发到 `cli.main()`；再导出 `tests/selftest.py` 用到的函数，不承载逻辑 |
| 命令层 | `cli.py` 的 `command_*`、`submit_route`、`submit_page` | 只做参数解析、调用 `Wiki`、组装输出；`main`、`COMMANDS`、`build_parser` 也在这里 |
| 常量与配置 | `ledger.py` 的目录常量、`DEFAULT_CONFIG`、`load_config` | `TASK_LEASE_SECONDS=900` 在 `task_store.py`；正文/摘要上限在 `validation.py` |
| 错误与输出 | `errors.py` 的 `WikiError`；`emit`、`fail` 在 `cli.py` | 错误码 + `next` + 退出码；所有命令走同一信封 |
| 工具 | `util.py` | `norm_key`、`digest_bytes`/`digest_text`、`same_stamp`、`now_iso`/`parse_iso`、`dump_meta`、`write_atomic` |
| 工作区边界 | `workspace.py` | `WIKI_DIR`、`window_safe`、`inside_root`、`dry_store`；文件参数一律先过 `inside_root` |
| 摄入与解析 | `ingest.py` | `extract_text`/`pdf_to_text`/`docx_to_text`/`html_to_text`、`decode_bytes`、`raw_facts`、`raw_body_digest`、`ingest_decision`；纯函数，不碰数据库；超限在 `read_bytes` 抛 |
| frontmatter | `frontmatter.py` | `split_frontmatter`、`unquote_scalar`、`yaml_scalar`；页面渲染与工作流加载共用一套语法 |
| 页面层 | `pages.py` 的 `PagesMixin` | `page_record`、`write_rev`、`rebuild_links`、`links_for`、`view_text`、`page_ids` |
| 页面渲染 | `pages.py` 的 `render_page` | 依赖 `frontmatter.yaml_scalar`；改动一侧要同步另一侧 |
| 页面检索 | `search.py` | `search_pages`、`rank_key`、`snippet`；只读 `Wiki`，不碰命令状态 |
| 观看区与发布 | `view.py` 的 `sync_view`、`sync_view_file`、`render_view_readme`；`command_sync` 在 `cli.py` | 账本 → `<view>/pages`；`raw/` 由摄入维护；`topics/`、`workflows/` 只首次投放；手改只告警，`sync` 回收 |
| 任务层 | `task_store.py` 的 `TaskStoreMixin` | 任务行、目录、领取/释放/过期；`TASK_OPEN_STATES`、`TASK_LEASE_SECONDS`；租约归属（`task_leases`、`sweep_leases`、`lease_owner`）；并发走同一条 CAS 约定 |
| 任务信息面 | `tasks.py` | `brief`（做什么/怎么做/提交后的影响）、`template`（固定骨架）、章节骨架；不读写数据库 |
| 工作流能力 | `workflows.py` | `scan`/`inventory`/`match`/`load`；只扫 `workflows/`，再与技能内置工作流合并；`parse_steps`/`_contract` 解析四类步骤与判定；只读文件，不写 `.wiki/`，不生成任务 |
| 话题沉淀 | `topics.py` | `scan`/`match`；只扫 `话题/*.md`，只读文件，不写 `.wiki/`、不排任务；检索打分同 `search.py` |
| 页面校验 | `validation.py` | 页载荷字段、章节、evidence 逐字校验、来源并集与旧证据保留并复核；纯函数，只读 `Wiki` |
| 账本结构 | `ledger.py` | `SCHEMA_SQL`、`create_schema`、`ensure_columns`、`ensure_open_page_index`；表与索引契约只在这里定义，`Wiki.ensure` 调用 |
| 账本 | `ledger.py` 的 `LedgerMixin` | `__init__`/`ensure`/`close`、来源与 raw、推送入口；`Wiki` 在文件末尾由四个 mixin 组合 |
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

1. **账本是唯一事实源。** 页面读取一律走 `latest_rev_row`；`page_record` 返回
   `{id, rev, meta, body, status}`。
2. **`computed_status` 每次重算。** `meta.source_digests` 与当前 `sources.digest` 不一致，
   或来源 `missing/unsupported/oversize`，页面即 `stale`；不把 status 存成可漂移的字段。
3. **修订只增不改。** `write_rev` 取 `MAX(rev)+1`，写 `digest`（元数据+正文的规范化哈希）
   与 `render_digest`（渲染文本哈希），再重建链接；页面文件只由观看区推送写出。关系边
   同样只增不删，`page link`、`page import` 与提交共用这条存储不变量。
4. **账本不写页面副本。** `.wiki/` 只放账本、任务目录与草稿（原文在 `<view>/raw/`）；页面正文的唯一
   落盘副本在 `<view>/pages/`，由 `sync_view` 按账本推送，`render_digest` 只是渲染文本的
   历史哈希，不再用于比对文件。
5. **任务 epoch 单调。** `claim_tasks` 先回收过期 lease，领取时 `epoch = epoch + 1`；
   不重发仍在租约里的任务：有 `leased` 行时 `next` 先 `sweep_leases` 收掉过期租约，再把租约
   按 `owner` 分成自己的和别人的——自己的一律报 `E_TASK_UNSUBMITTED`（先 `submit` 或
   `release`），别人的报 `E_INGEST_BUSY` 并指出占用会话，让后来者立刻停手而不是重试等待；
   `ingest` 在入队前也过同一道检查（`--dry-run` 用临时库，不受限），避免两个执行体一起改
   任务目录与 `.wiki/tmp`；也避免「反复 next 续租」把 `attempts`、`epoch` 变成无效计数；
   `close_task` 用
   `WHERE id=? AND epoch=? AND state='leased'` 的 CAS，rowcount=0 抛
   `E_TASK_STALE`。不要用先查后写替代 CAS。漂移任务不发出，`expire_task` 置终态
   `expired` 并写 `last_error`、`closed_at`；任务行只增不删，供审计。`enqueue_task` 做树
   唯一性：一份正文不能同时属于两棵未关闭的 page 任务树，冲突抛 `E_TASK_CONFLICT`；
   route 任务只是临时持有者，不挡新树。同一次 route 提交拆出的 page 任务共享
   `route_task`，允许同一来源按主题进入多页，是这条不变量的唯一例外。
   领取顺序按任务树深度优先：`_task_order` 以 `inputs.route_task` 判是否属于某棵树，
   页面任务（有 parent）排在来源任务之前，同层按 `created_at`、`rowid`；`rowid` 让同秒入队的
   兄弟页保持 route 里的书写顺序，链接不再靠运气。`_claim_queued` 在未指定 `--kind` 时，只要
   还有未关闭的 page 任务，就先不发新的 route 任务——一棵树没写完不换正文。
   `claim_tasks` 先用 `_active_page_tree` 钉住最老那棵未关闭的页面树，`--batch` 只在这棵树里
   发：`_batch_rows` 过滤成同一棵树，树里凑不满一批就少发，不拿别的树补位；`--kind route`
   不钉树，route 任务各自成树。树里连漂移都没有可发的任务时才退回常规顺序，避免一棵坏树
   饿死整条队列。
6. **任务快照有 9 个来源指纹。** 入队时快照 `key`、`raw_id`、`source_size`、
   `source_mtime`、`source_digest`、`raw`、`raw_size`、`raw_mtime`、`raw_digest`；
   `task_drift` 先比 stat，不一致再读字节比 digest（只是 touch 不算漂移），raw 副本则读文件、
   剥前言后按正文比 `raw_digest`——前言里的挂载路径列表变了不算漂移。报错分别为
   `source <key> changed`、`raw copy <raw> was edited`。缺指纹的旧快照直接判漂移；update
   任务还比 `base_rev`。`claim_tasks` 不发出漂移任务，直接过期；提交已过期任务返回
   `E_SOURCE_DRIFT`。
7. **校验先于写入。** `submit_route` 先整体构建 plan 再逐条应用；`submit_page` 先
   `validate_page_payload`（字段、长度、slug、链接存在性、引文逐字命中）再 `write_rev`。
   任何一步失败都不得留下部分写入。
8. **引文比较忽略空白。** 两侧都做 `re.sub(r"\s+", "", ...)`，其余逐字。
9. **观看区只在命令入口推送。** `open_wiki()` 按 `publish` 开关调用 `sync_view()` 推送
   `<view>/pages`；`sync` 用 `open_wiki(push=False)`，先读磁盘再改账本，避免推送把刚挪走
   的文件重建出第二份。`--dry-run` 不建库、不推送。
10. **没有旧页面接管。** `page_revs` 为空时也不扫描 `.wiki/pages/`：该目录不再被读取，
   账本里没有的页面不会被文件内容凭空生成；升级后残留的 `.wiki/pages/` 由用户手动删除。
11. **工作区边界。** `open_wiki` 以 cwd 为 workspace，`--root` 必须是 workspace 或其子目录；
    `--payload`、`body_file`、`page import --from`、`export` 统一走
    `inside_root`。`--dry-run` 临时库在 `<root>/.wiki/tmp/`，进程退出时删除。`ingest` 跳过
    解析后落在扫描根外的符号链接。导入 `errors.py` / `workspace.py` 前设
    `sys.dont_write_bytecode`，避免全局技能目录在区外留下 `__pycache__`。
12. **工作流只读、不进账本、不随查询注入。** `scan_files` 在扫描根一级跳过 `workflows/`
    （`workflows.WORKFLOW_DIRS`），不因旧 `scan.ignore` 是否包含它而改变；`workflows.py` 不
    打开数据库、不写 `.wiki/`、不排任务。`workflow_roots` 只返回实际存在的 `workflows/`，
    `scan` 收其一级子目录，主文件固定为 `WORKFLOW.md`。
    `query` 不读工作流文件，工作流只通过显式 `workflow search` 检索，项目工作流与内置工作流
    一起参与，只返回 `valid` 项。`workflow list` 仍列出非法项并带 `error`，`roots` 只有
    实际扫描到的 `workflows/`，并汇总带 warning 的项数。`workflow load` 的 id 必须是
    该目录的一级目录名或内置工作流 id，拒绝 `.`、`..`、路径分隔符和区外符号链接。工作流文件是活
    文件，不做缓存或 digest。
    步骤契约在 `parse_steps` / `_contract`：只取第一个 `## 步骤` 到下一个二级标题之间的顶层
    列表项（缩进最浅一层），代码块、缩进子项和其他节不算；`tool:`/`human:`/`agent:`/`verify:`
    接受全角冒号与大小写变体，输出统一小写。部分步骤未标给 `W_STEP_UNTYPED`、标记后无内容给
    `W_STEP_EMPTY`；整份零标记、无 `## 步骤` 或该节无列表项在 `UNTYPED_ALLOWED` 为真
    （0.5.0）时只给 warning，翻转后（0.6.0）改判 error。有标记但没有 `tool:` 一律 invalid，
    hint 指向 `process` 页（纯人工流程不属于工作流）。
13. **话题只读且不进账本。** `scan_files` 同时跳过 `话题/`，话题不会被当成来源。`topics.py`
    只读 `话题/*.md`，取 frontmatter 的 `name/description/status/updated`；正文为空或文件超过
    200000 字符记为非法，但不进 `query` 的 `topics`。`query` 的 `topic_matches` 与页面
    `matches` 分开计数；话题文件由 agent 直接写，不经账本。
14. **观看区目录由 `publish` 配置决定。** `publish_dir_name` 要求 `dir` 是单个目录名，拒绝
    `.wiki`、`话题`、`workflows`；`ensure()` 在开关判断之前调用它，
    非法值即使 `enabled:false` 也报 `E_CONFIG`。`publish.workflows` 控制是否投放工作流。
    `ensure_publish` 始终创建 `<view>/` 与 `<view>/raw/`，
    `enabled:false` 只跳过 `sync_view`。`scan_files` 在扫描根无条件跳过该目录名，所以关了
    发布也不能拿它当来源；换掉名字后旧目录恢复为普通来源。`sync_view` 对 `pages/` 的手改
    只告警不覆盖，`topics/`、`workflows/` 只在目标不存在时投放，`README.md` 每次推送重写，
    其他文件（含 `.obsidian/`）不删不改。
15. **route 任务必须登记检索。** `query --task <id> 词...` 每个词独立检索各返 5 条、按词分组，
    追加一行 `{ts,words,hits}` 到 `<task_dir>/queries.jsonl`；重复词不重复计配额，配额是
    `5 × max(1, ceil(chars/1000))`（`QUERY_BUDGET_CHARS`、`QUERY_WORDS_PER_WINDOW`）。
    `submit_route` 在载荷结构校验之后、写账本之前检查：没登记报 `E_NO_QUERY`，超配额报
    `E_QUERY_BUDGET`；两者都把任务退回队列，查询日志按任务目录保留，任务关闭时一起删掉，
    不留审计历史。`task_work` 把这些数字放进 `work.queries`，route brief 直接读它。
16. **任务目录只是素材，不是队列。** `next` 领取后把任务物化到
    `.wiki/tasks/<id>/`（`brief.md`、`work.json`、`payload.json`）；发放仍只由 `next` 决定。
    `payload.json` 只在不存在时写入，重领只刷新 `brief.md` 与 `work.json`，不覆盖 agent 已填
    的内容；未提交的租约任务不重发：本会话报 `E_TASK_UNSUBMITTED`、别的会话报
    `E_INGEST_BUSY`，都指向这个目录并说明归属，租约到期后按现有规则回队，漂移任务置
    `expired`。
    `close_task`/`expire_task` 成功后删目录，残留目录由 `next`、`ingest` 的
    `cleanup_task_dirs` 清掉。`submit` 默认读该目录的 `payload.json`，`--payload` 只是后备。
17. **同一页面同时只有一个未关闭的 page 任务。** 账本里用部分唯一索引
    `tasks_open_page ON tasks(subject) WHERE kind='page' AND state IN ('queued','leased')`
    兜底（`ledger.ensure_open_page_index` 建索引前把重复行置 `expired` 并迁移
    `task_sources`）。`enqueue_task` 撞 `IntegrityError` 时回查已有任务并复用；`submit_route`
    命中已领走的 page 任务仍报 `E_STATE`，不另开第二条。route 任务与来源自己的转换任务
    不计入这条约束。
18. **写命令按工作区排队。** 会写账本或观看区的命令在 `main()` 里用
    `workspace.workspace_lock` 抢 `<root>/.wiki/lock`（`O_CREAT|O_EXCL` 写入 pid），等待
    5 秒仍拿不到抛 `E_BUSY`（exit 4）；非 posix 平台不按 pid 判活，只按 30 分钟陈旧阈值
    接管，posix 用 `os.kill(pid, 0)` 判断持有者是否还在。`--dry-run` 且没有 `.wiki/` 时不
    加锁，避免试运行留下目录。


## 数据方向

```text
源文件 → scan_files → extract_text → <view>/raw/<key>-<digest>.md + sources 行
                                        ↓ enqueue route 任务
route 载荷 → 校验 → 页面任务（new/merge）或来源结算（skip）
page 载荷 / page link / page import / page link --repair → 校验 → page_revs + links → 推送
→ <view>/pages 副本

workflows/<id>/WORKFLOW.md ──query/scan──→ 工作流命中（只读，不落盘）
                              └─workflow load→ 全文 + 分类型步骤 + resources

话题/*.md ──query/scan──→ topics 命中（只读，不落盘）
```

`raw` 文件名取来源 basename，同名按 `-2`、`-3` 顺延，不跟正文摘要走；`sources.raw` 存相对
路径。一份正文一行 `raws`：手改按整份正文采纳，挂载它的 `sources` 一起换到新摘要，文件留在
原路径。

`links` 是 `page_revs.meta` 里的页面 id 数组，并镜像到 `links` 表；`write_rev` 按 meta 重建，
不再从 body 的 `[[...]]` 或 `## 关联` 段解析。观看区由 `links_for` 取出链与入链，渲染成一个
`## 关联` 段；正文里的该段在提交与推送时剥掉。`page link` 与 `page import` 的 `links` 走
同一套 `check_links`/`write_rev`；`page link` 成功后立即 `sync_view`，不等下一个命令。

`page link --repair` 是唯一的推断式补链：把 `meta.sources` 经 `sources.raw` 归并到同一条
原始正文，同组页两两配对，
已有任一方向就跳过，每对只加小 id → 大 id 一条；`--dry-run` 不写修订。它只补缺边，不改变
已有方向，也不把正文 `[[...]]` 变成第二套边。

`sync` 回收观看区时先剥掉关系段，再解析末尾证据段：关系段渲染在证据之后，若先解析证据，
带关联的页面会把证据块当正文收进来、下次推送再追加一份。重复的证据段也会在回收时一并
剥掉，只保留最后一份。

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
  `tests/selftest.py`；保持工作流只经显式 `workflow search` 检索、`load` 只读、内置可覆盖。
  改进步骤契约时同时改 `parse_steps`、`_contract`、selftest 与 `workflows.md` 的判定表。
- 改话题：同步 `docs/readme/cli.md`、`docs/readme/topics.md`、`SKILL.md`、
  `skills/llm-wiki/workflows/topic-exploration/WORKFLOW.md`、`scripts/health.py` 的
  `SCRIPT_FILES`、`tests/selftest.py`；保持 `ingest` 跳过 `话题/`、`query` 只带话题。
- 改观看区或发布：同步 `docs/readme/cli.md`、`README.md`、
  `docs/readme/design-principles.md`、`SKILL.md`、`tests/selftest.py`；保持 `sync` 只回收、
  推送只发生在命令入口，且观看区目录名始终跳过扫描。
- 改表结构：同步 `ledger.REQUIRED_COLUMNS`、`ledger.TABLES` 与本文件的表清单；有数据的旧库
  由 `ledger.ensure_columns` 直接 `E_STATE` 拒绝，全空的旧表按新结构重建，不写迁移。
- 改渲染：`render_page` 与 `split_frontmatter` 必须能往返；跑 `tests/selftest.py` 的
  import/restore 用例。
- 改任务：并发前提是单写者；不要为了"更快"去掉提交前的状态与租约检查。
