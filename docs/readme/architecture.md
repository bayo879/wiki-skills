# 架构与模块

`llm-wiki` 是一个单二进制的仓库内工具：所有状态都在仓库目录里，没有服务端、没有数据库服务，唯一的外部依赖是可选的解析服务。本页写实际实现，命令用法见 [README](../../README.md)。

## 五层目录

| 层 | 位置 | 进 git | 内容 |
|---|---|---|---|
| 契约 | `wiki.toml`、`SCHEMA.md` | 是 | 配置与页面类型枚举；人和 agent 都读 |
| 源 | `source/` | 是 | 唯一输入；只读，工具从不在其下写文件 |
| 中间层 | `raw/` | 是 | 解析产物，可重建；frontmatter 记录来源、哈希、解析后端 |
| 知识层 | `wiki/` | 是 | 页面、`index.md`、`log.md`、`audit/`、`archive/` |
| 引擎 | `.wiki/` | 部分 | 账本 `state.db`、`staging/`、`versions/`、`sessions/`、`cache/`、`lock` |

`.wiki/.gitignore` 忽略可重建的状态（`cache/`、`staging/`、`sessions/`、`state.db*`、`lock`），保留 `versions/` 与 `next-instruction.md`。

## 流水线

```
source/ ──scan──▶ state.db ──convert──▶ raw/ ──ingest 轮次──▶ .wiki/staging/
                                                              │ approve --confirm
                                                              ▼
                     wiki/ ◀── lint / checkpoint ──▶ .wiki/versions/vNNN/
                       │
                       ├── sync：scan → convert → 门槛 → 审计（wiki/audit/<date>-sync.md）
                       ├── query：FTS5 索引（wiki/ 与 wiki/archive/）
                       ├── graph：链接图（只读派生，不落库）
                       └── archive seal → recall：分层记忆
```

1. **scan** 对比 `source/` 与账本表 `sources`，得出新增 / 修改 / 删除 / 改名。
2. **convert** 把源交给解析后端（`src/parse/`），写 `raw/<key>.md`，再登记 `sources` 行（记 `raw_key`）与 `raws` 行（记 `source_key`）。反查由这两列互指完成，v5 迁移已删除独立的 `source_pages` 表。
3. **ingest** 一轮读写：`begin` 把涉及的源与预判页面写进 `pending` 表，`commit` 把候选页与暂存清单对齐，`abort` 丢弃本轮但保留已写候选页。
4. **staging → wiki**：候选页放在 `.wiki/staging/<type>/<id>.md`；`approve` 只在 `--confirm` 下合并进 `wiki/` 并写 `log.md`、重建 `index.md`，`reject` 记录原因后清空暂存。
5. **lint** 跑十三条规则（`L1`–`L13`），`--fix` 只修不需要判断的机械缺陷，`--strict` 把警告也算失败。
6. **checkpoint** 把 `wiki/` 拷进 `.wiki/versions/vNNN/`，只保留最近 `[versions].keep` 份。
7. **sync** 无人值守串起 scan → convert → 稳定门槛 → lint，并把本轮写进审计文件；`--dry-run` 不写任何文件。
8. **archive / recall** 是分层记忆：agent 先写 `wiki/archive/<YYYY-MM>.md` 摘要，`seal` 校验账本、改写引用页的 `sources`、登记 `archive` 表；`recall` 子串检索摘要，`query` 用 FTS5 检索全库。
9. **query** 是派生索引：`page_meta` 存每页哈希，每次搜索前增量刷新，`--reindex` 全量重建。`page_meta` 有哈希但索引里没有该页时（例如 `approve` 刚发布的页），同样会补建索引。

## 模块

| 文件 | 职责 |
|---|---|
| `src/cli.rs` | clap 定义，只描述参数，不含业务 |
| `src/main.rs` | 命令分发、`--json` 与人类输出的分叉、退出码 |
| `src/config.rs` | `wiki.toml` 解析、默认值、`validate` |
| `src/init.rs` | 契约文件与引擎目录的初始内容 |
| `src/paths.rs` | 路径归一（`identity_key`）与安全拼接 |
| `src/scan.rs` | 两层对账（`source/`、`raw/` ↔ 账本）；唯一分类引擎与「账本认不认识这个 raw」的唯一答案 |
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
| `src/sync.rs` | 无人值守一轮 |
| `src/query.rs` | FTS5 索引与检索 |
| `src/archive.rs` | 摘要封存、清理与召回 |
| `src/graph.rs` | `[[wikilink]]` 结构：出入度、边、孤立页、断链 |
| `src/doctor.rs` | 十一项健康检查 |
| `src/hash.rs` | blake3 内容哈希 |
| `src/report.rs` | JSON / 人类输出信封与退出码 |

## 不变量

- 账本只由 CLI 写；`source/` 是唯一输入，`raw/` 与 `wiki/` 都是派生产物，但都进 git。
- 账本表只有 `src/store.rs` 能写；`sources` 与 `raws` 的业务写入口只有三处：`convert` 登记、`moves` 改键、`archive purge` 删行。想加第四处，先确认它不是前三种变更的另一种写法。
- 先文件后账本：`convert` 先原子写 `raw/`，再登记两张表，两步之间没有跨表事务。中断留下的缺口由 `scan`（`layer=raw` 的 `new`/`changed`）与 `lint` `L7` 暴露，所以「账本与磁盘暂时不一致」是被允许并会被检出的状态，不是靠事务禁止的状态。
- 对账只有一个答案：`scan::classify`（`source/`）与 `scan::classify_raw`（`raw/`）是唯一的分类引擎，「账本认不认识这个 raw」只由 `scan::RawLedger` 回答；`scan --layer raw`、`lint` `L7`、`ingest begin` 的 `orphan_pages` 都调它，不在调用点自建集合。`tests/reconcile.rs` 钉住这三处必须同答。
- 页面 `sources` 里的 raw 键必须在 `raws` 账本内；违反由 `ingest begin` 的 `orphan_pages` 报告，只报不修。
- 页面必须有 frontmatter 七字段：`id`、`title`、`type`、`summary`、`sources`、`status`、`updated`；`type` 取自 `[schema].types`。
- 候选页只能经 `.wiki/staging/` 由 `approve --confirm` 进入 `wiki/`；覆盖已存在页面同样需要确认。
- `archive seal` 先改引用页、后写账本：中途崩溃时重跑能补齐，不会留下引用已删记录的页面。
- `archive purge` 只在「总数超 `purge_cap`」且「超过 `purge_days`」时才删，且跳过仍被引用的摘要。
- 索引与暂存清单都是派生状态，可以从 `wiki/` 与账本重建；`--reindex` 是兜底。
