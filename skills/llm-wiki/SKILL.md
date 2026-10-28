---
name: llm-wiki
description: 当用户要把仓库或一批文件变成可检索知识库、问「文档里怎么说的」「上次那个结论在哪」、要同步/体检/排障 llm-wiki 时使用。llm-wiki 只在隐藏的 .wiki/ 内写入，把仓库其余文件当只读源。本 skill 是唯一入口：判断意图、给命令序列、把 --json 翻成人话、在需要人拍板时停下；具体操作手册按需读 references/ 下的对应文件。
---

# llm-wiki

把仓库里的文件（Markdown / 网页 / PDF / 图片…）变成可全文检索的知识库：
仓库根（源）→ `.wiki/raw/`（转换出的 markdown）→ `.wiki/pages/`（一页一事的知识页）。

## 边界（动手前先记住）

- **只写 `.wiki/`。** 所有写入都在 `.wiki/` 内；`.wiki/` 之外的文件是源，只读——不改、不删、不移。
- 三层一条链：仓库根 → `.wiki/raw/` → `.wiki/pages/`。页面的 `sources` 指向 `.wiki/raw/...`，
  raw 的 frontmatter `source:` 指回仓库根。页面永远不直接引用源文件。
- `.wiki/` 里的 `index.md`、`log.md`、`audit/`、`versions/`、`state.db`、`staging/` 都是 CLI 的产物，只读。
- 手写 `[paths]` 无效：路径固定，写入无法被配置移出 `.wiki/`。

## 开场三步

1. `llm-wiki version` 确认命令可用；不在 PATH 上就用仓库根的 `.wiki/llm-wiki`。
2. `llm-wiki doctor --json`：`data.checks[]` 里 `level: fail` 的先修，只有 warn 可以继续。
3. 没有 `.wiki/wiki.toml` 就 `llm-wiki init`（可重复跑，不覆盖已有文件）。所有命令都在仓库根执行。

## 路由

先看用户要什么，再决定要不要读手册。拿不准就先问一句，不要猜着往下做。

| 用户说 | 先跑 | 再读 |
|---|---|---|
| 「搜一下」「上次那个结论在哪」 | `llm-wiki query --text "..." --json` | 不用读手册；但**答结论前先读 `hits[].page` 的正文** |
| 「有新文件了」「扫一下变了什么」 | `llm-wiki scan --json` | `references/scan.md` |
| 「把这份材料写进 wiki」 | `llm-wiki ingest begin --sources <key> --dry-run --json` | `references/ingest.md` |
| 「这份 PDF 解析得不对」 | `llm-wiki parse --key <key> --json` | `references/parse.md` |
| 「有没有矛盾」「体检」「哪些页是孤岛」 | `llm-wiki lint --json`、`llm-wiki graph --json` | `references/audit.md` |
| 「定时同步」「一批一批地跟」 | `llm-wiki sync --dry-run --json` | `references/sync.md` |
| 「退回上一版」「上次改坏了」 | `llm-wiki history --json`、`llm-wiki diff vNNN --json` | `references/automation.md` |
| 「这个文件老是变，别再跟了」 | `llm-wiki ignore check <path> --json` | `references/automation.md` |
| 「上次那批怎么样了」 | 停在这里 | 不读；走下面的决策门，等人选 |
| 任何命令报错、退出码看不懂 | `llm-wiki doctor --json` | `references/troubleshooting.md` |

## 最小可用序列

```bash
llm-wiki scan --json                    # 仓库根有什么新/变/删
llm-wiki convert --limit 20 --json      # 源 → .wiki/raw/
llm-wiki ingest begin --sources a.md --json   # 开一轮，看 affected
#   写候选页到 .wiki/staging/<type>/<id>.md
llm-wiki ingest commit --json           # 盖清单，等批
llm-wiki approve --confirm              # 人拍板后才进 .wiki/pages/
llm-wiki query --text "..." --json      # 检索
```

只做检索不需要前面任何一步；只想看变化，跑完 `scan` 就可以停，不会破坏任何东西。

## 需要人拍板的地方

退出码 3、暂存区有待批候选、`reason: page_budget` 或 `page_overlap`、或 `archive purge` 不带 `--yes` 时，**照抄这四个选项，不要自己发挥**：

1. 批准（`llm-wiki approve --confirm`）
2. 不批准并说明原因（`llm-wiki reject --reason "..."`）
3. 我自己补充要求（`llm-wiki instruct --text "..."`）
4. 先放着（`llm-wiki defer`）

只汇报数量与清单，不替人选。2 与 4 都不合并，差别只在留痕：2 写原因进日志，4 不写。

## 把 `--json` 说成人话

每条命令都可加 `--json`：stdout 一行 JSON，失败时 `ok:false`，错误码在 `error.code`。
汇报只讲结论与下一步，不贴原始 JSON。

| 拿到 | 说成 |
|---|---|
| `scan` 的 `changes[].kind` | `new` 新文件、`changed` 内容变了、`moved` 改名、`missing` 源没了、`duplicate` 内容重复、`oversize` 太大 |
| `lint` 的 `findings[]` | 「L6 在 `.wiki/pages/concept/x.md` 报 error：`sources` 指向不存在的文件」，按规则号归类，不逐条念 |
| `approve` 的 `NEEDS_CONFIRMATION` | 「N 页候选待批」，紧接着给上面的四选项 |
| `sync` 的 `converted` / `ingested` / `queued` / `failures` | 「转了 3、摄了 2、余 5」；`failures` 大于 0 必须点名失败的源 |
| `query` 的 `count` 与 `hits[].snippet` | 页面路径 + 标题 + 片段，并说明「要结论得读正文」 |

## 四条铁律

1. **簿记归 CLI。** `index.md`、`log.md`、页面的 `updated` 全部由 CLI 生成，不手写、不手改、不补日期。
2. **不主动汇报。** 自动跑完不发摘要、不发变更清单；用户问「变了什么」再给 `history` / `diff` 的结论。摄取的页要先列出来等人点头，那是用户在要求摄取，不是推送。
3. **先初始化，再干活。** 报 `E_NOT_INITIALISED` 就 `llm-wiki init`，不要手工造目录或契约文件。
4. **结论先读正文。** `query` 只给摘要与片段，片段不是答案。

## 退出码

| 码 | 含义 | 怎么做 |
|---|---|---|
| 0 | 成功；`query` 无命中、`doctor` 只有 warn 也是 0 | 继续 |
| 1 | 命令跑完但内容有问题（部分源失败、`lint` 有 error） | 报失败项，问是否处理 |
| 2 | 用法、配置或环境错误（含 `E_NOT_INITIALISED`） | 修环境：多半没在仓库根跑，或 `.wiki/wiki.toml` 有问题；先 `doctor` |
| 3 | 需要人拍板 | 走决策门，给四选项，停下 |
| 4 | 仓库被另一个进程锁住 | 等一会重试；**不要删 `.wiki/lock`** |

1 与 2 的分界：跑完了但内容有问题 = 1；根本没跑起来 = 2。别把 2 当 1 去改页面。
`sync --dry-run` 演练走同一套退出码，它不写盘。

## 按需阅读

| 文件 | 什么时候读 |
|---|---|
| `references/scan.md` | 要看差分、跑 `convert`、解释七种 `kind`、判断是不是摄取队列 |
| `references/ingest.md` | 要开一轮摄取、写页面正文、理解 `begin/commit/abort` 与 frontmatter 归属 |
| `references/parse.md` | 解析结果不对、要换后端、要看警告与外部服务 |
| `references/audit.md` | 跑 `lint`、做判断层分析（矛盾/过时/孤儿/重复）、出体检报告 |
| `references/sync.md` | 无人值守一轮、定时任务、退避与锁 |
| `references/automation.md` | 页面预算、整版回退（`history`/`diff`/`rollback`）、`.wiki/ignore` 忽略列表、`sync --only` |
| `references/troubleshooting.md` | 报错、退出码、`doctor` 检查项、旧布局残留在根目录 |
