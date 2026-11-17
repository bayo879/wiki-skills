# 未提交的任务被 `next` 再次发放，而不是报出「当前任务尚未提交」

## 问题

用户提出：`next` 提取任务后未提交，再次 `next` 会返回同一个任务；期望是体系明确告知
「当前 `next` 提取的任务 xxx 尚未完成提交」，而不是把同一个任务再次当作可取的新任务发回。

在临时工作区用 0.4.1 复现（一个源、一条 route 任务）：

```
ingest --json  → summary: "ingested 1 sources, queued 1 route tasks; ..."
next --json    → summary: "1 task(s) ready; read each brief, fill its payload.json, then submit"
                 tasks[0].id = t_1ec81f7a
next --json    → summary: "resuming 1 unfinished task(s); keep the payload you already have and submit"
                 tasks[0].id = t_1ec81f7a，brief/template/work 与第一次逐字相同
```

同一条任务行在两次 `next` 前后：

```
before: t_1ec81f7a  state=leased  epoch=2  attempts=2  lease_until=2026-11-17T04:28:15Z
after : t_1ec81f7a  state=leased  epoch=3  attempts=3  lease_until=2026-11-17T04:28:39Z
```

即：第二次 `next` 既没有给出「已有任务未提交」的结论，也没有停在原处，而是续租、加 epoch、
加 attempts，然后把同一包内容原样发回。

代码位置：

- `Wiki.claim_tasks`（`skills/llm-wiki/scripts/wiki.py:1443-1460`）先把过期租约回队，再
  `_claim_queued`；不足一批时用 `_resume_leased` 补齐，返回值是 `(claimed, resumed)`。
- `Wiki._resume_leased`（`skills/llm-wiki/scripts/wiki.py:1484-1513`）对每个仍在租约里的行执行
  `UPDATE tasks SET state = 'leased', epoch = epoch + 1, lease_until = ?, attempts = attempts + 1`
  （`skills/llm-wiki/scripts/wiki.py:1508-1511`），并把同一个 id 放进 `resumed`。
- `command_next`（`skills/llm-wiki/scripts/wiki.py:1999-2032`）直接把 `claimed + resumed` 当
  `tasks` 返回；`resumed` 的唯一区别是 `summary` 前缀 `resuming`（
  `skills/llm-wiki/scripts/wiki.py:2016-2020`）。`data` 里只有 `tasks`/`count`，没有字段标明
  「这是同一任务、尚未提交」。
- 与 `status` 的建议自相矛盾：`status --json` 的 `data.next` 在有租约、无排队时给
  「wait for leased tasks to expire」（`skills/llm-wiki/scripts/wiki.py:1986`），但 `next`
  实际会立刻重发并续租，永远走不到过期。

这不是实现疏漏，而是 `docs/agent/development-decisions.md:218-230` 的已记决策：为修已关闭的
issue「`next` 在只有租约任务时返回空」（agent 按技能说明停摆到租约过期；历史见 Git），选择
「按同样顺序重新发放仍在租约里的任务」。用户现在的要求既不是回到「返回空」，也不是「重发
当作新任务」，而是要**报出未完成任务**这第三种输出，因此不能简单回退旧实现，需要重新定义
`next` 在「已有未提交任务」时的契约。

## 影响

- **重复 `next` 不产生进展却刷新租约**：每次续 15 分钟（`TASK_LEASE_SECONDS`，
  `skills/llm-wiki/scripts/wiki.py:54`）。agent 只要继续调用 `next`，任务永不过期，
  `status` 的「等租约过期」永远不成立。
- **每次重发都 `epoch + 1`**：用首次 `next` 拿到的 epoch 提交会得到 `E_TASK_STALE`
  （`command_submit` 的 epoch 校验，`skills/llm-wiki/scripts/wiki.py:2086-2091`）。日常提交
  不带 epoch 时可以绕过，但同一任务被记成两次 attempt，`attempts` 失去「重试次数」的含义。
- **「手头任务」和「新工作」在输出上不可区分**：agent 无法从 `tasks`/`count` 判断该继续填
  已有的 `payload.json`，还是把它当新流程重开；技能与文档只说「同一个任务会再发回来」，
  没有「必须先提交或释放」的阻塞语义。
- **`--kind` 与重发口径不一致**：`_resume_leased` 按 kind 过滤（
  `skills/llm-wiki/scripts/wiki.py:1494-1496`），`next --kind page` 在只有 route 租约时仍返回
  `no queued tasks`，与「当前有任务未提交」的事实冲突。
- **`--batch > 1` 时两种任务混在一批**：`claimed` 与 `resumed` 同列返回，进一步弱化
  「先结束手头任务」的约束。批次与任务树的交叉问题另记在
  `docs/issues/2026-11-17-P2-next-batch-mixes-task-trees.md`，本 issue 只处理「未提交任务
  应被报出而不是重新发放」这一条。

## 候选方案（未定案）

1. **硬错误**：`next` 发现有 `leased` 任务时直接失败，例如 `E_TASK_UNSUBMITTED`，
   `message` 为「任务 t_xxx 尚未提交」，`next` 字段给 `submit t_xxx`（`payload.json` 已在
   `.wiki/tasks/t_xxx/`）或 `release t_xxx`。最贴近用户表述，但会话重启后没有上下文的新
   agent 必须先从错误里取到 task id 才能继续；`--batch`/`--kind` 的语义要重新定义（是否只要
   有任一租约就拒绝整批）。
2. **保留返回但显式标记，且不续租**：`data` 增加 `resumed: true` 与
   `unfinished: [{id, ...}]`，`summary` 改为「任务 t_xxx 尚未提交；先 submit，或 release 后
   再 next」；同时去掉这次重发里的 `epoch + 1` 与续租。恢复能力保留，但不再伪装成新任务。
3. **只报不领**：有租约时返回 `{tasks: [], pending: [ids]}` 且 `ok: true`。最弱，agent 容易
   再次忽略，也与「`no queued tasks` 才该停」的现有约定难区分。
4. **维持现状只改文案**：`summary` 已写 `resuming ... unfinished`；若用户认为不够，则本项
   由用户否决。

需要用户先定：接受 `next` 在未提交时失败（方案 1），还是保留返回任务但标记未完成且不续租
（方案 2）。方案 1 会让 2026-11-17 决策里的「同一工作区随时可恢复任何未提交任务」失效，
需要同步改写决策记录。

## 待确认与验收条件

待确认：

- [ ] 选方案 1 还是方案 2。
- [ ] 有未提交任务时 `--batch` 与 `--kind` 的行为：整批拒绝，还是只限制同类任务。
- [ ] 提示/错误里是否给任务目录路径与 `submit` 命令；会话重启后的自助恢复是否仍成立。
- [ ] 是否把 `release` 明确为「放弃当前任务」的出口（现状只写在运维口子，
      `docs/readme/cli.md:176`）。

验收（按选定方案调整）：

- [ ] 提取任务后不提交，再 `next` 不再返回「可当新任务处理」的同一任务：报出
      「任务 t_xxx 尚未提交」并给出 `submit`/`release`，或返回同一任务但标记未完成且租约不变。
- [ ] 重复 `next` 不再延长 `lease_until`、不再 `epoch + 1`；租约到期后按现有规则重新领取或
      置 `expired`。
- [ ] `status --json` 的 `data.next` 与新行为一致，不再出现「等租约过期」与「立刻重发」并存。
- [ ] 同步文档：`docs/readme/cli.md:115-118`、`docs/readme/recovery.md:38` 与 `:66`、
      `docs/agent/engine.md:57-58` 与 `:108`、`docs/agent/verification.md:113-119`、
      `docs/agent/development-decisions.md:218-230`。
- [ ] `tests/selftest.py:1300-1304` 的
      「next resumes a leased task instead of reporting no work」改为断言新契约。
- [ ] 改动后同一轮检索核实新内容，并跑通 `tests/selftest.py`。
