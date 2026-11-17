# raw 进入观看区后，`read` 命令是否还需要存在

## 问题

用户提出：既然 raw 副本已经放到观看区，`read` 命令是否就不需要存在了。需要判断的是它
还有没有别处无法替代的职责。

raw 的位置已经与观看区绑定，不再是引擎内部文件：

- `skills/llm-wiki/scripts/wiki.py:597-599`：`self.raw_dir = self.publish_raw_dir`，注释
  「Raw text is engine data: the publish switch never moves it」；即使关掉发布，raw 仍在
  `<view>/raw/`。
- 文件名由 `key2file` 生成（`skills/llm-wiki/scripts/wiki.py:481-486`）：
  `basename` 清洗后接 `-` 与 key 的 sha 前 8 位，写成 `<view>/raw/<name>-<sha8>.md`，
  内容带引擎前言（`write_raw`，`skills/llm-wiki/scripts/wiki.py:791-795`）。

`read` 目前覆盖三种读法（`command_read`，`skills/llm-wiki/scripts/wiki.py:2138-2179`）：

1. `read "raw:<key>"`：用账本把 source key 映射成 raw 路径再读（`resolve_ref`，
   `skills/llm-wiki/scripts/wiki.py:2182-2198`）；
2. `read "page:<id>"`：直接读账本里的元数据与正文；
3. `read <root 内路径>`：通用文件读取，`.md` 开头是 `---` 时拆出 frontmatter，正文只留
   `content`，并按 `--offset/--limit` 切窗口。

对照现有输出，三条读法各自的独占性都不强：

- 页面正文：`query --page` 已返回 `page`（元数据）、`rev`、`path`、`outgoing`、`incoming`
  与 `body` 全文，是 `read "page:<id>"` 的超集（`docs/readme/cli.md:173-176`）。
- raw 路径：任务包已带 `work.sources[].raw`（`source_view`/`task_work`，
  `skills/llm-wiki/scripts/wiki.py:1392-1411`），但 route brief 仍教 agent 运行
  `read "raw:<key>"`（`skills/llm-wiki/scripts/tasks.py:79`），page brief 也写「先读
  work.sources 里每份 raw 原文」（`skills/llm-wiki/scripts/tasks.py:131`）。
- 任意路径：agent 的通用读工具能读工作区内文件，`--offset/--limit` 也覆盖；差别只在于
  `read` 会拆 frontmatter、把前言从 `content` 里去掉。

`read` 被下面这些地方当作契约引用，删改是跨文档、跨测试的改动：

- `skills/llm-wiki/SKILL.md:100-101`（示例）与 `:106`（「单点查页面、查字段……直接用
  `query`/`read`」）。
- `docs/readme/cli.md:161-169`（`read` 的完整契约与 frontmatter 规则）、`:295`（错误码
  表把 `read` 列为定位手段）。
- `docs/agent/verification.md:123`、`:128`、`:136`（`read` 的区外、raw、page 三条用例）。
- topics 命中项带可直接执行的 `read "<path>"` 字段（`skills/llm-wiki/scripts/topics.py:67`，
  文档见 `docs/readme/topics.md:88`）。
- 内置场景 `skills/llm-wiki/scenarios/topic-exploration/SCENARIO.md:18`、`:28`、`:32`
  （`read "话题/<name>.md"`、`read "raw:<source-key>"`）。
- `tests/selftest.py` 十余处：`378-391`（raw 正文与前言剥离）、`501`、`627`、`667`、
  `682`、`697`、`879-890`、`923`、`949`、`960`、`1013`、`1036`。

另外，`query` 的文字检索结果项只有 `id/title/type/status/summary/sources/rev/score/snippet`
（`skills/llm-wiki/scripts/search.py:28-46`），没有文件路径；拿到页面 id 后要再 `query --page`
才有 `path`。也就是说，删掉 `read` 后，「知道 source key 但不知道 raw 文件名」的缺口需要
由别处补上。

## 影响

删得掉的部分：`read "raw:<key>"` 与 `read "page:<id>"` 在常规工作区里都能被
「任务包给的 raw 路径 + 通用读工具」和「`query --page`」替代；`read <path>` 与通用读工具
重叠，只是多了 frontmatter 拆分。

删不掉的风险：

- source key → raw 文件名的映射只存在于 `key2file` 与账本里。sha8 是隐藏契约，让 agent 自己
  拼文件名很脆；`work.sources[].raw` 之外没有稳定通道（`query` 与 `query --page` 都不返回
  raw 路径）。
- 发布关闭时 `page_ref` 退化为 `page:<id>`（`skills/llm-wiki/scripts/wiki.py:898-901`），
  页面对应文件不在观看区；`query --page` 仍能从账本返回正文，所以这一条只削弱 `read`
  的必要性，不构成保留理由，但要在删前确认「只用 query 也能读页」。
- raw 为空的来源（oversize/unsupported）没有 raw 文件；`read "raw:<key>"` 会返回带
  `next` 的 `E_NOT_FOUND`（`skills/llm-wiki/scripts/wiki.py:2186-2190`）。直接读文件只会
  得到「文件不存在」，少了「这个来源本来就没 raw」的区分。
- 通用读工具会显示 `---` 前言，agent 需要自己判断正文边界；引文核对本身不受影响。
- `read` 的 `E_PATH` 越界用例是工作区边界的守卫之一（`docs/agent/engine.md:76`），
  删除后边界测试要换到别的入口。

## 候选方案

未定案。可选：

1. **直接删除 `read`**：把路径发现并进查询——`query --page` 与文字检索结果补 raw 路径
   （或新增轻量 `query --source <key>`），topics 的 `read` 字段改为 `path`；SKILL、SCENARIO
   改成「按返回路径直接读文件」。最彻底，命令数从 9 降到 8，但迁移面最大。
2. **收缩为两件事**：`read` 只保留 `raw:<key>` 与 `page:<id>`，删掉任意路径读与 frontmatter
   拆分，内容交给通用工具。迁移面小，但仍是与通用读工具并行的第二个入口。
3. **改名 `path`**：只输出路径（`raw:<key>`、`page:<id>`、根内路径），不输出内容。与
   「raw 已在观看区、agent 自己读文件」最契合，但要改全部调用点与文档。
4. **维持现状**：认为 key 映射、发布关闭读账本、raw 为空提示必须由引擎统一，就保留 `read`，
   但把 brief、SKILL、SCENARIO 里重复教「用 read 读文件」的文案收敛为「路径由任务包/查询
   给出，读文件用通用工具」。

## 待确认与验收条件

- [ ] 明确 raw 路径的稳定发现通道：除 `work.sources[].raw` 外，`query`/`query --page` 是否
      需要返回 raw 路径；发布关闭时是否还需要从命令读页面。
- [ ] 定案：删 / 收缩 / 改名 / 维持。
- [ ] 若删：同步 `skills/llm-wiki/SKILL.md`、`docs/readme/cli.md`、`docs/readme/topics.md`、
      `docs/agent/verification.md`、`scenarios/topic-exploration/SCENARIO.md`、
      `skills/llm-wiki/scripts/tasks.py` 的 brief、`skills/llm-wiki/scripts/topics.py` 的
      `read` 字段与 `tests/selftest.py`；错误码表里 `read` 的定位手段说明一并改。
- [ ] 无论保留与否，selftest 覆盖「发布关闭时读页面」与「raw 为空的来源」两条边界。
- [ ] 改动后同一轮检索核实新内容，并确认没有残留 `read "raw:` / `read "page:` 的教法。
