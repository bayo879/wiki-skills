# AGENTS

本项目的开发指引。只写两类内容：用户定下的开发方向，以及这个项目特有的坑与解法。
服务说明、模块划分、配置项看 [README.md](README.md) 与 [docs/readme/architecture.md](docs/readme/architecture.md)。

## 1. 开发方向

（由用户填写。agent 不擅自增删本节，改动前先问。）

## 2. 特有问题与解决方案

### 2.1 账本写入只有一个入口

`src/store.rs` 仍是唯一能执行 SQL 的地方，但业务写入统一走 [`src/ledger.rs`](src/ledger.rs) 的 `Ledger`：登记转换、跟随改名、遗忘删除各一个方法。
想加第四种意图，就加 `Ledger` 的方法，不要绕回 `store.upsert_*`——绕过会被 `tests/invariants.rs::only_ledger_writes_the_ledger` 抓住。
`store.rs` 里的 `upsert_source` / `upsert_raw` 保留 `pub` 只是给集成测试播种用。

不变量原文见 [architecture.md 的「不变量」节](docs/readme/architecture.md)。

### 2.2 「账本认不认识这个 raw」只有一个答案

`scan::RawLedger` 负责这个判断，`doctor` 的 lint L7 与 `ingest` 的 `orphan_pages` 都必须调它，不许各自再写一遍 `raws` 表的 join——以前就是三处各写一遍，才出现「L7 提示的修法根本跑不通」。
`tests/reconcile.rs` 钉住三处同答。

### 2.3 技能不复述 CLI 的输出形状

`skills/llm-wiki/` 只写流程与判断；JSON 字段路径、必填字段、候选路径、报错码含义都由 CLI 现打（`ingest begin` 给 `write_to`/`type`，`submit` 点名缺哪个字段），技能里不再有枚举表、字段表、错误码表和 `data.*` 路径。
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
| `tests/skills_pack.rs` | 只发一个技能、frontmatter 只有两项、正文行数上限、引用的子命令真实存在 |
| `tests/snapshots/cli-keys.json` | 每条命令输出的键路径 |
| `tests/docs_tokens.allow` | 词表白名单，加名字要有理由 |

诚实边界写在 [architecture.md 的「机器守卫」节](docs/readme/architecture.md)：它保证「文档没写错名字」，不保证「文档写得够全」。

### 2.6 写入边界靠结构，不靠自觉

引擎（契约、raw、pages、账本、暂存、版本）全部在隐藏目录 `.wiki/` 里，`Config.paths` 用 `#[serde(skip)]` 固定成常量，手写 `[paths]` 移不出去；工具不生成也不检查任何 git 配置——用 git 的仓库自己把 `.wiki/` 加进根 `.gitignore`。`doctor --fix` 不再生成 `.wiki/.gitignore`（引擎里不放任何 git 文件），旧库遗留的那份不删、也不再被检查；`tests/init.rs::init_leaves_git_configuration_to_the_repository` 钉住新库不生成。`.wiki/` 之外的一切都是源，只读。
`tests/e2e.rs::doctor_ingest_submit_query_round_trip` 在完整链路前后逐字节比对引擎外的文件，任何命令往引擎外写文件都会红。任何命令都**永远不删源文件**。

### 2.7 一个技能，三份文件

`skills/llm-wiki/SKILL.md` 是 wiki 本身的入口（边界、路由、最小序列、门禁、`submit` 选项、退出码）；`content.md` 写「写什么」（建页判断、新源怎么处理、切片、前后矛盾、去重）；`ops.md` 写「出了问题怎么看」（体检、判断层、冲突与回退、忽略、叫人）。
`skills/llm-wiki-session/` 是第二个技能：会话蒸馏，写进 `[ingest].session_dir` 指向的源目录（默认 `sessions`），原始会话不存。一个会话一个文件，不按时间切。
拆分只为了让 agent 按需只读一份，不是新的契约；`tests/skills_pack.rs` 断言只发这两个技能、正文行数不超限。

### 2.8 技能的价值在流程纪律，不在用法说明

用 A/B（读技能 vs 裸跑 CLI）实测过四个任务：技能的增量集中在顺序与门禁——`ingest` 一次给出 `write_to`、候选只能写到那里、`submit` 只有每个候选都被扣住才 exit 3；
flag 与字段的解释与 `--help`、`init` 生成的 `SCHEMA.md` 高度重复，加长技能文本收益很低，反而会和 CLI 漂移。
所以：技能里写「什么时候做什么、做错了怎么判」，不写「这个参数是什么意思」。

### 2.9 版本是 manifest + 对象，不是目录拷贝

`ingest` 与 `submit` 记账时不复制目录：它们把 `pages/` 与 `raw/` 每条路径的 blake3 写进 `manifest.json`，字节按摘要存进 `.wiki/objects/`。
所以「改一页」不会让整版体积翻倍，但**任何直接改写内容文件的代码都必须走 `version::write_atomic`**——就地截断会同时改掉历史版本共享的那个对象。
版本是每轮自动记的：`ingest` 结束记一个锚点，`submit` 不再记版本（它发布的内容在 ingest 锚点之后，所以 `version diff` 会看到它们）。回退入口是 `version restore <TAG> --confirm`，它恢复后清空 staging 与 pending，并把这次恢复本身也记成一个 `rollback` 版本；不做单页回退。

### 2.10 忽略列表只有一个解释入口

`src/ignore.rs::IgnoreList` 是唯一读 `.wiki/ignore` 的地方：`ingest` 用它跳过。
不要在调用点自己拼通配，也不要把它和 `[scan].ignore`（只按目录名粗过滤）混用——两者语义不同，混了就会出现「列表里明明写了却还被扫到」。
显式 `ingest <KEY>` 绕开忽略是故意的：先看一眼再决定要不要永久挡住。

### 2.11 三道刹车只扣住命中的候选

`submit` 不带 `--confirm` 时依次过三道，**命中就扣住那一个候选**（留在 `.wiki/staging/`，下一轮还能再试），其余的照常发布：

1. **近义页**（`reason: page_overlap`）：标题归一化后与已发布页相同，或候选页的 `sources` 是某张已发布页 `sources` 的子集（没引入新证据）。只比 frontmatter，不读正文、不调模型——语义级「近义」在这里做不到，别试图加。候选自己的路径不算重叠：改同一页不触发（`src/version.rs::overlap_reason` 跳过相同路径）。
2. **切片预算**（`reason: page_slice_budget`）：同一份源的第 N 个切片已经养出 `[ingest].max_pages_per_slice` 页（默认 5，0 不限）。切片是 `#p=<n>` 指的那一段；小于 `parse.chunk_threshold` 的文件，页面上写什么片段都算整份文件。
3. **页面预算**（`reason: page_budget`）：本轮新增页数超过 `[ingest].max_new_pages_per_run`（默认 20，0 不限）；已存在的页算覆盖，不占预算。

只有**每个**候选都被扣下时才 exit 3（`NEEDS_CONFIRMATION`，`data.held_back` 列出原因）——那才是真要人看一眼的时刻。`--confirm` 是显式越过全部三道。
另有一条容易踩的：这几道都必须写在 `if !confirm` 里，曾经写在外面，结果 `--confirm` 也被拦（`tests/version.rs` 的 `approve_confirm_overrules_*` 钉住）。

### 2.12 同步在 CLI 之外

`.wiki/` 是这台机器唯一的事实源，**明确不考虑跨机器**：不要加任何推送/拉取子命令、不要读远端、不要假设网络、不要为多机场景改账本，也不要在文档里教人搬目录或合并账本——两边各写再拼会得到互相矛盾的 `source_hash`。
目录内仍然只存仓库相对路径（键一律来自 `paths::identity_key`），这是「不写死绝对路径」的卫生要求，不是为搬移做准备。

### 2.13 否决也是对源的表决

`reject` 只丢页面是不够的：人看过这批变动、说了「不要」，变动的字节就已经被承认为已知状态。所以 `abort` 走 `Ledger::mark_reviewed`，把这一轮每个源的账本摘要换成磁盘上的当前摘要，并把它们在 manifest 里的 `baseline_hash` 推到 `current_hash`。
不这么做，同一个文件会一轮轮以 `changed` 回来，直到有人把它加进忽略列表——而拒绝和忽略是两件事。
摘要必须按 `scan` 的选法算（小文件全量、大文件采样，见 `accept_reviewed_sources`），否则下一次 `doctor` 的扫描立刻报假 `changed`；`tests/version.rs::reject_accepts_the_source_bytes_so_it_is_not_reported_again` 钉住。超大读不了的文件跳过，让它继续报 `oversize`。

这道表决依赖开轮时把这一轮的 raw 摘要写进 `manifest.json` 的 `current_hash`：`check_sources`（漂移闸门）和 `abort`（认领本轮源）都靠它定位。它一度没人写，于是漂移闸门静默失效、`abort` 找不到本轮源——改这两处时先确认 `current_hash` 有值。`current_hash` 存的是 **raw** 的摘要，`pending.json` 里存的是源的摘要，两者不是一回事。

### 2.14 决策日志不是内容

每次 `submit` / `abort` 只往 `.wiki/journal.jsonl` 追加一行 JSON（`journal::append`），不再写 `pages/log.md`。三条后果要记住：日志不进版本（`collect_content` 只走 `pages/` 与 `raw/`），所以版本回退抹不掉它；它不参与 `query` 索引，所以搜不到；`detail` 里的换行被 JSON 转义，多行 `--reason` 不会破坏格式。想给它加读者就加，但别把它塞回页面树——那会让「回退会不会改历史」重新变成一个问题。

### 2.15 发布后要清 `page_meta`，否则搜索停在旧正文

`ingest` 把候选的摘要写进 `page_meta`，发布时用它对账「交稿后有没有人手改」。但发布只把字节移进 `wiki/`，不写 FTS；`query::refresh` 的跳过条件是「摘要相同且索引里已有这个 key」，于是改过的旧页会被永久跳过，直到 `doctor --fix` 重建索引。所以发布后要对每个已发布页 `delete_page_meta`，让下一次 refresh 重建索引；`tests/version.rs::publishing_a_page_drops_the_index_record_so_the_next_query_reindexes_it` 钉住。


### 2.16 目录布局只有一张表

`src/layout.rs::LAYOUT` 是引擎目录的唯一清单：每条只有 `path` 与 `authored`，父先于子。
`doctor --fix` 遍历全表；`precondition::check` 只看 `authored`；`provision` 只补非 `authored`，并且跳过“父目录是缺失的 `authored` 目录”的子项——否则 `.wiki/pages/audit` 会顺带把 `.wiki/pages` 造出来，闸门就形同虚设。
不要再加回 `DIRS` / `PROVISIONED` 这类第二份清单；加目录只改 `LAYOUT` 并标明是否 `authored`。
`tests/precondition.rs::provision_never_creates_the_authored_tree_but_repairs_its_children_when_it_exists` 钉住补建不会复活产品目录；`src/layout.rs` 的单元测试钉住父先于子、只有 `.wiki/pages` 是 `authored`。

### 2.17 初始化是被动路径，不是开场仪式

`doctor --fix` 与它的两步后续（写项目指令、把 `ingest` 排成定时任务）只在新仓库第一次有收益；放进技能开场，每轮日常都要背一段用不上的指令，还会和 `init` 自己的输出抢同一份事实。所以技能没有「开场」一节：agent 正常跑命令，撞上 `E_NOT_INITIALISED` / `E_NO_CONFIG` 才回头建，并按 CLI 的 message 与 `doctor --fix` 输出末尾的两步走。
细节只写在 `src/init.rs` 的 `NEXT_STEPS`（默认周期、先问用户），技能只留触发条件与去向——两处都在讲同一件事，改一处必须改另一处；`tests/init.rs::init_ends_with_the_fixed_follow_up_prompt` 钉住 CLI 这一侧。

### 2.18 删命令前先和 spec 做一次差集

命令面收窄那一轮（22 → 5）把 `version` 整条删了，但 spec 的 §3 一直写着 `version list/diff/restore`，于是文档承诺了一条不存在的退路。**`.wiki/` 是唯一的版本与内容所在，而且通常不进 git**（2.12），所以 `restore` 不是「人可能用不上的过程步骤」，而是坏发布的唯一出口；过程步骤能删，出口不能删。
删任何一条命令或 flag 前，先 `grep` spec 的 §3 与 §4 去向表，差集里没处理干净的要么补实现、要么改 spec，不能只删代码。
`tests/cli_version.rs` 钉住三条：`submit` 不建版本、`restore` 不带 `--confirm` 时不动树、`restore` 本身也记一个 `rollback` 版本；`src/version.rs::approve` 的 `NothingToApprove` 在 CLI 层是 exit 0（重复 `submit` 不是错误）。

## 3. 提交前自查

```bash
cargo test
cargo clippy --all-targets
cargo fmt --check
```

文档与代码同一次提交；只改文档的提交也要在 message 里说明为什么改。
