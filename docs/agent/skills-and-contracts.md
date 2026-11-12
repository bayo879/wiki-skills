# 技能、CLI 契约与文档维护

由根级开发指引迁入；原节号保留，便于识别既有交叉引用。本文记录开发约束与项目特有问题，产品说明见 [架构说明](../../docs/readme/architecture.md)。

### 2.3 技能不复述 CLI 的输出形状

`output/.wiki/skills/llm-wiki/` 只写流程与判断；JSON 字段路径、必填字段、正式页位置、报错码含义都由 CLI 现打（`ingest` 给 `write_to`、`types` 与 `next_task`，`task done` 在源漂移时点名哈希不符），技能里不再有枚举表、字段表、错误码表和 `data.*` 路径。`stage`、`guide`、`contract` 随任务现给：route 视图说明信息点、归属与维护处置，page 视图给出 `before` 快照与完整正文要求；技能只讲怎么读与怎么决断。
`tests/docs_contract.rs::the_skill_states_no_json_field_paths` 禁止技能文件出现 `data.*`（只放过 `error.code`/`error.message`/`error.detail`）；`tests/snapshots/cli-keys.json` 仍锁每条命令的输出形状（`LLM_WIKI_UPDATE_SNAPSHOTS=1` 重生成）。改输出形状时改快照即可，不需要同步技能。

### 2.5 文档一致性由机器守，不靠人记

五类守卫，改文档前先知道它们在管什么：

| 守卫 | 抓什么 |
|---|---|
| `tests/invariants.rs` | SQL 单点、账本单入口、对账唯一、不变量都有测试、每条 lint 规则有 `l{n}_` 测试（不再比技能表） |
| `tests/docs_contract.rs` | flag 存在、闭世界词表、`E_*` 存在、技能不写 JSON 字段路径 |
| `tests/skills_pack.rs` | 只发两个技能、frontmatter 只有两项、正文行数上限、引用的子命令真实存在 |
| `tests/snapshots/cli-keys.json` | 每条命令输出的键路径 |
| `tests/docs_tokens.allow` | 词表白名单，加名字要有理由 |

诚实边界写在 [architecture.md 的「机器守卫」节](../../docs/readme/architecture.md)：它保证「文档没写错名字」，不保证「文档写得够全」。

### 2.7 一个技能，四份文件

`output/.wiki/skills/llm-wiki/SKILL.md` 是 wiki 本身的入口（边界、命令入口、最小序列、route 与 page 的衔接、来源确认、触发入口、退出码）；`init.md` 写「首次接入」（持久化记忆与定时任务两件 agent 侧的事，不写 CLI 初始化流程）；`content.md` 写「写什么」（建页判断、来源路由、并入与改写、证据、去重）；`ops.md` 写「出了问题怎么看」（体检、判断层、冲突与覆盖风险、忽略、叫人）。
`output/.wiki/skills/llm-wiki-session/` 是第二个技能：会话蒸馏，写进 `[ingest].session_dir` 指向的源目录（默认 `sessions`），原始会话不存。一个会话一个文件，不按时间切。
拆分只为了让 agent 按需只读一份，不是新的契约；`tests/skills_pack.rs` 断言只发这两个技能、正文行数不超限。

### 2.8 技能的价值在流程纪律，不在用法说明

用 A/B（读技能 vs 裸跑 CLI）实测过四个任务：技能的增量集中在顺序与门禁——`ingest` 一次给出 `write_to` 与 `next_task`，route 视图再给 `affected_pages`、`stage`/`guide`/`contract`，page 任务只在 route 判定了新增或修订后出现；`task done` 只认根任务登记的 raw 哈希、漂移就不合并且保留待办；
flag 与字段的解释与 `--help`、`init.rs` 生成的 `SCHEMA.md` 高度重复，加长技能文本收益很低，反而会和 CLI 漂移。
所以：技能里写「什么时候做什么、做错了怎么判」，不写「这个参数是什么意思」。
例外是 `SKILL.md` 保留一份最小命令速查（`doctor` / `ingest` / `ingest --auto` / `read` / `query` / `task done` / `task status` / `task retry` / `ignore` 与常用 flag）：agent 需要先知道入口存在，但参数、字段形状与报错码仍以 `--help` 和 CLI 输出为准。

### 2.18 删命令前先和 spec 做一次差集

命令面收窄过三轮：22 → 6 删了 `version`，直接写页那一轮又删了 `submit` 与整套版本管理，任务树这一轮把 `done` 换成 `task done/status/retry`。两次都出现过文档还在教旧入口的情况（`README.md` 曾写着「submit 建版本」，而代码明确不建）。
删之前先回答「删掉以后坏了怎么办」：`.wiki/` 是唯一内容所在，而且通常不进 git（2.12），所以没有回退就等于没有出口——现在答案只能是仓库外的独立备份，文档必须这么写，不能假装还有内置退路。
删任何一条命令或 flag 前，`grep` 一遍 `README.md`、`docs/readme/`、`docs/issues/`、`output/.wiki/skills/`、`src/init.rs`（`SCHEMA.md`），差集里没处理干净的要么补实现、要么改文档。旧命令现在由 clap 返回 `E_USAGE` exit 2，`tests/cli_submit.rs::submit_is_rejected_without_touching_legacy_candidates` 与 `tests/cli_version.rs::version_subcommands_are_rejected_and_preserve_legacy_bytes` 钉住「拒绝且不动旧字节」。

### 2.23 改文档用会报错的工具，不用静默替换

`edit` 找不到 `old_string` 会报错；`python str.replace`、`sed -i` 找不到模式照样返回 0。本项目的文档句子是可被引用的断言（README、`docs/readme/`、`AGENTS.md` 里写着"已经这么做"），用静默成功的工具改它，等于把"改过了"建立在"我记得改过了"上。第五道机器守卫（2.5）守不住这个：它检查的是词表和形状，不检查某一句是否真的落进了文件。

规矩：

- 单点改一句、改一行、追加一段：用 `edit`，失败就说明我记错了原文。
- 整段重写或超过五处独立替换：才用脚本，并且同一轮 `grep` 到新内容才算改完。
- 复核是取证，不是礼貌：报告"文档已改"之前，先看见 `grep` 的输出。
