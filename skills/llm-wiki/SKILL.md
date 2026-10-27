---
name: llm-wiki
description: 当用户提到知识库、wiki、摄取材料、解析文件、体检矛盾或同步上游时使用。本技能是 llm-wiki 体系的总纲与调度层：先确认仓库已初始化，再按用户意图把工作转给 wiki-scan、wiki-parse、wiki-ingest、wiki-audit 或 wiki-sync 中的一个，单纯的检索则直接调用 query。它负责把命令行返回的 JSON 翻译成一句人能读懂的话，并在需要人拍板的地方给出标准的四选项问法。它不写页面正文，也不碰索引、日志与日期，这些簿记全部由命令行生成。
---

# llm-wiki

总纲。只做三件事：判断该调哪个子命令、该转给哪个下级 skill；把 `--json` 输出翻译成人话；
在需要人判断的地方停下来问。页面正文怎么写，归下级 skill。

## 结构：三层一条链

`source/` → `raw/` → `wiki/`，约定写在 `SCHEMA.md`。

- `source/`：上游同步来的原始文件，任意格式，**只读**。不改、不删、不移动。
- `raw/`：由 `source/` 转换出的 markdown，**只由 `llm-wiki convert` 写**。页面引用的是它。
- `wiki/`：人读的页面，**只由 `llm-wiki approve --confirm`** 从暂存区合并进来。
- 候选页先落在 `.wiki/staging/`；`approve` 之前，`wiki/` 里不会有它。
- `SCHEMA.md`：页面类型、必填字段、命名约定。写页之前先读它。

引用链固定为 page → raw → source：页面的 `sources` 指向 `raw/`，`raw/` 的 frontmatter
用 `source` 指回 `source/`。所以页面永远不直接引用 `source/`。

`wiki/` 里这几样是生成物，只能读：`index.md`（按类型分组的目录）、`log.md`（append-only 流水）、
`audit/`（审计报告）。`.wiki/versions/` 里的版本快照同样不手改。
用户问「目录怎么没更新」「日志缺一条」时，答案是让 CLI 重跑，不是去补文件。

## 开场第一步

1. `llm-wiki version` 确认命令在 PATH 上；跑不起来就先装，别拿别的办法拼凑。
2. `llm-wiki doctor --json`：取 `data.root` 当仓库根（未提供时就是当前目录），读 `data.checks` 看健康。
3. 没有 `wiki.toml` / `SCHEMA.md` 就 `llm-wiki init`；不要手工造目录、手写契约。
4. 所有命令都在这个根目录下跑——CLI 的根就是当前目录，它不向上查找。
5. `doctor` 有 Fail 就先修那一项；只有 Warn 可以带着往下走。
   - Fail 在 `config` / `schema`：`llm-wiki init --force` 重写两份契约（不动 `source/` 与页面）。
   - Fail 在 `paths` / `db`：多半目录被挪过或账本坏了，重跑 `llm-wiki init` 补齐目录再看。
   - Fail 在 `lock`：另一个进程正在跑，等它结束，别删 `.wiki/lock`。
   - Fail 在 `staging`：暂存区与清单不一致，先让人在批准 / 不批准里选一个，清掉队列再看。

## 路由

| 用户说 | 转给 | 说明 |
|---|---|---|
| 「有新文件了」「一批变了」 | `wiki-sync` | 一批、可无人值守；只想先看差分就转 `wiki-scan` |
| 「这份 PDF 解析得不对」 | `wiki-parse` | 解析失败、抽不出文本、要换后端 |
| 「把这份材料写进 wiki」 | `wiki-ingest` | 读源、议要点、建页、收尾 `checkpoint` |
| 「查一下有没有矛盾」「体检」 | `wiki-audit` | 跑 `lint`、读报告、做判断层分析 |
| 「搜一下」「上次那个结论在哪」 | 直接 `llm-wiki query` | 检索不需要下级 skill |
| 「上次那批怎么样了」 | 停在这里，走决策门 | 不转；等人在四个选项里选 |
| 「源文件删了 / 改名了」 | `wiki-sync` | 走 `scan` 的 `missing` / `moved`，别手工删页 |
| 「把上次会话封存掉」 | `wiki-ingest` | 封存只在人工会话收尾时做，不进定时任务 |
| 定时的批量跟进 | `wiki-sync` | `sync` 是它编排的一轮，总纲只负责转交 |

拿不准意图就先问一句，不要猜着往下做。

## 把 `--json` 翻译成人话

每条命令都可加 `--json`：stdout 只有一行 JSON；失败时 `ok:false`，错误码在 `error.code`。
汇报只讲结论和下一步，不贴原始 JSON。

| 拿到 | 说成 |
|---|---|
| `scan` 的 `changes[].kind` | `new` 新文件、`changed` 内容变了、`moved` 改名、`missing` 源没了、`duplicate` 内容重复、`oversize` 太大没读 |
| `lint` 的 `findings[]` | 「L6 在 `wiki/concept/x` 报 error：`sources` 指向不存在的文件」；按规则号归类，不逐条念 |
| `approve` 的 `NEEDS_CONFIRMATION` | 「N 页候选待批」，紧接着给下面的四选项 |
| `sync` 的 `converted` / `ingested` / `queued` / `failures` | 「转了 3、摄了 2、余 5」；`failures` 大于 0 必须点名失败的源 |
| `query` 的 `count` 与 `hits[].snippet` | 给页面路径、标题、命中片段，并补一句「要结论得读正文」 |

## 决策门：四个选项，原样给出

退出码 3（`NEEDS_CONFIRMATION`），或暂存区有待批准候选时，照抄这四个选项，**不要自己发挥**：

三个触发点：`approve` 不带 `--confirm`；暂存区里躺着一批待批候选；`archive purge` 不带 `--yes`。
三种情况都只汇报数量与清单，不替人选。

1. 批准（`llm-wiki approve --confirm`）
2. 不批准并说明原因（`llm-wiki reject --reason "..."`）
3. 我自己补充要求（自行输入指令：`llm-wiki instruct --text "..."`）
4. 先放着（暂不处理：`llm-wiki defer`）

2 与 4 对 `wiki/` 的状态效果相同：都不合并，已批准的页面一行不动。
差别只在留痕：2 把原因写进 `log.md`，4 只留一行 `defer`、没有任何原因。
所以有价值的分歧必须选 2——不写原因，下一轮还会在同一个地方重演。

## 三条铁律

1. **簿记归 CLI。** `index.md`、`log.md`、页面的 `updated` 全部由 CLI 生成。
   不手写、不手改、不补日期；要改索引，就去改页面 frontmatter，再让 CLI 重建。
2. **先汇报后落笔。** 摄取前先把要建、要改的页列出来（id + 一句 summary），等人点头。
   一份源最多建几页、该不该建，都先问，不先写。
3. **引用结论前先读页面正文。** `query` 只给摘要与片段，片段不是答案。
   报出任何结论前，把 `hits[].page` 对应的文件读一遍。

## 退出码

| 码 | 含义 | 怎么做 |
|---|---|---|
| 0 | 成功；`query` 无命中、`doctor` 只有 Warn 也是 0 | 继续 |
| 1 | 命令失败（含部分源失败），或 `lint` 有 error，`recall` 无命中 | 报失败项，问是否处理 |
| 2 | 用法、配置或环境错误 | 多半没在仓库根跑，或 `wiki.toml` 有问题；修环境，不要硬重试 |
| 3 | 需要人拍板 | 走决策门，给四选项，停下 |
| 4 | 仓库被另一个进程锁住 | 等一会儿重试；不要删 `.wiki/lock` |

1 与 2 的分界：命令跑完了但内容有问题（`lint` 报错、部分源转换失败）是 1；
命令根本没跑起来（配置坏了、没在仓库根）是 2。别把 2 当成 1 去改页面。
`sync` 的 `--dry-run` 演练同样走这套退出码，它不写盘。
