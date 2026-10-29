# llm-wiki

维护一份 Karpathy 式 LLM wiki 的命令行工具：仓库里的任意文件都是源，解析成 `.wiki/raw/` 里的 markdown，由人和 agent 一起写成 `.wiki/pages/` 页面，再用 lint、版本快照与 ingest 轮次保证它不腐烂。工具只往 `.wiki/` 里写，源文件永远只读。

## 服务说明

没有服务端，只有一个隐藏目录 `.wiki/`。引擎之外的一切都是只读的源：

| 层 | 位置 | 说明 |
|---|---|---|
| 源 | 仓库根（`.wiki/` 之外） | 唯一输入，任意格式，工具只读；`[scan].ignore` 可排除目录 |
| 契约 | `.wiki/wiki.toml`、`.wiki/SCHEMA.md` | 配置与页面类型枚举，人和 agent 都读 |
| 中间层 | `.wiki/raw/` | 解析产物，可重建；frontmatter 记录来源、哈希、解析后端 |
| 知识层 | `.wiki/pages/` | 页面、`index.md`、`audit/`、`archive/` |
| 引擎状态 | `.wiki/` 其余 | SQLite 账本、暂存区、版本 manifest 与对象库、决策日志 `journal.jsonl`、会话、锁 |

写入边界是结构性的：契约、页面、账本全在 `.wiki/` 里，手写 `[paths]` 也移不出去。

一条源文件的完整生命周期：

1. **扫描**：`scan` 对比仓库根与账本 `state.db`，得出新增 / 修改 / 删除 / 改名。
2. **解析**：`convert` 把源交给解析后端，写 `.wiki/raw/<key>.md`，frontmatter 记录来源、`source_hash`、`parser` 与 `pin`。
3. **成页**：agent 读 `.wiki/raw/`，把候选页写进 `.wiki/staging/<type>/<id>.md`；`ingest begin / submit / abort` 管住一轮的读写——`submit` 只是交稿，发布永远是 `approve`。
4. **发布**：`approve` 直接把候选合并进 `.wiki/pages/` 并重建 `index.md`——不等人，三道刹车只扣住命中的候选，其余的照常发布；`--confirm` 是显式越权。`reject` 丢弃候选并把这一轮的源按当前字节记为已复核（否决即是对源的表决）。两种决策都只追加一行 `.wiki/journal.jsonl`。
5. **校验**：`lint` 跑十二条规则，`--fix` 修掉不需要判断的机械缺陷。
6. **留痕**：`checkpoint` 记一份 manifest（`pages/` 与 `raw/` 每条的 blake3），字节按摘要存进 `.wiki/objects/`；`sync` 把「扫描 → 转换 → 稳定门槛 → 审计」串成无人值守的一轮。`history` / `diff` 读同一份 manifest，`rollback <vNNN>` 整版还原并把自己记成新版本；`.wiki/ignore` 挡住反复制造变动的源，超出刹车额度的候选留在 `.wiki/staging/` 等下一轮。
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
llm-wiki doctor            # 十项健康检查，有 Fail 则退出码 1
llm-wiki doctor --json     # 机器可读：{"ok":true,"cmd":"doctor","data":{"checks":[...]}}
```

`.wiki/` 里是本地引擎（契约、页面、账本、版本），工具不生成也不修改任何 git 配置。用 git 的仓库请把 `.wiki/` 加进仓库根的 `.gitignore`；没有 git 的环境不需要这一步。CI 在 Ubuntu / macOS / Windows 三平台跑 `build`、`test`、`test --release`、`fmt --check`、`clippy -D warnings`，不安装 poppler/pandoc（PDF 测试用模板现造）。

### Agent skills 安装

1 个 skill 是给 Claude Code 用的薄指令层，所有簿记与判定逻辑都在 `llm-wiki` 里。
`skills/llm-wiki/SKILL.md` 是入口（边界、路由、最小序列、门禁、退出码）；
`content.md` 写「写什么」（建页判断、新源、切片、矛盾、去重），`ops.md` 写「坏了怎么看」
（体检、判断层、同步、冲突与回退、忽略、叫人）。三份都短，agent 按需只读一份。
字段名、候选路径、缺失字段、报错码含义由 CLI 现打，技能不复述。
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
| `skills/llm-wiki/SKILL.md` | 入口：边界、路由、最小序列、门禁、`approve` 选项、退出码 |
| `skills/llm-wiki/content.md` | 写什么：一页一事、新源怎么处理、切片、前后矛盾、重复 |
| `skills/llm-wiki/ops.md` | 坏了怎么办：体检、判断层、同步、冲突与回退、忽略、叫人 |

装完在目标仓库根目录跑 `llm-wiki doctor` 自检。

## 服务操作说明

第一次使用先初始化；其余命令在未初始化的仓库里会直接以 `E_NOT_INITIALISED`
（exit 2）拒绝，并告诉你缺的是 `.wiki/wiki.toml` 还是 `.wiki/pages/`：

```bash
llm-wiki init          # 写 .wiki/wiki.toml / .wiki/SCHEMA.md，建目录；可重复跑，已有文件不覆盖
llm-wiki doctor        # 在 init 前后都能跑，用来诊断
```

`init` 的输出末尾带一段固定提示词（`--json` 下是 `data.next`），是给 agent 看的：
把本仓库路径注入当前项目的 User 设定（之后优先用 `llm-wiki` 技能检索），再把
`llm-wiki scan` 排成定时任务。周期先问用户，默认每天 01:00 跑一次。
两件事都在引擎之外，CLI 只提示、不代劳。

日常一轮，按顺序：

```bash
llm-wiki scan                              # 报告仓库根的变更
llm-wiki convert --limit 5                 # 最多转换 5 个待转换源
llm-wiki convert --key notes/a.md          # 指定源，已转换也重转
llm-wiki parse --key docs/a.pdf            # 只看解析结果，不写任何文件

llm-wiki ingest begin --sources notes/a.md --pages concept/alpha
# ……agent 把候选页写进 .wiki/staging/concept/alpha.md……
llm-wiki ingest submit --pages concept/alpha
llm-wiki lint --fix                        # 先修机械缺陷，再报告剩下的
llm-wiki approve                           # 刹车放行的候选进 .wiki/pages/
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
| `doctor` | 检查契约、schema、路径、账本、FTS5、旧布局、锁、暂存区、token、外部后端 |
| `init [--force]` | 写契约文件、建引擎目录，并给出初始化后的两步指引 |
| `scan [--apply-moves]` | 报告仓库根、`.wiki/raw/` 与账本的差异；`--apply-moves` 一并迁移改名 |
| `convert [--key <KEY>] [--limit <N>]` | 转换待处理源（默认 10）；单个失败不中断 |
| `parse --key <KEY> [--chunk <N>] [--range <A-B>] [--force-large]` | 只显示解析结果，不写文件；大文件按需分块；`--force-large` 越过尺寸上限只为看一眼 |
| `ingest begin [--sources <K>]... [--pages <P>]... [--dry-run]` | 开一轮，报告受影响的页面 |
| `ingest submit [--pages <P>]...` | 交稿：刷新候选页与暂存清单，**不发布**（发布是 `approve`） |
| `ingest abort` | 丢弃本轮，保留已写候选页 |
| `lint [--strict] [--fix]` | 十二条规则；`--strict` 让警告也失败 |
| `checkpoint --label <LABEL>` | 记 manifest 与内容对象，成为新版本（只保留最近 `[versions].keep` 份） |
| `history` | 列出版本：`version` / `kind` / `label` / `created_at` / `pages` |
| `diff <VERSION>` | 该版本与当前工作树的差异：`added` / `removed` / `changed` |
| `rollback <VERSION>` | 整版还原 `pages/` 与 `raw/`、重建索引，并把自己记成新版本（exit 2 `E_NO_VERSION` 表示没有这个版本） |
| `ignore list` | 列出 `.wiki/ignore` 的规则 |
| `ignore add <PATTERN>...` / `ignore remove <PATTERN>...` | 增删忽略规则（`.gitignore` 语法，`#` 注释、`!` 取反、`/` 锚定、`**` 跨目录） |
| `ignore check <PATH>` | 报 `ignored`、命中的 `rule` 与归一化后的 `path` |
| `approve [--confirm]` | 发布候选：三道刹车只扣住命中的，其余照发；`--confirm` 越过全部刹车；全部被扣下才 exit 3 |
| `reject --reason <REASON>` | 丢弃候选、记录原因，并把本轮的源按当前字节记为已复核（下一轮不再报同一变动） |
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
| `3` | 需要人看一眼：`approve` 这一轮**每个**候选都被刹车扣下（`page_overlap` / `page_slice_budget` / `page_budget`），一页也没发出去；`archive purge` 未带 `--yes` |
| `4` | 引擎被另一个进程锁住（`lock` 真正被占用，不是过期残留） |

## 服务特殊说明

- 配置只有 `.wiki/wiki.toml`（另有人类可读的 `.wiki/SCHEMA.md`）。`[parse]` 与 `[parse.external]` 的每个键、默认值与校验规则见 [parse 配置](docs/readme/config/parse.md)；`[scan]`、`[formats]`、`[schema]`、`[ingest]`、`[audit]`、`[versions]`、`[sync]`、`[memory]` 的行为分别由对应模块读取。`[paths]` 已固定为 `.wiki/` 内，写进配置也会被忽略。
- 解析外部服务的 token **只从环境变量读**：`[parse.external].auth_env` 存的是变量名，不是 token 字面量。把 `Bearer xxx`、`sk-...` 这类值写进 `auth_env` 会被 `Config::validate` 以 `E_TOKEN_IN_CONFIG` 拒绝，`doctor` 的 `token` 检查也会报 Fail。
- `parse.primary` 取 `external` 却没写 `[parse.external]` 时，`doctor` 的 `external_backend` 直接报 Fail（每个解析都会以 `E_NO_EXTERNAL` 失败）；服务不可达只报 Warn，因为服务可能在别处恢复。
- 内置解析后端的 id 会写进 `.wiki/raw/` frontmatter：`builtin-text`、`builtin-html`、`builtin-asset`、`builtin-pdf`、`external-http`。升级解析器后可以据此筛出需要重转的源。
- 常见错误码：`E_NO_PARSER`（无后端认领扩展名）、`E_BAD_SOURCE`（后端认领但解不开）、`E_NO_AUTH_ENV`、`E_EXTERNAL`、`E_BOTH_FAILED`（`auto` 下内置与外部都失败）、`E_BAD_CONFIG`（外部后端配置残缺）、`E_OVERSIZE`（超过 `max_source_bytes`）、`E_STAGING_CORRUPT` / `E_STAGING_DRIFT`（暂存区与清单不一致）、`E_NO_SUMMARY` / `E_ALREADY_SEALED`（封存前置条件不满足）、`E_LOCKED`（仓库被占用）。
- 版本在 `.wiki/versions/vNNN/`：`manifest.json` 记路径与摘要，`.version.json` 记 `kind` / `label` / `pages`，字节按摘要存在 `.wiki/objects/`（同一内容只存一份）。只保留最近 `[versions].keep` 份（默认 5）；`.wiki/journal.jsonl` 记录每次 `approve` / `reject` / `instruct` / `defer` / `seal` / `rollback`（一行一个 JSON，不进版本、不被回退抹掉）。索引不进版本，回退后自动重建。
- 三道刹车都在 `approve` 不带 `--confirm` 时生效，命中的候选留在 `.wiki/staging/`、其余照常发布，所以无人值守链路也能跑完：近义（`page_overlap`）、切片预算（`page_slice_budget`）、页面预算（`page_budget`，默认 20，0 表示不限）。只有全部候选都被扣下时才 exit 3，此时确实需要人看一眼；`--confirm` 是显式越过全部三道。
- 切片预算：`[ingest].max_pages_per_slice`（默认 5，0 表示不限）限制「同一个切片养出多少页」。切片是 `#p=<n>` 指向的那一段（大文件按 `parse.chunk_target` 切块），没带片段的页算整份文件一个切片。所以长文档按信息量拿页数，短文件不能靠拆 id 刷页；小于切块阈值的文件，页面上写的任何 `#p=` 都算整份文件那一个切片，编造不存在的切片换不到额度。超出时这一页被扣下（`reason: page_slice_budget`），已在库里的页继续算数，所以要腾额度得先归档或删旧页。
- 近义页判定：`approve` 会扣下两种「同一页写了两遍」——标题归一化后与已发布页相同（`Retry Policy` = `retry_policy`），或候选页引用的源全部是某张已发布页引用过的（没有新证据）。命中的候选不发布（`reason: page_overlap`），`detail` 点名对端页；判定只比 frontmatter，不读正文，所以不花钱、不误判语义。
- 忽略列表 `.wiki/ignore` 用 `.gitignore` 语法，`scan` / `convert` 跳过、`archive purge` 不清；显式 `convert --key` 仍可读单个文件。`.wiki/` 不随 git 共享，所以忽略列表也是每台机器一份。
- `.wiki/` 是这台机器的事实源，**不随仓库共享，CLI 也不提供同步**。不考虑跨机器：不要手动搬、不要合并账本，两边各写再拼会得到互相矛盾的 `source_hash`。目录内只存仓库相对路径（键由 `paths::identity_key` 产出）。
- 仓库锁 `.wiki/lock` 的 TTL 是 600 秒；崩溃残留会被下一次运行回收，只有真正被别的进程持有时才以 exit 4 退出。
- 索引是派生状态：`query` 前会增量刷新，刚 `approve` 的页也能立刻搜到；怀疑索引不一致时用 `query --reindex` 全量重建。
- `sync` 的 `--dry-run` 保证不写盘（`settle_seconds` 与 `stable_streak` 门槛在演练里同样生效），`--force` 才跳过早写门槛。
