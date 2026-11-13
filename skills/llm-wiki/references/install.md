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

`selftest.py` 在临时目录跑 12 项检查（建库、摄入 Markdown/HTML/DOCX、建页、检索、
链接、源消失与恢复、源变更需重写、dry-run 不建库、错误码、doctor、含空格/中文路径搬迁），
不接触真实数据。全部通过即安装可用。

## 4. 在业务目录启用

技能安装位置与知识库位置是两件事。到要整理的资料目录执行：

```bash
python3 <安装路径>/scripts/wiki.py ingest --json
```

首次运行会在**当前目录**创建 `.wiki/`（`wiki.json`、`wiki.db`、`raw/`、`pages/`）。
也可以在任何位置用 `--root <资料目录>` 指向知识库，不需要 `cd`。

## 5. 搬迁与卸载

- **搬迁**：连同 `.wiki/` 一起复制到新位置即可；`sources.key` 是相对路径，根目录整体
  移动不受影响。只复制技能目录不会带走知识库。
- **卸载**：删除技能目录。知识库在业务目录的 `.wiki/` 内，独立存在；不需要时手动删除
  该目录即可，删除前先备份 `pages/`。
- **升级**：用新版本目录替换技能目录；`.wiki/wiki.db` 的表结构只增不改，旧库可直接用。
  替换前备份整个 `.wiki/`。

## 6. 与旧 Rust CLI 的关系

本技能是独立实现，只认自己的 `.wiki/wiki.json` 与 `.wiki/wiki.db`。不要把它指向旧 CLI
已经初始化的 `.wiki/` 目录：两者的库表与配置格式不同。需要迁移时，在新目录重新
`ingest` 一次原始文件，再把 `pages/` 里的知识页按需搬过去。
