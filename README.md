# llm-wiki

维护一份 Karpathy 式 LLM wiki 的命令行工具：源文件进 `source/`，解析成 `raw/` 里的 markdown，由人和 agent 一起写成 `wiki/` 页面，再用 lint、版本快照与 ingest 轮次保证它不腐烂。

## 服务说明

五个目录层，全部在仓库里，没有服务端：

| 层 | 位置 | 说明 |
|---|---|---|
| 契约 | `wiki.toml`、`SCHEMA.md` | 配置与页面类型枚举，人和 agent 都读 |
| 源 | `source/` | 唯一输入，工具只读 |
| 中间层 | `raw/` | 解析产物，可重建；frontmatter 记录来源、哈希、解析后端 |
| 知识层 | `wiki/` | 页面、`index.md`、`log.md`、`audit/`、`archive/` |
| 引擎 | `.wiki/` | SQLite 账本、暂存区、版本快照、会话、锁 |

一条源文件的完整生命周期：

1. **扫描**：`scan` 对比 `source/` 与账本 `state.db`，得出新增 / 修改 / 删除 / 改名。
2. **解析**：`convert` 把源交给解析后端，写 `raw/<key>.md`，frontmatter 记录来源、`source_hash`、`parser` 与 `pin`。
3. **成页**：agent 读 `raw/`，把候选页写进 `.wiki/staging/<type>/<id>.md`；`ingest begin / commit / abort` 管住一轮的读写。
4. **发布**：`approve --confirm` 把候选合并进 `wiki/` 并重建 `index.md`、追加 `log.md`；`reject` 记录原因后丢弃。
5. **校验**：`lint` 跑十三条规则，`--fix` 修掉不需要判断的机械缺陷。
6. **留痕**：`checkpoint` 给 `wiki/` 拍版本快照；`sync` 把「扫描 → 转换 → 稳定门槛 → 审计」串成无人值守的一轮。
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
llm-wiki init              # 写 wiki.toml、SCHEMA.md，建 source/ raw/ wiki/ .wiki/
llm-wiki init --force      # 覆盖重写这两份契约文件（不动 source/ 与 wiki/ 页面）
llm-wiki doctor            # 十一项健康检查，有 Fail 则退出码 1
llm-wiki doctor --json     # 机器可读：{"ok":true,"cmd":"doctor","data":{"checks":[...]}}
```

`init` 生成的 `.wiki/.gitignore` 只忽略可重建的状态（`cache/`、`staging/`、`sessions/`、`state.db*`、`lock`）；`wiki/`、`raw/`、`source/` 与 `.wiki/versions/` 都进 git。CI 在 Ubuntu / macOS / Windows 三平台跑 `build`、`test`、`test --release`、`fmt --check`、`clippy -D warnings`，不安装 poppler/pandoc（PDF 测试用模板现造）。

### Agent skills 安装

6 个 skill 是给 Claude Code 用的薄指令层，所有簿记与判定逻辑都在 `llm-wiki` 里。一次装好：

```bash
./scripts/install.sh            # mac / linux
.\scripts\install.ps1           # windows
```

脚本做两件事：`cargo build --release --locked` 后把二进制装到 `~/.local/bin/llm-wiki`（Windows 为 `%LOCALAPPDATA%\Programs\llm-wiki`），再把 `skills/*` 复制到 `~/.claude/skills/`。安装目录可用 `CLAUDE_SKILLS_DIR` / `WIKI_BIN_DIR` 覆盖，`--dry-run` 只打印动作不落盘，`--skip-build` 复用已有的 `target/release/llm-wiki`。

| skill | 管什么 |
|---|---|
| `llm-wiki` | 总纲：判断该调谁、把 `--json` 翻成人话、决策门四选项 |
| `wiki-scan` | `scan` / `convert`，解释七种变更 |
| `wiki-parse` | 单文件解析诊断、后端选择、外部服务配置 |
| `wiki-ingest` | `ingest begin/commit/abort`，唯一要 agent 写正文的环节 |
| `wiki-audit` | `lint` 与判断层分析（矛盾、过时、孤儿、重复） |
| `wiki-sync` | 定时同步、读变更报告、失败退避 |

装完在目标仓库根目录跑 `llm-wiki doctor` 自检。

## 服务操作说明

第一次使用先初始化；其余命令在未初始化的仓库里会直接以 `E_NOT_INITIALISED`
（exit 2）拒绝，并告诉你缺的是 `wiki.toml` 还是 `wiki/`：

```bash
llm-wiki init          # 写 wiki.toml / SCHEMA.md，建目录；可重复跑，已有文件不覆盖
llm-wiki doctor        # 在 init 前后都能跑，用来诊断
```

日常一轮，按顺序：

```bash
llm-wiki scan                              # 报告 source/ 的变更
llm-wiki convert --limit 5                 # 最多转换 5 个待转换源
llm-wiki convert --key notes/a.md          # 指定源，已转换也重转
llm-wiki parse --key docs/a.pdf            # 只看解析结果，不写任何文件

llm-wiki ingest begin --sources notes/a.md --pages concept/alpha
# ……agent 把候选页写进 .wiki/staging/concept/alpha.md……
llm-wiki ingest commit --pages concept/alpha
llm-wiki lint --fix                        # 先修机械缺陷，再报告剩下的
llm-wiki approve --confirm                 # 合并候选进 wiki/
llm-wiki checkpoint --label "after alpha"

llm-wiki graph                            # 链接图：枢纽页、孤立页、断链
llm-wiki query 熔断                        # FTS5 检索，命中数 0 也是 exit 0
llm-wiki query --reindex                   # 全量重建索引后再搜
llm-wiki sync --dry-run                    # 演练一轮：只报告，不转换、不写审计
llm-wiki sync --limit 3 --force            # 忽略稳定门槛，真跑一轮

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
| `scan [--apply-moves]` | 报告 `source/`、`raw/` 与账本的差异；`--apply-moves` 一并迁移改名 |
| `convert [--key <KEY>] [--limit <N>]` | 转换待处理源（默认 10）；单个失败不中断 |
| `parse --key <KEY> [--chunk <N>] [--range <A-B>] [--force-large]` | 只显示解析结果，不写文件；大文件按需分块；`--force-large` 越过尺寸上限只为看一眼 |
| `ingest begin [--sources <K>]... [--pages <P>]... [--dry-run]` | 开一轮，报告受影响的页面 |
| `ingest commit [--pages <P>]...` | 关闭本轮，刷新候选页与暂存清单 |
| `ingest abort` | 丢弃本轮，保留已写候选页 |
| `lint [--strict] [--fix]` | 十三条规则；`--strict` 让警告也失败 |
| `checkpoint --label <LABEL>` | 把 `wiki/` 拍成新版本快照 |
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
| `sync [--limit <N>] [--dry-run] [--force]` | 无人值守一轮：scan → convert → 门槛 → 审计 |

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
| `3` | 需要人拍板：`approve` 未带 `--confirm`、`archive purge` 未带 `--yes` |
| `4` | 引擎被另一个进程锁住（`lock` 真正被占用，不是过期残留） |

## 服务特殊说明

- 配置只有仓库根目录的 `wiki.toml`（另有人类可读的 `SCHEMA.md`）。`[parse]` 与 `[parse.external]` 的每个键、默认值与校验规则见 [parse 配置](docs/readme/config/parse.md)；`[paths]`、`[formats]`、`[schema]`、`[ingest]`、`[audit]`、`[versions]`、`[sync]`、`[memory]` 的行为分别由对应模块读取。
- 解析外部服务的 token **只从环境变量读**：`[parse.external].auth_env` 存的是变量名，不是 token 字面量。把 `Bearer xxx`、`sk-...` 这类值写进 `auth_env` 会被 `Config::validate` 以 `E_TOKEN_IN_CONFIG` 拒绝，`doctor` 的 `token` 检查也会报 Fail。
- `parse.primary` 取 `external` 却没写 `[parse.external]` 时，`doctor` 的 `external_backend` 直接报 Fail（每个解析都会以 `E_NO_EXTERNAL` 失败）；服务不可达只报 Warn，因为服务可能在别处恢复。
- 内置解析后端的 id 会写进 `raw/` frontmatter：`builtin-text`、`builtin-html`、`builtin-asset`、`builtin-pdf`、`external-http`。升级解析器后可以据此筛出需要重转的源。
- 常见错误码：`E_NO_PARSER`（无后端认领扩展名）、`E_BAD_SOURCE`（后端认领但解不开）、`E_NO_AUTH_ENV`、`E_EXTERNAL`、`E_BOTH_FAILED`（`auto` 下内置与外部都失败）、`E_BAD_CONFIG`（外部后端配置残缺）、`E_OVERSIZE`（超过 `max_source_bytes`）、`E_STAGING_CORRUPT` / `E_STAGING_DRIFT`（暂存区与清单不一致）、`E_NO_SUMMARY` / `E_ALREADY_SEALED`（封存前置条件不满足）、`E_LOCKED`（仓库被占用）。
- 版本快照在 `.wiki/versions/vNNN/`，只保留最近 `[versions].keep` 份（默认 5）；`log.md` 记录每次 `approve` / `checkpoint` / `seal`。
- 仓库锁 `.wiki/lock` 的 TTL 是 600 秒；崩溃残留会被下一次运行回收，只有真正被别的进程持有时才以 exit 4 退出。
- 索引是派生状态：`query` 前会增量刷新，刚 `approve` 的页也能立刻搜到；怀疑索引不一致时用 `query --reindex` 全量重建。
- `sync` 的 `--dry-run` 保证不写盘（`settle_seconds` 与 `stable_streak` 门槛在演练里同样生效），`--force` 才跳过早写门槛。
