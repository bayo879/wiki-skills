# CLI 契约

所有命令在知识库根目录执行，或加 `--root <目录>`。除 `version` 外都接受 `--json`。
引擎只依赖 Python 标准库。

## 输出信封

成功：

```json
{"ok": true, "cmd": "next", "version": "0.2.0", "root": "/abs/root",
 "summary": "claimed 3 task(s)…", "data": {…}}
```

失败：

```json
{"ok": false, "cmd": "submit", "version": "0.2.0", "root": "/abs/root",
 "error": {"code": "E_EVIDENCE_NOT_FOUND", "message": "quote not found in docs/a.md: '…'",
           "next": "re-read the raw source and copy an exact sentence"}}
```

不加 `--json` 时输出人类可读文本，错误写 stderr。

退出码：`0` 成功；`1` 有阻断项（解析失败、超限、数据库损坏）；`2` 用法或载荷错误；
`3` 状态不满足（任务过期、来源漂移、页面冲突）。

## 语义模型

- 来源（source）：扫描到的文件，键是相对根目录的路径。状态 `pending`、`done`、
  `skipped`、`unsupported`、`oversize`、`missing`。
- 任务（task）：`route`（每个来源一条）与 `page`（每页一条）。状态
  `queued → leased → accepted`；提交被拒时回到 `queued` 并记录 `last_error`。
- 页面（page）：多版修订，每版有 `rev`、`meta`、`body`、`digest`。当前状态
  `stable` 或 `stale`（引用来源变化或缺失）。
- `.wiki/pages/<type>/<id>.md` 是按账本渲染的镜像，不是事实源。

## 命令

### init

创建 `.wiki/`（`wiki.db`、`wiki.json`、`raw/`、`pages/`、`drift/`）并报告状态。
`data`: `{created, config, pages, sources, open_tasks}`。

### ingest

扫描根目录（跳过隐藏目录与 `scan.ignore`），解析新文件或内容变化的文件，写入
`.wiki/raw/`，并为每个新来源入队一条 route 任务。`--dry-run` 不落盘、不建 `.wiki/`；
`--force` 忽略哈希重新解析。

`data`: `{ingested[{key,raw,parser,digest,size,chars}], unchanged, unsupported[{key,suffix,reason,hint}],
oversize[{key,message}], failed[{key,message}], vanished, restored, queued_tasks, pending_total, pending}`。

超过 `ingest.max_source_bytes` 的文件进 `oversize`，不解析；无文本层的进
`unsupported`。两者都不会中止整批摄入。

### next

领取 queued 任务，默认数量取 `ingest.batch`（默认 20），可 `--batch N`、`--kind`。
route 任务优先。领取时 `epoch + 1`，租约 900 秒。

`data.tasks[]`: `{id, kind, subject, state, epoch, attempts, lease_until, last_error,
inputs, work}`。`work.sources[]` 带 `key/digest/raw/status/chars`；page 任务另有
`work.mode`、`work.base_rev`，以及当前页面 `work.page{id,rev,meta,body}`。

### submit

`submit <id> --epoch <n> --payload <file|->`。校验通过才写入并关闭任务；否则返回
`E_*` 并把任务退回队列，`last_error` 记录原因，可重新 `next` 后再交。

**route 载荷**：

```json
{"decisions": [{"source": "docs/a.md", "action": "new", "page": "refund-process",
                "title": "退款流程", "type": "process", "reason": "完整办理步骤"}]}
```

每个输入来源恰好一条决定，`action` 三选一：

- `new`：`page`、`title`、`type` 必填；页面不能已存在。
- `merge`：`page` 必填；页面必须存在。若该页已有未完成任务，来源并入该任务。
- `skip`：`reason` 必填；来源标记 `skipped`。

**page 载荷**：

```json
{"page": "refund-process", "title": "退款流程", "type": "process",
 "summary": "签收后 7 天内可申请，原路退回。",
 "links": ["refund-policy"],
 "body": "## 结论\n…", 
 "evidence": [{"source": "docs/a.md", "quote": "签收后七日内可申请退款"}],
 "boundaries": {"covered": "线上订单", "not_covered": "线下门店"}}
```

- `body` 或 `body_file` 二选一；最长 60000 字符；不能以 `---` 开头。
- `summary` 1–200 字符；`title` 1–120 字符；`type` 必须在 `schema.types` 里。
- `links` 只能指向已存在页面。
- `evidence` 至少一条；`quote` 去空白后须逐字命中该来源的 raw 文本。
- `boundaries.covered`、`boundaries.not_covered` 必填。

成功后 `data`: `{task, kind, page, rev, sources, queued_page_tasks}`。

### release

`release <id>`：把已领取未提交的任务退回队列，不记录错误。

### read

`read "raw:<key>" [--offset N] [--limit N]` 读解析后的来源文本；
`read "page:<id>"` 读页面正文；也可传根目录内的相对路径。
`data`: `{path, offset, limit, total_lines, next_offset, meta, content}`。

### query

`query <词...> [--limit N]`：按标题、摘要、正文命中数排序，返回子集。
`query --page <id>`：返回单页 `meta`、`rev`、`path`、`outgoing`、`incoming`、`body`。

检索结果是定位工具，不是覆盖证明。

### page

- `page import <id> --from <file>`：采用人工编辑的页面文件（可带 frontmatter），
  生成新修订（`origin=import`），不强制 evidence。只允许已存在的页面。
- `page restore <id> --rev N`：把第 N 版内容写成新修订（`origin=restore`）。

### export

`export <dir>`：把所有页面按 `type/id.md` 写到目标目录，用于离线浏览或迁移。

### status

`status [--brief]`。`data`: `{created, sources{状态:数量}, source_total, pages,
stale_pages, pending, tasks{open,queued,leased,route,page}, next}`。
`--brief` 只给一行摘要。

### doctor

`doctor [--fix]`：`PRAGMA integrity_check`、镜像修复、stale 页、未注册来源、
断链、过期租约。`--fix` 重建链接索引与镜像。`data`: `{issues[{severity,code,message}],
fixed, healthy}`；有 `fail` 级问题时退出码 1。

## 错误码

| 码 | 含义 | 处理 |
| --- | --- | --- |
| `E_USAGE` | 参数缺失或组合非法 | 看 `error.next`，不要重试同一载荷 |
| `E_PAYLOAD` | 载荷字段缺失/类型/长度不合法 | 按 message 修正字段 |
| `E_TASK_UNKNOWN` | 任务 id 不存在 | `next` 取新任务 |
| `E_TASK_STALE` | 任务未领取、已关闭或 epoch 过期 | 重新 `next`，用新 epoch |
| `E_TASK_SCOPE` | 载荷引用了任务范围外的来源 | 只用任务 `work.sources` 里的 key |
| `E_STATE` | 目标页面正被其他任务写 | 等该任务提交后再试 |
| `E_PAGE_CONFLICT` | 页面已存在（new）或版本已变（update） | 重新领任务，基于当前 rev 重写 |
| `E_PAGE_UNKNOWN` | merge 指向不存在的页面 | 改用 new，或查 `page_index` |
| `E_PAGE_GONE` | update 时页面消失 | 重新 `next` |
| `E_BAD_ID` | 页面 slug 非法 | 用小写字母数字与 `-_.` |
| `E_LINK_UNRESOLVED` | 链接目标不存在 | 先建目标页，或去掉链接 |
| `E_EVIDENCE_NOT_FOUND` | 引文不在来源原文里 | 重读 raw，逐字复制 |
| `E_SOURCE_DRIFT` | 来源在任务领取后被改动或删除 | 重新 `next`，重读来源 |
| `E_UNKNOWN_SOURCE` | 页面引用未登记的来源 | 先 `ingest` |
| `E_EMPTY` | 还没有任何来源 | 先 `ingest` |
| `E_NOT_FOUND` | 页面、版本或文件不存在 | 用 `query`/`read` 定位 |
| `E_PATH` | 路径越出知识库根目录 | 用根目录内相对路径 |
| `E_CONFIG` | `.wiki/wiki.json` 非法 | 按提示修正 JSON |
| `E_SOURCE_TOO_LARGE` | 单文件超过 `max_source_bytes` | 调高阈值或拆分来源 |
| `E_DB` | 数据库不可读 | 从备份恢复，不要手改 |

## 配置

`.wiki/wiki.json`（首次运行生成）：

```json
{"schema": {"types": ["concept", "entity", "process", "note"]},
 "scan": {"ignore": [".git", "node_modules", "target", "dist"]},
 "ingest": {"max_source_bytes": 50000000, "batch": 20}}
```

改 `schema.types` 会立即影响后续校验；已存在的页面不会被自动迁移。
