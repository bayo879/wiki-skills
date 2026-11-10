# llm-wiki

维护一份 Karpathy 式 LLM wiki 的命令行工具：仓库里的任意文件都是源，解析成 `.wiki/raw/` 里的 markdown，由人和 agent 一起写成 `.wiki/pages/` 页面，直接写正式页，并以来源待办支持中断后继续。工具只往 `.wiki/` 里写，源文件永远只读。

## 服务说明

没有服务端，只有一个隐藏目录 `.wiki/`。引擎之外的一切都是只读的源：

| 层 | 位置 | 说明 |
|---|---|---|
| 源 | 仓库根（`.wiki/` 之外） | 唯一输入，任意格式，工具只读；`[scan].ignore` 可排除目录 |
| 契约 | `.wiki/wiki.toml`、`.wiki/SCHEMA.md` | 配置与页面类型枚举，人和 agent 都读 |
| 中间层 | `.wiki/raw/` | 解析产物，可重建；frontmatter 记录来源、哈希、解析后端 |
| 知识层 | `.wiki/pages/` | 页面、`index.md`、`audit/` |
| 引擎状态 | `.wiki/` 其余 | SQLite 账本、来源待办、缓存、会话、锁与工作租约；旧库遗留数据不自动删除 |

写入边界是结构性的：契约、页面、账本全在 `.wiki/` 里，手写 `[paths]` 也移不出去。

对外只有五条命令（外加 `--version`）：`doctor`、`ingest`、`done`、`query`、`ignore`。工具转换资料并维护来源待办；作者直接更新正式页，不再有候选发布或工具内版本管理。

一条源文件的生命周期：

1. **看**：`doctor` 体检，`doctor --fix` 修机械缺陷；`ingest --dry-run` 演练转换计划，不写盘。
2. **摄入**：`ingest` 扫描、转换并登记 `.wiki/raw/`，持久化未整理来源，输出正式页目录、页面类型、schema、待办 raw 哈希和受影响页面。刚改过的源等 `[ingest].settle_seconds`（默认 300 秒），`--force` 跳过等待并全文复核。
3. **直接写页**：作者实际读取 raw 后保存对应哈希，重读受影响页，再直接更新 `.wiki/pages/`。按 schema 显式填写真实使用的 `sources` 与 `updated`，工具不自动补来源。页面落盘立即生效。
4. **按来源确认**：核对页面并检索复验后，用 `done <KEY> --hash <RAW_HASH>` 确认实际读过的 raw。成功只清该来源待办，重复确认成功无动作；资料漂移以 `E_SOURCE_DRIFT` 退出 1，保留待办。重读并核对已写结论后再确认，不得用新哈希为旧阅读背书。
5. **继续**：中断后重跑 `ingest` 会重新交付未完成来源；可以合并已有页或判断无需新建页。一次只开放一批：交付的待办记在 `.wiki/work.lock`，这批没做完之前 `ingest` 不接新来源，`done` 也只接受这批里的来源（批外的确认以 `E_WORK_LEASE` 退出 4）。`query` 默认含会话蒸馏，`--no-archive` 排除它们。
6. **删源**：源文件被删后不用另外下命令，下一轮 `ingest` 按 `[ingest].on_missing` 自动收口（默认 `forget`）：页面对它的引用去掉、转 `stale`，一条来源不剩的页整页删除，raw 文件与账本行（含待办）一并清掉，轮次警告以 `vanished:` 开头。收口是自动的、可重试的，失败只报 finding 并把账本行留到下一轮；删除本身不可撤销，只能靠你自己的备份。

**写页没有发布门。** `done` 不发布、不验证正文，也不回滚已写页面；工具不自动备份，没有内置历史恢复。CLI 锁只约束工具命令，不能阻止多个作者直接覆盖同一页。需要恢复必须有独立备份，写同一页应指定单一作者并写前重读。

解析层是可替换的后端集合：文本、HTML、图片、PDF 各有一个内置后端，另有一个可选的 `external-http` 后端把扫描件之类「内置读不出字」的文档交给外部 HTTP 服务。分层、数据流、模块表与不变量见 [架构与模块](docs/readme/architecture.md)，后端选择与回退规则见 [解析后端与 auto 回退](docs/readme/parse/external.md)。未完成的设计与待判别问题按日期一问题一文件归档在 [docs/issues/](docs/issues/)，目录只放还没结论的事。

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
llm-wiki doctor            # 十一项健康检查，有 Fail 则退出码 1
llm-wiki doctor --json     # 机器可读：{"ok":true,"cmd":"doctor","data":{"checks":[...]}}
```

`doctor --fix` 也覆盖已经初始化的仓库：缺的目录补建、契约文件不存在就写、索引坏了重建；它从不删文件、不改源。

`.wiki/` 里是本地引擎（契约、页面、账本），工具不生成也不修改任何 git 配置。用 git 的仓库请把 `.wiki/` 加进仓库根的 `.gitignore`；没有 git 的环境不需要这一步。CI 在 Ubuntu / macOS / Windows 三平台跑 `build`、`test`、`test --release`、`fmt --check`、`clippy -D warnings`，不安装 poppler/pandoc（PDF 测试用模板现造）。

### Agent skills 安装

两个 skill 是给 Claude Code 用的薄指令层，所有簿记与判定逻辑都在 `llm-wiki` 里。

`skills/llm-wiki/` 维护 wiki 本身：`SKILL.md` 是入口（边界、最小序列、门禁、退出码）；`content.md` 写「写什么」（建页判断、新源、切片、矛盾、去重）；`ops.md` 写「出了问题怎么看」（体检、判断层、冲突与覆盖风险、忽略、叫人）。
`skills/llm-wiki-session/` 写会话蒸馏：从一段对话里抽出值得留给项目的判断，产物写进 `[ingest].session_dir`（默认 `sessions`），原始会话不存。一个会话一个文件，不按时间切。

字段名、正式页位置、schema、待办与报错含义由 CLI 现打，技能不复述。skill 只有 markdown，复制即可；CLI 是另一个可执行文件，按下面的方式放到能调用的地方。

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
| `skills/llm-wiki/SKILL.md` | 入口：边界、最小序列、来源确认、退出码 |
| `skills/llm-wiki/content.md` | 写什么：一页一事、新源怎么处理、切片、前后矛盾、重复 |
| `skills/llm-wiki/ops.md` | 坏了怎么办：体检、判断层、冲突与覆盖风险、忽略、叫人 |
| `skills/llm-wiki-session/SKILL.md` | 会话蒸馏：抽什么、写到哪、怎么去重 |

装完在目标仓库根目录跑 `llm-wiki doctor` 自检。

## 服务操作说明

第一次使用跑 `doctor --fix`；其余命令在未初始化的仓库里会直接以 `E_NOT_INITIALISED`
（exit 2）拒绝，并告诉你缺的是 `.wiki/wiki.toml` 还是 `.wiki/pages/`。

日常整理：

```bash
llm-wiki doctor
llm-wiki ingest --dry-run
llm-wiki ingest                            # 转换并交付未完成来源
# 读取 raw，保存所读哈希；按 schema 直接更新 .wiki/pages/ 正式页
llm-wiki query 熔断                        # 写页后即可检索，不必先确认来源
llm-wiki done notes/a.md --hash <RAW_HASH> # 只确认这一来源，必须用实际读取的 raw 哈希
llm-wiki query --graph
```

点名摄入或只看解析：

```bash
llm-wiki ingest notes/a.md docs/b.pdf
llm-wiki ingest --key docs/a.pdf
llm-wiki ingest --key docs/a.pdf --chunk 2000 --range 0-4000
```

无人值守可定时执行 `ingest`，但转换不等于已整理；作者仍须读 raw、写页并逐源确认。安排定时任务先征得用户同意，不把未读来源批量标成完成。

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
| `doctor [--fix] [--only scan\|convert\|session] [--key <KEY>]` | 体检；补建目录、契约与索引 |
| `ingest [<KEY>...] [--dry-run] [--key <KEY>] [--chunk <N>] [--range <A-B>] [--force-large] [--force]` | 扫描、转换、登记及交付来源待办；`--key` 只预览，`--force` 强制复核并跳过等待；保留的 `--force-large` 不绕体积上限 |
| `done <KEY> --hash <RAW_HASH>` | 持锁核对 raw 与源；成功只清这一来源待办，重复确认 exit 0，漂移 exit 1 |
| `query [TEXT] [--no-archive] [--graph] [--limit <N>]` | FTS5 检索与链接图，无命中 exit 0 |
| `ignore [--add <PATTERN>] [--remove <PATTERN>] [--check <KEY>]` | 查看、维护和检查忽略列表 |
| `--version` | 打印工具软件版本，不是内容版本管理 |

机器可读输出：任意命令加全局 `--json`，stdout 只有一行 JSON。

```json
{"ok":true,"cmd":"ingest","data":{...}}
{"ok":false,"cmd":"query","error":{"code":"E_NO_WIKI","message":"..."}}
```

退出码：

| 码 | 含义 |
|---|---|
| `0` | 成功；含无检索命中、只有 Warn、来源幂等确认 |
| `1` | 命令执行失败（含部分源失败、来源漂移、源文件读不到），或 `doctor` 有 Fail |
| `2` | 用法、配置或环境错误（含未初始化、预览超过 `max_source_bytes`、锁文件不可访问） |
| `4` | 另一个进程正持有引擎锁（按 OS 锁判断，不看文件年龄） |

## 服务特殊说明

- 配置只有 `.wiki/wiki.toml`，页面契约在 `.wiki/SCHEMA.md`。新配置不再生成版本保留与发布页数预算；旧 `[versions].keep`、`[ingest].max_pages_per_slice`、`[ingest].max_new_pages_per_run` 只为兼容读取，在当前 CLI 不生效。未知键仍报错。`[paths]` 固定在 `.wiki/` 内，手写也不能迁出。
- 哈希口径统一为全文：先以大小和修改时间初筛，元数据未变时可能复用已记录摘要，因此可能漏掉保留元数据的改动。`--force` 强制全文复核；它与 `--force-large` 都不能越过 `parse.max_source_bytes`（默认 100MB）；超限源不转换、不进待办，每轮 `ingest` 以 warning 点名，需改上限或加忽略。读取受限且对同一份固定字节哈希、解析；稳定窗口仍保留。详见 [parse 配置](docs/readme/config/parse.md)。
- `done` 校验实际读过的 raw 哈希及当前源，不根据页面内容判断完成；成功不自动补引用。正文正确性、引用真实性、覆盖风险由作者核对。
- 源文件消失由 `[ingest].on_missing` 一次决定：`forget`（默认）清引用与 raw、删空壳页、删账本行；`mark_stale` 只把页转 `stale`；`ignore` 全部留给作者。这也适用于被 `.wiki/ignore` 挡住的源——忽略只挡新摄入，不挡收口。
- 页面质量有两道自动闸门：`lint` 的 `L12`（页面不引用任何来源）与 `L13`（正文留着 `TODO`/`FIXME`/`待补`/`占位`），都是 warning；`ingest` 对受影响的页复跑这两条，直接写进轮次 warning，不等 `doctor`。
- 会话蒸馏源目录由 `[ingest].session_dir` 指定（默认 `sessions`，空串关闭）；`query --no-archive` 按页面目录前缀排除。
- 外部解析凭据只从环境变量读取，`auth_env` 存变量名不存 token；内置与外部后端规则见 [解析后端](docs/readme/parse/external.md)。
- 旧库的暂存、版本、对象与日志不自动清理或重新发布。可解析的旧待办只导入来源线索；异常项给出警告，保留原始数据供人工处理。
- `.wiki/ignore` 使用 gitignore 语法，点名 `ingest <KEY>` 可绕忽略查看，但不绕稳定窗口和体积上限。
- `.wiki/` 是单机事实源，不提供同步，不应搬移合并账本。工具不生成 git 配置，备份由用户另行安排；没有自动备份与工具内回退。
- 工作租约是「一次只开放一批」的落地：`ingest` 交付待办时写 `.wiki/work.lock`，`done` 逐条关闭，最后一条清掉后文件消失。租约未过期时不接新来源、不收口消失源、不跟随改名；批内来源可以重新摄入。过期按最后一次确认算：每结清一条 `done` 就把计时推到当下，所以边做边确认的长批次不会被腰斩，停手两小时后才过期。过期后 `ingest` 会重新接管，`doctor --fix` 直接删掉过期文件。代价说清：忘了 `done` 会让摄入停在原地，直到过期或手动清。
- 仓库锁是一个固定 inode 上的 OS 文件锁：按占用判断，不按文件年龄判断；进程退出即释放，文件本身保留复用。直接写页不持这个锁，并发作者仍可能覆盖。数据库连接显式设 5s busy_timeout（不依赖驱动的默认值），并发短写排队而不是立即 `SQLITE_BUSY`。
- 索引是派生状态，`query` 前增量刷新，直接写下的新页和修改可被检索；`doctor --fix` 可重建索引。
