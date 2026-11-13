# 引擎内部

对象：`skills/llm-wiki/scripts/wiki.py`（约 1970 行，单文件上限 2000 行；再长就按
解析 / 账本 / 任务 / 命令拆模块）。本文件只记录改代码时容易踩的内部约定，命令与字段以
`skills/llm-wiki/references/cli.md` 为准。

## 布局

| 区段 | 位置 | 说明 |
| --- | --- | --- |
| 常量与配置 | 文件头、`DEFAULT_CONFIG`、`load_config` | `BODY_MAX_CHARS=60000`、`SUMMARY_MAX_CHARS=200`、`TASK_LEASE_SECONDS=900`、`DEFAULT_BATCH=20` |
| 错误与输出 | `WikiError`、`emit`、`fail` | 错误码 + `next` + 退出码；所有命令走同一信封 |
| 解析 | `extract_text` 及 `pdf_to_text`、`docx_to_text`、`html_to_text`、`decode_bytes` | 纯函数，不碰数据库；超限在 `read_bytes` 抛 |
| 页面渲染 | `render_page`、`split_frontmatter`、`yaml_scalar` | 渲染与解析 frontmatter 必须对称；改动一侧要同步另一侧 |
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

## 数据方向

```text
源文件 → scan_files → extract_text → .wiki/raw/<key>-<digest>.md + sources 行
                                        ↓ enqueue route 任务
route 载荷 → 校验 → 页面任务（new/merge）或来源结算（skip）
page 载荷 → 校验 → page_revs + 镜像 + links
```

`raw` 文件名带 digest 前缀，避免同名覆盖；`sources.raw` 存相对路径。

## 改动检查表

- 改字段或校验：同步 `references/cli.md`、`SKILL.md`、`selftest.py`。
- 改表结构：在 `ensure()` 里做幂等迁移，并在 `verification.md` 记录旧库升级结果。
- 改渲染：`render_page` 与 `split_frontmatter` 必须能往返；跑 `selftest.py` 的
  import/restore 用例。
- 改任务：并发前提是单写者；不要为了"更快"去掉 CAS 或 epoch 校验。
