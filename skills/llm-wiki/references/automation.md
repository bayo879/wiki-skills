# 自动追平、回退与忽略列表

> 何时读：用户要定时任务、发现「一轮新增太多页」、要把 wiki 退回某个版本、或者抱怨某个文件反复制造变动。

三件事共同构成「无人值守也不失控」：预算（别一次爆）、回退（错了能退）、忽略（退完别再回来）。

## 谁决定什么时候跑

llm-wiki 没有常驻进程，也不会在你检索前偷偷追平。追平只有两个入口：

| 入口 | 谁触发 | 用在哪 |
|---|---|---|
| 计划任务跑 `sync` | 用户自己的 cron / systemd timer / 计划任务 | 定期把仓库根的新文件转成 raw |
| `sync --only <path>...` | 用户点名 | 只想跟某几个文件，与定时任务并存 |

`sync --only` 里的每个 key 都跳过「静置窗口」——用户点名就是明确请求，不再猜文件是否还在写。没被点名的源这一轮不转换。

```bash
llm-wiki sync --json                          # 一轮：scan → convert → 稳定门槛 → 审计
llm-wiki sync --only notes/a.md --json         # 只跟这一个
llm-wiki sync --limit 5 --dry-run --json       # 只看这一轮会做什么
```

定时任务只跑 `sync`。它不写页面、不合并候选，所以不需要人盯；页面仍然由 agent 在用户要求时写。

## 一轮最多新增多少页

`approve` 在**不带** `--confirm` 时先算这一轮会新增多少页（已存在的页算覆盖，不算新增）：

- 不超过 `[ingest].max_new_pages_per_run`（默认 20）→ 照常返回「N 页候选待批」（exit 3）。
- 超过 → 返回 `NEEDS_CONFIRMATION`，`reason` 是 `page_budget`，候选**全部留在** `.wiki/staging/`。

看到 `page_budget` 不要自己决定，把数字和清单给用户：

1. 批准（`llm-wiki approve --confirm`）——人显式越过预算。
2. 不批准并说明原因（`llm-wiki reject --reason "..."`）——候选丢弃，但这一轮的源按当前字节记为已复核，下一轮不再报同一变动。
3. 补充要求（`llm-wiki instruct --text "..."`）——让下一轮少建几页。
4. 先放着（`llm-wiki defer`）。

真觉得预算太小就改 `.wiki/wiki.toml` 的 `[ingest].max_new_pages_per_run`，不要靠反复 `--confirm` 绕过。

比「一轮多少页」更细的一道是**切片预算**（`reason: page_slice_budget`）：同一个切片最多养出 `[ingest].max_pages_per_slice`（默认 5）页。

切片就是页面 `sources` 里 `#p=<n>` 指的那一段；没带片段就是整份文件算一个切片。大文件按 `parse.chunk_target` 切块，所以一份两百页的手册天然有几十个切片、可以产出几十页，而一页速记只有一个切片、最多五页——限额跟着信息量走，不跟着文件个数走。

`detail` 会点名「哪份源的第几个切片、已经养出几页」。处置同前四选一；要保留这些页面就说明理由后 `--confirm`，否则把同一切片的页并成一页，或改引更精确的 `#p=`。

预算之前还有一道更细的闸：**近义页**（`reason: page_overlap`）。候选页出现下面任一情况就停在暂存区：

- 标题归一化后与已发布页相同（`Retry Policy`、`retry-policy`、`retry_policy` 算同一个）；
- 候选页 `sources` 全是某张已发布页已经引用过的源——同样的证据写第二遍，没有新事实。

`detail` 会点名和哪张已发布页撞了。处置和 `page_budget` 一样四选一；要做就改 id/title、或补一个真正的新源，然后重新 `ingest commit`。它只比 frontmatter，不读正文，所以语义上「像」但标题与来源都不同的页不会被拦。

## 回退一版

版本是内容寻址的：`checkpoint` / `approve` / `rollback` 各打一个点，只存变化的字节。回退还原**页面和 raw**，索引随之重建。

```bash
llm-wiki history --json                # 列出版本：version / kind / label / created_at / pages
llm-wiki diff v003 --json              # 和当前工作树比：added / removed / changed
llm-wiki rollback v003 --json          # 还原 v003，并把自己记成新版本
```

`rollback` 是「整版还原」：v003 里没有的内容文件会被删除，不会与新内容合并。回退本身也记一个版本，所以退错了还能退回退之前——`history` 里那条 `kind: rollback` 就是它。

不做单页回退。只要一页就用 `git` 或手动改；版本层只保证「整版可复现」。

## 回退之后：把制造变动的文件挡住

回退通常解决不了根因：下一轮 `scan` 又会看到同一批变动，再转一遍、再建一遍。所以回退后问用户一句：

> 要不要把这几个一直变动的源加进忽略列表？

```bash
llm-wiki ignore add "vendor/**" "notes/scratch.md" --json
llm-wiki ignore list --json
llm-wiki ignore check vendor/lib.js --json     # ignored: true/false，rule 说明是哪条规则
llm-wiki ignore remove "notes/scratch.md" --json
```

`.wiki/ignore` 用 `.gitignore` 语法（`#` 注释、`!` 取反、尾部 `/` 只匹配目录、`/` 开头锚定根、`**` 跨目录）。被忽略的路径：

- `scan` 看不见它（不算新增/修改/删除），`convert` 也不会处理；
- `archive purge` 不清理它的派生 raw 与账本行——用户说不要，清理也不该绕过；
- 显式 `convert --key` 仍然能读单个文件，方便先看一眼再决定。

代价：`.wiki/` 不随 git 共享，忽略列表也是每台机器一份。换机器要重新 `ignore add`，或者把 `.wiki/ignore` 拷过去。

## 汇报口径

自动跑完**不主动汇报**。用户问「最近变了什么」时再给：

```bash
llm-wiki history --json      # 打过哪些点
llm-wiki diff v00N --json    # 与当前差了什么
llm-wiki query --text "..." --json
```

只说结论与页数，不贴 JSON。
