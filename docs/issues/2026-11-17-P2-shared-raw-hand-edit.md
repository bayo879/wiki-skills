# 共享 raw 被人工排版后，其他来源如何保留原正文

## 问题

`2026-11-17-P3-read-command-in-view-area.md` 把 raw 升为一等对象，其中一条规则是
「人工排版：raw 允许人工修改」，并声明「若该 raw 还被其他源指向，其他源不受影响」。
当前未提交的实现与 `tests/selftest.py` 新增用例对这一条的理解不一致，自检在第一个
raw 用例就中断。

用例（原 `tests/selftest.py` 的 `llm-wiki-raw-` 段，现已被临时删除，见文末「现状」）：

- `a.md` 与 `copy/a.md` 正文相同，首次 `ingest` 后共用一行 raw、一个 raw 文件；
- 直接修改该 raw 文件，追加「补充：人工排版。」；
- 再次 `ingest` 后期望：
  - 原 raw 文件仍在原路径，内容仍是用户追加后的字节；
  - `raws` 变成两行，`a.md` 的 `raw_id` 指向新行，`last_parsed_digest` 仍是旧行；
  - `copy/a.md` 的 `raw_id` 仍是旧行。

实际结果：读原 raw 文件时抛 `FileNotFoundError`，原 raw 文件已被删除。链路是：

1. `a.md` 的 raw 被判定为人工修改，走 `adopt_raw_edit`（`skills/llm-wiki/scripts/wiki.py:798`）；
2. 该 raw 有 `a.md`、`copy/a.md` 两个挂载，不满足「只被当前源挂载」，转
   `attach_raw`（`skills/llm-wiki/scripts/wiki.py:823-825`）；
3. `attach_raw` 因正文摘要变化新建 raw 行和新文件（如 `a-2.md`，
   `skills/llm-wiki/scripts/wiki.py:729-796`）；
4. `copy/a.md` 随后读到同一份已修改文件，也命中新行并改指过去；
5. `collect_raws`（`skills/llm-wiki/scripts/wiki.py:847`）发现旧行已无来源引用，连同旧
   文件一起删除。

（2026-11-18 复核：这段链路已从 `wiki.py` 搬进 `skills/llm-wiki/scripts/ledger.py`，
`adopt_raw_edit` 在 `ledger.py:550`，`attach_raw` 在 `ledger.py:481`，`collect_raws` 在
`ledger.py:623`；下文旧行号仅保留排查时的原貌。）

根因是账本只存 raw 的文件路径，不存正文（`skills/llm-wiki/scripts/ledger.py` 的
`raws` 表）。用户覆盖共享文件后，旧正文在系统里已无副本，因此「其他源保持原
`raw_id` 且内容不变」在事后无法同时成立。测试的期望与「旧行无引用即回收」也
互相冲突：若 `copy/a.md` 仍指向旧行，旧行就不该被回收；但旧行的文件已经被覆盖。

## 影响

- `tests/selftest.py` 基线是红的，且 `check()` 首个失败即抛错，后续用例（旧版快照、
  删除 `read` 等）根本不会执行，无法验证这次 raw 重构。
- 共享 raw 的人工修改是数据安全问题：当前实现会让另一个来源静默读到新正文，
  或（若简单改成不迁移）让两个来源指向同一文件但账本各说各话。
- P3 issue 的验收项「共享 raw」「人工排版保持」没有可执行的判定标准，实施顺序第 7 步
  无法收口。

## 候选方案（2026-11-18 选定方案 1）

1. **共享即全体迁移（选定）**：把共享 raw 的人工修改视为对整份内容的修改，所有挂载来源一起
   改指新行；raw 文件留在原名原路径，账本行换摘要，旧行回收。最简单、与现有回收逻辑一致，
   也保住用户手改的字节。
2. **从来源重建旧正文**（未采用）：要区分「谁改了文件」，还要为源文件也变了或缺失定规则，
   顺序复杂。
3. **账本保存正文**（未采用）：schema 迁移与库体积代价大，本版不需要审计或回滚到旧正文。
4. **禁止共享 raw 人工编辑**（未采用）：把 CLI 能自行处理的场景推回给人，和现有原则相悖。

## 待确认与验收条件

- [x] 确认语义：共享 raw 的人工编辑是「全局变更」还是「按来源分离」——归属单位是一个 raw
  行（一份按正文摘要去重的正文），整份变更。
- [x] 选定方案，并回写 P3 issue 的对象模型、行为规则与实施顺序——P3 issue 已随
  `bc30a8d` 删除，语义直接落在本文件与 `docs/agent/development-decisions.md`（2026-11-18）。
- [x] 修复后原路径保留用户字节，不再抛 `FileNotFoundError`。
- [x] `copy/a.md` 的 `raw_id` 与文件内容有明确断言，不再依赖未定义行为。
- [x] 恢复被临时删除的用例（`tests/selftest.py` 的 `llm-wiki-raw-` 段），`selftest.py` 全绿。
- [x] 契约有变化时同步 `docs/agent/engine.md`、`docs/readme/cli.md`。

## 定案与验证（2026-11-18）

`adopt_raw_edit`（`skills/llm-wiki/scripts/ledger.py:550`）不再在共享时转 `attach_raw`：

- 读原 raw 正文算新摘要；挂载 = 旧 `raw_id` 的全部来源（空 `raw_id` 只按当前 key）。
- 同摘要的 `raws` 行已存在时，所有挂载指向那一行的路径；旧行没有挂载后由 `collect_raws`
  回收。
- 否则删旧行、按原 `name`/`path` 插入新行，文件不重写，用户字节原样保留；
  `last_parsed_digest` 不动，`parser` 沿用。
- 一条 `UPDATE sources` 换掉所有挂载的 `raw`、`raw_id`、`raw_digest`、`raw_size`、
  `raw_mtime`，再对每个挂载 `pin_open_task_raw`，排队或租用任务的快照不会被自己的任务判成
  漂移。

验证：

- 一次手改后 `raws` 只剩一行、`path` 仍是原路径；`a.md` 与 `copy/a.md` 的 `raw_id` 都是新
  摘要，`last_parsed_digest` 保持旧值，文件内容仍是手改后的字节，`data.reclaimed` 为空。
- 紧接的 `ingest` 无新增摄入；连续第二次手改后文件名仍是 `a.md`，不再在 `a.md`/`a-2.md`
  之间来回跳。
- `python3 tests/selftest.py`：262/262 通过。
- 文档同步：`docs/agent/engine.md`、`docs/readme/cli.md`、`docs/readme/recovery.md`、
  `docs/readme/design-principles.md`、`docs/agent/development-decisions.md`。

## 现状（2026-11-17 补记）

- 用户 2026-11-17 要求先删掉与「任务目录化 / 单页单任务 / 文件锁」这轮改动无关的失败
  用例，`tests/selftest.py` 的 `llm-wiki-raw-` 段只保留「两份相同正文共用一个 raw」
  「共享 raw 不会被解析两次」两条，hand-edited raw 的三条断言（含
  `a changed source re-parses and drops the dead row`）已删除；当时自检 201 项全绿。
- 语义仍未裁决，实现未改：本文件描述的行为缺陷现在没有任何用例覆盖，删除用例只是
  让红基线不再挡住别的改动，不等于问题已解决。
- 2026-11-18 已按方案 1 修复并补齐共享用例，本节保留为排查时的历史记录。
