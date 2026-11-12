# 架构与模块

`llm-wiki` 是一个单二进制的仓库内工具：所有状态都在仓库目录里，没有服务端、没有数据库服务，唯一的外部依赖是可选的解析服务。本页写实际实现，命令用法见 [README](../../README.md)。

## 一个隐藏目录，四层

| 层 | 位置 | 进 git | 内容 |
|---|---|---|---|
| 源 | 仓库根（`.wiki/` 之外） | 是 | 唯一输入；只读，工具从不在其下写文件 |
| 契约 | `.wiki/wiki.toml`、`.wiki/SCHEMA.md` | 否 | 配置与页面类型枚举；人和 agent 都读 |
| 中间层 | `.wiki/raw/` | 否 | 解析产物，可重建；frontmatter 记录来源、哈希、解析后端 |
| 知识层 | `.wiki/pages/` | 否 | 页面、`index.md`、`audit/` |
| 引擎状态 | `.wiki/` 其余 | 否 | 账本 `state.db`、来源待办、`sessions/`、`cache/`、`lock`；旧数据原样保留 |

这些层都是本地状态，工具不生成也不检查任何 git 配置。用 git 的仓库把 `.wiki/` 加进仓库根的 `.gitignore`；没有 git 的环境不需要这一步。仓库里进 git 的只有源文件。

写入边界是结构性的：`Config.paths` 不从配置反序列化（`#[serde(skip)]`），固定为 `source=""`、`raw=".wiki/raw"`、`wiki=".wiki/pages"`、`engine=".wiki"`，手写 `[paths]` 移不出去。

扫描根是仓库根；点目录天然跳过，`[scan].ignore` 再加一层目录黑名单（默认 `node_modules`、`target`、`dist`、`build`、`vendor`、`.git`）。

## 流水线

```
源文件 ── ingest：有界固定字节、全文哈希、解析 ──▶ .wiki/raw/
                     │ 登记转换、来源待办与根任务
                     ▼
                  state.db（tasks / work_items）
                     │ 交付 next_task：一个 route，或一个 page
                     ▼
agent 路由 ── task done --payload ──▶ route-v1（points / assignments /
                     │                 maintenance / proposals / searches）
                     │ CLI 按归属建 page 任务；零写页直接结算
                     ▼
agent 写页 ── task done --payload ──▶ .wiki/cache/staging/
                     │
                     ▼
根收尾：按 before/expected 校验 ──▶ .wiki/pages/ ── query：增量索引
                     │
                     └── worklist::complete：核对四道哈希，只清对应来源待办
```

`ingest` 保留稳定窗口，`--force` 可跳过等待并强制全文复核，不能越 `parse.max_source_bytes`。扫描以大小和修改时间缓存全文摘要：可漏检元数据保持的改动，不能宣称每轮重读所有源。转换对同一份受限固定字节计算摘要并解析，读取期间检测到变化则拒绝交付。超限源不进待办、不失败，但每轮以 `oversize:` warning 点名键名与上限——静默跳过会被读成「已经整理过」。

来源待办独立于页面：转换登记时持久化，重跑交付未完成来源；作者可以更新已有页，不要求每源新建。`affected_pages` 是现有引用关系得到的提示，不保证语义影响完整，也不是候选全集。任务树以它做维护下限：每个已引用这个来源的现有页必须在 `maintenance` 里得到 `update` 或 `unchanged` 的处置并写理由。

`task done` 关闭 `route` 时校验来源基线、载荷引用与归属：每个信息点有去向（`update`/`create`/`skip`），空信息点有 `source_reason`，`create` 指向的提案带类型与两条理由；关闭后 CLI 只为 `update`/`create` 的目标建 `page` 任务（每根每目标一个），没有目标就置 `settling` 直接结算（零写页）。关闭 `page` 时校验来源与目标快照：规划时记下的 `before` 哈希必须与当前页一致，新页则必须仍不存在，否则 `E_PAGE_CONFLICT`；载荷是融合后的完整正文，渲染后写进 staging 并记下 `expected`。关闭 `reconcile` 时用 `--accept` 把该页当前哈希写进基线，或用 `--regenerate` 把来源重新入队：已有根走 `--replan`，保留已写页、丢弃旧 route/page 与 staging，再按当前 raw 重建 route；只有没有根时才新建。根收尾时才整批合并进 `.wiki/pages/`，逐目标比较当前哈希：等于 `before`（新页则文件仍不存在）就原子写入，等于 `expected` 说明上次已写、只补基线，出现第三种哈希则拒绝合并并阻塞根。随后用四道哈希核对来源：源文件当前摘要、`raws` 行、待办记录、根任务登记的 raw 哈希。漂移退出 1，保留待办；已写页不回滚，根阻塞后由人用 `task retry <根> --replan` 恢复。它不验证正文，不发布或恢复页面；没有候选发布、自动内容版本或内置回退，也没有自动备份。CLI 锁不能保护文件系统层的写页，规划后被改动的目标页在关闭时以 `E_PAGE_CONFLICT` 拒绝。独立备份是否存在由用户负责。

页面与全局基线的比对发生在 `advance` 开头（`task::reconcile_scan`）：有 `sources` 的差异页建 `reconcile` 任务，没 `sources` 的直接写进基线，基线里已不存在的页按接受的删除删掉对应行。基线不再在空闲轮无条件刷新，否则任务外的改动会被静默吸收。

转换与确认之间没有批次门禁，取代它的是 SQLite 里的任务树：一个来源一棵 `breakdown` 根，根下先交付唯一的 `route`；route 关闭后才有 `page` 任务，逐个交付、逐个关闭，全部关闭后根收尾。`task::advance` 是唯一决定下一个任务的函数，`ingest` 与 `task done` 都调它：有 `pending` page 先交付 page；没有 page 时先交付 `reconcile`（任务外改动待复核）；再取最早一棵待路由的根。route 完成前不存在 page 任务。

`ingest` 与 `task done` 交付同一种 `task::view` 结果：`source.read_ref` 是要读的 raw，`target`（有目标页时）带目标页、是否存在、标题/要点与 `before` 哈希，`submit` 给关闭命令与载荷 schema，`stage` 标 `routing`/`writing`/`reconcile`，`guide` 是这一步该做什么，`contract` 是载荷字段语义、约束与真实示例。route 视图里的 `affected_pages` 是已引用该来源的页，只是维护下限；不再把 `TaskRow` 的数据库列暴露给 agent。推进后的公共结果用 `outcome`（`complete`/`in_progress`/`waiting`/`needs_attention`/`partial`）、`effects`（`merged_pages`/`confirmed_sources`/`removed_pages`）、`remaining`（`actionable`/`waiting`/`failed`/`blocked`）与 `issues` 表达：页面合并、来源确认、页面删除是三种独立事实。正常改名进 `events`，碰撞、超限、失败进 `warnings` 或 `failures`。根收尾在 `query::refresh` 后自检新页是否进了关系索引，缺失写进 `issues`，不要求 agent 再跑一次 `query` 证明索引可用。`.wiki/work.lock` 工作租约随「一次只开放一批」一起删除——它服务的是锁人，而任务树只做结算。`moves::apply` 与消失源收口因此每轮都跑。`src/lock.rs` 的仓库锁仍在，只罩 `ingest` 与 `task done`。

转换预算按队列顺序取前 `[ingest].max_per_run` 条，而队列顺序里带着一条让位规则：`source_attempts` 记下每个源最近一次转换失败的时间与错误，上一轮失败的源排在从未尝试过的文件之后，最近失败的沉到队尾。这样坏文件不会每轮占住一个名额把健康文件堵在后面，同时也不会被永久排除——队列里只剩它时照样会被取到。转换成功或源被遗忘时这条记录随同一次事务删除；文件悄悄消失时由下一轮 `ingest` 按「键还有没有对应文件」清掉——从未转换成功的源没有 `sources` 行，消失源收口看不见它。本轮超出预算的源以 `waiting:` 逐条列出，不只是一个数字。

源文件消失是唯一由引擎自己决定的界面变化，策略在 `[ingest].on_missing`：`forget`（默认）把页面里那一条引用去掉（Stable 转 Stale），一条来源都不剩的页整页删除，再删 raw 文件与账本行（`sources`/`raws`/`work_items`/`source_facts` 一次事务）；`mark_stale` 只把引用它的 Stable 页转 Stale；`ignore` 什么都不做。收口顺序是「先页与 raw、后账本行」，任何一步失败只增加本轮 `failures` 并把账本行留给下一轮，所以可以重试；页面与 raw 的删除不可撤销。`sync` 对受影响页复跑 `lint::page_quality`（`L12` 无来源、`L13` 未完成标记），把结果写进轮次 warning，不等 `doctor`。

`doctor` 检查配置、账本、布局、锁、页面及解析后端；修复只处理机械问题。`query` 每次增量刷新正式页索引，`--graph` 读链接图；`--no-archive` 排除配置的会话页面前缀。旧库暂存、版本与对象不删不发，可解析的旧待办只导入来源线索，损坏项显式警告。

## 模块

| 文件 | 职责 |
|---|---|
| `src/cli.rs` | clap 定义，只描述参数，不含业务 |
| `src/main.rs` | 命令分发、`--json` 与人类输出的分叉、退出码 |
| `src/config.rs` | `.wiki/wiki.toml` 解析、默认值、`validate`；路径固定不可配 |
| `src/init.rs` | 契约文件与引擎目录的初始内容（由首条命令自动调用） |
| `src/layout.rs` | 引擎目录布局的唯一一张表：路径 + `authored`，`init`、闸门、补建共用 |
| `src/paths.rs` | 路径归一（`identity_key`）与安全拼接 |
| `src/scan.rs` | 两层对账（仓库根、`.wiki/raw/` ↔ 账本）；唯一分类引擎与「账本认不认识这个 raw」的唯一答案 |
| `src/convert.rs` | 转换的 `prepare`（并行读源、解析）与 `commit`（串行写 raw 与账本） |
| `src/parse/` | 解析后端与 `auto` 回退（`text`/`html`/`asset`/`pdf`/`external`） |
| `src/chunk.rs` | 大文件的按需分块 |
| `src/page.rs` | 页面 frontmatter 的读写、校验与渲染（`render` 是工具生成 frontmatter 的唯一路径） |
| `src/task/` | 任务树：`ensure_root`、`advance`、`close`（route/page/reconcile 校验）、`finish_root`（整批合并）、`plan`（根与页载荷）、`route`（`route-v1` 解析与建任务）、`contract`（TaskView 的说明与契约）；全局基线 |
| `src/read.rs` | 内部引用解析与按行窗口读取；`read` 命令的唯一入口 |
| `src/render.rs` | `index.md` 与人类可读渲染 |
| `src/lint.rs` | 十三条规则，由 doctor 的 `lint` 检查与 `--fix` 使用；`page_quality`（L12/L13）也给 `sync` 复用 |
| `src/ignore.rs` | `.wiki/ignore` 的解析与增删；`ingest` 共用的唯一判定 |
| `src/lock.rs` | 仓库锁（固定 inode + OS 文件锁，不按年龄回收） |
| `src/store.rs` | SQLite 账本、schema 迁移（v12 的 `route` 任务与三个唯一索引）、FTS5；连接显式持有 5s busy_timeout |
| `src/ledger.rs` | 账本唯一的写入口：登记转换、跟随改名、遗忘删除 |
| `src/sync.rs` | 摄入一轮的执行体：`ingest` 调它跑 scan → convert → 来源待办交付，并收口消失的源（`on_missing`） |
| `src/relation.rs` | 唯一的 wikilink 与来源引用解析：别名解析、围栏过滤、raw 片段拆分 |
| `src/query.rs` | FTS5 与关系索引的刷新、文本检索、`--page` / `--source` 精确读取 |
| `src/graph.rs` | 从关系索引读 `[[wikilink]]` 结构：出入度、边、孤立页、断链（`query --graph`） |
| `src/doctor.rs` | 各项健康检查、`--fix` 修复、`--only` 单跑 |
| `src/hash.rs` | 全文 blake3 与有界固定输入读取 |
| `src/worklist.rs` | 来源完成核对（四道哈希）、遗留来源线索导入 |
| `src/report.rs` | JSON / 人类输出信封与退出码 |

## 不变量

- 账本只由 CLI 写；仓库根是唯一输入，`.wiki/raw/` 与 `.wiki/pages/` 都是派生产物，且都与账本一起留在 `.wiki/` 里。**任何命令都不写 `.wiki/` 之外**（检查：tests/e2e.rs::doctor_ingest_direct_page_query_round_trip）
- 引擎目录布局只有一张表 `layout::LAYOUT`：每条只写 `path` 与 `authored`；`init` 全建，`check` 只看 `authored`，`provision` 只补 `authored = false` 的项，并跳过“父目录是缺失的产品目录”的子项——所以补建不会凭空造出 `.wiki/pages`。（检查：tests/precondition.rs::provision_never_creates_the_authored_tree_but_repairs_its_children_when_it_exists）
- 账本表只有 `src/store.rs` 能执行 SQL，业务写入只有 `src/ledger.rs` 一个入口：`ingest` 登记、`moves` 改键都表达成 `ledger::Ledger` 的一次调用，顺序与失败语义写在 `ledger.rs` 的注释里。想加意图，加 `Ledger` 的方法，不要绕回 `store`。（检查：tests/invariants.rs::only_ledger_writes_the_ledger）
- 转换一次只有一个事务：`Store::register_conversion` 在同一个事务里写 `sources`、`raws`、`work_items`、`source_facts` 四张表，raw 文件先落盘、字节与摘要来自同一次读取。半路失败整笔回滚，不会留下「转换成功、待办丢失」的中间态。事务只覆盖账本，不覆盖文件：落在盘的 raw 与账本之间仍可能因断电不一致，由 `doctor` 的 `scan`（`layer=raw` 的 `new`/`changed`）与 `lint` `L7` 暴露。（检查：tests/worklist.rs::failed_enqueue_rolls_back_conversion_baseline_and_can_retry）
- 对账只有一个答案：`scan::classify`（仓库根）与 `scan::classify_raw`（`.wiki/raw/`）是唯一的分类引擎，「账本认不认识这个 raw」只由 `scan::RawLedger` 回答；`scan` 输出的 `layer` 字段与 `lint` `L7` 都调它，不在调用点自建集合。`tests/reconcile.rs` 钉住这两处必须同答。（检查：tests/reconcile.rs::an_unledgered_raw_is_reported_the_same_way_by_both）
- 页面 frontmatter 由 page 任务 `--payload` 的载荷驱动，`sources` 由根任务的 raw 键写入，`updated` 由合并当天的日期写入；同一页已有引用时追加而非替换。（检查：tests/cli_task.rs::the_last_page_settles_the_source）
- 页面必须有 frontmatter 七字段：`id`、`title`、`type`、`summary`、`sources`、`status`、`updated`；`type` 取自 `[schema].types`。（检查：tests/lint.rs::l1_reports_each_missing_required_field）
- page 只写 staging，正式页只在根收尾时整批出现；合并前检索不到这一批。（检查：tests/cli_task.rs::the_last_page_settles_the_source）
- 根收尾只清这一棵根的来源待办，重复收尾是幂等空操作。（检查：tests/cli_task.rs::the_last_page_settles_the_source）
- 忽略列表只有一个解释入口 `ignore::IgnoreList`：`ingest` 跳过，不许在调用点各自拼通配。`[scan].ignore` 只是目录名的粗过滤，两者不混用。（检查：tests/scan_signature.rs::an_entry_of_the_ignore_list_is_invisible_to_the_scan）
- 源在任务期间漂移不得合并，`E_SOURCE_DRIFT` 保留待办与任务；已写页不回滚，恢复走 `task retry <根> --replan`。（检查：tests/cli_task.rs::a_changed_raw_is_refused_before_staging）
- 索引是派生状态，从正式页增量刷新；根收尾合并后立即刷。（检查：tests/cli_task.rs::the_last_page_settles_the_source）
- 关系索引（`page_nodes` / `page_links` / `page_source_refs`）同样只从 Markdown 重建：`query`、`graph`、`lint` 与 `task::affected_pages` 共用 `relation::Resolver` 和同一份反查，不各自扫页；`Store::apply_refresh` 在一个事务里写 FTS 与三张关系表。`generation` 只在关系真变化时递增。（检查：tests/query.rs::refresh_keeps_links_both_ways_and_survives_page_deletion）

## 机器守卫

上面每条不变量都在 CI 里有执行点，按「被违反时哪一层会红」分工：

| 文件 | 抓什么 |
| --- | --- |
| `tests/invariants.rs` | 代码结构：SQL 只在 `store.rs`、账本写入只在 `ledger.rs`、对账只有 `scan`、不变量标注指向真实测试、迁移步数等于 `SCHEMA_VERSION`、每条 lint 规则有 `l{n}_` 测试 |
| `tests/docs_contract.rs` | 文档与 CLI 一致：JSON 形状快照、flag 真存在、`snake_case` 名字与 `E_*` 码闭世界、技能不写 `data.*` 字段路径（只放过 `error.*` 信封）；旧配置兼容键可能接受但不再生成 |
| `tests/snapshots/cli-keys.json` | 各命令 `--json` 的键路径。改输出必须改快照；置 `LLM_WIKI_UPDATE_SNAPSHOTS=1` 重跑 `tests/docs_contract.rs` 会重写它，之后人工过一遍 |
| `tests/docs_tokens.allow` | 白名单：文档里合理出现、但不是本仓词汇的名字，每条写理由 |

行为层面的守卫在不变量行尾点名（`tests/lint.rs`、`tests/cli_task.rs`、`tests/worklist.rs`、`tests/query.rs`、`tests/reconcile.rs`）。

诚实边界：快照只证明「键还在」，不证明语义对；技能禁字段路径只抓 `data.*` 字面量，换成文字同义改写即失效；闭世界词表只证明「这个名字在代码里出现过」，抓不到「代码里有但已废弃」；`invariants.rs` 是文本扫描，注释里出现针脚也会触发（宁可误报）。
