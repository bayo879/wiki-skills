# CLI 契约：任务流

`next`、`submit`、`release`：领任务、提交与放回。

返回 [CLI 契约](../cli.md)；所有命令以会话工作区为根目录。

## next

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

## submit

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
  `body` 里不要写 `## 关联`，引擎会剥掉；观看区按 `links` 表在页末渲染出链与入链，
每行是 `[目标页标题](相对路径)`。
- `evidence` 至少一条；`source` 写来源 key（如 `docs/a.md`），不要写链接或 raw 路径；
  `quote` 去空白后须逐字命中该来源的 raw 文本。观看区渲染时引擎会把证据行写成
  ``- [`<raw 路径>`](<相对链接>)（`<来源 key>`）：<引文>``，链到 raw 副本。
- 更新页时 `sources` 取旧来源与本任务来源的并集，旧 evidence 未在本次载荷中重写时保留；
  更新任务模板会带出旧 evidence。保留下来的旧引文同样逐字复核：来源改写后原句已不在
  raw 里时，提交报 `E_EVIDENCE_NOT_FOUND`，需在本次载荷里为该来源补一条新引文；来源已
  不可用（没有 raw）时无从比对，原样保留。
- `boundaries.covered`、`boundaries.not_covered` 必填。

成功后 `data`: `{task, page, rev, sources, queued_page_tasks}`；page 的 `sources` 是并集。

## release

`release <id>`：把已领取未提交的任务退回队列，不记录错误。任何会话都能释放（给运维留的
恢复口子），`data.owner` 给出原持有方，`summary` 末尾带 `(was leased to <id>)`；原本没记
归属时为空串。
