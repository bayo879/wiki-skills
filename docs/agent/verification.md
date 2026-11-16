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
read page 的 meta 不含 `source_digests`。
手工 `/tmp/llm-wiki-fix-test` 复测：A/B 首批合并为 rev1 双来源双证据；C 更新为 rev2 后
sources 为 A/B/C，evidence 保留 A/B 并加入 C。PowerShell 引导仍未实机验证，见
[Windows 引导验收](../issues/2026-11-16-P2-windows-bootstrap-acceptance.md)。

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

新增 `topics.py`、内置场景 `scenarios/话题探索/`，`query` 增加 `topic_scenario` 与
`topics`，`ingest` 跳过 `话题/`。`python3 -m py_compile skills/llm-wiki/scripts/*.py` 通过；
`python3 tests/selftest.py`：57/57 通过，退出码 0。新增检查覆盖：

- `scenario list` 同时列出项目场景与内置场景（3 个，2 个合法）；
- `scenario load 话题探索` 返回内置正文，`source=builtin`，`path` 以 `内置场景/` 开头，
  `resources` 为空；
- 项目放同名 `场景能力/话题探索/` 时覆盖内置，`source=project`；
- `ingest` 不把 `话题/` 当来源，话题文件存在时 `pending_total` 不变；
- `query` 恒返回 `topic_scenario`（id 为 `话题探索`），且 `topic_matches` 与页面
  `matches`、场景 `scenario_matches` 分开计数；
- `topics` 命中项带 `path` 与可直接执行的 `read "<path>"`。

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
`python3 tests/selftest.py`：105/105 通过，退出码 0。新增检查：

- `scenario list` 的 `roots == ["scenario", "场景能力"]`；
- `scenario/deck-flow` 可 `load`，`path` 以 `scenario/deck-flow/` 开头；
- 同一 id 在 `场景能力/deck-flow` 也有一份旧副本时，`load` 返回 `scenario/` 那份
  （`name == "English flow"`）；
- `ingest` 不摄入 `scenario/` 下的文件；
- `publish.dir = "scenario"` 被判为保留名并返回 `E_CONFIG`。

仓库根实测：`scenario list --json` → `2 valid, 0 invalid`，`roots: ["scenario"]`，
`scenario/ppt_creator` 的 `resources` 识别到 `references/outline-template.md`；
`scenario search "PPT"` 命中 1 条。

## 一、自测：26/26 通过

```bash
python3 tests/selftest.py
```

| 用例 | 覆盖点 |
| --- | --- |
| `init` 建库 | 生成 `.wiki/`、`wiki.db`、`wiki.json`，来源为 0 |
| `--root` 区外 | 指向工作区外的根目录返回 `E_PATH`，退出码 2 |
| `read` 区外 | 读工作区外文件返回 `E_PATH` |
| `ingest` 排队 | 每个来源一条 route 任务，`pending_total` 正确 |
| 图片分类 | `.png` 进 `unsupported`，不影响其他文件 |
| route 领取 | 一个来源一条任务，`work.sources` 带 raw 路径 |
| payload 区外 | 载荷文件在工作区外返回 `E_PATH` |
| `read` raw | 返回解析文本，`content` 为正文 |
| `skip` 决定 | 被拒决定返回可读错误，接受后来源变 `skipped` |
| route `new` | 入队 page 任务，任务带来源与 raw |
| `body_file` 区外 | 草稿路径越界返回 `E_PATH`，任务回队列 |
| page 提交 | 生成 rev1 与 `.wiki/pages/<type>/<id>.md` |
| 任务关闭后重交 | `E_TASK_STALE`，退出码 3 |
| `status` | 页数、stale、开放任务统计正确 |
| `query` | 命中页数与 id 正确 |
| `read` page | 读账本而不是镜像文件 |
| 镜像被改 | 原文进 `.wiki/drift/`，镜像按账本还原 |
| `page import` | 生成 rev2，`origin=import` |
| import 区外 | `--from` 越界返回 `E_PATH` |
| `page restore` | 回滚内容生成 rev3 |
| `export` | 页面镜像复制到目标目录 |
| export 区外 | 目标目录越界返回 `E_PATH`，不写文件 |
| 超限 | `max_source_bytes=10` 时 100 字节文件进 `oversize`，退出码 1 |
| `--dry-run` | 未初始化目录不创建 `.wiki/` |
| `doctor` | 跑完并返回结构化 issues |

输出以 `26/26 checks passed` 结尾，退出码 0。`--json` 输出可供 CI 解析。

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
| 载荷引用任务范围外来源 | `E_TASK_SCOPE`，退出码 3 |
| 引文不在 raw 里 | `E_EVIDENCE_NOT_FOUND`，退出码 2 |
| 缺 `boundaries` | `E_PAYLOAD`，退出码 2 |
| `read ../outside.md` | `E_PATH`，退出码 2 |
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
- Windows 引导脚本：当前环境没有 PowerShell，`scripts/bootstrap.ps1` 与 `bootstrap.cmd`
  未在 Windows 实机验证；镜像下载、静默安装、PATH 刷新与技能根放置都待验收，见
  `docs/issues/2026-11-16-P2-windows-bootstrap-acceptance.md`。
- 页面删除：当前没有命令，未验证。
