# P2：多个脚本与自测文件超出单文件上限，需要按职责拆分

## 问题

用户全局指导（2026-11-20 更新）新增硬约束：**代码与配置单文件上限 1000 行 / 2 万字符，
Markdown 文档 1000 行 / 1.2 万字符**。本仓库现有文件已超出，且集中在改引擎与测试时最常触及的几个文件：

| 文件 | 行数 | 字符数 | 超限维度 |
| --- | --- | --- | --- |
| `skills/llm-wiki/scripts/cli.py` | 1741 | 72762 | 行数 + 字符数 |
| `skills/llm-wiki/scripts/ledger.py` | 810 | 32203 | 字符数 |
| `skills/llm-wiki/scripts/task_store.py` | 631 | 26910 | 字符数 |
| `skills/llm-wiki/scripts/workflows.py` | 627 | 20990 | 字符数（刚超出） |
| `tests/selftest.py` | 2916 | 163602 | 行数 + 字符数 |

证据（基线 12188b9）：

```text
$ wc -l -m skills/llm-wiki/scripts/cli.py skills/llm-wiki/scripts/ledger.py \
      skills/llm-wiki/scripts/task_store.py skills/llm-wiki/scripts/workflows.py tests/selftest.py
    1741   72762 skills/llm-wiki/scripts/cli.py
     810   32203 skills/llm-wiki/scripts/ledger.py
     631   26910 skills/llm-wiki/scripts/task_store.py
     627   20990 skills/llm-wiki/scripts/workflows.py
    2916  163602 tests/selftest.py
```

`cli.py` 里共有 16 个 `command_*` 入口函数，共用一套 `ARGS` / `emit` / `open_wiki` 约定；
`tests/selftest.py` 里的 suite 已按主题命名（`repair_flow`、`links_flow`、`topics_view_checks`
等），但全部堆在同一个文件里，`main` 逐条登记。

Markdown 文档同样超限（按 1.2 万字符口径，行数均未超 1000）：

| 文档 | 行数 | 字符数 |
| --- | --- | --- |
| `docs/agent/development-decisions.md` | 590 | 44753 |
| `docs/readme/cli.md` | 442 | 31524 |
| `docs/agent/verification.md` | 367 | 22671 |
| `docs/agent/engine.md` | 247 | 22367 |
| `skills/llm-wiki/SKILL.md` | 331 | 21627 |
| `docs/readme/workflows.md` | 234 | 13753 |
| `workflows/personal-style-writing/WORKFLOW.md` | 150 | 12593 |

## 影响

- 单次任务的全文件读取成本高，agent 与人都难定位；改动时容易在无关段落上产生冲突。
- 与用户的全局硬约束不一致，后续每次改引擎/测试都会继续推高行数，越晚拆越贵。
- 不影响当前运行行为：现有自测全绿，功能无缺陷。

## 相关文件

- `skills/llm-wiki/scripts/cli.py`（1741 行；16 个 `command_*`；`ARGS` / `emit` / `open_wiki` 共享约定）
- `skills/llm-wiki/scripts/ledger.py`（810 行；schema、迁移、config、state 混在一起）
- `skills/llm-wiki/scripts/task_store.py`（631 行；任务状态机与存储）
- `skills/llm-wiki/scripts/workflows.py`（627 行；刚超 2 万字符）
- `tests/selftest.py`（2916 行；全部 suite 与 `main` 登记）
- `skills/llm-wiki/scripts/health.py:24-38`（`SCRIPT_FILES` 白名单，新增脚本文件必须同步）
- `docs/agent/engine.md`（单文件规模边界的既有约定，需随拆分更新）

## 候选方案（未定案）

- **A：先拆 `tests/selftest.py`。** 按主题拆到 `tests/` 下的多个模块（账本、页面与链接、话题与工作流、
  观看区渲染），保留一个薄 `tests/selftest.py` 作为入口逐个导入并汇总 `check`。收益最大（2916 行），
  与产品代码解耦，回归风险低。
- **B：拆 `cli.py` 的命令入口。** 把 16 个 `command_*` 按主题分到若干模块（query / task / page /
  workflow / ingest），`cli.py` 只留参数解析与分发。需要同步 `health.py` 的 `SCRIPT_FILES` 与
  `engine.md` 的模块地图；共享的 `ARGS` / `emit` / `open_wiki` 要放进独立的小模块，避免循环导入。
- **C：拆 `ledger.py` 与 `task_store.py`。** 按"schema 与迁移 / config / 状态读写 / 任务状态机"分文件。
  最贴近数据层，风险集中在事务与迁移路径，建议在 A 之后做。
- **D：拆超限 Markdown。** `docs/agent/development-decisions.md` 按决策主题分页并留索引；
  `docs/readme/cli.md` 按命令族拆；`SKILL.md` 只留索引，细则移入 `references/`。
  文档拆分不改行为，可与代码拆分并行。
- 各方案都按"一次一个文件、每步跑 `tests/selftest.py`"推进，不合并成一次大改。

## 待确认与验收条件

- [ ] 决定拆分顺序（建议 A → B → C）与是否先做 A
- [ ] 拆分后每个文件都在上限以内（代码 1000 行 / 2 万字符；Markdown 1000 行 / 1.2 万字符），且职责可用一句话说明
- [x] `tests/selftest.py` 仍可一条命令跑全量，输出与现在一致（`N/N checks passed`）
- [x] 新增脚本文件同步到 `health.py` 的 `SCRIPT_FILES`，`health` 自测不报缺失
- [x] `docs/agent/engine.md` 的模块地图与 `cli.py` 入口约定同步更新
- [ ] 每次拆分单独提交，提交信息说明拆分边界，不改运行行为

## 进展

- 2026-11-20：方案 A 已完成。`tests/selftest.py` 从 3187 行 / 179195 字符拆成薄入口
  （120 行 / 4621 字符）+ `support.py`、`fixtures.py` 与 18 个 `checks_*.py`；最大单文件
  300 行 / 15588 字符。`python3 tests/selftest.py` 仍是一条命令跑全量，368/368 通过、退出码 0，
  输出与拆分前逐字节一致（`--json` 汇总同样不变）。
- 2026-11-20：方案 B 已完成。`cli.py` 从 1872 行 / 77913 字符拆成入口（164 行 / 7812 字符）
  + `cli_common.py`（156 行 / 5251 字符）+ 6 个按命令族分的 `cli_*.py`，最大
  `cli_submit.py` 356 行 / 15692 字符。分两步提交：先抽共用层（`da37e74`），再按族搬命令。
  `python3 tests/selftest.py` 368/368、输出与拆分前逐字节一致；`ARGS` 改成共享
  `Namespace` 对象，`main` 只更新属性，78 处使用点与所有命令输出不变。
- 2026-11-20：C（`ledger.py` / `task_store.py`）、D（超限 Markdown）未开工；
  这些文件当前体积见上方表格，C 是下一个候选。
