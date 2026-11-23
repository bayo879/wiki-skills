# 技能与契约维护

文档体系与提交的通用规则见全局 `~/.dsh/AGENTS.md`；本文件只记录这个项目的同步范围与
守卫。

## 一处定义，多处引用

| 内容 | 唯一定义处 | 其他位置只引用 |
| --- | --- | --- |
| 命令、载荷字段、错误码、退出码 | `docs/readme/cli.md` | `SKILL.md` 只给取任务/提交示例，不复制字段表 |
| 任务 brief 与 template 的形状 | `skills/llm-wiki/scripts/tasks.py` | `cli.md` 给字段说明，`tests/selftest.py` 给断言 |
| agent 行为约束 | `skills/llm-wiki/SKILL.md` | `README.md` 只讲产品边界 |
| 页面质量与证据标准 | `skills/llm-wiki/references/curation.md` | — |
| 话题的文件格式、归属与写入流程 | `docs/readme/topics.md`（流程细节在内置工作流 `skills/llm-wiki/workflows/topic-exploration/WORKFLOW.md`） | `SKILL.md` 只给触发、边界与入口 |
| 页面固定版式与正文模板 | `skills/llm-wiki/templates/`（按类型分文件） | — |
| 安装与目录约定 | `docs/readme/install.md` | `README.md` 给一行入口 |
| 工作流目录、frontmatter、步骤契约与写法 | `docs/readme/workflows.md` | `SKILL.md` 只给加载与执行约束 |
| 故障与恢复 | `skills/llm-wiki/references/recovery.md`（随技能安装，按错误码分节） | `docs/readme/recovery.md` 只留原则与入口 |
| 实测结果与未覆盖项 | `docs/agent/verification.md` | — |
| 内部实现约定 | `docs/agent/engine.md`、`docs/agent/engine-invariants.md` | 布局与检查表在前者，存储/并发不变量在后者 |
| 已定方向 | `docs/agent/development-decisions.md` | — |

改命令、载荷、错误码时，至少同时检查：`docs/readme/cli.md`、`skills/llm-wiki/scripts/tasks.py`、
`skills/llm-wiki/scripts/validation.py`、`skills/llm-wiki/scripts/health.py`、`SKILL.md`、
`skills/llm-wiki/references/recovery.md`、`tests/selftest.py`、`README.md` 中的示例。
改工作流行为时，同时检查 `docs/readme/workflows.md`、`skills/llm-wiki/scripts/workflows.py`、
`skills/llm-wiki/scripts/workflow_steps.py` 与 `query`/`workflow` 的输出断言。`workflows.WORKFLOW_DIRS` 改动时还要同步 `ingest` 的根级
跳过、`publish.dir` 的保留名、观看区投放目录与 `docs/readme/cli.md`。改步骤契约
（`workflow_steps.py` 的 `parse_steps`、`_contract`、`UNTYPED_ALLOWED`）时，同步 `workflows.md` 的判定表、
`SKILL.md` 的四类标记段、`health.py` 的 `BUILTIN_WORKFLOWS` 与 selftest 的夹具。
改话题流程时，同时检查 `SKILL.md` 的触发描述、`docs/readme/topics.md` 与内置工作流
`skills/llm-wiki/workflows/topic-exploration/WORKFLOW.md` 的归属、授权与格式约束。
示例里的命令必须能直接跑通。
改章节骨架（`tasks.py` 的 `PAGE_SECTIONS`）时，同步 `templates/` 下对应类型的正文模板，
保证模板章节与引擎一致。`PAGE_SECTIONS` 里可以留着不在默认 `schema.types` 中的类型
（目前只有 `note`）：那是给老库兜底的遗留映射，删掉会让老库的这类页面更新时被套上
`concept` 的骨架而被拒；新增或删除类型时，`ledger.DEFAULT_CONFIG`、`templates/`、
`health.TEMPLATES`、`curation.md` 与 `tests/selftest.py` 要一起看。
改 Windows 入口行为（`scripts/bootstrap.ps1`、`scripts/install.py`）时，同步 `README.md`、
`docs/readme/install.md`、`SKILL.md`、`references/recovery.md` 的运行时小节；这两个脚本是
可选的手动安装工具，不在默认使用路径上，行为没有实机证据前不要写成已验证。

## SKILL.md 的 description

`description` 是技能表（catalog）里唯一常驻的元数据，只负责“要不要加载”，不承担加载后的
契约；它随会话预载，写了什么就等于每次会话都付费。

- 只写两件事：什么情况下用（`wiki:` / `workflow:` 前缀与请求类型）、一句话能力。流程
  （`next`/`brief`/`submit`、`query` 的工作流候选、默认话题探索）、命令字段、依赖与安装
  事实都不写，分别归 `SKILL.md` 正文、`docs/readme/cli.md`、`docs/readme/install.md`。
- 反例（联网取数、OCR、写代码、排版渲染）留在 `SKILL.md` 正文的“需求超出 wiki”一条，
  不进 `description`：触发后读到再退出，代价小于让负条款压低触发率。
- 上限沿用 Anthropic 规范的 1024 字符（`.dsh/skills/writing-skills/references/anthropic-best-practices.md:150`），
  `tests/checks_repo.py` 有对应守卫；这条守卫只拦失控，正常改动仍按上面两条判。

## 机器守卫

```bash
# 需要 Python 3.12+
python3 -m py_compile skills/llm-wiki/scripts/*.py skills/llm-wiki/scripts/*/*.py
python3 tests/selftest.py
```

`tests/selftest.py` 是端到端契约测试的入口：它建临时工作区，然后按固定顺序调用 `tests/checks_*.py`
里的检查组（`support.py` 放路径、子进程 helper 与跨组状态 `Ctx`，`fixtures.py` 放工作流/话题样例）；
覆盖 next 任务包、route/page 提交、工作区越界、观看区推送、import/restore、超限与 dry-run。改引擎行为
时先补一个检查再改实现，新检查加进对应主题的 `checks_*.py`；入口同时被 CI 调用，不允许跳过或
`--keep` 写入仓库。

新增行为若无法在临时目录验证（真实 PDF、OCR、并发），写进 `docs/agent/verification.md`
的"未覆盖"（当前在 `verification/2026-11-18-19.md`），不要写成已验证。

- 不拿开发目录当 wiki 测试场：仓库自身带 `.wiki/` 与观看区，但所有验证都在
  `tests/selftest.py` 的临时工作区（`tempfile.mkdtemp`）或另起的临时目录里跑；会在仓库根生成
  `.wiki/`、任务队列或观看区投放的命令（`init`、`ingest`、`next`、`submit`、`sync`）不要手动
  执行。测试代码放 `tests/`，一次性复现脚本与语料放临时目录，不落到仓库根。

## 改文档

- 用 `edit`（找不到原文就报错），不用 `sed`/`python` 静默替换；改完当轮用 `grep` 或
  检索核实新内容，再报告。
- 不要把候选方案写进 README 或 `docs/readme/` 当作已实现能力。
- 纯文档提交在提交信息里说明原因。

## 提交

- 一个改动一个提交；行为变化与文档同步放在同一提交。
- 不提交 `.wiki/`、`__pycache__/`、`superpowers/`、本地语料或临时验证脚本。
- Rust 相关内容已删除；如果发现残留（`Cargo.toml`、`output/`、workflow 里的 release
  构建），先确认是否遗漏，不要把旧发布流程重新引入。