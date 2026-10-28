---
name: wiki-audit
description: 当用户要求给知识库做体检、问「有没有说法不一致的地方」，或在 checkpoint、sync 之后、交接前需要确认页面可信时使用。先跑 llm-wiki lint --json 收机械层结论，再读页面正文与 sources 原文做判断层分析，覆盖跨页矛盾、被新源推翻的过时结论、孤儿页与重复页。只摆证据、给建议动作，不自动改页面；机械修复交 lint --fix，正文改动交 wiki-ingest，源变更跟进交 wiki-scan。
---

# wiki-audit

命令是 `llm-wiki`；不在 PATH 上时，用仓库根的 `.wiki/llm-wiki`。

## 何时用

- 用户说「体检」「查一下 wiki 有没有问题」「有没有说法不一致的地方」
- `checkpoint` 前、`sync` 之后、交接前，要确认页面还可信
- 有人怀疑某页结论已经过时

## 职责边界

- 拥有：`lint` 的调用，以及判断层分析——机械问题之外，只有读过正文才能发现的事
- 不碰：机械修复（交 `llm-wiki lint --fix`）、摄取与写页（交 `wiki-ingest`）

## 两个层次

| 层 | 谁判 | 查什么 |
|---|---|---|
| 机械层 | CLI | 断链、重复 id、缺源、未入账的 raw、空页、日志格式……规则固定，可重跑 |
| 判断层 | 你 | 矛盾、过时、孤儿、重复——要读懂正文与 `sources` 原文 |

**编号会漂**：同一件事在不同版本里可能叫 L4 也可能叫 L8，spec 与实现已经错过一次。认检查内容，不认编号；编号只用来跟 `--json` 输出对照。

## 机械层

先确认仓库就绪：`llm-wiki doctor --json` 的 `checks[].level` 有 `fail` 就先处理，缺 `wiki.toml` 跑 `llm-wiki init`。`doctor` 退出 1 只表示有检查项失败，不代表 wiki 坏了。

```bash
llm-wiki lint --json            # 默认：有 error 才 exit 1
llm-wiki lint --strict --json   # warning 也算失败，CI 用
llm-wiki lint --fix             # 先修机械缺陷，再报剩下的
```

- `exit 0`：没有 error（`--strict` 下没有 warning）
- `exit 1`：有 error；`--strict` 时有 warning
- `exit 2`：跑不起来——缺 `wiki.toml`、账本坏了。这是环境问题，别当 finding 汇报

`--json` 的形状是 `data.findings[]`，每条 `{rule, level, page, msg}`，已按严重度排序。`page` 为仓库相对路径，跨页问题可能是 `null`。

### 三档严重度

- `error`：必须修。断链、重复 id、缺源都在这一档，带病发布比不发布更糟
- `warning`：要人拍板。能不能接受取决于场景，别默认「顺手修掉」
- `info`：只报告，不动手。它经常是正常的

### 十三条检查（按内容）

| 检查 | 级别 | 人话 | 常见误报 |
|---|---|---|---|
| 必填字段 | error | 页缺 `id`/`title`/`status`，读不进索引 | 写成 `Status:`，大小写不对被判缺失 |
| 断链 | error | `[[id]]` 指不到任何页 | 反引号里的示例 `[[foo]]` 也算链接（只跳过代码围栏）；指向 `audit/` 报告也算 |
| 重复 id | error | 一个 id 出现在两个类型目录 | 复制一页到新目录当草稿，草稿也查重 |
| status 非法 | error | 只认 `draft`/`stable`/`stale` | 团队自定义 `review`、大写 `Draft` |
| updated 格式 | warning | 必须是 `YYYY-MM-DD` | 写成时间戳 `2026-10-27T10:00:00Z` |
| 缺源 | error | `sources` 指的文件不存在 | 写了 `source/` 原文路径而不是 `raw/` 产物；归档引用豁免 |
| 未入账 raw | warning | `raw/` 里有账本不认识的文件 | 新 clone 后 `state.db` 为空，整层都报；`scan` 只读不记账，要 `convert` 才补 |
| 空页 | warning | 正文短于 `[audit].min_page_chars`（默认 200 字符） | 导航页、目录页本来就短；归档页豁免 |
| 改了没跟日期 | warning | 文件哈希变了而 `updated` 没动 | 只改了 `tags`/`summary`，正文其实没变 |
| 共享 raw 过多 | info | 一份 raw 被超过 3 页引用 | 规范页被多页引用是合理的 |
| 日志格式 | warning | `log.md` 的 `##` 行不是 `## [date] action \| title` | 手写的说明性二级标题 |
| 未引用 asset | info | 图片等 asset 没被任何页引用 | 刚 `convert` 完还没用上 |
| 归档页零入链 | info | 归档摘要无入链且超 `[memory].idle_days` 未 `recall` | 只用 `recall` 查、不写链接的归档 |

有两个检查不在 `--json` 里，别等它报：

- **索引漂移**：`index.md` 与磁盘页面集合不一致时，`lint --fix` 直接重写，不产生 finding。推论：谁也不许手写 `index.md`
- **近似重复**：CLI 不查（目标态），归判断层，见下节

## 判断层

总则三条：先读 `sources` 原文再下结论；每条结论挂两个页面路径；**发现矛盾不要直接改**。

### 矛盾

1. 用 `llm-wiki query <关键词>` 或 grep 找讲同一件事的页
2. 结论互斥时，打开各自 `sources` 指向的 `raw/` 原文
3. 比三件事：原文日期（`converted_at`）、来源类型（`origin: conversation` 的页可信度低于文档页）、是否一手
4. 把证据摆出来让人决定，建议动作写「改哪页、改成什么」，不执行

### 过时

1. `llm-wiki scan --json`，找 `kind` 为 `changed` / `missing` 的项。只当证据，不做变更解释
2. 反查谁引用它：grep 页面的 `sources`，比对时去掉 `#p=..` 片段
3. 页面 `updated` 早于 raw 的 `converted_at` → 结论可能已被新源推翻
4. 建议动作：交 `wiki-ingest` 重写该页；来不及就先报告，由人决定是否标 `stale`

### 孤儿

- `sources` 全指不到东西：lint 报缺源，但它只查存在性。要判断「页还在、结论已无源支撑」
- 零入链：`llm-wiki graph --json` 的 `orphans` 直接就是名单，`nodes[].in_degree` 给的是扇入度；要确认「正文也没提过它」再 grep 一次 `\[\[<id>\]\]`
- 结构信号：`graph` 的 `broken` 是「有人想链但链不到」的知识缺口，比 lint 的断链报告更适合拿来提「该补哪页」

### 重复

- 信号：标题归一化后相同、`summary` 高度相似、两页 `sources` 大面积重叠
- 判合并还是分：同一个概念给同一读者 → 合并；同一事实不同用途（教程 vs 参考）→ 分并加互链
- 动作一律交 `wiki-ingest`

## 出报告

一次只报最要紧的 3–5 条，按影响面排序。每条三行：

```
问题：concept/retry 与 guide/retry-policy 对最大重试次数给出互斥结论
证据：wiki/concept/retry.md:12 说 3 次；wiki/guide/retry-policy.md:8 说 5 次；两页分别引 raw/api.md 与 raw/handbook.md
建议：以较新的 raw/handbook.md 为准，改 guide/retry-policy.md；需人工确认
```

- 完整清单落 `wiki/audit/YYYY-MM-DD-<主题>.md`；`audit/` 不进索引、不参与 lint，写完提醒用户提交
- `info` 默认不进会话汇报，除非它指向真实风险
- 没有 finding 就直说 clean，不凑数

## 交接

| 需要 | 交给 |
|---|---|
| 改页、补源、合并 | `wiki-ingest` |
| 源变了要跟进（`convert`、变更解释） | `wiki-scan` |
| 定时体检、无人值守 | `wiki-sync` |

## 别做

- 不手写 `index.md`、`log.md`、`updated`
- 不用 `--fix` 掩盖要判断的问题；它只修 `updated`、正文变了没跟日期、索引漂移
- 发现矛盾不直接改页，不猜哪个对
- 不把所有 `info` 倒给用户
- 不改 `[audit].min_page_chars` 来消 warning
