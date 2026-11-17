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
- `read page:` / `query --page` 的 `path` 改为观看区路径；发布关闭时退回 `page:<id>`。
- 旧版残留的 `.wiki/pages/` 与 `.wiki/drift/` 不自动接管也不自动删除，由用户手动清理。
- 理由：镜像除 drift 留证、旧库接管、稳定 id 路径外没有查询/读取/导出/回收消费者，
  与观看区重复维护，反而让"账本是唯一事实源"的表述含混。

## 2026-11-13：任务协议

- 对外只暴露"任务 + 语义载荷 + 证据"，内部表结构、租约、epoch、哈希不写进技能文档。
- 任务只有 `route` 与 `page` 两种；一个来源一条 route 任务，一页一条 page 任务。
- todo 只作为显示层投影，账本是唯一进度权威；没有 todo 工具的运行时不受影响。
- 默认每批 20 条；租约 15 分钟；提交失败零写入并保留 `last_error`。
- 已取消 `reconcile` 任务类型：页面回滚用 `page restore`（当时的镜像漂移自动还原机制已于
  2026-11-17 随镜像一并删除）。

## 2026-11-17：一个来源可以拆成多页

用户确认按“引擎 + 示例 + 文档”一起改，触发是一篇讲话同时讲“主题日活动”与“双零”行动，
旧协议只能合并成一页。

- route 决定从“每个来源恰好一条”改为“每个来源至少一条”：一份来源含多个可独立检索的
  主题时，提交多条 decision 指向不同 page；同一来源不得重复指向同一 page，skip 不能与
  new/merge 并用。
- 同一次 route 提交拆出的 page 任务共享 `route_task`，是“一个来源只挂一棵 page 树”的
  唯一例外；来源在最后一个相关 page 任务关闭后才转为 `done`。
- `curation.md` 的示例同步拆成两页并互链，避免示例继续示范“一份文件一页”。

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

## 2026-11-17：`next` 重新发放未提交的任务

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
- 同步 `README.md`、`docs/readme/install.md`、`skills/llm-wiki/SKILL.md`、
  `skills/llm-wiki/references/recovery.md` 与 issue「Windows 引导脚本尚未实机验收」的场景表。
- `VERSION=0.4.3`；`selftest.py` 仍 208 项（PowerShell 脚本不进自测，Windows 实机验收另算）。
