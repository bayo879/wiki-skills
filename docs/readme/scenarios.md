# 场景能力

场景能力把一类任务的执行流程写成可检索、可加载的文件，放在项目根目录的
`场景能力/` 下。它借用 skill 的组织方式：一个主流程文件，加上按需读取的脚本和说明。
和知识页不同，场景描述“怎么做”，不描述“是什么”；它不进入账本，也不参与页面引用。

## 目录结构

```text
<root>/场景能力/
└── account-opening/
    ├── SCENARIO.md         主流程，必需
    ├── scripts/
    │   └── check_materials.sh
    └── references/
        └── material-list.md
```

- `场景能力/` 只扫描一级子目录，每个子目录是一个场景；隐藏目录和普通文件忽略。
- `SCENARIO.md` 是唯一入口，文件名固定；缺它、缺 `description` 或正文为空都视为不合法。
- `scripts/` 放确定性操作的脚本，`references/` 放执行时才需要读的细节。
- 其他文件也会列进资源清单，`kind` 为 `other`；它们不会自动执行或加载。

## SCENARIO.md

```markdown
---
name: 开户流程
description: 新客户开户时核验身份、登记资料并完成激活
triggers:
  - 开户
  - 新客户
  - 激活
---

# 开户流程

## 适用

客户首次开立账户时使用。已有账户的补充资料不适用。

## 步骤

1. 核对身份证件与本人是否一致；不一致时停止，并记录原因。
2. 在系统登记客户基本信息，证件号码按页面规则脱敏。
3. 让客户确认资料，保存确认记录。
4. 激活账户，并把回执编号写进结果。

## 判断点

- 证件过期：不继续，告知客户更新证件后再来。
- 客户为未成年人：转到监护人办理流程。

## 完成标准

- 账户状态为已激活；
- 有客户确认记录；
- 结果里有回执编号。
```

frontmatter 字段：

| 字段 | 必需 | 说明 |
| --- | --- | --- |
| `name` | 否 | 展示名；缺省用目录名 |
| `description` | 是 | 一句话说明场景解决什么问题，参与检索与展示 |
| `triggers` | 否 | 关键词列表；逗号、顿号、分号分隔的字符串也可以 |

正文写给执行者看，重点写清顺序、判断点、完成标准和失败时怎么处理。步骤要能执行，
不要只写原则；需要事实依据的地方，指向知识页或原始文件，而不是把事实抄进场景。
`SCENARIO.md` 超过 200000 字符会被判为不合法；主流程保持精简，细节放 `references/`。

## 脚本与说明

`scripts/` 里的脚本应只做确定性、可重复的操作，并在文件头或同目录 README 里写清用途、
参数、输入输出和失败行为。执行前先确认它做什么；需要外部环境、凭据或联网时，在场景正文
里显式说明，不要假设执行者会盲跑。

`references/` 放长说明、字段表、检查清单等。主流程只写“何时读哪个文件”，细节放进去，
避免 `SCENARIO.md` 膨胀。

## 检索与加载

场景不经过 `ingest`，`场景能力/` 在摄入时整目录跳过，因此改流程不需要重建知识库。
`query` 会直接扫描场景文件，结果里的 `scenarios` 非空表示命中：

```bash
python3 <skill>/scripts/wiki.py query "开户" --json
python3 <skill>/scripts/wiki.py scenario list --json
python3 <skill>/scripts/wiki.py scenario load account-opening --json
```

`scenario load` 返回 `content`（`SCENARIO.md` 全文）、`meta`、`path` 和 `resources`。
按 `content` 的步骤执行，需要细节时再读 `resources` 里的文件。结构不合法的场景不会出现在
`query` 结果里，但会出现在 `scenario list` 中并带 `error`，方便修正。

## 边界

- 场景是能力，不是知识页：不要把场景当来源摄入，也不要在页面里引用场景文件。
- 场景步骤与知识页冲突时，流程以场景为准；事实依据仍以页面和 raw 为准。
- 场景文件是活文件，直接编辑即可生效；不要在 `.wiki/` 里另存副本。
- `scenario list` / `scenario load` 只读，不要求先 `init`，也不会创建 `.wiki/`。

完整命令字段与错误码见 [CLI 契约](cli.md#scenario)。
