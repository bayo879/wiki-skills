---
name: llm-wiki
description: 把工作区里的本地文档整理成可检索、可追溯的 Markdown 知识库。引擎负责扫描、解析、任务、校验与落盘；agent 按 next 返回的 brief 和 template 填内容，再 submit；query/read 查知识。回答知识问题时默认按内置「话题探索」场景办事：定主题、多轮检索、总结，再询问是否把主题沉淀成 `话题/` 文件（附能力候选与待查资料）。场景能力必须显式 `scenario search` 后再 `scenario load`，不会随 query 自动介入。适用于「建个 wiki」「把这些资料整理成知识库」「查一下已有知识」「梳理某个主题」「按开户/巡检等流程办事」「把这次调研沉淀下来」等场景。仅依赖 Python 标准库；PDF 可选 pdftotext 或 pypdf。
---

# llm-wiki：按任务填内容

引擎决定有哪些任务、校验什么、写到哪里；你只做 brief 要求的内容判断。**不要直接改
`.wiki/` 下的数据库或任务文件**，手改不会生效，页面只能通过 `submit` 修改。
观看区（`publish.dir`，默认 `wiki/`）里，`pages/` 由引擎按账本推送，手改不会被覆盖、只会
告警，只有人工 `sync` 才能收进账本；`raw/` 是原文副本，`README.md` 每次推送重写，都不要手改。

## 工作区边界

技能装在项目里还是全局技能目录，不改变这条规则：**所有 wiki 操作都以会话工作区为根目录**。

- 命令一律在工作区根目录执行；不要 `cd` 到工作区外。`--root` 只接受当前目录或它下面的
  子目录，指向区外会被拒绝。
- 资料在工作区外时，停下来让用户把那个目录作为工作区打开，或复制进来；不要跨区扫描、
  读取或写入。
- 草稿、任务目录、导出与临时文件都放 `<root>/.wiki/` 下：`.wiki/drafts/`、
  `.wiki/tasks/<id>/`、`.wiki/export`、`.wiki/tmp/`。根目录下非隐藏文件会被当成来源；
  场景/话题目录与观看区（默认 `wiki/`）整目录跳过，即使关了发布，观看区目录名也不能
  当来源。

## 运行时

命令示例用 `python3`（项目基线 Python 3.12，更新的 3.x 也能用）。Windows 没有 `python3` 时改用
`py -3.12` 或指向 3.12+ 的 `python`；首次使用可运行 `scripts\bootstrap.ps1`，它查找已有的
Python 3.12+（agent 自带运行时用 `-Python <python.exe>` 指定）并把技能放进 agent 的技能目录。
脚本不下载、不安装 Python；放置只在技能目录外进行，不要把它当成 wiki 任务的一步。

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
- `work`：原文位置、已有页面索引、已排队但尚未写入的页面（`pending_pages`）、当前页内容。

按 `brief.md` 做，把 `.wiki/tasks/<id>/payload.json` 填好（只填空，不要改字段名），然后：

```bash
python3 <skill>/scripts/wiki.py submit <id> --json
```

`submit` 默认读这个任务目录里的 `payload.json`，不用再传路径；`--payload` 只在你确实要
从别处提交时用（`-` 表示从 stdin 读）。提交成功或任务过期后目录会被清掉；任务被释放后
重新 `next` 会刷新 `brief.md`、`work.json`，但不会覆盖你已经填好的 `payload.json`。

提交成功后重新 `next`。如果上次领了任务但没提交（中断、上下文压缩、误重跑），`next` 会把
同一个任务发回来，`payload.json` 保留，接着填完提交即可。只有 `next` 说没有任务时才停止，
不要自己造任务，也不要为了“推进”去跑别的命令。同一页面同时只会有一个未关闭的页面任务：
给同一页面排第二个来源时，如果该页面已被领走，`submit` 返回 `E_STATE`，等那个任务提交后
再试。

## 填写内容

- 只写原文支持的事实；数字、日期、文号、责任主体都要能回到 `work.sources` 里的 raw。
- `evidence[].quote` 从 raw 逐字复制，不改写、不概括；一句到两句，引文对不上会被拒绝。
- 新建页面的章节由 `template.body` 固定，不要改章节名、不要删章节；更新页面沿用原有结构。
  每节按页面类型的固定组合写，模板在 `templates/<type>.md`（concept、entity、process、note），
  按 `template` 的类型读对应文件。
- 新建页面的 `summary` 必填，1–200 字，一句完整的话；更新页面时 `template.evidence`
  里旧来源的证据要保留，不要删。
- `links` 由 route 预填，只含提交时已存在或同批已提交的页面 id；更新页只增不删，正文里
  不要写 `[[page-id]]` 或 `## 关联`；`boundaries` 写清覆盖与不覆盖。
- 推断要标“据此推断”；个人联系方式不写入页面。
- 页面边界、标题、摘要、合并优先等标准见 [references/curation.md](references/curation.md)。

## 查知识

回答「是什么 / 怎么做 / 有哪些 / 有什么区别 / 帮我梳理」这类问题时，默认按内置
「话题探索」场景办事：定主题、多轮检索、总结，最后问用户是否沉淀。

```bash
python3 <skill>/scripts/wiki.py scenario load "topic-exploration" --json
python3 <skill>/scripts/wiki.py query "关键词" --json
python3 <skill>/scripts/wiki.py query --page <page-id> --json
python3 <skill>/scripts/wiki.py read "page:<page-id>" --json
python3 <skill>/scripts/wiki.py read "raw:<source-key>" --offset 1 --limit 120 --json
```

`query` **不检索场景**；话题探索是先显式 `scenario load "topic-exploration"` 再照着走的流程，
不是查询时自动注入的分支。`topics` 是已沉淀话题，相关的先读再探索，不重复劳动。单点查
页面、查字段或确认某个已知事实，直接用 `query`/`read`，不必走完整流程。

回答时给出页面 id 和原始文件 key。页面与 raw 冲突时以 raw 为准，并说明冲突。

## 观看区

命令会在根目录维护一份给人看的产出 `<view>/`（默认 `wiki/`，由 `.wiki/wiki.json` 的
`publish.dir` 指定；`publish` 段另有 `enabled`、`topics`、`scenarios`，默认都为 `true`，
一般不需要动）：

- `pages/`：按账本推送的知识页，按 `<type>/<标题>.md` 命名；手改不会覆盖，只会告警。
- `raw/`：摄入时写入的原文副本，用于核对引文；不要手改。
- `README.md`：目录说明与统计，每次推送重写。
- `topics/`、`scenarios/`：首次推送时从 `话题/`、`scenario/` 各投放一份；之后归用户。

不需要为了交付结果去动观看区；`next`/`submit` 正常跑完，下一次命令会自动推送。只有用户
要求把观看区里的手改收回时，才运行 `sync`（先确认再跑；它只回收、不推送）：

```bash
python3 <skill>/scripts/wiki.py sync --json
python3 <skill>/scripts/wiki.py sync --body-only --json
```

## 按场景执行

项目可以把一类任务的流程写成场景，放在 `<root>/scenario/<场景>/`（旧名 `场景能力/`，二者同 id 时以
`scenario/` 为准）：`SCENARIO.md` 是主流程，
`scripts/` 和 `references/` 是配套脚本与说明。技能自带 `topic-exploration` 内置场景；项目放同名
场景即可覆盖它。场景是能力，不是知识页：它不会被 `ingest` 写进账本；`query` 也不检索
它，必须显式 `scenario search`，避免场景自动介入普通知识问答。改完流程不用重新摄入。
内置场景的 `path` 只是展示标签（`builtin/<id>/SCENARIO.md`），不是磁盘路径。
场景 frontmatter 的 `name` 写英文机器名，中文展示名放 `alias_name`；`scenario list` 会
以 `id（alias_name）` 显示。

需要场景能力时，先显式检索，再加载照做：

```bash
python3 <skill>/scripts/wiki.py scenario search "开户" --json
python3 <skill>/scripts/wiki.py scenario list --json
python3 <skill>/scripts/wiki.py scenario load "<场景>" --json
```

- 只有确认要按场景办事时才检索场景；普通提问不要先搜场景，也不要让场景替用户决定范围。
- 用 `content` 里的步骤执行任务，用 `resources` 找配套文件；需要细节时再读
  `references/`，不要一次把所有脚本和说明都倒进上下文。
- 运行 `scripts/` 前先看清用途与参数；没有把握时先向用户确认，不要盲跑。
- 场景步骤与知识页冲突时，流程以 `SCENARIO.md` 为准；事实依据仍以 raw 和页面为准，
  缺依据就说明，不要补编步骤。
- 没有命中场景时按普通知识检索回答；不要凭目录名猜流程，也不要声称执行了不存在的场景。

## 沉淀话题

话题探索收尾时默认问用户是否沉淀；用户要求把一次讨论或调研留给下次时，写到**当前工作区
根目录**的 `话题/<主题>.md`，一个主题一份。话题是 wiki 的探索层，不是正式知识页：不运行
`ingest`，不写 `.wiki/`，不建页面、不排任务；写入后报告“未执行摄入”。观看区的 `topics/`
只是首次推送时的副本，之后归用户；改话题仍以根目录 `话题/` 为准。

- 话题文件的 frontmatter、固定六节、归属判断、授权与完成标准，见内置场景
  `scenario load "topic-exploration"` 的“话题文件”与“沉淀前的归属判断”两节。
- 流程稳定、可重复执行时，同时给出 `场景能力/<id>/SCENARIO.md` 的能力候选草稿，用户确认
  后再建；资料不足时列出待摄入来源与下一轮要查的关键词。
- 项目事实归产品文档，强制规则归指导文档，未决问题归 issues；只有可复用的方法、经验、
  踩坑、方案取舍和探索结论写话题。凭据不落盘。

## 提交失败

- 提交失败会先把任务退回队列。读返回的 `error.message` 与 `error.next`，重新 `next`
  领取任务（通常还是同一个 id），再改 `.wiki/tasks/<id>/payload.json` 里对应的内容重交；
  不要拿旧 id 直接重交。
- 返回 `E_TASK_STALE` 表示任务已被领取、关闭或页面已更新；重新 `next`，不要复用旧 payload。
- 返回 `E_BUSY` 表示另一个 wiki 进程正在这个工作区写账本：等它结束再重试，不要删
  `.wiki/lock` 抢跑（只有确认它已崩溃时才删）。
- 不要绕过校验，也不要直接改 `.wiki/` 下的文件来“修好”它。

## 不做

- wiki 任务中不 OCR、不下载依赖、不联网。扫描件没有文本层时会被标为不支持，等有可复制文本后再摄入。
- 不跨工作区读写，不把缓存放到 `.wiki/` 之外。
- 不手写页面文件、不直接改数据库、不编辑任务文件。
