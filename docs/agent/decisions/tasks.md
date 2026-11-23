# 开发方向与决策：任务、并发与实现

返回 [开发方向与决策](../development-decisions.md)。任务协议、锁与租约，以及引擎自身的实现取舍。
条目保持当初的写法，按时间顺序排列。

## 2026-11-20：普通 query 按只读快照处理，不参与工作区锁

用户原话："对于常规非task的query，原则上不应该加锁，现状怎么样搜到给什么，哪怕搜完下一秒就变化了，逻辑上也是没问题，搜索是个快照场景"。

- 普通 `query`（文本检索与 `query --page`）只读账本和观看区文件，不写账本、不推观看区。
  它接受读到某一时刻的状态，不为此加锁，也不靠长读事务保证一致性：SQLite 默认 rollback
  journal，长读事务会挡住写命令。
- 因此普通 `query` 不再走整命令工作区锁，打开时也不能执行 `ensure(push=True)`：不建
  `.wiki`、不补 schema、不跑 `sync_view`、不创建 `.wiki/lock`。已落地（2026-11-20）：
  `Wiki.open_readonly` 用 `mode=ro` + `query_only` 打开；`.wiki/wiki.db` 不存在或 schema
  缺列时明确报 `E_STATE`，next 要求先 `ingest`，不静默返回空结果。
- `query --task` 仍写 `<task_dir>/queries.jsonl` 并做配额读改写，保留锁或等价的短临界区；
  它是拆分任务树的写入路径，不在本条豁免内。
- 本条只改写 2026-11-17「任务目录化、单页单任务与工作区文件锁」中 `query` 的锁归属。
  `ingest`、`next`、`submit`、`release`、`sync`、`doctor`、`page import/restore/link`
  等写命令仍按整命令文件锁排队，直到锁分层落地。
- 锁的长期方向是分层：任务归属用硬租约（进程身份、心跳、epoch fencing），互斥粒度按任务
  树和共享资源而不是整条命令，观看区推送收敛为单写者。普通 query 的只读免锁已在
  2026-11-20 落地，其余方向尚未实现；锁的创建方式同日改为临时名写 pid 后 `os.link`
  原子发布，堵掉“空锁被判陈旧”的窗口，细节见 `docs/agent/engine-invariants.md` 第 18 条。
  `docs/readme/cli.md` 与
  `skills/llm-wiki/SKILL.md` 已同步 query 只读行为。

## 2026-11-18：任务租约按会话归属，别的会话立刻停手

触发：Session `3768c894-2598-4eb4-9d63-00b547a27048` 一批次摄入时，主智能体派 3 个子智能体
并行领取。`next` 是全局单消费者，三路加起来 100 次 `E_TASK_UNSUBMITTED` 空转，24 分钟只多
写 10 页（≈0.4 页/分，串行阶段 ≈0.9 页/分），还有一个子智能体停手时仍占着租约未提交。

- 租约记领取会话：新表 `task_leases(task_id, owner, leased_at)`，`tasks` 结构不动（`ensure_columns`
  不做迁移，加列会让旧库直接报 `E_STATE`）。`--owner` 优先，其次 `$LLM_WIKI_OWNER`、
  `$DSH_SESSION_ID`；都没有就不记归属。
- `next` 先 `sweep_leases`，再把租约分成本会话与他会话：本会话仍报 `E_TASK_UNSUBMITTED`，
  他会话报 `E_INGEST_BUSY` 并指出占用方；`submit`、`query --task` 提交他会话的任务报
  `E_TASK_FOREIGN`。`release` 仍对任何会话开放，作为运维恢复口子，输出原持有方。
- 改写 2026-11-17「单写者前提下不再区分「谁的租约」」：不区分归属时，后来者只知道自己领不到，
  无法判断是"自己没交"还是"别人在摄入"，于是照常重试或派并行子智能体；归属可见才能让后来者
  立刻停。
- 文档同步 `SKILL.md`（取任务硬约束、提交失败两条）、`references/recovery.md`（新小节）、
  `cli.md`、`recovery.md`、`design-principles.md`、`engine.md`。

## 2026-11-13：任务协议

- 对外只暴露"任务 + 语义载荷 + 证据"，内部表结构、租约、epoch、哈希不写进技能文档。
- 任务只有 `route` 与 `page` 两种；一份正文（一个 raw）一条 route 任务（同正文的多个路径
  合在一条里），一页一条 page 任务。
- todo 只作为显示层投影，账本是唯一进度权威；没有 todo 工具的运行时不受影响。
- 默认每批 20 条；租约 15 分钟；提交失败零写入并保留 `last_error`。
- 已取消 `reconcile` 任务类型：页面回滚用 `page restore`（当时的镜像漂移自动还原机制已于
  2026-11-17 随镜像一并删除）。

## 2026-11-17：任务目录化、单页单任务与工作区文件锁

用户原话："我认为看不到是技能面不暴露，最好放在db不借助工具读不了的程度就好了，不要
脱离工作区"、"1. 按后者走吧"、"1.、3感觉是一个问题，按目录改造看看难度大不大，大的话，
这次先不做；2. 转换任务不算的话，不允许，而且得用文件做一个进程锁，防止多进程同时
竞争资源"。

- 真实源仍是 `.wiki/wiki.db`：任务状态、租约、epoch 全在账本里，技能与 brief 不出现
  表名、租约、epoch。载荷文件只是中转，不是任务的唯一事实源。
- 目录只是任务素材，不是队列：`next` 领取后物化 `.wiki/tasks/<id>/`
  （`brief.md`、`work.json`、`payload.json`），`submit` 默认读该 `payload.json`；
  提交成功或过期删目录，释放后重领只刷新 brief/work，不覆盖 agent 已填的 payload。
  `.wiki/payloads/` 不再创建。
- 不改 2026-11-15「任务一次一个、agent 不参与任务选择」；同一页面同时只允许一个未关闭
  的 page 任务（route 与来源自己的转换任务不计），账本用部分唯一索引兜底，`enqueue_task`
  撞约束时复用已有任务；已领走的页面仍报 `E_STATE` 让 route 等待。
- 多进程竞争用文件锁解决：写账本/观看区的命令在 `main()` 里抢 `.wiki/lock`，等 5 秒仍
  拿不到返回 `E_BUSY`（退出码 4）；posix 按 pid 判活，非 posix 只按 30 分钟陈旧阈值接管。
  `--dry-run` 且没有 `.wiki/` 时不加锁，保持「试运行不建 `.wiki/`」。
- 与本次无关的共享 raw 人工改写用例（`tests/selftest.py` 的 hand-edited raw 三条）按用户
  要求先删除，不在这轮裁决共享 raw 语义；`selftest.py` 自测 201 项。

## 2026-11-17：`next` 重新发放未提交的任务（同日晚已改写，见下一条）

修 `docs/issues/2026-11-16-P2-next-empty-with-leased-tasks.md`：领取后中断再跑 `next`，
旧实现返回空，agent 按技能说明停摆到租约过期。

- 队列不足一批时，`next` 按同样顺序（route 优先、`created_at`、`id`）重新发放仍在租约里的
  任务，`summary` 以 `resuming` 开头；任务目录与已填 `payload.json` 保持不变。重新发放前
  照旧查漂移，失效任务置 `expired` 不发出；队列与租约都空时才返回 `no queued tasks`。
- 单写者前提下不再区分「谁的租约」，同一工作区允许恢复任何未提交任务；并发重复领取由
  `submit` 的 `state='leased'` 与可选 `epoch` 兜底，后到的提交拿 `E_TASK_STALE`，账本不会
  出现重复记录。
- `release` 不进技能流程，只作为运维口子保留；`SKILL.md` 仍不出现租约、epoch 字样，
  只说「同一个任务会再发回来，`payload.json` 保留」。

## 2026-11-17：`next` 不再重发未提交的任务

改写上面那条「重新发放」的决定：用户指出 `next` 把同一任务当新任务再发，既不报出「当前
任务未提交」，还会续租、加 `epoch`、加 `attempts`，`status` 的「等租约过期」永远不会发生。

- 有 `leased` 任务时 `next` 直接失败，报 `E_TASK_UNSUBMITTED`：`message` 列任务 id，
  `error.next` 给 `submit <id> --payload .wiki/tasks/<id>/payload.json` 与 `release <id>`。
- 不重发、不续租、不 `epoch + 1`、不 `attempts + 1`；租约到期仍按现有规则回队或置
  `expired`。`--kind` 与 `--batch` 受同一条门禁限制：手头没结束就不发新任务。
- 会话重启的自助恢复不变：错误里带着任务目录路径，`brief.md`、`work.json` 与已填的
  `payload.json` 都还在，`release` 是「放弃当前任务」的出口（只写在运维口子，技能流程
  仍不提租约与 epoch）。

## 2026-11-17：`wiki.py` 按职责拆分，入口只留转发

关闭 `docs/issues/2026-11-16-P2-split-wiki-engine.md`：`wiki.py` 长到 2868 行，命令、账本、
观看区与任务混在一个文件里，超过单文件 2000 行的可维护边界。

- 落地：命令层进 `cli/`（`__init__.py` 入口，其余按命令族分）；`ledger/` 收拢 DDL、
  `LedgerMixin` 与组合根 `Wiki`；页面、观看区、任务、摄入分别进 `pages.py`、`view.py`、
  `task/`、`ingest.py`；纯函数公共部分进 `util.py`。`wiki.py` 只保留 `python3 wiki.py <cmd>`
  入口与 `tests/selftest.py` 用到的再导出。
- 取舍：`Wiki` 仍是唯一持有连接的对象，四个 mixin 组合成一个类，拆分只「按职责搬方法」，
  不改命令输出、错误码、`docs/readme/cli.md` 契约与 `.wiki/` 布局；新增 `util.py` 是为了
  让页面与观看区不反向依赖账本模块。
- 约束：新增或再拆 `scripts/` 模块时，同步 `docs/agent/engine.md` 的布局表与 `health.py`
  的 `SCRIPT_FILES`，`wiki.py` 不再回长。

## 2026-11-17：账本自愈，`check`/`init` 不再是前置门禁

关闭 issue「初始化与自检仍写在提示词里」（已删除，历史见 Git）：提示词里的「先 `check`，通过
再 `init`，然后 `ingest`」把初始化责任推给 agent，空目录、半初始化目录都会先撞一次错误。

- 自愈在打开账本时做：`Wiki.ensure` 补目录、`wiki.json`、表与索引；补出的路径收进
  `data.repaired`（路径数组，与 `created` 同形），每个命令的 JSON 输出都带。`command_init`
  不再单独给 `created`。
- `check`/`doctor` 退回只读诊断；命令入口只跑安装级检查（解释器、技能文件、工作区可写），
  失败 `E_HEALTH`。账本缺失或残缺不再阻塞任何命令。
- 致命错误对象加 `doc`（技能内 `references/recovery.md` 绝对路径）与 `section`（小节标题）；
  新增 `references/recovery.md` 作为按错误码分节的处置手册，`docs/readme/recovery.md` 留入口。
- 不做迁移（用户原话「不用考虑已有库，迁移，就按照全新的来，不要留历史包袱」）：有数据的
  旧结构直接 `E_STATE`；旧表全空时按当前 `SCHEMA_SQL` 原地重建，不算迁移。
- 不加 `init --reset`（用户原话「手动直接把.wiki删了也能做到」）：重置就是删 `.wiki/` 重新
  `ingest`，手册里写清步骤，不再加命令。
- `VERSION=0.4.2`；`selftest.py` 208 项。
