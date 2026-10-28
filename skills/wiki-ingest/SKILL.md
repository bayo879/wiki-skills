---
name: wiki-ingest
description: 当需要把已转换的 raw 源并进 wiki 时使用。它驱动 ingest 的 begin、commit、abort，并亲手撰写候选页正文，把源里的结论落成一页一事、可追溯来源的知识页。它只写 .wiki/staging/ 下的候选页，进 wiki 要人确认。典型场合是新源入库、旧源更新后结论可能被改写、多源讲同一件事需要合并成页。它不扫描、不转换、不调试解析、不跑 lint。动笔前先汇报要建哪些页、改哪些页和各页要点，等人点头后才提交。
---

# wiki-ingest

命令是 `llm-wiki`；不在 PATH 上时，用仓库根的 `.wiki/llm-wiki`。

把**已经转成 `raw/*.md` 的源**写成 wiki 页。本 skill 只做两件别人不做的事：驱动 `ingest begin/commit/abort`，以及**亲手写页面正文**。

边界：源没进来回 `wiki-scan`，页读不对回 `wiki-parse`，发布前的审计回 `wiki-audit`。

## 0. 前置

```bash
llm-wiki doctor --json
```

- 用它取仓库根，确认 `.wiki/state.db` 可用。
- 动笔前先读仓库根的 `SCHEMA.md`：类型枚举、必填字段、`status` 三态、图片路径都以它为准，本 skill 不再重述。
- 源必须是 `raw/<key>.md`，且已在账本里。没有就先 scan 和 convert，回 `wiki-scan`。
- `begin` 只认账本里的源；不在账本报 `source ... is not in the ledger`。
- 源在这一轮之后还会变的话，先把它转换固定下来再开始。

## 1. 开一轮：begin

先空跑，看这一轮会碰到哪些页：

```bash
llm-wiki ingest begin --sources order-timeout.md --dry-run --json
```

看清楚了再落：

```bash
llm-wiki ingest begin \
  --sources order-timeout.md retry-policy.md \
  --pages concept/order-retry \
  --json
```

- `--sources` 是账本 key，写 `order-timeout.md` 或 `raw/order-timeout.md` 都行。
- **新页必须写进 `--pages`**。不写，`begin` 不会列出它，这一轮就不会建它；写了才作数。
- 已存在的页也可以写进 `--pages`，会跟扫码结果合并。
- `--dry-run` 只算不写 `pending.json`，可以反复跑。

`--json` 返回（`data` 的键）：

<!-- fields: ingest-begin -->

| 字段 | 含义 |
|---|---|
| `source_keys` | 本轮归一化后的 raw key |
| `affected` | 本轮会碰的页；`id` 是页 id，`path` 为空的就是还没建的页 |
| `new_pages` | 上面这种空 `path` 条目的**数量**，不是清单 |
| `orphan_pages` | `sources` 引用了账本不认识的 raw 的页，可能失去全部来源 |
| `pending_hash` | 本轮 `pending.json` 的摘要；重试对不上说明轮次被动过 |

`orphan_pages` 里的页，汇报时要单独点名，不许悄悄删来源糊过去。

## 2. 写正文

正文写到 `.wiki/staging/<type>/<id>.md`，一个 id 一个文件。

- **正文是你要写的东西**。已有页的 frontmatter 交给 CLI，不要手工维护 `updated`。
- `id` 不要自己编：它必须等于文件名。新页照抄文件名，已有页别动。
- 结论先行，再给依据；不写「本文介绍…」这类空转。

新页照这份骨架起笔，字段含义见 `SCHEMA.md`，`type` 必须取自 `wiki.toml` 的 `[schema].types`：

```markdown
---
id: order-retry
title: 订单超时重试
type: concept
summary: 一句话说清这页回答哪个问题
sources:
  - raw/order-timeout.md
status: draft
updated: 1970-01-01
---
```

`updated:` **必须自己写一行**，占位日期即可：`commit` 只改值、不补行，缺了报 `no 'updated:' field`。

正文好坏对照：

```markdown
<!-- 差：自述式开头，读完不知道结论，也没有落点 -->
本文介绍了订单超时重试的相关机制，分为以下几个部分。

<!-- 好：一句结论，一句依据，读者立刻能判断信不信 -->
重试上限是 3 次，第 4 次直接判失败（`raw/order-timeout.md`）。
超过上限没有退避——这是当前实现的已知问题，源里写明了。
```

## 3. frontmatter 归属

| 字段 | 谁写 | 说明 |
|---|---|---|
| `id` | agent | kebab-case，等于文件名（去掉 `.md`），也是 `[[链接]]` 的目标 |
| `title` | agent | 进 `index.md` |
| `type` | agent | 取 `wiki.toml` 的 `[schema].types`；文件放在同名子目录下 |
| `summary` | agent | 一句话，进 `index.md`；写不出一句话说明这页还没想清楚 |
| `sources` | agent 起笔，`commit` 追加 | 仓库相对路径 `raw/order-timeout.md`；大文件带片段 `raw/big.md#p=12-40` |
| `status` | agent | 新页 `draft`；人审过才改 `stable` |
| `updated` | CLI（`commit`） | 只做行级替换，保留注释、键序和引号风格 |

`sources` 一律仓库相对路径。漏了 `sources` 等于「无源声称」。

## 4. 收口：commit

```bash
llm-wiki ingest commit --pages concept/order-retry --json
```

`--pages` 是这一轮真正发布进 staging 的候选页；不传直接报错，不是「全提交」。裸 id 命中多个类型时会报歧义，改写成 `type/id`。

`commit` 依次做：刷 `updated`、把本轮 `raw/<key>` 补进 `sources`、记录页哈希、重建 `.wiki/staging/index.md`、盖 manifest、删 `pending.json`。

- 它**不写 `wiki/`**，也**不写 `wiki/index.md` 与 `wiki/log.md`**：前者由 `approve` 重建，后者由 `approve`/`reject` 追加。commit 之后不要自己再改这两个文件。
- **`commit` 之后不要再改候选正文**。`approve` 会拿记录的哈希比对，改了就报 drift，只能 abort 重来。
- 中途失败可以整轮重试；`pending.json` 还在，轮次就没关。

## 5. 放弃：abort

```bash
llm-wiki ingest abort
```

写歪了、源理解错了、页拆错了，用 `abort`。它丢掉本轮 `pending.json`，**保留已写的候选**，下一轮按 id 覆盖或复用。

不要手工删 `.wiki/staging/` 下的文件，也不要改 `pending.json`。

## 6. 摄取判断

命令替不了这一段，它才是 skill 存在的理由。

- **一页只讲一件事，一页只服务一个读者问题。** 两个问题写成两页，再互相 `[[链接]]`。
- **三个以上源讲同一件事才建页。** 单源短内容通常并进已有页；先 `llm-wiki query "<词>"` 找归属。
- 新页一律 `status: draft`。人看过、认可了，才改成 `stable`；不要自己给自己升。
- **源变了、结论没变**：只补 `sources`（`updated` 由 `commit` 刷），正文不动。
- **结论变了**：改写正文，并留一句「以前认为…；现在…」。不许静默覆盖，读者需要知道判断被推翻过。

## 7. 先汇报，再落笔

`begin` 之后、动笔之前，先用几句话讲清三件事，等人点头：

1. 要新建哪些页，每页的 id、标题、一句话要点；
2. 要改哪些页，改哪一段、为什么；
3. `orphan_pages` 怎么处理：补来源，还是弃页。

讲完等确认。人没点头就不 commit。

## 8. 收尾

```bash
llm-wiki checkpoint --label "ingest order-retry"
```

- `checkpoint` 把**已批准的 `wiki/`** 登记成一个版本；候选页还在 staging，要人 `approve --confirm` 才进 `wiki/`。
- 告诉人：不满意可以 `llm-wiki reject --reason "..."` 撤回本轮，staging 会回滚到 `wiki/` 的样子。
- 回复里写清这一轮改了哪些页、留了什么没做，别只说「已提交」。
