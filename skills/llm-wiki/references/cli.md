# CLI 契约

入口：`scripts/wiki.py`。所有子命令接受 `--root <目录>`（默认当前目录）和 `--json`。
参数放在子命令之后：`wiki.py query 反诈 --root ../kb --json`。

## 输出协议

成功：

```json
{"ok": true, "cmd": "ingest", "version": "0.1.0", "root": "/abs/root",
 "summary": "人类可读一行", "data": {}}
```

失败：

```json
{"ok": false, "cmd": "page put", "version": "0.1.0", "root": "/abs/root",
 "error": {"code": "E_BAD_TYPE", "message": "…", "next": "…"}}
```

- `ok: false` 表示这次调用没有完全达成目标；`error.next` 是下一步建议。
- 退出码：`0` 成功；`1` 部分失败（如个别文件失败、settle 被阻断）；`2` 输入/配置错误；
  `3` 状态不满足（找不到对象、没有来源、未确认删除）；`130` 中断。
- 不要解析人类输出；需要机器判断时始终加 `--json`。

## 命令

### init

创建 `.wiki/{wiki.json,wiki.db,raw/,pages/}`（幂等），返回 `created` 列表。

### ingest [--dry-run] [--force]

扫描根目录（跳过 `scan.ignore`、点文件与隐藏目录），对每个文件：

1. 计算 blake2b 摘要；摘要未变则跳过（`unchanged`）。
2. 解析为文本：PDF → `pdftotext`/`pypdf`；DOCX → 标准库；HTML → 去标签；其余文本格式直读。
3. 写入 `.wiki/raw/<可读名>-<哈希8>.md`，登记来源为 `pending`。

返回：`ingested[]`（key/raw/parser/digest/size/chars）、`unchanged[]`、`unsupported[]`、
`oversize[]`、`failed[]`、`vanished[]`、`restored[]`、`pending[]`。

- `--dry-run` 只报告，不写 raw、不改账本、不动页面；在尚未初始化的目录里也不会创建
  `.wiki/`（已用文件哈希验证）。
- `--force` 忽略摘要，强制重解析。
- 源文件消失：来源标记 `missing`，引用它的页面状态改为 `stale`（正文不动）。
- 源文件恢复且摘要与消失前一致：来源回到 `done`，页面从 `stale` 回到 `stable`，记入
  `restored`。摘要变了则按新来源重新进入 `pending`。
- 源文件内容变化（摘要不同）：来源回到 `pending`，`raw/` 被覆盖，引用它的页面标为
  `stale`。必须重读 raw 并重写页面（`page put` 默认写回 `stable`），否则 `settle` 会阻断。

### read <ref> [--offset N] [--limit N]

`ref` 三种形式：`raw:<key>`、`page:<id>`、相对路径。返回 `content`、`total_lines`、
`next_offset`（读完为 `null`）。offset 从 1 开始；offset 超出总行数时 `content` 为空、
`next_offset` 为 `null`。仓库外的路径会被拒绝（`E_PATH`）。

### query <词...> [--limit N]

在页面标题、summary、正文中做不区分大小写的子串匹配，按命中次数加权排序
（标题 ×5、summary ×3）。返回 `matches`（命中页数）、`results[]`（id/title/type/status/
summary/sources/path/score/snippet）。结果是**排序子集**，不是覆盖性证明。

### query --page <id>

返回单页 frontmatter、正文、`outgoing`/`incoming` 链接（来自 `[[id]]`）。

### page put --id ... --title ... --type ... --summary ... --source ... --body <file|-> [--status stable|draft|stale]

- `--id`：`^[a-z0-9][a-z0-9._-]*$`，全局唯一；同 id 重写会覆盖旧文件（类型目录变化时
  删除旧文件）。
- `--type`：必须在 `.wiki/wiki.json` 的 `schema.types` 中。
- `--summary`：非空，一句话。
- `--source`：可重复，至少一个，且必须是 `sources` 表中已登记的 key。
- `--body`：文件路径，或 `-` 从 stdin 读取。
- 写入是原子的（同目录临时文件 + `os.replace`），随后重建链接索引。

### page delete <id> --yes

不可逆；无 `--yes` 返回 `E_CONFIRM`。

### settle [<key>...] [--skip]

收口来源：不带 key 时处理全部 `pending`。

- 默认要求该来源至少被一个页面引用，且至少有一个引用页不是 `stale`；否则该 key 进入
  `blocked`（退出码 1），并提示重读来源、重写页面。
- `--skip` 把来源标为 `skipped`，用于“看过但决定不建页”（空文件、重复件、纯附件）。
- 仍有 `pending` 时返回剩余列表；`ok` 为 false 表示有阻断项。

### status

来源状态计数、页面数、`stale_pages`、待处理 key。

### doctor [--fix]

检查：数据库完整性、目录存在、页面 id 唯一、frontmatter 必填字段、类型合法、引用来源已
登记、页面有来源、`[[链接]]` 是否有目标、来源是否消失。

- 默认只报告，`healthy: true` 表示无问题。
- `--fix` 只做机械修复：补目录、重建链接索引；不改正文、不删页面。
- 有 `fail` 级问题时退出码 1。

## 错误码

| 码 | 含义 | 处理 |
| --- | --- | --- |
| `E_CONFIG` | `wiki.json` 不是合法 JSON | 修语法或删除后重建 |
| `E_EMPTY` | 还没有任何来源 | 先 `ingest` |
| `E_NOT_FOUND` | 页面/来源/文件不存在 | 用 `query`/`status` 定位 |
| `E_PATH` | 路径越出知识库根 | 用相对路径 |
| `E_USAGE` | 参数缺失或组合不合法 | 看 `error.next` |
| `E_BAD_ID` | 页面 id 不合规 | 用小写 slug |
| `E_BAD_TYPE` | 类型不在 schema | 换类型或改 `wiki.json` |
| `E_BAD_SUMMARY` | summary 为空 | 补一句话 |
| `E_UNKNOWN_SOURCE` | 来源 key 未登记 | 用 `ingest` 输出的原 key |
| `E_NO_SOURCE` | 建页没给来源 | 加 `--source` |
| `E_CONFIRM` | 删除未确认 | 确认后加 `--yes` |
| `E_NOTHING` | 没有可结算的来源 | 先 `ingest` |
| `E_SOURCE_TOO_LARGE` | 超过 `ingest.max_source_bytes` | 调大阈值或移出目录 |
| `E_DB` | 数据库不可读 | 用备份恢复，不要手改 `wiki.db` |
| `E_NO_TARGET` | 安装时没有技能根（仅 install.py） | 原地加载或指定 `--target` |

## 磁盘布局

```
<root>/.wiki/
├── wiki.json      # schema.types / scan.ignore / ingest.max_source_bytes
├── wiki.db        # sources 账本 + links 索引（SQLite）
├── raw/           # 解析后的来源文本，一源一文件，带 frontmatter 溯源
└── pages/<type>/<id>.md   # 知识页，frontmatter + 正文
```

页面 frontmatter 字段：`id`、`title`、`type`、`summary`、`sources`（列表）、`status`、
`updated`（日期）。source 状态：`pending`、`done`、`skipped`、`missing`。

## 并发

SQLite 设置 5 秒 busy timeout；页面与 raw 均为原子替换。写入串行执行即可；不要并行跑
两个 `ingest` 或同时改同一页。读取（`query`/`read`）不受影响。
