# 引擎内部

对象：`skills/llm-wiki/scripts/wiki.py`（1908 行，单文件上限 2000 行；公共错误在
`errors.py`，frontmatter 解析在 `frontmatter.py`，页面检索在 `search.py`，场景能力在
`scenarios.py`，运行时基线在 `runtime.py`，安装自检在 `health.py`，工作区边界在
`workspace.py`，任务信息面在 `tasks.py`，页面载荷校验在 `validation.py`；再长就按解析 / 账本 / 任务 / 命令拆模块）。
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
| 场景能力 | `scenarios.py` | `scan`/`inventory`/`match`/`load`；只读文件，不写 `.wiki/`，不生成任务 |
| 页面渲染 | `render_page` | 依赖 `frontmatter.yaml_scalar`；改动一侧要同步另一侧 |
| 任务信息面 | `tasks.py` | `brief`（做什么/怎么做/提交后的影响）、`template`（固定骨架）、章节骨架；不读写数据库 |
| 安装自检 | `health.py` | 解释器、技能文件与模板、工作区可写、账本可读；`check` 深检，门禁用轻检 |
| 页面校验 | `validation.py` | 页载荷字段、章节、evidence 逐字校验、来源并集与旧证据保留；纯函数，只读 `Wiki` |
| 账本 | `class Wiki` | 表、来源、修订、镜像、任务全在这里 |
| 命令层 | `command_*`、`submit_route`、`submit_page` | 只做参数解析、调用 `Wiki`、组装输出 |

## 表

- `sources(key, path, digest, size, mtime, raw, parser, status, updated_at)`
- `links(src, dst)`
- `tasks(id, kind, subject, state, epoch, inputs, payload, last_error, attempts,
  lease_until, created_at, closed_at)`
- `task_sources(task_id, source_key)`
- `page_revs(page, rev, meta, body, digest, render_digest, origin, accepted_at)`
- `drift(id, page, path, digest, found_at, note)`

表结构只增不改；迁移写在 `ensure()` / `_create_schema()`，不要静默重建。

## 不变量

1. **账本是唯一事实源。** 页面读取一律走 `latest_rev_row`；`page_record` 返回
   `{id, rev, meta, body, status}`。
2. **`computed_status` 每次重算。** `meta.source_digests` 与当前 `sources.digest` 不一致，
   或来源 `missing/unsupported/oversize`，页面即 `stale`；不把 status 存成可漂移的字段。
3. **修订只增不改。** `write_rev` 取 `MAX(rev)+1`，写 `digest`（元数据+正文的规范化哈希）
   与 `render_digest`（渲染文本哈希），再原子写镜像、重建链接。
4. **ledger 与镜像分离。** 渲染实现变化只会让 `render_digest` 变，不应被当成"文件被改"；
   文件被改是"实际文本 ≠ 存储的 render_digest"。
5. **任务 epoch 单调。** `claim_tasks` 先回收过期 lease，领取时 `epoch = epoch + 1`；
   `close_task` 用 `WHERE id=? AND epoch=? AND state='leased'` 的 CAS，rowcount=0 抛
   `E_TASK_STALE`。不要用先查后写替代 CAS。
6. **page 任务领取时刷新来源摘要。** `claim_tasks` 对 page 任务调用
   `refresh_source_digests`，否则重新解析后任务会永远 `E_SOURCE_DRIFT`。
7. **校验先于写入。** `submit_route` 先整体构建 plan 再逐条应用；`submit_page` 先
   `validate_page_payload`（字段、长度、slug、链接存在性、引文逐字命中）再 `write_rev`。
   任何一步失败都不得留下部分写入。
8. **引文比较忽略空白。** 两侧都做 `re.sub(r"\s+", "", ...)`，其余逐字。
9. **镜像修复在命令入口。** `open_wiki()` 调用 `ensure_mirror()`：一致跳过；被改先
   `record_drift` 再写回；孤儿 `.md` 移入 drift。`--dry-run` 不建库、不进镜像修复。
10. **旧页面迁移只跑一次。** `migrate_legacy_pages` 仅在 `page_revs` 为空时把
    file-first 页面收为 rev1（`origin=external`），之后不再猜测文件内容。
11. **工作区边界。** `open_wiki` 以 cwd 为 workspace，`--root` 必须是 workspace 或其子目录；
    `read` 路径、`--payload`、`body_file`、`page import --from`、`export` 统一走
    `inside_root`。`--dry-run` 临时库在 `<root>/.wiki/tmp/`，进程退出时删除。`ingest` 跳过
    解析后落在扫描根外的符号链接。导入 `errors.py` / `workspace.py` 前设
    `sys.dont_write_bytecode`，避免全局技能目录在区外留下 `__pycache__`。
12. **场景只读且不进账本。** `scan_files` 在扫描根一级跳过 `场景能力/`，不因旧
    `scan.ignore` 是否包含它而改变；`scenarios.py` 不打开数据库、不写 `.wiki/`、不排任务。
    `query` 只匹配 `valid` 的场景；`scenario list` 仍列出非法项并带 `error`。`scenario load`
    的 id 必须是 `场景能力/` 下的一级目录名，拒绝 `.`、`..`、路径分隔符和区外符号链接。
    场景文件是活文件，不做缓存或 digest。

## 数据方向

```text
源文件 → scan_files → extract_text → .wiki/raw/<key>-<digest>.md + sources 行
                                        ↓ enqueue route 任务
route 载荷 → 校验 → 页面任务（new/merge）或来源结算（skip）
page 载荷 → 校验 → page_revs + 镜像 + links

场景能力/<id>/SCENARIO.md ──query/scan──→ 场景命中（只读，不落盘）
                              └─scenario load→ 全文 + resources
```

`raw` 文件名带 digest 前缀，避免同名覆盖；`sources.raw` 存相对路径。

`next` 只输出 `{id, brief, template, work}`；`epoch`、`lease_until`、`attempts` 留在账本里。
`submit` 的 `--epoch` 只是兼容旧脚本的可选项，正常路径不要求。任务包由 `tasks.py` 生成，
改任务设计时只改这一处，不要在 `SKILL.md` 里补流程说明。

## 改动检查表

- 改字段或校验：同步 `docs/readme/cli.md`、`skills/llm-wiki/scripts/tasks.py`、
  `skills/llm-wiki/scripts/validation.py`、`skills/llm-wiki/scripts/health.py`、`SKILL.md`、
  `tests/selftest.py`。
- 改场景：同步 `docs/readme/cli.md`、`docs/readme/scenarios.md`、`SKILL.md`、
  `skills/llm-wiki/scripts/health.py` 的 `SCRIPT_FILES`、`tests/selftest.py`；保持 `query`
  只匹配合法场景、`load` 只读。
- 改表结构：在 `ensure()` 里做幂等迁移，并在 `docs/agent/verification.md` 记录旧库升级结果。
- 改渲染：`render_page` 与 `split_frontmatter` 必须能往返；跑 `tests/selftest.py` 的
  import/restore 用例。
- 改任务：并发前提是单写者；不要为了"更快"去掉提交前的状态与租约检查。
