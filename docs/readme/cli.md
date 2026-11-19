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
- 会写账本或观看区的命令（`init`、`ingest`、`next`、`submit`、`release`、`query`、
  `export`、`status`、`sync`、`doctor`、`page import`、`page restore`、`page link`）持有一把工作区文件锁
  `.wiki/lock`；锁被另一个存活进程占着时排队等待 5 秒，仍拿不到返回 `E_BUSY`（退出码 4）。
  进程崩溃留下的锁（记录 pid 已不存在，或超过 30 分钟）会被下一个命令接管。

## 命令

### check

只读诊断：Python 3.12+、技能必需文件与模板、工作流与手册文件、工作区可写、已有账本可读。
不创建 `.wiki/`，不读写页面内容，也不修任何东西。`data`: `{ok, doc, checks[{name,ok,detail}]}`；
`doc` 是恢复手册路径，失败时用它找到对应小节；`ok` 与退出码一致（`0` 通过，`1` 有失败项）。
账本项可以给出 `repairable`（结构能补，例如有 `wiki.json` 却缺 `wiki.db`）：仍算通过，
`detail` 以 `repairable:` 开头，下一条打开账本的命令会补上。

`check` 不是前置门禁：`init`、`ingest`、`next`、`submit`、`release` 只做安装级检查
（解释器、技能文件、工作区可写），失败返回 `E_HEALTH`；账本缺失或残缺在打开账本时自愈。
完整回归测试在仓库的 `tests/selftest.py`，不随技能分发。

### init

创建 `.wiki/`（`wiki.db`、`wiki.json`、`tasks/`、`drafts/`），并准备
观看区 `<view>/` 与 `<view>/raw/`。不是前置步骤：任何打开账本的命令都会补同样的东西；
只建库、暂不摄入时才单独跑。`data`: `{config, pages, sources, open_tasks}`，本次补出的
路径在 `data.repaired`。

### ingest

扫描根目录（跳过隐藏目录、`scan.ignore`、`workflows/`、`topics/` 与观看区 `<view>/`），解析
新文件或内容变化的文件，写入 `<view>/raw/`（默认 `wiki/raw/`），并为每个新来源入队任务，
同一份正文的多个路径合成一条 route 任务。`<view>/raw` 被手改时按整份正文采纳：共享它的
路径一起换摘要，文件留在原名原路径。`workflows/` 放可执行工作流，`topics/` 是 wiki 的探索
层，都不作为摄入来源，摄入时整目录跳过；观看区目录名即使关了发布也仍跳过。工作流与话题
一样由 `query` 直接扫描文件、返回候选；显式 `workflow search` 也可用。

`--dry-run` 用 `.wiki/tmp/` 下的临时库，退出时清理，不写账本；它只列 `would_ingest`，不预告
末尾补偿入队会补哪些来源。`--force` 忽略哈希重新解析。`--only` 按根目录相对 key
精确匹配，不校验参数形状：传绝对路径或含 `..` 的值只会匹配不到来源，命令仍成功退出 0，
也不会摄入任何文件。

`data`: `{ingested[{key,raw,parser,digest,size,chars}], unchanged, unsupported[{key,suffix,reason,hint}],
oversize[{key,message}], failed[{key,message}], vanished, restored, queued_tasks,
pending_total, pending, queue{open,queued,leased,route,page}}`。`queued_tasks` 只列本次
新入队的任务 id；`queue` 是提交后的未完成任务快照，`open` 为 `queued` 与 `leased` 之和。
没有新来源时 `queue.open` 可以为 0，此时再跑 `next` 只会得到空任务列表。

超过 `ingest.max_source_bytes` 的文件进 `oversize`，不解析；无文本层的进
`unsupported`。两者都不会中止整批摄入。

别的会话正持有租约时 `ingest` 不动账本，直接报 `E_INGEST_BUSY`：两个执行体一起改任务目录
和 `.wiki/tmp` 正是载荷被写串的入口。`--dry-run` 用临时库，不受这条限制。

### next

领取 queued 任务，默认 1 个；`--batch N` 只用于调试或批量运维，`--kind` 可限定类型。
按任务树深度优先，一批只发一棵树：一棵来源树拆出的页面任务没做完，就不发新来源；一批里
不会同时出现 route 任务和页面任务，页面任务只从当前这棵树取，树没做完时别的来源排在后面。
批次没填满时用同一棵树里已领但未提交的任务补位，不拿别的树凑数——所以 `--batch 2` 在只剩
1 个待写页面时返回 1 个，而不是混进下一份正文。同一秒入队的兄弟页按入队顺序发放，和
route 里的书写顺序一致。
`next` 是 agent 的唯一取任务入口，返回任务包而不是内部状态：

`data.tasks[]`: `{id, brief, template, work}`。

同时把任务物化到 `.wiki/tasks/<id>/`：`brief.md`（同 `brief` 的可读版）、`work.json`（同
`work`）、`payload.json`（同 `template`，agent 在这里填空）。目录是任务的素材，不是队列：
只有 `next` 发放任务，任务结束就删目录，残留目录在下一次 `next` 或 `ingest` 时清掉。

- `brief`: `{goal, steps[], submit, effect}`，说明做什么、怎么做、提交后发生什么。
- `template`: 已固定结构的载荷；route 为 `decisions[]`，page 为 `page/title/type/
  summary/body/links/evidence/boundaries`。
- `work`: `sources[{key,raw,chars}]`、`page_index[{id,title,type,summary,outgoing,incoming}]`、
  `pending_pages[{id,title,type}]`（已排队、尚未写入的页面）、`tree`（任务所属树的 id：
  同一次 route 拆出的页面任务相同，route 任务是自己）、
  `queries{budget,used,remaining,log}`（route 任务的检索配额与查询日志位置）；更新页任务另有
  `work.page{id,rev,meta,body}`。

不再返回 `epoch`、`lease_until`、`attempts`、`last_error` 或原始 `inputs`。

一次只做一件事：只要有已领取（`leased`）而未提交的任务，`next` 直接失败并报
`E_TASK_UNSUBMITTED`，`message` 列出任务 id，`error.next` 给出 `submit <id> --payload
.wiki/tasks/<id>/payload.json` 与 `release <id>`。不再重发同一任务，也不续租、不加 epoch：
已经领到手的任务继续做，会话重启后按错误里的路径接着填即可。`--kind` 与 `--batch` 同样受
这条门禁限制——手头没结束，就不发任何新任务。只有队列与租约都空时才返回 `no queued tasks`。

队列只认一个会话：`--owner <id>` 记下领取方，缺省依次取 `$LLM_WIKI_OWNER`、`$DSH_SESSION_ID`
（宿主给每个会话注入，子智能体各不相同），都没有就不记归属；`data.owner` 回显该值，
`status` 的 `leases` 列出当前租约。别的会话持有租约时 `next` 与 `ingest` 都失败报
`E_INGEST_BUSY`，`message` 指出占用方和任务 id，要求停止而不是重试；同一个会话自己持有仍是
`E_TASK_UNSUBMITTED`。租约过期（`TASK_LEASE_SECONDS`，默认 900 秒）后由下一次 `next`
收回，死会话的租约不会一直占着队列。

### submit

`submit <id>`。默认读 `.wiki/tasks/<id>/payload.json`；`--payload <file|->` 可改读别的
文件（须在根目录内）或从 stdin 读。校验通过才写入并关闭任务；否则返回 `E_*` 并把任务
退回队列，可重新 `next` 后按新任务再交。`--epoch` 仅保留给旧脚本，日常提交不需要。
任务记在别的会话名下时不写账本，直接报 `E_TASK_FOREIGN`。

载荷应从 `next` 落到任务目录的 `payload.json` 直接填写，不要另造字段。任务结束（提交成功
或过期）后目录被删除；任务被释放后重新 `next` 会刷新 `brief.md` 与 `work.json`，不覆盖
已填的 `payload.json`。

**route 载荷**：

```json
{"decisions": [
  {"source": "docs/a.md", "action": "new", "page": "refund-policy",
   "title": "退款政策", "type": "concept", "reason": "政策口径可独立检索"},
  {"source": "docs/a.md", "action": "new", "page": "refund-process",
   "title": "退款流程", "type": "process", "reason": "完整办理步骤",
   "links": ["refund-policy"]}]}
```

每份正文至少一条决定，`source` 填它的任一挂载路径（同一份正文可能挂在多个路径上），
这条决定覆盖它的全部路径；`reason` 每条都必须非空，`action` 三选一。一份正文含多个
可独立检索的主题时，提交多条决定指向不同页面；同一页面不得重复指向，
`skip` 不能与 `new`/`merge` 并用：

- `new`：`page`、`title`、`type` 必填；页面不能已存在，同一载荷里同一个页面只能
  `new` 一次。
- `merge`：`page` 必填；页面可以已存在，也可以已在 `pending_pages` 里排队
  （包括同一 route 载荷里的 `new`，顺序不限）。若该页已有未完成任务，来源并入该任务。
- `skip`：把这份正文仍 `pending` 的挂载路径标记 `skipped`。

decision 的 `links` 声明这页与哪些页面相关；写页前应逐条判断，确实没有相关页就写空数组。
目标只能是提交时已存在的页，或同一载荷里 `new` 的兄弟页；兄弟页要等被链页面先提交，顺序
错会报 `E_LINK_UNRESOLVED`，重新 `next` 后按顺序重试。`new` 的 `links` 原样进入 page 模板；
`merge` 的 `links` 是增量，提交时与页面已有链接取并集。

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
- `links` 只能指向提交时已存在的页；更新页取模板带入的旧链接与本次新增的并集，只增不删。
  `body` 里不要写 `## 关联`，引擎会剥掉；观看区按 `links` 表在页末渲染出链与入链。
- `evidence` 至少一条；`quote` 去空白后须逐字命中该来源的 raw 文本。
- 更新页时 `sources` 取旧来源与本任务来源的并集，旧 evidence 未在本次载荷中重写时保留；
  更新任务模板会带出旧 evidence。保留下来的旧引文同样逐字复核：来源改写后原句已不在
  raw 里时，提交报 `E_EVIDENCE_NOT_FOUND`，需在本次载荷里为该来源补一条新引文；来源已
  不可用（没有 raw）时无从比对，原样保留。
- `boundaries.covered`、`boundaries.not_covered` 必填。

成功后 `data`: `{task, page, rev, sources, queued_page_tasks}`；page 的 `sources` 是并集。

### release

`release <id>`：把已领取未提交的任务退回队列，不记录错误。任何会话都能释放（给运维留的
恢复口子），`data.owner` 给出原持有方，`summary` 末尾带 `(was leased to <id>)`；原本没记
归属时为空串。

### query

`query <词...> [--limit N] [--task <id>]`：按标题、摘要、正文命中数排序，返回页面子集；同时直接扫描
`topics/` 与 `workflows/`，命中时分别在 `topics`、`workflows` 里返回。工作流候选最多 5 个，
每条带 `applies`，但不会自动加载；是否按流程办事由调用方核对后显式 `workflow load`。
`query --page <id>`：`data.page` 是账本元数据，同层返回 `rev`、
`path`（观看区路径；发布关闭时退回 `page:<id>` 引用）、`sources`（`[{key, raw}]`，`raw`
是账本渲染的观看区副本路径，源缺失时为 `null`）、`outgoing`、`incoming`、`body`，不查话题。

`data`: `{query, matches, returned, results, topic_matches, topics, workflow_matches, workflows,
boundary}`。`matches` 只数页面，`topic_matches` 只数话题，`workflow_matches` 只数工作流；
`topics[]` 为 `{name, title, description, status, updated, path, score, snippet}`，按 `path`
直接打开文件；`workflows[]` 的字段与 `workflow search` 的候选相同。

`query --task <id> 词1 词2 ...`：把这次检索登记到任务上，route 任务提交前必须至少登记一次，
否则 `submit` 报 `E_NO_QUERY`。每个词单独检索、各返上限 5 条，按词分组返回，不并成一次
排序；这次检索只查页面，不带工作流候选。重复的词不重复计配额。配额是 5 个词/1000 字（正文不足 1000 字也给 5 个），超出报
`E_QUERY_BUDGET`。登记要求任务归本会话：别的会话持有的任务报 `E_TASK_FOREIGN`。每次调用
追加一行到 `.wiki/tasks/<id>/queries.jsonl`（`{ts,words,hits}`），
任务关闭时随目录一起删除，不保留审计历史。`data`:
`{task, queries[{word,matches,returned,results}], budget, boundary}`。

检索结果是定位工具，不是覆盖证明。

### workflow

`workflow list`：列出 `<root>/workflows/` 下的一级工作流目录及技能内置工作流，包括结构
不合法的项；项目工作流与内置工作流同 id 时只列项目版。

`data`: `{root, roots, count, valid, warned, workflows[]}`；`root` 是工作流目录名，
`roots` 是实际存在的工作流目录（只有 `workflows/`），`warned` 是带 warning 的项数；每项为
`{id, source, name, alias_name, description, tags, path, main, valid, error, resources,
tools, human_steps, agent_steps, verify_steps, warnings[]}`。`tools`、`human_steps` 等是
各类型步骤的数量，`warnings[]` 为 `{code, line, text, message}`；`name` 是工作流机器名、
`alias_name` 是可选的中文展示名（人类可读行优先显示它）；`tags` 按 Obsidian 规则规范化
（去 `#`、空白切分、保留 `a/b` 层级、丢弃纯数字），`source` 取 `project` 或 `builtin`。
`resources[]` 为 `{path, kind, size}`，`kind` 取 `script`（`scripts/` 下）、`reference`
（`references/` 下）或 `other`。

`workflow search <词...> [--limit N]`：显式检索工作流，返回命中的项目工作流与内置工作流；
最多 5 个，按分数排序；不传词返回 `E_USAGE`。候选带 `applies`（`## 适用` 原文），0 命中时
data 带 `message: "无可用workflow"`。只有核对 `applies` 并 `load` 后，才按工作流的分支流程
执行；候选不会自动加载。

`data`: `{query, matches, workflows[], message?}`；`matches` 只数工作流，`workflows[]` 为
`{id, source, name, alias_name, description, tags, applies, path, main, load, score, resources,
tools, human_steps, verify_steps, warnings}`，`load` 是建议的加载命令；0 命中时 `message` 为
`"无可用workflow"`。非法工作流不进 `workflow search` 结果。

`workflow load <id>`：加载一个合法工作流（项目或内置），返回主文件全文、资源清单与按类型
分好的步骤。

`data`: `{workflow, path, content, meta, resources, tools, human_steps, agent_steps,
verify_steps, warnings}`。`content` 是 `WORKFLOW.md` 全文（含 frontmatter），`path` 是主
文件的根目录相对路径。`tools`、
`human_steps`、`agent_steps`、`verify_steps` 每项为 `{line, text}`，`warnings[]` 同上。
内置工作流的 `path` 是展示标签 `builtin/<id>/WORKFLOW.md`，不是磁盘路径；其 `resources`
恒为空。

三个命令都只读：不创建 `.wiki/`，不写账本，不要求先 `init` 或 `ingest`。工作流文件改动后
下一次 `workflow list` / `workflow load` 立即生效。`id` 必须是工作流目录下的一级目录名
（或内置 id），包含路径分隔符或试图跳出目录返回 `E_USAGE`；工作流不存在返回
`E_NOT_FOUND`；缺 `WORKFLOW.md`、缺 `description`、正文为空、文件过大、工作流目录或主
文件指向区外，或步骤里没有 `tool:` 步骤，返回 `E_WORKFLOW_INVALID`。没有 `## 步骤`、
这一节没有列表项、或整份没有任何类型标记的工作流在 0.5.0 仍可加载但带 `warnings`，
`0.6.0` 起返回 `E_WORKFLOW_INVALID`。`workflow list` 不因个别工作流不合法而失败。

### page

- `page import <id> --from <file>`：采用人工编辑的页面文件（可带 frontmatter），
  生成新修订（`origin=import`），不强制 evidence。只允许已存在的页面。frontmatter 的
  `title/type/summary/sources/links` 会并入；`links` 必须是块列表，与旧链接取并集，
  未知目标报 `E_LINK_UNRESOLVED`；`aliases/status/updated` 忽略。
- `page link <id> <target...>`：给已存在页面补关联，生成新修订（`origin=link`），只增不删。
  目标必须是已存在页；未知页报 `E_LINK_UNRESOLVED`，空目标或指向自己报 `E_USAGE`；已链过
  的目标不产生新修订。成功后立即推送观看区，告警写 stderr。单页模式可加 `--dry-run`
  只预览、不写修订。`data`: `{mode:"single", dry_run, page, rev, links, added, view}`，
  预览时没有 `links`/`view`。
- `page link --repair [--dry-run]`：按“共享同一 raw 来源”推断缺失的边。同一来源下的页
  两两配对，已有任一方向的边就跳过，每对只加一条（页 id 小的指向大的，观看区两边都会
  显示）；不传页面 id 和目标。`--dry-run` 只输出计划。`data`:
  `{mode:"repair", dry_run, rules:["shared-source"], edges, pages, view}`，`pages` 每项为
  `{page, rev, added}`。只补不删，不改变已有边的方向。
- `page restore <id> --rev N`：把第 N 版内容写成新修订（`origin=restore`）。该页在观看区
  有未收割手改时，推送跳过它，恢复内容不会出现；此时跑 `sync` 反而会把那份手改收成更新一版
  并盖过 restore。先 `sync` 处理手改，再 `restore`。

### export

`export <dir>`：把所有页面按 `type/id.md` 写到目标目录，用于离线浏览或迁移。目标目录
必须在工作区内，建议 `.wiki/export`。

### status

`status [--brief]`。`data`: `{created, sources{状态:数量}, source_total, pages,
stale_pages, pending, tasks{open,queued,leased,route,page,expired,accepted}, leases,
next}`。`leases` 是 `[{task, owner}]`，列出当前租约及其归属会话；`next` 在别人持租约时
给出 `stop: … is already ingesting`，而不是让你去提交。
`--brief` 只返回 `brief` 一行摘要，没有 `tasks`；完整数量（含 `expired`、`accepted`）用
`status --json`，不要加 `--brief`。

### sync

`sync [--body-only]`：把观看区 `<view>/pages/**/*.md` 的手工修改回收进账本（新修订
`origin=sync`），只回收、不推送。页面正文只存在于账本与观看区两处，没有第二份镜像。
手改未收割期间该页整体冻结：推送不覆盖、也不按新标题改名，`page import` 的
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

`doctor [--fix]`：`PRAGMA integrity_check`、stale 页、未注册来源、
无可用来源的页（`sources` 为空或全部缺失/失效，L7；其中零入零出的标 `removable`，
doctor 直接删除，不新增命令、不逐页确认；有来源的零出链页不报）、过期租约。断链不再检查：
links 表只由通过 `check_links` 的提交写入，没有留下悬空 `dst` 的路径。
`--fix` 重建链接索引。`data`: `{issues[{severity,code,message}],
fixed, healthy}`；有 `fail` 级问题时退出码 1。`doctor` 不检查观看区：未收割的手改只在
推送时告警，`healthy:true` 不代表观看区没有待回收内容，用 `sync` 收割。

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
| `E_STATE` | 状态不满足：旧账本（有数据的旧结构）、观看区路径不是目录、页面正被其他任务写 | 旧库按恢复手册「旧账本」：备份后删 `.wiki/` 重新 `ingest`；其余按 message 处理，不要手改账本 |
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

## 配置

`.wiki/wiki.json`（首次运行生成）：

```json
{"schema": {"types": ["concept", "entity", "process"]},
 "scan": {"ignore": [".git", "node_modules", "target", "dist"]},
 "ingest": {"max_source_bytes": 50000000},
 "publish": {"dir": "wiki", "enabled": true, "topics": true, "workflows": true}}
```

改 `schema.types` 会立即影响后续校验；已存在的页面不会被自动迁移。老库配置里的 `note`
（连同 `wiki.json` 里显式列出的其他类型）继续可用：校验、章节骨架与推送都按原样走，
只是新建页面不要再选它；要收口就人工把这些页并进 `concept`/`process`，再把它从
`schema.types` 里删掉。

`publish.dir` 是观看区目录名，必须是单个目录名：不能以点开头、不能含路径分隔符，也不能
是 `.wiki`、`话题`、`workflows`；非法值即使 `enabled:false` 也返回 `E_CONFIG`。
`enabled:false` 只停推送，`<view>/` 与 `<view>/raw/` 仍会创建；`topics`、`workflows`
控制首次推送时是否投放对应目录。
