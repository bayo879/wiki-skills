---
name: wiki-scan
description: 当用户说「同步过来一批新文件」「扫一下有什么变化」「这些源进 wiki 了吗」，或定时任务要在摄取前确认 source/ 的增删改时使用。它只调用 llm-wiki scan 与 llm-wiki convert：先读 source/ 层差分，再分批把待转换源写成 raw/*.md，转换后再 scan 一次核对 raw 层与账本是否一致；同时解释 new、changed、moved、duplicate、missing、oversize、ignored 七类结果，报告失败源，并把需要成页的清单交给 wiki-ingest。不写页面、不改配置、不手改 raw/。
---

# wiki-scan

命令是 `llm-wiki`；不在 PATH 上时，用仓库根的 `.wiki/llm-wiki`。

把 `source/` 的变化翻成两句话：哪些要转换，哪些源变了该让下游复核。只调 `llm-wiki scan` 和 `llm-wiki convert`，其余判断交给下游。

## 何时用

- 用户说「同步过来一批新文件」「扫一下有什么变化」「这些文件进 wiki 了吗」。
- 准备摄取前，先确认待转换清单，别把 `raw` 层的变化当摄取任务。
- `convert` 报错，要判断是单个源坏了还是整批坏了。

不接：写页面、写正文、改配置、解析调试、lint、无人值守编排。那些属于 `wiki-ingest`、`wiki-parse`、`wiki-audit`、`wiki-sync`。

## 两层，一次读

`source/` 是原始事实，`raw/` 是转换产物。变更判据落在 `raw/`：`source/` 只改了元数据、或解析器只是重排换行，只要 `raw/*.md` 的内容哈希没变，就不该重新摄取。

一次 `llm-wiki scan` 同时比两层，结果合并在同一个 `data.changes[]` 里，每条带 `layer` 字段。**按 `layer` 过滤，不要靠 key 猜**——两层共用同一 key 空间。

| layer | 比什么 | 变化意味着 |
|---|---|---|
| `source` | `source/` ↔ `sources` 账本 | 待转换清单，交给 `convert` |
| `raw` | `raw/` ↔ `raws` 账本 | raw 被手改、丢失，或账本被重建；**不是待摄取队列** |

摄取队列不来自 `scan`：把 `layer: "source"` 的 `new` / `changed` key 交给 `wiki-ingest`，由它用 `ingest begin --dry-run` 算这一轮真正会碰的页。

固定三步，顺序不能换：

1. `llm-wiki scan --json`：取 `layer: "source"` 且 `kind` 为 `new` / `changed` 的 key，这是待转换清单。
2. `llm-wiki convert --limit 20 --json`：把源写成 `raw/*.md`。
3. 再跑一次 `llm-wiki scan --json`：这回只剩 `layer: "raw"` 的结果要管。正常情况下它是空的——`convert` 会把 raw 一并记账。非空说明 raw 被手改、被删，或账本是重建的，单独报给人，不要当摄取任务派下去。

## 命令

以下命令都在仓库根（含 `wiki.toml` 的目录）执行。

读差分：

```bash
llm-wiki scan --json
```

`data.changes[]` 每条五个字段：`kind`、`layer`（`"source"` 或 `"raw"`）、`key`（相对该层根目录，用 `/` 分隔）、`hash`（`blake3:...`；`missing` 与 `oversize` 为 null）、`moved_from`（只有 `moved` 有值，其余为 null）。`ignored` 不进数组，只看 `data.ignored_count`。

分批转换：

```bash
llm-wiki convert --limit 20 --json
```

`data.converted[]` 每条有 `raw_path`、`chars`、`backend`、`warnings`、`skipped`、`skip_reason`。`skipped: true` 且 `skip_reason: "pinned"` 表示该 raw 被人工固定，没有被覆盖。

指定单个源，已转换也会重转：

```bash
llm-wiki convert --key notes/a.md --json
```

首次接入或积压很多时，不要一次吞几千个。每批 10–20 个，重复跑到返回 0 个待转换（`data.converted` 为空）；`--limit` 默认 10，别省略。

每批之间看一眼 `warnings` 和失败项。连续几批都有错就停下来报告，不要盲目跑完。

## 七种 kind 怎么讲

同一种 kind 在两层意思不同：`layer: "raw"` 的 `new` / `changed` 是完整性信号，不是摄取信号。

| kind | 人话 | 该不该动手 |
|---|---|---|
| `new` | 账本没见过的源 | 可以转换，可以摄取 |
| `changed` | 同一 key 的内容变了 | 相关页面可能过时；列出来让人判断，**不要自动改写已有结论** |
| `moved` | 改名或移动了，内容没变 | CLI 已迁移引用，**不需要重新摄取** |
| `duplicate` | 同内容出现在多条路径 | 只认 key 排序最靠前的那条，其余忽略，不重复建页 |
| `missing` | 账本里有、磁盘上没了 | 页面保留并标 stale，**不要删页** |
| `oversize` | 超过 `max_source_bytes` | 被拒，`convert` 拿不到它。想看内容用 `llm-wiki parse --key <key> --force-large`（不落盘），再决定调配置还是换源 |
| `ignored` | 扩展名不在白名单，或路径点开头 | 只计数，不处理 |

解释时先报数量再报条目，例如「3 个新源、1 个改名、2 个缺失」，再逐条给 key。不要把七类混成一句「有变化」。

## 失败处理

`convert` 逐源执行：某个源失败会带着错误码退出，已经写好的 `raw/` 和账本不受影响，没轮到的 key 也还在待转换清单里。所以单源失败只算一条 finding，不是整批作废，补跑即可。

- 把失败的 key 和 `error.code` 原样贴给用户，例如 `notes/a.pdf: E_NO_PARSER`；不要改写成「转换失败」。
- 同一个源最多重试一次；再失败就停下，把错误码交给用户或 `wiki-parse`，不要循环重试。
- 补跑剩余的 key 用 `llm-wiki convert --key <key> --json` 逐个来；不要为了跳过坏源去改配置或删文件。
- 常见码：`E_NO_PARSER`（无后端认领扩展名）、`E_BAD_SOURCE`（解不开）、`E_OVERSIZE`（超限）、`E_EXTERNAL`、`E_BOTH_FAILED`（auto 下内外都失败）、`E_RAW_COLLISION`（raw 路径被另一个源占用）。
- 整批都没动、只报 `E_LOCKED`：另一个进程持锁，等它结束再跑，不要并发硬跑。

## 交接

- 转换完成、`layer: "raw"` 的结果确认为空后，把 `layer: "source"` 的 `new` 与 `changed` key 交给 `wiki-ingest`；`moved`、`duplicate`、`missing`、`ignored` 不进摄取队列。
- 源是 `changed` 时，「`raw/` 相对上次批准变了没有」CLI 不单独列出。按「变了就复核受影响页」处理：`llm-wiki ingest begin --sources <key> --dry-run --json`，看 `affected` 里是哪几页。真漂了会有硬信号：`approve` 以 `E_STAGING_STALE` 拒绝合并。
- 用户只想看看变了什么：跑完 `scan`（必要时 `convert`）就停。**到这里就可以停，不摄取也不会破坏任何东西**——账本、`raw/`、`wiki/` 都保持一致。
- 待摄取超过几十条时，先报总数并问要不要分批；不要默认全量摄取。

## 纪律

- 不手改 `raw/` 下的文件。`raw/` 是 `convert` 的产物，手改会在下次转换被覆盖。
- 唯一例外：确实要修正抽取质量（多栏错位、表格错乱）时可以手改，但必须把该文件 frontmatter 的 `pin` 改成 `true`，并告诉用户「`convert` 以后会跳过它，源再变也不会自动更新」。没 `pin` 的修改一律视为临时。
- 不手写 `wiki/index.md`、`wiki/log.md` 和页面的 `updated`；那些由 CLI 生成。
- 不改 `wiki.toml`、`SCHEMA.md`。需要调 `max_source_bytes`、`formats.allow` 时，说明影响后让用户改。
- `scan` 只读，可以反复跑；`convert` 会写 `raw/` 与账本，同一批不要并发跑。
