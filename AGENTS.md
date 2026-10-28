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

`scan::RawLedger` 负责这个判断，`lint` 的 L7 与 `ingest begin` 的 `orphan_pages` 都必须调它，不许各自再写一遍 `raws` 表的 join——以前就是三处各写一遍，才出现「L7 提示的修法根本跑不通」。
`tests/reconcile.rs` 钉住三处同答。

### 2.3 技能表格与 CLI 输出是契约

`skills/*/SKILL.md` 里的枚举表、字段表带标记（`<!-- enum: src/scan.rs::Layer -->`、`<!-- fields: ingest-begin -->`），`tests/docs_contract.rs` 拿真机 JSON 快照逐项比对。
改 CLI 输出形状 = 同时改技能表 + 更新 `tests/snapshots/cli-keys.json`（`LLM_WIKI_UPDATE_SNAPSHOTS=1` 重生成）。只改代码不改文档，测试会红。

### 2.4 schema 迁移必须与全新库同构

`SCHEMA_VERSION` 与 `MIGRATION_N` 一一对应，空库直建的 schema 和从 v1 一路升上来的 schema 必须完全一致（`src/store.rs` 里的 `schema_dump` 测试）。
加迁移时同时加 `MIGRATION_{n}` 与 `apply_migration(MIGRATION_{n}, n)` 调用，`tests/invariants.rs::migrate_steps_through_every_schema_version` 会核对个数。

### 2.5 文档一致性由机器守，不靠人记

四类守卫，改文档前先知道它们在管什么：

| 守卫 | 抓什么 |
|---|---|
| `tests/invariants.rs` | SQL 单点、账本单入口、对账唯一、不变量都有测试、lint 规则三方一致 |
| `tests/docs_contract.rs` | flag 存在、枚举/字段表与真机输出一致、闭世界词表（文档不许出现代码里没有的名字） |
| `tests/snapshots/cli-keys.json` | 每条命令输出的键路径 |
| `tests/docs_tokens.allow` | 词表白名单，加名字要有理由 |

诚实边界写在 [architecture.md 的「机器守卫」节](docs/readme/architecture.md)：它保证「文档没写错名字」，不保证「文档写得够全」。

### 2.6 技能的价值在流程纪律，不在用法说明

用 A/B（读技能 vs 裸跑 CLI）实测过四个任务：技能的增量集中在顺序与门禁——先 `convert --key` 再 `ingest begin`、候选页只能落 `.wiki/staging/`、`approve --confirm` 必须人做；
flag 与字段的解释与 `--help`、`init` 生成的 `SCHEMA.md` 高度重复，加长技能文本收益很低，反而会和 CLI 漂移。
所以：技能里写「什么时候做什么、做错了怎么判」，不写「这个参数是什么意思」。

## 3. 提交前自查

```bash
cargo test
cargo clippy --all-targets
cargo fmt --check
```

文档与代码同一次提交；只改文档的提交也要在 message 里说明为什么改。
