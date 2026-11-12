---
name: llm-wiki
description: 当要在仓库里查历史结论、把材料沉淀成可检索的知识页、体检既有 wiki、处理来源待办或按定时任务摄入时使用。触发词：查 wiki、写页、摄取、沉淀、体检、来源确认、报错、wiki自动摄入、自动摄入。
---

# llm-wiki

## 边界

- 源文件只读。要写入的内容作为载荷交给 `task done`：工具渲染 frontmatter、把产物写进 `staging`，最后一个 page 任务关闭时整批合并到 `write_to`。不要直接改 `.wiki/pages/` 下的文件。
- 工具维护 raw、账本、索引、任务树与锁；作者只提供 title、summary、tags、status 与正文，`sources` 与 `updated` 由工具填写。
- 合并没有候选审核、没有工具内版本、没有回退，也不自动备份。合并前工具比对基线，页面在任务外被改过就拒绝（`E_PAGE_CONFLICT`）；需要恢复只能靠仓库外的独立备份。
- CLI 锁不覆盖文件系统层的写页；同一页只安排一个任务，并发作者仍可能互相覆盖。
- 对外命令是 `doctor`、`ingest`、`read`、`task done`、`task status`、`task retry`、`query`、`ignore`。参数与字段以 `--help` 和命令实际输出为准。

## 命令

先确认用户要操作的工作区根，使用其 `.wiki/cli/llm-wiki` 的绝对路径（Windows 为 `llm-wiki.exe`）；下文 `llm-wiki` 均指这一份，不用 PATH 中的其他版本。CLI 从自身位置定位工作区根，源和载荷的相对路径也以该根为准。技能复制到 agent 目录后，不能从技能所在位置推断工作区；根不明确就问用户。首次安装引导见目标工作区 `.wiki/readme.md`，首次接入见 init.md。下面只列常用入口。

- `llm-wiki doctor` 体检；`llm-wiki doctor --fix` 修可修的机械缺陷与索引。摄入与关闭任务已自带机械自检，不需要每轮先跑它。
- `llm-wiki ingest` 摄入并交付下一个任务；`--dry-run` 先看计划；`ingest <KEY>...` 只处理点名源。
- 旧版本留下的坏 raw（例如扫描版 PDF 曾被写成附件链接）用 `llm-wiki ingest --reconvert <KEY>...` 显式重转；只处理列出的来源，受 pin、手改与缺失账本记录保护。活动根会被同一事务阻塞，须由用户确认后 `task retry <根> --replan`。
- 用户输入 `wiki自动摄入` 时运行 `llm-wiki ingest --auto`：只跑默认一轮，不能和手动参数组合。
- `llm-wiki task done <ID> --payload <FILE>` 关闭 route 任务（提交 route-v1 归属）或 page 任务（提交一页完整正文）；`--accept` / `--regenerate` 关闭复核任务。关闭后返回下一个任务。
- `llm-wiki task status` 看任务树、当前任务与失败任务；`llm-wiki task retry <ID>` 由人重置失败任务，不要自动重试。
- `llm-wiki read <REF>` 读取内部文件；一次最多 2000 行，`--offset` 从第几行开始，`--limit` 改条数。
- `llm-wiki query <词>` 检索；`--graph` 看链接图；`--page <页>` 看这一页的出入链与来源，`--source <raw>` 看哪些页引用了这个源（两者可分页）。
- `llm-wiki ignore --add <PATTERN>` 维护忽略列表。
- 需要机器可读输出时加 `--json`；不要靠解析人类输出。

## 用与不用

- 查方案或历史结论：先 `query`，不要为了查而写页。
- 整理材料：走下面的最小序列；一页讲清一件事，不转述全文。
- 一次性代码问题或项目事实：直接读代码或维护项目文档。
- 会话中值得留的通用判断：用 `llm-wiki-session`。
- 用户没有授权写入：只检索，先展示拟写的标题与要点，等明确同意；「wiki自动摄入」本身就是写入授权。

## 最小序列

1. `ingest` 转换源、登记来源待办并交付下一个任务；`--dry-run` 只看计划。任务视图自带要读的源（`source.read_ref`）、目标页（`target`）与提交命令，按它推进，一次只做一个。
2. 路由任务（`route`）：读源正文，结合视图里的 `affected_pages` 与 `query` 找现有页，把每条信息点分配到 `update` / `create` / `skip`，给每个已引用页写 `maintenance`（`update`，或不改并写理由），`create` 附带提案，空信息点写 `source_reason`，用 `--payload` 提交 route-v1。route 关闭前不会有 page 任务；没有任何页需要新增或修订时，零写页也会直接结算。
3. 页面任务（`page`）：只在 route 判定需要新增或修订时出现。读自己的目标页（存在时）与 raw，融合成完整正文，用 `--payload` 提交；不要只交 diff。工具填 id、type、路径、sources、updated，并对 `target.before` 做快照校验。
4. 关闭后看 `outcome` 与 `remaining`：`complete` 表示这一轮没有可做的事，`in_progress` 还有下一个任务，`waiting` 在等源稳定，`needs_attention` / `partial` 要看 `issues` 或 `effects`。不要再用「没有下一个任务」猜收敛。
5. 合并与来源确认是两件事：`effects.merged_pages` 是已写进页面的页，`effects.confirmed_sources` 是四道哈希通过、待办已清的源。`partial` 表示页面已生效但结算没完成，要照实汇报。
6. 工具在刷新索引后自检新页是否进了关系索引；正文质量、结论是否成立仍由你判断，需要时再 `query` 查重或看引用。

刚改过的源默认等稳定窗口；`--force` 可跳过等待并全文复核，但不能越过最大源文件体积。元数据缓存可能漏掉大小与修改时间都保持不变的改动，需要时强制复核。

## 停点与报错

- 结论冲突、需要删页或大改结构：问用户，不静默覆盖。
- `error.code` 分类，`error.message` 指下一步；有 `error.detail` 时以具体证据为准。
- `E_PAGE_CONFLICT`：目标页在规划后被改过或新建。不要绕过校验直接写；先看这页现状，让来源重新路由（重跑 `ingest`，或让根走 `task retry <根> --replan`，已写页保留）。
- 转换失败或没有可用正文（扫描版 PDF、只有资源链接、全篇空白）是未处理，不是已整理：收尾逐份点名文件与 `error.code`，不要把「没有下一个任务」当成这些源已经有正文。
- `unsupported:` 点名的文件没有后端读取，未处理；不要为它们手写 raw。`E_RAW_COLLISION` 表示 raw 无法证明是机器产物（缺账本记录、摘要不符或正文被改），保留文件并请用户决定是否删除后重转。

## 任务外改动的复核

页面在任务外被改过时，`ingest` 会先交付一条 `reconcile` 任务，`target.reference` 是被改的页，任务视图里是该页引用的 raw 键。读这页和它引用的源，然后二选一：

- `task done <ID> --accept`：改动成立，保留现状并推进基线。用户手动修正过正文、或改动只是补充上下文时选这个。
- `task done <ID> --regenerate`：改动不该保留，把该页引用的来源重新入队，走正常的路由与写页覆盖它。

没有 sources 的页不会进复核：工具没有可对照的源，直接吸收进基线。页被删除也被当作接受的删除，来源消失仍由 `on_missing` 收口。不要用直接改 `.wiki/pages/` 的方式"修好"冲突——那只会让下一次扫描再报一遍。
- 退出码 1 是执行失败或体检 Fail；退出码 2 修用法、配置或环境；退出码 4 等占锁进程结束。
- 找不到命令先找项目构建产物，不绕过 CLI 手改账本。

## 写页与汇报

- 先 `query` 查重，短更新并入已有页；独立结论才新建。内容判断见 content.md。
- 结论变了保留旧判断与改判依据；来源由工具按当前 raw 挂上，不手填。
- 汇报结论、写到哪、哪些来源被跳过。页面已合并但任务失败，必须明确页面仍已生效。
- 不把任务收敛说成正文正确或可回退；不要贴整份 JSON。

## 按需阅读

- content.md：建页、证据与改写。
- ops.md：体检、漂移、覆盖风险、忽略与叫人。
- init.md：首次接入时由 agent 完成的两件事（持久化记忆与定时任务）。
