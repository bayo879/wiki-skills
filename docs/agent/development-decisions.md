# 开发方向与决策

本文件记录用户已确认的方向与关键取舍。新决策由用户拍板后追加；未确认的候选方案写进
`docs/issues/`，不要写在这里。

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

- agent 不能直接写页面：写入必须经 `submit` 校验，或 `page import` 人工接管。
- `.wiki/pages/*.md` 作为自动镜像保留（用户："可以拿保留自动镜像吧"），用于浏览、diff、
  迁移；被外部修改时先存 `.wiki/drift/` 再按账本还原。
- `query`/`read page` 只读账本，不读镜像文件。

## 2026-11-13：任务协议

- 对外只暴露"任务 + 语义载荷 + 证据"，内部表结构、租约、epoch、哈希不写进技能文档。
- 任务只有 `route` 与 `page` 两种；一个来源一条 route 任务，一页一条 page 任务。
- todo 只作为显示层投影，账本是唯一进度权威；没有 todo 工具的运行时不受影响。
- 默认每批 20 条；租约 15 分钟；提交失败零写入并保留 `last_error`。
- 已取消 `reconcile` 任务类型：镜像漂移自动留证并还原，页面回滚用 `page restore`。

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
- 引擎强制：`--root` 只能是当前目录或其子目录；`read` 路径、`--payload`、`body_file`、
  `page import --from`、`export` 目标越出根目录返回 `E_PATH`。
- 草稿、载荷、导出、临时文件统一放 `.wiki/`；`--dry-run` 临时库从系统 temp 改到
  `.wiki/tmp/`，退出时清理。
- `selftest.py` 增加 6 项越界用例，总计 26 项。

## 2026-11-15：技能信息面与任务包

用户原话："关于skill信息切面的问题，wiki能力内部处理的部分，完全不要暴露到skill可读范围，
使用代码解决掉，需要agent协助的流程，在skill中展示清楚。这是这个技能开发最基本的前提，
不信任agent的流程决策。"

- 技能只展示 agent 真正要做的流程：`init/ingest`、`next`、按 `brief` 填 `template`、
  `submit`、`query/read`、工作区边界。epoch、租约、重试、任务状态、错误码表、doctor、
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
  `triggers` 可选，参与检索；正文是流程。
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
  普通知识问答不会被场景分支改写范围或答案结构。
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
