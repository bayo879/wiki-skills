# 已有页面无法补关联，旧库图边只能为空

## 问题

页面关系边只在写页时产生，事后没有入口补链，导致已经建好的页面无法维护关联，观看区的
`## 关联` 永远为空、Obsidian 图谱全孤立。

- `links` 只在 route 决策（`skills/llm-wiki/scripts/tasks.py:139-141`）和 page 提交
  （`skills/llm-wiki/scripts/validation.py:21-30` 的 `check_links`）时写入；没有
  `page link` 一类命令。
- `page import` 只从源文件读 `title/type/summary/sources`，frontmatter 里的 `links` 被
  丢弃（`skills/llm-wiki/scripts/cli.py:1105-1108`）；`page restore` 只回放旧修订的
  meta（`cli.py:1145`），也无法新增边。
- 正文里的 `[[page-id]]` 不解析，`## 关联` 会被 `split_relations` 剥掉
  （`skills/llm-wiki/scripts/pages.py:31-51`），所以手写正文同样不产生图边。
- `write_rev` 在存储层做并集、只增不删（`pages.py:295-301`），但除提交外的调用方都没有
  传入新链接，能力没有暴露到命令面。
- 文案把关联当可选项：route brief 写“可选的 `links`”（`tasks.py:139`），page brief 只写
  “由 route 预填”（`tasks.py:196`），没有要求写页时再判断一次。
- 实现补链时发现：观看区把 `## 关联` 渲染在 `## 证据` 之后，`sync` 原先只找末尾证据段，
  带关联的页面会把证据块当正文收进来，越推越多（`skills/llm-wiki/scripts/view.py` 的
  `sync_view_file`）。已改为先剥关系段、再去重证据段。
- 实测 `/Users/zhanbaiyang/Documents/work/hcxt/项目/llm-wiki/wiki1`（2026-11-18）：检查时
  22 页的 `meta.links` 全为 `[]`，`links` 表 0 行，观看区没有 `## 关联`；`aliases` 已按
  id 写出，但没有 `[[...]]` 可解析，所以 Obsidian 里 aliases 也没有效果。当天复查该库已
  增至 35 页、`links` 表 3 行，多数页面仍无出链。

## 影响

- 观看区只有单页正文，没有出链/入链导航；`query --page` 的 `outgoing/incoming` 恒为空。
- 已经摄入并写好的页面无法补关系，只能重写整页或重建库；知识越积越多，图谱越难恢复。
- `aliases` 的解析价值落空：它只在已有 `[[page-id]]` 时把 id 指到中文文件名，本身不建边。

## 相关文件

- `skills/llm-wiki/scripts/cli.py`（page 子命令、import/restore、`COMMANDS`/`LOCKED_COMMANDS`）
- `skills/llm-wiki/scripts/pages.py`（`write_rev`、`render_page`、`split_relations`）
- `skills/llm-wiki/scripts/tasks.py`（route/page brief）
- `skills/llm-wiki/scripts/validation.py`（`check_links`）
- `skills/llm-wiki/scripts/view.py`（`sync_view`）
- `skills/llm-wiki/SKILL.md`、`skills/llm-wiki/references/curation.md`
- `skills/llm-wiki/references/recovery.md`
- `docs/readme/cli.md`、`docs/agent/engine.md`、`docs/agent/development-decisions.md`、`README.md`
- `tests/selftest.py`

## 候选方案

已定案（2026-11-18 用户选择方案 A：补引擎能力）：

1. 新增 `page link <id> <target...>`：只增不删，目标必须是已存在页；空目标或自己报
   `E_USAGE`，未知目标报 `E_LINK_UNRESOLVED`，已链过不产生新修订；写新修订
   （`origin=link`）后立即推送观看区。
2. `page import` 接受 frontmatter 块列表 `links`，与旧链接取并集后走同一套目标校验。
3. route/page brief、`SKILL.md`、`curation.md` 把关联从“可选”改为“必须判断”：一页
   2–5 个出链，确实没有相关页才留空；已有页补链用 `page link`。
4. 保留“没有删链入口”的存储不变量；不做 pending edge、不做红链。
5. 旧库补链单独做：先给出建议链接表，经用户确认后按页执行 `page link`，不凭关键词批量
   乱连。

## 待确认与验收条件

- [x] `page link` 能新增边、写新修订，并在观看区 `## 关联` 出现 `[目标页标题](相对路径)`
- [x] 未知目标、空目标、指向自己分别报 `E_LINK_UNRESOLVED` / `E_USAGE`，且不写修订
- [x] 重复链接同一目标不产生新修订
- [x] `page import` 的 frontmatter `links` 能并入旧链接，未知目标被拒绝
- [x] `page link --repair` 能按共享 raw 来源批量补缺边，`--dry-run` 只预览、不写修订
- [x] `tests/selftest.py` 覆盖上述场景，`python3 -m py_compile skills/llm-wiki/scripts/*.py`
      与 `python3 tests/selftest.py` 通过
- [x] `docs/readme/cli.md`、`SKILL.md`、`curation.md`、`development-decisions.md`、
      `engine.md`、`recovery.md` 同步
- [ ] 当前 vault 的补链表经确认后执行，多数页面有出链
