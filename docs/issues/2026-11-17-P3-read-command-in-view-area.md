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
才有 `path`。删掉 `read` 后，「知道 source key 但不知道 raw 文件名」的缺口需要由别处补上。

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

## 已确认方案（2026-11-17）

曾评估四条路径（直接删除 / 收缩 / 改名 `path` / 维持）。定案是**删除 `read`**，但前提是
先把 raw 从「源的附属文件」升为一等对象，让路径发现、引用与核对都不再依赖 `read`。

### 对象模型

- **raw 表**：一行一份内容，正文 sha256 作主键（唯一键）。记 `display_name`（展示名）、
  `parser`、`updated`。hash 不进文件名，只在账本里；物理文件名用展示名。
- **origins 表**：源相对路径 → raw 主键的当前映射，另记 `last_parsed_digest`（上次从该源
  解析出的正文摘要）并保留历史以便追溯。一个源在同一时刻只对应一个 raw；同一内容的多
  个源指向同一个 raw。（实现上没有单独建 origins 表：`sources` 本身就是按源相对路径建
  主键的表，映射直接是它的 `raw_id` 与 `last_parsed_digest` 两列。）
- **展示名**：首次摄入的首见名。挂载 ≥2 时保持首见名，不随改名跟随；挂载 =1 时跟随该
  路径的 basename；挂载 =0 时保留最后的名字并标 orphan。
- 对外引用一律写**源相对路径**：页面 `sources`、证据段、任务包与命令输出都不出现 hash，
  读者凭路径就能回溯原始文件。raw 文件名变化不影响任何对外引用。

### 行为规则

- **摄入幂等**：正文 sha256 命中已有 raw 时，只新增/更新 origin 映射，不产生第二份 raw、
  不重复排 route/page 任务；未命中才新建 raw。同一内容 100 个文件只落一份。
- **人工排版**：raw 允许人工修改。修改后正文变了即生成新 hash 行，origin 改指到新行，
  `last_parsed_digest` 保持旧值。下一轮 ingest 看到「源没变、当前 raw 主键 ≠
  `last_parsed_digest`」，就认定这是人工改过的版本，不回源覆盖；只有显式重新摄入才用源
  内容重建。冻结是这条比较推出来的结论，不作为单独字段存储。
- **源变动、路径不变**：源摘要与 `last_parsed_digest` 不同即重新解析；新内容生成新 hash
  行、origin 改指过去，`last_parsed_digest` 更新为新值。对外看是原地覆盖，历史映射可
  追溯；若该 raw 还被其他源指向，其他源不受影响。
- **源路径变化**：旧 origin 删、新 origin 建（一删一赠）；新路径命中已有 raw 则直接挂上，
  否则新建。
- **引用解析**：页面与证据里的源路径经 `sources.raw_id` 映射到 raw 主键，再用该 raw 的正文做
  引文核对（`skills/llm-wiki/scripts/validation.py:88-97` 的 `source_body` 改为按 raw 主键取）。
- **旧内容清理**：旧行的价值只在过渡期——新行建立、origin 改指之后，页面按源路径已经解析
  到新行，旧行不再有人引用，可以直接删掉，不另设历史正文目录。仍被其他未变源指向的旧
  行保留，直到最后一个 origin 离开。
- **过期与清理**：页面引用的源全部 missing → 该页是孤儿页，清理掉；部分 missing → 页面
  标 stale 待复核。

### `read` 的删除

raw 路径由 `next` 工作包直接给出（`work.sources[].raw`），页面由 `query --page` 给出，
任意文件由 agent 通用读工具读取；`read` 不再有独占职责，命令从 9 个降为 8 个。

## 当前进度（2026-11-17）

### 已落地、未验收

- **账本拆出**：`skills/llm-wiki/scripts/ledger.py` 承接 `SCHEMA_SQL`、`create_schema(conn)`、
  `ensure_columns(conn)`、`REQUIRED_COLUMNS`、`ensure_open_page_index(conn)`；`wiki.py` 不再
  自建表，改调 `ledger.*`。
- **raw 表实际形态**：`raws(id, name, path, parser, size, mtime, updated_at)`。`id` 是正文
  sha256，`name` 是展示名，`path` 是 raw 文件在观看区的相对路径，`size/mtime` 是文件事实。
  与上文「对象模型」的差别只在列名（`name` 而非 `display_name`），没有 origins 表。
- **映射列**：`sources` 增 `raw_id`、`last_parsed_digest`（`ledger.py` 的 `REQUIRED_COLUMNS`
  一并守卫）。
- **ingest 接入**：`attach_raw`（`skills/llm-wiki/scripts/wiki.py:730`）、`adopt_raw_edit`
  （`:799`）、`collect_raws`（`:848`）、`raw_mounts`（`:723`）、`_raw_name`（`:711`）；
  rewrite 分支走 `attach_raw`（`:1810`），bookkeep_raw 分支走 `adopt_raw_edit`（`:1763`），
  ingest 输出新增 `reclaimed`（`:1876`、`:1888`）。
- **修掉一个实测 bug**：一份 raw 被多个源挂载时，文件重写后只有当事源的 `raw_size/raw_mtime`
  被更新，其余挂载源下一轮会被判成「人工改过」并各自分叉出一份新 raw。现在 `attach_raw`
  写完文件就按 `raw_id` 回填全部挂载源的 raw 事实（`skills/llm-wiki/scripts/wiki.py:790-796`）。
- **用例**：`tests/selftest.py:1195-1215` 用独立工作区覆盖「两源同正文共享一份 raw」与
  「共享 raw 不被重复解析」两条。

### 没做完

- 「人工排版保留用户字节」「人工排版独立成行」「源改动后回收死行」三条断言当前不在
  `tests/selftest.py` 里（并行会话改写该文件时丢失），要补回来。
- 第二步尚未验收：`source_body(key)`（`skills/llm-wiki/scripts/wiki.py:903`）与
  `computed_status`（`:960`）仍按源 key 和 `sources.raw_digest` 比对，没改成「按 `raw_id`
  取正文」。
- 第 3–6 步未开始：任务链仍以源 key 为 subject、`read` 命令与 `resolve_ref`、`topics.py:67`
  的 `read` 字段、`tasks.py:79` 与 `:131` 的 `read` 教法都还在。
- 待定语义：一份 raw 被多个源挂载时手改正文，现在是「该源另起一行、文件另存」（
  `adopt_raw_edit` 里 `raw_mounts(row["raw_id"]) != [key]` 分支），文件名会从 `a.md` 变成
  `a-2.md` 一类，而不是原地换行。是否符合预期待确认。

### 并行改动与测试基线

同一棵树另有会话在处理优先级更高的修复（`workspace.py` 拆出、`submit` 的
`read_payload(wiki, row)` 签名、`ledger.ensure_open_page_index`），期间
`tests/selftest.py` 的总断言数在 174–190 之间浮动。因此此刻的 selftest 结果不能当作本
issue 的验收证据；等并行改动落地、树稳定后重跑，再回填下面的验收项。

## 待实现与验收条件

### 实施顺序

1. **账本**（已落地，待验收）：新增 raw 表与映射列；正文 sha256 主键、展示名、
   `last_parsed_digest`、当前映射落库（历史不单独存表，靠旧 raw 行的保留与回收体现）。
2. **ingest**（进行中）：内容去重、origin 挂载、`last_parsed_digest` 判定人工修改、源变动换行、
   改名一删一赠、展示名规则、旧行无引用即删。
3. **任务链**：parse 任务目标仍是真实源；route/page 任务 subject 与输入改为 raw；工作包
   同时给 raw 路径与源路径；删除 brief 里教 `read` 的文案（`tasks.py:79`、`:131`）。
4. **引用与校验**：页面 `sources`、证据段继续写源相对路径；`sync` 的 `E_UNKNOWN_SOURCE`
   改查 `sources.raw_id`；引文核对按 raw 正文。
5. **过期与清理**：孤儿页清理、部分断链标 stale、旧行按引用情况回收。
6. **删除 `read`**：`command_read`、`resolve_ref` 的 raw 分支、`topics.py:67` 的 `read`
   字段（改为 `path`）、SKILL、cli、verification、SCENARIO、selftest 中相关引用。
7. **测试**：selftest 覆盖去重、共享 raw、人工修改保持、改名一删一赠、孤儿页清理、删
   `read` 后的检索与读取路径。

### 验收

- [ ] 同一内容的两份源只产生一份 raw、只排一次 route 任务。
- [ ] 源改名后页面里的旧路径失效可被识别：只剩这一个源时页面按孤儿页清理，多源时标 stale。
- [ ] 人工排版 raw 后 `last_parsed_digest` 保持旧值，下一轮 ingest 不覆盖；显式重新摄入
      可覆盖。
- [ ] 源内容变化后生成新 hash 行并改指 origin，`last_parsed_digest` 更新；旧行在没有其他
      源指向后即回收，仍有其他源指向时保留。
- [ ] 页面引用的源部分缺失 → stale；全部缺失 → 清理，不残留孤儿页。
- [ ] 删除 `read` 后 `skills/llm-wiki/SKILL.md`、`docs/readme/cli.md`、
      `docs/readme/topics.md`、`docs/agent/verification.md`、`docs/agent/engine.md`、
      `scenarios/topic-exploration/SCENARIO.md`、`skills/llm-wiki/scripts/tasks.py`、
      `skills/llm-wiki/scripts/topics.py`、`tests/selftest.py` 无残留 `read "raw:` /
      `read "page:` / `read "<root 路径>"` 的教法，错误码表的定位手段同步更新。
- [ ] 改动后同一轮检索核实新内容，并跑通 selftest。
