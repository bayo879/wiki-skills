# 引擎不变量

返回 [引擎内部](engine.md)。这里是从「不变量」一节拆出的存储与并发约定，编号在
`engine.md` 与其他文档里被直接引用，增删条目时同步引用处。

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
9. **观看区只在命令入口推送。** `open_wiki()` 按 `publish.enabled` 调用 `sync_view()` 推送
   `<view>/pages` 并重写 `README.md`；`workflows/`、`topics/`、`runs/` 不由推送维护。`sync`
   用 `open_wiki(push=False)`，先读磁盘再改账本，避免推送把刚挪走的文件重建出第二份。
   `--dry-run` 不建库、不推送。
10. **没有旧页面接管。** `page_revs` 为空时也不扫描 `.wiki/pages/`：该目录不再被读取，
   账本里没有的页面不会被文件内容凭空生成；升级后残留的 `.wiki/pages/` 由用户手动删除。
11. **工作区边界。** `open_wiki` 以 cwd 为 workspace，`--root` 必须是 workspace 或其子目录；
    `--payload`、`body_file`、`page import --from`、`export` 统一走
    `inside_root`。`--dry-run` 临时库在 `<root>/.wiki/tmp/`，进程退出时删除。`ingest` 跳过
    解析后落在扫描根外的符号链接。导入 `errors.py` / `workspace.py` 前设
    `sys.dont_write_bytecode`，避免全局技能目录在区外留下 `__pycache__`。
12. **工作流只读、不进账本、查询只返回候选。** `scan_files` 在扫描根一级跳过观看区目录名和
    旧根 `workflows/`（`workflows.WORKFLOW_DIRS`），不因旧 `scan.ignore` 是否包含它而改变；
    `workflows.py` 不打开数据库、不写 `.wiki/`、不排任务。`workflow_roots(root, view_root)`
    返回观看区与旧根中实际存在的 `workflows/`，观看区在前；`scan` 按观看区、旧根、内置的顺序
    收一级子目录，同 id 只留先出现的，主文件固定为 `WORKFLOW.md`。旧根条目 `origin="legacy"`；
    同 id 两边 `WORKFLOW.md` 不同时带 `legacy_conflict`/`legacy_path`，不改变加载优先级。
    `query` 会读工作流文件，在页面、话题之外带回最多 5 个候选（`workflows`，每条带
    `applies`），但不自动加载；`workflow:` 前缀只走工作流。`workflow search` 与 query 共用
    `match()`，项目工作流与内置工作流一起参与，只返回 `valid` 项。`workflow list` 仍列出非法项并带
    `error`，`roots` 只列实际扫描到的目录（观看区在前，旧根兜底），并汇总带 warning 的项数。
    `workflow load` 的 id 必须是该目录的一级目录名或内置工作流 id，拒绝 `.`、`..`、路径分隔符
    和区外符号链接。工作流文件是活文件，不做缓存或 digest。
    步骤契约在 `workflow_steps.py` 的 `parse_steps` / `_contract`：只取第一个 `## 步骤` 到下一个二级标题之间的顶层
    列表项（缩进最浅一层），代码块、缩进子项和其他节不算；`tool:`/`human:`/`agent:`/`verify:`
    接受全角冒号与大小写变体，输出统一小写。部分步骤未标给 `W_STEP_UNTYPED`、标记后无内容给
    `W_STEP_EMPTY`；整份零标记、无 `## 步骤` 或该节无列表项在 `UNTYPED_ALLOWED` 为真
    （0.5.0）时只给 warning，翻转后（0.6.0）改判 error。有标记但没有 `tool:` 一律 invalid，
    hint 指向 `process` 页（纯人工流程不属于工作流）。
13. **话题只读且不进账本。** `scan_files` 同时跳过观看区与旧根 `topics/`，话题不会被当成
    来源。`topics.py` 先读 `<view>/topics/*.md`，再读旧根 `topics/`，按文件名去重；取
    frontmatter 的 `name/description/status/updated`，条目带 `origin`（`view`/`legacy`）；
    正文为空或文件超过 200000 字符记为非法，但不进 `query` 的 `topics`。`query` 的
    `topic_matches` 与页面 `matches` 分开计数；命中页面时 `query` 反向给 `citing_topics`；
    话题文件由 agent 直接写，不经账本。
    `parse_refs` 是纯函数，只认 `## 依据` 节的 `- ` 列表项；校验在 `cli/query.py` 的
    `resolve_topic_refs` 里做，`topics.py` 不依赖账本。
14. **观看区目录由 `publish` 配置决定。** `publish_dir_name` 要求 `dir` 是单个目录名，拒绝
    `.wiki`、`topics`、`workflows`；`ensure()` 在开关判断之前调用它，非法值即使
    `enabled:false` 也报 `E_CONFIG`。`ensure_publish` 始终创建 `<view>/`、`<view>/raw/`、
    `<view>/workflows/`、`<view>/topics/`；`enabled:false` 只跳过 `sync_view`。`runs/` 在第一次
    产物时由工作流创建，`pages/` 在推送时创建。`scan_files` 在扫描根无条件跳过该目录名，所以
    关了发布也不能拿它当来源；换掉名字后旧目录恢复为普通来源，定义、话题、产物不会自动搬迁。
    `sync_view` 对 `pages/` 的手改只告警不覆盖，`README.md` 每次推送重写；`workflows/`、
    `topics/`、`runs/` 不覆盖、不删除。
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
18. **写命令按工作区排队。** 会写账本或观看区的命令（含 `query --task`）在 `main()` 里用
    `workspace.workspace_lock` 抢 `<root>/.wiki/lock`：持有者记录先写进
    `lock.<pid>.<rand>` 临时名，再用 `os.link` 原子发布，锁文件一出现就带 pid，不存在
    “存在但为空”的中间态，`link` 撞名即抢锁失败。等待 5 秒仍拿不到抛 `E_BUSY`（exit 4）；
    非 posix 平台不按 pid 判活，只按 30 分钟陈旧阈值接管，posix 用 `os.kill(pid, 0)`
    判断持有者是否还在；读不出 pid 的锁（旧版本遗留空锁、没有硬链接的文件系统退回独占
    创建）先按占用处理，超过 10 秒宽限期才认作陈旧。锁路径不是普通文件或陈旧锁无法
    删除时快速抛 `E_STATE`（exit 3），创建锁失败抛 `E_HEALTH`（exit 1）；判陈旧前先检查
    deadline，任何分支都不会绕过 5 秒上限。崩溃残留的 `lock.<pid>.<rand>` 由下一次加锁
    清扫（只删超过 30 分钟的）。`--dry-run` 且没有 `.wiki/` 时不加锁，避免试运行留下目录。
