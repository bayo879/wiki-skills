# llm-wiki

维护一份 Karpathy 式 LLM wiki 的命令行工具：仓库里的任意文件都是源，解析成 `.wiki/raw/` 里的 markdown，由人和 agent 一起写成 `.wiki/pages/` 页面，再用版本快照与轮次保证它不腐烂。工具只往 `.wiki/` 里写，源文件永远只读。

## 服务说明

没有服务端，只有一个隐藏目录 `.wiki/`。引擎之外的一切都是只读的源：

| 层 | 位置 | 说明 |
|---|---|---|
| 源 | 仓库根（`.wiki/` 之外） | 唯一输入，任意格式，工具只读；`[scan].ignore` 可排除目录 |
| 契约 | `.wiki/wiki.toml`、`.wiki/SCHEMA.md` | 配置与页面类型枚举，人和 agent 都读 |
| 中间层 | `.wiki/raw/` | 解析产物，可重建；frontmatter 记录来源、哈希、解析后端 |
| 知识层 | `.wiki/pages/` | 页面、`index.md`、`audit/` |
| 引擎状态 | `.wiki/` 其余 | SQLite 账本、暂存区、版本 manifest 与对象库、决策日志 `journal.jsonl`、会话、锁 |

写入边界是结构性的：契约、页面、账本全在 `.wiki/` 里，手写 `[paths]` 也移不出去。

对外只有六条命令（外加 `--version`）：`doctor`、`ingest`、`submit`、`query`、`version`、`ignore`。转换、开轮、登记、版本锚点、索引、改名、settle 都在命令内部完成——人只需要决定「读什么」和「接受还是丢」。

一条源文件的完整生命周期：

1. **看**：`doctor` 体检十项，`doctor --fix` 修掉不花钱的机械缺陷；`ingest --dry-run` 报告这一轮会碰哪些源、会转多少页。
2. **摄入**：`ingest` 扫仓库把新增 / 修改 / 删除 / 改名交给解析后端，写 `.wiki/raw/` 并登记账本，开一轮并把候选页的落点 `write_to` 给出来；顺手记一个版本锚点。
3. **写页**：agent 读 `.wiki/raw/`，把候选页写进 `ingest` 给的 `.wiki/staging/<type>/<id>.md`。
4. **收口**：`submit` 把整份暂存区发布进 `.wiki/pages/`、重建 `index.md`、重建索引、记一个发布版本——不等人，三道刹车只扣住命中的候选，其余的照常发布；`--confirm` 是显式越权，`--auto` 把扣下的跳过。不想要这一轮，用 `version restore <本轮 ingest 版本>`：页面与源回到摄入前，暂存区一并清空；如果同一批源不该再被扫到，用 `ignore --add` 挡住。两种决策都只追加一行 `.wiki/journal.jsonl`。
5. **检索与续命**：`query` 用 FTS5 检索全库，`query --graph` 看链接结构；写进 `[ingest].session_dir` 的会话蒸馏产物默认也在结果里，`query --no-archive` 才把它们藏起来。

解析层是可替换的后端集合：文本、HTML、图片、PDF 各有一个内置后端，另有一个可选的 `external-http` 后端把扫描件之类「内置读不出字」的文档交给外部 HTTP 服务。分层、数据流、模块表与不变量见 [架构与模块](docs/readme/architecture.md)，后端选择与回退规则见 [解析后端与 auto 回退](docs/readme/parse/external.md)。

## 服务部署操作

依赖只有 Rust stable（`rusqlite` 用 `bundled` 特性自带 SQLite，无需系统库），构建不联网的机器请预置好依赖缓存。

```bash
# 安装
cargo build --release --locked
install -m 0755 target/release/llm-wiki ~/.local/bin/llm-wiki
llm-wiki --version
```

本仓库自带离线依赖目录，开发与测试一律这样跑：

```bash
CARGO_HOME=$PWD/.cargo-home CARGO_NET_OFFLINE=true cargo build
CARGO_HOME=$PWD/.cargo-home CARGO_NET_OFFLINE=true cargo test
CARGO_HOME=$PWD/.cargo-home CARGO_NET_OFFLINE=true cargo test --release --locked
CARGO_HOME=$PWD/.cargo-home CARGO_NET_OFFLINE=true cargo fmt --check
CARGO_HOME=$PWD/.cargo-home CARGO_NET_OFFLINE=true cargo clippy --all-targets -- -D warnings
```

在任意仓库根目录初始化并自检：

```bash
llm-wiki doctor --fix      # 写 .wiki/wiki.toml、.wiki/SCHEMA.md，建 .wiki/ 下的目录
llm-wiki doctor            # 十项健康检查，有 Fail 则退出码 1
llm-wiki doctor --json     # 机器可读：{"ok":true,"cmd":"doctor","data":{"checks":[...]}}
```

`doctor --fix` 也覆盖已经初始化的仓库：缺的目录补建、契约文件不存在就写、索引坏了重建；它从不删文件、不改源。

`.wiki/` 里是本地引擎（契约、页面、账本、版本），工具不生成也不修改任何 git 配置。用 git 的仓库请把 `.wiki/` 加进仓库根的 `.gitignore`；没有 git 的环境不需要这一步。CI 在 Ubuntu / macOS / Windows 三平台跑 `build`、`test`、`test --release`、`fmt --check`、`clippy -D warnings`，不安装 poppler/pandoc（PDF 测试用模板现造）。

### Agent skills 安装

两个 skill 是给 Claude Code 用的薄指令层，所有簿记与判定逻辑都在 `llm-wiki` 里。

`skills/llm-wiki/` 维护 wiki 本身：`SKILL.md` 是入口（边界、最小序列、门禁、退出码）；`content.md` 写「写什么」（建页判断、新源、切片、矛盾、去重）；`ops.md` 写「出了问题怎么看」（体检、判断层、冲突与回退、忽略、叫人）。
`skills/llm-wiki-session/` 写会话蒸馏：从一段对话里抽出值得留给项目的判断，产物写进 `[ingest].session_dir`（默认 `sessions`），原始会话不存。一个会话一个文件，不按时间切。

字段名、候选路径、缺失字段、报错码含义由 CLI 现打，技能不复述。skill 只有 markdown，复制即可；CLI 是另一个可执行文件，按下面的方式放到能调用的地方。

```bash
# CLI：编译一次，放到 PATH 上
cargo build --release --locked
install -m 0755 target/release/llm-wiki ~/.local/bin/llm-wiki
llm-wiki --version

# skill：复制进 agent 的技能根目录
cp -R skills/* ~/.claude/skills/
```

不装 Rust 的机器从 Release 页取对应平台的二进制，放到同一位置即可。

#### CLI 的两种放法

| 放法 | 调用 | 适合 |
|---|---|---|
| PATH 上的目录（如 `~/.local/bin/llm-wiki`） | `llm-wiki ...` | 一个人维护多个 wiki 仓库 |
| 仓库根的 `.wiki/llm-wiki` | `.wiki/llm-wiki ...` | 单个仓库自带工具，随仓库走 |

`.wiki/llm-wiki` 在 `.wiki/` 下；如果仓库用 git，记得把 `.wiki/` 加进根 `.gitignore`，否则它会被当成未跟踪文件。
两种放法都要求在**仓库根目录**执行；CLI 不向上查找根。

#### 把 skill 同步给别人

`skills/` 是纯文本，整个复制进对方的技能根目录即可，不需要本仓库、不需要 Rust。
对方另外需要一份 `llm-wiki`，从 Release 页按平台取：

| 平台 | 资产 |
|---|---|
| macOS（Apple Silicon） | `llm-wiki-aarch64-apple-darwin` |
| macOS（Intel） | `llm-wiki-x86_64-apple-darwin` |
| Linux x86_64 | `llm-wiki-x86_64-unknown-linux-gnu` |
| Linux ARM64 | `llm-wiki-aarch64-unknown-linux-gnu` |
| Windows x86_64 | `llm-wiki-x86_64-pc-windows-msvc.exe` |

#### 发布

本机打不出 Linux / Windows 的二进制（缺目标平台的 C 工具链，`libsqlite3-sys` 是 bundled C）。
推一个 tag，`Release` workflow 在三个原生 runner 上各跑一遍 `cargo test --release`，
产出对应平台的一个二进制并挂到 Release 页：

```bash
git tag v0.1.1 && git push origin v0.1.1
```

没有 tag 时也可以去 Actions 页面手动跑 `Release`（`workflow_dispatch`）。

| 文件 | 管什么 |
|---|---|
| `skills/llm-wiki/SKILL.md` | 入口：边界、最小序列、门禁、`submit` 选项、退出码 |
| `skills/llm-wiki/content.md` | 写什么：一页一事、新源怎么处理、切片、前后矛盾、重复 |
| `skills/llm-wiki/ops.md` | 坏了怎么办：体检、判断层、冲突与回退、忽略、叫人 |
| `skills/llm-wiki-session/SKILL.md` | 会话蒸馏：抽什么、写到哪、怎么去重 |

装完在目标仓库根目录跑 `llm-wiki doctor` 自检。

## 服务操作说明

第一次使用跑 `doctor --fix`；其余命令在未初始化的仓库里会直接以 `E_NOT_INITIALISED`
（exit 2）拒绝，并告诉你缺的是 `.wiki/wiki.toml` 还是 `.wiki/pages/`。

日常一轮，按顺序：

```bash
llm-wiki doctor                            # 体检 + 看这一轮有什么变动
llm-wiki ingest --dry-run                  # 演练：只报告，不写盘
llm-wiki ingest                            # 开轮：转换、登记、给出 write_to
# ……agent 把候选页写进 ingest 给的 .wiki/staging/<type>/<id>.md……
llm-wiki submit                            # 收口：发布、索引
llm-wiki query 熔断                        # FTS5 检索，命中数 0 也是 exit 0
llm-wiki query --graph                     # 链接图：枢纽页、孤立页、断链
llm-wiki version list                      # 每版一行：谁打的、几页
llm-wiki version diff                      # 最新版与工作树的差异
llm-wiki version restore v003 --confirm    # 回到 v003，并清空本轮候选
```

人工点名摄入、只看解析、无人值守：

```bash
llm-wiki ingest notes/a.md docs/b.pdf      # 只摄入点名的源
llm-wiki ingest --key docs/a.pdf           # 只看解析结果，不写任何文件
llm-wiki ingest --key docs/a.pdf --chunk 2000 --range 0-4000   # 大文件按需分块看
```

无人值守没有独立命令：定时跑 `ingest`，再用 `submit --auto` 收口。

收口的几种姿势：

```bash
llm-wiki submit --confirm      # 越过全部三道刹车
llm-wiki submit --auto         # 扣下的跳过、其余发布，exit 0
llm-wiki submit --json         # 看 data.held_back / data.skipped 与 stats
llm-wiki version restore v003 --confirm   # 回到某次 ingest 之前，清空暂存区
```

其余的小工具：

```bash
llm-wiki doctor --fix                      # 补目录、写契约、重建索引
llm-wiki doctor --only convert --key a.pdf # 只跑转换，并打出失败原因
llm-wiki query --no-archive "重试"         # 不搜会话蒸馏目录下的页面
llm-wiki query --limit 5 熔断              # 最多 5 条
llm-wiki --version                         # 打印版本
```

完整命令清单：

| 命令 | 作用 |
|---|---|
| `doctor [--fix] [--only scan\|convert\|session] [--key <KEY>]` | 体检十项；`--fix` 补建目录/契约/索引，`--only` 只跑一项，`--key` 让转换只处理一个源并打出失败原因 |
| `ingest [<KEY>...] [--dry-run] [--key <KEY>] [--chunk <N>] [--range <A-B>] [--force-large]` | 唯一的写入口：扫描 + 转换 + 登记 + 开轮 + 版本锚点；给位置参数只处理点名的源；`--key` 只看解析 |
| `submit [--confirm] [--auto]` | 收口：发布整份暂存区、重建索引、记版本；三道刹车只扣住命中的；`--confirm` 越过全部刹车，`--auto` 扣下的跳过并 exit 0，全部被扣下才 exit 3 |
| `version restore <TAG> --confirm` | 页面与源回到某个版本，暂存区清空；不想要的源用 `ignore --add` 挡住 |
| `query [TEXT] [--no-archive] [--graph] [--limit <N>]` | FTS5 检索；`--no-archive` 排除 `[ingest].session_dir` 下的页，`--graph` 看链接图，无命中 exit 0 |
| `--version` | 打印版本 |

机器可读输出：任意命令加全局 `--json`，stdout 只有一行 JSON。

```json
{"ok":true,"cmd":"ingest","data":{...}}
{"ok":false,"cmd":"query","error":{"code":"E_NO_WIKI","message":"..."}}
```

退出码：

| 码 | 含义 |
|---|---|
| `0` | 成功；`query` 无命中、`doctor` 只有 Warn、`submit --auto` 跳过扣下的候选也是 0 |
| `1` | 命令执行失败（含部分源失败），或 `doctor` 有 Fail |
| `2` | 用法、配置或环境错误（含未初始化、源文件读不了） |
| `3` | 需要人看一眼：`submit` 这一轮**每个**候选都被刹车扣下（`page_overlap` / `page_slice_budget` / `page_budget`），一页也没发出去 |
| `4` | 引擎被另一个进程锁住（`lock` 真正被占用，不是过期残留） |

## 服务特殊说明

- 配置只有 `.wiki/wiki.toml`（另有人类可读的 `.wiki/SCHEMA.md`）。`doctor --fix` 生成的模板把每个键与默认值都写出来，改哪一项就取消注释哪一行。`[parse]` 与 `[parse.external]` 的每个键、默认值与校验规则见 [parse 配置](docs/readme/config/parse.md)；`[scan]`、`[formats]`、`[schema]`、`[ingest]`、`[audit]`、`[versions]` 的行为分别由对应模块读取。无人值守的节奏（`settle_seconds`、`stable_streak`、`auto_ingest` 等）在 `[ingest]` 里。`[paths]` 已固定为 `.wiki/` 内，写进配置也会被忽略。
- 会话蒸馏：`[ingest].session_dir`（默认 `sessions`，写空串关闭）下的页面和其他源一样入库、默认也在检索结果里；`query --no-archive` 按这个目录前缀把它们排除。目录本身就是标记，不新增 frontmatter 字段；页面 id 保留源目录前缀，可以随时挪位置。
- 解析外部服务的 token **只从环境变量读**：`[parse.external].auth_env` 存的是变量名，不是 token 字面量。把 `Bearer xxx`、`sk-...` 这类值写进 `auth_env` 会被 `Config::validate` 以 `E_TOKEN_IN_CONFIG` 拒绝，`doctor` 的 `token` 检查也会报 Fail。
- `parse.primary` 取 `external` 却没写 `[parse.external]` 时，`doctor` 的 `external_backend` 直接报 Fail（每个解析都会以 `E_NO_EXTERNAL` 失败）；服务不可达只报 Warn，因为服务可能在别处恢复。
- 内置解析后端的 id 会写进 `.wiki/raw/` frontmatter：`builtin-text`、`builtin-html`、`builtin-asset`、`builtin-pdf`、`external-http`。升级解析器后可以据此筛出需要重转的源。
- 常见错误码：`E_NO_WIKI`（仓库根没有页面目录或配置）、`E_NOT_INITIALISED`（没跑过 `doctor --fix`）、`E_LOCKED`（仓库被占用）、`E_NO_PARSER`（无后端认领扩展名）、`E_BAD_SOURCE`（后端认领但解不开）、`E_NO_AUTH_ENV`、`E_EXTERNAL`、`E_BOTH_FAILED`（`auto` 下内置与外部都失败）、`E_BAD_CONFIG`、`E_OVERSIZE`（超过 `max_source_bytes`）、`E_STAGING_CORRUPT` / `E_STAGING_DRIFT`（暂存区与清单不一致）。
- 版本在 `.wiki/versions/vNNN/`：`manifest.json` 记路径与摘要，`.version.json` 记 `kind` / `label` / `pages`，字节按摘要存在 `.wiki/objects/`（同一内容只存一份）。每轮 `ingest` 记一个 `kind: ingest` 锚点，每轮 `submit` 记一个发布版本；只保留最近 `[versions].keep` 份（默认 5）。`.wiki/journal.jsonl` 记录每次 `submit` / `version restore`（一行一个 JSON，不进版本、不被回退抹掉）。
- 三道刹车都在 `submit` 不带 `--confirm` 时生效，命中的候选留在 `.wiki/staging/`、其余照常发布，所以无人值守链路也能跑完：近义（`page_overlap`）、切片预算（`page_slice_budget`）、页面预算（`page_budget`，默认 20，0 表示不限）。只有全部候选都被扣下时才 exit 3，此时确实需要人看一眼；`--confirm` 是显式越过全部三道，`--auto` 是无人值守地跳过它们。
- 切片预算：`[ingest].max_pages_per_slice`（默认 5，0 表示不限）限制「同一个切片养出多少页」。切片是 `#p=<n>` 指向的那一段（大文件按 `parse.chunk_target` 切块），没带片段的页算整份文件一个切片。所以长文档按信息量拿页数，短文件不能靠拆 id 刷页；小于切块阈值的文件，页面上写的任何 `#p=` 都算整份文件那一个切片，编造不存在的切片换不到额度。超出时这一页被扣下（`reason: page_slice_budget`），已在库里的页继续算数，所以要腾额度得先删旧页。
- 近义页判定：`submit` 会扣下两种「同一页写了两遍」——标题归一化后与已发布页相同（`Retry Policy` = `retry_policy`），或候选页引用的源全部是某张已发布页引用过的（没有新证据）。命中的候选不发布（`reason: page_overlap`），`detail` 点名对端页；判定只比 frontmatter，不读正文，所以不花钱、不误判语义。
- 无人值守：`ingest` 是显式请求，开轮就立刻执行，不等任何窗口；定时跑 `ingest` 再用 `submit --auto` 收口就是无人值守链路（扣下的候选跳过并留在暂存区）。`--dry-run` 保证不写盘。
- 忽略列表 `.wiki/ignore` 用 `.gitignore` 语法，`ingest` 跳过；显式 `ingest <KEY>` 仍可读单个文件。`.wiki/` 不随 git 共享，所以忽略列表也是每台机器一份。
- `.wiki/` 是这台机器的事实源，**不随仓库共享，CLI 也不提供同步**。不考虑跨机器：不要手动搬、不要合并账本，两边各写再拼会得到互相矛盾的 `source_hash`。目录内只存仓库相对路径（键由 `paths::identity_key` 产出）。
- 仓库锁 `.wiki/lock` 的 TTL 是 600 秒；崩溃残留会被下一次运行回收，只有真正被别的进程持有时才以 exit 4 退出。
- 索引是派生状态：`query` 前会增量刷新，刚 `submit` 的页也能立刻搜到；怀疑索引不一致时用 `doctor --fix` 全量重建。
