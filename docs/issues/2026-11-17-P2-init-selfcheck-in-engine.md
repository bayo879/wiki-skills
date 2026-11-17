# 初始化与自检仍写在提示词里，未由引擎自动完成

## 问题

用户要求：初始化与自检这部分「不要在提示词里」，由程序做掉；CLI 能修的自己修，
修不了的走异常处理交给 agent。当前实现与这条要求相反，初始化步骤分散在提示词、
文档和命令里，普通命令还会拒绝本可自动修复的半初始化工作区。

现状证据：

- `skills/llm-wiki/SKILL.md:32-44` 要求安装后、建库前、每次日常操作前先运行
  `check --json`，只有 `ok: true` 才允许继续；`:46-55` 又要求先 `init` 再 `ingest`。
- `docs/readme/design-principles.md:22-23` 把 `check` 定义为「初始化和日常操作的固定
  门禁」。
- 引擎侧 `open_wiki`（`skills/llm-wiki/scripts/wiki.py:1615`）实际上已经调用
  `Wiki.ensure()` 自动建 `.wiki/`、`wiki.json`、数据库 schema 和 raw 目录；空目录直接
  `ingest` 就能成功，并不需要先 `init`。
- 但 `require_health`（`:1644`）在打开账本前先做门禁，且 `health.require` 只检查
  `wiki.db` 是否存在、必需表是否齐全（`skills/llm-wiki/scripts/health.py:113-170`）。
  半初始化状态会被直接拒绝：
  - 复现：目录里只有 `.wiki/wiki.json`、没有 `wiki.db`，执行 `ingest --json` 返回
    `E_HEALTH`，message 为 `self-check failed: ledger: .../.wiki exists but wiki.db is missing`，
    `next` 却是「run `wiki.py check --json`; reinstall the skill if files are missing」。
    这条 next 既不能修复问题（`check` 会同样失败），也和缺文件无关。
- `allow_missing` 只在 `command_init`（`:1665`）开放；`ingest`、`next`、`submit`
  （`:1687`、`:1956`、`:2014`）都不允许缺失，把「先 init」固化成了命令顺序。
- 安装级检查（Python 版本、脚本/模板/场景文件是否齐全、工作区是否可写）与账本级
  可修复问题混在同一个 `E_HEALTH` 里，agent 无法从错误码判断该重装、该修权限，
  还是引擎自己重建即可。

问题不在「要不要保留 check/init」，而在于普通命令遇到未初始化、半初始化、schema
缺列这类可修复状态时，是否应当自动修复；以及无法修复时，异常是否给出可执行的
下一步。

还有一层：CLI 有意不向 agent 暴露内部流程事实，agent 看不到「账本缺表意味着什么」
「该重建还是该恢复备份」。现有错误只有 `code/message/next`，`next` 又是固定文案，
没有告诉 agent「这类问题该看哪份操作手册的哪一节」。SKILL.md 也没有错误分类与手册
入口，agent 只能靠猜或反复试。这与「结合 skill 和报错信息提示」的要求不符。

## 已确认决策（2026-11-17）

- 采用候选方案 1：初始化与自检由引擎在打开账本时完成，提示词不再要求先 `check`、
  再 `init`。
- **所有需要账本的命令都先检测并自愈**；`version`、`scenario ...` 不读写账本，不属于
  此列。`check`、`doctor` 保持只读诊断，不承担自愈。
- agent 操作手册以技能内 `skills/llm-wiki/references/recovery.md` 为准，随技能安装；
  `docs/readme/recovery.md` 只保留面向人的入口与原则，不复制操作步骤、不形成第二套
  说法。
- 错误结构增加 `doc` 与 `section`：`doc` 是手册的绝对路径，`section` 是小节标题。
  不拼 `#锚点`，避免读文件工具把锚点当成文件名。
- 自愈结果用 `repaired` 返回；可自愈状态不报错，只有不可自修时才进入异常路径。

## 影响

- 每个 agent 必须先读提示词并手动跑 `check`、`init`，漏跑就被门禁挡住；提示词因此
  承担了本应由程序保证的初始化职责，SKILL.md 越写越长。
- 半初始化工作区（例如手工删了 `wiki.db`、旧版本升级缺列、raw 目录缺失）无法用
  普通命令恢复，只能先跑 `init`，而错误提示并没有告诉用户这一点。
- `E_HEALTH` 同时覆盖「重装技能」「修权限」「重建账本」三类问题，`next` 文案固定，
  agent 只能猜；不符合「需要 agent 辅助的，跑异常处理让 agent 解决」。
- 与用户「程序里做掉」的目标直接冲突；也让「环境是否就绪」缺少单一、可验证的定义。

## 候选方案（方案 1 已采用，其余保留作取舍记录）

1. **普通命令自动 ensure + 自愈（已采用）**
   在 `open_wiki` 打开账本前增加 `ensure_environment`：创建/补齐 `.wiki/`、`wiki.json`、
   空库的数据库 schema、raw 目录；可修复项直接修，并在 JSON 输出里返回 `repaired`
   列表。旧 schema 有数据时不做隐式迁移，保留原库并报 `E_STATE`。只有不可修复项才
   报错，错误码区分「安装缺失」「权限」「账本损坏」「版本不兼容」，`next` 给出具体
   动作。`check` 保留为只读诊断，`init` 保留为显式重建入口，普通流程不再要求先跑。
2. **自动调用 `check --fix`**
   所有需要账本的命令先跑 `check(fix=True)`，把修复集中在 check。入口统一，但每次
   命令都有额外扫描开销，且 `check` 目前职责是安装自检，扩成修复入口会让职责混杂。
3. **只在失败时自愈**
   正常 `open_wiki` 成功就跳过检查，失败后再尝试修复并重试。性能最好，但「半初始化」
   与「真正损坏」的判定要精确，否则可能把损坏库当可修复状态处理。
4. **只改提示词，不改进程序**
   与用户要求相反，排除。

「环境就绪」的最小集合：目录与配置文件、数据库 schema（含必需列）、raw 目录、
锁与临时目录可写。可自动修复项：缺 `wiki.db`、空库缺表/缺列、缺 `wiki.json`（重建
默认配置）、缺 raw 目录。不可自动修复项：旧 schema 有数据、Python 版本过低、技能文件
缺失、工作区不可写、数据库 `integrity_check` 失败、配置 JSON 非法（保留原文件，交给
agent）。

## 错误与手册指引（方案 1 的配套，已定案）

「给解决方法」分两层：能自动修的由引擎直接修，不报错；不能自动修的，错误里要同时给出
可执行动作和手册位置。错误结构在现有 `code/message/next` 之外增加 `doc` 与
`section`：

```json
{
  "code": "E_DB",
  "message": "账本完整性检查未通过，继续操作可能丢数据",
  "next": "停止写入；从备份恢复整个 .wiki/ 后重跑上一条命令",
  "doc": "/path/to/skills/llm-wiki/references/recovery.md",
  "section": "账本损坏"
}
```

`doc` 是安装后手册的绝对路径，`section` 是手册内的小节标题；两者都由引擎在报错时
算出，agent 用通用读文件工具打开 `doc`、按 `section` 查找即可。`message` 只描述现象，
不解释内部表结构或任务状态机，避免泄露流程事实。`next` 必须能直接执行，不能是
「运行 check」这种绕回门禁的动作。

计划中的错误分类与手册对应关系：

| 情况 | 引擎处理 | 错误码/输出 | `next` 与手册 |
| --- | --- | --- | --- |
| `.wiki/` 或 `wiki.db` 缺失 | 自动建库 | 无错误，`repaired` | — |
| 空库缺表、缺列（无旧数据） | 自动建表/补列 | 无错误，`repaired` | — |
| 旧 schema 且有数据 | 不自动迁移，保留原库 | `E_STATE` | 备份后删 `.wiki/`，重跑 `ingest`；手册「旧账本」 |
| `wiki.json` 缺失 | 自动重建默认配置 | 无错误，`repaired` | — |
| `wiki.json` 非法 | 不猜，保留原文件 | `E_CONFIG` | 修正 JSON 或从备份恢复；手册「配置损坏」 |
| raw 目录缺失 | 自动创建 | 无错误，`repaired` | — |
| 数据库完整性失败 | 不删库、不重建 | `E_DB` | 从备份恢复 `.wiki/`；无备份时从观看区抢救；手册「账本损坏」 |
| Python 版本过低 | 不处理 | `E_HEALTH` | 安装 3.12 或运行 `bootstrap.ps1`；手册「运行时与安装」 |
| 技能文件缺失 | 不处理 | `E_HEALTH` | 重新安装技能；手册「运行时与安装」 |
| 工作区不可写 | 不处理 | `E_HEALTH` | 换可写工作区或修权限；手册「工作区不可写」 |
| 路径越界 | 不处理 | `E_PATH` | 在工作区内执行；手册「工作区边界」 |

配套改动：

- 新增 agent 侧操作手册 `skills/llm-wiki/references/recovery.md`，随技能目录一起安装；
  按错误类别分节，每节写「现象、能否自修、手动步骤、何时停下来问人」。技能内手册是
  唯一操作口径；`docs/readme/recovery.md` 只保留面向人的入口与原则，指向技能内手册，
  不复制步骤。
- `SKILL.md` 不展开流程，只保留一张很短的「遇到错误看哪里」索引：错误码 → 手册小节，
  并说明普通命令会自修、`repaired` 可忽略、不要手改 `.wiki/`。
- `check`/`doctor` 保留为诊断入口，但不再要求每次操作前运行；它们的输出也带 `doc`
  指向手册，而不是只给失败项名称。

## 待确认与验收条件

已确认：

- [x] 「环境就绪」的最小集合：`.wiki/` 与 `wiki.json`（缺则建）、数据库与必需表列、
  raw 目录、锁与临时目录可写。
- [x] 所有需要账本的命令一律先检测并自愈；`version`、`scenario ...` 不碰账本，
  `check`/`doctor` 只读诊断。
- [x] 可自修项：缺 `.wiki/`/`wiki.db`、空库缺表缺列、缺 `wiki.json`、缺 raw 目录。
  不可自修项：旧 schema 有数据、配置 JSON 非法、数据库完整性失败、运行时/安装/权限
  问题、路径越界。
- [x] 不可自修错误带 `doc`（绝对路径）与 `section`，`next` 可直接执行。

仍待确认：

- [ ] `repaired` 的字段形态：只列新建/修复的路径，还是同时给出动作
  （`created`/`repaired`/`rebuilt`）。建议先只列路径，与现有 `created` 对齐。
- [ ] 只读命令（`query`/`status`）在空目录会创建空账本；这是「所有命令先检测」的
  直接结果，需要确认是否接受，还是改为只读命令遇到未初始化时返回 `E_EMPTY` 而不建库。
- [ ] 旧 schema 是否需要 `init --reset` 这类显式重建入口；当前只记录手动步骤，不新增命令。

验收：

- [ ] 空目录直接 `ingest` 成功（现状已满足），且不再要求先 `init`。
- [ ] 半初始化目录（有 `.wiki/wiki.json`、无 `wiki.db`）直接 `ingest`/`next` 成功，
      输出包含 `repaired`。
- [ ] 空库缺表/缺列、缺 raw 目录、缺配置可自动修复且不丢已有数据。
- [ ] 旧 schema 有数据时不静默迁移，返回 `E_STATE` 并指向「旧账本」小节。
- [ ] 数据库损坏、工作区不可写、技能文件缺失时返回可区分的错误码和可执行 `next`，
      不静默删除或重建账本。
- [ ] 每个不可自修错误都带 `doc`，指向随技能安装的 `references/recovery.md` 具体小节；
      `next` 能直接执行，不是「去跑 check」。
- [ ] `skills/llm-wiki/references/recovery.md` 存在并覆盖上表所有错误类别，安装技能后
      可读；`docs/readme/recovery.md` 只做入口，不出现两套互相矛盾的步骤。
- [ ] `SKILL.md` 只保留错误码到手册入口的短索引，不再要求每次操作前 `check`/`init`；
      `docs/readme/cli.md`、`docs/readme/design-principles.md`、`docs/agent/engine.md`
      同步新的初始化与错误契约。
- [ ] `tests/selftest.py` 覆盖空目录、半初始化、空库缺列、旧 schema、损坏库、只读工作区
      六类场景，并断言不可自修错误的 `doc` 与 `next` 存在且指向有效小节。
