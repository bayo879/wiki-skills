---
name: wiki-sync
description: 当宿主定时任务唤起本技能，或上游同步服务刚把一批文件增删改同步进知识库的源目录时使用。它只做一件事：调用同步命令跑完一轮无人值守同步，依次拿锁、扫描、等待文件写稳、限量转换、稳定准入、开摄取轮、重建索引并写审计；随后读审计目录下的当日报告，按退出码区分正常、部分失败和锁被占用，并在同一源连续失败时退避。单个源的交互式摄取、仓库初始化、候选批准与合并都不在这里做，也不要把它拆成扫描、转换、摄取多步执行。每轮只跑一条命令，不替人拍板。
---

# wiki-sync

无人值守的一轮同步。调度器唤起后，先跑一条命令，读结论，决定要不要叫人。

## 何时用

- 定时任务到点。
- 上游同步服务刚把一批文件增删改同步进 `source/`。
- 人问“昨晚同步跑了吗”“为什么新文件还没进 wiki”。

不在本 skill：`init`、单个源的交互式摄取（交给 `wiki-ingest`）、批准与合并。

## 一条命令

```bash
llm-wiki sync --json
```

- 默认每轮最多转换 `sync.max_per_run`（20）个源；大库用 `--limit N` 分批，别一次梭哈。
- `--dry-run` 只报告：不转换、不写 audit、不动 `sync_state`。先演练时用它。
- 不要在定时任务里拆成 `scan` + `convert` + `ingest`：编排在 `sync` 里，拆开就绕过锁、settle 窗口和稳定门槛。
- 不要无人值守跑 `--force`：它跳过写稳与稳定门槛，等于放弃防半写。

`--json` 只输出一行信封；成功也是 `ok:true`，结论看退出码和 `data`。

## 它自己做什么（不要重复）

1. 拿 `.wiki/lock`；被别人占用退 4（写不了是 2）。
2. 扫 `source/`，分类 new、changed、moved、missing、duplicate、oversize。
3. 跳过 settle 窗口内刚写过的文件（`sync.settle_seconds`，默认 10 秒），下一轮再来。
4. 按 `--limit` 转换，没轮到的留在 `queued`。
5. 复核 `raw/`：同一源连续 `sync.stable_streak`（默认 3）轮内容哈希一致，才算稳定。
6. `sync.auto_ingest` 为真且源已稳定时，调 `ingest begin --sources ...` 开一轮；否则不碰页。
7. 重建 `wiki/index.md`。
8. 追加 `wiki/audit/<日期>-sync.md`。

`auto_ingest` 默认关闭；是否开启由人一次性决定，本 skill 不改配置。agent 只读结果、解释、叫人；不要手跑 `scan`/`convert` 复核，也不要重复 `ingest begin`（会覆盖已开的轮次）。

## 退出码就是结论

| 码 | 含义 | 动作 |
|---|---|---|
| 0 | 跑完，无失败 | 读 audit，收工 |
| 1 | 跑完，但有源失败 | 看 `data.failures` 和 `data.warnings`，逐源处理 |
| 2 | 配置、扫描或写盘错误 | 本轮不算跑过；修环境，下轮重跑 |
| 4 | 另一个进程持锁（exit 4） | 不重试、不删 `.wiki/lock`；等下一轮 |

`1` 时 stdout 仍是 `ok:true`；别把 `failures` 当致命错误，也别吞掉。`4` 时不要循环重试；崩溃残留的锁会按 TTL 自动回收。

## 失败退避

同一源连续失败 3 轮，CLI 把 `auto_ingest_disabled=1` 写进 `sync_state`（`.wiki/state.db`），不改 `wiki.toml`。

- 看到退避，不要劝人重开 `auto_ingest`，也不要手改配置把门槛调低。
- 先读 audit 的 `warnings`，定位是解析器、编码、超大文件，还是源本身坏了。
- 修好后交人决定是否恢复；恢复前不要假装同步在正常推进。

## 每轮读 audit

路径 `wiki/audit/<YYYY-MM-DD>-sync.md`，同一天多轮用 `---` 追加。读最后一段：

- `changes`：本轮变了什么（JSON 里是小写 `new`/`changed`/`moved`/`missing`/`duplicate`/`oversize`，audit 里是 `New`/`Changed` 等）。
- `ingested`：交给摄取轮、进了 review 队列的源。
- `warnings`：settle 跳过、失败、退避原因。
- 头部计数：`converted`、`queued`、`failures`。

audit 只到源级。要看具体页，用 `changes` 里的 key（或 `ingested` 的源）执行 `llm-wiki ingest begin --dry-run --sources <KEY>`，读 `affected`，再核这些页的 `status`；不要凭空写页名。

## 什么时候叫醒人

- `changes` 里有 `changed`：对 key 执行 `llm-wiki ingest begin --dry-run --sources <KEY>`，`affected` 里有 `status: stable` 的页就叫醒人，说明已批准的结论可能过期。
- `changes` 出现 `oversize`：超过 `max_source_bytes`，不会被自动转换；列清单，别静默跳过。
- `blocked_by_stability`：大文件持续被写，多轮仍是 `changed` 或卡在 settle，永远到不了 `stable_streak`；说明情况，不要反复空跑。
- 退避已触发：同一源连续 3 轮失败。

叫醒时用四选项，不替人选：

> 本轮同步需要你决定：<一句话 + 关键 key>。
> 1. 批准
> 2. 不批准，并说明原因
> 3. 自己补充要求
> 4. 先放着

人选完后交给对应流程执行；本轮不自动合并。

## 定时任务

先把仓库根写进 `WIKI_ROOT`。cron 每 15 分钟一轮：

```cron
*/15 * * * * cd "$WIKI_ROOT" && llm-wiki sync --json --limit 20 >> .wiki/sync.log 2>&1
```

PowerShell 注册计划任务（首次 09:00，之后每 15 分钟重复）：

```powershell
Register-ScheduledTask wiki-sync -Trigger (New-ScheduledTaskTrigger -Once -At 09:00 -RepetitionInterval (New-TimeSpan -Minutes 15)) -Action (New-ScheduledTaskAction -Argument "sync --json --limit 20" -WorkingDirectory $env:WIKI_ROOT -Execute llm-wiki)
```

## 三条铁律

1. 不删页、不合并页、不删源。
2. 自动写出的页一律 `status: draft`，人工审后才改 `stable`。
3. 不可逆动作一律进队列等人拍板，不自动执行。

## 不要做

- 不跑 `init`、不初始化仓库。
- 不手改 `wiki.toml`、不改 `stable_streak`、不跳 settle 窗口。
- 不在无人值守轮里执行 `approve`、`reject`、`instruct`、`defer`；这些要人给语义。
- 不把 `--dry-run` 的结果当成本轮已同步来汇报。
- 不因为 `failures` 是 0 就跳过 audit；安静轮次也要读 `warnings`。
