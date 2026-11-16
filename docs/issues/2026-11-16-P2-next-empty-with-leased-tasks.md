# `next` 在有 leased 任务时返回空

## 问题

`next` 只领取 `queued` 任务；如果任务已经被领取、但尚未提交（`leased`），它返回
`{"ok": true, "summary": "no queued tasks", "data": {"tasks": [], "count": 0}}`。
`SKILL.md` 写的是「`next` 返回没有任务时就停止」，agent 会把「暂时没有可领取任务」
误判成「工作完成」，在租约到期前停摆 15 分钟。

复现：同一工作区先 `next` 领取两个任务、不提交，再跑 `next`；同时 `status` 显示
`leased: 2`、`next: "wait for leased tasks to expire"`。

## 影响

- agent 中断、上下文压缩或误重跑 `next` 后，会停止推进，且没有任何带内提示说明还有未完成任务。
- `next --batch 2` 只提交其中一个时，也会掉进同一状态。
- 与「一次一个任务、按 brief 推进」的流程冲突：agent 无法从技能可见信息判断该等待还是该恢复。

## 候选方案

未定案。可选：

1. 无 `queued` 但有 `leased` 时，`next` 返回该 leased 任务（恢复同一任务），并在 `summary`
   里说明是恢复；适合单 agent 工作区。
2. 保持不返回任务，但把 `summary` 改成「no queued tasks; N task(s) leased」，并让
   `SKILL.md` 明确「有 leased 时不要停，等待后重试或按提示恢复」。
3. 返回 `E_TASKS_BUSY` 错误，让 agent 走错误分支，而不是把空结果当完成。

需要同时决定：是否允许恢复其他 agent 的租约，以及 `release` 是否进入技能流程。

## 待确认与验收条件

- [ ] 明确 `next` 在 `queued=0 && leased>0` 时的契约（返回任务 / 提示 / 错误码）。
- [ ] `SKILL.md` 的「没有任务就停止」与契约一致，不再把 leased 说成没有任务。
- [ ] selftest 覆盖「领取后不提交，再 `next`」的场景。
