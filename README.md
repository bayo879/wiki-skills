# llm-wiki

维护一份 Karpathy 式 LLM wiki 的命令行工具：源文件进 `source/`，解析成 `raw/` 里的 markdown，由人和 agent 一起写成 `wiki/` 页面，再用 lint、版本快照与 ingest 轮次保证它不腐烂。

## 服务说明

一条源文件的完整生命周期：

1. **扫描**：`scan` 对比 `source/` 与 `.wiki/state.db` 账本，得出新增、修改、删除。
2. **解析**：`convert` 把源文件交给解析后端，产出 `raw/<key>.md`，frontmatter 记录来源、哈希与所用后端。
3. **成页**：agent 读 `raw/`，在 `wiki/<type>/<id>.md` 写页面；`ingest begin/commit/abort` 把一轮读写的候选页、受影响页与暂存区管理起来。
4. **校验**：`lint` 跑十二条规则，`--fix` 修掉不需要判断的机械缺陷。
5. **留痕**：`checkpoint` 给当前 wiki 拍版本快照；`approve`/`reject` 处理暂存候选；`sync` 把「扫描→转换→门槛→审计」串成无人值守的一轮。
6. **续命**：会话摘要可 `archive seal` 封存、`recall` 检索、`archive purge` 按容量与时限清理。

解析层是可替换的后端集合：文本、HTML、图片、PDF 各有一个内置后端，另有一个可选的 `external-http` 后端把扫描件之类「内置读不出字」的文档交给外部 HTTP 服务。后端选择与回退规则见 [解析后端与 auto 回退](docs/readme/parse/external.md)。

## 服务部署

离线构建与测试（依赖已在本仓库 `.cargo-home` 中）：

```bash
CARGO_HOME=$PWD/.cargo-home CARGO_NET_OFFLINE=true cargo build
CARGO_HOME=$PWD/.cargo-home CARGO_NET_OFFLINE=true cargo test
CARGO_HOME=$PWD/.cargo-home CARGO_NET_OFFLINE=true cargo fmt --check
```

二进制在 `target/debug/llm-wiki`。在任意仓库根目录执行：

```bash
llm-wiki init            # 写 wiki.toml、SCHEMA.md，建 source/ raw/ wiki/ .wiki/
llm-wiki init --force    # 覆盖重写这两份契约文件（不动 source/ 与 wiki/ 页面）
```

`init` 生成 `.wiki/.gitignore`，只忽略可重建的状态（`cache/`、`staging/`、`sessions/`、`state.db*`、`lock`）；`wiki/`、`raw/`、`source/` 都进 git。

## 服务操作说明

```bash
llm-wiki scan                                  # 报告 source/ 的变更
llm-wiki convert --limit 5                     # 最多转换 5 个待转换源
llm-wiki convert --key notes/a.md              # 指定源，已转换也重转
llm-wiki parse --key docs/a.pdf                # 只看解析结果，不写任何文件

llm-wiki ingest begin --sources notes/a.md --pages concept/alpha
llm-wiki ingest commit --pages concept/alpha
llm-wiki ingest abort

llm-wiki lint                                  # 只报告
llm-wiki lint --fix                            # 顺手修机械缺陷
llm-wiki checkpoint --label "before refactor"
llm-wiki approve --confirm
llm-wiki reject --reason "证据不足"

llm-wiki sync --dry-run                        # 演练一轮：只报告，不转换、不写 audit
llm-wiki sync --limit 3 --force

llm-wiki archive status
llm-wiki archive seal --sources notes/a.md --title "2026-10 摘要"
llm-wiki archive purge --dry-run
llm-wiki recall "重试策略" --full
```

机器可读输出：任意命令加全局 `--json`，stdout 只有一行 JSON。

```json
{"ok":true,"cmd":"convert","data":{...}}
{"ok":false,"cmd":"parse","error":{"code":"E_NO_AUTH_ENV","message":"external parser token variable WIKI_PARSE_TOKEN is not set"}}
```

退出码：`0` 成功；`1` 命令执行失败（含部分源失败）；`2` 用法/配置/环境错误；`3` 需要人拍板（如 `approve` 未带 `--confirm`）；`4` 引擎被另一个进程锁住。人类可读的诊断写 stderr，JSON 只写 stdout。

## 服务特殊说明

- 配置只有仓库根目录的 `wiki.toml`（另有人类可读的 `SCHEMA.md`）。`[parse]` 与 `[parse.external]` 的每个键、默认值与校验规则见 [parse 配置](docs/readme/config/parse.md)。
- 解析外部服务的 token **只从环境变量读**：`[parse.external].auth_env` 存的是变量名，不是 token 字面量。把 `Bearer xxx`、`sk-...` 这类值写进 `auth_env` 会被 `Config::validate` 以 `E_TOKEN_IN_CONFIG` 拒绝。
- 内置解析后端的 id 会写进 `raw/` frontmatter：`builtin-text`、`builtin-html`、`builtin-asset`、`builtin-pdf`、`external-http`。升级解析器后可以据此筛出需要重转的源。
- 解析失败的错误码：`E_NO_PARSER`（无后端认领扩展名）、`E_BAD_SOURCE`（后端认领但解不开）、`E_NO_AUTH_ENV`、`E_EXTERNAL`、`E_BOTH_FAILED`（`auto` 下内置与外部都失败）、`E_BAD_CONFIG`（外部后端配置残缺）。
