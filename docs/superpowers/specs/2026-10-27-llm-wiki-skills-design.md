# LLM Wiki Skills —— 设计稿 v10（同步 + 检索 + 会话记忆）

日期：2026-10-27 ｜ 状态：**已定稿 v10**（42 条决策） ｜ 形态：Claude Code skill 包 + Rust CLI（`llm-wiki`）

---

## 1. 一句话

把一个普通仓库变成「LLM 持续维护的 wiki」：raw 里的源文件不动，wiki 页面由 agent 写，
而所有**簿记**——清单、哈希、目录、日志、一致性检查——由 CLI 做。

核心主张只有一条：**簿记归 CLI，判断归 LLM。**

Karpathy 原话是「LLM 在每次摄取后更新 index」。我们认为这是整个模式里最脆的一环：
让模型手写目录和日期，它迟早会和磁盘上的页面漂移，而漂移是不可见的。
所以 `index.md`、`log.md`、页面的 `updated` 字段一律由 CLI 从 frontmatter 生成，
LLM 只负责页面的正文、摘要、以及「该不该建这页」这类判断。

---

## 2. 目标 / 非目标 / 反目标

**目标**
- 装进任意仓库即可用，不绑定某个具体知识库。
- 上游随时 CRUD 时，靠定时任务把 wiki 自动跟住，不需要人守着。
- 四件事各自可独立调用、单独验收：扫描、解析、摄取、审计。
- 跨平台：macOS(arm64/x64)、Linux(x64/arm64)、Windows(x64)。
- 源文件只读；wiki 就是一堆 markdown，`git`、`Obsidian` 直接可用。
- 会话这种「高频、低增量、话题重复」的来源有收口办法：分层封存，源数不随会话量线性膨胀（§5.11）。

**非目标（v1 明确不做）**
- 向量检索、关系库、任何常驻服务。
- 图形界面（将来做，且只能是 CLI JSON 的消费者）。
- RAG 问答、embedding 生成、多用户权限。
- OCR、音视频。**扫描件不 OCR**：探针判定没有文本层就明确降级，不建页、不猜内容；图片只做链接，不做文字识别。

**反目标（做成了也算失败）**
- wiki 出现约定之外的目录、命名、字段。
- 同一个源被摄取两次、产生两张重复页。
- 审计报告很长，但没有一条可执行的结论。
- CLI 必须联网，或必须编译五分钟才能装。
- 会话记忆只进不出：`sources` 只增不减，一年后单页挂几十条引用，没人读得动。

---

## 3. 架构

```
repo/
├── wiki.toml            # 契约：唯一配置（可见、进 git）
├── SCHEMA.md            # 契约：页面约定（可见、人 + agent 共读、进 git）
├── source/              # 原始文件（任意格式），同步服务写入，只读，不进 git
├── raw/                 # 规范化 markdown，由 CLI 转换生成，页面引用它，进 git
├── wiki/                # 已批准版本：页面 + index.md + log.md + audit/
│   ├── index.md         # 由 CLI 生成，禁止手写
│   ├── log.md           # 由 CLI 追加，禁止手写
│   ├── audit/           # 审计报告，进 git
│   ├── archive/         # 会话封存摘要，进 git，不进 index，只由 recall 读（§5.11）
│   └── <type>/<id>.md
└── .wiki/               # 引擎目录：契约之外的一切机器产物
    ├── state.db         # 索引：源账本 + 哈希（可重建，不进 git）
    ├── versions/        # 已批准版本快照 v1/ v2/ ...（不可重建，进 git）
    ├── staging/         # 候选快照，未批准（不进 git）
    ├── sessions/        # 原始会话记录，未封存（不进 git，可丢）
    ├── cache/           # 分块与解析中间态（不进 git）
    └── lock             # 进程锁（不进 git）
```

**两层源**：`source/` 是不可变事实（任意格式，同步服务随时增删改）；`raw/` 是转换产物
（统一 markdown，只由 CLI 写）。页面引用 `raw/*.md`，而 `raw/*.md` 的 frontmatter 记录它来自哪个
`source/` 文件——溯源链是 page → raw → source。

分层带来的最大收益是**变更判据的升级**：从「源文件变了」变成「规范化后的内容变了」。
PDF 重新下载但正文没变、解析器只是把换行重排，都不会触发重新摄取。
代价是多一份磁盘、多一个转换阶段、多一段溯源链。

划分原则一句话：**契约留在明处，活动产物全进 `.wiki/`**。
契约是「人会改、要 review、要能被第二个人看见」的东西；`.wiki/` 里是机器自己吞吐的中间物。

`.wiki/` 内部再分两档，别混：

| 档 | 内容 | 进 git | 丢了会怎样 |
|---|---|---|---|
| 可重建 | `state.db`、`cache/`、`staging/` | 否 | 扫一遍就长回来 |
| 不可重建 | `versions/` | 是 | 历史版本没了，无法回滚 |

`versions/` 里的页面内容与 `wiki/` 相同，git 按 blob 去重，磁盘代价接近零，只多一倍 inode。

`.wiki/sessions/` 不属于这两档：它既不可重建、也不该留——封存成归档摘要后即可丢弃（§5.11）。

三层：**源层**（不可变事实）→ **wiki 层**（派生知识）→ **状态层**（机器账本）。
`wiki/archive/` 不是第四层，是 wiki 层的**历史侧**：已经蒸馏完毕、退出主视图的页面，
不进 `index.md`，生命周期绑在 §5.11 的会话记忆上。索引层、界面层刻意推迟到 v2，但接口在 v1 留好。

```toml
[paths]
source = "source"; raw = "raw"; wiki = "wiki"; engine = ".wiki"

[formats]
allow = ["md", "txt", "html", "pdf", "png", "jpg", "jpeg", "gif", "webp", "svg"]   # 白名单作用于 source/；图片只做链接，不解析

[schema]
types = ["concept", "entity", "source", "comparison", "synthesis"]

[parse]
primary = "auto"                 # auto | builtin | external
chunk_threshold = "2MB"          # 超过则强制分块
max_source_bytes = "100MB"       # 超过则拒绝，需 --force-large
chunk_target = "64KB"            # 文本块目标大小
min_chars = 200                  # 内置产出低于此值视为不达标，可触发外部后端

[parse.external]                 # 可选：接入外部文件解析服务
type     = "http"
url      = "https://<your-parse-service>/parse"   # 地址由使用方填，仓库里不记录任何内网地址
auth_env = "WIKI_PARSE_TOKEN"    # 只写环境变量名，禁止把 token 写进仓库
fields   = { file = "@file" }    # @file 表示取文件内容；服务端要的其它字段照填
response = { text = "data", ok_code = 200 }
timeout_s = 120

[ingest]
max_pages_per_source = 12
review_before_write = true

[audit]
min_page_chars = 200

[versions]
keep = 5                      # 保留几个历史版本快照

[sync]
auto_ingest    = false        # 定时任务下改成 true
max_per_run    = 20
max_pending_sources = 200     # 累积候选超过这个数就在汇报里喊停
settle_seconds = 10           # 文件写入稳定阈值，防半写
stable_streak  = 3            # 连续几次扫描一致才准自动摄取
read_retries    = 3           # 读不一致时的重试次数
lag_warn_rounds = 3           # 连续几轮追不上就降优先级
on_missing     = "mark_stale"

[memory]                      # 会话记忆分层，见 §5.11
session_soft_cap = 50         # .wiki/sessions/ 超过此数，汇报里喊「该总结了」，不自动动作
idle_days        = 60         # 未触达多少天进入封存候选
purge_cap        = 200        # 会话文件数超过此数
purge_days       = 180        # 且距最后触达超过此天数；两个条件同时满足才准清原文
```

---

## 4. CLI 契约

对 agent 的唯一接口是 **stdout 的 JSON**（`--json`），人类模式只是渲染。

**退出码**：`0` 成功 ｜ `1` 发现 error 级问题 ｜ `2` 用法/环境错误 ｜ `3` 需要人工确认（附 `{"status":"needs_confirmation","reason":...,"detail":...}`） ｜ `4` 另一个进程持锁。

| 子命令 | 作用 |
|---|---|
| `init` | 写 `wiki.toml`、`SCHEMA.md`、建目录 |
| `doctor` | 自检：版本、外部后端、权限、大小写敏感性、裸 token |
| `scan` | 差分：`new / changed / moved / missing / duplicate / ignored`，每条带 `layer`；`--apply-moves` 迁移改名账本（见 §5.1 两段） |
| `convert` | 原始层 → 规范化层：把 `source/` 转成 `raw/*.md`；`--limit` 分段；单个失败不中断，收在 `data.failures` |
| `parse` | 单文件调试入口：跑指定后端看 markdown 产出，不落 `raw/`；`--force-large` 越过 `max_source_bytes` 只为看一眼，同样不落盘、不进账本 |
| `sync` | 无人值守入口：差分 → 写 staging → 变更报告（见 §5.5） |
| `ingest begin` | 开事务，返回候选页 |
| `ingest commit` | 校验 + 落账：给候选页刷 `updated`、补 `sources`、写进 `.wiki/staging/`；**发布由 `approve` 统一做** |
| `ingest abort` | 丢弃事务 |
| `checkpoint` | 人工会话收尾：把当前 `wiki/` 登记为新版本（见 §5.6） |
| `archive status` | 会话记忆水位：活跃会话 / 可封存候选 / 可清理 / 封存后仍被引用数（见 §5.11） |
| `archive seal` | 封存一组会话为归档摘要：重挂 `sources`、出 index、进 FTS、记版本；需 `--confirm` |
| `archive purge` | 只清 `.wiki/sessions/` 原文，永不删归档页；需 `--confirm` |
| `recall` | 归档层唯一读口：摘要 + 入口，`--full` 才给蒸馏正文（见 §5.11） |
| `approve` / `reject` / `instruct` / `defer` | 决策门四个选项，语义见 §5.6 |
| `lint` | 机械审计；`--fix` 只修机械项；`--strict` 让 warning 也失败 |
| `graph` | 链接图：节点出入度、边、孤立页、断链（只读，供判断层找枢纽与孤岛） |
| `query` | 检索（FTS5，索引 `wiki/` 含 `archive/` 摘要，见 §5.9） |

**CLI 永不写页面正文**，也不做任何交互提问——需要人确认时用退出码 3 交还给 agent。

---

## 5. 四件事的契约

### 5.1 扫描 `scan` 与转换 `convert`

`scan` 分两段，中间隔着一次转换：

| 段 | 输入 | 输出 |
|---|---|---|
| A 原始层 | `source/` ↔ 账本 | 待转换清单 → `convert` 写出 `raw/*.md` |
| B 规范化层 | `raw/` ↔ 账本 | 待摄取清单 → 交给 ingest |

**变更判据落在 B 段**：只要 `raw/*.md` 的内容哈希没变，`source/` 里换了文件也不重摄。

**不做常驻守护进程**：所谓「后台转换」就是 `convert` 这个独立子命令，定时任务里串在 scan 前，
`--limit N` 每次干一段就退出。跨平台不用管 worker 池和 IPC。

- 身份键 = 相对仓库根、`/` 分隔、NFC 归一、保留原始大小写。
- 变更判定 = 内容哈希（blake3）。
- 输出：`new` / `changed` / `moved` / `missing` / `duplicate` / `ignored`（白名单外只计数）。
- 每条变更带 `layer`：「`source`」（A 段）或「`raw`」（B 段）——两次扫描输出的形状一致，调用方不用猜是哪个账本给出的。
- `changed` / `missing` **不自动删页**，只标记，由审计与人决定。
- 幂等：反复跑不改 state（streak 计数在 sync 里做）。`moved` 的账本迁移是唯一例外，因此要显式 `--apply-moves`
  （`sync` 内部自动应用），迁移时若目标键已被另一个账本行占用则报 `E_MOVED_COLLISION` 停下，不覆盖。

`raw/*.md` 的 frontmatter：

```yaml
---
source: source/2026-03-01-xxx.pdf
source_hash: blake3:...
parser: builtin-pdf@0.1
converted_at: 2026-10-27T10:00:00Z
pin: false            # 人手修过则置 true，convert 不再覆盖
---
```

**允许人手修 raw**（修正糟糕的抽取结果很常见，多栏、表格错位）：手改后置 `pin: true`，`convert` 跳过；
不 pin 就按 source 重新生成，覆盖前在汇报里列出将被覆盖的文件。

### 5.2 解析 `parse`（`convert` 的执行体）

**标准入口。** 所有解析后端实现同一个契约：输入文件，输出 **markdown**。
出口统一成 markdown，md / html / pdf / 外部服务的结果才同构，下游只看一种东西。

解析器本身不知道 `raw/` 的存在：`convert` 调它，把 `Extracted.markdown` 连同溯源 frontmatter
写成 `raw/*.md`；`parse` 子命令只是拿单个文件试后端、看产出，不落盘。

```
trait Parser {
    fn id(&self) -> &str;
    fn can_handle(&self, hint: &Hint) -> bool;
    fn parse(&self, input: &Source) -> Result<Extracted>;  // Extracted { markdown, warnings }
}
```

| 后端 | 处理 | 说明 |
|---|---|---|
| `builtin-text` | md / txt | 直读，剥 frontmatter，保留结构标记 |
| `builtin-html` | html | 正文抽取（引 crate，不自研），去导航/脚本 |
| `builtin-pdf` | pdf | 内嵌抽取，按页分段标记；无文本层则产出 asset 型 raw 并标 `no_text_layer`，**不 OCR** |
| `builtin-asset` | png/jpg/gif/webp/svg | **不解析内容**，只产出一份含链接的 asset 型 raw（见 §5.10） |
| `external-http` | 白名单内文本类格式 | 可配置的 HTTP 解析服务，响应里取 markdown；**不接图片** |

**选择与回退**（`[parse].primary`）：

- `auto`（默认）：md/txt 走内置；html/pdf 先走内置，若产出不达标（`chars < min_chars` 或
  `chars < min_chars`）且外部服务已配置，再走外部；外部失败（超时/鉴权/非 200）→ 记 warning 回退内置。
  **两条都失败才报错，且同时列出两个原因**，不假装成功。
- `builtin` / `external`：强制单一后端，失败即失败。

外部服务是**通用适配器**，只约定一类协议，不绑定任何具体接口：POST multipart，一个文件字段
（`@file` 取文件内容）+ 任意附加表单字段；`Authorization: Bearer` 从**环境变量**读；响应是 JSON，
按点分路径取一段字符串当 markdown。url、表单字段名、成功状态码、响应路径、超时全部写在
`wiki.toml` 里，仓库不记录任何内网地址或私有字段名——换一个服务只改配置，不改代码。

- `convert` 落盘 `raw/<同源相对路径>.md`，返回 `{raw_path, chars, backend, duration_ms, warnings}`；
  `parse` 只打印同样的结构。分块中间态在 `.wiki/cache/<hash>/`，不是产物。
- 缓存键 = `backend_id + 内容哈希`，换后端不会误命中旧缓存。
- 非 UTF-8 输入报错并给出探测到的编码，**不静默替换**。
- 扫描件 PDF 抽不出正文时返回 `warning: no_text_layer`，同时产出 asset 型 raw（只有链接），不假装成功，也不 OCR。
- 超过 `max_source_bytes`（100 MB）直接拒绝并记 `oversize`，不静默跳过；要处理必须显式 `--force-large`（见 §5.8）。
- `doctor --check-external` 只做连通与鉴权探测，不上传文件。

### 5.3 摄取 `ingest`

两段式，防「摄取到一半崩了但账本已前进」：

1. `ingest begin <raw>` → 记 `{raw_path, raw_hash, index_hash, log_hash, began_at}` 到
   `state.db` 的 pending 表（**同时只允许一个**，第二个 begin 报错）。
   返回候选受影响页（graph + title/tags 匹配）——**候选只是建议，不是约束**。
2. agent 读源、汇报要点（`review_before_write` 时）、写页面。
3. `ingest commit --pages a,b,c`：
   - `--pages` 必须显式给出（空列表报错），且必须有 pending，否则报错；
   - 每个 id 必须能在 `.wiki/staging/` 里找到，找不到即失败；
   - 通过 → 给候选页刷 `updated`、把 pending 里的 `raw/<key>` 补进页面 `sources`（已存在则跳过）、
     记 `page_meta` 哈希、重建 **staging** 的 `index.md`、盖 manifest、清 pending。
   - **不写 `wiki/index.md`、不追加 `wiki/log.md`**：这两件事只在 `approve` 时做。因为
     `wiki/` 必须永远等于「已批准」，中间产物全部留在 staging（见 §5.6）。
   - 全库反查 `sources` 覆盖情况、以及「begin 之后源又被重新转换」的复核，**前移到 `approve` 之前**
     （`check_sources` → `staging_stale`，exit 1）。发布前一次性挡住，agent 不会在 commit 阶段
     被一个它改不了的哈希卡住。

失败时 state 不前进，返回 `{page, rule, detail}` 清单。页面已写一半是可接受的——
lint 会抓到「写了但没账」的孤儿。

### 5.4 审计 `lint`

分两层，这是第二个主张：**确定性检查给代码，语义判断给模型。**

下表就是 `src/lint.rs` 落地的 13 条，编号与代码一致（2026-10-27 校正：此前本表是
动工前写的另一套 12 条，与代码只有 L6/L12 同义，按号对会错位）。

| 编号 | 检查 | 级别 |
|---|---|---|
| L1 | 页面缺 `id`、`title` 或 `status` | error |
| L2 | `[[wikilink]]` 指向不存在的页面（代码块内的 `[[` 不算） | error |
| L3 | 同一个 id 出现在两个 type 目录下 | error |
| L4 | `status` 不是 `draft`／`stable`／`stale` | error |
| L5 | `updated` 缺失或不是 `YYYY-MM-DD` | warning |
| L6 | 页面 `sources` 指向的文件不存在；**归档层来源豁免**：指向 `wiki/archive/` 的引用不要求进 raw 账本（见 §5.11） | error |
| L7 | `raw/` 下有文件不在账本里（转换出来了但没记账） | warning |
| L8 | 正文短于 `[audit].min_page_chars` 字符（按字符数，不按字节）；`asset: true` 与 `archived: true` 豁免 | warning |
| L9 | 账本里该页的正文摘要变了，但 `updated` 没动（只对账本里有记录的页生效，新克隆没有记录所以静默） | warning |
| L10 | 同一个 raw 被超过 3 个页面引用（源太粗或页该合并） | info |
| L11 | `log.md` 的 `## ` 行不符合 `## [日期] 动作 | 标题` | warning |
| L12 | 没有任何页面引用的 asset（图片等） | info |
| L13 | 归档的会话摘要零入链，且超过 `idle_days` 没被 `recall`（可提前清理的候选） | info |

**两条设计时列过、最终没做成检查的**，别在代码里找：

- **索引漂移**：不给检查，因为在写路径上它不可能存活 —— `approve`、`checkpoint`、
  `lint --fix` 都会用 `render::write_index` 把 `wiki/index.md` 整份重写。能看到旧索引的
  只有「纯只读」的瞬间，不值得为它留一条永远不该出现的 finding。
- **近似重复**（标题归一化相同 / summary 的 Jaccard ≥ 0.7）：归判断层。中文标题做模糊
  匹配误报率太高，而且要不要合并两页是判断不是校验；`wiki-audit` 技能里按内容提示即可。

判定边界两处容易搞错：**L9 只对账本里有记录的页生效**（`state.db` 不进 git，新克隆没有
记录，若一律报「改过没打日期」会让干净检出直接失败）；**L8 按字符数**，所以短中文页
仍然算短。

判断层（skill）：跨页矛盾、被新源推翻的结论、反复出现却没建页的概念、跨源缺口、
下一步该找什么源。CLI 只给料：`graph` 的孤立页（零入零出）、高扇入页（可能是枢纽）、`broken`（声称有链接但指向不存在的页）。

输出：`wiki/audit/YYYY-MM-DD.md`（进 git，`--no-write` 可关）+ `--json`。
`--fix` 只做 100% 确定的修复：重建 index、修 log 头、补 `id`。语义问题一律不自动改。
默认只有 error 让 exit 1；`--strict` 时 warning 也算。

---

### 5.5 同步与无人值守 `sync`

上游是同步服务，会以任意 CRUD 形式改 `source/`；宿主 agent 能跑定时任务。所以需要一个
**可以没人看着跑**的入口。

**变更分类与动作**

| 变体 | 判定 | 无人值守下的动作 |
|---|---|---|
| `added` | 账本无、磁盘有 | 入队（`auto_ingest = true` 时直接摄取） |
| `modified` | 同路径、哈希变 | 相关页标 `stale` 候选并进队列，**不自动改写已有结论** |
| `moved` | 哈希同、路径变 | **只迁引用不重摄**：迁 state 键 + 替换页面 `sources` 里的路径 |
| `removed` | 账本有、磁盘无 | 不删页；页面保留源引用并标 `(missing)`，状态转 `stale`，进审计队列 |
| `duplicate` | 同哈希、多路径 | 只认规范路径（最早入库），其余记 `ignored` |

`moved` 单独成一类很重要：同步服务一改名，若按「删 + 增」处理，就会把同一份内容重摄一遍，
产出一张重复页 —— 正是反目标里要避免的事。

**稳定准入（只对自动模式生效）**：一个源要被自动摄取，必须连续 `stable_streak`（默认 3）次扫描
哈希完全一致。计数由 `sync` 维护（每次 sync 内含一次扫描），`scan` 自身保持纯读、不写状态，
「scan 幂等」这条不破。

streak 是**跨轮**观察，`settle_seconds` 是**同一时刻**的写入检查，两者互补：
前者防「每隔几分钟追加一点」，后者防「此刻正在写」。少任何一个都有漏洞。

代价是新文件至少要等 3 轮才被自动摄，所以交互式摄取不受 streak 约束——人说了就摄。

**无人值守的三条规矩**

1. 允许建页、改页、追加 log、重建 index；**禁止删页、禁止合并页、禁止删源**。不可逆动作一律进 review 队列。
2. 自动写出的页面一律 `status: draft`，人审后改 `stable`。复用现有字段，不新增 schema。
3. 每轮产出一份 `wiki/audit/<日期>-sync.md`：变了什么、动了哪些页、哪些进了队列。
4. **永不封存、永不清理归档层**：封存 = 合并 + 删原文，正是第 1 条禁止的不可逆动作。
   `sync` 收到此类请求直接以退出码 `2` 拒绝（见 §5.11）。

**并发与断点**

- 所有写命令持 `.wiki/lock` 文件锁；拿不到 → 退出码 `4`（busy）。
  锁 mtime 超过 10 分钟视为崩溃残留，可抢占并记 warning。
- `sync` 可重入：每个源处理完立即记账，中断只损失当前这一个源。
- `--limit N` 分批：定时任务小批量高频跑，避免首次接入几千文件时一把梭。
- `settle_seconds`：mtime 距今过近的文件跳过本轮，防止读到同步服务写了一半的文件。
- 连续失败 3 轮自动把 `auto_ingest` 置回 false 并写报告 —— 宁可停下，也不要持续污染 wiki。

**调度**：宿主定时唤起 `wiki-sync` skill，它调用 `llm-wiki sync --json`。
skill 只负责读报告、解释、退避；编排本身在 CLI 里，保证「没人看」时行为一致。

---

### 5.6 版本、候选与决策门

**三层状态**

| 层 | 位置 | 谁写 | 谁看得见 |
|---|---|---|---|
| 已批准版本 | `wiki/`（当前）+ `.wiki/versions/vN/`（历史） | CLI，只在 approve 时 | 人、agent |
| 候选 | `.wiki/staging/` | CLI + agent（定时任务） | 只在汇报里 |
| 索引 | `.wiki/state.db` | CLI | 没人 |

**baseline = 最后一个已批准版本**，不是上一次扫描。这带来两种比较，别混为一谈：

| 比较 | 用途 | 成本 |
|---|---|---|
| 磁盘 ↔ draft 清单 | 检测**本轮新增**的变动 | O(源数) 次哈希，零解析 |
| draft 清单 ↔ baseline | 生成**累积视图**（汇报用） | 纯内存比对，不碰文件 |

`draft 清单` = `.wiki/staging/manifest.json`，每源一行：`{baseline_hash, current_hash, last_processed_at, pages[]}`。
`current_hash != baseline_hash` 的源就是「累积未合并」的集合，随时直接读出，不需要重扫。

**写入策略是增量的**：staging 第一次产生时是 baseline 的完整副本（形态上是快照，便于原子合并），
之后每轮只重写受影响的那几个页，其余文件原样留着。所以一直 defer 不会让每轮工作量变大——
变大的是 staging 相对 baseline 的**分叉度**，不是重算量。

**真正的成本不是 diff，是重新派生**：解析、重写页、让模型重读源。所以缓存分两级：
源级（`current_hash` 没变就不重新解析，直接走 `cache/`）、页级（只有 `pages[]` 里列到的页需要重新合成）。

**多源合成页的坑**：页 P 同时由 A、B 支撑，A 变了、B 没变，P 必须用「新 A + baseline 版 B」重新合成，
不能打补丁。所以 staging 必须保留完整副本而不是只记 delta——未变的源要能就地读到原内容。

**硬链接用不得**：staging 里未改的页若 hardlink 到 `wiki/` 同一 inode，人一改 `wiki/` 那份，
staging 跟着变，「库内状态不更新」直接失效。复制必须是真的复制（100 源规模下几 MB，代价可忽略）。

**为什么候选必须隔离**：`defer` 的语义是「库内状态不更新合并」。候选一旦就地写进 `wiki/`，
即使用代码回滚，用户看到的也已经是「合并了」。所以定时任务的产出只写 `.wiki/staging/`，
`approve` 时才做一次 `rename` 原子替换。这条同时把原来的「事务态」问题一起解决了：`ingest begin/commit`
也只写 staging，**`approve` 是唯一的发布动作**，不再需要第二套事务机制。

**checkpoint**：人工会话结束时由 skill 调用 `llm-wiki checkpoint`，把当前 `wiki/` 登记为一个版本。
会话结束没有可靠钩子（用户可能直接关窗口），所以忘了调用时，下一轮 `sync` 报 `baseline_dirty`，
要求先 checkpoint 或 approve，**不允许静默漂移**。

**决策门**：`sync` 发现未批准候选时不自动合并，输出 `{"status":"decision_required", ...}`（退出码 3）。
四个选项的语义：

| 选项 | 用户语义 | CLI | 系统动作 |
|---|---|---|---|
| 1 批准 | 审计通过，合并并递增版本 | `approve` | 校验后原子合并，v+1，baseline 前移 |
| 2 不批准 | 用户给原因，指示如何处置 | `reject --reason "..."` | 记录原因，staging 标 rejected，baseline 不动 |
| 3 自行输入 | 自由文本指令 | `instruct "..."` | 交 agent 按指令处置，不进版本账本 |
| 4 暂不处理 | 不合并，审计不通过 | `defer` | staging 保留，baseline 不动，下轮继续累积 |

2 和 4 对系统的效果一样（baseline 都不动），区别在于 2 留下了「为什么不要」的记录，
能反哺 schema 和源策略；4 只是搁置。汇报里必须把这个区别讲清楚，否则用户会以为二者等价。

**晋升前的防漂移校验**：approve 时比对 `staging/pending.json` 里的哈希清单与 `wiki/` 实际内容，
不一致说明人中途手改过主目录 → 拒绝晋升，报 `staging_drift`（退出码 1），不做静默覆盖。

**超限与清理**：`max_pending_sources` 超限时汇报显式喊停，不继续闷头算；
`versions.keep` 超出时删最旧并写 `log.md`。

---

### 5.7 执行期间文件继续变动

同步服务不会因为我们开始跑就停下来。三层防线，缺一层就会出现「账本记的和实际处理的不一致」：

| 时机 | 手段 | 失败时 |
|---|---|---|
| 入场 | `settle_seconds`：mtime 距今过近不碰 | 本轮跳过，下轮再来 |
| 读取 | 一次读入内存 → 对字节算哈希 → 读前后 `stat(size, mtime)` 一致 | 重试 `read_retries` 次仍不一致 → 标 `racy` |
| 收尾 | 本轮涉及过的源全部再复核一次哈希 | 变了就撤下，标 `racy`，留到下轮 |

收尾复核是关键那一层：它保证**账本里记的哈希 = 真正被处理的那份字节**，
而不是「扫描开始时看到的」。少了它，一轮跑十分钟、期间文件变了三次，账本就会指向一个从未被处理过的状态。

**一次读入内存，不要流式读**：一次 `read` 拿到的字节是自洽的；分块流式读可能把两个版本拼在一起。
文件上限 100 MB，一次性读入内存仍在可接受范围。

**跨轮追不上**：`racy` 源进滞后者名单；连续 `lag_warn_rounds` 轮仍是 racy → 降优先级
（每轮最多试一次，排在队列最后）。汇报必须给出 `lagging` 计数与名单——追不上就明说，
不要装作同步完成。持续追不上说明写入频率高于处理速度，汇报里直接给建议（拉长间隔 / 加大 `--limit` / 收窄白名单）。

**approve 前复核依赖源**：候选里每页记录它依赖的源哈希快照；approve 时若源当前哈希与记录不符 →
报 `staging_stale`（退出码 1），拒绝合并。少了这一步，会批准一个「基于旧内容合成」的版本——
它看起来审计通过，内容却已经过期。

**竞态导致的删除/改名**：读取时 ENOENT 归为 `vanished`，不算错误，下轮按 `removed` 处理。

---

### 5.8 大文件

「5 MB 给个警告然后照读」是不够的：500 MB 的 PDF 会把内存和模型上下文一起打爆。按大小分三段：

| 段 | 判定 | 动作 |
|---|---|---|
| 小 | ≤ `chunk_threshold`（2 MB） | 整份解析，一次给全 |
| 中 | ≤ `max_source_bytes`（100 MB） | **强制分块**，按需取块 |
| 大 | > `max_source_bytes` | 拒绝；要处理必须显式 `--force-large` |

**分块与按需取块**：大文件 `parse` 不返回全文，返回 `{mode:"chunked", chunks:N, chunks_dir}`；
取块用 `parse --chunk 3` / `--range 3-5`。这样 agent 永远只把用得上的部分塞进上下文。
文本按行切并对齐 UTF-8 边界，PDF 按页切（天然边界）。块缓存到 `.wiki/cache/<hash>/`，重跑只补未完成的块。

**分块是读取层的派生物，不是存储形态**（决策 43）：`raw/` 里每个源仍然只有一份完整的 markdown，分块视图缓存在 `.wiki/cache/<hash>/`（`chunk-NNNN.md`），随缓存清空可重建。所以 `convert` 不需要为大文件分支——它照常写一份 raw。这样账本、变更判据、lint 仍只有一条路径（与「每个源都产出 raw」同一个理由），片段引用 `raw/big.md#p=12-40` 也永远指向一个真实存在的文件；缓存丢了只影响「取块」的快慢，不影响引用是否成立。

**大文件的哈希**：2 GB 读一遍算哈希本身就是几十秒。所以分级：≤ 阈值全量 blake3；
超过则先用 `size + mtime` 快判，变了再抽样哈希（头尾各 1 MB + 中段 1 MB）。
抽样理论上会漏判中间几字节的改动，但这比每轮把 2 GB 读一遍现实得多，而且漏判有兜底——
被消费过的块单独存内容哈希，lint 能发现「块内容与源对不上」。

**片段引用**：大文件只能被部分消费，页面 `sources` 用片段语法 `sources: ["raw/big.md#p=12-40"]`。
这让审计能回答「这页只用了源的 4%，是有意还是漏了」。写成裸路径表示全量消费。

**大 + 持续变动 = 永远不稳定**：这正好是应有的行为——大文件又一直在写，本来就不该自动摄。
但汇报里要单列这类（`blocked_by_stability`），否则用户会以为任务挂了。

---

### 5.9 检索 `query`

**范围**：只索引 `wiki/` 页面。`raw/` 全文不进索引——1000 个源、单源平均 30 MB 时 raw 总量是几十 GB，
而 query 要回答的是「我们记过什么」，不是「原文第几页出现过这个词」。需要原文时按命中里的
`sources` 路径直接读（见下），不需要全文索引。

**`wiki/archive/` 参与索引**（§5.11）：归档摘要是「原先那组会话文档」的位置替代物，
不索引它等于把历史从记忆里抹掉。但进索引的只有蒸馏摘要，`recall --full` 才取正文，
原文（`.wiki/sessions/`）永不进上下文。

**引擎**：SQLite FTS5，与 `state.db` 同库同事务。中文按**单字预切分**（入库前把 `订单处理` 写成
`订 单 处 理`），`tokenize='unicode61'`；查询侧用短语 `"订 单"`，天然是位置相邻匹配。

实测（SQLite 3.43.2、macOS、合成中文语料）：

| 语料 | 索引 | 查询 | 命中 | count | +bm25 排序 |
|---|---|---|---|---|---|
| 50 MB | 112 MB | 罕见单字 | 950 | 3 ms | 3 ms |
| 50 MB | 112 MB | 常见双字 | 77 K | 66 ms | 261 ms |
| 150 MB | 305 MB | 罕见单字 | 2.8 K | 3 ms | 8 ms |
| 150 MB | 305 MB | 常见双字 | 231 K | 140 ms | 733 ms |

慢的根因不是 FTS5 本身，是「逐字切分 + 短语」要扫百万级 posting 再做位置校验。所以整套结论有个前提：
**索引对象足够小**。`wiki/` 正文通常 3–30 MB，对应典型查询 3–30 ms、停用词级别最坏约 150 ms；
外推到 GB 级 raw 全文才会到秒级。

**Hit 结构**（`--json`）：命中不只是页面身份，还要带三类**可执行的下一步**，否则 agent 只能停在摘要上猜。

```json
{
  "page": "wiki/concept/order-timeout.md",
  "id": "order-timeout",
  "title": "订单超时",
  "type": "concept",
  "status": "stable",
  "updated": "2026-10-27",
  "score": 12.3,
  "summary": "...",
  "snippet": "...<em>订单超时</em>...",
  "links": [{"to": "wiki/concept/retry.md", "id": "retry", "title": "重试策略"}],
  "backlinks": 3,
  "sources": [
    {"raw": "raw/订单系统设计.md", "source": "source/订单系统设计.pdf",
     "chunk": "p=12-40", "raw_available": true, "source_available": true}
  ]
}
```

| 字段 | 给 agent 的下一步 |
|---|---|
| `links` | 跳到相邻页：这个问题还有第二半 |
| `sources` | `read` 原文，路径**已拼好**，不用它自己查账本 |
| `backlinks` | 引用数；扇入高的是枢纽页，优先读 |
| `archived` / `recall` | 归档命中：摘要占位，`recall <id>` 才展开蒸馏正文（§5.11） |

**只给一跳，展开是 agent 的权限**：默认不内联邻居正文，`--expand 1` 才带邻居摘要。
检索结果要省上下文，不是把图灌进去——10 条命中各带 3 个邻居正文，几万 token 就没了。

**`raw_available` 不是装饰**：`raw/` 进 git、`source/` 不进，别人 clone 后 `source` 路径必然不存在，
照读就是 ENOENT。字段为 false 时 hit 附 `hint`：「原文未同步，先跑 `convert`」。

**关系的质量由 lint 保底**：`links` 与 `sources` 是后续行动的路径，断了引导就断。L2（断链）、
L6（源不存在或哈希不符）必须在 `ingest commit` 就挡住，不能等审计。

**纪律**：命中只提供入口，**引用页面里的结论前必须读正文**。拿一行 summary 当答案是编造，
这条写进 skill（v1 由 `wiki-audit` 与总纲承载，v2 落 `wiki-query`）。

**重建**：FTS 表属于 `state.db`，是可重建索引——`query --reindex` 从 `wiki/` 全文重灌。
丢了只影响速度，不影响正确性。

**升级触发线**（任一命中才换引擎，现在都离得很远）：

1. `wiki/` 正文 > 200 MB → 换 bigram 分词
2. 需要搜 `raw/` 全文 > 500 MB → 换 Tantivy
3. 单查询 P95 > 100 ms → 先查查询构造，再谈换引擎

### 5.9.1 链接图 `graph`

`query` 回答「哪页讲了这个」，`graph` 回答「这页在结构里是什么位置」。它只读、不落盘、不建缓存：
一次目录遍历，产出节点（出入度）、边、孤立页、断链。

```json
{"nodes":[{"page":"wiki/concept/order-timeout.md","in_degree":7,"out_degree":3}],
 "edges":[{"from":"wiki/concept/order-timeout.md","to":"wiki/entity/order.md"}],
 "orphans":["wiki/concept/scratch.md"],
 "broken":["concept/not-written-yet"]}
```

- 链接可以是 `[[concept/a]]`、`[[a]]` 或 `[[wiki/concept/a.md]]`，三种写法解析到同一条边；代码块里的 `[[` 不算。
- 一条边只出现一次，但出入度按提及次数算——同一页里写两遍 `[[b]]` 是更强的信号，不是两条边。
- 自指不算边；指向自己的链接不构成孤立的补集，孤立页的判据是出入度都为 0。
- `broken` 与 lint 的 L2 重叠，这里再列一次是因为消费场景不同：lint 报给人修，`graph` 报给 agent 判断
  「这条关系是不是本来该有、只是还没写」。

用途落在判断层：高扇入页是枢纽（改它要谨慎），孤立页是候选（可能是没接进网络的死内容），
`broken` 是被声称但不存在的知识缺口。三者都是审计者的线索，不是结论。

---

### 5.10 图片与无文本层

**不做 OCR**。目标语料以文字为主，扫描件不是主要输入；识别不出来时的正确做法是**明确降级**，不是猜。

但降级不等于「什么都不产出」——那样会留下一条永远进不了 `raw/` 的源，`scan` 每轮都得给它开特殊分支，
溯源链也断在半路。统一成一条规则：**每个源都产出产物，退化形式是 asset 型 raw**。

| 输入 | 产物 |
|---|---|
| 文本类（md/txt/html/pdf 有文本层） | 正常 raw，有正文 |
| 图片（png/jpg/gif/webp/svg） | asset 型 raw，正文只有链接 |
| 扫描件 PDF（探针判无文本层） | asset 型 raw，正文只有链接 + `no_text_layer: true` |

账本、缓存、变更判据、lint 全部走同一条路径，只在 frontmatter 上分叉。

**探针怎么判**：只解析头 2 + 中 2 + 尾 2 页，六页全无文本层才判扫描件。
只看前三页会误伤「封面是扫描件、正文是文字」的 PDF；三处抽样几乎不增加成本。

**asset 型 raw**：

```yaml
---
source: source/diagrams/order-flow.png
source_hash: blake3:...
parser: builtin-asset@0.1
converted_at: 2026-10-27T10:00:00Z
asset: true
---
![order-flow](../../source/diagrams/order-flow.png)
```

扫描件的形态完全相同，只是正文换成普通链接、多一个标记：

```yaml
---
source: source/path/scan.pdf
source_hash: blake3:...
parser: builtin-pdf@0.1
converted_at: 2026-10-27T10:00:00Z
asset: true
no_text_layer: true          # 说明为什么它没有正文
---
[scan.pdf](../../source/path/scan.pdf)
```

- `asset: true` 告诉下游「这不是知识来源，是资源」；lint 的 L9（空页）对它豁免。
- `no_text_layer: true` 记录降级原因，报告里单列，`doctor` / `audit` 能统计扫描件占比。
- **不复制原文件**：raw 里用相对路径直指 `source/`，保证只有一份真相。代价是 `source/` 不进 git，
  clone 后链接会裂——内部每台机器都有同步服务，这个代价我们认，不引入第二份。
- **路径深度是固定的**：`wiki/<type>/<id>.md` 与 `raw/<相对路径>.md` 都在根下两级，
  所以 `../../source/...` 在两种文件里都成立。这条写进 `SCHEMA.md`。
- asset **不单独建知识页**：孤立资源只登记，不产生页面；未被引用的由 L12 报 info。

---

### 5.11 会话记忆分层 `archive` / `recall`

**问题在哪**：会话是最高产的知识来源，也是最快把 wiki 撑爆的。1000 次对话可能只沉淀 30 条事实——
高频、低增量、话题高度重复。若一次会话产一份 `raw/`：

- 源数线性膨胀，而 `scan` 每轮对全部 raw 做全量哈希（`src/scan.rs:184`），成本随会话数线性上涨；
- 更早爆的是**页面侧**：同一事实被反复撰写，`sources` 只增不减，一年后单页挂几十条引用，没人读得动；
- 每轮 sync 都在改同一批页面，决策门退化成审批疲劳。

**所以会话不进 `raw/`**。`raw/` 的定义是「规范化后的源文件」，会话不是那东西。
新增的是**封存层** `wiki/archive/`：存的不是原文，是「对过去一段时间的事实源留存」。

| 层 | 位置 | 进 git | 内容 | 谁能读 |
|---|---|---|---|---|
| L0 活跃会话 | `.wiki/sessions/<id>.md` | 否 | 原始会话记录，可丢 | 只有写蒸馏文时 |
| L1 主题页 | `wiki/<type>/<id>.md` | 是 | 当前结论，正常参与 index/lint/query | 所有人 |
| L2 封存摘要 | `wiki/archive/<period>/<topic>.md` | 是 | 自包含蒸馏文，替代它原先那组会话的位置 | `query` 见摘要，`recall` 取正文 |
| L3 清理 | 只删 L0 | — | L2 永久保留 | — |

这四层对应 billion-context 的三条理念——分层摘要、只给入口不给正文、摘要必须自包含——
但**它的生命周期不能照搬**：那边块的生命周期绑在对话上（敢删是因为原文只对当前任务有用），
这里的生命周期必须绑在「这条事实是否还被引用」。

**三条判定**（缺一条就会误封存或误清理）

1. **封存必须人工指导**，不做自动封存。它同时是「合并」和「删原文」，两条都是不可逆的。
2. **触发是三轴，不是单一时间**。使用强度因人而异，纯时间阈值对不同强度的人不公平；
   但只用「未触达时长」又会封存掉最该留的东西——稳定结论（如「VIP 才重试」）本来就长期没人改，
   未触达恰恰因为它是靠得住的事实。所以：
   - 容量水位：`.wiki/sessions/` 超过 `memory.session_soft_cap` → 只喊「该总结了」，不动作；
   - 未触达时间：超 `memory.idle_days`；
   - 引用度：`graph` 的 backlinks 为 0。**零引用 + 长期未触达**才进候选，仍被引用的一律不动。
3. **清理双条件取较晚者**：容量（`purge_cap`）与期限（`purge_days`）同时满足才清 L0。
   高强度使用者不会因期限被误清，低频使用者也不会攒下几千个死文件。

**封存时的簿记**（全由 CLI 做，agent 只写蒸馏正文）

1. 校验：会话存在、未封存、归档 `id` 不与现有页冲突；
2. 落 L2：`archived: true`、`period`、`origin: conversation`，`sources` 记原会话路径；
3. **重挂 `sources`**：原本引用这组会话的页面，其 `sources` 改指 L2 路径——不重挂就等于清理时断链；
4. 出 `index.md`、进 FTS（摘要可搜）、追加 `log.md`；
5. `state.db` 增 `archive(key, level, sealed_at, last_touched, refs)`；
6. 记一次 `checkpoint`：归档是不可逆变更，必须有版本可回滚。

**`query` 怎么看待归档**：正常参与索引，命中与普通页同构，多两个字段。

```json
{"page": "wiki/archive/2026-Q4/vip-retry.md", "archived": true, "period": "2026-Q4",
 "recall": "llm-wiki recall vip-retry", "origin": "conversation"}
```

不采用「query 为空时才提示归档」：那等于记住了却想不起来，agent 根本不知道要去看。
正确形态是归档摘要**占据它原先那组会话的位置**，在 `links` / `sources` 关系里正常出现，
由 agent 自主决定要不要顺着 `recall` 追溯、追溯哪一篇。正文仍然不注入，一条命中一行入口。

**`recall`：归档层唯一读口**

- 输入归档 `id` 或话题词；输出摘要 + 路径 + 被哪些页引用 + 原文是否还在。
- 默认只给摘要与入口，`--full` 才返回蒸馏正文；**永不返回 L0 原文**（已过期或已清理，读了就是幻觉来源）。
- 与 `raw_available` 同风格：取不到时给 `hint`，不让 agent 撞 ENOENT。

**成本要记账**：`archive status` 输出四个数——活跃会话 / 可封存候选 / 可清理 / **封存后仍被引用的归档数**。
最后一个是这套机制唯一的自证指标：长期为 0，说明归档只是把噪声从 `raw/` 挪到了 `wiki/archive/`，
该收紧蒸馏标准，而不是继续封存。

**与既有约定的关系**

- CLI 仍不写正文：蒸馏文由 agent 写，CLI 只做搬运、重挂、记账、校验。
- L6 加豁免：`sources` 指向已封存或已清理的来源时按归档状态判定，不报 error（见 §5.4）。
- 归档页不进 `index.md`：目录是「当前知识」的视图，归档是「历史证据」，混排会让人误读。
- 不做向量检索、不做自动摘要：v1 只有「人工指导 + 机械簿记」，蒸馏质量由 skill 纪律负责。

---

## 6. 文件约定

**页面 frontmatter**

```yaml
id: order-processing        # kebab，等于文件名，等于 [[链接]]
title: 订单处理
type: concept               # ∈ wiki.toml [schema].types
summary: 一句话，进 index
tags: [支付, 后台]
sources: [raw/2026-03-01-x.md]
status: draft|stable|stale
updated: 2026-10-27         # 只由 CLI 写
origin: document            # document（缺省）| conversation（会话蒸馏而来）
archived: false             # 仅 wiki/archive/ 下为 true，替代它原先那组会话来源
```

- **机制层固定**（id/title/type/summary/sources/status/updated 必填，类型固定）；
  **枚举层可配**（`types`）。共演进落在 type 集合与正文规范上，校验器不会变成不设防。
- `sources` 是审计骨架：没有它就是「无源声称」。
- `origin` 区分来源可信度：缺省 `document`；`conversation` 表示结论来自会话蒸馏，判断层复核时优先看这些页。
- `updated` 由 CLI **行级替换**写入，保留注释、键序、引号风格、未知字段（不做全量 round-trip）。
- `SCHEMA.md` 要写清三件事：`archive/` 不进 `index.md`、`../../source/...` 的相对深度、
  `origin` 与 `archived` 的语义。`init` 模板同步这三行（代码侧由另一程序在写，本稿只定约定）。

**`.wiki/state.db`**（SQLite，不进 git）：`sources(path, hash, size, ingested_at)` + `source_pages` + `versions`
+ `fts`（页面全文索引，单字预切分）。
它是**可重建索引**——从页面 `sources` 反推、再对磁盘算哈希即可重建，所以丢了不致命。
不可重建的是 `.wiki/versions/` 与 `wiki/audit/`，那两个必须持久化。

**`index.md`**：按 `type` 分组的 `- [[id]] — summary`，首尾各一行「由 llm-wiki 生成」。
**`log.md`**：`## [YYYY-MM-DD] <动词> | <标题>` + 缩进子项，append-only，可 `grep "^## \["`。

---

## 7. Skill 切分

总纲 + 五能力，边界互斥：

| skill | 触发 | 拥有 | 不碰 |
|---|---|---|---|
| `llm-wiki` | 没有 `wiki.toml`；问用法；需要路由；收到决策门 | 初始化、约定解读、调度、纪律、**四选项怎么问** | 任何具体内容 |
| `wiki-scan` | 「有哪些新文件/哪些没进 wiki」 | 调 `scan`、解释差分、给下一步 | 解析、写页 |
| `wiki-parse` | 源不是 md、要抽文本 | 调 `parse`、处理失败、图片只登记不识别 | 写 wiki 页 |
| `wiki-ingest` | 「把这个加进 wiki」 | 读源、讨论要点、写页、commit、会话收尾 `checkpoint`、**会话封存**（写蒸馏文 + `archive seal --confirm`，§5.11） | 手写 index/log/日期；不做自动封存与自动清理 |
| `wiki-audit` | 「体检」「有没有矛盾」 | 跑 lint、读报告、判断层分析 | 机械修复（交 `--fix`） |
| `wiki-sync` | 「定时跑」「上游又同步了一批」 | 编排 scan→parse→ingest→lint、产出 staging、汇报决策门、退避 | 初始化、直接合并、封存与清理归档 |

封存不单独开 skill：它只在人工会话收尾时发生，和 `checkpoint` 同一时刻；塞进 `wiki-sync`
就会违反「无人值守不合并」那条。

共享约定放在被操作的仓库（`wiki.toml` + `SCHEMA.md`），**不放在 skill 包里**。
理由：Karpathy 说 schema 是人机共同演进——它属于那个知识库，不属于工具。

Claude Code 硬约束：frontmatter 只有 `name`(≤64) 与 `description`(≤1024)，正文建议 < 500 行，
引用文件只能一层深。所以 6 个 skill 都必须是薄的调度层，复杂逻辑落 CLI。

---

## 8. 跨平台与分发

- 单二进制，Rust；release 开 LTO + strip，目标 < 15 MB。
- 平台：darwin-arm64/x64、linux-arm64/x64、windows-x64。
- skill 里固定 `command -v llm-wiki` 再干活；`doctor --json` 给 `{version, backends, warnings}`。
  **版本只警告不硬失败**（除非 major 不符）。
- 外部后端（`pdftotext`、`pandoc`、外部 HTTP 服务）全部可选，缺失退回内嵌，`doctor` 报告可用性。
- 路径在 JSON 与 log 里统一 `/`；写文件统一 LF，读入容忍 CRLF。
- 大小写不归一：`doctor` 检测文件系统是否 case-insensitive 并警告「同名不同大小写会打架」。
- 符号链接默认不跟随；`.` 开头文件忽略；`source/` 与 `raw/` 下都允许嵌套目录（`raw/` 镜像 `source/` 的相对路径）。
- 安装按**内部使用**处理：不建 release 流水线。`cargo build --release` 出单二进制，
  `install.sh` / `install.ps1` 负责复制到 PATH + 可选把 skill 软链进 `.claude/skills/`。
  团队机器之间可以直接传二进制，`doctor` 打印 version 与 git commit 便于对齐。

---

## 9. 失败模式

| 失败 | 对策 |
|---|---|
| 摄取中途崩溃 | 候选写 staging、主目录未动；重跑即可，天然可重入 |
| 同一内容换名重摄 | 内容哈希去重，`scan` 报 `duplicate` |
| index 被手改 | commit 比对哈希报 `E_MANUAL_EDIT`；L4 兜底 |
| agent 绕过 CLI 写 log | 总纲明文禁止 + L5 抓格式 |
| 源更新、旧结论过期 | L6/L11 标 `stale`，agent 决定改页还是改 status |
| 页面爆炸 | `max_pages_per_source`：一轮 commit 写的页面数超限时**不拦**，在报告 `warnings` 里点名（CLI 拿不到「这页由哪个源生出」的可靠归属，硬拦会误伤多源批量；点名的用途是被 sync 的 audit 收走，让漂移可见） |
| 扫描件 PDF | 探针（头中尾六页）判无文本层 → 产出 asset 型 raw 并标 `no_text_layer`，不建知识页，**不 OCR** |
| 图片 | 产出 asset 型 raw（只有链接），不识别、不复制、不建独立页 |
| 外部解析服务不可用 | 回退内置并留 warning；两者都失败则同时报出两因 |
| token 被写进仓库 | 配置里只允许 `auth_env`；`doctor` 检查是否存在裸 token 字面量 |
| 同步服务写到一半 | `settle_seconds` 跳过，下一轮再来 |
| 定时任务与人工会话并发 | 文件锁，后到的退出码 4 |
| 源被改名 | 按 `moved` 只迁引用，不重摄、不产重复页 |
| 自动模式连续失败 | 3 轮后退避，关闭 `auto_ingest` |
| 忘了 checkpoint | 下轮 sync 报 `baseline_dirty`，要求先 checkpoint 或 approve |
| approve 时主目录被手改 | 哈希比对报 `staging_drift`，拒绝晋升，不静默覆盖 |
| 一直 defer 累积过多 | 超 `max_pending_sources` 在汇报里显式喊停 |
| 执行中文件又变 | 入场 settle + 读前后 stat 一致 + 收尾复核；仍不一致标 `racy` 留到下轮 |
| 追不上持续写入 | `lagging` 名单显式汇报并降优先级，不假装同步完成 |
| 批准时依赖源已过期 | `staging_stale` 拒绝合并 |
| 超大文件 | 强制分块 + 按需取块；超上限拒绝，需 `--force-large` |
| 源超过 100 MB | 记 `oversize` 并拒绝转换，汇报里列清单，不静默跳过 |
| 扫描件白跑全量解析 | 解析前先探针（头中尾六页判文本层），直接标 `no_text_layer` 出 asset raw |
| clone 后 `source/` 不存在 | hit 带 `raw_available=false` 与 hint，不让人撞 ENOENT |
| 大文件持续被写 | streak 永不达标，归 `blocked_by_stability` 单列汇报 |
| 大文件哈希太慢 | `size+mtime` 快判 + 抽样哈希，块级内容哈希兜底 |
| 编码问题 | 强制 UTF-8，非法编码报错不静默 |
| 源文件没转换 / 转换失败 | 不进 `raw/`，报 `unconverted`，不阻塞其他源 |
| 人手改过 `raw/` | 与转换产物哈希不符报 `raw_drift`；`pin: true` 不覆盖，否则重转前列出将被覆盖的文件 |
| `source/` 改名 | 两层各做一次 `moved` 迁移，`raw/*.md` 的 `source:` 字段同步改 |
| 会话数量线性膨胀 | 会话不进 `raw/`，只进 `.wiki/sessions/`；超 `session_soft_cap` 只喊「该总结了」，不自动动作 |
| 清理原文导致溯源断裂 | 封存时把页面 `sources` 重挂到归档页；L6 对归档来源豁免（§5.11） |
| 封存落进无人值守循环 | `sync` 硬拒（退出码 2），封存只在人工会话里显式执行 |
| 归档被当成事实源直读 | `recall` 默认只给摘要与入口，`--full` 才给正文，永不返回 L0 原文 |
| 归档摘要无人回查 | `archive status` 输出「封存后仍被引用的归档数」，长期为 0 就收紧蒸馏标准 |

---

## 10. v1 验收

- `fixtures/` 8 个「难看」样本：中文文件名、多栏 PDF、无正文标记 HTML、500 KB 单文件、
  同内容双文件名、空文件、混图 md、乱码 txt。
- PDF 断言放宽但仍是硬门禁：**必须命中 3 个关键短语**（多栏允许列序错乱）；
  扫描件 fixture 期望 `no_text_layer`（不是失败）：无产物、不建页、进报告清单。
- 端到端：`init → scan(7 new) → parse → 写 3 页 → commit → lint(0 error) →
  改一个源 → scan(1 changed) → lint(1 stale)`，逐步断言 JSON。
- 外部解析后端用 **本地 mock HTTP 服务** 测契约（成功 / 非 200 / 超时 / 缺 token 四种响应），
  不打真实内网地址。
- CRUD 全序列剧本：新增 → 改动 → 改名 → 删除 → 放入重复文件；断言 `moved` 不产生新页、`removed` 不删页、锁冲突返回 4。
- 决策门剧本：sync 产出 staging → `defer` 后主目录哈希不变、baseline 不动 →
  再改文件仍以原 baseline 求 diff → `approve` 后版本 +1、`versions/vN` 出现 → 手改主目录再 approve 命中 `staging_drift`。
- 大文件剧本：生成 30 MB 文本 → 断言 `mode: chunked`、`--range` 取块、二次解析命中块缓存、`sources` 片段引用通过 lint。
- 两层转换剧本：`source/` 放 pdf+html+md → `convert` 产出三份 `raw/*.md` 且 frontmatter 溯源正确 →
  只改 pdf 的元数据不改正文 → 重转后 `raw/` 哈希不变 → `scan` 报 0 changed（证明判据在规范化层）→
  手改 raw 且 `pin: true` → 重转不覆盖。
- 检索剧本：造 200 页 wiki → `query` 命中 → 断言 hit 带 `links` / `sources` / `raw_available`；
  `--expand 1` 只多一跳；搜 raw 独有词 **0 命中**（证明没索引 raw）。
- 图片与降级剧本：3 张 png + 1 份扫描件 PDF → `convert` 产出 **4 份** raw，全部 `asset: true`；
  3 份正文是图片链接，扫描件那份带 `no_text_layer: true` 且正文是普通链接 →
  页面引用图片 → lint 0 error（L9 不误报）→ 删掉引用后 L12 报 4 条 info。
- 会话记忆剧本：造 3 个会话 → `archive status` 报 3 活跃 / 0 候选 → 封存 1 个（agent 写蒸馏文 + `--confirm`）→
  断言 L2 出现、`index.md` 不含它、`query` 能命中摘要且 hit 带 `recall` → `recall --full` 返回正文且不含 L0 原文 →
  清掉 L0 后 `lint` 0 error（证明 L6 豁免生效）→ 再删掉主线页里的引用 → L13 报 info。
- CI 三平台，**不装 poppler/pandoc** 也要绿。

---

## 11. 推迟到 v2+

query skill（CLI 的 `query` 已在 v1 实现，v2 只加 skill 包装）；向量/关系库后端（替换 `query` 实现，不动上层）；交互界面；docx/xlsx；
页面级 diff 与「结论演化史」；会话记忆的**自动**蒸馏与自动封存（v1 只有人工指导封存，见 §5.11）。

---

## 12. 我替你做的决定（请复核）

1. **index/log/`updated` 由 CLI 生成**，与 Karpathy 原文「LLM 更新 index」相反。
2. 账本用 `.wiki/state.db`（SQLite），**不进 git**：它是可重建索引，扫一遍就长回来。
   需要审计的部分（版本账本）以可读形式落在 `.wiki/versions/` 里并进 git。
3. 摄取默认先汇报要点再落笔，`review_before_write=false` 可关；这是 skill 纪律，不是 CLI 强制。
4. v1 **实现** `query` CLI（FTS5，只索引 `wiki/`），但**不做 query skill**——v1 由 agent 直接调 CLI。
5. 6 个 skill：1 总纲 + 5 能力（scan / parse / ingest / audit / sync）。
6. 解析层内嵌优先、外部后端可选，不强制装 poppler。
7. `ingest commit` 以「全库反查 + agent 显式 `--pages`」为准，begin 的候选只是建议。
8. schema 分机制层（固定）与枚举层（可配），不做 type 改名迁移工具。
9. CLI 全程非交互，人工确认走退出码 3 + `--confirm`。
10. `audit/` 报告进 git，作为判断层的输入留档。
11. 解析层做成**可插拔后端 + 统一 markdown 出口**，内置优先，可配置接入内部 HTTP 解析服务。
12. 分发按内部使用处理：不建 release 流水线，本地构建 + 脚本复制。
13. 变更检测分 added / modified / moved / removed / duplicate 五类，`moved` 只迁引用不重摄。
14. 定时模式允许建页改页，禁止删页与合并页；自动写出的页 `status: draft` 待人审。
15. 所有写命令加文件锁，拿不到退出码 4；连续失败 3 轮自动退避。
16. `.wiki/` 定位为**引擎目录，只装活动产物**；契约（`wiki.toml`、`SCHEMA.md`）留在根目录可见可 review。
17. 版本快照自存 `.wiki/versions/<v>/`，不依赖 git；保留 `versions.keep` 个。
18. 定时任务永不自动合并，一律走决策门（approve / reject / instruct / defer），候选隔离在 `.wiki/staging/`。
19. 候选以增量方式维护：staging 为完整副本但逐页重写，配 `manifest.json` 做「新增检测」与「累积视图」两套比较，不做每轮全量 diff。
20. 一致性靠三层防线：入场 `settle_seconds`、读取前后 stat 一致 + 一次读入内存、收尾复核哈希；对不上的标 `racy` 留给下轮。
21. 自动摄取需要**稳定准入**：连续 3 次扫描哈希一致才摄；计数由 sync 维护，scan 保持纯读。
22. 大文件分段处理：2 MB 以上强制分块、100 MB 以上需显式放行；页面 `sources` 用 `#p=12-40` 片段引用。
23. **源分两层**：`source/` 存原始文件（任意格式、不可变），`raw/` 存转换后的 markdown（CLI 写）。
    页面引用 `raw/`，溯源链 page → raw → source。
24. **变更判据落在 `raw/` 的内容哈希上**，不落在 `source/` 上：重下文件、解析器重排换行都不触发重摄。
25. 转换阶段没有独立守护进程，`convert` 是普通子命令，由定时任务串在 `scan` 之前分段执行。
26. `raw/` 允许人手修（修 OCR），靠 frontmatter 的 `pin: true` 声明，否则重转会覆盖。
27. **检索用 SQLite FTS5，不引入 Tantivy**：与 `state.db` 同库、可同事务、零额外依赖，
    实测在 `wiki/` 规模上延迟不是瓶颈。Tantivy 留作「需要搜 raw 全文」时的替换项。
28. **`query` 只索引 `wiki/` 页面**，`raw/` 全文不进索引：raw 有几十 GB，而 query 回答的是
    「我们记过什么」。原文按 hit 里的 `sources` 路径直接读。
29. **单文件上限 100 MB**，超限记 `oversize` 并列清单，不静默跳过。这个上限解决的是解析速度与
    单次内存，不是 query 性能——后者靠「只索引 wiki」解决，两件事分开。
30. **命中带三类下一步线索**：`links`（邻页）、`sources`（原文路径，已拼好）、`backlinks`（枢纽度），
    默认只给一跳，`--expand` 才展开。CLI 提供入口，读不读由 agent 判断。
31. **解析前先探针**：解析前先探针（头 2 + 中 2 + 尾 2 共六个采样页）判定有无文本层，扫描件直接标
    `no_text_layer`，不在全量解析上白跑。
32. **锁按粒度分层**：长批次 `convert` 不持全局写锁，只有账本提交的毫秒级区间持锁。
33. **首次接入走长任务**：`convert --until-done` 独立跑完，不塞进定时任务的单轮预算。
34. **不做 OCR**：扫描件由探针（头中尾六页）判定无文本层后，产出与图片同构的 asset 型 raw 并标
    `no_text_layer: true`，不建知识页、不猜内容。
35. **每个源都产出 raw，没有例外**：降级形式统一为 asset 型 raw（只有链接）。这样账本、缓存、
    变更判据、lint 只走一条路径，不需要给「永远进不了 raw」的源开特殊分支。
36. **图片只做链接**：不识别、不复制、不单独建页；未被引用的 asset 由 L12 报 info。
    原文件留在 `source/`，raw 用 `../../source/...` 相对引用，保证只有一份真相。
37. **`min_chars` 是配置项**：§5.2 依赖它判定「内置产出不达标」，但 §3 的 wiki.toml 漏写。
    现补 `[parse].min_chars = 200`，随 T11 落到 `config.rs` 与 `init` 模板；不做成常量，
    因为不同语料（短笔记 vs 长文档）的达标线不同。
38. **会话不进 `raw/`，进封存层 `wiki/archive/`**：`raw/` 的定义是「规范化后的源文件」，会话不是那东西。
    归档存蒸馏摘要，替代它原先那组会话的位置；原文放 `.wiki/sessions/`，不进 git，可丢。
39. **封存必须人工指导，`sync` 硬拒**：封存 = 合并 + 删原文，两条都是 §5.5 禁止的不可逆动作，
    收到请求即以退出码 `2` 拒绝。v1 不做自动蒸馏，只做人工指导 + 机械簿记。
40. **封存触发三轴**：容量水位 + 未触达时间 + 引用度为 0。纯时间阈值对使用强度不公平；
    纯「未触达」会封存掉最靠得住的稳定结论。清理用「容量 + 期限」双条件同时满足，取较晚者。
41. **清理只删 L0，L2 永久保留**；封存时 CLI 把页面 `sources` 重挂到归档页，
    L6 对归档来源豁免、L9 对 `archived: true` 豁免。不重挂就必然断链，这是整套机制唯一的硬依赖。
42. **归档摘要进 `query` 索引**：摘要可搜，`recall --full` 才取蒸馏正文，永不返回 L0 原文。
43. **分块是读取层的派生物**：`convert` 不改，`raw/` 永远是一份完整 markdown；
    `parse --chunk/--range` 按需从原件切，缓存 `.wiki/cache/<hash>/` 可随时重建。
44. **`approve` 是唯一的发布门**：`ingest commit` 只把候选页写进 `.wiki/staging/` 并记账，
    不写 `wiki/index.md`、不追加 `wiki/log.md`。这样「`wiki/` = 已批准」是一条可验证的不变式，
    而不是靠每个写路径自觉维护；代价是交互式摄取也要先 `approve` 再 `checkpoint`。
45. **`checkpoint` 只拍已发布状态**：它快照当前 `wiki/`，不提升 staging。所以正确顺序是
    `ingest commit → approve --confirm → checkpoint`，漏掉 `approve` 会让新页游离在 staging 里。
46. **`doctor` 不返回 `data.root`**：它按当前目录工作，`data` 只有 `checks`。
    skill 里写「在仓库根目录执行」，不假装有根路径字段。
    不做「query 为空才提示归档」——那等于记住了却想不起来；由 agent 自主决定追溯哪一篇。
47. **`graph` 是只读派生视图**：一次目录遍历，不落库、不缓存。只给节点出入度、边、孤立页、断链；
    **不做「未链接提及」**——那要靠标题模糊匹配，误报率高且代价 O(页数²)，审计者自己读两页比信这个准。
48. **`--force-large` 只是 `parse` 的诊断口**：超过 `max_source_bytes` 的源永不进账本（`scan` 报 `oversize`、
    `convert` 跳过），但完全没法看它抽出来什么就没法判断「该拆分还是该调大上限」。所以只给 `parse` 一个
    强行出口，`convert`/`sync` 恒为 false，不提供任何「悄悄塞进 wiki」的路径。
49. **未初始化直接拒绝，不猜不补**：`wiki.toml` 或 `wiki/` 缺失时，所有仓库命令在入口处
    以 `E_NOT_INITIALISED`（exit 2）拒绝，`detail` 带 `missing`/`kind`/`hint`；只有 `init`、
    `doctor`、`version` 例外。`SCHEMA.md` 只由 `doctor` 报，因为没有任何代码路径读它。
    反过来，`source/`、`raw/`、`wiki/audit/`、`wiki/archive/` 与整个引擎目录属于**派生态**：
    每次命令入口按需创建，否则「克隆下来直接跑」就不可能（引擎目录被 gitignore）。
    判据是「工具不创建就没法继续 vs. 用户没做过的决定」，不是「init 建了哪些目录」。
