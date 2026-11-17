# 引擎内部

对象：`skills/llm-wiki/scripts/wiki.py`（2575 行，仍超过单文件 2000 行上限；拆分方案见
`docs/issues/2026-11-16-P2-split-wiki-engine.md`）。公共错误在 `errors.py`，frontmatter
解析在 `frontmatter.py`，账本表结构在 `ledger.py`，页面检索在 `search.py`，场景能力在
`scenarios.py`，话题沉淀在 `topics.py`，运行时基线在 `runtime.py`，安装自检在 `health.py`，
工作区边界在 `workspace.py`，任务信息面在 `tasks.py`，页面载荷校验在 `validation.py`。
本文件只记录改代码时容易踩的内部约定，命令与字段以 `docs/readme/cli.md` 为准。

## 布局

| 区段 | 位置 | 说明 |
| --- | --- | --- |
| 常量与配置 | 文件头、`DEFAULT_CONFIG`、`load_config` | `TASK_LEASE_SECONDS=900`；正文/摘要上限在 `validation.py` |
| 错误与输出 | `errors.py` 的 `WikiError`、`emit`、`fail` | 错误码 + `next` + 退出码；所有命令走同一信封 |
| 工作区边界 | `workspace.py` | `WIKI_DIR`、`window_safe`、`inside_root`、`dry_store`；文件参数一律先过 `inside_root` |
| 解析 | `extract_text` 及 `pdf_to_text`、`docx_to_text`、`html_to_text`、`decode_bytes` | 纯函数，不碰数据库；超限在 `read_bytes` 抛 |
| frontmatter | `frontmatter.py` | `split_frontmatter`、`unquote_scalar`、`yaml_scalar`；页面渲染与场景加载共用一套语法 |
| 页面检索 | `search.py` | `search_pages`、`rank_key`、`snippet`；只读 `Wiki`，不碰命令状态 |
| 场景能力 | `scenarios.py` | `scan`/`inventory`/`match`/`load`/`default`；`scenario/` 与旧 `场景能力/` 都扫描（同 id 前者优先），再与 `scenarios/` 内置场景合并，只读文件，不写 `.wiki/`，不生成任务 |
| 话题沉淀 | `topics.py` | `scan`/`match`；只扫 `话题/*.md`，只读文件，不写 `.wiki/`、不排任务；检索打分同 `search.py` |
| 页面渲染 | `render_page` | 依赖 `frontmatter.yaml_scalar`；改动一侧要同步另一侧 |
| 观看区与发布 | `sync_view`、`sync_view_file`、`render_view_readme`、`command_sync` | 账本 → `<view>/pages`；`raw/` 由摄入维护；`topics/`、`scenarios/` 只首次投放；手改只告警，`sync` 回收 |
| 任务信息面 | `tasks.py` | `brief`（做什么/怎么做/提交后的影响）、`template`（固定骨架）、章节骨架；不读写数据库 |
| 安装自检 | `health.py` | 解释器、技能文件与模板、工作区可写、账本可读；`check` 深检，门禁用轻检 |
| 页面校验 | `validation.py` | 页载荷字段、章节、evidence 逐字校验、来源并集与旧证据保留；纯函数，只读 `Wiki` |
| 账本结构 | `ledger.py` | `SCHEMA_SQL`、`create_schema`、`ensure_columns`、`ensure_open_page_index`；表与索引契约只在这里定义，`Wiki.ensure` 调用 |
| 账本 | `class Wiki` | 表、来源、修订、观看区推送、任务全在这里 |
| 命令层 | `command_*`、`submit_route`、`submit_page` | 只做参数解析、调用 `Wiki`、组装输出 |

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

表结构每版固定；缺列的旧库由 `ledger.ensure_columns` 拒绝（`E_STATE`），不写迁移。

## 不变量

1. **账本是唯一事实源。** 页面读取一律走 `latest_rev_row`；`page_record` 返回
   `{id, rev, meta, body, status}`。
2. **`computed_status` 每次重算。** `meta.source_digests` 与当前 `sources.digest` 不一致，
   或来源 `missing/unsupported/oversize`，页面即 `stale`；不把 status 存成可漂移的字段。
3. **修订只增不改。** `write_rev` 取 `MAX(rev)+1`，写 `digest`（元数据+正文的规范化哈希）
   与 `render_digest`（渲染文本哈希），再重建链接；页面文件只由观看区推送写出。
4. **账本不写页面副本。** `.wiki/` 只放账本、任务目录与草稿（原文在 `<view>/raw/`）；页面正文的唯一
   落盘副本在 `<view>/pages/`，由 `sync_view` 按账本推送，`render_digest` 只是渲染文本的
   历史哈希，不再用于比对文件。
5. **任务 epoch 单调。** `claim_tasks` 先回收过期 lease，领取时 `epoch = epoch + 1`；
   队列不足一批时按同样顺序重新发放仍在租约里的任务（`_resume_leased`，同样 `epoch = epoch + 1`、
   同样先查漂移），让中断后重跑的 agent 接着做同一个任务；`close_task` 用
   `WHERE id=? AND epoch=? AND state='leased'` 的 CAS，rowcount=0 抛
   `E_TASK_STALE`。不要用先查后写替代 CAS。漂移任务不发出，`expire_task` 置终态
   `expired` 并写 `last_error`、`closed_at`；任务行只增不删，供审计。`enqueue_task` 做树
   唯一性：一个来源不能同时属于两棵未关闭的 page 任务树，冲突抛 `E_TASK_CONFLICT`；
   route 任务只是临时持有者，不挡新树。同一次 route 提交拆出的 page 任务共享
   `route_task`，允许同一来源按主题进入多页，是这条不变量的唯一例外。
6. **任务快照有 8 个来源指纹。** 入队时快照 `key`、`source_size`、`source_mtime`、
   `source_digest`、`raw`、`raw_size`、`raw_mtime`、`raw_digest`；`task_drift` 先比 stat，
   不一致再读字节比 digest（只是 touch 不算漂移），raw 副本同理，报错分别为
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
    `read` 路径、`--payload`、`body_file`、`page import --from`、`export` 统一走
    `inside_root`。`--dry-run` 临时库在 `<root>/.wiki/tmp/`，进程退出时删除。`ingest` 跳过
    解析后落在扫描根外的符号链接。导入 `errors.py` / `workspace.py` 前设
    `sys.dont_write_bytecode`，避免全局技能目录在区外留下 `__pycache__`。
12. **场景只读、不进账本、不随查询注入。** `scan_files` 在扫描根一级跳过 `scenario/` 与
    旧 `场景能力/`（`scenarios.SCENARIO_DIRS`），不因旧 `scan.ignore` 是否包含它们而改变；
    `scenarios.py` 不打开数据库、不写 `.wiki/`、不排任务。`scenario_roots` 按
    `(scenario, 场景能力)` 顺序返回实际存在的目录，`scan` 逐目录收一级子目录并跳过重复 id，
    因此同名时英文目录生效。`query` 不读场景文件，场景只通过显式 `scenario search` 检索，
    项目场景与内置场景一起参与，只返回 `valid` 项。`scenario list` 仍列出非法项并带 `error`，
    并在 `roots` 里报告实际扫描到的目录。`scenario load` 的 id 必须是这两个目录之一的一级
    目录名或内置场景 id，拒绝 `.`、`..`、路径分隔符和区外符号链接。场景文件是活文件，
    不做缓存或 digest。
13. **话题只读且不进账本。** `scan_files` 同时跳过 `话题/`，话题不会被当成来源。`topics.py`
    只读 `话题/*.md`，取 frontmatter 的 `name/description/status/updated`；正文为空或文件超过
    200000 字符记为非法，但不进 `query` 的 `topics`。`query` 的 `topic_matches` 与页面
    `matches` 分开计数；话题文件由 agent 直接写，不经账本。
14. **观看区目录由 `publish` 配置决定。** `publish_dir_name` 要求 `dir` 是单个目录名，拒绝
    `.wiki`、`话题`、`scenario`、`场景能力`；`ensure()` 在开关判断之前调用它，非法值即使
    `enabled:false` 也报 `E_CONFIG`。`ensure_publish` 始终创建 `<view>/` 与 `<view>/raw/`，
    `enabled:false` 只跳过 `sync_view`。`scan_files` 在扫描根无条件跳过该目录名，所以关了
    发布也不能拿它当来源；换掉名字后旧目录恢复为普通来源。`sync_view` 对 `pages/` 的手改
    只告警不覆盖，`topics/`、`scenarios/` 只在目标不存在时投放，`README.md` 每次推送重写，
    其他文件（含 `.obsidian/`）不删不改。
15. **任务目录只是素材，不是队列。** `next` 领取后把任务物化到
    `.wiki/tasks/<id>/`（`brief.md`、`work.json`、`payload.json`）；发放仍只由 `next` 决定。
    `payload.json` 只在不存在时写入，重领只刷新 `brief.md` 与 `work.json`，不覆盖 agent 已填
    的内容；未提交的租约任务由 `next` 以同一 id 重新发放，不置 `expired`。
    `close_task`/`expire_task` 成功后删目录，残留目录由 `next`、`ingest` 的
    `cleanup_task_dirs` 清掉。`submit` 默认读该目录的 `payload.json`，`--payload` 只是后备。
16. **同一页面同时只有一个未关闭的 page 任务。** 账本里用部分唯一索引
    `tasks_open_page ON tasks(subject) WHERE kind='page' AND state IN ('queued','leased')`
    兜底（`ledger.ensure_open_page_index` 建索引前把重复行置 `expired` 并迁移
    `task_sources`）。`enqueue_task` 撞 `IntegrityError` 时回查已有任务并复用；`submit_route`
    命中已领走的 page 任务仍报 `E_STATE`，不另开第二条。route 任务与来源自己的转换任务
    不计入这条约束。
17. **写命令按工作区排队。** 会写账本或观看区的命令在 `main()` 里用
    `workspace.workspace_lock` 抢 `<root>/.wiki/lock`（`O_CREAT|O_EXCL` 写入 pid），等待
    5 秒仍拿不到抛 `E_BUSY`（exit 4）；非 posix 平台不按 pid 判活，只按 30 分钟陈旧阈值
    接管，posix 用 `os.kill(pid, 0)` 判断持有者是否还在。`--dry-run` 且没有 `.wiki/` 时不
    加锁，避免试运行留下目录。


## 数据方向

```text
源文件 → scan_files → extract_text → <view>/raw/<key>-<digest>.md + sources 行
                                        ↓ enqueue route 任务
route 载荷 → 校验 → 页面任务（new/merge）或来源结算（skip）
page 载荷 → 校验 → page_revs + links → 推送 → <view>/pages 副本

scenario/<id>/SCENARIO.md ──query/scan──→ 场景命中（只读，不落盘）
                              └─scenario load→ 全文 + resources

话题/*.md ──query/scan──→ topics 命中（只读，不落盘）
```

`raw` 文件名带 digest 后缀（`<basename>-<digest8>.md`），避免同名覆盖；`sources.raw` 存相对路径。

观看区 `pages/` 是账本的唯一页面落盘副本（按标题命名，`view_digest` 记录上次推送的
字节）；手改过的副本不覆盖，`sync` 收成新修订后才恢复推送。`sync` 只认
`<view>/pages/**/*.md`。

`next` 只输出 `{id, brief, template, work}`；`epoch`、`lease_until`、`attempts` 留在账本里。
`submit` 的 `--epoch` 只是兼容旧脚本的可选项，正常路径不要求。任务包由 `tasks.py` 生成，
改任务设计时只改这一处，不要在 `SKILL.md` 里补流程说明。

## 改动检查表

- 改字段或校验：同步 `docs/readme/cli.md`、`skills/llm-wiki/scripts/tasks.py`、
  `skills/llm-wiki/scripts/validation.py`、`skills/llm-wiki/scripts/health.py`、`SKILL.md`、
  `tests/selftest.py`。
- 改场景：同步 `docs/readme/cli.md`、`docs/readme/scenarios.md`、`SKILL.md`、
  `skills/llm-wiki/scripts/health.py` 的 `SCRIPT_FILES` 与 `BUILTIN_SCENARIOS`、
  `tests/selftest.py`；保持场景只经显式 `scenario search` 检索、`load` 只读、内置可覆盖。
- 改话题：同步 `docs/readme/cli.md`、`docs/readme/topics.md`、`SKILL.md`、
  `skills/llm-wiki/scenarios/topic-exploration/SCENARIO.md`、`scripts/health.py` 的
  `SCRIPT_FILES`、`tests/selftest.py`；保持 `ingest` 跳过 `话题/`、`query` 只带话题。
- 改观看区或发布：同步 `docs/readme/cli.md`、`README.md`、
  `docs/readme/design-principles.md`、`SKILL.md`、`tests/selftest.py`；保持 `sync` 只回收、
  推送只发生在命令入口，且观看区目录名始终跳过扫描。
- 改表结构：同步 `ledger.REQUIRED_COLUMNS` 与本文件的表清单；旧库由 `ledger.ensure_columns`
  直接 `E_STATE` 拒绝，不写迁移。
- 改渲染：`render_page` 与 `split_frontmatter` 必须能往返；跑 `tests/selftest.py` 的
  import/restore 用例。
- 改任务：并发前提是单写者；不要为了"更快"去掉提交前的状态与租约检查。
