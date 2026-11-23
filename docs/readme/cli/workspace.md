# CLI 契约：工作区与安装

`check`、`init`、`export`、`status`、`sync`、`doctor` 与 `.wiki/config.json`。

返回 [CLI 契约](../cli.md)；所有命令以会话工作区为根目录。

## check

只读诊断：Python 3.12+、技能必需文件与模板、工作流与手册文件、工作区可写、已有账本可读。
不创建 `.wiki/`，不读写页面内容，也不修任何东西。`data`: `{ok, doc, checks[{name,ok,detail}]}`；
`doc` 是恢复手册路径，失败时用它找到对应小节；`ok` 与退出码一致（`0` 通过，`1` 有失败项）。
账本项可以给出 `repairable`（结构能补，例如有 `wiki.json` 却缺 `wiki.db`）：仍算通过，
`detail` 以 `repairable:` 开头，下一条打开账本的命令会补上。

`check` 不是前置门禁：`init`、`ingest`、`next`、`submit`、`release` 只做安装级检查
（解释器、技能文件、工作区可写），失败返回 `E_HEALTH`；账本缺失或残缺在打开账本时自愈。
完整回归测试在仓库的 `tests/selftest.py`，不随技能分发。

## init

创建 `.wiki/`（`wiki.db`、`wiki.json`、`tasks/`、`drafts/`），并准备
观看区 `<view>/` 与 `<view>/raw/`。不是前置步骤：任何打开账本的命令都会补同样的东西；
只建库、暂不摄入时才单独跑。`data`: `{config, pages, sources, open_tasks}`，本次补出的
路径在 `data.repaired`。

## export

`export <dir>`：把所有页面按 `type/id.md` 写到目标目录，用于离线浏览或迁移。目标目录
必须在工作区内，建议 `.wiki/export`。

## status

`status [--brief]`。`data`: `{created, sources{状态:数量}, source_total, pages,
stale_pages, pending, tasks{open,queued,leased,route,page,expired,accepted}, leases,
next}`。`leases` 是 `[{task, owner}]`，列出当前租约及其归属会话；`next` 在别人持租约时
给出 `stop: … is already ingesting`，而不是让你去提交。
`--brief` 只返回 `brief` 一行摘要，没有 `tasks`；完整数量（含 `expired`、`accepted`）用
`status --json`，不要加 `--brief`。

## sync

`sync [--body-only]`：把观看区 `<view>/pages/**/*.md` 的手工修改回收进账本（新修订
`origin=sync`），只回收、不推送。页面正文只存在于账本与观看区两处，没有第二份镜像。
手改未收割期间该页整体冻结：推送不覆盖、也不按新标题改名，`page import` 的
改名要到回收之后才在观看区出现。

- 文件没有 `id` 或 `id` 不在账本里：跳过，`reason` 分别为 `no id`、`unknown id`；路径逃出
  观看区（例如符号链接指向外面）时记为 `escapes the publish dir`。
- 同一 `id` 出现在多个文件：每个文件都记 `E_DUPLICATE`，不回收。
- frontmatter 的 `title`、`type`、`summary`、`sources` 与账本不一致：该文件记
  `E_META_CHANGED`；加 `--body-only` 只取正文与证据，保留账本元数据（类型、来源、标题等）。
  `status`、`updated`、`aliases` 不参与比较，成功回收后按账本重写；`id` 改了会按
  `unknown id` 跳过。
- 正文为空 `E_PAYLOAD`；证据引用未登记来源 `E_UNKNOWN_SOURCE`；文件读不出 `E_IO`。
- `## 证据` 段存在但格式不合法：整段保留为正文并在 stderr 警告，账本原证据不变；同一条
  警告也会追加到 `data.warnings`。回收同时认旧式纯文本证据行和新式 Markdown 链接
  证据行。最常见的情形是在证据块后面追加一行——整块会被当正文，规范化重写后正文里会
  多出一个 `## 证据` 段。
- 回收成功后把规范化文本写回观看区；只是位置或字节与上次发布一致的文件记为 `written`，
  不产生新修订。

`data`: `{imported[page], skipped[{file,reason}], failed[{file,page,code,message}],
unchanged[page], written[{page,from,to}], warnings[message]}`。有失败项时 `ok=false`，
退出码 1；`warnings` 不改变成功状态。

`sync` 之后，下一条会启动引擎的命令启动时才把账本推回观看区。

## doctor

`doctor [--fix]`：`PRAGMA integrity_check`、stale 页、未注册来源、
无可用来源的页（`sources` 为空或全部缺失/失效，L7；其中零入零出的标 `removable`，
doctor 直接删除，不新增命令、不逐页确认；有来源的零出链页不报）、过期租约。断链不再检查：
links 表只由通过 `check_links` 的提交写入，没有留下悬空 `dst` 的路径。
`--fix` 重建链接索引。`data`: `{issues[{severity,code,message}],
fixed, healthy}`；有 `fail` 级问题时退出码 1。`doctor` 不检查观看区：未收割的手改只在
推送时告警，`healthy:true` 不代表观看区没有待回收内容，用 `sync` 收割。

## 配置

`.wiki/wiki.json`（首次运行生成）：

```json
{"schema": {"types": ["concept", "entity", "process"]},
 "scan": {"ignore": [".git", "node_modules", "target", "dist"]},
 "ingest": {"max_source_bytes": 50000000},
 "publish": {"dir": "wiki", "enabled": true}}
```

改 `schema.types` 会立即影响后续校验；已存在的页面不会被自动迁移。老库配置里的 `note`
（连同 `wiki.json` 里显式列出的其他类型）继续可用：校验、章节骨架与推送都按原样走，
只是新建页面不要再选它；要收口就人工把这些页并进 `concept`/`process`，再把它从
`schema.types` 里删掉。

`publish.dir` 是观看区目录名，必须是单个目录名：不能以点开头、不能含路径分隔符，也不能
是 `.wiki`、`topics`、`workflows`；非法值即使 `enabled:false` 也返回 `E_CONFIG`。
`enabled:false` 只停推送，`<view>/` 与 `raw/`、`workflows/`、`topics/` 仍会创建；`pages/` 不推送，`runs/` 不主动创建。`publish.dir` 改名等于换观看区：旧目录不再跳过，定义、话题和产物
不会自动搬迁；需要保留时先手动移动，再改配置。
