# 开发方向与决策：工作流与话题

返回 [开发方向与决策](../development-decisions.md)。工作流与话题的目录、定义来源与检索方式。
条目保持当初的写法，按时间顺序排列。

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
- 本仓库根 `workflows/` 是示例/开发用定义，和 `skills/` 一样是可选产物，不是观看区定义；
  旧根兜底仍会把它们列为 `legacy` 项，迁移到观看区后应删除。本仓库不迁移、不改
  `.gitignore`、不在根跑写状态命令。

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
- `topics.TOPIC_DIR` 是唯一事实源：`ledger_sources.py` 的扫描跳过与 `publish.dir` 保留名校验都从它
  取值；`view.py` 的投放目标本来就是 `<view>/topics/`，不用改。
- 不做旧目录兼容：该功能上线两天、各工作区都没有存量 `话题/` 文件，直接改名比长期维护
  双目录读取简单。
