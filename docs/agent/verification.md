# 验证记录

本文件记录 2026-11-13 在 skills-only 版本（`VERSION=0.2.0`）上的实测结果，用于交付验收
与回归对照。数字来自实际命令输出，不是估算。

环境：macOS，Python 3.15.2（项目基线 3.12），SQLite 3.54.1，pdftotext 25.11.0（Poppler）。没有安装
`pypdf`，PDF 全部走 `pdftotext` 分支。

## 2026-11-15 任务信息面改造后复跑

`next` 改为只返回 `{id, brief, template, work}`，`submit` 不再要求 epoch；新建页面的
正文章节由 `template.body` 固定。复跑 `python3 tests/selftest.py`：
30/30 通过，退出码 0。新增检查覆盖任务包字段、内部字段不暴露、route/page 模板骨架。
下文表格是 2026-11-13 的 26 项基线，字段结构以本次改造后的契约为准。

## 2026-11-16 更新页与首批合并修复后复跑

`validate_page_payload` 拆到 `validation.py`；更新页保留旧 sources/evidence；首批多来源
可以 merge 到已排队页面。复跑 `python3 tests/selftest.py`：40/40 通过，
退出码 0。新增检查覆盖 route brief 的 reason、更新页旧证据保留、更新后 sources 并集、
`pending_pages` 对下一条 route 可见、pending merge 合并两来源、批量页面双来源、
`query --page` 的 page 元数据不含 `source_digests`。
手工 `/tmp/llm-wiki-fix-test` 复测：A/B 首批合并为 rev1 双来源双证据；C 更新为 rev2 后
sources 为 A/B/C，evidence 保留 A/B 并加入 C。Windows 侧没有跑过 PowerShell：技能目录由
上游放置、运行时自带，安装脚本不在使用路径上。

## 2026-11-16 自检门禁与测试拆分

- 自检从技能载荷移到仓库：`tests/selftest.py` 40/40 通过；安装后的副本只含
  `health.py` 与 `check` 命令，不含 `tests/` 或 `selftest.py`。
- `wiki.py check --json`：空工作区 `ok:true`；`init` 后 `ok:true`（ledger `integrity ok`）。
- 删除副本的 `SKILL.md` 后：`check` 返回 `ok:false`、退出码 1；`init` 返回 `E_HEALTH`。
- `init` 允许修复缺失的 `.wiki/wiki.db` 或 `wiki.json`（`allow_missing`），避免自检把首次初始化
  或半初始化目录锁死。
- 安装到临时目标后 `health.py` 存在、`tests/` 不存在，`check` 通过。

## 2026-11-16 场景能力后复跑

新增 `frontmatter.py`、`search.py`、`scenarios.py`，`query` 同时返回页面与场景，
新增 `scenario list/load`，`ingest` 跳过 `场景能力/`。复跑 `python3 tests/selftest.py`：
50/50 通过，退出码 0。新增检查覆盖：

- `scenario list` 同时报告合法与非法场景；
- `scenario list/load` 不创建 `.wiki/`；
- `scenario load` 返回主文件全文、路径与 `script`/`reference` 资源；
- `scenario load ../x` 返回 `E_USAGE`；
- `ingest` 不把场景目录当来源；
- `query` 分别返回 `matches` 与 `scenario_matches`；
- 修改场景正文后不重新摄入即可检索到；
- 非法场景不进 `query`。

手工临时目录复测：`scenario list/load` 在 `init` 前可用，执行后无 `.wiki/`；`query`
只命中场景时 `matches=0`、`scenario_matches=1`，人类摘要同时给出场景数与页面数。
本版本 `VERSION=0.3.0`。

## 2026-11-16 话题探索与话题沉淀后复跑

新增 `topics.py`、内置场景 `scenarios/topic-exploration/`，`query` 增加 `topic_scenario` 与
`topics`，`ingest` 跳过 `话题/`。`python3 -m py_compile skills/llm-wiki/scripts/*.py` 通过；
`python3 tests/selftest.py`：57/57 通过，退出码 0。新增检查覆盖：

- `scenario list` 同时列出项目场景与内置场景（3 个，2 个合法）；
- `scenario load topic-exploration` 返回内置正文，`source=builtin`，`name` 为英文、`alias_name` 为中文，`path` 以 `builtin/` 开头，
  `resources` 为空；
- 项目放同名 `scenario/topic-exploration/` 时覆盖内置，`source=project`；
- `ingest` 不把 `话题/` 当来源，话题文件存在时 `pending_total` 不变；
- `query` 恒返回 `topic_scenario`（id 为 `topic-exploration`），且 `topic_matches` 与页面
  `matches`、场景 `scenario_matches` 分开计数；
- `topics` 命中项带 `path`，按路径直接打开话题文件。

手工临时目录复测：只有 `话题/复机办理.md` 时，`query "复机"` 输出默认流程一行、topics 段，
`matches=0`、`topic_matches=1`，段落之间只有一个空行；`scenario list` 在 `init` 前可用。
本版本 `VERSION=0.4.0`。

## 2026-11-16 场景显式检索后复跑

`query` 不再返回 `scenarios`/`scenario_matches`/`topic_scenario`；新增
`scenario search <词...>`，项目场景与内置场景一起检索，`source` 标明来源。复跑
`python3 -m py_compile skills/llm-wiki/scripts/*.py` 通过；`python3 tests/selftest.py`：
57/57 通过，退出码 0。新增与改动检查：

- `query` 结果不含场景字段；
- `scenario search "开户"` 命中项目场景；`scenario search "话题"` 命中内置场景且
  `source=builtin`；
- 场景正文修改后 `scenario search` 立即命中，无需 `ingest`；
- 非法场景不进 `scenario search`；
- 项目同名场景覆盖内置（`source=project`）。

手工复测：`scenario search` 在 `init` 前可用且不创建 `.wiki/`。本版本 `VERSION=0.4.1`。

## 2026-11-16 场景目录改用 `scenario/` 后复跑

`scenarios.SCENARIO_DIRS = ("scenario", "场景能力")`，`scenario_roots` 只返回实际存在的目录，
`scan` 跨目录去重；`ingest` 两个目录都跳过，`publish.dir` 两个名字都算保留名。
本节基于 `d5f4327`（漂移失效与单树重构之后）：main 单独跑 102/102 通过，叠加本节改动后
`python3 tests/selftest.py`：105/105 通过，退出码 0。观看区与发布改动（`60c1e5b..f2e40bb`）
之后当前为 178/178（含 `sync`、发布事实、任务快照、`ingest` 补偿入队，以及
"未变化的源不被打开"的断言）；中间版本 81e9e77 是
175/175，177 与补偿入队同在 f2e40bb。套件只对墙钟有一处依赖：任务租约 900 秒
（`TASK_LEASE_SECONDS`），机器挂起超过它会让 `submit` 报 `E_TASK_STALE`。本节新增检查：

- `scenario list` 的 `roots == ["scenario", "场景能力"]`；
- `scenario/deck-flow` 可 `load`，`path` 以 `scenario/deck-flow/` 开头；
- 同一 id 在 `场景能力/deck-flow` 也有一份旧副本时，`load` 返回 `scenario/` 那份
  （`name == "English flow"`）；
- `ingest` 不摄入 `scenario/` 下的文件；
- `publish.dir = "scenario"` 被判为保留名并返回 `E_CONFIG`。

仓库根实测：`scenario list --json` → `2 valid, 0 invalid`，`roots: ["scenario"]`，
`scenario/ppt_creator` 的 `resources` 识别到 `references/outline-template.md`；
`scenario search "PPT"` 命中 1 条。

## 2026-11-17 `next` 恢复未提交任务后复跑（同日已改写，见下条）

队列不足一批时 `next` 重新发放仍在租约里的任务（`Wiki._resume_leased`），`summary` 以
`resuming` 开头，任务目录与已填 `payload.json` 保持不变；重新发放前照旧查漂移。
`python3 tests/selftest.py`：203/203 通过，退出码 0。新增两条检查：

- `next resumes a leased task instead of reporting no work`：领取后不提交再 `next` 返回同一
  任务，`payload.json` 不变；
- `next keeps reporting no work when nothing is open`：队列与租约都空时仍返回
  `no queued tasks`，agent 才停止。

## 2026-11-17 `next` 改为报出未提交任务后复跑

`_resume_leased` 删除，`claim_tasks` 只回队过期租约再按树领取；`command_next` 见 `leased`
行就报 `E_TASK_UNSUBMITTED`（退出码 3），错误里带任务 id、`payload.json` 路径与
`submit`/`release` 命令；`status.data.next` 在同样情形下改说「先提交手头任务」。
`python3 tests/selftest.py`：239/239 通过，退出码 0。相关检查：

- `next reports the open task instead of reissuing it`：领取后不提交再 `next` 得到
  `E_TASK_UNSUBMITTED`，`payload.json` 不变；
- `a refused next does not renew the lease or bump the epoch`：被拒前后 `epoch`、`attempts`、
  `lease_until` 三个字段完全相同；
- `an open page task outranks a freshly queued route task`：手头页面任务未交时，队列里新到的
  route 任务拿不到，`next` 报出页面任务；
- `a taken batch is reported instead of reissued`：`--batch 2` 领了两个后再次 `next` 报出这两个
  id，不重发。

## 2026-11-17 账本自愈与错误带手册定位

去掉「先 `check` 再 `init`」的门禁：打开账本的命令补齐缺的目录、`wiki.json` 与表，补出的
路径放在 `data.repaired`；`check`/`doctor` 退回只读诊断。旧结构有数据时 `E_STATE` 拒绝，
旧表全空时按当前结构原地重建（不写迁移）。`E_HEALTH`、`E_STATE`、`E_CONFIG`、`E_DB`、
`E_PATH` 的错误对象新增 `doc`（技能内 `references/recovery.md` 的绝对路径）与 `section`
（手册小节标题）。

`python3 tests/selftest.py`：208/208 通过，退出码 0。新增与改写的检查：

- 空目录直接 `ingest` 成功并返回 `repaired`，不依赖 `init`；
- 旧结构 + 有数据 → `E_STATE`，`section` 为「旧账本」，`doc` 指向 `references/recovery.md`；
- 旧结构 + 全空 → 原地按新结构重建（`sources` 出现 `raw_digest` 列）；
- 非数据库文件冒充 `wiki.db` → `E_DB`，`section` 为「账本损坏」；
- 只有 `wiki.json` 的半初始化工作区 → `ingest` 成功，`repaired` 含 `.wiki/wiki.db`；
- 只读工作区 → `E_HEALTH`，`section` 为「工作区不可写」，`next` 不指向 `check`；
- `check` 在无账本的工作区仍 `ok:true`。

手工复测（macOS，Python 3.15.2）：

| 场景 | 结果 |
| --- | --- |
| 空目录 `ingest` | `ok:true`，`repaired` 含 `.wiki/tasks`、`.wiki/drafts`、`.wiki/wiki.json`、`wiki/raw` |
| 只有 `.wiki/wiki.json` | `ingest` 成功，`repaired` 含 `.wiki/wiki.db` |
| 空 db 文件（0 张表） | `ingest` 建表成功 |
| 旧库有行 | `E_STATE`，退出码 3，`section`「旧账本」 |
| `wiki.db` 写成文本 | `E_DB`，`message` 为 `ledger cannot be opened: file is not a database`，退出码 3 |
| 半初始化目录 `check` | `ok:true`，ledger 项 `detail` 以 `repairable:` 开头 |

## 2026-11-18 场景改名为可执行工作流后复跑

`scenario` 改为 `workflow`：命令硬切为 `workflow list/search/load`，目录仍三向读取
（`workflow/` > `scenario/` > `场景能力/`），主文件 `WORKFLOW.md` 优先、缺失回退
`SCENARIO.md`；步骤要求每步标 `tool:`/`human:`/`agent:`/`verify:`，整份至少一个 `tool:`。
目录双读在同日复核后改为只认 `workflows/`，见下一节。

`python3 tests/selftest.py`：258/258 通过，退出码 0。新增与改写的检查：

- 三目录优先级与同名 id 去重；`WORKFLOW.md` 缺失时回退 `SCENARIO.md`；
- `workflow list` 的 `roots`/`warned` 与每项的步骤计数；`workflow load` 返回
  `tools`/`human_steps`/`agent_steps`/`verify_steps`/`warnings`；
- 全角冒号与大小写归一；3 空格缩进的子项、代码块与其他节的列表不算步骤；
- 零标记给 `W_STEPS_UNTYPED`（0.5.0 合法，0.6.0 起判无效），部分未标给
  `W_STEP_UNTYPED`，纯人工流程 `E_WORKFLOW_INVALID` 且 `next` 指向 `process` 页；
- 内置 `topic-exploration` 从 `workflows/` 加载，`check` 列出 `workflow:topic-exploration`；
- 观看区投放 `workflows/`、旧 `scenarios/` 不被删除、`publish.workflows=false` 停止投放；
- `ingest` 跳过三目录，`query` 不混入工作流结果；
- `publish.scenarios` 按值迁移为 `publish.workflows`。

`wiki.py check` 的 `workflow:topic-exploration` 一项通过，账本完整性报 `ok`。

## 2026-11-18 工作流目录统一为 `workflows/` 后复跑

项目工作流只从 `<root>/workflows/` 读取；`workflow/`、`scenario/`、`场景能力/` 的兼容扫描、
`SCENARIO.md` 回退与 `publish.scenarios` 迁移一并移除，场景概念不再存在。复跑
`python3 tests/selftest.py`：281/281 通过，退出码 0。相关检查：

- `workflow_roots` 只返回实际存在的 `workflows/`，`workflow list` 的 `roots` 为 `["workflows"]`；
- 目录里只有 `SCENARIO.md` 时判为不合法，`error` 为 `missing WORKFLOW.md`，不再回退；
- 观看区投放 `workflows/`，`publish.workflows=false` 停止投放，遗留的 `publish.scenarios` 键被忽略；
- `ingest` 跳过 `workflows/`。

`python3 skills/llm-wiki/scripts/wiki.py workflow load ppt_creator --json`：`valid: true`、
`warnings: []`，步骤计数 2 tool / 1 human / 4 agent / 1 verify。

## 一、自测：281/281 通过

```bash
python3 tests/selftest.py
```

| 用例 | 覆盖点 |
| --- | --- |
| `init` 建库 | 生成 `.wiki/`、`wiki.db`、`wiki.json`，来源为 0 |
| `--root` 区外 | 指向工作区外的根目录返回 `E_PATH`，退出码 2 |
| `ingest` 排队 | 每份正文一条 route 任务（同正文多路径合并），`pending_total` 正确 |
| 共享正文 | 两源同正文只有一份 raw，只排一条 route 任务，`subject` 是 raw id |
| 挂载新路径 | 已排队的任务不受影响：不报 `E_SOURCE_DRIFT`，引用页不误标 `stale` |
| 孤儿页清理 | 全源缺失且无出入链的页在 `ingest` 末尾删除，有链的只标 `stale` |
| 图片分类 | `.png` 进 `unsupported`，不影响其他文件 |
| route 领取 | 一条任务带该正文的全部挂载源，`work.sources` 带 raw 路径 |
| payload 区外 | 载荷文件在工作区外返回 `E_PATH` |
| 任务目录 | `next` 生成 `.wiki/tasks/<id>/{brief.md,payload.json,work.json}` |
| 任务目录生命周期 | 提交后删目录；释放后重领只刷新 brief/work，已填 `payload.json` 不变；关闭任务的残留目录被 `ingest` 清掉 |
| 任务恢复 | 领取后不提交再 `next` 返回同一任务且 `payload.json` 不变；队列与租约都空时才返回 `no queued tasks` |
| 租约归属 | `next --owner` 记下领取会话并回显；别的会话领取、`ingest` 报 `E_INGEST_BUSY` 且 `message` 含占用方，提交报 `E_TASK_FOREIGN` |
| 过期租约回收 | 租约过期后下一次 `next` 直接收回，不当作活会话挡住队列；`release` 输出原持有方，释放后别的会话能领取 |
| 单页单任务 | 第二个来源路由到已排队的页面时挂到该任务上，账本只有一条未关闭 page 任务；被领走时返回 `E_STATE` |
| 工作区文件锁 | 另一进程持锁时写命令等 5 秒后 `E_BUSY`（退出码 4）；陈旧锁被接管 |
| `skip` 决定 | 被拒决定返回可读错误，接受后来源变 `skipped` |
| route `new` | 入队 page 任务，任务带来源与 raw |
| route 拆页 | 同一来源两条 `new` 指向不同页 → 两个 page 任务，来源可挂两棵同源树；重复 `(source, page)` 与 `skip` 混用被拒 |
| `body_file` 区外 | 草稿路径越界返回 `E_PATH`，任务回队列 |
| page 提交 | 生成 rev1，且不写 `.wiki/pages/` |
| 任务关闭后重交 | `E_TASK_STALE`，退出码 3 |
| `status` | 页数、stale、开放任务统计正确 |
| `query` | 命中页数与 id 正确 |
| `query --page` | 读账本，`path` 指向观看区，`sources[].raw` 由账本渲染 |
| 观看区被改 | 推送只告警不覆盖，`sync` 收成新修订 |
| `page import` | 生成 rev2，`origin=import` |
| import 区外 | `--from` 越界返回 `E_PATH` |
| import links | frontmatter 块列表并入旧链接，未知目标报 `E_LINK_UNRESOLVED` |
| `page link` | 新增边生成新 rev，重复链不写修订 |
| `page link --repair` | 同源两页 dry-run 计划 1 条边；执行后两边 `## 关联` 可见；重跑 0 条 |
| link 非法目标 | 未知目标、自己、空目标分别报 `E_LINK_UNRESOLVED`/`E_USAGE` |
| 关系页 sync | 先剥关系段再解析证据，正文不重复累积证据块 |
| `page restore` | 回滚内容生成 rev3 |
| `export` | 渲染文本复制到目标目录 |
| export 区外 | 目标目录越界返回 `E_PATH`，不写文件 |
| 超限 | `max_source_bytes=10` 时 100 字节文件进 `oversize`，退出码 1 |
| `--dry-run` | 未初始化目录不创建 `.wiki/` |
| `doctor` | 跑完并返回结构化 issues |

输出以 `281/281 checks passed` 结尾，退出码 0。`--json` 输出可供 CI 解析。

## 二、真实语料：4 份中国电信 PDF

从只读语料目录复制到 `/tmp` 隔离目录，原文件不动。

| 来源（节选） | 解析字符数 | parser |
| --- | --- | --- |
| `676516_…云网安全特别主题日活动…讲话` | 3021 | pdftotext |
| `676532_…反诈模型规范性运营管理…通知` | 29357 | pdftotext |
| `676593_…火灾隐患排查整治专项工作检查情况…通报` | 3574 | pdftotext |
| `676604_…天翼云一城一池标准化维护实施方案` | 66627 | pdftotext |

另复制一份 80,298,855 字节的合同 PDF：`ingest` 将其归入 `oversize`（默认
`max_source_bytes=50000000`），退出码 1，其余文件照常解析。

全流程（真实任务闭环）：

```text
ingest: 4 sources ingested, 4 route tasks queued; 4 pending; 1 oversize
next --batch 10: 4 route tasks
route: skip × 3, new × 1 (fire-hazard-inspection-2026) → 1 page task
page submit: rev 1
doctor: healthy true, 0 issues
status --brief: 5 sources (0 pending, 0 unsupported, 1 oversize), 1 pages (0 stale)
```

页面证据取自 raw 第 4 个非空行，`submit` 校验通过；把引文改错一个词则返回
`E_EVIDENCE_NOT_FOUND`。

## 三、边界与错误码

| 场景 | 结果 |
| --- | --- |
| 未初始化目录跑 `ingest --dry-run` | 不创建 `.wiki/`，目录内容原样 |
| 放入 `.png` | `unsupported`（no text backend produced content），不中断其他文件 |
| `max_source_bytes=10` 摄入 100 字节文件 | `oversize`，退出码 1，不写 raw |
| 已关闭任务重交 | `E_TASK_STALE`，退出码 3 |
| 另一个进程持锁 | `E_BUSY`，退出码 4 |
| 载荷引用任务范围外来源 | `E_TASK_SCOPE`，退出码 3 |
| 引文不在 raw 里 | `E_EVIDENCE_NOT_FOUND`，退出码 2 |
| 缺 `boundaries` | `E_PAYLOAD`，退出码 2 |
| `doctor` 有 `fail` 级问题 | 退出码 1 |

## 四、安装

`scripts/install.py` 只做目录复制与自检，不写全局状态：

| 操作 | 结果 |
| --- | --- |
| `--target auto` | 装入第一个已存在的技能根，安装后 `wiki.py version` 自检通过 |
| 再执行一次 | `skipped`，退出码 0，不覆盖 |
| `--force` | 旧目录改名为 `llm-wiki.bak-<时间>` 后重新复制 |
| `--target "<含空格目录>"` | 安装成功，可在该路径直接运行 |
| 无任何已知技能根 | `E_NO_TARGET`，提示原地加载 `SKILL.md` 或显式传 `--target` |

## 五、未覆盖

- 66 份全量语料的 dry-run 与真实入库：只跑了 4 份 + 1 份超限样本。
- `pypdf` 分支：环境没有该库，未实测。
- OCR/扫描件：明确不支持，只在 `unsupported` 中提示。
- 并发写入与多 agent 同时提交：只验证了 lease 与 epoch 的单进程语义。
- 万级文件、`MAX_RAW_CHARS` 截断边界、超大页面的渲染性能。
- DSH 之外的 agent 运行时：按目录约定复制，未逐个实测其技能加载器。
- Windows 引导脚本：没有实机执行过（当前环境没有 PowerShell）。脚本已精简为「查找已有的
  Python 3.12+ → 调用 `install.py`」，不再下载或安装 Python。Windows 上的技能目录由上游
  放置、agent 自带 3.12+ 运行时，这两个脚本是可选手动工具、不在默认使用路径上，因此不再
  作为未覆盖项跟踪；需要手动安装时再补测语法、参数透传与 `E_PYTHON`。
- 页面删除：当前没有命令，未验证。

## 2026-11-18 话题目录改名后复跑

根目录 `话题/` 改名 `topics/`，同步 `topics.TOPIC_DIR`；`ledger.py` 的扫描跳过与
`publish.dir` 保留名校验从该常量取值，`view.py` 投放目标不变。复跑
`python3 -m py_compile skills/llm-wiki/scripts/*.py` 通过；`python3 tests/selftest.py`：
290/290 通过，退出码 0。检查点：

- `ingest` 跳过 `topics/`，话题文件不出现在 `ingested`；
- `query` 命中话题时 `topics[].path` 以 `topics/` 开头；
- `publish.dir=topics` 报 `E_CONFIG`（保留名）；
- 观看区仍把话题首次投放到 `<view>/topics/`，用户改过的副本不被覆盖。
