# query 检索成本高于同规模 grep

优先级：P2

## 问题

改前，`query` 每次调用都把账本里所有页面的完整正文读进 Python 再计分，`query --task` 还按词重复整轮扫描；
同规模下 `grep -rn` 只做一次 C 扫描。页面越多、任务词越多，绝对耗时差距越大。

2026-11-23 在合成账本上复现（2000 个页面、正文约 9 MB、100 个话题、5 个工作流；
Python 3.15 / SQLite 3.54.1 / macOS）：

| 场景 | query | grep -rn | 备注 |
| --- | --- | --- | --- |
| 100 页 | 93 ms | 18 ms | 固定启动约 56–60 ms |
| 2000 页 | 230 ms | 147 ms | |
| 2000 页，`--task` 5 个词 | 约 780 ms | 约 150 ms | 5 次 `search_pages` 微基准；grep 一次扫 5 个模式 |

合成账本只用于复现比例与增长趋势，绝对值随机器、正文大小和命中数变化。

用户 2026-11-23 反馈实际使用中常到 5–6 秒；本机两个 50 页库实测 `query 会议` 分别为
216 ms 与 89 ms。按 2000 页、5 词 780 ms 外推，5–6 秒大致对应万页级；单页正文过大或
每页来源过多也会放大。具体库的规模待用户提供后确认。

证据（改前源码）：

- `skills/llm-wiki/scripts/cli/query.py:65-73`：`query --task` 对每个词各调一次
  `search.search_pages(wiki, [word], limit)`，每次都重新 `all_pages()`；最多 5 个词就是 5 次全量扫描。
- `skills/llm-wiki/scripts/pages.py:251-259`：`all_pages()` 取全部最新版页面；
  `_page_row_to_record`（`pages.py:232-249`）逐页调 `links_for`（2 条 SQL，`pages.py:459-470`）
  与 `computed_status`（每个来源 1 条 SQL，`pages.py:310-319`）。2000 页实测约 6010 次
  `execute`，`all_pages()` 86 ms，其中纯取行 23 ms。
- `skills/llm-wiki/scripts/search.py:14-42`：对每页拼 `title`/`summary`/`body`，对每个词做
  `lower()` 与 `str.count`；`snippet()`（`search.py:49-58`）对每个命中页做整段
  `re.sub(r"\s+", " ", body)`，不只为返回的 5–8 条。2000 页单次 `search_pages` 170 ms，
  其中 `all_pages` 86 ms、snippet 43 ms（667 个命中）。
- `skills/llm-wiki/scripts/cli/query.py:270-278`：每次查询都完整扫描并解析全部话题与工作流；
  `resolve_topic_refs` 与 `workflows.match` 各扫一次工作流。100 话题 / 5 工作流时约 14 ms，
  随文件数线性增长。
- 普通 `query` 每次进程启动导入约 56–60 ms（`python3 wiki.py version` 实测），grep 约 5 ms。

## 影响

- `query --task` 是 route 任务提交前的必做步骤（`skills/llm-wiki/SKILL.md:142`），每个页面任务
  要付 5 次全量扫描；页面到万级时任务查询进入秒级，agent 会倾向跳过检索直接用 grep。
- snippet 对每个命中页做整段正则，公共词命中多时单次查询的工作量随正文总量放大，
  不是随返回条数增长。
- 固定启动让空库、小库上的 query 也永远慢于 grep，形成“query 不如 grep”的直觉。

## 相关文件

- `skills/llm-wiki/scripts/search.py`（改前 14-58 行）
- `skills/llm-wiki/scripts/pages.py:232-259`、`pages.py:310-319`、`pages.py:459-470`
- `skills/llm-wiki/scripts/cli/query.py:65-74`、`query.py:264-291`（改前）
- `skills/llm-wiki/scripts/topics.py:112-175`
- `skills/llm-wiki/scripts/workflows.py:103-134`、`workflows.py:210-249`
- `skills/llm-wiki/SKILL.md:142`
- `docs/readme/cli/content.md`（query 小节）

## 候选方案

未定案。按“先优化查询路径、按需再加索引”的顺序：

1. **不改 schema 的查询路径优化。** `query --task` 的多词合并为一次页面遍历后按词分组；
   `search_pages` 先用 SQL `instr(body, ?)` / `instr(meta, ?)` 收窄候选，只把命中行读进 Python；
   `snippet` 只对排序后的前 N 条计算；搜索路径不为返回集之外的页面算 `links`/`status`。
   按 2000 页原型（/tmp 内验证，未改仓库）：单次 145.5→42.7 ms，5 词 343.2→121.3 ms，
   5 词分开 752.1→78.4 ms（一次遍历分组）。端到端预计单次 230→约 125 ms，5 词任务
   780→200 ms 量级；输出字段与排序语义不变。注意 SQLite `lower()` 只处理 ASCII，
   非 ASCII 大小写需回退全量扫描或另存规范化文本。
2. **派生索引。** FTS5 可用（SQLite 3.54.1），但 `trigram` 对 2 字中文词命中为 0（实测），
   `unicode61` 不分中文词；要支持中文需写入/查询时做 bigram 预处理，或自建 `page_terms`
   倒排表。2000 页建 trigram 索引 349 ms、3 字词查询 1.9 ms。索引必须是可重建的派生缓存，
   在 submit/import/restore/delete 后维护，并提供 `doctor --fix` 重建；与“账本为唯一事实源”
   不冲突，但要新增写入路径与失败处理。
3. **接口层。** 增加 `--ids`/`--brief` 只返回候选 id、标题、分数，供多词探索；详情仍用
   `query --page` 或对候选文件 grep。字面量/穷尽式查找继续用 grep：query 是排序候选，
   不承诺覆盖；它只查账本页面，raw 里尚未成页的内容 grep 能看到。

## 处理结果（2026-11-23）

按方案 1 的子集落地，未加 SQL 预筛（避开 `lower()` 只处理 ASCII 的语义偏差），未做索引：

- `search.py` 直读最新版行（`page`/`rev`/`meta`/`body`），不再经 `all_pages()` 逐页解析
  links/status；`status`/`links` 只为返回的前 N 条解析；`snippet` 只为返回项计算。
- 新增 `search_pages_grouped`：一次遍历账本，每个词保持独立排序；`query --task` 改用它。
- 计分、排序（`-score, id`）、`snippet` 与 `matches` 语义不变；同库 JSON 输出与改前逐字节一致。

实测（同一合成账本，2000 页 / 约 9 MB）：

| 场景 | 改前 | 改后 |
| --- | --- | --- |
| `--task` 5 词核心检索 | 734.6 ms | 84.5 ms |
| 普通 query 5 词端到端 | 440 ms | 165 ms |
| 普通 query 1 词端到端 | 240 ms | 140 ms |

`tests/selftest.py` 368/368 通过；50 页真实库 `query 会议` 90→70 ms。用户报告 5–6 秒的库
尚未复测，若仍慢则继续按候选方案 2/3 处理。

## 待确认与验收条件

- [x] 确认优先解决耗时还是命中质量（排序、覆盖），以及目标规模（页数、单页大小）。
- [x] 方案 1 落地后，2000 页、5 词的 `query --task` 降到 200 ms 量级，单次 query 降到 120 ms 量级；
      输出字段、排序与 `matches` 语义不变。
- [x] 大正文、公共词多命中时，耗时不再随命中页数线性放大（snippet 只算前 N 条）。
- [ ] 若做索引：中文 2 字词可命中；索引缺失或损坏时可回退全量扫描，不阻断查询；
      重建路径有测试覆盖（本次未做索引）。
- [x] 同步 `docs/agent/engine.md` 的检索模块说明（CLI/SKILL 的输出契约未变，无需改）。
- [ ] 在用户报 5–6 秒的真实库上复测，确认耗时降到可接受范围。
