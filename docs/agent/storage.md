# 存储、写入边界与迁移

由根级开发指引迁入；原节号保留，便于识别既有交叉引用。本文记录开发约束与项目特有问题，产品说明见 [架构说明](../../docs/readme/architecture.md)。

### 2.1 账本写入只有一个入口

`src/store.rs` 仍是唯一能执行 SQL 的地方，但业务写入统一走 [`src/ledger.rs`](../../src/ledger.rs) 的 `Ledger`：登记转换、跟随改名、遗忘删除各一个方法。
想加第四种意图，就加 `Ledger` 的方法，不要绕回 `store.upsert_*`——绕过会被 `tests/invariants.rs::only_ledger_writes_the_ledger` 抓住。
`store.rs` 里的 `upsert_source` / `upsert_raw` 保留 `pub` 只是给集成测试播种用。

不变量原文见 [architecture.md 的「不变量」节](../../docs/readme/architecture.md)。

### 2.2 「账本认不认识这个 raw」只有一个答案

`scan::RawLedger` 负责这个判断，`scan` 的 `layer` 输出与 `doctor` 的 lint L7 都必须调它，不许各自再写一遍 `raws` 表的 join——以前就是三处各写一遍，才出现「L7 提示的修法根本跑不通」。旧 `ingest` 模块随发布路径一起删除后，问这个问题的只剩这两处。
`tests/reconcile.rs` 钉住两处同答。

### 2.4 schema 迁移必须与全新库同构

`SCHEMA_VERSION` 与 `MIGRATION_N` 一一对应，空库直建的 schema 和从 v1 一路升上来的 schema 必须完全一致（`src/store.rs` 里的 `schema_dump` 测试）。
加迁移时同时加 `MIGRATION_{n}` 与 `apply_migration(MIGRATION_{n}, n)` 调用，`tests/invariants.rs::migrate_steps_through_every_schema_version` 会核对个数。
schema 12 重建 `tasks`：`kind` 增加 `route`，`id` 与 `AUTOINCREMENT` 高水位原样保留，并建 `tasks_one_root_per_source`、`tasks_one_convert_per_source`、`tasks_one_route_per_root` 三个唯一索引；旧行只复制不改写。

### 2.6 写入边界靠结构，不靠自觉

引擎（契约、raw、pages、账本、来源待办、索引）全部在隐藏目录 `.wiki/` 里，`Config.paths` 用 `#[serde(skip)]` 固定成常量，手写 `[paths]` 移不出去；工具不生成也不检查任何 git 配置——用 git 的仓库自己把 `.wiki/` 加进根 `.gitignore`。`doctor --fix` 不再生成 `.wiki/.gitignore`（引擎里不放任何 git 文件），旧库遗留的那份不删、也不再被检查；`tests/init.rs::init_leaves_git_configuration_to_the_repository` 钉住新库不生成。`.wiki/` 之外的一切都是源，只读。
`tests/e2e.rs::doctor_ingest_direct_page_query_round_trip` 在完整链路前后逐字节比对引擎外的文件，任何命令往引擎外写文件都会红。任何命令都**永远不删源文件**。

### 2.9 没有候选、没有发布、没有工具内历史

`.wiki/pages/` 就是页面的唯一副本。内容经 `task done` 提交载荷后由工具写进 staging，根收尾时整批合并；没有候选审核、没有 `submit`、没有 manifest/objects 版本，也没有 `restore` 回退；工具不自动备份，恢复只能靠仓库外的独立备份（通常是 git，但 `.wiki/` 未必进 git）。
代价必须说清：工具侧没有撤销入口，同一页被两个人同时写会互相覆盖——CLI 锁只约束工具命令，不约束文件系统层的写页。
工具自己写回内容文件时一律原子替换（`src/render.rs::write_atomic`、`src/convert/commit.rs::write_atomic`：同目录临时文件再 rename），不得就地截断：作者可能正在读，而并发覆盖已经是既有限制，别再把它扩大到半个文件。

raw 的写入比页面多一步补偿：`commit::apply` 先原子写文件、再调 `Ledger::record_conversion_input`；登记失败要回写旧字节或删除新文件，且回写前比对当前摘要——文件已被别人改过就不覆盖，只把旧内容存成 `.*.recovery` 并报 `E_IO`。这不是新的事务机制，只是承认文件与 SQLite 无法同一事务，失败必须暴露可恢复状态，不能吸收成成功。

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

### 2.20 并发：锁管多步骤，busy_timeout 管单条写

`lock::acquire` 只罩 `ingest` 与 `task done`（多步骤写）；`query` 是高频入口不取锁，但它会写 FTS 与 `page_meta`。这类单条短写靠 SQLite 的 busy handler 排队，不靠仓库锁。

**「SQLite 默认 busy timeout 是 0」在这个栈上是错的。** rusqlite 0.31 每次 `Connection::open` 都调 `sqlite3_busy_timeout(db, 5000)`（`inner_connection.rs`，busy.rs 文档也写默认 5000ms 但「may change」）。`src/store.rs` 仍显式设 5s：值钉在我们这边，驱动换掉或改默认时行为不变。持锁超过 5s 仍返回 `DatabaseBusy`，那是真实的长时间占用，不是配置缺失。`tests/store.rs::a_second_writer_waits_for_the_lock_instead_of_failing` 钉住「第二个写者会等」。

`doctor --fix` 不取仓库锁：它本身要检查锁（`src/doctor.rs` 的 `check_lock`），持锁会把自己的检查变成 Warn。与 `ingest` 并发时两边各写派生文件，但都用原子替换，不会留下半个文件。
