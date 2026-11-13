# 验证记录

本文件记录这套技能在 2026-11-13 的实测结果，用于交付验收和回归对照。所有数字来自实际
命令输出，不是估算。

环境：macOS，Python 3.15.2，SQLite 3.54.1，pdftotext 25.11.0（Poppler）。没有安装
`pypdf`，因此 PDF 走的是 `pdftotext` 分支。

## 一、自测：12/12 通过

```bash
python3 scripts/selftest.py
```

| 用例 | 覆盖点 |
| --- | --- |
| 首次 ingest 自动建库并分类格式 | 无 `.wiki/` 时自动初始化；Markdown/HTML/DOCX 分流 |
| read 支持窗口与越界 | `--offset/--limit`、`next_offset`、越界返回空 |
| 建页与 stdin 正文 | `page put --body -`、frontmatter 生成 |
| settle 收口并保留未处理来源 | 未引用来源被阻断；`--skip` 后才能清零 |
| 检索与双向链接 | 标题/摘要/正文加权、`[[链接]]` 双向索引 |
| 摘要未变不重写 | 重复 `ingest` 不动 raw、不改页面 |
| dry-run 无副作用 | 已初始化库文件哈希不变；**未初始化目录不创建 `.wiki/`** |
| 来源消失标记 stale、恢复后复位 | `missing`、`L10` 诊断、恢复后回 `done/stable` |
| 来源变更需重新收口 | 内容变化→页面 `stale`→`settle` 退出 1→重写后通过 |
| 非法输入返回稳定错误码 | `E_BAD_TYPE`、`E_CONFIRM`、`E_PATH` |
| doctor 最终健康 | `healthy: true` |
| 含空格与中文路径下可搬迁 | 复制到 `/tmp/.../技能 目录/` 后仍可运行 |

输出以 `12/12 passed` 结尾，退出码 0。

## 二、真实语料：4 份中国电信 PDF

从只读语料目录复制 4 份 PDF 到隔离目录，原文件不动：

| 来源（节选文件名） | 解析字符数 | 建页 id |
| --- | --- | --- |
| `676516_...云网安全特别主题日活动...讲话...pdf` | 3021 | `cloud-network-safety-day` |
| `676532_...关于进一步加强反诈模型规范性运营管理的通知...pdf` | 29357 | `anti-fraud-model-operations` |
| `676593_...火灾隐患排查整治专项工作检查情况的通报...pdf` | 3574 | `fire-hazard-inspection-2026` |
| `676604_...天翼云一城一池标准化维护实施方案...pdf` | 66627 | `tianyi-cloud-one-city-one-pool` |

结果：

- `ingest`：4 份全部由 `pdftotext` 解析，`unsupported` 为 0；重复执行时 3 份
  `unchanged`，只有改动过的那份重新解析。
- `page put`：4 页写入成功；正文取自 raw 片段，未整篇复制。
- `settle`：`settled 4, skipped 0, blocked 0; 0 pending`，退出码 0。
- `query`：`反诈`→`anti-fraud-model-operations`；`火灾`→`fire-hazard-inspection-2026`；
  `天翼云`、`一城一池`→`tianyi-cloud-one-city-one-pool`，均命中 1 页。
- `doctor`：`healthy: true`，退出码 0。

### 源内容变化

向 `676593` 的副本追加尾注字节后重新 `ingest`：

```text
ingested 1 sources; 1 waiting for curation | untouched 3
settle: exit 1, blocked 1
  reason: every citing page is stale; re-read the changed source and rewrite the page
status: stale_pages = ["fire-hazard-inspection-2026"], pending = 1
```

按 raw 重写该页后：

```text
settle: exit 0, settled 1, blocked 0; 0 pending
doctor: healthy true
```

### 全量语料 dry-run（76 份 PDF）

对只读语料目录整批跑 `ingest --dry-run`，原文件与目录均未写入：

```text
dry run: 66 new or changed, 0 unchanged, 9 unsupported
exit 0
```

- 66 份由 `pdftotext` 解析，共 831,175 字符，单份 866–76,198 字符。
- 9 份无文本层（扫描件），进入 `unsupported`，提示需要外部 OCR。
- 1 份 80,298,855 字节，超过默认 `max_source_bytes=50MB`，进入 `oversize`。

这轮全量扫描暴露并修复了一个崩溃：无文本层的 PDF 在后端全部返回空时，
`pdf_to_text` 返回 `None` 而不是空串，`command_ingest` 里 `text.strip()` 直接抛
`AttributeError: 'NoneType' object has no attribute 'strip'`。现改为返回 `""`，
并在 `extract_text` 对 PDF 分支兜底 `text or ""`；修复后同一批语料 dry-run
退出 0，selftest 仍 `12/12 passed`。

## 三、边界与错误码

| 场景 | 结果 |
| --- | --- |
| 未初始化目录跑 `ingest --dry-run` | 不创建 `.wiki/`，目录内容原样 |
| 放入 `.png` | 进入 `unsupported`（`no text backend produced content`），不中断其他文件 |
| `max_source_bytes = 500` 时摄入 2 KB 文件 | 进入 `oversize`，退出码 1，不写 raw |
| `read ../outside.md` | `E_PATH`，退出码 2 |
| `page put --id "Bad Id"` | `E_BAD_ID`，退出码 2 |
| `page put --source nope.md` | `E_UNKNOWN_SOURCE`，退出码 2 |
| 已存在页面 `page delete` 不带 `--yes` | `E_CONFIRM`，退出码 3 |
| `query --page missing` | `E_NOT_FOUND`，退出码 3 |
| 空库 `settle` | `E_EMPTY`，退出码 3 |

## 四、安装

在临时 HOME + 项目内已有 `.dsh/skills` 的条件下：

| 操作 | 结果 |
| --- | --- |
| `install.py --target auto` | 装入项目 `.dsh/skills/llm-wiki`，安装后 `--version` 自检通过 |
| 再执行一次 | `skipped`，退出码 0，不覆盖 |
| `--force` | 旧目录改名为 `llm-wiki.bak-<时间>` 后重新复制 |
| `--target "<含空格目录>"` | 安装成功，可在该路径直接运行 |
| 无任何已知技能根 | `E_NO_TARGET`，提示原地加载 `SKILL.md` 或显式传 `--target` |

## 五、未覆盖

- `pypdf` 分支：环境没有该库，未实测；代码路径存在，但只验证了 `pdftotext`。
- OCR/扫描件：明确不支持，只会在 `unsupported` 中提示。
- 旧 Rust CLI 初始化过的 `.wiki/`：配置与表结构不同，不兼容，需在新目录重新摄入。
- 规模与性能：已对 76 份 PDF 整批 dry-run，未做万级文件、实际全量入库或并发写入压测。
- DSH 之外的 agent 运行时：按目录约定复制，未逐个实测其技能加载器。
