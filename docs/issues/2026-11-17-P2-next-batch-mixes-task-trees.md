# `next --batch` 跨任务树发放任务

## 问题

`next --batch N` 领取任务时只做一次扁平 `LIMIT`：`_claim_queued` 按「route 优先、
`created_at`、`id`」排序后取 N 条（`skills/llm-wiki/scripts/wiki.py:1462-1483`，
排序在 `wiki.py:1468`），再用仍是 `leased` 的任务补满同一批
（`wiki.py:1443-1460`、`wiki.py:1484-1513`）。领取侧不认任务树；树唯一性只落在入队侧
（`enqueue_task` 的 `route_task` 例外，`wiki.py:1301-1330`）。因此一批任务可以：

1. 把同一棵 page 树（同一次 route 拆出的多个 page 任务，共享 `inputs.route_task`）
   拆到不同批次，并与别的树混在同一批；
2. 同一批里同时出现 route 任务和 page 任务，route 提交时撞上本批自己已租的页面任务；
3. 用上一次未提交的 `leased` 任务补位，旧树与本次新树继续混在一起。

任务包本身也没有树标识：`task_work` 只给 `sources/page_index/pending_pages`
（`wiki.py:1593-1616`），`task_packet` 只组装 `{id, brief, template, work}`
（`skills/llm-wiki/scripts/tasks.py:171-186`），`route_task` 从不进入任务包。

### 复现一：拆页的树被拆散并与其他树交错

临时工作区，a.md 经一次 route 拆成 pa1/pa2/pa3（三个 page 任务共享 `route_task`），
b.md → pb，c.md → pc，五个 page 任务都 `queued`：

- `next --batch 2` 返回 `pa3`（a 的树）+ `pb`（b 的树）；
- 紧接着再跑 `next --batch 2` 返回 `pa1`（a 的树）+ `pc`（c 的树）。

a 的树被拆成 2+1，两批里都混入其他树，且输出都没有说明谁与谁同树。

### 复现二：同一批里的 route 与 page 互相阻塞

a.md 拆成 pa1/pa2，之后新增 b.md 入队；`next --batch 2` 返回 route b + a 树的某一页
任务（示例里是 pa2，已租）。按「route 优先」的自然顺序先提交 route b，决定 `merge` 到 pa2：

```
[E_STATE] b.md: page pa2 is being written by another task
next: retry this route after that page task is submitted
```

持有 pa2 的正是本批自己（`wiki.py:2209-2214` 只比较任务状态，不比较批次或领取者）。
任务被退回队列，但 pa2 的租约还在；按 `error.next` 重试，仍会在 route b 与 pa2 之间撞车。

同一份 route 载荷还会随提交顺序得到不同结果：目标页仍为 `queued` 时
`attach_page_source` 直接把来源并进那棵树（`wiki.py:2256-2258`）；目标页已接受时改排
一条新的 update 任务；目标页在本批已租时如上直接报错。

## 影响

- agent 无法从任务包判断同树关系，只能把扁平的 `tasks[]` 当互相独立的任务处理。同树页面
  共享同一来源、页面边界互相依赖（拆页场景），拆批或混批后容易各写各的，或按错误顺序提交。
- 同批 route + page 的自锁把「本批自己」报成「另一个任务」；agent 按提示重试会在 route
  与 page 之间反复，或者误 `release` 掉本可推进的任务。
- 同一份 route 载荷的提交结果取决于提交顺序，契约不稳定，批量运维难以复现和校验。
- 一次批次租下多棵树的任务，未提交的部分把租约占满 `TASK_LEASE_SECONDS = 900`
  （`wiki.py:54`）；期间这些树对应的来源不会重新入队（`wiki.py:1869`、`wiki.py:1913`），
  只能等租约过期或手工 `release`。
- 与既定决策「任务一次一个；`--batch` 只留给调试/批量运维」
  （`docs/agent/development-decisions.md:99`）不一致：`docs/readme/cli.md:97` 允许
  `--batch`，测试也在用，但契约没有约束批次的树边界。`tests/selftest.py:1158` 只用
  `--batch 5` 验证单棵树拆页，`tests/selftest.py:1372` 的队列只有一条任务，
  `tests/selftest.py:373`、`tests/selftest.py:785` 的批次只用来一次取够 route 任务；
  没有用例覆盖「多棵树排队且 `--batch` 小于树规模」。

## 候选方案

未定案。

1. **按树领取。** 先按树键分组 `queued`（route 任务以自身为树；page 任务取
   `inputs.route_task`，缺失时回退自身 id），`--batch N` 按棵树计数并整棵发放，剩余槽位
   留到下一次。取舍：树大于 N 时要么整棵超发、要么留到下一轮；拆页会一次返回多任务，
   与 agent 的「一次一个任务」流程不一致，需要把 N 的语义改成「棵树」。
2. **只隔离类型与树。** 一批只发 route 或只发 page；page 只取同一棵树；route 提交不会
   再撞本批的 page 租约。取舍：仍可能把一棵树拆到不同批，`--batch N` 变成「至多 N 个、
   且同树同类型」，文档和测试都要改口。
3. **保持扁平批次，但彻底降级为运维工具。** 任务包加树标识与显式警告，文档写明不保证
   树边界；同时至少在发放时拒绝 route 与 page 混批，消掉自锁。取舍：agent 仍可能误用，
   只是把风险显式化。
4. **关联的小修（不替代上面的选择）。** 任务包暴露树标识（`work.tree` 或 brief 一行），
   并让 `attach_page_source` 追加来源后刷新已物化的 `work.json`/`brief.md`，避免 agent
   继续按旧任务范围写载荷。

## 待确认与验收条件

- [ ] 选定批次的树边界契约：整树领取 / 同类型与同树隔离 / 明确不保证并给出警告。
- [ ] `--batch N` 的计数单位与超发规则写进 `docs/readme/cli.md`（`next` 一节，`:97`）
      和 `docs/agent/engine.md` 的领取与树不变量段落。
- [ ] 一批任务不会互相阻塞提交：route 不得 merge 到本批已租的页面任务，或在发放时就
      避免这种组合。
- [ ] 任务包暴露树标识，agent 能判断哪些任务同树；`next` 的 `summary`/`lines` 不再把
      同树任务与别的树随机交错。
- [ ] 部分提交后的恢复不把旧树与新树混成一批；若仍混批，包内标明树归属与恢复状态。
- [ ] selftest 覆盖：跨树队列且 `--batch` 小于树规模；同批 route + page 合并到已租页面；
      部分提交后再 `next`。
- [ ] 现有依赖 `--batch` 的测试（`tests/selftest.py:1158`、`tests/selftest.py:1372`）按
      新契约调整，或保留并注明其队列前提。
