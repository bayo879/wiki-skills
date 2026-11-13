# 故障与恢复

原则：**`.wiki/wiki.db` 是账本与资产，页面文件只是镜像。** 旧版可以"删库重跑"，
这一版不行：页面正文、修订、证据都在账本里。备份优先，重跑其次。

## 一、来源解析失败

`ingest` 的 `unsupported` 给出后缀与提示，`oversize` 给出限额。按顺序处理：

1. **PDF 无文本层**（扫描件）：不要建页，也不要自行安装 OCR 或下载语言包。让用户提供
   OCR 文本或可复制版本，另存为 `.md` 后再 `ingest`。
2. **PDF 有文本但乱序**：`pdftotext -layout` 已默认开启；仍乱序时用 `read raw:<key>`
   抽查前几十行，必要时 `--force` 重跑；不要基于乱码写页。
3. **DOCX 解析为空**：确认是 `.docx` 而非改扩展名的 `.doc`；老 `.doc` 先用办公套件另存。
4. **编码问题**：文本按 UTF-8 → GB18030 → Latin-1 降级；仍异常时抽查 raw，不要凭印象补。
5. **单文件超限**：默认 50 MB。调大 `.wiki/wiki.json` 的 `ingest.max_source_bytes`，或把
   大文件移出知识库根目录。

解析失败不阻塞其他文件；`ingest` 退出码 1 只表示"有需要处理的项目"。

## 二、来源消失或内容变化

- 源文件被删除/改名：下次 `ingest` 标 `missing`，引用页变 `stale`，正文保留。回答时要说明
  "来源已不在原位"。
- 源文件恢复且摘要不变：自动回到 `done` / `stable`，不需要重写页面。
- 源文件内容改变：来源回到 `pending`，raw 被覆盖，引用页变 `stale`，并重新入队 route 任务。
  按 route → merge → 重读 raw → 提交新修订的顺序处理；旧结论失效就在正文里改掉，不要只在
  末尾追加"补充"。

## 三、提交被拒

被拒任务回到 `queued`，`last_error` 记录原因；看 `error.next`，不要原样重交。

| 错误 | 处理 |
| --- | --- |
| `E_EVIDENCE_NOT_FOUND` | 重读 raw，逐字复制引文（比较时忽略空白） |
| `E_SOURCE_DRIFT` | 来源在领取后被改；重新 `next` 取新任务 |
| `E_TASK_STALE` | epoch 过期或任务已关闭；重新 `next` |
| `E_TASK_SCOPE` | 只提交任务 `work.sources` 里的来源 |
| `E_PAGE_CONFLICT` | 页面已被别的任务改过；重新 `next`，基于新 rev 重写 |
| `E_LINK_UNRESOLVED` | 先建链接目标页，或去掉该链接 |

## 四、镜像被改（drift）

除了 `page import` / `page restore`，任何对 `.wiki/pages/*.md` 的直接修改都不生效：

1. 下一次任意命令启动时，`ensure_mirror()` 发现文件与账本渲染结果不一致；
2. 原文件先复制到 `.wiki/drift/<page>.<时间戳>.md`，并记入 `drift` 表；
3. 镜像按账本重写；没有账本对应的孤儿文件也移入 `drift/`。

要保留手改内容，用 `page import <id> --from <文件>` 提交成新修订，再把 drift 副本删掉。
`status` 会报告 drift 数量，`doctor` 会列出最近条目。

## 五、数据库损坏与并发

- `wiki.db` 损坏（`doctor` 报 `E_DB` 或完整性检查失败）：从备份恢复整个 `.wiki/`。
  没有备份时，页面正文只能从 `.wiki/pages/` 或 `export` 产物里人工抢救，且无法自动重建
  任务与修订历史——这是本版最需要避免的故障。
- 并发：SQLite 有 busy timeout，但不要并行跑两个写命令。多个 agent 同时工作时，用
  "一个写者 + 其他只读" 的约定；任务 lease 只能防止同一任务被重复领取，不解决库级写冲突。
- 中断：数据库写入在事务内，镜像用临时文件 + 原子替换；中断后重跑同一条命令即可。
  已领取未提交的任务在 15 分钟后可被重新领取。

## 六、备份与迁移

```bash
# 最小备份：账本 + 配置
tar czf wiki-ledger-$(date +%F).tgz .wiki/wiki.db .wiki/wiki.json

# 完整备份（含 raw 与镜像，可离线还原）
tar czf wiki-full-$(date +%F).tgz .wiki

# 只导出可读页面（不含账本，用于浏览或迁移参考）
python3 <技能>/scripts/wiki.py export ./wiki-export
```

整个根目录搬迁时连 `.wiki/` 一起复制；`sources.key` 是相对路径，根目录移动不影响引用。
只复制技能目录不会带走知识库。

## 七、什么情况下停下来问人

- 两份来源对同一事实给出互斥口径，且都像正式文件。
- 来源涉及个人隐私、合同金额、未公开人事信息，而用户没有说明可否入库。
- 需要 OCR、解密或访问知识库之外的系统。
- 页面大量 `stale` 且无法判断应以哪版为准。
- 需要删除页面或整批重建账本（当前没有删除命令）。

停下来时说明：哪一页、哪两个来源、冲突点、可选处理方式。不要自行选一个继续。
