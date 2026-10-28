# 架构与模块

`llm-wiki` 是一个单二进制的仓库内工具：所有状态都在仓库目录里，没有服务端、没有数据库服务，唯一的外部依赖是可选的解析服务。本页写实际实现，命令用法见 [README](../../README.md)。

## 一个隐藏目录，四层

| 层 | 位置 | 进 git | 内容 |
|---|---|---|---|
| 源 | 仓库根（`.wiki/` 之外） | 是 | 唯一输入；只读，工具从不在其下写文件 |
| 契约 | `.wiki/wiki.toml`、`.wiki/SCHEMA.md` | 否 | 配置与页面类型枚举；人和 agent 都读 |
| 中间层 | `.wiki/raw/` | 否 | 解析产物，可重建；frontmatter 记录来源、哈希、解析后端 |
| 知识层 | `.wiki/pages/` | 否 | 页面、`index.md`、`log.md`、`audit/`、`archive/` |
| 引擎状态 | `.wiki/` 其余 | 否 | 账本 `state.db`、`staging/`、`versions/`、`sessions/`、`cache/`、`lock` |

`.wiki/.gitignore` 只有一行 `*`：整个引擎都不进 git。仓库里进 git 的只有源文件；工具不修改根 `.gitignore`。
写入边界是结构性的：`Config.paths` 不从配置反序列化（`#[serde(skip)]`），固定为 `source=""`、`raw=".wiki/raw"`、`wiki=".wiki/pages"`、`engine=".wiki"`，手写 `[paths]` 移不出去。
扫描根是仓库根；点目录天然跳过，`[scan].ignore` 再加一层目录黑名单（默认 `node_modules`、`target`、`dist`、`build`、`vendor`、`.git`）。

## 流水线

```
仓库根 ──scan──▶ state.db ──convert──▶ .wiki/raw/ ──ingest 轮次──▶ .wiki/staging/
                                                                   │ approve --confirm
                                                                   ▼
              .wiki/pages/ ◀── lint / checkpoint ──▶ .wiki/versions/vNNN/
                    │
                    ├── sync：scan → convert → 门槛 → 审计（.wiki/pages/audit/<date>-sync.md）
                    ├── query：FTS5 索引（.wiki/pages/ 与 .wiki/pages/archive/）
                       ├── graph：链接图（只读派生，不落库）
                       └── archive seal → recall：分层记忆
```

1. **scan** 对比仓库根与账本表 `sources`，得出新增 / 修改 / 删除 / 改名。
2. **convert** 把源交给解析后端（`src/parse/`），写 `.wiki/raw/<key>.md`，再登记 `sources` 行（记 `raw_key`）与 `raws` 行（记 `source_key`）。反查由这两列互指完成，v5 迁移已删除独立的 `source_pages` 表。
3. **ingest** 一轮读写：`begin` 把涉及的源与预判页面写进 `pending` 表，`commit` 把候选页与暂存清单对齐，`abort` 丢弃本轮但保留已写候选页。
4. **staging → pages**：候选页放在 `.wiki/staging/<type>/<id>.md`；`approve` 只在 `--confirm` 下合并进 `.wiki/pages/` 并写 `log.md`、重建 `index.md`，`reject` 记录原因后清空暂存。
5. **lint** 跑十三条规则（`L1`–`L13`），`--fix` 只修不需要判断的机械缺陷，`--strict` 把警告也算失败。
6. **checkpoint** 把 `.wiki/pages/` 拷进 `.wiki/versions/vNNN/`，只保留最近 `[versions].keep` 份。
7. **sync** 无人值守串起 scan → convert → 稳定门槛 → lint，并把本轮写进审计文件；`--dry-run` 不写任何文件。
8. **archive / recall** 是分层记忆：agent 先写 `.wiki/pages/archive/<YYYY-MM>.md` 摘要，`seal` 校验账本、改写引用页的 `sources`、登记 `archive` 表；`recall` 子串检索摘要，`query` 用 FTS5 检索全库。
9. **query** 是派生索引：`page_meta` 存每页哈希，每次搜索前增量刷新，`--reindex` 全量重建。`page_meta` 有哈希但索引里没有该页时（例如 `approve` 刚发布的页），同样会补建索引。

## 模块

| 文件 | 职责 |
|---|---|
| `src/cli.rs` | clap 定义，只描述参数，不含业务 |
| `src/main.rs` | 命令分发、`--json` 与人类输出的分叉、退出码 |
| `src/config.rs` | `.wiki/wiki.toml` 解析、默认值、`validate`；路径固定不可配 |
| `src/init.rs` | 契约文件与引擎目录的初始内容 |
| `src/paths.rs` | 路径归一（`identity_key`）与安全拼接 |
| `src/scan.rs` | 两层对账（仓库根、`.wiki/raw/` ↔ 账本）；唯一分类引擎与「账本认不认识这个 raw」的唯一答案 |
| `src/convert.rs` | 转换一轮的调度与写盘 |
| `src/parse/` | 解析后端与 `auto` 回退（`text`/`html`/`asset`/`pdf`/`external`） |
| `src/chunk.rs` | 大文件的按需分块 |
| `src/page.rs` | 页面 frontmatter 的读写与校验 |
| `src/render.rs` | `index.md`、`log.md` 与人类可读渲染 |
| `src/ingest.rs` | 一轮 ingest 的账本操作 |
| `src/staging.rs` | 暂存区与清单 `pending.json` |
| `src/lint.rs` | 十三条规则与 `--fix` |
| `src/version.rs` | 版本快照与保留策略 |
| `src/lock.rs` | 仓库锁（TTL 600s） |
| `src/store.rs` | SQLite 账本、schema 迁移、FTS5 |
| `src/ledger.rs` | 账本唯一的写入口：登记转换、跟随改名、遗忘删除 |
| `src/sync.rs` | 无人值守一轮 |
| `src/query.rs` | FTS5 索引与检索 |
| `src/archive.rs` | 摘要封存、清理与召回 |
| `src/graph.rs` | `[[wikilink]]` 结构：出入度、边、孤立页、断链 |
| `src/doctor.rs` | 十一项健康检查 |
| `src/hash.rs` | blake3 内容哈希 |
| `src/report.rs` | JSON / 人类输出信封与退出码 |

## 不变量

- 账本只由 CLI 写；仓库根是唯一输入，`.wiki/raw/` 与 `.wiki/pages/` 都是派生产物，且都与账本一起留在 `.wiki/` 里。**任何命令都不写 `.wiki/` 之外**（检查：tests/e2e.rs::init_scan_convert_ingest_publish_lint_checkpoint_query_sync_archive_recall）（检查：tests/invariants.rs::only_store_writes_sql）
- 账本表只有 `src/store.rs` 能执行 SQL，业务写入只有 `src/ledger.rs` 一个入口：`convert` 登记、`moves` 改键、`archive purge` 删行都表达成 `ledger::Ledger` 的一次调用，顺序与失败语义写在 `ledger.rs` 的注释里。想加第四个意图，加 `Ledger` 的方法，不要绕回 `store`。（检查：tests/invariants.rs::only_ledger_writes_the_ledger）
- 先文件后账本：`convert` 先原子写 `.wiki/raw/`，再登记两张表，两步之间没有跨表事务。中断留下的缺口由 `scan`（`layer=raw` 的 `new`/`changed`）与 `lint` `L7` 暴露，所以「账本与磁盘暂时不一致」是被允许并会被检出的状态，不是靠事务禁止的状态。（未机器化：没有崩溃注入测试，无法证明两次写入的先后）
- 对账只有一个答案：`scan::classify`（仓库根）与 `scan::classify_raw`（`.wiki/raw/`）是唯一的分类引擎，「账本认不认识这个 raw」只由 `scan::RawLedger` 回答；`scan` 输出的 `layer` 字段、`lint` `L7`、`ingest begin` 的 `orphan_pages` 都调它，不在调用点自建集合。`tests/reconcile.rs` 钉住这三处必须同答。（检查：tests/reconcile.rs::an_unledgered_raw_is_reported_the_same_way_by_all_three）
- 页面 `sources` 里的 raw 键必须在 `raws` 账本内；违反由 `ingest begin` 的 `orphan_pages` 报告，只报不修。（检查：tests/ingest.rs::begin_reports_orphan_pages）
- 页面必须有 frontmatter 七字段：`id`、`title`、`type`、`summary`、`sources`、`status`、`updated`；`type` 取自 `[schema].types`。（检查：tests/lint.rs::l1_reports_each_missing_required_field）
- 候选页只能经 `.wiki/staging/` 由 `approve --confirm` 进入 `.wiki/pages/`；覆盖已存在页面同样需要确认。（检查：tests/version.rs::approve_without_confirm_asks_a_human）
- `archive seal` 先改引用页、后写账本：中途崩溃时重跑能补齐，不会留下引用已删记录的页面。（未机器化：靠 seal 的幂等实现，没有崩溃注入测试）
- `archive purge` 只在「总数超 `purge_cap`」且「超过 `purge_days`」时才删，且跳过仍被引用的摘要。（检查：tests/archive.rs::purge_candidates_need_capacity_and_deadline_both）
- 索引与暂存清单都是派生状态，可以从 `.wiki/pages/` 与账本重建；`--reindex` 是兜底。（检查：tests/query.rs::reindex_is_what_makes_a_new_page_searchable）

## 机器守卫

上面每条不变量都在 CI 里有执行点，按「被违反时哪一层会红」分工：

| 文件 | 抓什么 |
| --- | --- |
| `tests/invariants.rs` | 代码结构：SQL 只在 `store.rs`、账本写入只在 `ledger.rs`、对账只有 `scan`、不变量标注指向真实测试、迁移步数等于 `SCHEMA_VERSION`、`L1`–`L13` 三方一致 |
| `tests/docs_contract.rs` | 文档与 CLI 一致：JSON 形状快照、flag 真存在、表格与枚举等于代码、被点名对象的字段在对应技能里有枚举行、文档里的 `snake_case` 名字与 `E_*` 码闭世界 |
| `tests/snapshots/cli-keys.json` | 各命令 `--json` 的键路径。改输出必须改快照；置 `LLM_WIKI_UPDATE_SNAPSHOTS=1` 重跑 `tests/docs_contract.rs` 会重写它，之后人工过一遍 |
| `tests/docs_tokens.allow` | 白名单：文档里合理出现、但不是本仓词汇的名字，每条写理由 |

行为层面的守卫在不变量行尾点名（`tests/lint.rs`、`tests/version.rs`、`tests/archive.rs`、`tests/ingest.rs`、`tests/query.rs`、`tests/reconcile.rs`）。

诚实边界：快照只证明「键还在」，不证明语义对；枚举与字段检查要求文档保持表格/枚举行形态，改写成散文即失效；闭世界词表只证明「这个名字在代码里出现过」，抓不到「代码里有但已废弃」；`invariants.rs` 是文本扫描，注释里出现针脚也会触发（宁可误报）。
