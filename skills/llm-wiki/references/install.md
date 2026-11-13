# 安装与搬迁

这个技能没有构建步骤、没有服务、没有全局状态。整个 `llm-wiki/` 目录就是交付物：
复制到哪里，哪里就能用。本文写给**执行安装的 agent**，人只需要说“装上”。

## 1. 检查运行时

```bash
python3 -V          # 需要 3.8+，只用标准库
command -v pdftotext # 可选；没有也能用，PDF 会走 pypdf 或报告 unsupported
```

PDF 支持按优先级降级：`pdftotext` → `pypdf` → 跳过并提示。不要为了装 PDF 解析器
阻塞其他格式的使用。

## 2. 放置目录

标准做法是把整个目录放到 agent 会扫描的技能根下，目录名保持 `llm-wiki`：

| 运行时 | 常见技能根 |
| --- | --- |
| DSH | `~/.dsh/skills/` 或项目内 `.dsh/skills/` |
| Claude Code | `~/.claude/skills/` |
| 通用 AGENTS 约定 | `~/.agents/skills/` |
| Codex | `~/.codex/skills/` |
| OpenCode | `~/.config/opencode/skills/` |

自动放置（只写入**已经存在**的技能根，不创建无关目录）：

```bash
python3 scripts/install.py --target auto --json
```

- `auto` 只装进第一个已存在的根：项目内 `.agents/skills` → `.claude/skills` →
  `.dsh/skills`，然后是全局 `~/.dsh`、`~/.claude`、`~/.agents`、`~/.codex`、
  `~/.config/opencode`；`--all` 写入全部，`--dry-run` 只打印计划。
- 已存在同名目录时默认不动；`--force` 会先改名为 `llm-wiki.bak-<时间>` 再复制。
- 安装脚本会从目标位置执行一次 `wiki.py --version` 自检。

同名冲突：如果已有旧版 `llm-wiki` 技能（例如随 Rust CLI 分发的
`.wiki/skills/llm-wiki`），先移除或改名再安装。两个同名技能同时存在时，agent 只会加载
其中一个，无法保证是你期望的版本。

没有任何已知技能根时，脚本返回 `E_NO_TARGET`。此时**不要**自己发明目录：把技能目录
留在原处，直接按路径加载 `SKILL.md` 即可，或由用户指定 `--target <技能根>`。

## 3. 验收安装

```bash
python3 <安装路径>/scripts/wiki.py --version
python3 <安装路径>/scripts/selftest.py
```

`selftest.py` 在临时目录跑 20 项检查（建库、摄入与分类、route 决定、页面提交、
镜像漂移、import/restore、导出、超限、dry-run、doctor），不接触真实数据。全部通过即
安装可用。

## 4. 在业务目录启用

技能安装位置与知识库位置是两件事。到要整理的资料目录执行：

```bash
python3 <安装路径>/scripts/wiki.py ingest --json
```

首次运行会在**当前目录**创建 `.wiki/`（`wiki.json`、`wiki.db`、`raw/`、`pages/`、`drift/`）。
也可以在任何位置用 `--root <资料目录>` 指向知识库，不需要 `cd`。

摄入只负责把文件变成来源并排队 route 任务；建页由后续任务循环完成，见 `SKILL.md`
的主循环。不要手工编辑 `.wiki/` 下的文件，页面只能通过 `submit`、`page import` 修改。

## 5. 搬迁与卸载

- **搬迁**：连同 `.wiki/` 一起复制到新位置即可；`sources.key` 是相对路径，根目录整体
  移动不受影响。只复制技能目录不会带走知识库。
- **卸载**：删除技能目录。知识库在业务目录的 `.wiki/` 内，独立存在；不需要时手动删除
  该目录即可，删除前先备份 `pages/`。
- **升级**：用新版本目录替换技能目录；`.wiki/wiki.db` 的表结构只增不改，旧库可直接用。
  替换前备份整个 `.wiki/`。

## 6. 与旧版本的关系

本技能是独立实现，只认自己的 `.wiki/wiki.json` 与 `.wiki/wiki.db`。旧版（Rust CLI 或
0.1.x Python 版）初始化的 `.wiki/` 表结构与命令面不同，不要直接指向；在新目录重新
`ingest` 一次原始文件即可。旧版按"页面文件为源"写出的 `.wiki/pages/*.md` 会在首次
运行时被收为 rev1（`origin=external`），但页面元数据不完整，建议对照 raw 复核后重交一版。
