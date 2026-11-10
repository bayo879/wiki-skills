# AGENTS

本项目的开发指引。只写两类内容：用户定下的开发方向，以及这个项目特有的坑与解法。
服务说明、模块划分、配置项看 [README.md](README.md) 与 [docs/readme/architecture.md](docs/readme/architecture.md)；skill 与 CLI 的分工及设计原则见 [docs/readme/design-principles.md](docs/readme/design-principles.md)。

## 1. 开发方向

（本节由用户决策；agent 不擅自增删。）

- 移除候选暂存、`submit` 发布与工具内版本管理（含历史回退）。agent 不直接改 `.wiki/pages/`：它只提交页面载荷（title/summary/tags/status/正文），工具渲染 frontmatter、写 staging、在最后一个子叶关闭时整批合并。旧库遗留数据的处理方式另行确认，不自动删除。
- 保留尚未整理成知识页的来源待办，供中断后继续。来源由任务树结算：一棵根任务的所有子叶关闭后，根收尾合并页面、核对四道哈希并清除待办；资料变化则保留待办、根回到待拆叶。不发布页面、不生成版本、不自动追加来源。
- agent 不直接修改 wiki 结构是硬设定，但它是流程约束不是权限隔离：文件系统层面 agent 仍能写 `.wiki/pages/`，工具只能在叶关闭与根收尾时比对基线发现越界（`E_PAGE_CONFLICT`），真正的隔离靠运行时沙箱。
- 交互目标是「少交互、尽量无感」：能由规则直接判定的事就由工具自己处置，不要再让用户下一条命令。典型是源文件被删——raw、页面对它的引用、只靠它的孤儿页、账本行都应在下一轮摄入里自动收口，而不是等作者手工删页或跑额外命令。需要人判断的（正文冲突、结论取舍）仍然问。
- [未决问题](docs/issues/) 是整个项目的长期问题收集目录，后续优化以此为准，不限于摄入流程。一个未完成的设计或待判别的问题一个文件，写清影响、证据、候选方案与验收条件；未确认方案不得视为定案，解决后删除文件，断点留在 git 与提交信息里。每个文件头标 `优先级：P1`–`P4`：P1 正在丢数据，或流程卡死且没有出口；P2 用户会真实踩到，已有明确修法或只差一个决定；P3 低频路径的边界与设计缺口；P4 测试、评估与文档澄清，不改变当前行为。被问「issues 情况」时按优先级分组回答，P1 在前；该级没有条目就直接说没有，不硬凑。

## 2. 特有问题与解决方案

### 2.1 账本写入只有一个入口

`src/store.rs` 仍是唯一能执行 SQL 的地方，但业务写入统一走 [`src/ledger.rs`](src/ledger.rs) 的 `Ledger`：登记转换、跟随改名、遗忘删除各一个方法。
想加第四种意图，就加 `Ledger` 的方法，不要绕回 `store.upsert_*`——绕过会被 `tests/invariants.rs::only_ledger_writes_the_ledger` 抓住。
`store.rs` 里的 `upsert_source` / `upsert_raw` 保留 `pub` 只是给集成测试播种用。

不变量原文见 [architecture.md 的「不变量」节](docs/readme/architecture.md)。

### 2.2 「账本认不认识这个 raw」只有一个答案

`scan::RawLedger` 负责这个判断，`scan` 的 `layer` 输出与 `doctor` 的 lint L7 都必须调它，不许各自再写一遍 `raws` 表的 join——以前就是三处各写一遍，才出现「L7 提示的修法根本跑不通」。旧 `ingest` 模块随发布路径一起删除后，问这个问题的只剩这两处。
`tests/reconcile.rs` 钉住两处同答。

### 2.3 技能不复述 CLI 的输出形状

`skills/llm-wiki/` 只写流程与判断；JSON 字段路径、必填字段、正式页位置、报错码含义都由 CLI 现打（`ingest` 给 `write_to`、`types` 与 `next_task`，`task done` 在源漂移时点名哈希不符），技能里不再有枚举表、字段表、错误码表和 `data.*` 路径。
`tests/docs_contract.rs::the_skill_states_no_json_field_paths` 禁止技能文件出现 `data.*`（只放过 `error.code`/`error.message`/`error.detail`）；`tests/snapshots/cli-keys.json` 仍锁每条命令的输出形状（`LLM_WIKI_UPDATE_SNAPSHOTS=1` 重生成）。改输出形状时改快照即可，不需要同步技能。

### 2.4 schema 迁移必须与全新库同构

`SCHEMA_VERSION` 与 `MIGRATION_N` 一一对应，空库直建的 schema 和从 v1 一路升上来的 schema 必须完全一致（`src/store.rs` 里的 `schema_dump` 测试）。
加迁移时同时加 `MIGRATION_{n}` 与 `apply_migration(MIGRATION_{n}, n)` 调用，`tests/invariants.rs::migrate_steps_through_every_schema_version` 会核对个数。

### 2.5 文档一致性由机器守，不靠人记

五类守卫，改文档前先知道它们在管什么：

| 守卫 | 抓什么 |
|---|---|
| `tests/invariants.rs` | SQL 单点、账本单入口、对账唯一、不变量都有测试、每条 lint 规则有 `l{n}_` 测试（不再比技能表） |
| `tests/docs_contract.rs` | flag 存在、闭世界词表、`E_*` 存在、技能不写 JSON 字段路径 |
| `tests/skills_pack.rs` | 只发两个技能、frontmatter 只有两项、正文行数上限、引用的子命令真实存在 |
| `tests/snapshots/cli-keys.json` | 每条命令输出的键路径 |
| `tests/docs_tokens.allow` | 词表白名单，加名字要有理由 |

诚实边界写在 [architecture.md 的「机器守卫」节](docs/readme/architecture.md)：它保证「文档没写错名字」，不保证「文档写得够全」。

### 2.6 写入边界靠结构，不靠自觉

引擎（契约、raw、pages、账本、来源待办、索引）全部在隐藏目录 `.wiki/` 里，`Config.paths` 用 `#[serde(skip)]` 固定成常量，手写 `[paths]` 移不出去；工具不生成也不检查任何 git 配置——用 git 的仓库自己把 `.wiki/` 加进根 `.gitignore`。`doctor --fix` 不再生成 `.wiki/.gitignore`（引擎里不放任何 git 文件），旧库遗留的那份不删、也不再被检查；`tests/init.rs::init_leaves_git_configuration_to_the_repository` 钉住新库不生成。`.wiki/` 之外的一切都是源，只读。
`tests/e2e.rs::doctor_ingest_direct_page_query_round_trip` 在完整链路前后逐字节比对引擎外的文件，任何命令往引擎外写文件都会红。任何命令都**永远不删源文件**。

### 2.7 一个技能，四份文件

`skills/llm-wiki/SKILL.md` 是 wiki 本身的入口（边界、命令入口、最小序列、来源确认、触发入口、退出码）；`init.md` 写「首次接入」（持久化记忆与定时任务两件 agent 侧的事，不写 CLI 初始化流程）；`content.md` 写「写什么」（建页判断、新源怎么处理、切片、前后矛盾、去重）；`ops.md` 写「出了问题怎么看」（体检、判断层、冲突与覆盖风险、忽略、叫人）。
`skills/llm-wiki-session/` 是第二个技能：会话蒸馏，写进 `[ingest].session_dir` 指向的源目录（默认 `sessions`），原始会话不存。一个会话一个文件，不按时间切。
拆分只为了让 agent 按需只读一份，不是新的契约；`tests/skills_pack.rs` 断言只发这两个技能、正文行数不超限。

### 2.8 技能的价值在流程纪律，不在用法说明

用 A/B（读技能 vs 裸跑 CLI）实测过四个任务：技能的增量集中在顺序与门禁——`ingest` 一次给出 `write_to`、`next_task` 与受影响页，`task done` 只认根任务登记的 raw 哈希、漂移就不合并且保留待办；
flag 与字段的解释与 `--help`、`init.rs` 生成的 `SCHEMA.md` 高度重复，加长技能文本收益很低，反而会和 CLI 漂移。
所以：技能里写「什么时候做什么、做错了怎么判」，不写「这个参数是什么意思」。
例外是 `SKILL.md` 保留一份最小命令速查（`doctor` / `ingest` / `ingest --auto` / `read` / `query` / `task done` / `task status` / `task retry` / `ignore` 与常用 flag）：agent 需要先知道入口存在，但参数、字段形状与报错码仍以 `--help` 和 CLI 输出为准。

### 2.9 没有候选、没有发布、没有工具内历史

`.wiki/pages/` 就是页面的唯一副本。内容经 `task done` 提交载荷后由工具写进 staging，根收尾时整批合并；没有候选审核、没有 `submit`、没有 manifest/objects 版本，也没有 `restore` 回退；工具不自动备份，恢复只能靠仓库外的独立备份（通常是 git，但 `.wiki/` 未必进 git）。
代价必须说清：工具侧没有撤销入口，同一页被两个人同时写会互相覆盖——CLI 锁只约束工具命令，不约束文件系统层的写页。
工具自己写回内容文件时一律原子替换（`src/render.rs::write_atomic`、`src/convert.rs::write_atomic`：同目录临时文件再 rename），不得就地截断：作者可能正在读，而并发覆盖已经是既有限制，别再把它扩大到半个文件。

### 2.10 忽略列表只有一个解释入口

`src/ignore.rs::IgnoreList` 是唯一读 `.wiki/ignore` 的地方：`ingest` 用它跳过。
不要在调用点自己拼通配，也不要把它和 `[scan].ignore`（只按目录名粗过滤）混用——两者语义不同，混了就会出现「列表里明明写了却还被扫到」。
显式 `ingest <KEY>` 绕开忽略是故意的：先看一眼再决定要不要永久挡住。

### 2.11 来源待办只由根任务收尾清除

一个来源一条待办，转换与入队写在同一个事务里（`Store::register_conversion` 同时写 `sources`、`raws`、`work_items`、`source_facts`）。半路失败时整笔回滚，不会出现「转换成功、待办丢了」的漂移状态；`tests/worklist.rs::failed_enqueue_rolls_back_conversion_baseline_and_can_retry` 用触发器注入失败钉住。

`task::finish_root` 在整批合并成功后持仓库锁调 `worklist::complete`，四道哈希全部成立才删那一条：源文件当前摘要 == 账本记录的源摘要，raw 文件当前摘要 == 待办记录的 raw 摘要 == 根任务拆解时登记的 raw 哈希。任何一项不符都是 `E_SOURCE_DRIFT` exit 1，待办与任务都保留，staging 不清。

幂等是刻意的：根已删、待办已清时，`advance` 返回下一个任务或空，不报错；历史上「重复 submit 报错」的坑不要重演。

根收尾不读页面正文、不验正文质量、不回滚已写内容。写页期间源又变了，它只能保住待办不误清；正文质量与并发覆盖仍由作者负责。`tests/worklist.rs::done_rejects_old_hash_and_only_clears_the_matching_source` 与 `tests/cli_task.rs::the_last_leaf_merges_the_batch_and_clears_the_queue` 钉住。

### 2.12 同步在 CLI 之外

`.wiki/` 是这台机器唯一的事实源，**明确不考虑跨机器**：不要加任何推送/拉取子命令、不要读远端、不要假设网络、不要为多机场景改账本，也不要在文档里教人搬目录或合并账本——两边各写再拼会得到互相矛盾的 `source_hash`。
目录内仍然只存仓库相对路径（键一律来自 `paths::identity_key`），这是「不写死绝对路径」的卫生要求，不是为搬移做准备。

### 2.13 旧库数据只读保留，只导入来源线索

旧 `.wiki/staging/`、`versions/`、`objects/`、`journal.jsonl`、`pending.json` 不自动删除、不重新发布、不转成新版本。它们只是历史字节，命令不得再往里写。
旧 `pending.json` 里能自证的键会导入成来源待办（`src/worklist.rs::import_legacy`）：逐条校验 `raws.source_key` 与 `sources.raw_key` 双向映射、raw 文件摘要 == 账本摘要，成功才 `import_work_item` 并记进 `legacy_work_imports`，所以只导入一次、不会重灌已完成的待办。坏 JSON、缺 raw、断映射、raw 漂移一律降级为 warning 继续跑，原文件原样保留，下次还会再报——「能导的导、坏的可见」是这里的全部承诺。
旧表 `versions` / `pending` / `sync_state` 留在库里不 `DROP`：迁移只增不改，删数据要用户单独决定。`tests/worklist.rs::legacy_queue_imports_valid_entries_once_and_keeps_broken_entries_visible` 钉住。

### 2.14 没有工具侧决策日志，旧 `journal.jsonl` 只读

写它的代码已经删干净（`journal`/`staging`/`version`/`ingest` 四个模块连同测试一起移除），现行命令既不写也不读 `.wiki/journal.jsonl`。它不进页面树、不进 `query` 索引，只是历史字节：不要为了让谁读到它而把它塞回 `pages/`，那会让「日志算不算知识内容」重新变成一个需要回答的问题。
若将来要加审计，写独立文件并在 `LAYOUT` 里登记，不要混进页面目录。

### 2.15 `page_meta` 是索引的变更检测，不是发布凭据

`query::refresh` 把「上次建索引时页面的摘要」存在 `page_meta`：摘要相同且 FTS 里已有这个 key 才跳过。写 FTS 与写 `page_meta` 是同一步，两者不能分开维护。

页面被读坏（frontmatter 解析失败）时，`refresh` 必须把该 key 的旧 FTS 记录连同 `page_meta` 一起删掉，否则搜索会一直返回已经不存在的旧正文；稍后页面修好，再靠摘要变化重建。`tests/query.rs` 里「变坏后清除」「修好后重新可搜」两条钉住。

改完页面状态（`sync` 的收口与转 stale）也要刷新 `page_meta` 摘要，否则下一轮 `refresh` 会把刚标成 stale 的页面当成「未变化」继续用旧索引。`doctor --fix` 仍可整体重建索引，是最后的兜底。


### 2.16 目录布局只有一张表

`src/layout.rs::LAYOUT` 是引擎目录的唯一清单：每条只有 `path` 与 `authored`，父先于子。
`doctor --fix` 遍历全表；`precondition::check` 只看 `authored`；`provision` 只补非 `authored`，并且跳过“父目录是缺失的 `authored` 目录”的子项——否则 `.wiki/pages/audit` 会顺带把 `.wiki/pages` 造出来，闸门就形同虚设。
不要再加回 `DIRS` / `PROVISIONED` 这类第二份清单；加目录只改 `LAYOUT` 并标明是否 `authored`。
`tests/precondition.rs::provision_never_creates_the_authored_tree_but_repairs_its_children_when_it_exists` 钉住补建不会复活产品目录；`src/layout.rs` 的单元测试钉住父先于子、只有 `.wiki/pages` 是 `authored`。

### 2.17 初始化是自动的，不是开场仪式

初始化已经内化到 `open_repo` 与 `run_doctor`：目录里没有 `.wiki/` 时，第一条命令（包括 `doctor`）会静默建默认契约与目录，然后继续。技能不再提 CLI 初始化流程，也不再有 CLI 侧的后续提示；`init.md` 只保留 agent 侧的两件事。
首次接入只保留两件 agent 侧的事：把「工作区为 wiki，咨询问题默认调用 wiki」写进持久化记忆，以及问用户是否把「wiki自动摄入」设为定时任务；具体实现由各 agent 自己的记忆与调度能力完成，CLI 和技能都不假定实现方式。
只有 `.wiki/` 已存在但契约或产品目录缺失时才报 `E_NOT_INITIALISED`，让 `doctor` 诊断、`doctor --fix` 修；这条路径是损坏仓库的出口，不是首次使用的入口。
`tests/precondition.rs::every_repository_command_bootstraps_an_empty_directory` 钉住空目录直接可用，`a_missing_product_directory_is_refused_even_when_the_contract_exists` 钉住损坏仓库仍拒绝。

### 2.18 删命令前先和 spec 做一次差集

命令面收窄过三轮：22 → 6 删了 `version`，直接写页那一轮又删了 `submit` 与整套版本管理，任务树这一轮把 `done` 换成 `task done/status/retry`。两次都出现过文档还在教旧入口的情况（`README.md` 曾写着「submit 建版本」，而代码明确不建）。
删之前先回答「删掉以后坏了怎么办」：`.wiki/` 是唯一内容所在，而且通常不进 git（2.12），所以没有回退就等于没有出口——现在答案只能是仓库外的独立备份，文档必须这么写，不能假装还有内置退路。
删任何一条命令或 flag 前，`grep` 一遍 `README.md`、`docs/readme/`、`docs/issues/`、`skills/`、`src/init.rs`（`SCHEMA.md`），差集里没处理干净的要么补实现、要么改文档。旧命令现在由 clap 返回 `E_USAGE` exit 2，`tests/cli_submit.rs::submit_is_rejected_without_touching_legacy_candidates` 与 `tests/cli_version.rs::version_subcommands_are_rejected_and_preserve_legacy_bytes` 钉住「拒绝且不动旧字节」。

### 2.19 防抖只认 mtime，不记轮次

一轮要不要转换某个源，只看两件事：mtime 距今超过 `[ingest].settle_seconds`（默认 300 秒），并且读到的内容与账本记录不同。没有 `sync_state`、没有连续轮数：跨轮状态只会把「等多久」变复杂。转换失败的计数是另一件事，记在 `tasks` 的 `convert` 行里（2.24），不参与「等多久」的判断。
`--force` 与 `settle_seconds = 0` 是仅有的两个越权口；点名一个 key 不越权，因为等待属于文件，不属于谁在问。

**元数据缓存只用于省读，不用于证明内容没变。** `source_facts` 记上次全文读取的 hash + size + mtime_ns；只有三者全等且该源已经转换过，`scan` 才复用账本摘要，否则全文重读。代价是：大小和 mtime 都不变的改动会被漏掉，只能 `--force` 复核——文档不得写成「每轮全量校验」。
读文件只有一条路径 `hash::read_source`：先按 `parse.max_source_bytes` 预检，再有界读取并前后校验长度与 mtime，超限或不稳定都是错误而不是半份摘要。扫描、预览、转换必须对同一份固定字节做哈希与解析，不允许各读一遍（历史上大文件曾出现「扫描抽样、转换全文」的摘要分裂，`tests/sync.rs::settled_large_source_uses_the_same_full_digest_for_scan_and_conversion` 钉住）。

安静的一轮不是错误：没有新源时 `ingest` 照常跑完收尾（重建索引、写审计），不因为没有待办而拒绝执行。
旧配置里的 `auto_ingest` / `stable_streak` 已删除。`IngestCfg` 带 `deny_unknown_fields`，带这两个键的 `wiki.toml` 会加载失败（`doctor --fix` 修不了配置），删掉那两行即可。
`tests/sync.rs` 钉住：窗口内的文件等下一轮、mtime 够久就转换、`--force` 越权、大小与 mtime 不变的改动不被缓存掩盖、旧但存活的锁不可回收、掉电留下的锁文件原地复用。

超过 `parse.max_source_bytes` 的源在 `scan` 里是 `Oversize`，既不是「待转换」也不是失败：不转换、不进待办、退出码不变，但 `sync` 每轮必须把它写成 `oversize:` warning（键名 + 上限）。静默跳过会被读成「已经整理过」，`tests/cli_ingest.rs::an_oversize_source_is_named_in_the_warnings_of_every_round` 钉住人类输出与 JSON 两侧。

### 2.20 并发：锁管多步骤，busy_timeout 管单条写

`lock::acquire` 只罩 `ingest` 与 `task done`（多步骤写）；`query` 是高频入口不取锁，但它会写 FTS 与 `page_meta`。这类单条短写靠 SQLite 的 busy handler 排队，不靠仓库锁。

**「SQLite 默认 busy timeout 是 0」在这个栈上是错的。** rusqlite 0.31 每次 `Connection::open` 都调 `sqlite3_busy_timeout(db, 5000)`（`inner_connection.rs`，busy.rs 文档也写默认 5000ms 但「may change」）。`src/store.rs` 仍显式设 5s：值钉在我们这边，驱动换掉或改默认时行为不变。持锁超过 5s 仍返回 `DatabaseBusy`，那是真实的长时间占用，不是配置缺失。`tests/store.rs::a_second_writer_waits_for_the_lock_instead_of_failing` 钉住「第二个写者会等」。

`doctor --fix` 不取仓库锁：它本身要检查锁（`src/doctor.rs` 的 `check_lock`），持锁会把自己的检查变成 Warn。与 `ingest` 并发时两边各写派生文件，但都用原子替换，不会留下半个文件。

### 2.21 消失的源自动收口，删页是不可撤销的那一步

`[ingest].on_missing` 有三个值，默认 `forget`：`forget` 去掉页面里那一条引用（Stable 转 Stale）、一条来源都不剩的页整页删除、删 raw 文件、删账本行（`sources`/`raws`/`work_items`/`source_facts` 同一次事务）；`mark_stale` 只把引用它的 Stable 页转 Stale；`ignore` 直接返回空。
顺序是「先页与 raw、后账本行」，所以任一步失败只是本轮多一条 `failures` 与一条 `vanished: ... could not be settled` warning，账本行留着下一轮重试。**删页与删 raw 不可撤销**，这是 `mark_stale` 仍然存在的唯一理由——不要给「页被删了怎么恢复」发明工具内退路（2.9 已说明没有）。
判据只认页面自己的 `sources:`，不认账本的映射：页面可能引用一个账本已不认识的 raw，照样要清。`tests/sync.rs::forget_removes_the_page_the_raw_and_the_ledger_row`、`forget_keeps_a_page_that_still_has_another_source`、`a_failed_forget_keeps_the_ledger_row_for_the_next_round` 钉住三件事。
页质量闸门 `lint::page_quality`（`L12` 页面不引用任何来源、`L13` 正文留着 `TODO`/`FIXME`/`待补`/`占位`，代码围栏内不算）既是 `lint` 的规则，也是 `sync` 对 `affected_pages` 复跑的那一份——直接写进轮次 warning，不等 `doctor`。两处共用同一个函数，不要各写一遍。

### 2.22 任务树取代批次与工作租约

`.wiki/work.lock` 与 `src/lease.rs` 已删除。它服务的概念是「一次只开放一批来源」，现在由 SQLite 的 `tasks` 表承担：一个来源一棵根任务（`breakdown`），agent 把它拆成子叶（`page`），子叶逐个交付、逐个关闭，全部关闭后根收尾合并。这个单位是**结算单位**，不是锁：它不阻止任何人写文件，只在叶关闭与根收尾时比对基线。

不许以任何形式把它变回「锁人」的门禁：

- `ingest` 与 `task done` 都经 `task::advance` 交付**一个** `next_task`；整棵树用 `task status` 看。交付清单不截断指的是树本身完整，不是一次塞一堆任务。
- `done` 已移除；来源确认由根收尾的四道哈希取代；没有 `E_WORK_LEASE`。
- `moves::apply` 与消失源收口每轮都跑，不因为「批内页可能正在被写」而推迟。

当初的担忧（改名或删页撞上正在写的页）仍然成立，但保护位置变了：`page::write_bytes` 落盘前重读比对，不一致就跳过并写进 warning；任务树的基线比对只做事后越界发现（`E_PAGE_CONFLICT`），不是锁，也抵不了手工写页。

### 2.23 改文档用会报错的工具，不用静默替换

`edit` 找不到 `old_string` 会报错；`python str.replace`、`sed -i` 找不到模式照样返回 0。本项目的文档句子是可被引用的断言（README、`docs/readme/`、`AGENTS.md` 里写着"已经这么做"），用静默成功的工具改它，等于把"改过了"建立在"我记得改过了"上。第五道机器守卫（2.5）守不住这个：它检查的是词表和形状，不检查某一句是否真的落进了文件。

规矩：

- 单点改一句、改一行、追加一段：用 `edit`，失败就说明我记错了原文。
- 整段重写或超过五处独立替换：才用脚本，并且同一轮 `grep` 到新内容才算改完。
- 复核是取证，不是礼貌：报告"文档已改"之前，先看见 `grep` 的输出。

### 2.24 失败的源让位，但不被永久排除

`source_attempts`（migration 8）记「这个源上次转换失败的时间与错误」，只用于排序；同一份失败还会写进 `tasks` 的 `convert` 行（`kind='convert'`，`attempts` 自增，`last_error` 记录原因），那才是熔断依据。`sync::order_pending` 按 `(上次失败时间, key)` 升序排：从未失败过的源键是空串、排在最前，刚失败的源时间最新、沉到队尾。理由是把预算先让给能顺畅处理的文件（用户原话：「失败了，优先级可以放到最低」），同时坏文件不会消失——队列里只剩它时照样会被取到。

写入只有一处：`sync.rs` 转换失败的 `Err` 分支调 `Ledger::record_failure`。清除有三个出口，都是为了「文件不在了笔记也不留」：`register_conversion`（转换成功）与 `delete_source`（源被遗忘）在各自事务里删，文件悄悄消失时由 `sync::run` 的 3c 段按「`source_attempts` 里的键还有没有对应文件」清掉——`book.md` 这类从未转换成功的源没有 `sources` 行，`settle_missing` 根本看不见它。`src/store.rs` 的 `a_failure_note_lives_until_the_source_converts_or_goes_away` 与 `tests/sync.rs` 的 `a_failed_source_yields_the_budget_and_its_note_dies_with_the_file` 钉住这几条出口，`src/sync.rs` 的 `a_failed_source_sinks_behind_untried_ones_but_keeps_its_turn` 钉住排序本身。

连续 3 次转换失败把 `convert` 行置 `failed`：之后每轮只写 `failed: <key> ...` warning，不再给它预算、不再为它建根。它没有自动出口——源改了也不自动重置，只认 `task retry <id>` 删行并清 `source_attempts`。这是唯一允许的熔断，不要再往上加失败计数、退避时间或「N 次自动忽略」；具体放弃与否仍由人用 `task retry` 或 `doctor` 判断。`queued` 只是数字，超出预算的源还必须以 `waiting:` 逐条列出——只报数字等于没让人看见。



### 2.25 任务树的推进只有一个函数

`task::advance` 是唯一决定「下一个任务是谁」的地方：`ingest` 与 `task done` 都调它，别在两处各写一套排序，否则两边的「当前任务」会分叉。顺序是：有 `pending` 子叶先交付；全库没有子叶才取最早一棵待拆叶的根；根的子叶全清后在 `advance` 内部收尾。

基线是**全局一份**（`GLOBAL_BASELINE = 0`），不是每棵树一份：先收敛的树合并出的页，必须被后收敛的树当成正常状态。`audit/` 与 `index.md` 是派生文件，不进基线；`tasks.id` 用 `AUTOINCREMENT`，删掉的叶 id 不能被复用，否则 staging 或基线会串页。

任务外的改动**不静默吸收**：`advance` 开头调 `task::reconcile_scan`，把当前 `pages/` 与全局基线比对。有 `sources` 的差异页建 `reconcile` 任务，agent 用 `--accept`（承认改动、把该页当前哈希写进基线）或 `--regenerate`（重建来源待办并 `ensure_root`，走正常拆解覆盖）关闭；没有 `sources` 的页直接写进基线；基线里已消失的页删行（源也没了由 `on_missing` 收口）。空闲轮不再无条件 `refresh_global_baseline`——那正是手工改动被吞掉的原因。

## 3. 提交前自查

```bash
cargo test
cargo clippy --all-targets
cargo fmt --check
```

文档与代码同一次提交；只改文档的提交也要在 message 里说明为什么改。
