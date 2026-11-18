# 场景（scenario）改名 workflow：让可执行性成为名称与契约

## 问题

`scenario` 这个名字把重心放在“情形、用例”，不承载“可执行”。文档自己的定义也是描述性的：
改名前 `docs/readme/scenarios.md:1-6` 写“场景能力把一类任务的执行流程写成可检索、可加载的
文件”，并说“场景描述‘怎么做’，不描述‘是什么’”；`skills/llm-wiki/scripts/scenarios.py:3`
（现 `workflows.py`）也只写 “A scenario is shaped like a skill”。名称没有回答“这段流程
agent 能不能真的跑”。

这不是局部命名：定案前全仓有 176 处代码引用、约 300 处文档引用，分布在目录、文件名、CLI、
观看区、健康检查和测试里。

由此带来三个现象：

- agent 读到 `scenario`，容易当成参考文档，而不是一段要照着执行的能力；是否可执行全靠
  正文和读取者判断。
- `process` 页同样描述“怎么做”，两者的边界只能靠文档解释；名称不提供区分。
- 改名前的校验只检查目录、`SCENARIO.md`、frontmatter 与正文非空，不检查步骤是否落到工具
  或明确的人工步骤。

用户提出改名为 `workflow`，强调 “can work and flow, not just flow”。改名会触及项目目录
`scenario/`（含 legacy `场景能力/`）、主文件名 `SCENARIO.md`、CLI `scenario list/search/load`、
内置 `skills/llm-wiki/scenarios/`、观看区 `scenarios/`、`ingest` 跳过逻辑、文档与
`tests/selftest.py` 断言。

仓库已有一次同类迁移：`场景能力/` 改成 `scenario/` 时用了 `SCENARIO_DIRS` 多目录回退
（`docs/agent/development-decisions.md` 的 2026-11-16 条目），这次复用了同一套兼容思路。
账本不存场景状态，改名只动文件、配置与文档，不需要 schema 迁移。

## 影响

- 只改名不改契约：只是换标签，仍可能留下“只有流程、不能执行”的 workflow。
- 直接硬切：现有项目的 `scenario/` 立刻失效；需要兼容读取或迁移窗口。
- 名称与 `process` 的中文“流程”更近，若不写死可执行性，容易把刚分开的两个概念混回去。
- 改名本身有迁移成本：目录、命令、观看区、文档与测试都要跟。

## 相关文件

定案并落地后的位置（改名前的位置见本文 Git 历史）：

- `skills/llm-wiki/scripts/workflows.py`：目录常量、双读、`parse_steps`、`_contract`、`load`
- `skills/llm-wiki/scripts/cli.py`：`command_workflow` 与 `workflow list/search/load` 注册
- `skills/llm-wiki/scripts/view.py`：观看区 `workflows/` 投放与旧 `scenarios/` 标注
- `skills/llm-wiki/scripts/health.py`：`SCRIPT_FILES` 与 `BUILTIN_WORKFLOWS`
- `skills/llm-wiki/scripts/ledger.py`：`WORKFLOW_DIRS` 跳过、`publish.workflows` 与旧键迁移
- `skills/llm-wiki/scripts/frontmatter.py`、`search.py`：注释里的旧术语
- `skills/llm-wiki/SKILL.md`、`skills/llm-wiki/workflows/topic-exploration/WORKFLOW.md`
- `workflow/ppt_creator/WORKFLOW.md`
- `docs/readme/workflows.md`（原 `scenarios.md`）、`cli.md`、`design-principles.md`、
  `topics.md`、`recovery.md`、`skills/llm-wiki/references/recovery.md`
- `docs/agent/development-decisions.md`、`engine.md`、`maintenance.md`、`verification.md`
- `README.md`、`AGENTS.md`、`tests/selftest.py`

## 定案（2026-11-18）

五个待确认项逐项定案，整体走 B 方案，但去掉弃用窗口，改为命令硬切 + 目录双读：

1. **名称与命令。** 正式名 `workflow`，中文“可执行工作流”；首次出现写定义句，其后简称
   “工作流”；普通词“适用场景/使用场景”不替换。`scenario` 命令不保留别名、不输出弃用提示，
   只留 `workflow list/search/load`。目录仍三向读取：`workflow/` > `scenario/` >
   `场景能力/`，`WORKFLOW.md` 优先、缺失回退 `SCENARIO.md`。理由：概念清晰度靠命令和文档，
   而每个工作区已有的 `scenario/` 内容不能因为命令改名就消失；双读让旧内容零迁移继续可用。
2. **步骤契约。** 四类标记 `tool:` / `human:` / `agent:` / `verify:`，每步必标；只解析第一个
   `## 步骤` 到下一个二级标题之间的顶层列表项，代码块、缩进子项和其他节不算；规范写全小写
   半角冒号，解析接受全角冒号与大小写，输出统一小写。缺标记给 warning；有标记但没有
   `tool:` 判 invalid，next 指向 `process` 页；整份零标记在 0.5.0 仍合法但给 warning。
   `load` 返回 `tools`、`human_steps`、`agent_steps`、`verify_steps`、`warnings`。
   issue 原设想的 tool/human 二分装不下“判断收敛”“组论点”这类纯判断步骤，故加 `agent:`。
3. **旧内容处理。** 0.6.0 起零标记判 invalid，不是不设截止。仓库内两份（内置
   `topic-exploration`、`workflow/ppt_creator`）随本次改动补标记；外部工作区的旧文件在
   0.5.0 保持 valid + warning，文案写明“0.6.0 起将判无效”。代码用 `UNTYPED_ALLOWED`
   控制，0.6.0 发布时翻转并去掉 warning 分支。
4. **观看区。** 新副本投放 `workflows/`；旧 `scenarios/` 不搬不删，README 标注“已停止同步，
   可删除或手动并入 workflows/”；`publish.workflows` 为假只是不投放，不删旧目录；新投放只
   复制项目目录，不搬旧观看区的手改内容。
5. **版本窗口。** 0.5.0 一次装完：命令硬切、三目录双读、四类契约、内置 `workflows/`、仓库
   自身 `workflow/ppt_creator`、观看区 `workflows/`、`publish.workflows`、文档与 selftest。
   0.6.0 只收紧零标记；双读长期保留，不再为旧目录设移除窗口。

与 B 方案的差异：不保留 `scenario` 别名与弃用期；不设 0.4.x 过渡版本，直接把版本抬到
0.5.0；0.6.0 不删除旧目录与旧文件，只收紧零标记。

## 待确认与验收条件

### 待确认

- [x] 正式名称与中文写法：`workflow` / “可执行工作流”；不保留 `scenario` 别名，无弃用提示。
- [x] 步骤标记语法：`tool:`/`human:`/`agent:`/`verify:`，只认第一个 `## 步骤` 的顶层列表项；
      缺标记 warning，无 `tool:` invalid，强制至少一个 `tool:`。
- [x] 旧内容处理：仓库内两份本次补标记；外部旧文件 warning 到 0.6.0，之后 invalid。
- [x] 观看区：新投放 `workflows/`，旧 `scenarios/` 保留并标注，不一次性搬运。
- [x] 版本窗口：0.5.0 一次装完，0.6.0 只收紧零标记，双读长期保留。

### 验收条件

- [x] 双读优先级测试：`workflow/` > `scenario/` > `场景能力/`；`WORKFLOW.md` 缺失回退
      `SCENARIO.md`；同名 id 只列一次（selftest 的 `workflow_files` 夹具与 roots 断言）。
- [x] `workflow list/search/load` 可用；`scenario` 命令与别名已移除（无 deprecated 输出）。
- [x] `workflow list` 对缺标记的条目给出 warning；`workflow load` 返回 `tools`、
      `human_steps`、`agent_steps`、`verify_steps`。
- [x] 纯人工流程判 invalid，`next` 指向 `process` 页（`E_WORKFLOW_INVALID` 的 hint）。
- [x] 内置 `topic-exploration` 从 `workflows/` 加载，`source=builtin`；健康检查路径更新。
- [x] 观看区 seeding 到 `workflows/`，旧 `scenarios/` 不被删除；README 同时说明新旧目录。
- [x] `ingest` 跳过 `workflow/`、`scenario/`、`场景能力/`；`query` 不混入 workflow 结果。
- [x] `publish.workflows` 生效，`publish.scenarios` 兼容；配置测试覆盖默认值与旧配置。
- [x] 文档同步：`README.md`、`AGENTS.md`、`docs/readme/workflows.md`、`cli.md`、
      `design-principles.md`、`topics.md`、`engine.md`、`verification.md`、`maintenance.md`；
      `development-decisions.md` 记录本次改名与兼容窗口。
- [x] `tests/selftest.py` 覆盖以上并全绿；`health.py` 的技能文件清单同步。
