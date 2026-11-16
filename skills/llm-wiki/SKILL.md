---
name: llm-wiki
description: 把工作区里的本地文档整理成可检索、可追溯的 Markdown 知识库。引擎负责扫描、解析、任务、校验与落盘；agent 按 next 返回的 brief 和 template 填内容，再 submit；query/read 查知识。适用于「建个 wiki」「把这些资料整理成知识库」「查一下已有知识」等场景。仅依赖 Python 标准库；PDF 可选 pdftotext 或 pypdf。
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
- `work`：原文位置、已有页面索引、当前页内容。

按 `brief` 做，把 `template` 填好存到 `.wiki/payloads/<id>.json`，然后：

```bash
python3 <skill>/scripts/wiki.py submit <id> --payload .wiki/payloads/<id>.json --json
```

提交成功后重新 `next`。`next` 返回没有任务时就停止，不要自己造任务，也不要为了“推进”
去跑别的命令。

## 填写内容

- 只写原文支持的事实；数字、日期、文号、责任主体都要能回到 `work.sources` 里的 raw。
- `evidence[].quote` 从 raw 逐字复制，不改写、不概括；引文对不上会被拒绝。
- 新建页面的章节由 `template.body` 固定，不要改章节名、不要删章节；更新页面沿用原有结构。
- `links` 只填 `work.page_index` 里已有的页面 id；`boundaries` 写清覆盖与不覆盖。
- 推断要标“据此推断”；个人联系方式不写入页面。
- 页面边界、标题、摘要、合并优先等标准见 [references/curation.md](references/curation.md)。

## 查知识

```bash
python3 <skill>/scripts/wiki.py query "关键词" --json
python3 <skill>/scripts/wiki.py query --page <page-id> --json
python3 <skill>/scripts/wiki.py read "page:<page-id>" --json
python3 <skill>/scripts/wiki.py read "raw:<source-key>" --offset 1 --limit 120 --json
```

回答时给出页面 id 和原始文件 key。页面与 raw 冲突时以 raw 为准，并说明冲突。

## 提交失败

- 读返回的 `error.message` 与 `error.next`，只改 `template` 里对应的内容后重交。
- 任务已失效或页面已被更新时，重新 `next`，不要复用旧 payload。
- 不要绕过校验，也不要直接改 `.wiki/` 下的文件来“修好”它。

## 不做

- 不 OCR、不下载依赖、不联网。扫描件没有文本层时会被标为不支持，等有可复制文本后再摄入。
- 不跨工作区读写，不把缓存放到 `.wiki/` 之外。
- 不手写页面文件、不直接改数据库、不编辑任务文件。
