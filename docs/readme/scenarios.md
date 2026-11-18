# 场景能力

场景能力把一类任务的执行流程写成可检索、可加载的文件，放在项目根目录的
`scenario/` 下。它借用 skill 的组织方式：一个主流程文件，加上按需读取的脚本和说明。
和知识页不同，场景描述“怎么做”，不描述“是什么”；它不进入账本，也不参与页面引用。

旧工作区使用的 `场景能力/` 仍然有效：两个目录都会被扫描，同一 id 同时存在时
`scenario/` 优先，避免改过的英文目录被遗留的中文目录盖住。

技能另带一个内置场景 `topic-exploration`，是回答知识问题时的默认流程；项目放同名场景即可覆盖它。

## 目录结构

```text
<root>/scenario/
└── intake-backlog/
    ├── SCENARIO.md         主流程，必需
    ├── scripts/
    │   └── drain_queue.sh
    └── references/
        └── error-codes.md
```

- `scenario/` 与 `场景能力/` 都只扫描一级子目录，每个子目录是一个场景；隐藏目录和普通文件忽略。
- 场景 id 就是子目录名，用英文与下划线，不要把中文写进路径。
- `SCENARIO.md` 是唯一入口，文件名固定；缺它、缺 `description` 或正文为空都视为不合法。
- `scripts/` 放确定性操作的脚本，`references/` 放执行时才需要读的细节。
- 其他文件也会列进资源清单，`kind` 为 `other`；它们不会自动执行或加载。

## 内置场景

`skills/llm-wiki/scenarios/` 下的场景随技能安装，不需要项目文件：

```text
skills/llm-wiki/scenarios/
└── topic-exploration/
    └── SCENARIO.md         定主题、多轮检索、总结、按需沉淀
```

- `scenario list` 把项目场景和内置场景列在一起，每项带 `source`（`project` 或
  `builtin`）；同 id 时项目版优先，内置版不重复列出。
- `scenario load "topic-exploration"` 返回内置流程；它的 `path` 是展示标签
  `builtin/topic-exploration/SCENARIO.md`，不是磁盘路径，`resources` 恒为空（内置流程自包含）。
- 内置场景不自动进入 `query` 结果；需要时显式 `scenario search`，内置场景同样会被检出，
  `source` 为 `builtin`。项目覆盖后 `source` 变为 `project`。
- 内置场景只读，改流程请复制到 `scenario/topic-exploration/` 再改，不要改技能安装目录。

## SCENARIO.md

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

1. 看积压：`status --json`，记下 `pending` 条数与 `tasks.open`，`next` 的提示决定下一步。
2. 入库：有新文件就跑 `ingest --json`；`failed` 与 `oversize` 非空时先报给用户。
3. 领任务：`next --kind route --json` 一次领一条，按任务目录里的 `brief.md`、`work.json`
   执行；定案前先用 `query --task <id> 词...` 登记检索，同一批要搜的词全部搜完。
4. 提交：把结果写进 `.wiki/tasks/<id>/payload.json`，运行 `submit <id> --json`；
   被拒就按 `error.next` 修正后重交，不要 `release` 掉重来。
5. 循环：重复 3–4 直到 `next` 说没有任务；页面任务同理，载荷换成正文与证据。
6. 复核：`status --json` 的 `pending` 与 `tasks.open` 归零，`stale_pages` 里没有这次动过的页。

## 判断点

- `E_SOURCE_DRIFT`：raw 在排队期间被改过，重新读来源后重做这条 decision。
- `E_TASK_UNSUBMITTED`：手头还有领了没交的任务，填完再 `next`，不要并发领。
- `failed` 里有解析失败的来源：不猜内容，报给用户等修。

## 完成标准

- 没有 pending 来源、没有未关闭的任务；
- 每条新事实都有逐字引文；
- 这次没动过的页面 `status` 仍是 `stable`。

## 由人完成

- 来源解析失败、需要补原始文件或用印件时由人处理，agent 不代填。
```

frontmatter 字段：

| 字段 | 必需 | 说明 |
| --- | --- | --- |
| `name` | 否 | 机器名（英文），`scenario list` 与检索按它展示；缺省用目录名 |
| `alias_name` | 否 | 中文展示名，只用于 `scenario list` 的显示 |
| `description` | 是 | 一句话说明场景解决什么问题，参与检索与展示 |
| `tags` | 否 | Obsidian 风格的标签列表：不带 `#`、不能含空格（空白与 `,`、`、`、`;` 都当分隔符），可用 `a/b` 表示层级；`#` 前缀会被去掉，纯数字标签会被丢弃 |

正文写给执行者看，重点写清顺序、判断点、完成标准和失败时怎么处理。步骤要能执行，
不要只写原则；需要事实依据的地方，指向知识页或原始文件，而不是把事实抄进场景。
`SCENARIO.md` 超过 200000 字符会被判为不合法；主流程保持精简，细节放 `references/`。

## 脚本与说明

`scripts/` 里的脚本应只做确定性、可重复的操作，并在文件头或同目录 README 里写清用途、
参数、输入输出和失败行为。执行前先确认它做什么；需要外部环境、凭据或联网时，在场景正文
里显式说明，不要假设执行者会盲跑。

`references/` 放长说明、字段表、检查清单等。主流程只写“何时读哪个文件”，细节放进去，
避免 `SCENARIO.md` 膨胀。

## 检索与加载

场景不经过 `ingest`，`scenario/` 与 `场景能力/` 在摄入时整目录跳过，因此改流程不需要重建知识库。
`query` 不检索场景；要用场景能力时必须显式检索，避免场景自动介入普通知识问答：

```bash
python3 <skill>/scripts/wiki.py query "入库" --json
python3 <skill>/scripts/wiki.py scenario search "入库" --json
python3 <skill>/scripts/wiki.py scenario list --json
python3 <skill>/scripts/wiki.py scenario load intake-backlog --json
python3 <skill>/scripts/wiki.py scenario load "topic-exploration" --json
```

`scenario search` 返回命中的项目场景与内置场景，按分数排序；`scenario load` 返回
`content`（`SCENARIO.md` 全文）、`meta`、`path` 和 `resources`。
按 `content` 的步骤执行，需要细节时再读 `resources` 里的文件。结构不合法的场景不会出现在
`scenario search` 结果里，但会出现在 `scenario list` 中并带 `error`，方便修正。

话题探索流程依次做：定主题、找现成能力、找已有话题、多轮查页面、补原始依据、判断收敛、
总结，最后问用户是否沉淀到 `话题/<主题>.md`。`query` 的 `topics` 是已知话题，相关的先读
再探索，避免重复劳动；话题格式见 [话题沉淀](topics.md)。

## 边界

- 场景是能力，不是知识页：不要把场景当来源摄入，也不要在页面里引用场景文件。
- 场景步骤与知识页冲突时，流程以场景为准；事实依据仍以页面和 raw 为准。
- 场景文件是活文件，直接编辑即可生效；不要在 `.wiki/` 里另存副本。
- 场景只收 agent 真能执行的方法：每一步都要落到具体命令或脚本上；需要人工办理的环节
  单列一节写「由人完成」，不要包装成场景步骤。
- 没有系统权限、只能由经办人完成的流程（开户、审批、现场检查这类）不是场景：它们属于
  知识页里的 `process` 知识，按页引用；不要把人工步骤改写成 agent 步骤。
- `scenario list` / `scenario load` 只读，不要求先 `init`，也不会创建 `.wiki/`。
- 话题探索的沉淀物是 `话题/`，不是场景；只有流程稳定、可重复执行时才另建场景。

完整命令字段与错误码见 [CLI 契约](cli.md#scenario)。
