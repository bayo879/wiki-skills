---
name: llm-wiki
description: 把本地文档整理成可检索、可追溯的 Markdown 知识库：扫描来源、保留原文证据、按主题建页、维护来源账本与双向链接。适用于建立项目 wiki、批量摄入公文/论文/笔记、写作前检索既有知识。当用户说“建个 wiki”“把这些资料整理成知识库”“沉淀一下”“ingest 这批文档”，或需要 build/update a knowledge base、query existing notes 时使用。仅依赖 Python 标准库，PDF 解析可选 pdftotext 或 pypdf。
---

# llm-wiki：来源可追溯的知识库

这个技能把「一堆文档」变成「一套带出处的 Markdown 页面」。确定性工作由
`scripts/wiki.py` 完成：扫描、解析、哈希、原子写入、来源账本、链接索引。
语义工作由你完成：判断页面边界、写正文、标注证据与边界。

**不要**把原文大段复制进页面，也**不要**只凭记忆写页面。每个事实都要能回到
`.wiki/raw/` 里的来源。

## 何时使用

- 用户要把一个目录（公文、笔记、论文、网页存档）整理成可查询的知识库。
- 已有 wiki 需要增量摄入新文件，或核对某页是否过期。
- 写作/决策前要先检索既有知识，并给出处。

不适用：一次性问答、纯网页抓取、需要 OCR 的扫描件（本技能只提示，不做 OCR）。

## 五步工作流

在**知识库根目录**执行（或给所有命令加 `--root <目录>`）：

```bash
python3 <skill>/scripts/wiki.py ingest --json     # 1. 扫描并解析来源，输出 pending
python3 <skill>/scripts/wiki.py read "raw:<key>" --limit 120   # 2. 读证据
# 3. 决策：新建页 / 并入已有页 / 跳过（--skip）
python3 <skill>/scripts/wiki.py page put --id <slug> --title <标题> \
  --type concept --summary <一句话> --source <key> --body <草稿.md> --json
python3 <skill>/scripts/wiki.py settle <key> --json   # 4. 收口：来源须被非 stale 页面引用
python3 <skill>/scripts/wiki.py doctor --json         # 5. 体检
```

规则细节：

- **一个来源可以支撑多页，一页必须至少引用一个来源。** `page put --source` 的 key
  必须与 `ingest` 输出的 key 完全一致（相对路径）。
- **一页一个主题。** 同一通知的不同方面若会被分别检索，才拆页；不要按段落拆。
- **重复摄入是安全的。** 内容哈希未变时 `ingest` 不会重解析；源文件消失会把引用它
  的页标为 `stale`，文件恢复且哈希未变时自动复位；内容变化则来源回到 `pending`、引用页
  转 `stale`，必须重读重写后才能 `settle`。
- **先读再写。** 用 `read` 的 `--offset/--limit` 翻页，长文档不要一次读到底。
- **草稿别放知识库根目录。** 根下除点目录外的每个文件都会被当成来源；草稿放 `.drafts/`
  或库外，否则下次 `ingest` 会凭空多出来源。
- **查询是定位，不是证明。** `query` 只返回命中子集，回答“是否全覆盖”必须回到来源。

## 页面写法

页面是 Markdown，frontmatter 由 CLI 生成，不要手写。正文建议四段：

1. 一句话定义/结论；
2. 分点的事实（数字、时间、文号、责任主体）；
3. 与其他页的 `[[链接]]`；
4. `## 证据与边界`：来源文件名 + 本文覆盖范围 + 明确没覆盖的部分。

标题用名词短语，summary 写成可独立阅读的一句话（它会出现在检索结果里）。
类型默认 `concept`（概念/要求）、`entity`（组织/系统/人物）、`process`（流程/专项工作）、
`note`（杂项）；可在 `.wiki/wiki.json` 的 `schema.types` 增删。

## 命令速查

| 目的 | 命令 |
| --- | --- |
| 初始化/查看 | `init`、`status`、`doctor [--fix]` |
| 摄入 | `ingest [--dry-run] [--force]` |
| 读原文 | `read "raw:<key>" [--offset N] [--limit N]` |
| 读页面 | `read "page:<id>"` |
| 检索 | `query <词...> [--limit N]` |
| 看单页与链接 | `query --page <id>` |
| 写/改页 | `page put --id ... --title ... --type ... --summary ... --source ... --body <file|->` |
| 删页 | `page delete <id> --yes` |
| 收口来源 | `settle [<key>...] [--skip]` |

所有命令支持 `--json`（机器可读，含 `ok`、`error.code`、`error.next`）。退出码：
`0` 成功，`1` 部分失败/有阻断项，`2` 用法或输入错误，`3` 状态不满足。失败时读
`error.next` 再行动，不要猜。

## 环境与依赖

- **必需**：Python 3.8+（仅标准库）。首次运行自动创建 `.wiki/`，无需安装步骤。
- **PDF**：优先系统 `pdftotext`（poppler）；没有则尝试 `pip install pypdf`；两者都没有
  时 PDF 会进入 `unsupported`，技能会提示而不是静默跳过。
- **DOCX/HTML/TXT/Markdown**：标准库直接解析。
- 图片/扫描件：不解析；在 `unsupported` 里说明，需要时由用户提供 OCR 后的文本。

## 安装到其他 agent

技能目录可整体复制。安装、目录约定与各运行时的放置位置见
[references/install.md](references/install.md)；也可运行
`python3 scripts/install.py --target auto` 自动放入已存在的技能目录。

## 进一步阅读

- [references/cli.md](references/cli.md)：完整命令契约、JSON 字段、错误码。
- [references/curation.md](references/curation.md)：页面边界、证据标准、冲突处理。
- [references/recovery.md](references/recovery.md)：源消失、解析失败、并发与备份。
- [references/verification.md](references/verification.md)：可复现的自测与验收记录。
