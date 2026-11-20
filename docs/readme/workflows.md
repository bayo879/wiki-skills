# 可执行工作流

可执行工作流把一类任务的执行流程写成可检索、可加载的文件，放在观看区
`<view>/workflows/<id>/` 下（`publish.dir`，默认 `wiki/`）。它借用 skill 的组织方式：
一个主流程文件，加上按需读取的脚本和说明。和知识页不同，工作流描述“怎么做”，而且每一步
都要标明由谁执行；它不进入账本，也不参与页面引用。

工作流定义属于知识库内容，和知识页、话题一起管理。旧根目录 `<root>/workflows/` 只作为
一个发布版本的兼容兜底：观看区优先，旧根只读；迁移见下文「迁移旧目录」。

技能自带三个内置工作流：`topic-exploration`（回答知识问题时的默认流程）、`workflow-review`
（评审一个工作流）、`workflow-create`（新建工作流，建前先过评审前提）；项目放同名工作流
即可覆盖它。

## 目录结构

```text
<view>/workflows/
└── intake-backlog/
    ├── WORKFLOW.md         主流程，必需
    ├── scripts/
    │   └── drain_queue.sh
    └── references/
        └── error-codes.md
```

- 观看区 `<view>/workflows/` 只扫描一级子目录，每个子目录是一个工作流；隐藏目录和普通
  文件忽略。旧根 `<root>/workflows/` 只有在观看区没有同名工作流时才参与。
- 工作流 id 就是子目录名，用英文与下划线，不要把中文写进路径。
- `WORKFLOW.md` 是唯一入口，文件名固定；缺它、缺 `description`、正文为空或步骤契约不满足
  都视为不合法。
- `scripts/` 放确定性操作的脚本，`references/` 放执行时才需要读的细节。
- 其他文件也会列进资源清单，`kind` 为 `other`；它们不会自动执行或加载。

## 内置工作流

`skills/llm-wiki/workflows/` 下的工作流随技能安装，不需要项目文件：

```text
skills/llm-wiki/workflows/
├── topic-exploration/     定主题、多轮检索、总结、按需沉淀
├── workflow-review/       按七条维度评审一个工作流
└── workflow-create/       先过评审前提，再起草工作流并自审
```

- `workflow list` 把项目工作流和内置工作流列在一起，每项带 `source`（`project` 或
  `builtin`）和 `origin`（`view`、`legacy` 或 `builtin`）。同 id 时观看区优先，旧根其次，
  内置最后，只列一次。
- `workflow load "<内置 id>"` 返回内置流程；它不给根目录相对的 `path`/`main`，而是给
  `skill_path`（技能目录相对，形如 `workflows/<id>/WORKFLOW.md`），`resources` 恒为空
  （内置流程自包含）。项目工作流反过来只给 `path`（目录）与 `main`（主文件），两者都以
  工作区根目录为基准；`origin` 是 `view` 时来自观看区，是 `legacy` 时来自旧根兜底。
- 同 id 的观看区版与旧根版 `WORKFLOW.md` 不同时，条目带 `legacy_conflict: true` 和
  `legacy_path`；`workflow list` 打印警告，加载仍用观看区版。
- 内置工作流也会进入 `query` 候选和显式 `workflow search`，`source` 为 `builtin`。项目覆盖后
  `source` 变为 `project`，`origin` 变为 `view` 或 `legacy`。
- 内置工作流只读，改流程请复制到 `<view>/workflows/<id>/` 再改，不要改技能安装目录。新增或
  改动内置工作流要同步 `skills/llm-wiki/scripts/health.py` 的 `BUILTIN_WORKFLOWS`，否则
  `doctor` 不检查它。

## WORKFLOW.md

```markdown
---
name: intake-backlog
alias_name: 入库清积压
description: 把积压的来源跑完 route 与 page 任务，直到没有待处理来源
tags:
  - 维护
  - 入库
---

# 入库清积压

## 适用

`status` 显示 `pending` 非空，或有 route/page 任务排队时使用；空库先 `init`。

## 步骤

1. tool: **看积压**：`status --json`，记下 `pending` 条数与 `tasks.open`，`next` 的提示决定下一步。
2. tool: **入库**：有新文件就跑 `ingest --json`；`failed` 与 `oversize` 非空时先报给用户。
3. tool: **领任务**：`next --kind route --json` 一次领一条，按任务目录里的 `brief.md`、`work.json`
   执行；定案前先用 `query --task <id> 词...` 登记检索。
4. human: **确认归属**：页面归属或授权拿不准时停下来问用户，不要替用户决定。
5. tool: **提交**：把结果写进 `.wiki/tasks/<id>/payload.json`，运行 `submit <id> --json`；
   被拒就按 `error.next` 修正后重交，不要 `release` 掉重来。
6. verify: **复核**：`status --json` 的 `pending` 与 `tasks.open` 归零，`stale_pages` 里没有这次动过的页。

## 判断点

- `E_SOURCE_DRIFT`：raw 在排队期间被改过，重新读来源后重做这条 decision。
- `E_TASK_UNSUBMITTED`：手头还有领了没交的任务，填完再 `next`，不要并发领。
- `E_INGEST_BUSY`：别的会话正在摄入，立刻停手，不要重试、不要派子智能体并行。
- `failed` 里有解析失败的来源：不猜内容，报给用户等修。

## 完成标准

- 没有 pending 来源、没有未关闭的任务；
- 每条新事实都有逐字引文；
- 这次没动过的页面 `status` 仍是 `stable`。
```

frontmatter 字段：

| 字段 | 必需 | 说明 |
| --- | --- | --- |
| `name` | 否 | 机器名（英文），`workflow list` 与检索按它展示；缺省用目录名 |
| `alias_name` | 否 | 中文展示名，只用于 `workflow list` 的显示 |
| `description` | 是 | 一句话说明工作流解决什么问题，参与检索与展示 |
| `tags` | 否 | Obsidian 风格的标签列表：不带 `#`、不能含空格（空白与 `,`、`、`、`;` 都当分隔符），可用 `a/b` 表示层级；`#` 前缀会被去掉，纯数字标签会被丢弃 |

正文写给执行者看，重点写清顺序、判断点、完成标准和失败时怎么处理。需要事实依据的地方，
指向知识页或原始文件，而不是把事实抄进工作流。`WORKFLOW.md` 超过 200000 字符会被判为不合法；
主流程保持精简，细节放 `references/`。

## 步骤契约

只有第一个 `## 步骤` 二级标题到下一个二级标题之间的**顶层**列表项算步骤：缩进子项、
代码块里的内容和别的节里的列表都不算。每一步紧跟在列表标记后写类型，规范写法是全小写
半角冒号；解析接受全角冒号和大小写变体，输出统一小写。

| 标记 | 谁执行 | 怎么写 |
| --- | --- | --- |
| `tool:` | agent | 调命令、脚本或另一个技能；正文写清命令与参数 |
| `human:` | 人 | 需要人做或人确认；agent 停下来问，不替用户决定 |
| `agent:` | agent | 由 agent 判断或写作，不需要外部动作 |
| `verify:` | agent | 验证前一步结果；可选，通常放最后 |

判定规则：

- 每一步都必须有类型。个别步骤没标给 warning；整份一个标记都没有时仍可加载，但给
  warning，`0.6.0` 起将判为不合法。
- 至少要有一步 `tool:`。只由 `human:` / `agent:` 组成、没有可调用工具的流程是纯人工
  流程，应写成知识页里的 `process`，`workflow load` 会判它不合法并指向 `process`。
- 没有 `## 步骤` 一节、或这一节没有列表项，0.5.0 给 warning，`0.6.0` 起判为不合法。
- 标记后没有内容给 warning。
- `workflow list` 每项带 `tools` / `human_steps` 等计数与 `warnings`；`workflow load`
  返回 `tools`、`human_steps`、`agent_steps`、`verify_steps`（每项 `{line, text}`）和
  `warnings`，调用方按类型执行，不需要自己再解析正文。

## 脚本与说明

`scripts/` 里的脚本应只做确定性、可重复的操作，并在文件头或同目录 README 里写清用途、
参数、输入输出和失败行为。执行前先确认它做什么；需要外部环境、凭据或联网时，在工作流正文
里显式说明，不要假设执行者会盲跑。

`references/` 放长说明、字段表、检查清单等。主流程只写“何时读哪个文件”，细节放进去，
避免 `WORKFLOW.md` 膨胀。

## 任务产物

工作流执行时若需要保存中间结果和最终交付物，统一落在观看区的 `runs/` 下，不和工作流
定义混放：

```text
<view>/runs/workflows/<工作流 id>/<任务名>/
├── cache/    中间结果：检索命令与命中、原文摘录、拆解草稿
└── output/   最终交付给用户的那份文件
```

- `view` 是观看区目录（`publish.dir`，默认 `wiki/`）；任务名按主题起，如
  `2026-11-19-周会`。同一主题再次执行另起任务名，不覆盖上一轮产物。
- 定义放 `<view>/workflows/<id>/`，产物放 `<view>/runs/workflows/<id>/`，两者分层，任务目录
  不会挡住定义。
- 观看区整目录在 `scan_files` 的扫描根就被跳过（`skills/llm-wiki/scripts/ledger.py:394-404`），
  所以这里建目录、写文件都不会排队来源，也不会卡住 `next`。
- 不落根目录：根目录非隐藏文件会被 `ingest` 当来源排队，未提交任务以
  `E_TASK_UNSUBMITTED` 卡住后续 `next`（`skills/llm-wiki/scripts/cli_workspace.py:142-149`）。
- `runs/` 属于项目内容，引擎不覆盖、不删除；默认建议不提交，重要交付物由用户自己留底。

## 迁移旧目录

旧库可能同时有 `<root>/workflows/<id>/` 和 `<view>/workflows/<id>/`。引擎不改动旧根目录，
只按观看区优先读取；同 id 两边不同时给 `legacy_conflict` 警告。迁移由用户手动做：

1. 逐个比较 `<view>/workflows/<id>/WORKFLOW.md` 与 `<root>/workflows/<id>/WORKFLOW.md`，
   确认哪边是当前版本；观看区版已经生效。
2. 把旧根独有的工作流移到 `<view>/workflows/`；旧根里已被观看区覆盖的版本不要覆盖回去。
3. 确认无引用后删除 `<root>/workflows/`。删除前先备份；旧根定义不会再被加载。
4. 把旧任务目录 `<view>/workflows/<id>/<任务>/` 移到
   `<view>/runs/workflows/<id>/<任务>/`，保留 `cache/` 与 `output/`。
5. 用户库若要提交观看区，自行在 `.gitignore` 里放行 `<view>/workflows/`、`<view>/topics/`，
   继续忽略 `pages/`、`raw/`、`runs/`、`README.md`；不改本仓库的 `.gitignore`。

`publish.dir` 改名不会自动搬迁任何定义或产物；改名等于换了一个观看区，旧目录会变成普通
来源。需要保留工作时先手动移动，再改配置。

## 检索与加载

工作流不经过 `ingest`，观看区与旧根 `workflows/` 在摄入时整目录跳过，因此改流程不需要重建知识库。
普通 `query` 会在页面、话题之外带回最多 5 个工作流候选（每条带 `applies`），但不自动加载；
要按流程办事时再看候选或显式检索：

```bash
python3 <skill>/scripts/wiki.py query "入库" --json
python3 <skill>/scripts/wiki.py workflow search "入库" --json
python3 <skill>/scripts/wiki.py workflow list --json
python3 <skill>/scripts/wiki.py workflow load intake-backlog --json
python3 <skill>/scripts/wiki.py workflow load "topic-exploration" --json
```

`workflow search` 返回命中的项目工作流与内置工作流，按分数排序，最多 5 个；候选带
`applies`（`## 适用` 原文），0 命中时 data 带 `message: "无可用workflow"`。`workflow load`
返回 `content`（`WORKFLOW.md` 全文）、`meta`、`path`、`resources` 和分好类型的步骤。
按 `tools` 调工具、按 `human_steps` 停下来问人，需要细节时再读 `resources` 里的文件。
结构不合法的工作流不会出现在 `workflow search` 结果里，但会出现在 `workflow list` 中并带
`error`，方便修正。

`wiki:` 前缀入口也走这里：需求由 `SKILL.md` 的路由表落到具体工作流，仍必须 `workflow load`
后才按步骤执行。`workflow:` 前缀是独占入口，只跑 `workflow search`/`load`，不查页面和话题；
0 命中或候选都不符合 `applies` 时回复“无可用workflow”。路由表只在 `SKILL.md` 定义一处，
本文不复制。

话题探索流程依次做：定主题、找现成能力、找已有话题、多轮查页面、补原始依据、判断收敛、
总结，最后问用户是否沉淀到 `<view>/topics/<主题>.md`。`query` 的 `topics` 是已知话题，相关的先读
再探索，避免重复劳动；话题格式见 [话题沉淀](topics.md)。

## 边界

- 工作流是能力，不是知识页：不要把工作流当来源摄入，也不要在页面里引用工作流文件。
- 工作流步骤与知识页冲突时，流程以 `WORKFLOW.md` 为准；事实依据仍以页面和 raw 为准。
- 工作流文件是活文件，直接编辑即可生效；不要在 `.wiki/` 里另存副本。
- 工作流只收 agent 真能执行的方法：每一步都要标类型，`tool:` 要落到具体命令或脚本上；
  只能由经办人完成的环节标 `human:`，不要包装成 agent 步骤。
- 没有系统权限、只能由经办人完成的流程（开户、审批、现场检查这类）不是工作流：它们属于
  知识页里的 `process` 知识，按页引用；纯人工流程也没有 `tool:` 步骤，会被判为不合法。
- `workflow list` / `workflow load` 只读，不要求先 `init`，也不会创建 `.wiki/`。
- 话题探索的沉淀物是 `<view>/topics/`，不是工作流；只有流程稳定、可重复执行时才另建工作流。
- 观看区的 `workflows/`、`topics/`、`runs/` 属于项目内容，引擎不覆盖、不删除；`<view>/`
  与 `raw/`、`workflows/`、`topics/` 在任何配置下都会创建，`pages/` 推送时创建，`runs/`
  首次写产物时创建。

完整命令字段与错误码见 [CLI 契约](cli.md#workflow)。
