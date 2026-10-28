# 排障（troubleshooting）

> 何时读：任何命令意外失败、退出码看不懂、`doctor` 有 fail、或怀疑仓库还是旧布局时。

先记住三件事：

1. **先 `llm-wiki doctor --json`**，`data.checks[]` 每条是 `{name, level, detail}`；`level: fail` 的那条通常直接说明原因。
2. **不要手改 `.wiki/state.db`、`.wiki/staging/` 或 `index.md`/`log.md`。** 手工修账本会让下次运行报更奇怪的错。
3. **`.wiki/` 之外的文件是源，永远只读。** 任何「把源删了再重建」的修法都是错的。

## 退出码对照

| 码 | 含义 | 处理 |
|---|---|---|
| 0 | 成功 | — |
| 1 | 跑完了，但内容有问题：`lint` 有 error、部分源转换失败、`recall` 无命中 | 逐条报失败项；源的问题回 `references/scan.md`，页面问题回 `references/audit.md` |
| 2 | 没跑起来：用法、配置或环境错误 | 看 `error.code`，见下表；多半是没在仓库根，或 `.wiki/wiki.toml` 坏了 |
| 3 | 需要人拍板 | 回 `SKILL.md` 的决策门，给四选项，停下 |
| 4 | 锁被另一个进程占用 | 等它结束再重试；**不要删 `.wiki/lock`** |

## 常见错误码

| 码 | 典型原因 | 怎么做 |
|---|---|---|
| `E_NOT_INITIALISED` | 仓库里没有 `.wiki/wiki.toml`（新 clone、或整个引擎被删） | `llm-wiki init`。不要手工造目录。`detail.missing` 会说是缺文件还是缺目录 |
| `E_NO_CONFIG` | 同上，配置文件缺失 | `llm-wiki init` |
| `E_BAD_CONFIG` | `.wiki/wiki.toml` 解析失败或字段非法 | 按 `detail` 指的位置改；改不动就 `llm-wiki init --force` 重写契约（不动源与页面） |
| `E_LOCKED` | 另一个进程持锁 | 等；定时任务撞车算正常，退避到下一轮 |
| `E_NO_SOURCE` | `--key` 指的文件不在仓库根 | 用 `llm-wiki scan --json` 抄准确的 key；key 是仓库相对路径 |
| `E_OVERSIZE` | 源超过 `parse.max_source_bytes`（默认 100MB） | 换小一点的源，或让用户调配置；只为看一眼可用 `parse --force-large`（不落盘） |
| `E_NO_PARSER` | 该扩展名没有可用后端 | 看 `formats.allow`；装对应后端或换格式 |
| `E_BAD_FRONTMATTER` | raw / 候选页的 frontmatter 不合法 | 只改 `.wiki/staging/` 下的候选页；`.wiki/raw/` 的坏文件重跑 `convert` |
| `E_NOT_LEDGERED` | 拿一个账本不认识的 key 去 `ingest begin` | 先 `scan` + `convert` 把它登记进账本 |
| `E_STAGING_STALE` | 批准时候选页与基线漂了（源在中间变过） | `llm-wiki reject` 撤回本轮，重新 `ingest begin` |
| `E_STAGING_DRIFT` / `E_STAGING_CORRUPT` | 暂存区被手改，或清单与文件对不上 | 不要手修，`reject` 回滚后重来 |
| `E_NOTHING_TO_APPROVE` | 暂存区是空的 | 先 `ingest begin` 并写候选页 |
| `E_NEEDS_CONFIRMATION` | `approve` / `purge` 没带确认参数 | 这是设计如此：交给人拍板 |
| `E_IO` | 读写失败 | 看路径权限；`parse` 的 `E_IO` 常是 key 写错了 |

## `doctor` 检查项

| 检查 | fail 时怎么办 |
|---|---|
| `config` / `schema` | 契约文件缺失或坏了：`llm-wiki init`（只补不改），必要时 `--force` |
| `paths` | 目录不在 `.wiki/` 下：说明被手工挪过；重跑 `init` 补齐，别把页面搬回去 |
| `db` | 账本坏了：先备份 `.wiki/state.db` 再重建；能接受丢失就去掉它重跑 `init` |
| `lock` | 有进程在跑：等它结束 |
| `staging` | 暂存区与清单不一致：用 `reject` 清掉队列再看 |
| `legacy_layout`（warn） | 仓库根还有旧的 `wiki.toml` 或 `source/` 目录。旧布局不再支持：把 `source/` 里的文件当普通源留在原处即可，然后删掉根 `wiki.toml`，重跑 `init` |
| `engine_ignore`（warn） | `.wiki/.gitignore` 不是 `*`：整个引擎应该不进 git，补成一行 `*` |

只 warn 的项可以带着往下走，但要让用户知道；下次体检还会出现。

## 常见症状

- **「新加的文件扫不到」**：确认文件不在点目录（`.git`、`.venv`…）、不在 `[scan].ignore` 的目录里、扩展名在 `formats.allow` 里、大小没超限。
- **「页面搜不到」**：新页需要 `approve` 之后 `query` 才会重建索引；直接 `llm-wiki query` 会先补索引。
- **「`convert` 说不是待转换项」**：账本已登记同一 key。`scan` 看 `layer: "raw"` 的 `kind`，`changed` 才会重转。
- **「索引/日志不对」**：不要手写，让 CLI 重建（`approve`、`checkpoint`、`sync` 都会重建）。
- **「两个进程打架」**：退出码 4，等。定时任务与人工操作共用一个仓库时，把定时任务错开。
