# llm-wiki

维护一份 Karpathy 式 LLM wiki 的命令行工具：仓库里的任意文件都是源，解析成 `.wiki/raw/` 里的 markdown，由人和 agent 一起写成 `.wiki/pages/` 页面，再用 lint、版本快照与 ingest 轮次保证它不腐烂。工具只往 `.wiki/` 里写，源文件永远只读。

## 服务说明

没有服务端，只有一个隐藏目录 `.wiki/`。引擎之外的一切都是只读的源：

| 层 | 位置 | 说明 |
|---|---|---|
| 源 | 仓库根（`.wiki/` 之外） | 唯一输入，任意格式，工具只读；`[scan].ignore` 可排除目录 |
| 契约 | `.wiki/wiki.toml`、`.wiki/SCHEMA.md` | 配置与页面类型枚举，人和 agent 都读 |
| 中间层 | `.wiki/raw/` | 解析产物，可重建；frontmatter 记录来源、哈希、解析后端 |
| 知识层 | `.wiki/pages/` | 页面、`index.md`、`log.md`、`audit/`、`archive/` |
| 引擎状态 | `.wiki/` 其余 | SQLite 账本、暂存区、版本 manifest 与对象库、会话、锁 |

写入边界是结构性的：契约、页面、账本全在 `.wiki/` 里，手写 `[paths]` 也移不出去。

一条源文件的完整生命周期：

1. **扫描**：`scan` 对比仓库根与账本 `state.db`，得出新增 / 修改 / 删除 / 改名。
2. **解析**：`convert` 把源交给解析后端，写 `.wiki/raw/<key>.md`，frontmatter 记录来源、`source_hash`、`parser` 与 `pin`。
3. **成页**：agent 读 `.wiki/raw/`，把候选页写进 `.wiki/staging/<type>/<id>.md`；`ingest begin / commit / abort` 管住一轮的读写。
4. **发布**：`approve --confirm` 把候选合并进 `.wiki/pages/` 并重建 `index.md`、追加 `log.md`；`reject` 记录原因后丢弃。
5. **校验**：`lint` 跑十三条规则，`--fix` 修掉不需要判断的机械缺陷。
6. **留痕**：`checkpoint` 记一份 manifest（`pages/` 与 `raw/` 每条的 blake3），字节按摘要存进 `.wiki/objects/`；`sync` 把「扫描 → 转换 → 稳定门槛 → 审计」串成无人值守的一轮。`history` / `diff` 读同一份 manifest，`rollback <vNNN>` 整版还原并把自己记成新版本；`.wiki/ignore` 挡住反复制造变动的源，页面超预算时 `approve` 会以 `page_budget` 停下。
7. **检索与续命**：`query` 用 FTS5 检索全库；`archive seal` 把会话摘要封存，`recall` 检索摘要，`archive purge` 按容量与时限清理。

解析层是可替换的后端集合：文本、HTML、图片、PDF 各有一个内置后端，另有一个可选的 `external-http` 后端把扫描件之类「内置读不出字」的文档交给外部 HTTP 服务。分层、数据流、模块表与不变量见 [架构与模块](docs/readme/architecture.md)，后端选择与回退规则见 [解析后端与 auto 回退](docs/readme/parse/external.md)。

## 服务部署操作

依赖只有 Rust stable（`rusqlite` 用 `bundled` 特性自带 SQLite，无需系统库），构建不联网的机器请预置好依赖缓存。

```bash
# 安装
cargo build --release --locked
install -m 0755 target/release/llm-wiki ~/.local/bin/llm-wiki
llm-wiki version
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
llm-wiki init              # 写 .wiki/wiki.toml、.wiki/SCHEMA.md，建 .wiki/ 下的目录
llm-wiki init --force      # 覆盖重写这两份契约文件（不动源文件与页面）
llm-wiki doctor            # 十一项健康检查，有 Fail 则退出码 1
llm-wiki doctor --json     # 机器可读：{"ok":true,"cmd":"doctor","data":{"checks":[...]}}
```

`init` 生成的 `.wiki/.gitignore` 只有一行 `*`：**整个引擎都不进 git**，包括契约、页面、账本与版本快照；仓库里进 git 的只有源文件。工具绝不修改仓库根的 `.gitignore`。CI 在 Ubuntu / macOS / Windows 三平台跑 `build`、`test`、`test --release`、`fmt --check`、`clippy -D warnings`，不安装 poppler/pandoc（PDF 测试用模板现造）。

### Agent skills 安装

1 个 skill 是给 Claude Code 用的薄指令层，所有簿记与判定逻辑都在 `llm-wiki` 里。
`skills/llm-wiki/SKILL.md` 是入口与路由，`skills/llm-wiki/references/` 下是六份按需阅读的手册
（扫描、摄取、解析、审计、同步、排障）：agent 只在需要时读对应那份，不把全部细节塞进上下文。
skill 只有 markdown，复制即可；CLI 是另一个可执行文件，按下面的方式放到能调用的地方。

```bash
# CLI：编译一次，放到 PATH 上
cargo build --release --locked
install -m 0755 target/release/llm-wiki ~/.local/bin/llm-wiki
llm-wiki version

# skill：复制进 agent 的技能根目录
cp -R skills/* ~/.claude/skills/
```

不装 Rust 的机器从 Release 页取对应平台的二进制，放到同一位置即可。

#### CLI 的两种放法

| 放法 | 调用 | 适合 |
|---|---|---|
| PATH 上的目录（如 `~/.local/bin/llm-wiki`） | `llm-wiki ...` | 一个人维护多个 wiki 仓库 |
| 仓库根的 `.wiki/llm-wiki` | `.wiki/llm-wiki ...` | 单个仓库自带工具，随仓库走 |

`.wiki/llm-wiki` 已在 `init` 生成的 `.wiki/.gitignore` 里，不会被提交。
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
| `skills/llm-wiki/SKILL.md` | 入口：边界、路由、最小序列、决策门、退出码、把 `--json` 翻成人话 |
| `references/scan.md` | `scan` / `convert`，七种变更的判读 |
| `references/ingest.md` | `ingest begin/commit/abort`，唯一要 agent 写正文的环节 |
| `references/parse.md` | 单文件解析诊断、后端选择、外部服务配置 |
| `references/audit.md` | `lint` 与判断层分析（矛盾、过时、孤儿、重复） |
| `references/sync.md` | 定时同步、读变更报告、失败退避 |
| `references/automation.md` | 页面预算、整版回退、`.wiki/ignore` 忽略列表、`sync --only` |
| `references/troubleshooting.md` | 错误码、`doctor` 检查项、旧布局与常见症状 |

装完在目标仓库根目录跑 `llm-wiki doctor` 自检。

## 服务操作说明

第一次使用先初始化；其余命令在未初始化的仓库里会直接以 `E_NOT_INITIALISED`
（exit 2）拒绝，并告诉你缺的是 `.wiki/wiki.toml` 还是 `.wiki/pages/`：

```bash
llm-wiki init          # 写 .wiki/wiki.toml / .wiki/SCHEMA.md，建目录；可重复跑，已有文件不覆盖
llm-wiki doctor        # 在 init 前后都能跑，用来诊断
```

日常一轮，按顺序：

```bash
llm-wiki scan                              # 报告仓库根的变更
llm-wiki convert --limit 5                 # 最多转换 5 个待转换源
llm-wiki convert --key notes/a.md          # 指定源，已转换也重转
llm-wiki parse --key docs/a.pdf            # 只看解析结果，不写任何文件

llm-wiki ingest begin --sources notes/a.md --pages concept/alpha
# ……agent 把候选页写进 .wiki/staging/concept/alpha.md……
llm-wiki ingest commit --pages concept/alpha
llm-wiki lint --fix                        # 先修机械缺陷，再报告剩下的
llm-wiki approve --confirm                 # 合并候选进 .wiki/pages/
llm-wiki checkpoint --label "after alpha"
llm-wiki history                           # 打过哪些版本
llm-wiki diff v003                         # 与当前工作树差了什么
llm-wiki rollback v003                     # 整版还原（回退本身也记一版）

llm-wiki graph                            # 链接图：枢纽页、孤立页、断链
llm-wiki query 熔断                        # FTS5 检索，命中数 0 也是 exit 0
llm-wiki query --reindex                   # 全量重建索引后再搜
llm-wiki sync --dry-run                    # 演练一轮：只报告，不转换、不写审计
llm-wiki sync --limit 3 --force            # 忽略稳定门槛，真跑一轮
llm-wiki sync --only notes/a.md            # 只跟用户点名的源
llm-wiki ignore add "vendor/**"            # 之后 scan/convert/purge 都跳过

llm-wiki archive status
llm-wiki archive seal --sources notes/a.md --title "2026-10 摘要" --period-end 2026-10-31
llm-wiki recall "重试策略" --full
llm-wiki archive purge --dry-run           # 先看会删什么
```

完整命令清单：

| 命令 | 作用 |
|---|---|
| `version` | 打印版本 |
| `doctor` | 检查契约、目录、账本、FTS5、gitignore、锁、暂存区、token、外部后端 |
| `init [--force]` | 写契约文件、建引擎目录 |
| `scan [--apply-moves]` | 报告仓库根、`.wiki/raw/` 与账本的差异；`--apply-moves` 一并迁移改名 |
| `convert [--key <KEY>] [--limit <N>]` | 转换待处理源（默认 10）；单个失败不中断 |
| `parse --key <KEY> [--chunk <N>] [--range <A-B>] [--force-large]` | 只显示解析结果，不写文件；大文件按需分块；`--force-large` 越过尺寸上限只为看一眼 |
| `ingest begin [--sources <K>]... [--pages <P>]... [--dry-run]` | 开一轮，报告受影响的页面 |
| `ingest commit [--pages <P>]...` | 关闭本轮，刷新候选页与暂存清单 |
| `ingest abort` | 丢弃本轮，保留已写候选页 |
| `lint [--strict] [--fix]` | 十三条规则；`--strict` 让警告也失败 |
| `checkpoint --label <LABEL>` | 记 manifest 与内容对象，成为新版本（只保留最近 `[versions].keep` 份） |
| `history` | 列出版本：`version` / `kind` / `label` / `created_at` / `pages` |
| `diff <VERSION>` | 该版本与当前工作树的差异：`added` / `removed` / `changed` |
| `rollback <VERSION>` | 整版还原 `pages/` 与 `raw/`、重建索引，并把自己记成新版本（exit 2 `E_NO_VERSION` 表示没有这个版本） |
| `ignore list` | 列出 `.wiki/ignore` 的规则 |
| `ignore add <PATTERN>...` / `ignore remove <PATTERN>...` | 增删忽略规则（`.gitignore` 语法，`#` 注释、`!` 取反、`/` 锚定、`**` 跨目录） |
| `ignore check <PATH>` | 报 `ignored`、命中的 `rule` 与归一化后的 `path` |
| `approve [--confirm]` | 发布候选；不带 `--confirm` 只报告（exit 3） |
| `reject --reason <REASON>` | 丢弃候选并记录原因 |
| `instruct --text <TEXT>` | 给下一次 agent 会话留话（写入 `.wiki/next-instruction.md`） |
| `defer` | 保留队列，不改变任何状态 |
| `archive status` | 报告归档数、活跃会话、可清理数、仍被引用数 |
| `archive seal --sources <K>... [--id] [--title] [--period-start] [--period-end] [--by]` | 登记 agent 写好的摘要，并改写引用页的 `sources` |
| `archive purge [--dry-run] [--yes]` | 删除同时满足「超容量」与「超时限」的摘要 |
| `recall <QUERY> [--full]` | 子串检索摘要；无命中 exit 1 |
| `graph` | 链接图：节点出入度、边、孤立页、断链（只读） |
| `query [TEXT] [--limit <N>] [--expand <N>] [--scope all\|wiki\|archive] [--reindex]` | FTS5 检索页面；无命中 exit 0 |
| `sync [--limit <N>] [--dry-run] [--force] [--only <PATH>...]` | 无人值守一轮：scan → convert → 门槛 → 审计；`--only` 只处理点名的源并跳过早写门槛 |

机器可读输出：任意命令加全局 `--json`，stdout 只有一行 JSON。

```json
{"ok":true,"cmd":"convert","data":{...}}
{"ok":false,"cmd":"parse","error":{"code":"E_NO_AUTH_ENV","message":"external parser token variable WIKI_PARSE_TOKEN is not set"}}
```

退出码：

| 码 | 含义 |
|---|---|
| `0` | 成功；`query` 无命中、`doctor` 只有 Warn 也是 0 |
| `1` | 命令执行失败（含部分源失败），或 `lint` 有 error / `--strict` 下有 warning，或 `recall` 无命中、`doctor` 有 Fail |
| `2` | 用法、配置或环境错误 |
| `3` | 需要人拍板：`approve` 未带 `--confirm`（含 `page_budget` 超预算）、`archive purge` 未带 `--yes` |
| `4` | 引擎被另一个进程锁住（`lock` 真正被占用，不是过期残留） |

## 服务特殊说明

- 配置只有 `.wiki/wiki.toml`（另有人类可读的 `.wiki/SCHEMA.md`）。`[parse]` 与 `[parse.external]` 的每个键、默认值与校验规则见 [parse 配置](docs/readme/config/parse.md)；`[scan]`、`[formats]`、`[schema]`、`[ingest]`、`[audit]`、`[versions]`、`[sync]`、`[memory]` 的行为分别由对应模块读取。`[paths]` 已固定为 `.wiki/` 内，写进配置也会被忽略。
- 解析外部服务的 token **只从环境变量读**：`[parse.external].auth_env` 存的是变量名，不是 token 字面量。把 `Bearer xxx`、`sk-...` 这类值写进 `auth_env` 会被 `Config::validate` 以 `E_TOKEN_IN_CONFIG` 拒绝，`doctor` 的 `token` 检查也会报 Fail。
- `parse.primary` 取 `external` 却没写 `[parse.external]` 时，`doctor` 的 `external_backend` 直接报 Fail（每个解析都会以 `E_NO_EXTERNAL` 失败）；服务不可达只报 Warn，因为服务可能在别处恢复。
- 内置解析后端的 id 会写进 `.wiki/raw/` frontmatter：`builtin-text`、`builtin-html`、`builtin-asset`、`builtin-pdf`、`external-http`。升级解析器后可以据此筛出需要重转的源。
- 常见错误码：`E_NO_PARSER`（无后端认领扩展名）、`E_BAD_SOURCE`（后端认领但解不开）、`E_NO_AUTH_ENV`、`E_EXTERNAL`、`E_BOTH_FAILED`（`auto` 下内置与外部都失败）、`E_BAD_CONFIG`（外部后端配置残缺）、`E_OVERSIZE`（超过 `max_source_bytes`）、`E_STAGING_CORRUPT` / `E_STAGING_DRIFT`（暂存区与清单不一致）、`E_NO_SUMMARY` / `E_ALREADY_SEALED`（封存前置条件不满足）、`E_LOCKED`（仓库被占用）。
- 版本在 `.wiki/versions/vNNN/`：`manifest.json` 记路径与摘要，`.version.json` 记 `kind` / `label` / `pages`，字节按摘要存在 `.wiki/objects/`（同一内容只存一份）。只保留最近 `[versions].keep` 份（默认 5）；`log.md` 记录每次 `approve` / `checkpoint` / `seal` / `rollback`。索引不进版本，回退后自动重建。
- 页面预算：`[ingest].max_new_pages_per_run`（默认 20）限制 `approve` 一轮新增的页数，超出时返回 `reason: page_budget` 并把候选留在 `.wiki/staging/`；`approve --confirm` 是人显式越过预算。
- 忽略列表 `.wiki/ignore` 用 `.gitignore` 语法，`scan` / `convert` 跳过、`archive purge` 不清；显式 `convert --key` 仍可读单个文件。`.wiki/` 不随 git 共享，所以忽略列表也是每台机器一份。
- 仓库锁 `.wiki/lock` 的 TTL 是 600 秒；崩溃残留会被下一次运行回收，只有真正被别的进程持有时才以 exit 4 退出。
- 索引是派生状态：`query` 前会增量刷新，刚 `approve` 的页也能立刻搜到；怀疑索引不一致时用 `query --reindex` 全量重建。
- `sync` 的 `--dry-run` 保证不写盘（`settle_seconds` 与 `stable_streak` 门槛在演练里同样生效），`--force` 才跳过早写门槛。
