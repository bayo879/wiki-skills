# CLI 契约

所有命令以会话工作区为根目录，在当前目录执行。`--root` 可选，只能是当前目录或它下面的
子目录；指到工作区外返回 `E_PATH`。除 `version` 外都接受 `--json`。引擎只依赖 Python
标准库。

除 `check`、`version`、`workflow` 外，命令启动时会准备 `.wiki/` 与观看区 `<view>/`
（`publish.dir`，默认 `wiki/`）：缺的目录、缺的 `wiki.json`、缺的账本表当场补齐，补了什么
列在 `data.repaired`（路径数组，和 `created` 同形），并按账本推送一次；`sync` 例外，它只
回收、不推送。

## 输出信封

成功：

```json
{"ok": true, "cmd": "next", "version": "0.4.3", "root": "/abs/root",
 "summary": "1 task(s) ready; read each brief, fill its payload.json, then submit",
 "data": {…}}
```

失败：

```json
{"ok": false, "cmd": "submit", "version": "0.4.3", "root": "/abs/root",
 "error": {"code": "E_EVIDENCE_NOT_FOUND", "message": "quote not found in docs/a.md: '…'",
           "next": "re-read the raw source and copy an exact sentence"}}
```

致命错误（`E_HEALTH`、`E_STATE`、`E_CONFIG`、`E_DB`、`E_PATH`）的错误对象另带两项：
`doc` 是恢复手册的绝对路径（`references/recovery.md`），`section` 是手册里的对应小节标题。
先按 `next` 处理，不够再读那一节。

不加 `--json` 时输出人类可读文本，错误写 stderr。

退出码：`0` 成功；`1` 有阻断项（解析失败、超限、数据库损坏）；`2` 用法或载荷错误；
`3` 状态不满足（任务过期、来源漂移、页面冲突、旧账本）；`4` 工作区被另一个进程占用。

## 语义模型

- 来源（source）：扫描到的文件，键是相对根目录的路径。状态 `pending`、`done`、
  `skipped`、`unsupported`、`oversize`、`missing`。
- 任务（task）：`route`（每个 raw 一条，同一份正文的多个路径合成一条）与 `page`（每页一条）。状态
  `queued → leased → accepted`；另有过期终态 `expired`，保留行供审计，不再领取。
  提交被拒时回到 `queued` 并记录 `last_error`。
- 页面（page）：多版修订，每版有 `rev`、`meta`、`body`、`digest`。当前状态
  `stable` 或 `stale`（引用来源变化或缺失）。
- 页面按账本推送到观看区 `<view>/pages/<type>/<标题>.md`；`.wiki/` 里不存页面正文副本。
  观看区不是事实源，事实源是账本。

## 工作区边界

- 根目录默认是当前工作目录；`--root` 只允许当前目录或它的子目录，不能指到区外。
- 下列参数解析后必须落在根目录内，否则返回 `E_PATH`：`--payload`、
  `body_file`、`page import --from`、`export <dir>`。
- 草稿、任务目录、导出与临时文件放 `<root>/.wiki/` 下（`.wiki/drafts/`、
  `.wiki/tasks/<id>/`、`.wiki/export`、`.wiki/tmp/`）。`--dry-run` 的临时库在 `.wiki/tmp/`，
  退出时清理。
- 会写账本或观看区的命令（`init`、`ingest`、`next`、`submit`、`release`、`export`、
  `status`、`sync`、`doctor`、`page import`、`page restore`、`page link`，以及
  `query --task`）持有一把工作区文件锁 `.wiki/lock`；普通 `query` / `query --page`
  只读账本，不取锁、不创建 `.wiki/lock`。锁被另一个存活进程占着时排队等待 5 秒，
  仍拿不到返回 `E_BUSY`（退出码 4）。锁文件由临时名写入持有者 pid 后用 `os.link` 原子
  发布，出现时即带 pid；读不出 pid 的锁先排队 10 秒再按陈旧处理。
  进程崩溃留下的锁（记录 pid 已不存在，或超过 30 分钟）会被下一个命令接管；锁路径不是
  普通文件、陈旧锁删不掉时快速返回 `E_STATE`（退出码 3），`.wiki` 不可写时返回
  `E_HEALTH`（退出码 1），不会无限等待。

## 命令

参数、字段与错误码按命令族分页，公共约定（输出信封、语义模型、工作区边界、错误码）留在本页。

| 页面 | 命令 | 用途 |
| --- | --- | --- |
| [任务流](cli/tasks.md) | `next`、`submit`、`release` | 领取任务、提交结果、放回队列 |
| [内容与检索](cli/content.md) | `ingest`、`query`、`workflow`、`page` | 摄入来源、检索候选、加载工作流、维护页面 |
| [工作区与安装](cli/workspace.md) | `check`、`init`、`export`、`status`、`sync`、`doctor` | 自检、初始化、导出、状态、回收与诊断；含 `.wiki/config.json` |

## 错误码

| 码 | 含义 | 处理 |
| --- | --- | --- |
| `E_USAGE` | 参数缺失或组合非法 | 看 `error.next`，不要重试同一载荷 |
| `E_NO_QUERY` | route 提交前没有登记任何检索 | 先 `query --task <id> 词...`，再 `next` 领回任务提交 |
| `E_QUERY_BUDGET` | 登记的主题词超出「5 个/1000 字」配额 | 合并近义词，只留能区分页面的词 |
| `E_PAYLOAD` | 载荷字段缺失/类型/长度不合法 | 按 message 修正字段 |
| `E_TASK_UNKNOWN` | 任务 id 不存在 | `next` 取新任务 |
| `E_TASK_UNSUBMITTED` | 有已领取但没提交的任务 | 按 `error.next` 先 `submit <id>`（`payload.json` 还在任务目录里）或 `release <id>`，再 `next` |
| `E_INGEST_BUSY` | 另一个会话持有租约、正占着队列（`next`、`ingest`） | 立刻停止，不重试、不等待；见「队列被别的会话占用」 |
| `E_TASK_FOREIGN` | 任务的租约记在别的会话名下 | 不要提交或抢做，交给领取它的会话；确认对方停了再由用户授权 `release` |
| `E_TASK_STALE` | 任务已关闭或租约过期 | 重新 `next`，丢弃旧载荷；来源变化导致的过期是 `E_SOURCE_DRIFT` |
| `E_TASK_SCOPE` | 载荷引用了任务范围外的来源 | 只用任务 `work.sources` 里的 key |
| `E_TASK_CONFLICT` | 来源已属于另一棵未关闭的页面任务树（同一次 route 拆页除外） | 先完成或关闭那个页面任务；拆页请放在同一次 route 提交里 |
| `E_STATE` | 状态不满足：未初始化（`query` 只读）、旧账本（有数据的旧结构）、观看区路径不是目录、页面正被其他任务写 | 未初始化先 `ingest`；旧库按恢复手册「旧账本」：备份后删 `.wiki/` 重新 `ingest`；其余按 message 处理，不要手改账本 |
| `E_META_CHANGED` | `sync` 时观看区页面的 `title`/`type`/`summary`/`sources` 与账本不一致 | 只收正文加 `--body-only`；确实要改元数据用 `page import`（手改未收割时改名会被冻结） |
| `E_DUPLICATE` | `sync` 时同一页面 id 在观看区出现两次 | 删掉重复文件后重跑 |
| `E_PAGE_CONFLICT` | 页面已存在（new）或版本已变（update） | 重新领任务，基于当前 rev 重写 |
| `E_PAGE_UNKNOWN` | merge 指向不存在的页面 | 改用 new，或查 `page_index` / `pending_pages` |
| `E_PAGE_GONE` | update 时页面消失 | 重新 `next` |
| `E_BAD_ID` | 页面 slug 非法 | 用小写字母数字与 `-_.` |
| `E_LINK_UNRESOLVED` | 链接目标不存在 | 先建目标页，或去掉链接；已有页补链用 `page link` |
| `E_EVIDENCE_NOT_FOUND` | 引文（含保留的旧引文）不在来源原文里 | 重读 raw，逐字复制 |
| `E_SOURCE_DRIFT` | 来源在任务领取后被改动或删除 | 重新 `next`，重读来源 |
| `E_UNKNOWN_SOURCE` | 页面引用未登记的来源 | 先 `ingest` |
| `E_EMPTY` | 还没有任何来源 | 先 `ingest` |
| `E_NOT_FOUND` | 页面、版本或工作流不存在 | 用 `query`/`workflow list` 定位 |
| `E_PATH` | 路径越出工作区根目录 | 用工作区内路径；资料在区外时切换工作区 |
| `E_CONFIG` | `.wiki/wiki.json` 非法，或 `publish.dir` 不合规 | 修正 JSON，或删掉让它按默认重建；见「配置损坏」 |
| `E_WORKFLOW_INVALID` | 工作流缺主文件、`description`、正文，超过 200000 字符，或步骤里没有 `tool:` | 按 `workflow list` 的 `error` 修正工作流目录 |
| `E_SOURCE_TOO_LARGE` | 单文件超过 `max_source_bytes` | 调高阈值或拆分来源 |
| `E_DB` | 账本打不开或完整性检查失败 | 停写，从备份恢复整个 `.wiki/`；见「账本损坏」 |
| `E_HEALTH` | 安装检查未通过：解释器太旧、技能文件缺失、工作区不可写 | 按 `error.next`；见「运行时与安装」 |
| `E_BUSY` | 另一个 wiki 进程正占着工作区文件锁 `.wiki/lock` | 等它结束再重试；确认它已崩溃才删锁 |
