# 页面关联不可见、不可维护，关联图随写入退化

状态（2026-11-17）：A、B+F 已实现；E 的无可用来源孤儿页检查与自动清理已实现；
C、D、G 及 E 的缺页/同题检查仍未定案。

## 问题

页面之间的关联目前只能在「写页那一刻」通过载荷 `links` 建立一次，之后既看不见全图，
也没有独立的维护入口；更新页面时还会把 `## 关联` 段重复追加。结果是关联图沿时间单向
生长，旧页无法指向新页，孤儿页和待建页没有信号。

### 1. 建页时看不到已有图

任务包的 `page_index` 只有 `id`、`title`、`type` 三个字段
（`skills/llm-wiki/scripts/task_store.py:378-383`），没有摘要、没有出入链，也没有邻页。
route brief 要求「在 page_index 里找同主题的已有页面」，但仅凭标题无法判断语义是否重合，
同名/近义标题只能靠猜；page 任务同样拿不到该页的现有邻居。

### 2. `links` 只能指向已经存在的页

提交校验要求每个 `link` 都命中现有页面，否则报 `E_LINK_UNRESOLVED`
（`skills/llm-wiki/scripts/validation.py:44-52`）。同一来源拆出的多个新页共享一棵
route 任务，但页面按任务逐个创建，先提交的页无法引用尚未创建的兄弟页；`pending_pages`
只影响 route 的 merge 目标，不参与链接校验。

实测（临时库，来源 a.md 拆 p-a/p-b，先提交 p-a 并声明 `links: ["p-b"]`）：

```
{"code": "E_LINK_UNRESOLVED", "message": "link 'p-b' points at no page",
 "next": "create that page first, or drop the link"}
```

`skills/llm-wiki/references/curation.md:58-64` 与 `docs/readme/cli.md:166` 也明确写着
「不要链接到不存在的页 / `links` 只能指向已存在页面」，并把「载荷 links」与「正文
[[...]]」定为二选一。

### 3. 更新会重复生成 `## 关联`

提交时只要 `links` 非空，引擎就无条件把链接渲染成 `## 关联` 追加到 body 末尾，不检查
body 里是否已有该段（`skills/llm-wiki/scripts/validation.py:124-127`）。而更新任务的模板
沿用旧 body（其中已含上一次生成的 `## 关联`），同时把 `links` 重置为空数组
（`skills/llm-wiki/scripts/tasks.py:126-135`）。因此：

- 保留模板 body、补 `links` → 追加第二个 `## 关联`；
- 重写 body、漏传旧 `links` → 旧关联段被覆盖，链接丢失；
- 单次提交里 body 写了 `[[p-b]]` 又传 `links: ["p-b"]` → 同一次就出现两遍。

实测连续两次 merge 更新 p-a（均保留模板 body 并传 `links: ["p-b"]`）后，第 3 版 body：

```
## 关联
- [[p-b]]

## 关联
- [[p-b]]
```

观看区 `wiki/pages/concept/甲主题.md` 同样有两段；`query --page p-a` 的 `outgoing` 仍是
`["p-b"]`（links 表按对去重），正文与图不一致。

### 4. 改链没有独立入口

页面内容只能由新来源触发 merge 才能修订；`page import` / `page restore`
（`skills/llm-wiki/scripts/cli.py:947-1005`）是人工恢复入口，`sync` 只做观看区回收
（`skills/llm-wiki/scripts/cli.py:1006` 起）。没有「无新来源也能改页 / 补链」的任务类型，
agent 想给旧页补一条指向新页的链接，只能等下一次有来源并入，或请人手工 import。

### 5. 类型不是知识域，观看区没有目录

`schema.types` 的 `concept/entity/process/note` 只决定正文模板，不是知识域划分；观看区
README 只输出各目录篇数（`skills/llm-wiki/scripts/view.py:17-56`），没有带摘要的目录页，
也没有枢纽页/总览页，读者无法从入口看出页面之间如何组织。

### 6. 巡检不覆盖关联

`doctor` 只查数据库完整性、stale 页、未注册来源、broken links、过期租约
（`skills/llm-wiki/scripts/cli.py:1110-1145`）。实测：新建一个无入链、无出链的孤儿页 p-c
后，`doctor --json` 返回：

```
{"issues": [], "fixed": [], "healthy": true}
```

另外，正文里的悬空 `[[...]]` 只会被当作 broken link 报出，不会产生「这个页面该建」的信号。

### 7. 反向链接不可见

出入链只在 `query --page` 的返回值里出现（`skills/llm-wiki/scripts/cli.py:837-859`），
页面正文和观看区都不渲染 incoming；读者从一页出发看不到「谁引用了我」。

## 影响

- **route 决策依据弱**：只能按标题判断是否已有同主题页，近义标题容易重复建页或误并。
- **关联图单向生长**：旧页不能新增出链，新页也不会被旧页自动指向；图逐渐变成以少数更新
  频繁的页为中心的星形，其余页成为孤岛。
- **正文与 links 表可能不一致**：重复的 `## 关联` 会进入观看区、推送与人工阅读，且当前
  没有幂等的写法（保留旧段会重复，重写正文会丢链）。
- **拆页流程被迫串行**：先建的页不能引用后建的页，「先规划整棵页树再落笔」无法表达。
- **质量退化不可发现**：孤儿页、零出链页、同题近似页、待建页都没有巡检信号，只能靠人工
  lint 或阅读。
- **维护动作绕开任务流**：补链依赖人工 `page import`/`restore`，容易跳过引文与边界校验。

## 相关文件

- `skills/llm-wiki/scripts/task_store.py:378-383`：`page_index` 只含 `id/title/type`。
- `skills/llm-wiki/scripts/validation.py:44-52`：`links` 必须命中现有页面（`E_LINK_UNRESOLVED`）。
- `skills/llm-wiki/scripts/validation.py:124-127`：无条件把 `links` 追加为 `## 关联`。
- `skills/llm-wiki/scripts/tasks.py:115-135`：更新模板沿用旧 body、`links` 重置为空。
- `skills/llm-wiki/scripts/cli.py:837-859`：`query --page` 才返回 outgoing/incoming。
- `skills/llm-wiki/scripts/cli.py:947-1005`：`page import` / `page restore` 人工入口。
- `skills/llm-wiki/scripts/cli.py:1110-1145`：`doctor` 的检查项。
- `skills/llm-wiki/scripts/view.py:17-56`：观看区 README 只给目录计数。
- `skills/llm-wiki/templates/concept.md:1-4`（entity/process/note 同类注释）：声明
  `## 关联` 由引擎按载荷生成。
- `skills/llm-wiki/references/curation.md:58-64`：链接规则（不链未建页、二选一）。
- `docs/readme/cli.md:130-170`：route/page 载荷契约；`:166` 规定 `links` 只能指向
  已存在页面。
- `docs/agent/development-decisions.md:59-66`：一份来源可拆多页的已定决策。

## 候选方案（未定案）

**A. 让图可见**：`page_index` 增加 `summary` 与出入链计数（或邻页标题），brief 要求先看
邻域再决策。改动集中在 `task_store.page_index` 与任务 brief；成本小。只解决「看得见」，
不解决「改得动」，任务包会变大。

**B. 关联一等化**：`## 关联` 不再写进 body，由 links 表在渲染时生成；更新模板回传现有
`links` 并允许增补（删除暂不提供入口）；提交时清理旧 body 里的 `## 关联` 段，并做一次旧页
迁移。涉及 `validation.py`、`tasks.py`、`pages.py` 渲染与推送；成本中等。这是 C、F 的前置，
否则「红链」和「反链」都没有稳定的落点。

已确认（2026-11-17）：body 不再保留 `## 关联`；观看区由 links 表渲染出链与入链，混排去重。
写入规则见「已确认的关联规则」。

**C. 红链 / 待建页**：允许 `links` 指向未建页，`doctor` 列出 missing pages，route 可据此
建页；同时改写 `curation.md` 的「不要链接到不存在的页」。成本中等；查询、渲染、推送都要
容忍悬空链接，还要定义待建页的生命周期（谁补、多久算失败）。

**D. 分类与目录**：`schema` 增加受控 domain 或 tags；观看区生成带 summary 与链接数的目录/
枢纽页。成本中等；需先定 domain 是自由标签还是受控词表，并做 schema 迁移与回填。

**E. 巡检补项**：`doctor` 增加无可用来源的孤儿页（`sources` 为空或全部失效，且零入零出，
标 removable）、指向未建页的链接、同题近似（标题/summary 相似）；
指向未建页的链接。成本小；近似检查需定阈值，避免噪声。

**F. 反向链接**：页末渲染 incoming；页面修订影响他人时，需重推所有引用页。成本中高，
依赖 B 的渲染入口。

**G. route 判定流程：先抽主题，再查图，再判 merge**：现状 brief 让 agent「在 page_index 里
找同主题标题」，只能按标题猜。改为：

1. 只读来源，列候选主题卡：一句话、关键实体/动作、需要哪些出处、初步 `boundaries`。
2. 每个主题卡成组检索：主题词 + 同义/缩写/旧称 + 上位词，逐个 `query`。注意 `query` 是
   字面命中、按命中次数排序（`skills/llm-wiki/scripts/search.py:14-42`），泛词会淹没关键词；
   `matches` 大于 `returned` 时说明结果被截断，要换词而不是判定「没有相似页」。
3. 对命中页跑 `query --page`，读正文与 `boundaries`，判「同一对象/过程/口径、边界可容纳、
   类型一致」。
4. 四分类：merge / 仅关联 / new / skip，merge 优先；`new` 的 `reason` 必须写「现有页为何
   都不行」，而不是「这是一个新概念」。
5. 单独核对 `pending_pages`：已排队未写入的页搜不到，只能按 id/title 对。

改动只涉及 `skills/llm-wiki/references/curation.md` 与 `tasks.py:97-114` 的 route brief，
不动引擎，成本小；仍受 A（`page_index` 无摘要）限制，但 `query` 已能补足大部分。未定案：
「仅关联」是否作为独立 decision（当前载荷只有 new/merge/skip），以及是否强制每个主题卡
都跑 `query`。

建议顺序：E → A → B → F（入链渲染）→ C；D、G 可独立推进，G 是纯文案改动，可立即做。
旧页 → 新页的存储边暂不补（目前不做深度探索）；观看区入链已覆盖阅读。

## 已确认的关联规则（2026-11-17，已实现）

以下为讨论后确认的方向，A、B+F 与 E 的孤儿页部分已在 2026-11-17 实现；C/D/G 仍未定。

- `body` 不保留 `## 关联`。观看区由 links 表渲染，出链与入链混排、按 page id 去重，不标
  「出链/入链」。
- 不设人工补链入口。入库只写「新页 → 已存在页」的存储边；新页只能链已存在页（现有
  `E_LINK_UNRESOLVED` 已强制）。旧页不因新页入库而改内容，也不新增出链；旧页通过入链显示新页。
- 人工只允许修改正文；frontmatter、links、summary、boundaries 等引擎字段不接受人工修改，
  写了也不认（由引擎覆盖或忽略）。body 里的 `[[...]]` 只是正文，不产生图边。
- 旧页 → 新页的存储边不在入库时补；目前不做深度探索，这批边暂缺，观看区入链已覆盖阅读。
  将来需要时再批量回填，避免每来一个新页就回头改一批旧页。
- 同一 route 拆出的兄弟页：后提交页链先提交页，先提交页靠入链显示。不做 pending edge；
  links 只能指向提交时已存在的页；顺序错时报 `E_LINK_UNRESOLVED`，按队列顺序重试。
- route 的 decision 增加可选 `links`：`new` 原样进 page 模板；`merge` 只表示增量，page 模板
  = 现有 links ∪ 新增。提交时校验存在、去重、非自身；暂不设数量上限，2–5 只作建议值，
  以后由 doctor 提示。
- `merge` 是把新来源的事实并入一个已存在页（body/boundaries/sources 更新），不是把两个页面
  合并。merge 页的 links = 目标页现有 links（更新模板必须带入，不再重置为空）+ decision
  显式给出的新增（只能已存在页）。**只增不删**；当前版本不提供删链入口。
- 更新任务模板必须回传现有 `links`，不再重置为空数组。
- 巡检只报「无可用来源的孤儿页」：`sources` 为空，或所有来源都已失效（不存在 / missing /
  unsupported / oversize），且零入链、零出链；`doctor` 用 L7 标 removable 并直接删除
  （内部处理，不新增命令、不逐页问 agent；有来源的零出链页不删）。
  正常页即使暂时零出链，只要还有可用来源，就不报。

实现落点（2026-11-17）：`pages.py` 的 `normalize_links`/`split_relations`/`render_page`/
`view_text`/`write_rev`/`rebuild_links`；`validation.py` 的 `check_links` 与 meta `links`；
`tasks.py` 的 route/page 模板；`cli.py` 的 `submit_route` 与 export；`task_store.py` 的
`attach_page_links`；`tests/selftest.py` 的 `links_flow`。

## 待确认与验收条件

待确认：

- [x] `## 关联` 完全改由 links 表渲染，body 不再接受或保留该段（方案 B，2026-11-17 确认）。
- [x] 更新任务回传现有 `links`，且只增不删（2026-11-17 确认）。
- [x] 不设人工补链入口；入库只写「新页 → 已存在页」，旧 → 新存储边暂缺（目前不做深度
      探索，2026-11-17 确认）。
- [ ] 是否允许红链/待建页，以及待建页由谁、在什么时机补建（方案 C）。
- [ ] 类型之外是否引入 domain/tags；受控词表还是自由标签（方案 D）。
- [x] 孤儿口径（2026-11-17 确认）：只报无可用来源、且零入零出的页；有来源的零出链页不报。
- [ ] 同题近似的判定口径与阈值（方案 E 剩余部分）。
- [x] 目前不做深度探索（2026-11-17 确认）：不新增补链任务，旧 → 新存储边暂缺，留待以后
      需要时批量回填。
- [ ] route 是否强制「先列主题卡再逐组 query」，以及「仅关联」是否作为独立 decision
      （方案 G）。
- [x] route 载荷携带 links（2026-11-17 确认）：new/merge decision 增加可选 `links`；new 原样
      进模板，merge 为增量；校验存在、去重、非自身，不设硬上限。
- [x] 不做 pending edge（2026-11-17 确认）：links 只能指向提交时已存在的页；兄弟页按队列
      顺序提交，顺序错时报 `E_LINK_UNRESOLVED` 并重试。
- [x] 旧 body 的 `## 关联` 迁移：直接删除，不从 body 解析链接（人工只能改正文，body 里的
      `[[...]]` 不产生图边）；错链没有删除入口，人工也不认（2026-11-17 确认）。

验收：

- [x] 同一来源拆出的多页，按队列顺序提交时后提交页能链先提交页，先提交页通过入链显示；
      顺序错时报 `E_LINK_UNRESOLVED`，可重新领取后按顺序重试；不引入 pending edge。
      （2026-11-17 自测覆盖成功路径与未知链接拒绝，重试沿用现有 stale → next。）
- [x] route decision 的 `links` 可选；new 原样进 page 模板，merge 与现有 links 求并集；
      校验存在、去重、非自身，不设数量上限。
- [x] 更新后 body 不含 `## 关联`；观看区只有一个 `## 关联` 段，出链与入链混排、按 page id
      去重，且与 links 表一致。
- [x] 入库后旧页 body 不变、不新增出边；旧页观看区通过入链显示新页。
- [x] merge 只增链，不删除既有 links；links = 模板带入的现有 links ∪ decision 新增。
- [x] 建页 brief 在决策前能看到邻域（至少 title + summary + 出入链数）。
      （2026-11-17：`page_index` 已带 summary/outgoing/incoming，route brief 要求先看再
      读 boundaries。）
- [ ] route brief 按方案 G 改写后，用一篇含 2–3 个主题、且已有近义页的来源实测，能查到
      近义页并给出 merge，而不是按标题新建。
- [x] `doctor` 能报出无可用来源的孤儿页并标 removable（2026-11-17 实现，L7）。
- [x] `doctor` 直接删除 removable 页，不新增删页命令（2026-11-17 实现）。
- [ ] `doctor` 能报出指向未建页的链接（依赖方案 C）。
- [ ] 观看区有带 summary 与链接数的目录入口（若采纳方案 D）。
- [x] 同步文档：`curation.md` 链接规则、`docs/readme/cli.md` 载荷契约、
      `docs/agent/development-decisions.md` 新决策。
- [x] 改动后跑 `tests/selftest.py` 并在同一轮检索核实新内容。
      （2026-11-17：219/219 通过。）
