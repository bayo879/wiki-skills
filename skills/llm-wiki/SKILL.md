---
name: llm-wiki
description: 当用户以 `wiki:` 或 `workflow:` 开头下达任务，或要把工作区里的本地文档整理成可检索、可追溯的 Markdown 知识库时使用：建库与增量摄入、检索已有知识、梳理某个主题、按点名的工作流办事、把这次调研沉淀成话题。
---

# llm-wiki：按任务填内容

引擎决定有哪些任务、校验什么、写到哪里；你只做 brief 要求的内容判断。**不要直接改
`.wiki/` 下的数据库或任务文件**，手改不会生效，页面只能通过 `submit` 修改。
观看区（`publish.dir`，默认 `wiki/`）里，`pages/` 由引擎按账本推送，手改不会被覆盖、只会
告警，只有人工 `sync` 才能收进账本；`raw/` 是原文副本，`README.md` 每次推送重写，都不要手改。

## 入口：`wiki:` 与 `workflow:` 前缀

用户消息以 `wiki:` 或 `workflow:` 开头（也接受全角冒号、前导空白、大小写）时，这次就明确走
对应入口：`wiki:` 去掉前缀后按需求落位，`workflow:` 只走工作流。前缀只认消息开头，正文里
出现的不算；单独一个不带冒号的 `wiki` / `workflow` 也不算。

`wiki:` 去前缀后按需求落位，不推测意图：

| 去前缀后的需求 | 去向 |
| --- | --- |
| 要交付物：PPT、演示、幻灯片、汇报材料、逐页大纲 | `workflow load ppt_creator`，按它的步骤执行 |
| 把会议记录、纪要拆成行动清单或待办（谁做什么、什么时候前） | `workflow load meeting-actions`，按它的步骤检索记录、读原文、拆带依据的清单 |
| 评审、检查或优化某个工作流（「这个工作流有什么不足」） | `workflow load workflow-review`，按它给的清单出结论 |
| 新建、起草或搭建工作流（「建个工作流」「把这套流程写成工作流」） | `workflow load workflow-create`，前提不过先给替代方案，不硬建 |
| 问、找、梳理、有哪些、有什么区别 | 内置 `topic-exploration` |
| 建库、整理、把这批资料摄入 | 本文「建库或增量摄入」一节 |
| 点名了工作流（`wiki:ppt_creator …` 或「按 xx 工作流」） | 直接 `workflow load` 那个 |

两段式：需求里既有「找资料」又有交付物，且用「我确定范围后」「确认后」「再…」隔开时，
**先只做到范围确认**——`query` 2–4 组关键词，按主题簇列出命中页面、能支撑什么、缺什么，
停下来等用户挑；挑定后把它记为「已明确」，再 `workflow load` 交付物工作流接着做，不重复
取证。没写这句话，就一次做到交付物。

- 只写了 `wiki:`：问一句要做什么，不猜。
- 需求超出 wiki（联网找资料、OCR、写代码、排版渲染）：直接说明不覆盖，不硬套摄入或工作流。

### `workflow:`：独占的工作流入口

- 去掉前缀后为空：问一句要办什么，不猜。
- 前缀后是有效工作流 id（如 `workflow:ppt_creator`）：直接 `workflow load`，再核对返回
  `content` 里的 `## 适用` 与 `## 前置检查`。
- 否则拆成 2–4 个关键词跑 `workflow search`，最多 5 个候选；先看候选的 `applies`，符合启动
  条件才 `load` 并按步骤执行。中文长句不要整句丢给检索。
- 0 命中，或候选都不符合 `applies`：直接回“无可用workflow”，不硬套、不转去查知识，也不做
  页面兜底。需要查知识时让用户改用 `wiki:`。

## 工作区边界

技能装在项目里还是全局技能目录，不改变这条规则：**所有 wiki 操作都以会话工作区为根目录**。

- 命令一律在工作区根目录执行；不要 `cd` 到工作区外。`--root` 只接受当前目录或它下面的
  子目录，指向区外会被拒绝。
- 资料在工作区外时，停下来让用户把那个目录作为工作区打开，或复制进来；不要跨区扫描、
  读取或写入。
- 草稿、任务目录、导出与临时文件都放 `<root>/.wiki/` 下：`.wiki/drafts/`、
  `.wiki/tasks/<id>/`、`.wiki/export`、`.wiki/tmp/`。根目录下非隐藏文件会被当成来源；
  工作流/话题目录与观看区（默认 `wiki/`）整目录跳过，即使关了发布，观看区目录名也不能
  当来源。

## 运行时

命令示例用 `python3`（项目基线 Python 3.12，更新的 3.x 也能用）。Windows 没有 `python3` 时改用
`py -3.12` 或指向 3.12+ 的 `python`；首次使用可运行 `scripts\bootstrap.ps1`，它查找已有的
Python 3.12+（agent 自带运行时用 `-Python <python.exe>` 指定）并把技能放进 agent 的技能目录。
脚本不下载、不安装 Python；放置只在技能目录外进行，不要把它当成 wiki 任务的一步。

### TeleAgent：初始化时记一条长期偏好

在 TeleAgent 里运行时，把下面这条写进当前账号的 `USER.md`（长期偏好，每次对话自动载入；
已有同义条目就跳过）：

> 当目录下存在 llm-wiki 技能和 `.wiki/` 文件夹时，这个工作区的问题默认先通过 wiki 进行检索。

按 memory-manager 的约定写：`##` 分大类、每条一行列表项；文件由 Runtime 托管，只说明目标
文件与内容，不猜绝对路径、不指定工具名。

## 建库或增量摄入

不需要先跑 `check` 或 `init`：打开账本的命令发现缺目录、缺配置、缺表会当场补齐，补了什么
写在返回值的 `repaired` 里。直接干活：

```bash
python3 <skill>/scripts/wiki.py ingest --json
```

`ingest` 扫描当前工作区，把新增或变化的文件排成任务。已有 wiki 重复执行即可；不要为了
“刷新”而删 `.wiki/`。返回的 `queue.open` 是摄入后的未完成任务数：大于 0 就继续 `next`，
为 0 就不用再取。只建库不摄入才用 `init`，它不是前置步骤。

`check` 与 `doctor` 只是诊断，不修东西：跑不动时再用它们定位，不要当日常步骤。空目录里
跑命令会建出空账本，这是正常的，不是“脏数据”。

出错时错误对象带 `code`、`message`、`next`，以及 `doc`（恢复手册绝对路径）和 `section`
（手册里的小节标题）：先按 `next` 处理，`next` 不够就打开 `doc` 读 `section` 那一节。

| 错误码 | 手册小节 |
| --- | --- |
| `E_HEALTH` | 运行时与安装 / 工作区不可写 |
| `E_STATE` | 旧账本 |
| `E_CONFIG` | 配置损坏 |
| `E_DB` | 账本损坏 |
| `E_PATH` | 工作区边界 |

完整处置与「何时停下来问人」见 [references/recovery.md](references/recovery.md)。

## 取任务

```bash
python3 <skill>/scripts/wiki.py next --json
```

每次只领一个任务。任务落在 `.wiki/tasks/<id>/`：`brief.md` 是玩法，`work.json` 是参考数据，
`payload.json` 是要填的骨架。`next` 的返回值同样有四项：

- `id`：提交时用；
- `brief`：这个任务要做什么、怎么做、提交后会发生什么；
- `template`：已经固定好的载荷骨架，和 `payload.json` 内容一致，只填空；
- `work`：原文位置、已有页面索引、已排队但尚未写入的页面（`pending_pages`）、当前页内容；
  route 任务另有 `queries{budget,used,remaining}`，是这次任务的检索配额。

route 任务多一步：先登记检索，再定案。

```bash
python3 <skill>/scripts/wiki.py query --task <id> 词1 词2 词3 --json
```

每个词单独检索、各返 5 条约页面，按词分组显示；配额是 5 个词/1000 字（正文不足 1000 字也
给 5 个），可以分多次登记，重复的词不重复计配额，超出报 `E_QUERY_BUDGET`。没登记就 `submit`
会报 `E_NO_QUERY`，任务退回队列：重新 `next` 领回、补查后再交。命中被截断时换词重查，
不要把「没搜到」当成「没有相似页」。

按 `brief.md` 做，把 `.wiki/tasks/<id>/payload.json` 填好（只填空，不要改字段名），然后：

```bash
python3 <skill>/scripts/wiki.py submit <id> --json
```

`submit` 默认读这个任务目录里的 `payload.json`，不用再传路径；`--payload` 只在你确实要
从别处提交时用（`-` 表示从 stdin 读）。提交成功或任务过期后目录会被清掉；任务被释放后
重新 `next` 会刷新 `brief.md`、`work.json`，但不会覆盖你已经填好的 `payload.json`。

提交成功后重新 `next`。`next` 按任务树深度优先：当前这份正文拆出的页面任务没写完，不会发下一
份正文。一次只做一件事：手头还有领了没交的任务时，`next` 直接报 `E_TASK_UNSUBMITTED`，错误里
带着任务 id 和 `payload.json` 路径——接着填完 `submit` 即可（`.wiki/tasks/<id>/` 里的
`brief.md`、`work.json` 与已填内容都还在，中断或上下文压缩后照这个路径续做），确实不要了
再 `release` 把它放回队列。只有 `next` 说没有任务时才停止，不要自己造任务，也不要为了
“推进”去跑别的命令。

队列只认一个写者：任务记在领取它的会话名下（`next` 返回的 `owner`，默认取宿主注入的会话
id；终端会话用 `--owner <id>` 或 `export LLM_WIKI_OWNER=<id>` 固定一个，后续命令都带同一个）。
**不要派子智能体并行摄入，也不要开多个会话轮流取任务**：别的会话正在摄入时，`next` 与
`ingest` 都报 `E_INGEST_BUSY` 并指出占用的会话，这时立刻停止——不要重试、不要 `sleep` 等、不要
`release` 别人的任务，向用户说明已有会话在摄入即可。要加快摄入只能减少单份文件的往返：
跳过没有新事实的来源、合并页面、一次把该页写全。只有用户明确要求接管时，才 `release`
对方的任务。

同一页面同时只会有一个未关闭的页面任务：
给同一页面排第二个来源时，如果该页面已被领走，`submit` 返回 `E_STATE`，等那个任务提交后
再试。

## 填写内容

- 只写原文支持的事实；数字、日期、文号、责任主体都要能回到 `work.sources` 里的 raw。
- `evidence[].quote` 从 raw 逐字复制，不改写、不概括；一句到两句，引文对不上会被拒绝。
  `evidence[].source` 写 `work.sources` 里的来源 key，不要写链接或 raw 路径；观看区由引擎
  渲染成指向 raw 的 Markdown 链接。
- 新建页面的章节由 `template.body` 固定，不要改章节名、不要删章节；更新页面沿用原有结构。
  每节按页面类型的固定组合写，模板在 `templates/<type>.md`（concept、entity、process），
  按 `template` 的类型读对应文件。类型只表示内容形态：要求与规则用 `concept`，组织/系统/
  人物用 `entity`，可复用的办理步骤用 `process`；`process` 不承诺 agent 能执行，同一主题
  的要求与步骤写在同一页，不要拆成两页。
- 新建页面的 `summary` 必填，1–200 字，一句完整的话；更新页面时 `template.evidence`
  里旧来源的证据要保留，不要删。
- `links` 由 route 预填，只含提交时已存在或同批已提交的页面 id；提交前再判断一次读者下一步
  该看哪页（2–5 个），确实没有相关页才留空。更新页只增不删，正文里不要写 `[[page-id]]` 或
  `## 关联`；已有页补链用 `page link`，批量按共享 raw 来源补链先用 `page link --repair --dry-run` 看计划。`boundaries` 写清覆盖与不覆盖。
- 推断要标“据此推断”；个人联系方式不写入页面。
- 页面边界、标题、摘要、合并优先等标准见 [references/curation.md](references/curation.md)。

## 查知识

回答「是什么 / 怎么做 / 有哪些 / 有什么区别 / 帮我梳理」这类问题时，默认按内置
「话题探索」工作流办事：定主题、多轮检索、总结，最后问用户是否沉淀。

```bash
python3 <skill>/scripts/wiki.py workflow load "topic-exploration" --json
python3 <skill>/scripts/wiki.py query "关键词" --json
python3 <skill>/scripts/wiki.py query --page <page-id> --json
```

`query` 会带回最多 5 个工作流候选：`workflows` 是候选列表，`workflow_matches` 是数量，每条
候选带 `applies`（`## 适用` 原文）。候选不是自动注入：只有 `applies` 与当前场景相符时才
`workflow load` 照做；不符合就忽略，照常按页面/话题回答。话题探索仍是先显式
`workflow load "topic-exploration"` 再照着走的流程。`query --task` 只登记任务检索，不带工作流
候选。`topics` 是已沉淀话题，相关的先读再探索，不重复劳动。单点查
页面、查字段或确认某个已知事实，直接用 `query`，不必走完整流程；要读原文就打开
`work.json` 里 `sources[].raw` 指向的观看区文件。

回答时给出页面 id 和原始文件 key。页面与 raw 冲突时以 raw 为准，并说明冲突。

## 观看区

命令会在根目录维护一份给人看的产出 `<view>/`（默认 `wiki/`，由 `.wiki/wiki.json` 的
`publish.dir` 指定；`publish` 段另有 `enabled`、`topics`、`workflows`，默认都为 `true`，
一般不需要动）：

- `pages/`：按账本推送的知识页，按 `<type>/<标题>.md` 命名；手改不会覆盖，只会告警。
  证据段每行是标准 Markdown 链接：先链到 raw 副本，括号里保留来源 key。
- `raw/`：摄入时写入的原文副本，用于核对引文；不要手改。末尾的 `## 原文来源` 段由引擎
  生成，链回挂载它的原始文件。
- `README.md`：目录说明与统计，每次推送重写。
- `topics/`、`workflows/`：首次推送时从 `<root>/topics/`、`<root>/workflows/` 各投放一份；
  之后归用户。

不需要为了交付结果去动观看区；`next`/`submit` 正常跑完，下一次命令会自动推送。只有用户
要求把观看区里的手改收回时，才运行 `sync`（先确认再跑；它只回收、不推送）：

```bash
python3 <skill>/scripts/wiki.py sync --json
python3 <skill>/scripts/wiki.py sync --body-only --json
```

## 按工作流执行

项目可以把一类可执行任务的流程写成工作流，放在 `<root>/workflows/<工作流>/`：`WORKFLOW.md`
是主流程，`scripts/` 和 `references/` 是配套脚本与说明。技能自带 `topic-exploration`、
`workflow-review`、`workflow-create` 内置工作流；项目放同名工作流即可覆盖它。内置项不给根目录
相对的 `path`，只给 `skill_path`（技能目录相对，如 `workflows/<id>/WORKFLOW.md`），
`resources` 恒为空：正文直接用 `workflow load` 返回的 `content`，不要按工作区去找这个文件。
工作流 frontmatter 的 `name` 写
英文机器名，中文展示名放 `alias_name`；`workflow list` 以 `id（alias_name）` 显示。

工作流是可执行流程，不是知识页：它不会被 `ingest` 写进账本；普通 `query` 会带回最多 5 个
候选（`workflows`，每条带 `applies`），显式 `workflow search` 也只返回最多 5 个，0 命中时带
`message: "无可用workflow"`。候选不会自动加载，只有核对 `applies` 成立后才 `workflow load`。
改完流程不用重新摄入。

`## 步骤` 一节里的每一步都要标类型：

- `tool:` 调命令、脚本或另一个技能；正文写清命令与参数。
- `human:` 需要人做或人确认；停下来问用户，不要替用户决定。
- `agent:` 由你判断或写作，不需要外部动作。
- `verify:` 验证前一步结果；可省略。

至少要有一步 `tool:`。只有 `human:`/`agent:`、没有可调用工具时，它是纯人工流程，应写成
`process` 页面，`workflow load` 会判它无效并指向 `process`。没有类型标记的旧文件 0.5.0
仍能加载，但会给出 warning；`0.6.0` 起没有标记将判无效。

需要按工作流办事时，先看 `query` 带回的候选或显式检索，再加载照做：

```bash
python3 <skill>/scripts/wiki.py workflow search "开户" --json
python3 <skill>/scripts/wiki.py workflow list --json
python3 <skill>/scripts/wiki.py workflow load "<工作流>" --json
```

`load` 返回 `workflow`（元数据）、`content`（全文）、`resources`（配套文件），以及按类型分好
的 `tools`、`human_steps`、`agent_steps`、`verify_steps`（每项 `{line, text}`）和 `warnings`；
`search` 与 `query` 的候选带 `applies`；`list` 的每一项也带 `tools`/`human_steps` 计数与
`warnings`。

- 普通提问直接用 `query`；它带回的候选只作提示，不要为了找流程额外先搜，也不要让工作流替用户
  决定范围。只有 `applies` 明确成立、且用户要按流程办事时才 `load`。
- 按 `tools` 调工具、按 `human_steps` 停下来问人；用 `resources` 找配套文件，需要细节时再读
  `references/`，不要一次把所有脚本和说明都倒进上下文。
- 运行 `scripts/` 前先看清用途与参数；没有把握时先向用户确认，不要盲跑。
- 工作流步骤与知识页冲突时，流程以 `WORKFLOW.md` 为准；事实依据仍以 raw 和页面为准，
  缺依据就说明，不要补编步骤。
- 普通提问没有命中候选时按普通知识检索回答；`workflow:` 前缀下没有命中或候选都不符合
  `applies` 时，直接回“无可用workflow”，不转页面兜底。不要凭目录名猜流程，也不要声称执行了
  不存在的工作流。

## 沉淀话题

话题探索收尾时默认问用户是否沉淀；用户要求把一次讨论或调研留给下次时，写到**当前工作区
根目录**的 `topics/<主题>.md`，一个主题一份。话题是 wiki 的探索层，不是正式知识页：不运行
`ingest`，不写 `.wiki/`，不建页面、不排任务；写入后报告“未执行摄入”。观看区的 `topics/`
只是首次推送时的副本，之后归用户；改话题仍以根目录 `topics/` 为准。

- 话题文件的 frontmatter、固定六节、归属判断、授权与完成标准，见内置工作流
  `workflow load "topic-exploration"` 的“话题文件”与“沉淀前的归属判断”两节。
- 流程稳定、可重复执行时，同时给出 `workflows/<id>/WORKFLOW.md` 的工作流候选草稿，用户确认
  后再建；资料不足时列出待摄入来源与下一轮要查的关键词。
- 项目事实归产品文档，强制规则归指导文档，未决问题归 issues；只有可复用的方法、经验、
  踩坑、方案取舍和探索结论写话题。凭据不落盘。

## 提交失败

- 提交失败会先把任务退回队列。读返回的 `error.message` 与 `error.next`，重新 `next`
  领取任务（通常还是同一个 id），再改 `.wiki/tasks/<id>/payload.json` 里对应的内容重交；
  不要拿旧 id 直接重交。
- 返回 `E_TASK_STALE` 表示任务已被领取、关闭或页面已更新；重新 `next`，不要复用旧 payload。
- 返回 `E_INGEST_BUSY`（`next`、`ingest`）表示别的会话正在摄入并占着队列：立刻停止，不要重试、不要等待。
- 返回 `E_TASK_FOREIGN` 表示任务记在别的会话名下：不要提交，也不要抢过来，交给领取它的会话。
- 返回 `E_BUSY` 表示另一个 wiki 进程正在这个工作区写账本：等它结束再重试，不要删
  `.wiki/lock` 抢跑（只有确认它已崩溃时才删）。
- 不要绕过校验，也不要直接改 `.wiki/` 下的文件来“修好”它。

## 不做

- wiki 任务中不 OCR、不下载依赖、不联网。扫描件没有文本层时会被标为不支持，等有可复制文本后再摄入。
- 不跨工作区读写，不把缓存放到 `.wiki/` 之外。
- 不手写页面文件、不直接改数据库、不编辑任务文件。
