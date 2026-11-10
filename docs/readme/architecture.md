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
                     │ 登记转换及来源待办
                     ▼
                  state.db
                     │ 交付 raw 哈希、正式页目录、schema、受影响页
                     ▼
作者实际读取 raw ── 直接更新 ──▶ .wiki/pages/ ── query：增量索引
                     │
                     └── done：持锁核对所读 raw 哈希及当前源，只清对应来源待办
```

`ingest` 保留稳定窗口，`--force` 可跳过等待并强制全文复核，不能越 `parse.max_source_bytes`。扫描以大小和修改时间缓存全文摘要：可漏检元数据保持的改动，不能宣称每轮重读所有源。转换对同一份受限固定字节计算摘要并解析，读取期间检测到变化则拒绝交付。超限源不进待办、不失败，但每轮以 `oversize:` warning 点名键名与上限——静默跳过会被读成「已经整理过」。

来源待办独立于页面：转换登记时持久化，重跑交付未完成来源；作者可以更新已有页，不要求每源新建。`affected_pages` 是现有引用关系得到的提示，不保证语义影响完整。页面作者显式写真实 `sources` 与 `updated`，工具不补引用。

`done` 对照作者实际读取的 raw 哈希；漂移退出 1，保留待办。重复确认是成功无动作。它不验证正文，不发布或恢复页面。页面直接落盘立即生效，没有候选发布、自动内容版本或内置回退；也没有自动备份。CLI 锁不能保护外部作者写页，并发作者可能覆盖，需单一作者及写前重读。独立备份是否存在由用户负责。

转换与确认之间没有批次门禁。`ingest` 每轮把全部未完成来源交给作者，`done` 不看在不在某一批里，只核对实际读取的 raw 哈希与当前源。曾经存在的 `.wiki/work.lock` 工作租约随「一次只开放一批」一起删除：它服务的是批次调度，而知识页没有版本与发布之后，「一批来源」不再是一个可操作的单位，剩下的只是「当前有哪些待处理文件」。`moves::apply` 与消失源收口因此每轮都跑，不再等某个批次关闭。`src/lock.rs` 的仓库锁仍在，只在单次命令期间存在，与这里无关。

转换预算按队列顺序取前 `[ingest].max_per_run` 条，而队列顺序里带着一条让位规则：`source_attempts` 记下每个源最近一次转换失败的时间与错误，上一轮失败的源排在从未尝试过的文件之后，最近失败的沉到队尾。这样坏文件不会每轮占住一个名额把健康文件堵在后面，同时也不会被永久排除——队列里只剩它时照样会被取到。转换成功或源被遗忘时这条记录随同一次事务删除；文件悄悄消失时由下一轮 `ingest` 按「键还有没有对应文件」清掉——从未转换成功的源没有 `sources` 行，消失源收口看不见它。本轮超出预算的源以 `waiting:` 逐条列出，不只是一个数字。

源文件消失是唯一由引擎自己决定的界面变化，策略在 `[ingest].on_missing`：`forget`（默认）把页面里那一条引用去掉（Stable 转 Stale），一条来源都不剩的页整页删除，再删 raw 文件与账本行（`sources`/`raws`/`work_items`/`source_facts` 一次事务）；`mark_stale` 只把引用它的 Stable 页转 Stale；`ignore` 什么都不做。收口顺序是「先页与 raw、后账本行」，任何一步失败只增加本轮 `failures` 并把账本行留给下一轮，所以可以重试；页面与 raw 的删除不可撤销。`sync` 对受影响页复跑 `lint::page_quality`（`L12` 无来源、`L13` 未完成标记），把结果写进轮次 warning，不等 `doctor`。

`doctor` 检查配置、账本、布局、锁、页面及解析后端；修复只处理机械问题。`query` 每次增量刷新正式页索引，`--graph` 读链接图；`--no-archive` 排除配置的会话页面前缀。旧库暂存、版本与对象不删不发，可解析的旧待办只导入来源线索，损坏项显式警告。

## 模块

| 文件 | 职责 |
|---|---|
| `src/cli.rs` | clap 定义，只描述参数，不含业务 |
| `src/main.rs` | 命令分发、`--json` 与人类输出的分叉、退出码 |
| `src/config.rs` | `.wiki/wiki.toml` 解析、默认值、`validate`；路径固定不可配 |
| `src/init.rs` | 契约文件与引擎目录的初始内容，以及初始化后给 agent 的固定指引 |
| `src/layout.rs` | 引擎目录布局的唯一一张表：路径 + `authored`，`init`、闸门、补建共用 |
| `src/paths.rs` | 路径归一（`identity_key`）与安全拼接 |
| `src/scan.rs` | 两层对账（仓库根、`.wiki/raw/` ↔ 账本）；唯一分类引擎与「账本认不认识这个 raw」的唯一答案 |
| `src/convert.rs` | 转换一轮的调度与写盘 |
| `src/parse/` | 解析后端与 `auto` 回退（`text`/`html`/`asset`/`pdf`/`external`） |
| `src/chunk.rs` | 大文件的按需分块 |
| `src/page.rs` | 页面 frontmatter 的读写与校验 |
| `src/render.rs` | `index.md` 与人类可读渲染 |
| `src/lint.rs` | 十三条规则，由 doctor 的 `lint` 检查与 `--fix` 使用；`page_quality`（L12/L13）也给 `sync` 复用 |
| `src/ignore.rs` | `.wiki/ignore` 的解析与增删；`ingest` 共用的唯一判定 |
| `src/lock.rs` | 仓库锁（固定 inode + OS 文件锁，不按年龄回收） |
| `src/store.rs` | SQLite 账本、schema 迁移、FTS5；连接显式持有 5s busy_timeout |
| `src/ledger.rs` | 账本唯一的写入口：登记转换、跟随改名、遗忘删除 |
| `src/sync.rs` | 摄入一轮的执行体：`ingest` 调它跑 scan → convert → 来源待办交付，并收口消失的源（`on_missing`） |
| `src/query.rs` | FTS5 索引与检索 |
| `src/graph.rs` | `[[wikilink]]` 结构：出入度、边、孤立页、断链（`query --graph`） |
| `src/doctor.rs` | 十一项健康检查、`--fix` 修复、`--only` 单跑 |
| `src/hash.rs` | 全文 blake3 与有界固定输入读取 |
| `src/worklist.rs` | 来源完成核对、遗留来源线索导入 |
| `src/report.rs` | JSON / 人类输出信封与退出码 |

## 不变量

- 账本只由 CLI 写；仓库根是唯一输入，`.wiki/raw/` 与 `.wiki/pages/` 都是派生产物，且都与账本一起留在 `.wiki/` 里。**任何命令都不写 `.wiki/` 之外**（检查：tests/e2e.rs::doctor_ingest_direct_page_query_round_trip）
- 引擎目录布局只有一张表 `layout::LAYOUT`：每条只写 `path` 与 `authored`；`init` 全建，`check` 只看 `authored`，`provision` 只补 `authored = false` 的项，并跳过“父目录是缺失的产品目录”的子项——所以补建不会凭空造出 `.wiki/pages`。（检查：tests/precondition.rs::provision_never_creates_the_authored_tree_but_repairs_its_children_when_it_exists）
- 账本表只有 `src/store.rs` 能执行 SQL，业务写入只有 `src/ledger.rs` 一个入口：`ingest` 登记、`moves` 改键都表达成 `ledger::Ledger` 的一次调用，顺序与失败语义写在 `ledger.rs` 的注释里。想加意图，加 `Ledger` 的方法，不要绕回 `store`。（检查：tests/invariants.rs::only_ledger_writes_the_ledger）
- 转换一次只有一个事务：`Store::register_conversion` 在同一个事务里写 `sources`、`raws`、`work_items`、`source_facts` 四张表，raw 文件先落盘、字节与摘要来自同一次读取。半路失败整笔回滚，不会留下「转换成功、待办丢失」的中间态。事务只覆盖账本，不覆盖文件：落在盘的 raw 与账本之间仍可能因断电不一致，由 `doctor` 的 `scan`（`layer=raw` 的 `new`/`changed`）与 `lint` `L7` 暴露。（检查：tests/worklist.rs::failed_enqueue_rolls_back_conversion_baseline_and_can_retry）
- 对账只有一个答案：`scan::classify`（仓库根）与 `scan::classify_raw`（`.wiki/raw/`）是唯一的分类引擎，「账本认不认识这个 raw」只由 `scan::RawLedger` 回答；`scan` 输出的 `layer` 字段与 `lint` `L7` 都调它，不在调用点自建集合。`tests/reconcile.rs` 钉住这两处必须同答。（检查：tests/reconcile.rs::an_unledgered_raw_is_reported_the_same_way_by_both）
- 页面字段必须显式填写，真实来源由作者核对，工具不自动补全。（检查：tests/cli_done.rs::done_clears_only_one_source_and_repeating_it_is_a_noop）
- 页面必须有 frontmatter 七字段：`id`、`title`、`type`、`summary`、`sources`、`status`、`updated`；`type` 取自 `[schema].types`。（检查：tests/lint.rs::l1_reports_each_missing_required_field）
- 作者直接写正式页，写后不经过发布，确认来源前也能检索。（检查：tests/cli_done.rs::directly_written_pages_are_queryable_before_done）
- 来源完成只清对应来源，重复确认不改页面或其他待办。（检查：tests/cli_done.rs::done_clears_only_one_source_and_repeating_it_is_a_noop）
- 忽略列表只有一个解释入口 `ignore::IgnoreList`：`ingest` 跳过，不许在调用点各自拼通配。`[scan].ignore` 只是目录名的粗过滤，两者不混用。（检查：tests/scan_signature.rs::an_entry_of_the_ignore_list_is_invisible_to_the_scan）
- 作者携带旧 raw 哈希不得确认新资料，漂移保留待办。（检查：tests/cli_done.rs::done_with_an_old_raw_hash_keeps_the_new_work）
- 索引是派生状态，从正式页增量刷新，不要求先确认来源。（检查：tests/cli_done.rs::directly_written_pages_are_queryable_before_done）

## 机器守卫

上面每条不变量都在 CI 里有执行点，按「被违反时哪一层会红」分工：

| 文件 | 抓什么 |
| --- | --- |
| `tests/invariants.rs` | 代码结构：SQL 只在 `store.rs`、账本写入只在 `ledger.rs`、对账只有 `scan`、不变量标注指向真实测试、迁移步数等于 `SCHEMA_VERSION`、每条 lint 规则有 `l{n}_` 测试 |
| `tests/docs_contract.rs` | 文档与 CLI 一致：JSON 形状快照、flag 真存在、`snake_case` 名字与 `E_*` 码闭世界、技能不写 `data.*` 字段路径（只放过 `error.*` 信封）；旧配置兼容键可能接受但不再生成 |
| `tests/snapshots/cli-keys.json` | 各命令 `--json` 的键路径。改输出必须改快照；置 `LLM_WIKI_UPDATE_SNAPSHOTS=1` 重跑 `tests/docs_contract.rs` 会重写它，之后人工过一遍 |
| `tests/docs_tokens.allow` | 白名单：文档里合理出现、但不是本仓词汇的名字，每条写理由 |

行为层面的守卫在不变量行尾点名（`tests/lint.rs`、`tests/cli_done.rs`、`tests/worklist.rs`、`tests/query.rs`、`tests/reconcile.rs`）。

诚实边界：快照只证明「键还在」，不证明语义对；技能禁字段路径只抓 `data.*` 字面量，换成文字同义改写即失效；闭世界词表只证明「这个名字在代码里出现过」，抓不到「代码里有但已废弃」；`invariants.rs` 是文本扫描，注释里出现针脚也会触发（宁可误报）。
