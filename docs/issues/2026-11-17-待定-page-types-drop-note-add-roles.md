# 待定：页面类型需要收敛：删除 note，收紧 process，补 event/index 角色

> 待定原因：记录完成后复核发现相关文件有未提交改动：`docs/readme/cli.md`、
> `skills/llm-wiki/SKILL.md`、`skills/llm-wiki/scripts/health.py`、
> `skills/llm-wiki/scripts/ledger.py`、`tests/selftest.py`。改动看起来是 0.4.2 的
> 恢复/自愈与检查分层，不是类型表本身；但按项目 issue 规则，本文降为待定，候选方案与
> 验收条件均未定案，等相关文件稳定后再复核转正。现阶段不要按本文实施。

## 问题

当前 `schema.types` 的四类 `concept/entity/process/note` 不在同一维度上：前三类是内容
形态，`note` 没有独立判据；`process` 与 `concept` 的边界靠「内容里有没有步骤」；类型还
被写进观看区路径。结果是 `note` 成为「装不进其他三类」的兜底出口，与
`curation.md:8` 的「一页 = 一个可独立检索、可独立引用的主题」直接冲突。另一个混淆是
`process` 与场景能力：文本上都是「一件事怎么完成」，但只有 agent 真能执行的那一段才该
成为场景（见 §3）。

### 1. `note` 没有判据，且与文档自己的反面模式矛盾

- 默认配置把 `note` 当一等类型（`skills/llm-wiki/scripts/ledger.py:182`），新库 `init`
  即提供。
- 它的章节只有「要点」（`skills/llm-wiki/scripts/tasks.py:20-25`），模板标题就是
  「note：零散要点」（`skills/llm-wiki/templates/note.md:1`），示例把「双零行动」写成
  主题、阶段、抓手、定位几条并列要点，没有结论。
- `curation.md:17-21` 的反面模式明确写着「把多个不相关主题塞进『工作笔记』→ 摘要无法
  概括，检索命中噪声大」，但 `note` 恰恰是这个出口。
- `curation.md:28` 一边定义「`note` 杂项」，一边劝「拿不准用 `concept`，不要为类型
  纠结」——同一句话既提供垃圾桶又劝人别用，说明它没有独立职责。

后果是 `note` 页的 `summary` 很难写成一句结论，`covered` 只能写「杂项」；而
`docs/readme/cli.md:165` 要求 `summary` 必填、1–200 字符，`skills/llm-wiki/SKILL.md:94`
要求它是一句完整的话。字段能填，内容没有判据。

### 2. `process` 与 `concept` 的边界靠「有没有步骤」，会诱导碎片化

- `curation.md:28` 只写「`process` 流程/专项工作」，没有说何时必须独立成页；
  `templates/process.md` 固定「结论/步骤/注意事项」。
- 公文里「要求」和「办理步骤」常在同一份文件、同一主题下。按类型分页会把同一主题拆成
  `concept` + `process` 两页，而 `curation.md:56` 明确「合并优先于新建」「同一主题出现
  第二页是最常见的质量退化」。
- 合理判据应该是「这个流程会被单独搜、且有独立出处组合吗」，而不是「内容里有没有
  步骤」。

### 3. `process` 与场景能力文本同构，但只有一段可执行

- 场景能力与 `process` 页都在写「一件事怎么完成」：场景是 `适用/步骤/判断点/完成标准`
  （`docs/readme/scenarios.md:50-84` 的「开户流程」示例），`process` 页是
  `结论/步骤/注意事项`。把 `scenario` 改叫 `workflow`，两者在文本上没有区别。
- 真正的分界是**可执行性**：场景要求「步骤要能执行」
  （`docs/readme/scenarios.md:95-96`），内置 `topic-exploration` 的每一步都落在
  `query`/`read`/`scenario` 上（`skills/llm-wiki/scenarios/topic-exploration/SCENARIO.md:20-42`）；
  而公文里的开户、审批、检查，agent 没有系统权限和经办身份，**根本执行不了**。把这类
  流程写成场景，脚本只能空转，步骤只能靠嘴念，不是真 workflow。
- 所以多数 `process` 页没有、也不该有对应场景：它们服务人读、引用、比对版本。只有流程
  里真有一段 agent 能执行的子任务（查账本、生成材料、核对清单）时，才值得封装场景；
  场景必须写明用哪些工具、哪些步骤由人完成。
- 场景不进账本、不进 `query`，页面也不能引用场景
  （`docs/readme/scenarios.md:5`、`:132-133`），只能场景单向引用页面。这进一步说明
  两者不是同一层的两种存法。

### 4. 类型轴混了内容形态与页面角色，`note` 因此成了万能出口

- `concept/entity/process` 是内容形态；参考文章里的 `comparison/overview/synthesis`
  是页面角色（跨源对比、总览、综述），本项目没有角色维度，也没有 index/枢纽页。
- 缺角色，任何「装不进三种形态」的内容就流向 `note`。类型不是知识域，却被写进观看区
  路径 `pages/<type>/`（`skills/llm-wiki/scripts/pages.py:169`、`:186`），改类型等于
  搬家，把一次性的分类固化成结构。

### 5. 没有 event/案例类型，时间型知识只能硬塞

- 公文语料里「某次专项检查发现问题及整改」「某事件的处置经过」有独立检索价值，但
  `concept` 的「结论/要点」装不下时间线、主体、结果；`process` 装的是可复用步骤，
  不是一次性经过。
- `curation.md:28` 把「专项工作」并列进 `process`，但专项工作/行动有起止、阶段和一次性
  结果，更接近 `event`/`overview`；`process` 应只表示**可复用的办理步骤**。
- 这类内容现在只能进 `note`（没结构）或 `concept`（丢时间性）。

### 6. 改动面已经可见：模板、自检、文档都把四类当一等

- `skills/llm-wiki/SKILL.md:92` 列四类模板；`skills/llm-wiki/scripts/health.py:39` 的
  `TEMPLATES` 元组含 `note.md`。
- `docs/readme/cli.md:328` 的默认配置示例含 `note`；`:334` 说明改 `schema.types` 只影响
  后续校验，已存在页面不自动迁移。
- `tests/selftest.py:646`、`:663` 用 `note` 建「机房巡检」页，正文是「每周巡检一次；
  记录保留一年」——这更接近 `process` 或 `concept`，恰好说明 `note` 被当成了顺手的选择。

所以删 `note` 不是删一个字符串：默认配置、章节表、模板文件、健康检查、示例文档、自检
用例都要同步；老库 `wiki.json` 仍保留 `note` 时应继续可用。

## 影响

- **知识质量**：`note` 页天然是流水账，`summary`/`covered` 写不实，检索与复用价值低，
  同主题重复建页的概率高。
- **决策一致性**：文档一边说「拿不准用 `concept`」，一边提供 `note`；route 时没有稳定
  判据，agent 的行为随措辞漂移。
- **结构固化**：类型进路径，改类型等于搬家；类型本可以只是 frontmatter 元数据加模板
  选择，不必成为目录结构。
- **扩展受阻**：index/overview/synthesis/event 没有合法落点；P1 issue 里的红链、目录、
  反链方案缺少类型侧的配合。
- **流程知识与执行能力混淆**：把不可执行的流程写成场景，agent 会假装能办；把场景当
  事实依据，又会绕过引文。边界不清时，错误都发生在「运行时」而不是「写入时」。
- **迁移成本可控但不可忽略**：新库默认变化、模板与自检同步；已有 `note` 页需要人工
  归类，数量取决于各库。

## 相关文件

- `skills/llm-wiki/scripts/ledger.py:181-191`：`DEFAULT_CONFIG`，`:182` 的 `types` 含
  `note`。
- `skills/llm-wiki/scripts/tasks.py:19-25`：`PAGE_SECTIONS`，`note` 只有「要点」；
  `:29-31` 的 `sections_for` 按类型取章节。
- `skills/llm-wiki/templates/note.md:1-18`：`note` 模板与示例。
- `skills/llm-wiki/scripts/validation.py:34-37`：类型必须在 `schema.types` 里；
  `:70-75` 按类型校验章节。
- `skills/llm-wiki/scripts/pages.py:164-187`：类型进入观看区路径 `pages/<type>/`。
- `skills/llm-wiki/scripts/health.py:39`：`TEMPLATES` 元组含 `note.md`。
- `skills/llm-wiki/SKILL.md:92`：四类模板的写法说明。
- `skills/llm-wiki/references/curation.md:8-21`（页面边界与反面模式）、`:28-31`（类型
  定义与目录）、`:45-56`（new/merge/skip 与合并优先）。
- `docs/readme/cli.md:165`（`summary` 要求）、`:328`（默认配置示例）、`:334`（改
  `types` 的影响）。
- `tests/selftest.py:646`、`:663`：用 `note` 建「机房巡检」页。
- `docs/readme/scenarios.md:5`（场景不是知识页）、`:50-84`（「开户流程」示例）、
  `:95-96`（步骤要能执行）、`:132-133`（页面不能引用场景，流程以场景为准）。
- `skills/llm-wiki/scenarios/topic-exploration/SCENARIO.md:20-42`：可执行步骤都落在
  `query`/`read`/`scenario` 上。
- `skills/llm-wiki/scripts/scenarios.py:12-15`：场景不进账本、不写 `.wiki/`，`query`
  直接读文件。

关联（非本 issue 改动对象）：`docs/issues/2026-11-17-P1-page-graph-invisible-and-unmaintainable.md`
已在方案 D/G 中讨论目录、红链与 route 判定流程；本 issue 是它的类型侧配套。

## 候选方案（未定案）

**先定判断标准，再谈类型表。** 一页成立需同时满足：会被单独搜；有独立出处组合；更新时
整体变化；能写出一句「是什么/谁/怎么办/发生了什么」的 `summary`，且 `covered` 不是
「杂项」。不满足就不建页，降级为现有页的一节、话题草稿或 `skip`。

**A. 删除 `note` 默认类型（建议的最小动作）**：从 `DEFAULT_CONFIG.schema.types` 去掉
`note`，删除 `templates/note.md` 与 `PAGE_SECTIONS` 条目，同步 `health.py` 的
`TEMPLATES`；`curation.md:28` 改成「拿不准用 `concept`」，并写明删 `note` 后的替代动作。
老库 `wiki.json` 保留 `note` 时仍可用，但文档标注为遗留配置。成本小；影响新库与自检
（`tests/selftest.py` 的 `note` 用例改为 `process`/`concept`）。

**B. 收紧 `process` 判据，划清与场景的边界**：

- `curation.md` 把 `process` 定义成「描述组织流程、可被引用/检索的知识」，**不承诺
  agent 可执行**；只有流程本身会被单独搜、且有独立出处组合时才独立成页。
- 同一主题的要求与步骤优先同页（`concept` 页可扩「办理」节，`process` 页可扩「要求」
  节，模板需同步；现有章节校验只查必需章节存在且有序，允许额外节，`tasks.py:39-49`），
  与合并优先一致。
- 「专项工作/行动」不算 `process`：有起止、阶段、一次性结果的，归 `event`/`overview`。
- 场景侧同步收紧：场景只收 agent 真能执行的方法；`SCENARIO.md` 必须写明用哪些工具、
  哪些步骤由人完成；不可执行的流程不得包装成场景；场景引用页面，不复制页面的事实与
  步骤。

成本小，主要是 `curation.md`、`docs/readme/scenarios.md` 与 route brief 的文案；不动
引擎。

**C. 引入角色维度**：frontmatter 增加 `role=knowledge/index/draft`（或 page kind），
`type` 只管骨架；index/overview/synthesis 作为角色页，不再塞进四种模板。它**不**用来
合并 `process` 与场景：`process` 仍是描述性内容形态，场景仍是账本外的执行能力。成本
中等；要改 frontmatter、校验、观看区路径与 `query` 输出，需与 P1 的 B/C/D 一起设计。

**D. 补 `event`/案例类型**：新增 `event`，章节为「经过/结论/影响（或处置）」，用于一次
性、时间型、有独立结论的事件，也承接「专项工作/行动」；与 `process`（可复用步骤）
区分。成本中等；模板、章节表、文档、迁移都要动；需先确认语料里这类页是否真会被单独
搜。

**E. 把 `note` 改造成 index**：保留名字但定义成「目录/枢纽页」，只放链接与一句话说明。
不推荐：名字会继续误导，且 index 的生成与维护依赖红链、目录能力（P1 的 C/D），不如直接
用角色维度。

建议顺序：先 A + B（配置、文档、模板，成本小，立刻消除垃圾桶并划清 process/场景边界），
再定 C（角色维度，决定 index/overview/synthesis 的落点），D 视语料证据再补；E 不采纳。

## 待确认与验收条件

待确认：

- [ ] 是否同意「写不出一句是什么/谁/怎么办/发生了什么、`covered` 只能写杂项，就不建页」
      作为成页判据。
- [ ] 是否删除默认 `note`（A）；老库的 `note` 页保留只读、限期归类，还是提供迁移命令。
- [ ] `process` 是否定义为「描述性流程知识、不承诺 agent 可执行」，并按「会被单独搜 +
      独立出处组合」收紧（B）；同主题的要求与步骤是否强制同页。
- [ ] 「专项工作/行动」是否归 `event`/`overview`，不再用 `process`。
- [ ] 场景是否强制声明所用工具与人工步骤，并禁止把不可执行的流程写成场景（B）。
- [ ] 是否引入 `role` 维度（C）；`role` 与 `type` 的边界，是否进入观看区路径。
- [ ] 是否新增 `event` 类型（D）；`event` 与「主题页里的案例节」如何取舍。
- [ ] 类型是否继续进观看区路径 `pages/<type>/`；若引入 `role`，路径按 `type` 还是
      `role`。

验收：

- [ ] 新库 `init` 的默认 `schema.types` 不再含 `note`，`templates/note.md` 与健康检查
      同步，`check` 通过。
- [ ] `curation.md` 给出成页判据与 `note` 移除后的替代动作（并节/话题/`skip`），不再
      出现「`note` 杂项」。
- [ ] `process` 判据写入 `curation.md` 与 route brief；用「同一文件既有要求又有步骤」
      的来源实测，产出单页而不是 `concept` + `process` 两页。
- [ ] `docs/readme/scenarios.md` 写明场景只收 agent 可执行方法、必须列工具与人工步骤；
      用「开户流程」类人工流程实测，不会产出场景；用 `topic-exploration` 类可执行流程
      实测，每个步骤都能落到具体工具。
- [ ] 用一份专项工作来源实测，route 产出 `event`/`overview` 而不是 `process`（若采纳 D）。
- [ ] 自检不再用 `note` 建页；`python3 tests/selftest.py` 通过。
- [ ] 老库（`wiki.json` 仍含 `note`）读取、更新、推送行为不变，文档写明这是遗留配置。
- [ ] 若采纳 C/D：frontmatter、校验、观看区路径、`query` 输出与文档同步，并有对应自检。
- [ ] 改动后在同一轮检索核实新内容。
