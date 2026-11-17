# 场景（scenario）改名 workflow：让可执行性成为名称与契约

## 问题

`scenario` 这个名字把重心放在“情形、用例”，不承载“可执行”。文档自己的定义也是描述性的：
`docs/readme/scenarios.md:1-6` 写“场景能力把一类任务的执行流程写成可检索、可加载的文件”，
并说“场景描述‘怎么做’，不描述‘是什么’”；`skills/llm-wiki/scripts/scenarios.py:3` 也只写
“A scenario is shaped like a skill”。名称没有回答“这段流程 agent 能不能真的跑”。

这不是局部命名：全仓有 176 处代码引用、约 300 处文档引用，分布在目录、文件名、CLI、观看区、
健康检查和测试里。

由此带来三个现象：

- agent 读到 `scenario`，容易当成参考文档，而不是一段要照着执行的能力；是否可执行全靠
  正文和读取者判断。
- `process` 页同样描述“怎么做”，两者的边界只能靠文档解释；名称不提供区分。
- 现有校验只检查目录、`SCENARIO.md`、frontmatter 与正文非空（`scenarios.py:205-248`），
  不检查步骤是否落到工具或明确的人工步骤。

用户提出改名为 `workflow`，强调 “can work and flow, not just flow”。改名会触及项目目录
`scenario/`（含 legacy `场景能力/`）、主文件名 `SCENARIO.md`、CLI `scenario list/search/load`
（`cli.py:1197-1204`）、内置 `skills/llm-wiki/scenarios/`、观看区 `scenarios/`、`ingest`
跳过逻辑、文档与 `tests/selftest.py` 断言。

仓库已有一次同类迁移：`场景能力/` 改成 `scenario/` 时用了 `SCENARIO_DIRS` 多目录回退
（`scenarios.py:33-40`，`docs/agent/development-decisions.md:173-186`），这次可以复用同一套
兼容思路。账本不存场景状态，改名只动文件、配置与文档，不需要 schema 迁移。

## 影响

- 只改名不改契约：只是换标签，仍可能留下“只有流程、不能执行”的 workflow。
- 直接硬切：现有项目的 `scenario/` 立刻失效；需要兼容读取或迁移窗口。
- 名称与 `process` 的中文“流程”更近，若不写死可执行性，容易把刚分开的两个概念混回去。
- 改名本身有迁移成本：0.4.x 双读、0.5.0 切换、旧名至少保留一个版本；文档、测试、观看区
  都要跟。

## 相关文件

- `skills/llm-wiki/scripts/scenarios.py:1-40`、`:205-248`（目录常量、扫描与校验）
- `skills/llm-wiki/scripts/cli.py:904-944`、`:1197-1204`（`scenario list/search/load`）
- `skills/llm-wiki/scripts/view.py:28-30`、`:138-139`、`:236-245`（观看区 `scenarios/`）
- `skills/llm-wiki/scripts/health.py:46`、`:213-219`（内置场景检查）
- `skills/llm-wiki/scripts/ledger.py:220`、`:258`、`:387`（`publish.scenarios`、跳过目录）
- `skills/llm-wiki/scripts/frontmatter.py:1-4`、`skills/llm-wiki/scripts/search.py:5`（注释与共用解析）
- `skills/llm-wiki/SKILL.md`、`skills/llm-wiki/scenarios/topic-exploration/SCENARIO.md`
- `docs/readme/scenarios.md`（全篇）、`docs/readme/cli.md`、`docs/readme/design-principles.md`、
  `docs/readme/topics.md`、`docs/readme/recovery.md`、`skills/llm-wiki/references/recovery.md`
- `docs/agent/development-decisions.md:104-120`、`:173-186`、`docs/agent/engine.md`、
  `docs/agent/maintenance.md`、`docs/agent/verification.md`
- `README.md`、`AGENTS.md`、`tests/selftest.py`（scenario 命令与观看区断言）

## 候选方案（未定案）

### A. 只改术语层

文档、README、AGENTS 统一叫“可执行工作流”，代码标识符、目录、命令仍用 `scenario`。

- 成本最低，不破坏任何现有项目。
- 代码与文档术语分裂：agent 在命令与目录里看到的还是 `scenario`，名称对行为的提示有限。
- 适合作为过渡，不单独采用。

### B. 完整改名 + 双读兼容 + 可执行契约（推荐）

**命名与兼容**

- 规范名：项目目录 `workflow/`，主文件 `WORKFLOW.md`，命令 `workflow list/search/load`；
  内置目录 `skills/llm-wiki/workflows/`，观看区 `workflows/`，配置 `publish.workflows`。
- 兼容读取：`scenario/`、`场景能力/` 仍可读；`WORKFLOW.md` 缺失时读 `SCENARIO.md`。
  优先级 `workflow/` > `scenario/` > `场景能力/`，同名 id 只列优先级最高的一份。
- `scenario` 命令保留一个版本作为别名，行为与 `workflow` 一致，帮助文本标 deprecated。
- 内置目录直接改名（技能自带，没有外部契约）；观看区新投放 `workflows/`，旧 `scenarios/`
  不删除，README 说明旧目录可手动迁移。
- `ingest` 继续跳过新旧目录；账本无 schema 迁移。

**可执行契约**

- 步骤标 `tool:` 或 `human:`（可选 `verify:`），至少有一个 `tool:` 步骤；纯人工流程不建
  workflow，留在 process 页。
- `workflow list` 对缺标记的条目返回 warning（旧内容先 grandfather），对没有任何 `tool:`
  步骤的条目判 invalid；`workflow load` 额外返回 `tools` 与 `human_steps`。
- 定义句写进 `docs/readme/workflows.md`、`SKILL.md` 与 `curation.md`：workflow 是 agent 能
  实际执行的工作流；只描述流程、不能执行的是 process 页。

**节奏**

- 0.4.x：双读 + 新命令 + 仅 warning，不破坏旧内容。
- 0.5.0：规范名与文档切到 workflow，`scenario` 标 deprecated；旧内容仍可读。
- 0.6.0：移除旧目录、旧文件与 `scenario` 别名。

成本中等偏高：176 处代码引用、约 300 处文档引用、`selftest.py` 大量断言要同步。

### C. 0.5.0 硬切

一次性改名，不保留 `scenario/`、`SCENARIO.md` 与旧命令；提供迁移手册让用户手动改目录。

- 代码最干净，没有双读分支与弃用窗口。
- 现有项目升级即失效，必须逐个改目录；对已经建了场景的工作区不友好。

### D. 不改名，只补可执行契约

保留 `scenario`，只加步骤标记、校验与 `list` 的 warning。

- 成本最低，能解决“只有流程、不能执行”的问题。
- 名称仍然弱化可执行性，agent 的默认预期不变；如果改名被否，可以作为退路。

推荐 B；如果希望代码更简单且能接受迁移，选 C；D 只作为不改名时的退路。以上均未定案。

## 待确认与验收条件

### 待确认

- [ ] 正式名称与中文写法：`workflow` / “可执行工作流”；是否保留 `scenario` 别名，保留到
      哪个版本，是否在 `list` 里显示弃用提示。
- [ ] 步骤标记语法：`tool:` / `human:` / `verify:` 的行内形式与解析规则；缺标记是 warning
      还是 invalid；是否强制至少一个 `tool:`。
- [ ] 旧内容处理：内置 `topic-exploration` 与用户已有 `SCENARIO.md` 是否立即补标记，还是
      grandfather 到 0.5.0。
- [ ] 观看区：新投放 `workflows/` 后，旧 `scenarios/` 保留、提示迁移，还是提供一次性搬运。
- [ ] 版本窗口：0.4.x 双读、0.5.0 切换、0.6.0 移除，是否符合发布节奏。

### 验收条件

- [ ] 双读优先级测试：`workflow/` > `scenario/` > `场景能力/`；`WORKFLOW.md` 缺失回退
      `SCENARIO.md`；同名 id 只列一次。
- [ ] `workflow list/search/load` 与 `scenario` 别名输出一致；别名标 deprecated。
- [ ] `workflow list` 对缺 `tool:`/`human:` 标记的条目给出 warning；`workflow load` 返回
      `tools` 与 `human_steps`。
- [ ] 纯人工流程判 invalid，并给出“改用 process 页”的 next。
- [ ] 内置 `topic-exploration` 从 `workflows/` 加载，`source=builtin`；健康检查路径更新。
- [ ] 观看区 seeding 到 `workflows/`，旧 `scenarios/` 不被删除；README 同时说明新旧目录。
- [ ] `ingest` 跳过 `workflow/`、`scenario/`、`场景能力/`；`query` 不混入 workflow 结果。
- [ ] `publish.workflows` 生效，`publish.scenarios` 兼容；配置测试覆盖默认值与旧配置。
- [ ] 文档同步：`README.md`、`AGENTS.md`、`docs/readme/workflows.md`、`cli.md`、
      `design-principles.md`、`topics.md`、`engine.md`、`verification.md`、`maintenance.md`；
      `development-decisions.md` 记录本次改名与兼容窗口。
- [ ] `tests/selftest.py` 覆盖以上并全绿；`health.py` 的技能文件清单同步。
