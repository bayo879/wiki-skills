# 验证记录

本文件记录 2026-11-13 在 skills-only 版本（`VERSION=0.2.0`）上的实测结果，用于交付验收
与回归对照。数字来自实际命令输出，不是估算。

> 历史说明：以下 2026-11-19 之前的验收记录保留当时口径。`publish.topics`、
> `publish.workflows` 与“首次投放”行为已于 2026-11-19 移除，相关旧段落只作历史对照。

环境：macOS，Python 3.15.2（项目基线 3.12），SQLite 3.54.1，pdftotext 25.11.0（Poppler）。没有安装
`pypdf`，PDF 全部走 `pdftotext` 分支。

## 历史记录

| 页面 | 时段 | 内容 |
| --- | --- | --- |
| [2026-11-15 至 11-18](verification/2026-11-15-18.md) | 11-15 ~ 11-18 | 任务信息面、更新页、自检门禁、场景能力、话题探索、显式检索、场景目录改名、`next` 两则、账本自愈、工作流改名与目录统一 |
| [2026-11-18 至 11-19](verification/2026-11-18-19.md) | 11-18 ~ 11-19 | 自测 281/281 与真实语料等五项检查、话题目录改名、内置工作流 `skill_path` |

最新的两条复跑留在本页。

## 2026-11-19 内置工作流路径改为 skill_path 后复跑

内置工作流不再使用 `builtin/` 展示标签，也不给根目录相对的 `path`/`main`，改给技能目录相对的
`skill_path`（`workflows/<id>/WORKFLOW.md`）；项目工作流保持 `path`（目录）与 `main`（主文件）。
复跑 `python3 -m py_compile skills/llm-wiki/scripts/*.py` 通过；
`python3 tests/selftest.py`：294/294 通过，退出码 0。检查点：

- `workflow list` 六个工作流：三个项目项带 `path`/`main`，三个内置项只有 `skill_path`、
  `resources` 为 0，文本行打印 `skill_path:` 而不是 `path:`；
- `workflow load workflow-create` 的 `data` 顶层没有 `path`/`main`，`skill_path` 为
  `workflows/workflow-create/WORKFLOW.md`；`workflow load ppt_creator` 仍返回
  `path: workflows/ppt_creator/WORKFLOW.md`；
- `workflow search 工作流 评审` 与 `query 工作流` 的候选按来源二选一给 `path` 或
  `skill_path`，内置项不再出现 `builtin/` 前缀。


## 2026-11-19 工作流与话题定义收进观看区后复跑

项目工作流与话题改为从观看区读取，旧根目录只作只读兜底；任务产物移到
`<view>/runs/workflows/`。复跑 `python3 -m py_compile skills/llm-wiki/scripts/*.py` 通过；
`python3 tests/selftest.py`：328/328 通过，退出码 0。检查点：

- 观看区定义优先：`<view>/workflows` 与 `<view>/topics` 命中时 `origin=view`，同 id 不再用
  旧根版本；旧根独有项 `origin=legacy`，`workflow list` 人类行标 `[legacy]`；
- 同 id 两边 `WORKFLOW.md` 不同时，`workflow list` 带 `legacy_conflict`/`legacy_path` 并
  打印“观看区版本生效”，`workflow load` 仍返回观看区正文；
- `query` 同时命中观看区话题与工作流，`publish.dir="kb"` 时路径以 `kb/` 开头；`sync` 不再
  投放或覆盖 `workflows/`、`topics/`；
- `ensure_publish` 在 `enabled:false` 时仍建 `<view>/workflows`、`<view>/topics`；`runs/`
  不在推送时创建。
