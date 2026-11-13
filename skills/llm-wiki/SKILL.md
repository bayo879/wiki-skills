---
name: llm-wiki
description: 把本地文档整理成可检索、可追溯的 Markdown 知识库。引擎(scripts/wiki.py)独占扫描、解析、账本与页面落盘，agent 只通过 next 领任务、submit 交 JSON 语义载荷。适用于「建个 wiki」「把这些资料整理成知识库」「批量摄入这批文档」「查一下已有知识」等场景。仅依赖 Python 标准库；PDF 可选 pdftotext 或 pypdf。
---

# llm-wiki：任务驱动的知识库

知识库的状态只存在 `.wiki/wiki.db` 账本里。引擎负责确定性工作（扫描、解析、
哈希、任务状态、校验、渲染页面镜像），你负责语义工作（页面边界、正文、证据）。

**你不需要也不允许直接写页面文件或数据库。** 唯一入口是下面这些命令：

```bash
python3 <skill>/scripts/wiki.py <command> --json
```

`.wiki/pages/*.md` 只是账本的自动导出镜像，供人阅读和 git 查看；直接改它会被
引擎还原，改动会存进 `.wiki/drift/` 并记入审计。需要人工改页时用 `page import`
（见 references/cli.md）。

## 何时使用

- 把一个目录（公文、笔记、论文、网页存档）整理成可查询的知识库。
- 已有 wiki 要增量摄入新文件，或核对某页是否过期。
- 写作/决策前检索既有知识，并给出处。

不适用：一次性问答、纯网页抓取、扫描件 OCR（本技能只把无文本层的文件标为
`unsupported`，不做 OCR，也不下载任何依赖）。

## 主循环

在知识库根目录执行（或给命令加 `--root <目录>`）。全程加 `--json` 读机器输出。

```bash
# 0. 首次：初始化；日常：看状态与待办
python3 wiki.py init --json
python3 wiki.py status --json

# 1. 扫描来源，产出 pending 与 route 任务
python3 wiki.py ingest --json

# 2. 领任务（默认一次 20 个，按 ingest.batch 配置）
python3 wiki.py next --json

# 3. 对每个 route 任务：读原文 → 判断归属 → 提交决定
python3 wiki.py read "raw:<key>" --offset 1 --limit 120 --json
python3 wiki.py submit <t_xxx> --epoch <n> --payload route.json --json

# 4. 领 page 任务：写正文 → 提交页面
python3 wiki.py next --kind page --json
python3 wiki.py submit <t_xxx> --epoch <n> --payload page.json --json

# 5. 收尾体检
python3 wiki.py doctor --json
```

`next` 输出的每个任务都带 `id`、`epoch`、`work`。`epoch` 是领取凭证，提交时必须
原样带回；任务租约 15 分钟，过期未提交会被自动回收，重新 `next` 即可。

## 任务与载荷

只有两种任务，载荷都是 JSON。

**route**：一个来源一条。决定这个来源进哪一页，或跳过。

```json
{"decisions": [{"source": "docs/a.md", "action": "new",
                "page": "refund-process", "title": "退款流程",
                "type": "process", "reason": "描述完整的退款办理步骤"}]}
```

- `action`：`new`（新建页，须给 `page/title/type`）、`merge`（并入已有页，只给
  `page`）、`skip`（不建页，须给 `reason`）。
- `page` 是小写 slug；`type` 取 `.wiki/wiki.json` 的 `schema.types`。
- 一个来源一条决定，`source` 必须与任务里的 key 完全一致。

**page**：一页一条。写这一页的标题、摘要、正文、证据与边界。

```json
{"page": "refund-process", "title": "退款流程", "type": "process",
 "summary": "退款需在签收后 7 天内申请，原路退回。",
 "links": ["refund-policy"],
 "body": "## 结论\n\n……\n",
 "evidence": [{"source": "docs/a.md", "quote": "签收后七日内可申请退款"}],
 "boundaries": {"covered": "线上订单退款", "not_covered": "线下门店与跨境订单"}}
```

校验规则（不满足会返回 `E_*`，任务回到队列，原因为 `last_error`）：

- `body` 不超过 60000 字符，`summary` 不超过 200 字符；正文不要带 frontmatter。
- `evidence` 至少一条；`quote` 去掉空白后必须逐字出现在该来源的 raw 文本里。
- `boundaries.covered` 与 `boundaries.not_covered` 都必填。
- `links` 指向的页面必须已存在；`page` 必须等于任务主题。
- 正文可以写在文件里，用 `"body_file": ".drafts/refund.md"` 代替 `body`。草稿放
  隐藏目录（如 `.drafts/`）或知识库外；根目录下非隐藏文件会被当成来源。

## 规则

- **先读后写。** 每个事实都要能回到 `read "raw:<key>"` 的原文；不要凭记忆写页。
- **一页一个主题。** 同一份材料的不同方面若会被分别检索，才拆页。
- **来源变化会让页面 `stale`。** `status` 里 stale 的页需要重读新材料再更新；
  不要绕过任务直接改页。
- **重复摄入是安全的。** 内容哈希未变不重解析；文件消失标 `missing`，恢复后自动复位。
- **合并优先。** 已有页面能容纳新事实时用 `merge`，不要制造近似重复页。
- **`query` 是定位，不是覆盖证明。** 它只返回命中子集。

## 可选：把任务投影到 todo

如果你的运行时有 todo/任务清单工具，可以按 `next --json` 的结果建一组待办：
一条 todo 对应一个任务，文案里带任务 `id` 与目标页面，`description` 写载荷要点。
只有 `submit` 返回成功才把对应 todo 标完成；失败则保留并把 `error.next` 记进去。
`status --json` 的 `tasks` 与 `pending` 可在会话恢复后重建 todo。

todo 只是显示层：进度以账本为准；没有 todo 工具也不影响流程。

## 命令速查

| 目的 | 命令 |
| --- | --- |
| 初始化 / 状态 / 体检 | `init`、`status [--brief]`、`doctor [--fix]` |
| 摄入 | `ingest [--dry-run] [--force]` |
| 领任务 | `next [--kind route\|page] [--batch N]` |
| 提交 | `submit <id> --epoch N --payload <file\|->` |
| 退回任务 | `release <id>` |
| 读原文 / 读页面 | `read "raw:<key>"`、`read "page:<id>"` |
| 检索 / 看单页 | `query <词...>`、`query --page <id>` |
| 人工改页 | `page import <id> --from <file>` |
| 回滚页面 | `page restore <id> --rev N` |
| 导出镜像 | `export <dir>` |

所有命令支持 `--json`。退出码：`0` 成功，`1` 有阻断项（如解析失败、超限），
`2` 用法或载荷错误，`3` 状态不满足。失败时读 `error.code` 与 `error.next` 再行动。

## 环境

- Python 3.8+，仅标准库；首次运行自动创建 `.wiki/`。
- PDF：优先系统 `pdftotext`，否则尝试 `pypdf`；都没有时进入 `unsupported`。
- 扫描件、图片：不解析，`unsupported` 会提示需要外部 OCR 得到的文本；本技能
  不安装依赖、不下载模型、不访问网络。
- DOCX/HTML/TXT/Markdown 等：标准库直接解析。

## 进一步阅读

- [references/cli.md](references/cli.md)：完整命令契约、JSON 字段、错误码。
- [references/curation.md](references/curation.md)：页面边界、证据标准、冲突处理。
- [references/install.md](references/install.md)：安装到其他 agent、目录约定。
- [references/recovery.md](references/recovery.md)：源消失、任务卡住、镜像漂移。
- [references/verification.md](references/verification.md)：自测与验收记录。
