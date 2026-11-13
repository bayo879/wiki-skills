# 验证记录

本文件记录 2026-11-13 在 skills-only 版本（`VERSION=0.2.0`）上的实测结果，用于交付验收
与回归对照。数字来自实际命令输出，不是估算。

环境：macOS，Python 3.15.2，SQLite 3.54.1，pdftotext 25.11.0（Poppler）。没有安装
`pypdf`，PDF 全部走 `pdftotext` 分支。

## 一、自测：20/20 通过

```bash
python3 skills/llm-wiki/scripts/selftest.py
```

| 用例 | 覆盖点 |
| --- | --- |
| `init` 建库 | 生成 `.wiki/`、`wiki.db`、`wiki.json`，来源为 0 |
| `ingest` 排队 | 每个来源一条 route 任务，`pending_total` 正确 |
| 图片分类 | `.png` 进 `unsupported`，不影响其他文件 |
| route 领取 | 一个来源一条任务，`work.sources` 带 raw 路径 |
| `read` raw | 返回解析文本，`content` 为正文 |
| `skip` 决定 | 被拒决定返回可读错误，接受后来源变 `skipped` |
| route `new` | 入队 page 任务，任务带来源与 raw |
| page 提交 | 生成 rev1 与 `.wiki/pages/<type>/<id>.md` |
| 任务关闭后重交 | `E_TASK_STALE`，退出码 3 |
| `status` | 页数、stale、开放任务统计正确 |
| `query` | 命中页数与 id 正确 |
| `read` page | 读账本而不是镜像文件 |
| 镜像被改 | 原文进 `.wiki/drift/`，镜像按账本还原 |
| `page import` | 生成 rev2，`origin=import` |
| `page restore` | 回滚内容生成 rev3 |
| `export` | 页面镜像复制到目标目录 |
| 超限 | `max_source_bytes=10` 时 100 字节文件进 `oversize`，退出码 1 |
| `--dry-run` | 未初始化目录不创建 `.wiki/` |
| `doctor` | 跑完并返回结构化 issues |

输出以 `20/20 checks passed` 结尾，退出码 0。`--json` 输出可供 CI 解析。

## 二、真实语料：4 份中国电信 PDF

从只读语料目录复制到 `/tmp` 隔离目录，原文件不动。

| 来源（节选） | 解析字符数 | parser |
| --- | --- | --- |
| `676516_…云网安全特别主题日活动…讲话` | 3021 | pdftotext |
| `676532_…反诈模型规范性运营管理…通知` | 29357 | pdftotext |
| `676593_…火灾隐患排查整治专项工作检查情况…通报` | 3574 | pdftotext |
| `676604_…天翼云一城一池标准化维护实施方案` | 66627 | pdftotext |

另复制一份 80,298,855 字节的合同 PDF：`ingest` 将其归入 `oversize`（默认
`max_source_bytes=50000000`），退出码 1，其余文件照常解析。

全流程（真实任务闭环）：

```text
ingest: 4 sources ingested, 4 route tasks queued; 4 pending; 1 oversize
next --batch 10: 4 route tasks
route: skip × 3, new × 1 (fire-hazard-inspection-2026) → 1 page task
page submit: rev 1
doctor: healthy true, 0 issues
status --brief: 5 sources (0 pending, 0 unsupported, 1 oversize), 1 pages (0 stale)
```

页面证据取自 raw 第 4 个非空行，`submit` 校验通过；把引文改错一个词则返回
`E_EVIDENCE_NOT_FOUND`。

## 三、边界与错误码

| 场景 | 结果 |
| --- | --- |
| 未初始化目录跑 `ingest --dry-run` | 不创建 `.wiki/`，目录内容原样 |
| 放入 `.png` | `unsupported`（no text backend produced content），不中断其他文件 |
| `max_source_bytes=10` 摄入 100 字节文件 | `oversize`，退出码 1，不写 raw |
| 已关闭任务重交 | `E_TASK_STALE`，退出码 3 |
| 载荷引用任务范围外来源 | `E_TASK_SCOPE`，退出码 3 |
| 引文不在 raw 里 | `E_EVIDENCE_NOT_FOUND`，退出码 2 |
| 缺 `boundaries` | `E_PAYLOAD`，退出码 2 |
| `read ../outside.md` | `E_PATH`，退出码 2 |
| `doctor` 有 `fail` 级问题 | 退出码 1 |

## 四、安装

`scripts/install.py` 只做目录复制与自检，不写全局状态：

| 操作 | 结果 |
| --- | --- |
| `--target auto` | 装入第一个已存在的技能根，安装后 `wiki.py version` 自检通过 |
| 再执行一次 | `skipped`，退出码 0，不覆盖 |
| `--force` | 旧目录改名为 `llm-wiki.bak-<时间>` 后重新复制 |
| `--target "<含空格目录>"` | 安装成功，可在该路径直接运行 |
| 无任何已知技能根 | `E_NO_TARGET`，提示原地加载 `SKILL.md` 或显式传 `--target` |

## 五、未覆盖

- 66 份全量语料的 dry-run 与真实入库：只跑了 4 份 + 1 份超限样本。
- `pypdf` 分支：环境没有该库，未实测。
- OCR/扫描件：明确不支持，只在 `unsupported` 中提示。
- 并发写入与多 agent 同时提交：只验证了 lease 与 epoch 的单进程语义。
- 万级文件、`MAX_RAW_CHARS` 截断边界、超大页面的渲染性能。
- DSH 之外的 agent 运行时：按目录约定复制，未逐个实测其技能加载器。
- 页面删除：当前没有命令，未验证。
