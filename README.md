# llm-wiki

把本地文档整理成可检索、可追溯的 Markdown 知识库。交付物是一个 agent 技能目录
`skills/llm-wiki/`：纯 Python 标准库，没有服务端、没有构建步骤、没有二进制的分发包。
项目还可以把一类任务的执行流程写成**可执行工作流**，放在观看区 `workflows/`
（`<view>/workflows/`）下；`WORKFLOW.md` 的每一步标明是调工具、人做、agent 判断还是验证，
检索命中后照着执行，不需要摄入成知识页。回答知识问题时默认走内置的「话题探索」
工作流：定主题、多轮检索、总结，收尾时询问是否把主题沉淀成观看区
`topics/<主题>.md`（`<view>/topics/`）。

设计前提：**确定性的事交给引擎，语义的事交给 agent。** 扫描、解析、哈希、任务状态、
引文校验、页面渲染都在 `scripts/` 里（入口 `scripts/wiki.py`）；agent 只领任务、读原文、
交 JSON 载荷。
知识库状态只存在 `.wiki/wiki.db` 账本中；引擎按账本把页面推送到 `<view>/` 观看区
（`publish.dir`，默认 `wiki/`），`.wiki/` 里不再保存第二份页面正文。工作流文件是只读能力，
不写账本。

## 快速开始

要求 Python 3.12+；PDF 可选 `pdftotext`（Poppler）或 `pypdf`。

```bash
# 安装到 agent 会扫描的技能目录（也可直接按路径加载 SKILL.md）
python3 skills/llm-wiki/scripts/install.py --target auto --json

# 在资料所在的工作区根目录摄入（不要 cd 到工作区外；首次运行会自己建 .wiki/）
python3 <skill>/scripts/wiki.py ingest --json

# 任务循环：next 把任务写进 .wiki/tasks/<id>/，填好其中的 payload.json 后 submit
python3 <skill>/scripts/wiki.py next --json
python3 <skill>/scripts/wiki.py submit <t_id> --json

# 工作流：显式检索并加载流程，按类型步骤执行（不摄入、不建库）
python3 <skill>/scripts/wiki.py workflow search "开户" --json
python3 <skill>/scripts/wiki.py workflow load "<工作流>" --json

# 默认话题探索：先加载内置流程，再检索、总结，最后按需沉淀
python3 <skill>/scripts/wiki.py workflow load "topic-exploration" --json
```

Windows 首次使用运行
`powershell -NoProfile -ExecutionPolicy Bypass -File skills\llm-wiki\scripts\bootstrap.ps1`；
它只查找已有的 Python 3.12+，不下载也不安装，找不到时用 `-Python <python.exe>` 指定运行时。
其余命令里的 `python3` 用该解释器替代。

在 agent 里也可以用 `wiki:` 开头直接下任务，例如
`wiki: 找一些跟云网主题日相关的资料，我确定范围后，帮我生成ppt汇报材料`。前缀表示这次明确
走 wiki，后面的内容决定走哪条路（问知识、建库、还是加载 PPT 等工作流），规则见
[SKILL.md](skills/llm-wiki/SKILL.md)。

完整命令与字段见 [CLI 契约](docs/readme/cli.md)。

## 工作方式

1. `ingest` 扫描根目录（跳过隐藏目录、`scan.ignore`、工作流/话题目录与观看区），把新文件或
   内容变化的文件按正文去重后解析到 `<view>/raw/`（默认 `wiki/raw/`），每个 raw 入队一条
   **route** 任务，同一份正文的多个路径合成一条。
2. agent 对每份正文提交决定：`new`（新建页）、`merge`（并入已有页）或 `skip`（不要），
   `source` 填它的任一挂载路径；一份正文可拆成多条决定、指向不同页。`new` 与 `merge` 会
   生成 **page** 任务。
3. agent 读 raw 与已有页，写正文、摘要、证据引文与边界，提交 page 任务。引文去掉空白后
   必须逐字命中来源文本，否则拒绝写入。
4. 页面以修订形式写入账本，并由引擎推送到 `<view>/pages/<type>/<标题>.md`（观看区）；
   正文只存在账本与观看区两处。来源变化会让引用页变 `stale`，下次 `ingest` 重新排队。
5. 可执行工作流放在观看区 `workflows/<工作流>/`，由 `WORKFLOW.md`、`scripts/`、
   `references/` 组成。`ingest` 跳过整个观看区；普通 `query` 会带回最多 5 个工作流候选
   （每条带 `applies`），但不自动加载；需要时显式 `workflow search`，核对适用条件后按
   `workflow load` 返回的分类步骤执行。`workflow:` 前缀是独占入口，只搜工作流，不查页面和
   话题。改工作流文件立即生效，不用重新摄入。技能自带 `topic-exploration`、`workflow-review`、
   `workflow-create` 内置工作流，项目放同名工作流即可覆盖，显式检索或 `query` 候选里同样能被
   检出。旧根 `<root>/workflows/` 只作兼容兜底，观看区优先。
6. 话题沉淀放在观看区 `topics/<主题>.md`，一个主题一份，固定六节（主题、结论、依据、未决
   问题、下一步、能力候选）。`ingest` 跳过 `topics/`；`query` 把命中话题放在 `topics`，与
   页面计数分开。话题由 agent 直接写，不进账本、不排任务；旧根 `<root>/topics/` 只作兼容
   兜底。

任务由引擎排队和回收：`next` 一次给一个，返回 `brief`（做什么、怎么做、提交后的影响）与
已固定结构的 `template`。agent 只填内容，不接触租约、重试或状态迁移；提交失败时任务回到
队列，按返回的 `error.next` 修正后重交。

## 边界

- **不 OCR、不下载依赖、不访问网络。** 扫描件与图片进入 `unsupported`，由人提供可复制
  文本后再摄入。
- **不越出工作区。** 所有命令以会话工作区为根目录；`--root` 只能是当前目录或它下面的
  子目录，草稿、载荷、导出与临时文件都放 `.wiki/` 下。资料在区外时请用户切换工作区，
  不跨区读写。
- **页面只能通过命令修改。** `<view>/pages` 里的手改不会被静默覆盖，引擎会告警，由人工
  `sync` 决定是否收进账本。人工修改走 `page import`，补关联走 `page link <id> <target...>`，
  回滚走 `page restore --rev N`；批量按共享来源补链用 `page link --repair --dry-run` 先预览。
- **观看区的所有权分开算。** `<view>/pages` 的手改不会被覆盖，只给告警，由人工 `sync`
  收进账本；`<view>/raw` 由摄入维护（手改会在下一次 `ingest` 被采纳为事实），
  `<view>/README.md` 每次推送重写；`<view>/workflows/`、`<view>/topics/`、
  `<view>/runs/` 属于项目内容，引擎不覆盖也不删除。详见[观看区与发布](#观看区与发布)。
- **工作流不是知识页。** `<view>/workflows/` 不摄入、不进账本、不生成任务；流程以
  `WORKFLOW.md` 为准，事实依据仍以页面和 raw 为准。工作流命令只读，不要求先 `init`。
- **话题是 wiki 的探索层，不是知识页。** `<view>/topics/` 由 agent 按内置「话题探索」工作流
  的格式直接写入，不运行 page 任务、不写 `.wiki/`、不排任务；写入不代表已摄入，也不参与
  页面证据校验。
- **超限文件不解析。** 默认单文件 50 MB（`ingest.max_source_bytes`），按 `oversize` 报告，
  不中止其他文件。
- 单机、单写者。SQLite 事务保证单进程一致性；多 agent 并发写不在承诺范围内。

## 观看区与发布

引擎在账本之外维护一份给人看的产出 `<view>/`，默认是工作区根目录的 `wiki/`：

- `pages/`：知识页，按 `<type>/<标题>.md` 渲染。手改过的页面不会被覆盖，只给告警，需要
  运行 `sync` 收进账本。
- `raw/`：原文唯一副本，由 `ingest` 维护；手改会被下一次 `ingest` 采纳为账本事实。改坏后
  删掉该文件再 `ingest`，会用源文件重建（源文件也没了才会丢）。源文件内容改变且比 raw 新时
  `ingest` 会主动重写；只改 mtime（`touch`）不会触发。
- `README.md`：目录说明与统计，每次推送重写。
- `workflows/`：项目工作流定义，一个子目录一个工作流；属于知识库内容。
- `topics/`：项目话题，一个主题一份；属于知识库内容。
- `runs/`：工作流任务产物，按 `runs/workflows/<id>/<task>/` 分目录；属于项目内容。
- `workflows/`、`topics/`、`runs/` 引擎不覆盖、不删除；其他文件（包括 `.obsidian/`）也不删
  不改。

手改尚未被 `sync` 收割期间，这一页在观看区整体冻结：推送既不覆盖它，也不按新标题改名或
移动路径，所以 `page import` 改标题后要等回收完成才会出现新文件名。推送的告警（手改、路径
被占等）只写 stderr，不进 JSON 的 `data`；只有 `sync` 会在 `data.warnings` 里给出同类信息。

配置在 `.wiki/wiki.json` 的 `publish` 段，两个键：`dir`（默认 `wiki`）、`enabled`（默认
`true`）。观看区与 `raw/`、`workflows/`、`topics/` 始终创建；`pages/` 在推送时创建，`runs/` 在第一次产物时创建；
`enabled:false` 只停推送，不写 `README.md`、不更新 `pages/`。

`dir` 必须是单个目录名：不能以点开头、不能含路径分隔符，也不能是 `.wiki`、`topics`、
`workflows`；名字非法时即使 `enabled:false` 也报 `E_CONFIG`。这个名字被引擎
占用，即使关了发布也仍跳过，所以不能拿它当来源目录。`enabled:false` 只停推送：不写
`README.md`，不再更新 `pages/`，但 `<view>/` 与 `raw/`、`workflows/`、`topics/` 仍会创建。
换 `dir` 等于换观看区，
旧目录名不再跳过，`ingest` 会把它当普通来源摄入；定义、话题和产物不会自动搬迁，需要先手动
移动。整目录删除会连 `raw/` 里的原文、`workflows/`、`topics/` 与 `runs/` 里的项目内容一起
丢掉；`raw/` 要重新 `ingest` 才能补回，`pages/` 与 `README.md` 会按账本重建。

```bash
python3 <skill>/scripts/wiki.py sync --json              # 回收 <view>/pages 的手改
python3 <skill>/scripts/wiki.py sync --body-only --json  # 只取正文与证据，保留账本元数据
```

`sync` 只回收、不推送；下一条会启动引擎的命令启动时才把账本推回观看区。字段与失败码见
[CLI 契约的 sync 段](docs/readme/cli.md#sync)。

## 仓库结构

```text
skills/llm-wiki/          技能：SKILL.md、references/（curation.md）、
                          workflows/（内置话题探索）、templates/（concept、entity、
                          process）、scripts/
tests/                    开发/CI 的端到端自测，不随技能安装
docs/readme/              产品说明：设计原则、CLI、工作流、话题、安装、恢复
docs/agent/               开发指导：决策、引擎内部、维护守则
docs/issues/              未决问题，一问题一文件
workflows/                示例/开发用工作流定义；不是运行时加载入口，观看区优先
wiki/                     观看区：运行时在工作区根目录创建（publish.dir 默认 wiki/），
                          pages/ raw/ workflows/ topics/ runs/ README.md
```

Rust 实现已于 2026-11-13 停止维护，归档在 Git tag `rust-archive-2026-11-13`，不在当前
分支保留源码。历史设计与坑点中仍适用的部分已并入本文与 `docs/agent/`。

## 开发

```bash
python3 -m py_compile skills/llm-wiki/scripts/*.py
python3 tests/selftest.py
```

`tests/selftest.py` 在临时目录跑 368 项端到端检查，不接触真实数据；它和 `tests/` 留在
仓库、由开发/CI 执行，不复制进安装后的技能目录。验证记录（含真实 PDF 语料结果）见
[docs/agent/verification.md](docs/agent/verification.md)。工作流目录、步骤契约与写法见
[docs/readme/workflows.md](docs/readme/workflows.md)，话题沉淀格式见
[docs/readme/topics.md](docs/readme/topics.md)。
