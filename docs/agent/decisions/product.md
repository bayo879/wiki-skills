# 开发方向与决策：产品形态与边界

返回 [开发方向与决策](../development-decisions.md)。技能是什么形态、装在哪、以什么为根、入口怎么叫。
条目保持当初的写法，按时间顺序排列。

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

## 2026-11-17：技能包不携带 `.cmd`/`.bat`

- 下游分发禁止 `.cmd` 后缀，`skills/llm-wiki/scripts/bootstrap.cmd` 删除，Windows 入口只保留
  `bootstrap.ps1`。cmd 与 Windows Terminal 里执行文档给出的
  `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\bootstrap.ps1`，参数原样追加，
  退出码由 PowerShell 透传；双击启动不再作为交付要求。
- `tests/selftest.py` 增加守卫：`skills/` 下出现 `.cmd`/`.bat` 即失败。

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
