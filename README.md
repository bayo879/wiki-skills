# llm-wiki

把本地文档整理成可检索、可追溯的 Markdown 知识库。交付物是一个 agent 技能目录
`skills/llm-wiki/`：纯 Python 标准库，没有服务端、没有构建步骤、没有二进制的分发包。
项目还可以把一类任务的执行流程写成**场景能力**，放在根目录 `场景能力/` 下；检索命中后按
`SCENARIO.md` 的流程执行，不需要摄入成知识页。回答知识问题时默认走内置的「话题探索」场景：
定主题、多轮检索、总结，收尾时询问是否把主题沉淀成根目录 `话题/<主题>.md`。

设计前提：**确定性的事交给引擎，语义的事交给 agent。** 扫描、解析、哈希、任务状态、
引文校验、页面渲染都在 `scripts/wiki.py` 里；agent 只领任务、读原文、交 JSON 载荷。
知识库状态只存在 `.wiki/wiki.db` 账本中，`.wiki/pages/*.md` 只是按账本渲染的镜像；
场景文件是只读能力，不写账本。

## 快速开始

要求 Python 3.12+；PDF 可选 `pdftotext`（Poppler）或 `pypdf`。

```bash
# 安装到 agent 会扫描的技能目录（也可直接按路径加载 SKILL.md）
python3 skills/llm-wiki/scripts/install.py --target auto --json

# 在资料所在的工作区根目录先自检，通过后再初始化和摄入（不要 cd 到工作区外）
python3 <skill>/scripts/wiki.py check --json
python3 <skill>/scripts/wiki.py init --json
python3 <skill>/scripts/wiki.py ingest --json

# 任务循环：next 返回 brief 与 template，填好 template 后 submit
python3 <skill>/scripts/wiki.py next --json
python3 <skill>/scripts/wiki.py submit <t_id> --payload .wiki/payloads/<t_id>.json --json

# 场景：query 命中后加载流程，按步骤执行（不摄入、不建库）
python3 <skill>/scripts/wiki.py query "开户" --json
python3 <skill>/scripts/wiki.py scenario load "<场景>" --json

# 默认话题探索：先加载内置流程，再检索、总结，最后按需沉淀
python3 <skill>/scripts/wiki.py scenario load "话题探索" --json
```

Windows 首次使用运行
`powershell -NoProfile -ExecutionPolicy Bypass -File skills\llm-wiki\scripts\bootstrap.ps1`；
其余命令里的 `python3` 用 `py -3.12` 或指向 3.12 的 `python` 替代。

完整命令与字段见 [CLI 契约](docs/readme/cli.md)。

## 工作方式

1. `ingest` 扫描根目录（跳过隐藏目录与 `scan.ignore`），把新文件或内容变化的文件解析到
   `.wiki/raw/`，每个来源入队一条 **route** 任务。
2. agent 对每个来源提交决定：`new`（新建页）、`merge`（并入已有页）或 `skip`（不要）。
   `new` 与 `merge` 会生成 **page** 任务。
3. agent 读 raw 与已有页，写正文、摘要、证据引文与边界，提交 page 任务。引文去掉空白后
   必须逐字命中来源文本，否则拒绝写入。
4. 页面以修订形式写入账本，并渲染到 `.wiki/pages/<type>/<id>.md`。来源变化会让引用页
   变 `stale`，下次 `ingest` 重新排队。
5. 场景能力放在 `场景能力/<场景>/`，由 `SCENARIO.md`、`scripts/`、`references/` 组成。
   `ingest` 跳过整个目录；`query` 直接扫描文件，命中时在 `scenarios` 返回，按
   `scenario load` 得到的流程执行。改场景文件立即生效，不用重新摄入。技能自带
   `话题探索` 内置场景，项目放同名场景即可覆盖；`query` 的 `topic_scenario` 恒指向它。
6. 话题沉淀放在 `话题/<主题>.md`，一个主题一份，固定六节（主题、结论、依据、未决问题、
   下一步、能力候选）。`ingest` 跳过 `话题/`；`query` 把命中话题放在 `topics`，与页面
   计数分开。话题由 agent 直接写，不进账本、不排任务。

任务由引擎排队和回收：`next` 一次给一个，返回 `brief`（做什么、怎么做、提交后的影响）与
已固定结构的 `template`。agent 只填内容，不接触租约、重试或状态迁移；提交失败时任务回到
队列，按返回的 `error.next` 修正后重交。

## 边界

- **不 OCR、不下载依赖、不访问网络。** 扫描件与图片进入 `unsupported`，由人提供可复制
  文本后再摄入。
- **不越出工作区。** 所有命令以会话工作区为根目录；`--root` 只能是当前目录或它下面的
  子目录，草稿、载荷、导出与临时文件都放 `.wiki/` 下。资料在区外时请用户切换工作区，
  不跨区读写。
- **页面只能通过命令修改。** 直接改 `.wiki/pages/` 会在下次命令启动时被账本覆盖，原文
  存进 `.wiki/drift/` 留证。人工修改走 `page import`，回滚走 `page restore --rev N`。
- **场景不是知识页。** `场景能力/` 不摄入、不进账本、不生成任务；流程以
  `SCENARIO.md` 为准，事实依据仍以页面和 raw 为准。场景命令只读，不要求先 `init`。
- **话题不是知识页。** `话题/` 由 agent 按内置「话题探索」场景的格式直接写入，不运行
  page 任务、不写 `.wiki/`、不排任务；写入不代表已摄入，也不参与页面证据校验。
- **超限文件不解析。** 默认单文件 50 MB（`ingest.max_source_bytes`），按 `oversize` 报告，
  不中止其他文件。
- 单机、单写者。SQLite 事务保证单进程一致性；多 agent 并发写不在承诺范围内。

## 仓库结构

```text
skills/llm-wiki/          技能：SKILL.md、references/（curation.md）、
                          scenarios/（内置话题探索）、templates/（concept、entity、
                          process、note）、scripts/
tests/                    开发/CI 的端到端自测，不随技能安装
docs/readme/              产品说明：设计原则、CLI、场景、话题、安装、恢复
docs/agent/               开发指导：决策、引擎内部、维护守则
docs/issues/              未决问题，一问题一文件
```

Rust 实现已于 2026-11-13 停止维护，归档在 Git tag `rust-archive-2026-11-13`，不在当前
分支保留源码。历史设计与坑点中仍适用的部分已并入本文与 `docs/agent/`。

## 开发

```bash
python3 -m py_compile skills/llm-wiki/scripts/*.py
python3 tests/selftest.py
```

`tests/selftest.py` 在临时目录跑 57 项端到端检查，不接触真实数据；它和 `tests/` 留在
仓库、由开发/CI 执行，不复制进安装后的技能目录。验证记录（含真实 PDF 语料结果）见
[docs/agent/verification.md](docs/agent/verification.md)。场景目录结构与写法见
[docs/readme/scenarios.md](docs/readme/scenarios.md)，话题沉淀格式见
[docs/readme/topics.md](docs/readme/topics.md)。
