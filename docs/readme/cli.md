# CLI 契约

所有命令以会话工作区为根目录，在当前目录执行。`--root` 可选，只能是当前目录或它下面的
子目录；指到工作区外返回 `E_PATH`。除 `version` 外都接受 `--json`。引擎只依赖 Python
标准库。

除 `check`、`version`、`scenario` 外，命令启动时会准备 `.wiki/` 与观看区 `<view>/`
（`publish.dir`，默认 `wiki/`），并按账本推送一次；`sync` 例外，它只回收、不推送。

## 输出信封

成功：

```json
{"ok": true, "cmd": "next", "version": "0.4.1", "root": "/abs/root",
 "summary": "1 task(s) ready; read each brief, fill its template, then submit",
 "data": {…}}
```

失败：

```json
{"ok": false, "cmd": "submit", "version": "0.4.1", "root": "/abs/root",
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
  `queued → leased → accepted`；另有过期终态 `expired`，保留行供审计，不再领取。
  提交被拒时回到 `queued` 并记录 `last_error`。
- 页面（page）：多版修订，每版有 `rev`、`meta`、`body`、`digest`。当前状态
  `stable` 或 `stale`（引用来源变化或缺失）。
- 页面按账本渲染两份产物：`.wiki/pages/<type>/<id>.md` 镜像与观看区
  `<view>/pages/<type>/<标题>.md`。两者都不是事实源，事实源是账本。

## 工作区边界

- 根目录默认是当前工作目录；`--root` 只允许当前目录或它的子目录，不能指到区外。
- 下列参数解析后必须落在根目录内，否则返回 `E_PATH`：`read` 的路径、`--payload`、
  `body_file`、`page import --from`、`export <dir>`。
- 草稿、载荷、导出与临时文件放 `<root>/.wiki/` 下（`.wiki/drafts/`、`.wiki/payloads/`、
  `.wiki/export`、`.wiki/tmp/`）。`--dry-run` 的临时库在 `.wiki/tmp/`，退出时清理。

## 命令

### check

检查当前安装的运行时：Python 3.12+、技能必需文件与模板、工作区可写、已有账本可读。
不创建 `.wiki/`，不读写页面内容。`data`: `{ok, checks[{name,ok,detail}]}`；`ok` 与退出码
一致（`0` 通过，`1` 有失败项）。`init`、`ingest`、`next`、`submit` 会先做同一项轻量检查，
失败返回 `E_HEALTH`。`check` 是安装/日常流程的门禁；完整回归测试在仓库的
`tests/selftest.py`，不随技能分发。

### init

创建 `.wiki/`（`wiki.db`、`wiki.json`、`pages/`、`drift/`、`payloads/`、`drafts/`），并准备
观看区 `<view>/` 与 `<view>/raw/`。`data`: `{created, config, pages, sources, open_tasks}`。

### ingest

扫描根目录（跳过隐藏目录、`scan.ignore`、`scenario/`、`场景能力/`、`话题/` 与观看区
`<view>/`），解析新文件或内容变化的文件，写入 `<view>/raw/`（默认 `wiki/raw/`），并为每个
新来源入队一条 route 任务。`scenario/`（旧名 `场景能力/`）是流程能力、`话题/` 是 wiki 的
探索层，都不作为摄入来源，摄入时整目录跳过；观看区目录名即使关了发布也仍跳过。场景与
话题的检索分别由 `scenario search` 和 `query` 直接扫描文件完成。

`--dry-run` 用 `.wiki/tmp/` 下的临时库，退出时清理，不写账本；它只列 `would_ingest`，不预告
末尾补偿入队会补哪些来源。`--force` 忽略哈希重新解析。`--only` 按根目录相对 key
精确匹配，不校验参数形状：传绝对路径或含 `..` 的值只会匹配不到来源，命令仍成功退出 0，
也不会摄入任何文件。

`data`: `{ingested[{key,raw,parser,digest,size,chars}], unchanged, unsupported[{key,suffix,reason,hint}],
oversize[{key,message}], failed[{key,message}], vanished, restored, queued_tasks, pending_total, pending}`。

超过 `ingest.max_source_bytes` 的文件进 `oversize`，不解析；无文本层的进
`unsupported`。两者都不会中止整批摄入。

### next

领取 queued 任务，默认 1 个；`--batch N` 只用于调试或批量运维，`--kind` 可限定类型。
route 任务优先。`next` 是 agent 的唯一取任务入口，返回任务包而不是内部状态：

`data.tasks[]`: `{id, brief, template, work}`。

- `brief`: `{goal, steps[], submit, effect}`，说明做什么、怎么做、提交后发生什么。
- `template`: 已固定结构的载荷；route 为 `decisions[]`，page 为 `page/title/type/
  summary/body/links/evidence/boundaries`。
- `work`: `sources[{key,raw,chars}]`、`page_index[{id,title,type}]`、
  `pending_pages[{id,title,type}]`（已排队、尚未写入的页面）；更新页任务另有
  `work.page{id,rev,meta,body}`。

不再返回 `epoch`、`lease_until`、`attempts`、`last_error` 或原始 `inputs`。

### submit

`submit <id> --payload <file|->`。`--payload` 文件须在根目录内，或传 `-` 从 stdin 读。
校验通过才写入并关闭任务；否则返回 `E_*` 并把任务退回队列，可重新 `next` 后按新任务
再交。`--epoch` 仅保留给旧脚本，日常提交不需要。

载荷应从 `next` 返回的 `template` 直接填写，不要另造字段。

**route 载荷**：

```json
{"decisions": [{"source": "docs/a.md", "action": "new", "page": "refund-process",
                "title": "退款流程", "type": "process", "reason": "完整办理步骤"}]}
```

每个输入来源恰好一条决定，`reason` 每条都必须非空，`action` 三选一：

- `new`：`page`、`title`、`type` 必填；页面不能已存在。
- `merge`：`page` 必填；页面可以已存在，也可以已在 `pending_pages` 里排队
  （包括同一 route 载荷里的 `new`，顺序不限）。若该页已有未完成任务，来源并入该任务。
- `skip`：标记 `skipped`。

**page 载荷**：

```json
{"page": "refund-process", "title": "退款流程", "type": "process",
 "summary": "签收后 7 天内可申请，原路退回。",
 "links": ["refund-policy"],
 "body": "## 结论\n…\n\n## 步骤\n…\n\n## 注意事项\n…",
 "evidence": [{"source": "docs/a.md", "quote": "签收后七日内可申请退款"}],
 "boundaries": {"covered": "线上订单", "not_covered": "线下门店"}}
```

- `body` 或 `body_file` 二选一；最长 60000 字符；不能以 `---` 开头。`body_file` 须在
  根目录内，建议放 `.wiki/drafts/`。
- 新建页面的 `body` 必须保留模板给定的章节标题（如 `## 结论`、`## 步骤`、`## 注意事项`），
  缺章节或改名会被拒绝；更新页面沿用原有结构。
- `summary` 必填，1–200 字符；`title` 1–120 字符；`type` 必须在 `schema.types` 里。
- `links` 只能指向已存在页面。
- `evidence` 至少一条；`quote` 去空白后须逐字命中该来源的 raw 文本。
- 更新页时 `sources` 取旧来源与本任务来源的并集，旧 evidence 未在本次载荷中重写时保留；
  更新任务模板会带出旧 evidence。
- `boundaries.covered`、`boundaries.not_covered` 必填。

成功后 `data`: `{task, page, rev, sources, queued_page_tasks}`；page 的 `sources` 是并集。

### release

`release <id>`：把已领取未提交的任务退回队列，不记录错误。

### read

`read "raw:<key>" [--offset N] [--limit N]` 读解析后的来源文本；
`read "page:<id>"` 读页面正文；也可传根目录内的相对路径。
`data`: `{path, offset, limit, total_lines, next_offset, meta, content}`。

对 `.md`，`read` 先看正文首行：以 `---` 开头就按 frontmatter 解析，`meta` 给出结果，
`content` 只留正文。按路径读取的 `.md`（例如 `SCENARIO.md`）同样如此，开头那段
frontmatter 不再出现在 `content` 里，也不会告警；`raw:` 读的是 raw 副本，返回的同样是
去掉引擎前言后的正文。`page:` 直接返回账本里的元数据与正文，不做切分。

### query

`query <词...> [--limit N]`：按标题、摘要、正文命中数排序，返回页面子集；同时直接扫描
`话题/`，命中时在 `topics` 里返回。`query` **不检索场景**，场景必须显式
`scenario search`。`query --page <id>`：`data.page` 是账本元数据，同层返回 `rev`、
`path`（`.wiki/pages` 镜像路径，不是观看区路径）、`outgoing`、`incoming`、`body`，不查话题。

`data`: `{query, matches, returned, results, topic_matches, topics, boundary}`。
`matches` 只数页面，`topic_matches` 只数话题；`topics[]` 为
`{name, title, description, status, updated, path, read, score, snippet}`。

检索结果是定位工具，不是覆盖证明。

### scenario

`scenario list`：列出 `<root>/scenario/` 与 `<root>/场景能力/` 下的一级场景目录及技能内置场景，
包括结构不合法的项；同一 id 出现在两个目录时只列 `scenario/` 那份，项目场景与内置场景
同 id 时只列项目版。

`data`: `{root, roots, count, valid, scenarios[]}`；`root` 是生效的目录名，`roots` 是实际存在的
场景目录（`scenario/` 在前）；每项为
`{id, source, name, description, triggers, path, main, valid, error, resources[]}`，
`source` 取 `project` 或 `builtin`。`resources[]` 为 `{path, kind, size}`，`kind` 取
`script`（`scripts/` 下）、`reference`（`references/` 下）或 `other`。

`scenario search <词...> [--limit N]`：显式检索场景，返回命中的项目场景与内置场景；
不传词返回 `E_USAGE`。只有显式检索并 `load` 后，才按场景的分支流程执行，避免场景自动
介入普通知识问答。

`data`: `{query, matches, scenarios[]}`；`matches` 只数场景，`scenarios[]` 为
`{id, source, name, description, triggers, path, main, load, score, resources}`，
`source` 取 `project` 或 `builtin`；`load` 是建议的加载命令，`resources` 是配套文件数量。
非法场景不进 `scenario search` 结果。

`scenario load <id>`：加载一个合法场景（项目或内置），返回主文件全文与资源清单。

`data`: `{scenario, path, content, meta, resources}`。`content` 是 `SCENARIO.md`
全文（含 frontmatter），`path` 是主文件的根目录相对路径，`resources` 同上。内置场景的
`path` 是展示标签 `builtin/<id>/SCENARIO.md`，不是磁盘路径；其 `resources` 恒为空。

三个命令都只读：不创建 `.wiki/`，不写账本，不要求先 `init` 或 `ingest`。场景文件改动后
下一次 `scenario search` / `scenario load` 立即生效。`id` 必须是 `scenario/` 或 `场景能力/` 下的一级目录名
（或内置场景 id），包含路径分隔符或试图跳出目录返回 `E_USAGE`；场景不存在返回
`E_NOT_FOUND`；缺 `SCENARIO.md`、缺 `description`、正文为空、文件过大或场景目录/主文件
指向区外返回 `E_SCENARIO_INVALID`。`scenario list` 不因个别场景不合法而失败。

### page

- `page import <id> --from <file>`：采用人工编辑的页面文件（可带 frontmatter），
  生成新修订（`origin=import`），不强制 evidence。只允许已存在的页面。
- `page restore <id> --rev N`：把第 N 版内容写成新修订（`origin=restore`）。该页在观看区
  有未收割手改时，推送跳过它，恢复内容不会出现；此时跑 `sync` 反而会把那份手改收成更新一版
  并盖过 restore。先 `sync` 处理手改，再 `restore`。

### export

`export <dir>`：把所有页面按 `type/id.md` 写到目标目录，用于离线浏览或迁移。目标目录
必须在工作区内，建议 `.wiki/export`。

### status

`status [--brief]`。`data`: `{created, sources{状态:数量}, source_total, pages,
stale_pages, pending, tasks{open,queued,leased,route,page,expired,accepted}, next}`。
`--brief` 只返回 `brief` 一行摘要，没有 `tasks`；完整数量（含 `expired`、`accepted`）用
`status --json`，不要加 `--brief`。

### sync

`sync [--body-only]`：把观看区 `<view>/pages/**/*.md` 的手工修改回收进账本（新修订
`origin=sync`），只回收、不推送。它针对观看区，不是 `.wiki/pages/` 镜像；镜像被改仍按
drift 处理。手改未收割期间该页整体冻结：推送不覆盖、也不按新标题改名，`page import` 的
改名要到回收之后才在观看区出现。

- 文件没有 `id` 或 `id` 不在账本里：跳过，`reason` 分别为 `no id`、`unknown id`；路径逃出
  观看区（例如符号链接指向外面）时记为 `escapes the publish dir`。
- 同一 `id` 出现在多个文件：每个文件都记 `E_DUPLICATE`，不回收。
- frontmatter 的 `title`、`type`、`summary`、`sources` 与账本不一致：该文件记
  `E_META_CHANGED`；加 `--body-only` 只取正文与证据，保留账本元数据（类型、来源、标题等）。
  `status`、`updated`、`aliases` 不参与比较，成功回收后按账本重写；`id` 改了会按
  `unknown id` 跳过。
- 正文为空 `E_PAYLOAD`；证据引用未登记来源 `E_UNKNOWN_SOURCE`；文件读不出 `E_IO`。
- `## 证据` 段存在但格式不合法：整段保留为正文并在 stderr 警告，账本原证据不变；同一条
  警告也会追加到 `data.warnings`。最常见的情形是在证据块后面追加一行——整块会被当正文，
  规范化重写后正文里会多出一个 `## 证据` 段。
- 回收成功后把规范化文本写回观看区；只是位置或字节与上次发布一致的文件记为 `written`，
  不产生新修订。

`data`: `{imported[page], skipped[{file,reason}], failed[{file,page,code,message}],
unchanged[page], written[{page,from,to}], warnings[message]}`。有失败项时 `ok=false`，
退出码 1；`warnings` 不改变成功状态。

`sync` 之后，下一条会启动引擎的命令启动时才把账本推回观看区。

### doctor

`doctor [--fix]`：`PRAGMA integrity_check`、镜像修复、stale 页、未注册来源、
断链、过期租约。`--fix` 重建链接索引与镜像。`data`: `{issues[{severity,code,message}],
fixed, healthy}`；有 `fail` 级问题时退出码 1。`doctor` 不检查观看区：未收割的手改只在
推送时告警，`healthy:true` 不代表观看区没有待回收内容，用 `sync` 收割。

## 错误码

| 码 | 含义 | 处理 |
| --- | --- | --- |
| `E_USAGE` | 参数缺失或组合非法 | 看 `error.next`，不要重试同一载荷 |
| `E_PAYLOAD` | 载荷字段缺失/类型/长度不合法 | 按 message 修正字段 |
| `E_TASK_UNKNOWN` | 任务 id 不存在 | `next` 取新任务 |
| `E_TASK_STALE` | 任务已关闭或租约过期 | 重新 `next`，丢弃旧载荷；来源变化导致的过期是 `E_SOURCE_DRIFT` |
| `E_TASK_SCOPE` | 载荷引用了任务范围外的来源 | 只用任务 `work.sources` 里的 key |
| `E_TASK_CONFLICT` | 来源已属于另一棵未关闭的页面任务树 | 先完成或关闭那个页面任务 |
| `E_STATE` | 状态不满足：账本结构过旧、观看区路径不是目录、页面正被其他任务写 | 按 message 处理，不要手改账本 |
| `E_META_CHANGED` | `sync` 时观看区页面的 `title`/`type`/`summary`/`sources` 与账本不一致 | 只收正文加 `--body-only`；确实要改元数据用 `page import`（手改未收割时改名会被冻结） |
| `E_DUPLICATE` | `sync` 时同一页面 id 在观看区出现两次 | 删掉重复文件后重跑 |
| `E_PAGE_CONFLICT` | 页面已存在（new）或版本已变（update） | 重新领任务，基于当前 rev 重写 |
| `E_PAGE_UNKNOWN` | merge 指向不存在的页面 | 改用 new，或查 `page_index` / `pending_pages` |
| `E_PAGE_GONE` | update 时页面消失 | 重新 `next` |
| `E_BAD_ID` | 页面 slug 非法 | 用小写字母数字与 `-_.` |
| `E_LINK_UNRESOLVED` | 链接目标不存在 | 先建目标页，或去掉链接 |
| `E_EVIDENCE_NOT_FOUND` | 引文不在来源原文里 | 重读 raw，逐字复制 |
| `E_SOURCE_DRIFT` | 来源在任务领取后被改动或删除 | 重新 `next`，重读来源 |
| `E_UNKNOWN_SOURCE` | 页面引用未登记的来源 | 先 `ingest` |
| `E_EMPTY` | 还没有任何来源 | 先 `ingest` |
| `E_NOT_FOUND` | 页面、版本、场景或文件不存在 | 用 `query`/`read`/`scenario list` 定位 |
| `E_PATH` | 路径越出工作区根目录 | 用工作区内路径；资料在区外时切换工作区 |
| `E_CONFIG` | `.wiki/wiki.json` 非法 | 按提示修正 JSON |
| `E_SCENARIO_INVALID` | 场景缺主文件、`description`、正文，或超过 200000 字符 | 按 `scenario list` 的 `error` 修正场景目录 |
| `E_SOURCE_TOO_LARGE` | 单文件超过 `max_source_bytes` | 调高阈值或拆分来源 |
| `E_DB` | 数据库不可读 | 从备份恢复，不要手改 |
| `E_HEALTH` | 安装自检未通过 | 运行 `check --json`；缺文件时重装技能 |

## 配置

`.wiki/wiki.json`（首次运行生成）：

```json
{"schema": {"types": ["concept", "entity", "process", "note"]},
 "scan": {"ignore": [".git", "node_modules", "target", "dist"]},
 "ingest": {"max_source_bytes": 50000000},
 "publish": {"dir": "wiki", "enabled": true, "topics": true, "scenarios": true}}
```

改 `schema.types` 会立即影响后续校验；已存在的页面不会被自动迁移。

`publish.dir` 是观看区目录名，必须是单个目录名：不能以点开头、不能含路径分隔符，也不能
是 `.wiki`、`话题`、`scenario`、`场景能力`；非法值即使 `enabled:false` 也返回 `E_CONFIG`。
`enabled:false` 只停推送，`<view>/` 与 `<view>/raw/` 仍会创建；`topics`、`scenarios`
控制首次推送时是否投放对应目录。
