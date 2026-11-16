---
name: llm-wiki
description: 把工作区里的本地文档整理成可检索、可追溯的 Markdown 知识库。引擎负责扫描、解析、任务、校验与落盘；agent 按 next 返回的 brief 和 template 填内容，再 submit；query/read 查知识。回答知识问题时默认按内置「话题探索」场景办事：定主题、多轮检索、总结，再询问是否把主题沉淀成 `话题/` 文件（附能力候选与待查资料）。也能检索并加载项目 `场景能力/` 下的流程场景。适用于「建个 wiki」「把这些资料整理成知识库」「查一下已有知识」「梳理某个主题」「按开户/巡检等流程办事」「把这次调研沉淀下来」等场景。仅依赖 Python 标准库；PDF 可选 pdftotext 或 pypdf。
---

# llm-wiki：按任务填内容

引擎决定有哪些任务、校验什么、写到哪里；你只做 brief 要求的内容判断。**不要直接改
`.wiki/` 下的数据库、页面或任务文件**，手改不会生效，页面只能通过 `submit` 修改。

## 工作区边界

技能装在项目里还是全局技能目录，不改变这条规则：**所有 wiki 操作都以会话工作区为根目录**。

- 命令一律在工作区根目录执行；不要 `cd` 到工作区外。`--root` 只接受当前目录或它下面的
  子目录，指向区外会被拒绝。
- 资料在工作区外时，停下来让用户把那个目录作为工作区打开，或复制进来；不要跨区扫描、
  读取或写入。
- 草稿、载荷、导出与临时文件都放 `<root>/.wiki/` 下：`.wiki/drafts/`、
  `.wiki/payloads/`、`.wiki/export`、`.wiki/tmp/`。根目录下非隐藏文件会被当成来源。

## 运行时

命令示例用 `python3`（项目基线 Python 3.12）。Windows 没有 `python3` 时改用 `py -3.12` 或指向 3.12 的 `python`；首次使用可运行
`scripts\bootstrap.ps1` 检查/安装 Python 3.12 并把技能放到 agent 的技能目录。安装只在
技能目录外进行，不要把它当成 wiki 任务的一步。

## 自检

安装后、建库前，以及每次开始日常操作前，先运行：

```bash
python3 <skill>/scripts/wiki.py check --json
```

只有返回 `ok: true` 才能继续 `init`、`ingest`、`next` 或 `submit`。`ok: false` 时停止，
把 `data.checks` 里失败项告诉用户；不要跳过自检，也不要用 `doctor` 或手改 `.wiki/`
绕过。`check` 只检查解释器、技能文件、工作区可写和现有账本可读，不接触真实页面内容。
`init`、`ingest`、`next`、`submit` 也会先做同一项轻量检查；失败返回 `E_HEALTH`，按
`error.next` 处理。

## 建库或增量摄入

```bash
python3 <skill>/scripts/wiki.py init --json
python3 <skill>/scripts/wiki.py ingest --json
```

`ingest` 扫描当前工作区，把新增或变化的文件排成任务。已有 wiki 重复执行即可；不要为了
“刷新”而删 `.wiki/`。

## 取任务

```bash
python3 <skill>/scripts/wiki.py next --json
```

每次只领一个任务。每个任务只有四项：

- `id`：提交时用；
- `brief`：这个任务要做什么、怎么做、提交后会发生什么；
- `template`：已经固定好的载荷骨架，只填空；
- `work`：原文位置、已有页面索引、已排队但尚未写入的页面（`pending_pages`）、当前页内容。

按 `brief` 做，把 `template` 填好存到 `.wiki/payloads/<id>.json`，然后：

```bash
python3 <skill>/scripts/wiki.py submit <id> --payload .wiki/payloads/<id>.json --json
```

提交成功后重新 `next`。`next` 返回没有任务时就停止，不要自己造任务，也不要为了“推进”
去跑别的命令。

## 填写内容

- 只写原文支持的事实；数字、日期、文号、责任主体都要能回到 `work.sources` 里的 raw。
- `evidence[].quote` 从 raw 逐字复制，不改写、不概括；一句到两句，引文对不上会被拒绝。
- 新建页面的章节由 `template.body` 固定，不要改章节名、不要删章节；更新页面沿用原有结构。
  每节按页面类型的固定组合写，模板在 `templates/<type>.md`（concept、entity、process、note），
  按 `template` 的类型读对应文件。
- 新建页面的 `summary` 必填，1–200 字，一句完整的话；更新页面时 `template.evidence`
  里旧来源的证据要保留，不要删。
- `links` 只填 `work.page_index` 里已有的页面 id；`boundaries` 写清覆盖与不覆盖。
- 推断要标“据此推断”；个人联系方式不写入页面。
- 页面边界、标题、摘要、合并优先等标准见 [references/curation.md](references/curation.md)。

## 查知识

回答「是什么 / 怎么做 / 有哪些 / 有什么区别 / 帮我梳理」这类问题时，默认按内置
「话题探索」场景办事：定主题、多轮检索、总结，最后问用户是否沉淀。

```bash
python3 <skill>/scripts/wiki.py scenario load "话题探索" --json
python3 <skill>/scripts/wiki.py query "关键词" --json
python3 <skill>/scripts/wiki.py query --page <page-id> --json
python3 <skill>/scripts/wiki.py read "page:<page-id>" --json
python3 <skill>/scripts/wiki.py read "raw:<source-key>" --offset 1 --limit 120 --json
```

`query` 的 `topic_scenario` 恒指向这个默认场景（项目有同名场景时以项目为准），`topics`
是已沉淀话题，相关的先读再探索，不重复劳动。单点查页面、查字段或确认某个已知事实，
直接用 `query`/`read`，不必走完整流程。

回答时给出页面 id 和原始文件 key。页面与 raw 冲突时以 raw 为准，并说明冲突。

## 按场景执行

项目可以把一类任务的流程写成场景，放在 `<root>/场景能力/<场景>/`：`SCENARIO.md` 是主流程，
`scripts/` 和 `references/` 是配套脚本与说明。技能自带 `话题探索` 内置场景；项目放同名
场景即可覆盖它。场景是能力，不是知识页：它不会被 `ingest` 写进账本，但 `query` 会直接
检索它，所以改完流程不用重新摄入。内置场景的 `path` 只是展示标签
（`内置场景/<id>/SCENARIO.md`），不是磁盘路径。

`query` 结果的 `scenarios` 非空时，说明命中了场景。先加载再照做：

```bash
python3 <skill>/scripts/wiki.py scenario list --json
python3 <skill>/scripts/wiki.py scenario load "<场景>" --json
```

- 用 `content` 里的步骤执行任务，用 `resources` 找配套文件；需要细节时再读
  `references/`，不要一次把所有脚本和说明都倒进上下文。
- 运行 `scripts/` 前先看清用途与参数；没有把握时先向用户确认，不要盲跑。
- 场景步骤与知识页冲突时，流程以 `SCENARIO.md` 为准；事实依据仍以 raw 和页面为准，
  缺依据就说明，不要补编步骤。
- 没有命中场景时按普通知识检索回答；不要凭目录名猜流程，也不要声称执行了不存在的场景。

## 沉淀话题

话题探索收尾时默认问用户是否沉淀；用户要求把一次讨论或调研留给下次时，写到**当前工作区
根目录**的 `话题/<主题>.md`，一个主题一份。话题不是知识页：不运行 `ingest`，不写
`.wiki/`，不建页面、不排任务；写入后报告“未执行摄入”。

- 话题文件的 frontmatter、固定六节、归属判断、授权与完成标准，见内置场景
  `scenario load "话题探索"` 的“话题文件”与“沉淀前的归属判断”两节。
- 流程稳定、可重复执行时，同时给出 `场景能力/<id>/SCENARIO.md` 的能力候选草稿，用户确认
  后再建；资料不足时列出待摄入来源与下一轮要查的关键词。
- 项目事实归产品文档，强制规则归指导文档，未决问题归 issues；只有可复用的方法、经验、
  踩坑、方案取舍和探索结论写话题。凭据不落盘。

## 提交失败

- 提交失败会先把任务退回队列。读返回的 `error.message` 与 `error.next`，重新 `next`
  领取任务（通常还是同一个 id），再改 `template` 里对应的内容重交；不要拿旧 id 直接重交。
- 返回 `E_TASK_STALE` 表示任务已被领取、关闭或页面已更新；重新 `next`，不要复用旧 payload。
- 不要绕过校验，也不要直接改 `.wiki/` 下的文件来“修好”它。

## 不做

- wiki 任务中不 OCR、不下载依赖、不联网。扫描件没有文本层时会被标为不支持，等有可复制文本后再摄入。
- 不跨工作区读写，不把缓存放到 `.wiki/` 之外。
- 不手写页面文件、不直接改数据库、不编辑任务文件。
