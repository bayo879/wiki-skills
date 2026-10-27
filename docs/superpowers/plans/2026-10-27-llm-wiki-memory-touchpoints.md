# 会话记忆 · 实现触点交底

> 契约：[设计稿 v10 §5.11](../specs/2026-10-27-llm-wiki-skills-design.md)。本页只交底，不改代码。
> 锚点以 HEAD `7496402` 为准。`src/ingest.rs`（T15，`7af14d6`）与 `src/lint.rs`（T18，`1dac87b`）都已落地并提交，`SCHEMA_VERSION` 仍是 3 —— 本批没有人和你抢 v4。

## 0 先看这条：lint 编号在两侧不一致

代码里已落地的 12 条规则，和设计稿 §5.4 的 12 条**只有 L6、L12 语义相同**。这不是笔误，是计划（[2026-10-27-llm-wiki-cli.md:1413](2026-10-27-llm-wiki-cli.md)）与 spec 各自演化出来的两套编号。

| 设计稿语义 | 代码里的号 | 代码里这个号实际是什么 |
| --- | --- | --- |
| L1 frontmatter 合法 | L1 + L4 | L1 必填字段缺失；L4 frontmatter 解析失败 |
| L2 断链 | L2 | 一致 |
| L3 孤儿页 | **未实现** | 被「同一 id 出现在两个 type 目录」占用 |
| L4 index 漂移 | **未实现** | 被「frontmatter 解析失败」占用 |
| L5 log 格式 | L11 | — |
| L6 sources 不存在 | L6 | 一致（只是不查哈希） |
| L7 无源声称 | **未实现** | 被「raw 未被 convert 记录」占用 |
| L8 近似重复 | **未实现** | 被「正文低于 min_page_chars」占用 |
| L9 空页占位 | L8 | 代码 L9 是「字段变了 updated 没变」 |
| L10 已摄取无页引用 | **未实现** | 被「同一 raw 被 >3 页引用」占用 |
| L11 updated 早于源 | 近似 L9 | — |
| L12 未引用 asset | L12 | 一致 |
| **L13 归档零入链（本批新增）** | 空号 | 两侧都未占用，可以安全使用 |

结论：**按检查内容对齐，不要按号对齐**。下文提到「空页规则」一律写「现 L8 / 目标 L9」，不写裸编号。编号统一是独立的一件事，不要夹在这批里做。

## 1 范围

做五件：archive 表、`archive status/seal/purge`、`recall`、`[memory]` 配置段、lint 三处改动。
不做：自动蒸馏、自动封存、OCR、让 sync 参与封存。这四件在 §11 里明确划到 v2 之后。

## 2 触点表

| # | 文件 | 现状锚点 | 要改什么 | 风险 |
| --- | --- | --- | --- | --- |
| 1 | `src/store.rs` | `SCHEMA_VERSION = 3`（:19）；`migrate()`（:262）逐版本升；`MIGRATION_1/2/3`（:22/:66/:80）；`apply_migration(&mut self, sql, version)`（:300）把 DDL 与版本戳放进同一事务 | 加 `MIGRATION_4` 与 `if version < 4 { self.apply_migration(MIGRATION_4, 4)?; }` | 和 T15 抢版本号，见 §5 |
| 2 | `src/config.rs` | `Config{paths,formats,schema,parse,ingest,audit,versions,sync}`（:60-68）；`IngestCfg`（:326）、`AuditCfg`（:343） | 加 `pub memory: MemoryCfg`，四个字段见 §3 | 无 |
| 3 | `src/cli.rs` | `Command` 变体 Version/Init/Scan/Convert/Parse/Ingest/Lint；子命令枚举 `IngestCommand` 是现成范式 | 加 `Archive{command}`、`Recall{query, full, json}`，加 `ArchiveCommand` 枚举 | 无 |
| 4 | `src/main.rs` | `dispatch`（:46-58）只有 7 个分支 | 加分支；`archive`/`recall` 都不需要 store 之外的新上下文 | 无 |
| 5 | `src/render.rs` | `page_files`（:300）→ 被 `index_pages`（:271）与 `lint::load_pages`（:268）**共用**；`is_bookkeeping`（:339）已排除根 `index.md`/`log.md` 与整个 `audit/` | 只在 `index_pages`（:271）里跳过首段为 `archive` 的路径 | 见 §6 的坑 |
| 6 | `src/lint.rs` | `check_missing_sources`（:425）、`check_short_pages`（:459）、`is_asset`（:628，读 `front.extra`） | 前三处改动 + 新增 L13 函数 | 增量，不动既有编号 |
| 7 | `src/page.rs` | `FrontMatter`（:74-93），最后一个字段 `extra: Vec<(String, String)>`（:92） | **不用改** | `origin`/`archived` 自动落进 `extra` |
| 8 | `src/ingest.rs` | `commit`（:297）已有现成范式：`staging::source_keys(&front.sources, prefix)`（:330）解析引用、`page::append_sources`（:334）追加、改页后 `store.upsert_page_meta`（:346） | seal 复用前两个，但要的是**替换**不是追加，见 §5 | 漏 `upsert_page_meta` 会让 lint 的空页/未更新规则误报 |
| 9 | `docs/.../SCHEMA.md` | — | archive 不进 index、`../../source` 相对深度、`origin`/`archived` 语义 | — |

`src/scan.rs` 不用改：扫描根是 `source/` 与 `raw/`，`.wiki/` 根本不在其下，`sessions/` 不需要排除逻辑。

## 3 配置与表

```toml
[memory]
session_soft_cap = 50   # L0 会话条目水位，超了提示蒸馏
idle_days = 60          # L2 封存摘要多久没人碰就进清理候选
purge_cap = 200         # L2 摘要条数上限
purge_days = 180        # 清理候选的期限
```

四个默认值是拍的，等真实使用节奏再校（见 §8）。

```sql
-- v4
CREATE TABLE archive(
    key TEXT PRIMARY KEY,          -- repo 相对路径 wiki/archive/<id>.md
    period_start TEXT NOT NULL,    -- YYYY-MM-DD
    period_end TEXT NOT NULL,
    sources_json TEXT NOT NULL,    -- 重挂后的 sources 快照
    sealed_at TEXT NOT NULL,
    sealed_by TEXT NOT NULL,       -- human | agent
    last_recalled_at TEXT,         -- recall 命中时回写，purge 的第二条件靠它
    recall_count INTEGER NOT NULL DEFAULT 0
);
```

**不进表的东西**：`archive status` 的第四个数字「封存后仍被引用的归档数」现算 —— 扫 `wiki/**/*.md` 的 `[[...]]` 反查指向 `archive/` 的页面。理由：它只是自证指标，为它建表就等于要让每次改页维护一张反链表，成本大于收益。query 上线后若发现现算太慢再议。

## 4 四条命令的行为边界

| 命令 | 成功 | 退出码 |
| --- | --- | --- |
| `archive status` | 四个数：摘要总数、L0 条目数、清理候选数、封存后仍被引用数 | 0 |
| `archive seal --sources <k>... --title <t>` | 写 `wiki/archive/<id>.md` 摘要 + archive 行 + 重挂引用页 sources + checkpoint | 摘要已存在 2；sources 里有未入账 key 2；拿不到锁 4 |
| `archive purge --dry-run` / `--yes` | 只清「已被摘要覆盖 + 过期」的源：删 `source/` 原文、`raw/` 派生、`sources`/`raws` 账本行，并写 checkpoint | 无 `--yes` 且非 `--dry-run` → 3 |
| `recall <kw> [--full]` | 命中摘要，默认返回蒸馏正文，`--full` 才取原摘要全文 | 未命中 1 |

CLI 不写页面正文这条不破例：`seal` 的摘要正文由 agent 写好交给 CLI，CLI 只做校验、落账本、重挂 sources、写 checkpoint。

## 5 顺序与撞车

1. `[memory]` + v4 迁移 —— 无依赖，可先做。T15/T18 都没加表，v4 目前空闲。
2. `cli.rs` + `main.rs` 骨架 —— 依赖 1。
3. `archive seal/purge/status` —— 依赖 1、2。sources 重挂见下方「追加 ≠ 重挂」。
4. lint 三条 —— 依赖 3（没有归档页就测不出来）；编号漂移先按 §0 对齐。
5. `recall` —— 依赖 3，但不依赖 FTS（独立只读扫描，扫 `wiki/archive/`），可以并行。
6. SCHEMA.md 与 skill 文案 —— 最后。

### 追加 ≠ 重挂

`page::append_sources`（`src/page.rs:244`）做的是 splice：只改 `sources` 字段那几个字节，不重新序列化 frontmatter，支持内联数组 / 块序列 / 裸标量 / 无字段四种形态。这个机制正是 seal 要的，但**语义不对** —— 追加只增不减，重挂要求把被清掉的那组引用换成 `wiki/archive/<id>.md`。

要加的是 `page::replace_sources(p, remove: &[String], add: &[String])`，复用同一套 splice，不要图省事用 `serde` 重写全文：那会把 `extra` 顺序和用户手写的格式一起冲掉。

去重与片段：重挂时按 `staging::source_keys`（`src/ingest.rs:330`）归一后的 key 比对；被移除项若带 `#p=12-40` 片段，片段随原文一起丢弃 —— 归档摘要是整篇蒸馏文，没有分块概念。

改完页必须 `store.upsert_page_meta`（`src/ingest.rs:346` 是现成调用），否则 lint 会认为「页面变了但 `updated` 没变」。

## 6 一个具体的坑：别在 `is_bookkeeping` 里排 archive

`is_bookkeeping`（`src/render.rs:339`）看着正是「目录级排除」该待的地方 —— `audit/` 就是这么被排除的。但它服务的是 `page_files`（:300），而 `page_files` 同时被 `index_pages`（:271，建 index 用）和 `lint::load_pages`（`src/lint.rs:268`，校验用）调用。

在 `is_bookkeeping` 里排除 `archive/` 会同时让 lint 看不见归档页，后果：

- L6 归档豁免变成死代码；
- L13「归档零入链」根本无从实现，因为页面压根没被加载；
- 归档页的 frontmatter/断链也不再有校验，而它恰恰是清理后唯一的溯源证据。

正确位置是 `index_pages` 里跳首段为 `archive` 的路径，三行。index 排除、lint 包含，两个消费者必须分开。

## 7 验收断言

- `archive seal` 之后：引用页的 `sources` 全部指向 `wiki/archive/`；`lint` 无 L6 error；`lint --strict` 退出 0。
- `archive purge --yes` 之后：`scan` 不再报该源；`lint` 仍无 L6；`recall` 仍取得到摘要。
- `write_index` 输出不含 `archive/` 条目；`lint --fix` 不会因为 index 漂移反复报（`index_text` 与磁盘一致，见 `src/render.rs:265`）。
- v3 库升 v4：既有 8 张表数据不动，`user_version` = 4；v4 库被旧二进制打开走 `SchemaTooNew`（`src/store.rs:264`）。

## 8 未定

- `[memory]` 四个阈值等真实节奏校准；`session_soft_cap = 50` 若一天十来个会话会一周就喊，从提醒退化成噪音。
- lint 编号统一（§0 那张表）单独排，不要混进这批。
