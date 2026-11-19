# 开发方向与决策

本文件记录用户已确认的方向与关键取舍。新决策由用户拍板后追加；未确认的候选方案写进
`docs/issues/`，不要写在这里。

## 2026-11-19：工作流与话题定义收进观看区

用户确认实际知识库会提交 `wiki/`，工作流定义和话题不应再裸露在观看区外；产物也要和定义
分层，避免任务目录挡住定义。此决策取代 2026-11-16 观看区设计里 `topics/`/`workflows/` 的
“首次投放后归用户”模型。

- 项目工作流定义放 `<view>/workflows/<id>/`，话题放 `<view>/topics/`，任务产物放
  `<view>/runs/workflows/<id>/<task>/`。`runs/` 不叫 `cache/`，因为其中的 `output/` 是最终
  交付物，叫缓存会诱导清理。
- 查找顺序：观看区 → 旧根 `<root>/workflows/`、`<root>/topics/`（只读兜底）→ 内置工作流。
  同 id 观看区优先；旧根与观看区 `WORKFLOW.md` 不同时只给 `legacy_conflict` 警告，加载仍用
  观看区版。旧根兜底保留一个发布版本，不自动搬迁。
- 新增 `origin`（`view`/`legacy`/`builtin`）；`source` 仍是 `project`/`builtin`，不合并两个
  语义。`workflow list` 的 `roots` 列出观看区与旧根中实际存在的目录，观看区在前。
- 删除 `view._seed_view_topics`、`view._seed_view_workflows` 与 `publish.topics`、
  `publish.workflows`；`publish` 只留 `dir`/`enabled`。`ensure_publish` 建
  `<view>/workflows`、`<view>/topics`，`enabled:false` 也建；`runs/` 在第一次产物时创建。
- 新增只读 `ledger.view_dir(root)`，供不打开账本的 `workflow` 命令解析 `publish.dir`；
  `query` 直接用已打开账本的 `publish_dir`。
- `SKILL.md` 必须自包含目录树、所有权、查找顺序、产物路径与 `publish.dir` 语义，技能单独
  拿出去也能落地。
- 本仓库根 `workflows/` 是示例/开发用定义，和 `skills/` 一样是可选产物，不是运行时加载入口；
  本仓库不迁移、不改 `.gitignore`、不在根跑写状态命令。

## 2026-11-19：页面证据与 raw 来源用标准 Markdown 链接

用户要求“先完成图谱的三层链接”，并指出有明确路径的信息应可用超链接标定图谱关系、在其他
Markdown 软件里也有效果。

- 页面的 `## 证据` 行改为 `- [\`<raw 相对路径>\`](<相对链接>)（\`<来源 key>\`）：<引文>`：
  链接文字仍显示 raw 路径，括号里保留机器 key，引文校验与 `sources` 并集都不变。
- raw 副本末尾由引擎追加 `<!-- llm-wiki:provenance -->` + `## 原文来源`，每行链回一个挂载
  来源；读者从页面可点到 raw，从 raw 可点到原始文件，形成 page → raw → source 的可点击链。
- page ↔ page 也改成标准 Markdown 链接 `[目标页标题](相对路径)`：账本 `meta.links` 仍只存
  page id，标题和路径由渲染层按当前观看区/导出目录现算；Obsidian 仍能建立图谱边，其他
  Markdown 软件也能点开。`[[id]]` 只作为找不到目标时的降级输出，正常页面不再出现。
- 链接目标按发布位置现算，不写进账本正文；空格、括号、`#`、`?`、`%` 做 percent-encode。
  `render_page` 的 `view_path` 是仓库相对路径，`view_repo_path` 负责把发布相对路径换过来。
- raw 的来源段不是正文：`split_provenance` 在 `body_digest` 前剥掉它，给旧文件补链接不会让
  引用页 stale，也不会被当成手改。`ingest` 末尾的 `refresh_raw_provenance` 只补缺失的旧
  raw；已有来源段的文件不再重排，避免覆盖用户追加的文字。
- `split_evidence` 同时认旧式纯文本行和新式链接行，`sync` 反向回收不会因格式升级丢证据。

## 2026-11-18：任务租约按会话归属，别的会话立刻停手

触发：Session `3768c894-2598-4eb4-9d63-00b547a27048` 一批次摄入时，主智能体派 3 个子智能体
并行领取。`next` 是全局单消费者，三路加起来 100 次 `E_TASK_UNSUBMITTED` 空转，24 分钟只多
写 10 页（≈0.4 页/分，串行阶段 ≈0.9 页/分），还有一个子智能体停手时仍占着租约未提交。

- 租约记领取会话：新表 `task_leases(task_id, owner, leased_at)`，`tasks` 结构不动（`ensure_columns`
  不做迁移，加列会让旧库直接报 `E_STATE`）。`--owner` 优先，其次 `$LLM_WIKI_OWNER`、
  `$DSH_SESSION_ID`；都没有就不记归属。
- `next` 先 `sweep_leases`，再把租约分成本会话与他会话：本会话仍报 `E_TASK_UNSUBMITTED`，
  他会话报 `E_INGEST_BUSY` 并指出占用方；`submit`、`query --task` 提交他会话的任务报
  `E_TASK_FOREIGN`。`release` 仍对任何会话开放，作为运维恢复口子，输出原持有方。
- 改写 2026-11-17「单写者前提下不再区分「谁的租约」」：不区分归属时，后来者只知道自己领不到，
  无法判断是"自己没交"还是"别人在摄入"，于是照常重试或派并行子智能体；归属可见才能让后来者
  立刻停。
- 文档同步 `SKILL.md`（取任务硬约束、提交失败两条）、`references/recovery.md`（新小节）、
  `cli.md`、`recovery.md`、`design-principles.md`、`engine.md`。

## 2026-11-18：已有页面可以补关联，仍然只增不删

用户确认方案 A：补引擎能力，不做迁移、不做删链。触发是实际库检查时 22 页的 `links`
全为空，Obsidian 图谱全孤立；当天复查该库已增至 35 页、3 条边。

- 新增 `page link <id> <target...>`：只改已存在页，写新修订（`origin=link`），目标必须是
  已存在页；空目标或指向自己报 `E_USAGE`，未知目标报 `E_LINK_UNRESOLVED`，重复链不产生
  新修订。写入后立即推送观看区，不再等下一个命令。
- `page import` 读 frontmatter 的块列表 `links`，与旧链接取并集；内联 `[a, b]` 不认，报
  `E_PAYLOAD`，避免把字符串当 id。
- route/page brief 与 `SKILL.md`、`curation.md` 改为写页时必须判断关联（2–5 个出链，确实
  没有才留空），避免“可选”继续制造孤立页。
- 存储层继续只增不删；不设删链入口、不做 pending edge、不做红链。
- 增加 `page link --repair [--dry-run]`：按共享同一 raw 来源把页两两配对，已有任一方向的
  边就跳过，每对只补一条（小 id → 大 id）；先 `--dry-run` 看计划，确认后执行。它只补
  缺边，不改变已有方向，也不把正文 `[[...]]` 变成第二套边。
- 落地时发现并修复：观看区把 `## 关联` 排在 `## 证据` 之后，`sync` 回收时先按关系段剥掉再
  解析证据，否则带关系的页面会在正文里重复累积证据块。

## 2026-11-13：转型为 skills-only

用户原话："这个项目要开始转型了，把重心放在skills内的能力开发上，把之前使用rust的设定
和坑点，可以借鉴的借鉴，不可以借鉴的删除，最终只会有python skill这版了"。

- 交付物收敛为 `skills/llm-wiki/`（Python，标准库）与 `skills/llm-wiki-session/`（会话技能
  已于 2026-11-16 内化，见下）。
- 删除 Rust 源码、`output/` 发布包、`.github` 的 release 构建、`scripts/build-output.sh`。
- 归档：分支 `rust-final`、tag `rust-archive-2026-11-13`；旧解析器分支
  `fix/parser-content-reliability` 留 tag `rust-parser-archive`。
- 保留并已落地的借鉴项：
  - 解析严格性：PDF 先 `pdftotext` 后 `pypdf`，都不出文本进 `unsupported`；
  - `ingest.max_source_bytes` 超限分类为 `oversize`，不解析、不中止整批；
  - 来源变化/消失的漂移处理与 `stale` 状态；
  - 结构化错误码与 `error.next`；
  - `scan.ignore` 与 `ingest.batch` 配置键。
- 不借鉴：任务树（根/子任务与合并阶段）、模型执行器、单二进制分发、发布包目录约定。

## 2026-11-13：账本为源，镜像保留

> 镜像部分已于 2026-11-17 撤销，见「删除页面镜像」。

- agent 不能直接写页面：写入必须经 `submit` 校验，或 `page import` 人工接管。
- `query`/`read page` 只读账本，不读页面文件。

## 2026-11-17：删除页面镜像

用户原话："删除镜像"。

- `.wiki/pages/` 镜像与 `.wiki/drift/` 漂移留证机制整体删除：`write_rev` 不再写副本，
  `ensure_mirror`/`record_drift`/`migrate_legacy_pages` 与 `drift` 表一并移除，
  `doctor` 不再修复镜像。
- 页面正文只剩两处：账本 `page_revs` 与观看区 `<view>/pages/`；观看区由 `sync_view` 按
  账本推送，手改仍只告警、由人工 `sync` 回收。
- `query --page` 的 `path` 改为观看区路径；发布关闭时退回 `page:<id>`。
- 旧版残留的 `.wiki/pages/` 与 `.wiki/drift/` 不自动接管也不自动删除，由用户手动清理。
- 理由：镜像除 drift 留证、旧库接管、稳定 id 路径外没有查询/读取/导出/回收消费者，
  与观看区重复维护，反而让"账本是唯一事实源"的表述含混。

## 2026-11-13：任务协议

- 对外只暴露"任务 + 语义载荷 + 证据"，内部表结构、租约、epoch、哈希不写进技能文档。
- 任务只有 `route` 与 `page` 两种；一份正文（一个 raw）一条 route 任务（同正文的多个路径
  合在一条里），一页一条 page 任务。
- todo 只作为显示层投影，账本是唯一进度权威；没有 todo 工具的运行时不受影响。
- 默认每批 20 条；租约 15 分钟；提交失败零写入并保留 `last_error`。
- 已取消 `reconcile` 任务类型：页面回滚用 `page restore`（当时的镜像漂移自动还原机制已于
  2026-11-17 随镜像一并删除）。

## 2026-11-17：一个来源可以拆成多页

用户确认按“引擎 + 示例 + 文档”一起改，触发是一篇讲话同时讲“主题日活动”与“双零”行动，
旧协议只能合并成一页。

- route 决定从“每份正文恰好一条”改为“每份正文至少一条”：一份正文含多个可独立检索的
  主题时，提交多条 decision 指向不同 page；同一目标页不得重复，skip 不能与
  new/merge 并用。
- 同一次 route 提交拆出的 page 任务共享 `route_task`，是“一份正文只挂一棵 page 树”的
  唯一例外；来源在最后一个相关 page 任务关闭后才转为 `done`。
- `curation.md` 的示例同步拆成两页并互链，避免示例继续示范“一份文件一页”。

## 2026-11-17：关联由 route 决定，正文不再写关联

用户确认：不设人工补链，新页与旧页的关联在 route 时定下；`links` 只增不删。

- route decision 增加可选 `links`：`new` 原样进 page 模板，`merge` 是增量，提交时与页面
  已有 `links` 取并集。
- `links` 存进 `page_revs.meta` 与 `links` 表，由提交载荷写入；不再从正文 `[[...]]` 派生。
  提交时剥掉旧 body 的 `## 关联` 段，正文里的双方括号不产生图边。
- 观看区按 `links` 表渲染一个 `## 关联` 段，出链与入链混排去重；旧页 body 不动也能看到
  指向它的新页。
- 不做 pending edge：`links` 只能指向提交时已存在的页（含同批已先提交的兄弟页）；同批
  兄弟页按提交顺序，顺序错报 `E_LINK_UNRESOLVED` 后重试。当时没有人工补链或删链入口；
  2026-11-18 增加只增的 `page link` 与 import `links`，仍不设删链。
- 理由：关联原来在写页时冻结在 body 里，旧页无法指向新页；改成 route 决策后入库即有边，
  观看区靠入链补齐阅读导航。

## 2026-11-17：关联只连现有页面，不引入分类与近似检测

三件事都在「页面关联不可见、不可维护」的复核里定案并已落地，对应的 issue 文件已删除，
决定留在这里。

- **页面类型只表示形态，不表示知识域。** 不引入 `domain` 字段或分类维度、不生成目录页；
  观看区当前只有各类型篇数，没有主题入口，这个「组织方式不可见」的现状暂时接受。要重开
  先回答三件事：为什么需要这一维、谁来维护、它和页面类型怎么分工。
- **不设红链，也不登记待建页。** `links` 只能指向提交时已存在的页（同批已提交的兄弟页
  算在内）；「该建但还没建」由 route 的 `pending_pages` 和决策理由承载，不在账本里建第二个
  事实源——有红链却没有人工补链入口，也没人能确认它还该不该补。
- **不做同题相似自动检测。** 中文没有可靠分词，标题与 summary 的字符级近似误报过高，
  `doctor` 报多了只会被忽略；同题判定交给 route 流程里逐条看 `query --task` 的命中页。

## 2026-11-13：环境边界

- 不 OCR、不下载模型/语言包、不联网、不装依赖。扫描件只报 `unsupported`。
- 触发事件：旧技能副本的 agent 为处理 9 份扫描件自行下载 `chi_sim.traineddata`，被用户
  中止。技能文档中"安装 OCR 后再摄入"的表述已删除，避免再次诱导。

## 2026-11-15：所有 wiki 操作以工作区为根

用户原话："所有的wiki操作都需要以工作区为操作目标，无论wiki技能是项目级还是全局的，
在具体的工作区目录下使用时，都必须要以会话所在工作区作为执行根目录，不要跃出工作区
操作，缓存相关的信息都放到.wiki目录下"。

- 技能安装位置（项目级/全局）只决定从哪里加载，不改变知识库位置：知识库根 = 会话
  工作区根。
- 引擎强制：`--root` 只能是当前目录或其子目录；`--payload`、`body_file`、
  `page import --from`、`export` 目标越出根目录返回 `E_PATH`。
- 草稿、载荷、导出、临时文件统一放 `.wiki/`；`--dry-run` 临时库从系统 temp 改到
  `.wiki/tmp/`，退出时清理。
- `selftest.py` 增加 6 项越界用例，总计 26 项。

## 2026-11-15：技能信息面与任务包

用户原话："关于skill信息切面的问题，wiki能力内部处理的部分，完全不要暴露到skill可读范围，
使用代码解决掉，需要agent协助的流程，在skill中展示清楚。这是这个技能开发最基本的前提，
不信任agent的流程决策。"

- 技能只展示 agent 真正要做的流程：`init/ingest`、`next`、按 `brief` 填 `template`、
  `submit`、`query`、工作区边界。epoch、租约、重试、任务状态、错误码表、doctor、
  drift 细节不写进 `SKILL.md`。
- `next` 只返回 `{id, brief, template, work}`。`brief` 说明做什么、怎么做、提交后发生
  什么；`template` 是引擎固定好的载荷骨架。`submit` 不再要求 epoch。
- 可预先决定的结构由代码固定：route 模板预填来源与可选动作；page 模板预填页面 id、
  标题、类型、章节骨架与来源证据槽；新建页面提交时校验章节未缺失、未改名。
- 任务一次一个；`--batch` 只留给调试/批量运维。agent 不参与任务选择与状态迁移。
- 技能目录只保留 `references/curation.md`（页面质量标准）。CLI、安装、恢复、验证记录
  移到 `docs/readme/` 与 `docs/agent/`，不再躺在技能可读范围内。
- `selftest.py` 增加任务包断言，总计 30 项。

## 2026-11-16：场景能力与知识页分离

用户要求：检索到场景时，按场景流程完成任务；场景放项目根目录 `场景能力/`，组成参考
skill 的主 Markdown + 关联脚本/说明。确认的取舍：

- 入口扩展 `query`：一次检索同时返回知识页与场景，场景在 `scenarios` 字段；另提供
  `scenario list` / `scenario load <id>` 做检查与加载。
- 目录形状：`场景能力/<id>/SCENARIO.md`、`scripts/`、`references/`。`description` 必填，
  `tags` 可选，参与检索；正文是流程。
- 不摄入：`ingest` 整目录跳过 `场景能力/`，不写 sources、不排任务、不进账本；`query`
  直接扫描文件。这样改流程不需要重新摄入。
- 加载返回：主文件全文 `content`、解析后的 `meta`、`path` 与 `resources[]`（`script` /
  `reference` / `other`），由 agent 按流程执行。
- 只读：`scenario list/load` 不创建 `.wiki/`，不要求先 `init`；非法场景不进 `query`，
  但在 `list` 里带 `error` 可见。
- 边界：场景是能力不是知识页；流程以 `SCENARIO.md` 为准，事实依据仍以页面和 raw 为准。
- 代码拆出 `frontmatter.py`、`search.py`、`scenarios.py`，`wiki.py` 从 1954 行降到
  1908 行，保持单文件 2000 行上限。

## 2026-11-16：会话素材内化为 llm-wiki 能力

用户原话："我想把llm-wiki-session内化为llm-wiki的能力，其他设定不变"。

- 交付物收敛为一个技能 `skills/llm-wiki/`，删除独立的 `skills/llm-wiki-session/`；技能
  描述同时覆盖建库、场景与会话素材三类触发。
- 会话素材的定位、归属分流、提炼标准、授权与写入流程原样保留，落在
  `skills/llm-wiki/references/session-materials.md`；`SKILL.md` 只给触发条件、边界与入口。
- 素材仍写当前项目根目录的 `wiki-sources/`，不运行 wiki CLI、不写 `.wiki/`、不摄入；
  引擎、命令面、场景能力与安装方式都不变。

## 2026-11-16：话题探索成为默认流程，会话素材流程移除

用户原话："query默认使用话题探索场景，基于用户的问题，设定探索主题，围绕这个主题做多次
探索，给出一个总结性的回复，并且默认咨询用户是否将主题沉淀下来……一个是能力的沉淀，一个
是资料的进一步深度探索"。

- `query` 直接回答知识问题的默认路径固定为「话题探索」：定主题 → 找现成能力 → 找已有
  话题 → 多轮检索 → 补原始依据 → 判断收敛 → 总结 → 问是否沉淀。入口是内置场景
  `topic-exploration`；项目放同名场景即覆盖。0.4.1 起改为显式 `scenario load`（见下条）。
- 内置场景随技能分发，放在 `skills/llm-wiki/scenarios/topic-exploration/SCENARIO.md`，自包含、
  无 `resources`，不参与隐式匹配，只通过 `scenario search` / `scenario load` 使用。
- 旧的会话素材流程（写 `wiki-sources/` 待摄入）移除；沉淀物改为 `话题/<主题>.md`。
  话题是 wiki 的探索层，与全局 `wiki-sources/` 的开发解决方案沉淀无关。位置按 2026-11-15
  的工作区边界落在 `<root>/话题/`；一个主题一份，固定六节：主题、结论、依据、未决问题、
  下一步、能力候选。`ingest` 跳过 `话题/`，`query` 用 `topics` 返回命中，计数与页面、
  场景分开。
- 沉淀同时产出两类后续：能力候选（流程稳定时转 `场景能力/<id>/SCENARIO.md`）与资料深挖
  （待摄入来源、待补页面、下一轮关键词）。
- `skills/llm-wiki/references/session-materials.md` 删除，其归属分流、可复用标准、授权与
  去重约束并入内置场景。`~/.dsh/AGENTS.md` 的 wiki-sources 规则保持原样：它服务于各开发
  项目的解决方案沉淀，与本项目 wiki/话题无关。
- `selftest.py` 增加内置场景、话题跳过摄入、话题检索与项目覆盖内置场景用例，总计 57 项。

## 2026-11-16：场景改为显式检索，不随 query 注入

用户原话："场景检索要显式操作，必须明确检索场景才能应用场景相关分支能力，以免场景过度
干预主流程"。

- `query` 只返回页面与 `topics`，移除 `scenarios`、`scenario_matches`、`topic_scenario`；
  普通知识问答不会被场景分支改写范围或答案结构。2026-11-19 起 `query` 也返回工作流候选，
  但仍不自动加载，见下文「`query` 带工作流候选」。
- 新增 `scenario search <词...> [--limit N]`：显式检索项目场景与内置场景，返回 `source`
  与 `load`；只有检索并 `scenario load` 之后才按场景流程执行。
- 内置场景不再自动出现在 `query`；话题探索默认流程仍保留，但入口是显式
  `scenario load "topic-exploration"`，由 `SKILL.md` 与内置场景约定，不靠查询字段注入。
- 场景只读、不进账本、不参与 `ingest` 不变；`scenario list` 仍列非法项，非法场景不进
  `scenario search`。
- `selftest.py` 改为断言 `query` 无场景字段、`scenario search` 命中项目/内置场景、
  场景改动即时可检索、非法场景不进检索，总计仍 57 项；`VERSION=0.4.1`。

## 2026-11-16：场景目录改用英文名 `scenario/`

用户要求：项目里不要用中文做路径命名，中文只出现在 Markdown 内容里；第一个外部场景落在
仓库根的 `scenario/ppt_creator/`。

- 扫描根从单一 `场景能力/` 扩为 `scenarios.SCENARIO_DIRS = ("scenario", "场景能力")`，
  `scenario_roots(root)` 只返回实际存在的目录，`scan` 逐目录收一级子目录并跳过重复 id，
  因此同名时 `scenario/` 生效，遗留的中文目录不会盖住改过的副本。
- 旧目录继续可用，不迁移存量工作区；`ingest` 两个目录都整目录跳过，
  `publish.dir` 两个名字都算保留名。
- `scenario list` 增加 `roots` 字段，`root` 改为实际生效的目录名；`docs/readme/scenarios.md`
  与 `cli.md` 同步。
- `selftest.py` 增加 `scenario/` 场景可加载、同名时英文目录优先、两个目录都不被摄入、
  `scenario` 不能作为 `publish.dir` 的断言；叠加在 `d5f4327` 上总计 105 项。

## 2026-11-17：技能包不携带 `.cmd`/`.bat`

- 下游分发禁止 `.cmd` 后缀，`skills/llm-wiki/scripts/bootstrap.cmd` 删除，Windows 入口只保留
  `bootstrap.ps1`。cmd 与 Windows Terminal 里执行文档给出的
  `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\bootstrap.ps1`，参数原样追加，
  退出码由 PowerShell 透传；双击启动不再作为交付要求。
- `tests/selftest.py` 增加守卫：`skills/` 下出现 `.cmd`/`.bat` 即失败。

## 2026-11-17：任务目录化、单页单任务与工作区文件锁

用户原话："我认为看不到是技能面不暴露，最好放在db不借助工具读不了的程度就好了，不要
脱离工作区"、"1. 按后者走吧"、"1.、3感觉是一个问题，按目录改造看看难度大不大，大的话，
这次先不做；2. 转换任务不算的话，不允许，而且得用文件做一个进程锁，防止多进程同时
竞争资源"。

- 真实源仍是 `.wiki/wiki.db`：任务状态、租约、epoch 全在账本里，技能与 brief 不出现
  表名、租约、epoch。载荷文件只是中转，不是任务的唯一事实源。
- 目录只是任务素材，不是队列：`next` 领取后物化 `.wiki/tasks/<id>/`
  （`brief.md`、`work.json`、`payload.json`），`submit` 默认读该 `payload.json`；
  提交成功或过期删目录，释放后重领只刷新 brief/work，不覆盖 agent 已填的 payload。
  `.wiki/payloads/` 不再创建。
- 不改 2026-11-15「任务一次一个、agent 不参与任务选择」；同一页面同时只允许一个未关闭
  的 page 任务（route 与来源自己的转换任务不计），账本用部分唯一索引兜底，`enqueue_task`
  撞约束时复用已有任务；已领走的页面仍报 `E_STATE` 让 route 等待。
- 多进程竞争用文件锁解决：写账本/观看区的命令在 `main()` 里抢 `.wiki/lock`，等 5 秒仍
  拿不到返回 `E_BUSY`（退出码 4）；posix 按 pid 判活，非 posix 只按 30 分钟陈旧阈值接管。
  `--dry-run` 且没有 `.wiki/` 时不加锁，保持「试运行不建 `.wiki/`」。
- 与本次无关的共享 raw 人工改写用例（`tests/selftest.py` 的 hand-edited raw 三条）按用户
  要求先删除，不在这轮裁决共享 raw 语义；`selftest.py` 自测 201 项。

## 2026-11-17：`next` 重新发放未提交的任务（同日晚已改写，见下一条）

修 `docs/issues/2026-11-16-P2-next-empty-with-leased-tasks.md`：领取后中断再跑 `next`，
旧实现返回空，agent 按技能说明停摆到租约过期。

- 队列不足一批时，`next` 按同样顺序（route 优先、`created_at`、`id`）重新发放仍在租约里的
  任务，`summary` 以 `resuming` 开头；任务目录与已填 `payload.json` 保持不变。重新发放前
  照旧查漂移，失效任务置 `expired` 不发出；队列与租约都空时才返回 `no queued tasks`。
- 单写者前提下不再区分「谁的租约」，同一工作区允许恢复任何未提交任务；并发重复领取由
  `submit` 的 `state='leased'` 与可选 `epoch` 兜底，后到的提交拿 `E_TASK_STALE`，账本不会
  出现重复记录。
- `release` 不进技能流程，只作为运维口子保留；`SKILL.md` 仍不出现租约、epoch 字样，
  只说「同一个任务会再发回来，`payload.json` 保留」。

## 2026-11-17：`next` 不再重发未提交的任务

改写上面那条「重新发放」的决定：用户指出 `next` 把同一任务当新任务再发，既不报出「当前
任务未提交」，还会续租、加 `epoch`、加 `attempts`，`status` 的「等租约过期」永远不会发生。

- 有 `leased` 任务时 `next` 直接失败，报 `E_TASK_UNSUBMITTED`：`message` 列任务 id，
  `error.next` 给 `submit <id> --payload .wiki/tasks/<id>/payload.json` 与 `release <id>`。
- 不重发、不续租、不 `epoch + 1`、不 `attempts + 1`；租约到期仍按现有规则回队或置
  `expired`。`--kind` 与 `--batch` 受同一条门禁限制：手头没结束就不发新任务。
- 会话重启的自助恢复不变：错误里带着任务目录路径，`brief.md`、`work.json` 与已填的
  `payload.json` 都还在，`release` 是「放弃当前任务」的出口（只写在运维口子，技能流程
  仍不提租约与 epoch）。

## 2026-11-17：`wiki.py` 按职责拆分，入口只留转发

关闭 `docs/issues/2026-11-16-P2-split-wiki-engine.md`：`wiki.py` 长到 2868 行，命令、账本、
观看区与任务混在一个文件里，超过单文件 2000 行的可维护边界。

- 落地：命令层进 `cli.py`；`ledger.py` 收拢 DDL、`LedgerMixin` 与组合根 `Wiki`；页面、
  观看区、任务、摄入分别进 `pages.py`、`view.py`、`task_store.py`、`ingest.py`；纯函数公共
  部分进 `util.py`。`wiki.py` 只保留 `python3 wiki.py <cmd>` 入口与 `tests/selftest.py`
  用到的再导出。
- 取舍：`Wiki` 仍是唯一持有连接的对象，四个 mixin 组合成一个类，拆分只「按职责搬方法」，
  不改命令输出、错误码、`docs/readme/cli.md` 契约与 `.wiki/` 布局；新增 `util.py` 是为了
  让页面与观看区不反向依赖账本模块。
- 约束：新增或再拆 `scripts/` 模块时，同步 `docs/agent/engine.md` 的布局表与 `health.py`
  的 `SCRIPT_FILES`，`wiki.py` 不再回长。

## 2026-11-17：账本自愈，`check`/`init` 不再是前置门禁

关闭 issue「初始化与自检仍写在提示词里」（已删除，历史见 Git）：提示词里的「先 `check`，通过
再 `init`，然后 `ingest`」把初始化责任推给 agent，空目录、半初始化目录都会先撞一次错误。

- 自愈在打开账本时做：`Wiki.ensure` 补目录、`wiki.json`、表与索引；补出的路径收进
  `data.repaired`（路径数组，与 `created` 同形），每个命令的 JSON 输出都带。`command_init`
  不再单独给 `created`。
- `check`/`doctor` 退回只读诊断；命令入口只跑安装级检查（解释器、技能文件、工作区可写），
  失败 `E_HEALTH`。账本缺失或残缺不再阻塞任何命令。
- 致命错误对象加 `doc`（技能内 `references/recovery.md` 绝对路径）与 `section`（小节标题）；
  新增 `references/recovery.md` 作为按错误码分节的处置手册，`docs/readme/recovery.md` 留入口。
- 不做迁移（用户原话「不用考虑已有库，迁移，就按照全新的来，不要留历史包袱」）：有数据的
  旧结构直接 `E_STATE`；旧表全空时按当前 `SCHEMA_SQL` 原地重建，不算迁移。
- 不加 `init --reset`（用户原话「手动直接把.wiki删了也能做到」）：重置就是删 `.wiki/` 重新
  `ingest`，手册里写清步骤，不再加命令。
- `VERSION=0.4.2`；`selftest.py` 208 项。

## 2026-11-17：Windows 引导脚本不再安装 Python

下游 agent 都自带 Python 运行时（用户验证：运行时为 3.12.13 以上），`bootstrap.ps1` 里
「下载并静默安装 Python 3.12.8」这条路无人走，镜像列表、安装器参数、`winget` 回退与
「只认恰好 3.12」的判定都是净负债。

- `scripts/bootstrap.ps1` 精简为两步：找 3.12+ 运行时（`-Python <python.exe>` 可直接指定，
  否则依次 `%LOCALAPPDATA%\Programs\Python\Python3*` → `py -3` → `python` → `python3`，跳过
  WindowsApps 转发桩），然后把参数透传给 `install.py` 并照传退出码。
- 不再下载、不装 Python、不改 PATH；找不到运行时输出 `E_PYTHON`、退出码 1，错误对象带
  `doc`（`references/recovery.md`）与 `section`（「运行时与安装」）。
- 版本判定改为 ≥3.12（旧版只认恰好 3.12，会把只有 3.13 的机器强行降到 3.12.8）。
- 同步 `README.md`、`docs/readme/install.md`、`skills/llm-wiki/SKILL.md` 与
  `skills/llm-wiki/references/recovery.md` 的运行时小节。
- `VERSION=0.4.3`；`selftest.py` 仍 208 项（PowerShell 脚本不进自测）。
- 关闭 issue「Windows 引导脚本尚未实机验收」（已删除，历史见 Git）：Windows 上技能目录由
  上游放置、agent 自带 3.12+ 运行时，没有安装这一步，`bootstrap.ps1` 与 `install.py` 只在
  手动安装时才涉及，不为它们单独排验收。验收改为在真实环境里走一遍 `ingest` 到 `query`。

## 2026-11-18：页面类型收敛为三类，不引入 role 与 event

复核 issue「页面类型需要收敛」时定案。原四类里 `note` 没有独立判据、成了「装不进其他三类」
的兜底出口，`process` 与场景能力又都在写「一件事怎么完成」。已落地与未采纳的分开记：

- **默认类型只保留 `concept`/`entity`/`process`。** 删 `note` 的默认配置、模板与健康检查；
  章节表里保留 `note` 的映射，作为老库遗留（老库页面更新时仍按原骨架校验，删掉会被判缺章节）。
  收口方式是人把老库这类页并进 `concept`/`process`，不做迁移命令。
- **`process` 是描述性流程知识，不承诺 agent 可执行。** 同一主题的要求与步骤写在同一页，
  不拆成 `concept` + `process`；「专项工作/行动」这类有起止、一次性结果的内容不算 `process`，
  写成相关主题页里的一节或并进 `concept` 页。
- **场景只收 agent 可执行的方法。** 公文里的开户、审批只有流程本身会被单独搜、且有独立
  出处组合时才成页；没有系统权限和经办身份的流程属于 `process` 知识，不包装成场景。
  人工环节在场景里单列「由人完成」。
- **不引入 `role` 维度（C）。** 它要同时改 frontmatter、校验、观看区路径与 `query` 输出，
  而收益是给 index/overview/synthesis 找落点；这些页目前按成页判据处理，不值得为一个
  尚未出现的需求先铺一条维度。
- **不把 `note` 改造成 index（E）。** 名字继续误导，且 index 的生成与维护依赖红链和目录
  能力，都与「不设红链、不生成目录页」的决定冲突。
- **暂不加 `event` 类型（D）。** 事件与专项工作的落点目前是主题页的一节。要重开先拿语料
  验证：同一事件是否被多个页面引用、被反复单独检索、结论是否冻结到不该跟着主题页改。
  出现「这一节该能独立检索」的实证再加，不因为「类型表少一个形态」而加。
- 自检不再用 `note` 建页，另加老库回归（`tests/selftest.py` 的 `legacy_note_flow`）；
  `VERSION=0.4.3`，`selftest.py` 242 项。

## 2026-11-18：raw 升为一等对象，删除 `read`

复核 issue「read 命令在观看区里的定位」后定案并落地：

- **raw 身份取正文，不取文件字节。** `raws.id` 与 `sources.raw_digest` 都用去前言后的正文
  摘要（`util.body_digest`：CRLF 归一、去首尾空行）；同一份正文的多个路径挂同一个 raw，
  前言里的挂载列表变化不再让引用页误报 `E_SOURCE_DRIFT` 或假 `stale`。
- **一份正文一条 route 任务。** 任务 `subject` 是 raw id，`work.sources` 带该 raw 的全部挂载
  路径；一条 decision 覆盖整个 raw，页面 `sources` 收录全部路径；skip 只把仍 `pending` 的
  路径置 `skipped`。已打开任务持有同一 raw 时，新路径直接挂进该任务，不再排第二条。
- **`ingest` 末尾清理孤儿页。** 被引来源全部缺失、且没有出入链的页面连同观看区文件一起
  删除；有链的只由 `computed_status` 标 `stale`，等链断了再清。
- **删除 `read`。** 原文已在观看区，agent 直接打开 `work.json` 的 `sources[].raw`；页面正文用
  `query --page`；话题与场景结果只给 `path`。日常命令面去掉 `read`。
- 同步 `docs/readme/cli.md`、`topics.md`、`design-principles.md`、`recovery.md`、
  `docs/agent/engine.md`、`verification.md`、`skills/llm-wiki/SKILL.md` 与
  `references/`、内置场景；`selftest.py` 249 项。

## 2026-11-18：场景能力改名为可执行工作流

复核 issue「scenario 改名 workflow」后逐项定案并落地。`scenario` 这个名字不承载「可执行」，
和 `process` 页的边界也说不清；改名同时把「可执行」写成契约，而不是只换术语。

- **正式名 `workflow`，中文「可执行工作流」。** 首次出现写定义句，其后简称「工作流」；
  「适用场景/使用场景」这类普通词不替换。
- **命令硬切，不设别名。** 只留 `workflow list/search/load`，`scenario` 命令直接移除；
  目录双读已经保住旧工作区的内容，再留一套旧命令只会让两种说法并存。
- **目录曾短期双读。** 改名当次保留 `workflow/` > `scenario/` > `场景能力/` 与 `SCENARIO.md`
  回退，同日复核后收成只认 `workflows/`（见下条），旧目录不再读取。
- **步骤契约四类、每步必标。** `tool:`/`human:`/`agent:`/`verify:`，只解析第一个 `## 步骤`
  的顶层列表项；整份至少一个 `tool:`，纯人工流程判 invalid 并指向 `process` 页。issue 原设想
  的 tool/human 二分装不下「判断收敛」「组论点」这类纯判断步骤，故加 `agent:`。
- **旧内容 0.6.0 截止。** `UNTYPED_ALLOWED` 控制：0.5.0 零标记仍合法但给 warning，0.6.0
  翻转后判 invalid；仓库内两份（内置 `topic-exploration`、`workflows/ppt_creator`）当次补标记。
- **观看区随目录收口。** 只投放 `workflows/`；`publish.workflows` 为新键，旧
  `publish.scenarios` 的迁移兼容同日移除，遗留键按未知键忽略。
  > 该条已被 2026-11-19「工作流与话题定义收进观看区」取代：不再投放，
  > `publish.workflows` 已移除。
- **0.5.0 一次装完，0.6.0 只收紧。** 没有为旧目录设移除窗口，旧目录支持在改名当天删掉。
- 代码把 `scenarios.py` 改为 `workflows.py`，`SCENARIO_DIRS`→`WORKFLOW_DIRS`，
  `E_SCENARIO_INVALID`→`E_WORKFLOW_INVALID`；内置目录改 `skills/llm-wiki/workflows/`；
  同步 `README.md`、`SKILL.md`、`docs/readme/workflows.md`、`cli.md`、`design-principles.md`、
  `topics.md`、`docs/agent/` 与 selftest；`VERSION=0.5.0`，`selftest.py` 258 项。

## 2026-11-18：共享 raw 的手改属于整份正文

复核 `docs/issues/2026-11-17-P2-shared-raw-hand-edit.md`：同一份正文被多个路径共享时，
手改曾转 `attach_raw` 另存一份，文件名跟着摘要跳，原路径消失。

- **归属单位是一个 raw 行。** `raws.id` 是正文摘要，一份正文一行；手改视为这份正文变更，
  挂载它的所有 `sources` 一起迁到新摘要，不是整个 wiki，也不回写源文件。
- **文件留在原名原路径。** 采纳只换账本行：删旧行、按原 `name`/`path` 插入新行，文件字节
  不动；重复手改不再在 `a.md` 与 `a-2.md` 之间来回跳。
- **同摘要已存在时整份并过去。** 新正文在其他 raw 行已落过一份，所有挂载指向那一行，旧行
  留给 `collect_raws` 回收。
- **任务快照一起对齐。** 采纳后对每个挂载调用 `pin_open_task_raw`，排队或租用任务的
  `raw`、`raw_digest`、`raw_size`、`raw_mtime` 跟着换，不会被自己的任务判成漂移。
- 同步 `docs/agent/engine.md`、`docs/readme/cli.md`、`recovery.md`、`design-principles.md`；
  `tests/selftest.py` 补共享手改三条与并库一条，共 262 项。

## 2026-11-18：重写页面时复核旧证据

`submit` 只校验本次载荷带来的引文；旧页面没被重写的 evidence 整条保留，之前不再复核。
来源改写后，失效引文会跟着新修订一起留下，重算 `source_digests` 后 `stale` 也会消失。

- 保留旧 evidence 时逐条对照当前 raw；原句不在就报 `E_EVIDENCE_NOT_FOUND`，要求本次
  载荷为该来源补一条新引文。
- 来源已不可用（没有 raw）时无从比对，原样保留；这类页面仍由 stale 与 doctor L7 兜底。
- 同步 `docs/readme/cli.md` 与 `docs/agent/engine.md`；自检新增 3 条，265 项全过。

## 2026-11-18：工作流目录统一为 `workflows/`

项目工作流目录 `workflow/` 和观看区 `workflows/`、内置 `skills/llm-wiki/workflows/` 的复数
写法打架，同一个词在根目录与安装目录两种拼法。用户确认保留新目录名，并明确项目里只剩
workflow 一个概念、场景相关的一切都移除。

- **唯一目录 `workflows/`。** `WORKFLOW_DIR = "workflows"`、`WORKFLOW_DIRS = (WORKFLOW_DIR,)`；
  删除 `LEGACY_DIRS`、`SCENARIO.md` 回退与 `publish.scenarios` 迁移，`ingest` 跳过与观看区
  投放都读常量，不再各写一份目录名。只有 `SCENARIO.md` 的目录判 `missing WORKFLOW.md`。
- **命令名不变。** 改的只是磁盘目录，`workflow list/search/load`、工作流 id 与 `path`
  字段照旧，`path` 前缀变为 `workflows/<id>/`。
- **不提供迁移脚本。** 旧工作区把 `workflow/`、`scenario/`、`场景能力/` 删掉或改名为
  `workflows/` 即可；同时存在时只读 `workflows/`，不再回退。

用户已明确「是我有意改名，保留 workflows/」「已经没有场景这个概念了」，文档与测试按此同步。

## 2026-11-18：`wiki:` 前缀作为固定入口

用户要一句话就固定进 wiki，而不是每次靠 agent 猜意图。两条实测否掉了别的做法：整句丢给
`workflow search` 不成立（`match()` 按词做子串计数，中文长句 0 命中）；技能描述里没有
`wiki:` 字样，前缀本身也没有被写进任何规则，只能靠 agent 自己联想。

- **前缀即显式 load。** 只认消息开头（接受全角冒号、前导空白、大小写，正文出现不算，单独的
  `wiki` 不算）；去掉前缀后按 `SKILL.md` 的路由表落位：交付物走 `ppt_creator` 等工作流，
  问/找/梳理走内置 `topic-exploration`，建库整理走 `ingest`。不改引擎，不加 `triggers:` 字段。
- **与「不做强制注入」不冲突。** 2026-11-16 定的是不做自动注入、只有显式 `scenario load`
  才生效；前缀是用户亲手写的，等于用户替 agent 做了这次显式 load。
- **两段式停点。** 需求里写明「我确定范围后 / 确认后 / 再…」时，先只做到材料范围确认，用户
  挑完再进交付物工作流。为此 `ppt_creator` 把取证提到补问之前：第 2 步粗查，第 3 步先摆候选
  范围与缺口，再问听众、时长等缺失项。
- **仍是软触发。** 靠描述被认出，不是硬保证；要硬保证得做引擎级 `triggers:` 字段，代价是改
  契约与测试，当前不做。

## 2026-11-19：`query` 带工作流候选，`workflow:` 前缀独占

用户要求把「强制通道」做成类似 `wiki:` 的输入前缀，同时明确普通 `query` 也要保留工作流候选：
不强制时全靠 agent 联想，触发仍然太难。

- **两条入口。** `workflow:` 只认消息开头，去掉前缀后只走 `workflow search`/`load`，不调
  `query`、不查页面和话题；0 命中或候选都不符合 `applies` 时直接回“无可用workflow”。普通
  `query` 在页面、话题之外固定带最多 5 个工作流候选（`workflows`、`workflow_matches`），每条带
  `applies` 和 `load`。
- **候选不等于注入。** 候选只作提示，agent 必须核对 `applies` 成立且用户要按流程办事才
  `load`；不自动执行，也不替用户决定范围。2026-11-16 的「不做自动注入」继续成立。
- **匹配权重。** tags 6、标题/别名 4、description 2、正文 1；正文保留 1 分，让“见证开户”这类
  只在步骤正文出现的词仍可检索。上限 5，排序按分数、同分按 id。
- **`query --task` 不带候选。** 它面向任务内页面检索，加候选会污染 route brief 和查询预算；
  只有普通 `query <关键词>` 带。
- 该决定取代 2026-11-16「场景改为显式检索，不随 query 注入」中「`query` 只返回页面与
  `topics`」的部分；「不随 query 注入」仍成立。前缀仍是软触发，不做引擎级 `triggers:`。

## 2026-11-18：话题目录改为英文 `topics/`

用户原话："话题探索这个工作里你看下，怎么还是用的中文路径"。

- 根目录 `话题/` 改名 `topics/`：同级的 `workflows/`、观看区的 `topics/` 都已是英文，只有
  话题目录还是中文，路径层级不一致；中文目录在 Windows 打包、压缩与部分工具链里还要额外
  处理编码。
- 只改目录名，话题文件名仍用中文主题词；`ingest` 仍整目录跳过，`query` 的 `topics` 字段、
  六节格式、内置工作流都不变。
- `topics.TOPIC_DIR` 是唯一事实源：`ledger.py` 的扫描跳过与 `publish.dir` 保留名校验都从它
  取值；`view.py` 的投放目标本来就是 `<view>/topics/`，不用改。
- 不做旧目录兼容：该功能上线两天、各工作区都没有存量 `话题/` 文件，直接改名比长期维护
  双目录读取简单。
