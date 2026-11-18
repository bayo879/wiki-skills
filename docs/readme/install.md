# 安装与搬迁

这个技能没有构建步骤、没有服务、没有全局状态。整个 `llm-wiki/` 目录就是交付物：
复制到哪里，哪里就能用。本文写给**执行安装的 agent**，人只需要说“装上”。

## 1. 检查运行时

```bash
python3 -V          # 需要 3.12+，只用标准库
command -v pdftotext # 可选；没有也能用，PDF 会走 pypdf 或报告 unsupported
```

### Windows（PowerShell 5.1+）

Windows 上通常没有 `python3`，用 `py -3.12` 或指向 3.12+ 的 `python` 替代。首次使用可以运行引导脚本，
它查找已有的 Python 3.12+，找到后调用 `install.py`：

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\bootstrap.ps1
```

在 cmd 或 Windows Terminal 里执行同一条命令即可，参数原样追加（如 `-DryRun`）。技能包不附带
`.cmd`/`.bat` 包装：下游分发不接受批处理脚本，`bootstrap.ps1` 是唯一的 Windows 入口。

- 查找顺序：`%LOCALAPPDATA%\Programs\Python\Python3*` → `py -3` → `python` → `python3`，
  跳过 WindowsApps 的转发桩；只要求 3.12 及以上，不要求恰好 3.12。
- 运行时由调用方提供：agent 自带解释器时用 `-Python <python.exe>` 直接指定，跳过搜索。
- 脚本不下载、不安装 Python，也不改 PATH；找不到合适运行时时输出 `E_PYTHON` 并退出 1，
  错误对象里带 `references/recovery.md` 的路径与小节名。
- 不需要 `pip install`：引擎只用标准库；PDF 解析器仍是可选项，不阻塞安装。
- 放置失败时用 `-Target` 指定技能根，例如
  `-Target "$env:USERPROFILE\.dsh\skills"`；`-DryRun` 只打印计划，不复制。
- 引导脚本只负责运行时和放置技能，不创建 `.wiki/`；知识库仍要在资料目录里 `ingest`。

整个项目基线是 Python 3.12（最低版本），更新的 3.x 都能用。

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

安装位置（项目级或全局）只决定技能从哪里加载，不改变知识库位置：所有 wiki 命令仍在
会话工作区根目录执行。不要把技能目录或数据目录当作知识库根。

自动放置（只写入**已经存在**的技能根，不创建无关目录）：

```bash
python3 scripts/install.py --target auto --json
```

- `auto` 只装进第一个已存在的根：项目内 `.agents/skills` → `.claude/skills` →
  `.dsh/skills`，然后是全局 `~/.dsh`、`~/.claude`、`~/.agents`、`~/.codex`、
  `~/.config/opencode`；`--all` 写入全部，`--dry-run` 只打印计划。
- 已存在同名目录时默认不动；`--force` 会先改名为 `llm-wiki.bak-<时间>` 再复制。
- 安装脚本会从目标位置执行一次 `wiki.py --version` 启动检查；验收用下一节的 `check`。

同名冲突：如果已有旧版 `llm-wiki` 技能（例如随 Rust CLI 分发的
`.wiki/skills/llm-wiki`），先移除或改名再安装。两个同名技能同时存在时，agent 只会加载
其中一个，无法保证是你期望的版本。

没有任何已知技能根时，脚本返回 `E_NO_TARGET`。此时**不要**自己发明目录：把技能目录
留在原处，直接按路径加载 `SKILL.md` 即可，或由用户指定 `--target <技能根>`。

## 3. 验收安装

```bash
python3 <安装路径>/scripts/wiki.py --version
python3 <安装路径>/scripts/wiki.py check --json
```

`--version` 能打印版本，就说明解释器与技能目录可用。`check` 是只读诊断：验证解释器、技能
必需文件与模板、当前工作区可写、已有账本可读，不接触真实页面内容，也不修任何东西。不必先
跑 `check` 再 `ingest`：第一次命令会自己建出 `.wiki/`，补了什么写在 `repaired` 里。完整回归
测试在仓库的 `tests/selftest.py`，属于开发/CI 资产，不随技能复制到安装目录。

## 4. 在业务目录启用

技能安装位置与知识库位置是两件事。**知识库根 = 会话工作区根。** 用会话工作区打开要
整理的资料目录，在该目录执行（不要 `cd` 到工作区外）：

```bash
python3 <安装路径>/scripts/wiki.py ingest --json
```

首次运行会在**当前工作区**创建 `.wiki/`（`wiki.json`、`wiki.db`、`tasks/`、
`drafts/`），以及观看区 `<view>/` 与 `<view>/raw/`（`publish.dir`，默认 `wiki/`）；
原文副本与页面副本都在观看区，不在 `.wiki/` 下。`--root` 只接受当前目录或它下面的子目录，不能指向工作区外；资料在区外时，
请用户把那个目录作为工作区打开，或复制进来。

草稿、任务目录、导出与临时文件都放 `.wiki/` 下：`.wiki/drafts/`、
`.wiki/tasks/<id>/`、`.wiki/export`、`.wiki/tmp/`。根目录下非隐藏文件会被当成来源，缓存不要散落在根目录。

摄入只负责把文件变成来源、按正文去重并排队 route 任务；建页由后续任务循环完成，见 `SKILL.md`
的主循环。不要手工编辑 `.wiki/` 下的文件，页面只能通过 `submit`、`page import` 修改。

## 5. 搬迁与卸载

- **搬迁**：连同 `.wiki/` 和观看区（默认 `wiki/`，含 `raw/` 原文与 `pages/` 页面）一起
  复制到新位置即可；`sources.key` 是相对路径，根目录整体移动不受影响。只复制技能目录不会
  带走知识库；只带 `.wiki/` 会丢掉原文副本。
- **卸载**：删除技能目录。知识库在业务目录的 `.wiki/` 与观看区里，独立存在；不需要时手动
  删除这两个目录即可，删除前先备份 `pages/` 与 `raw/`。
- **升级**：用新版本目录替换技能目录；替换前备份整个 `.wiki/` 与观看区。`.wiki/wiki.db`
  有数据且缺少本版本新增的列时会被拒绝（`E_STATE`）：备份后删 `.wiki/` 重新 `ingest`；
  旧库全空时按新结构自动重建。

## 6. 与旧版本的关系

本技能是独立实现，只认自己的 `.wiki/wiki.json` 与 `.wiki/wiki.db`。旧版（Rust CLI 或
0.1.x Python 版）初始化的 `.wiki/` 表结构与命令面不同，不要直接指向；在新目录重新
`ingest` 一次原始文件即可。旧版按"页面文件为源"写出的 `.wiki/pages/*.md` 已不再被接管：
升级到本版本后这些文件留在原地，请人工核对后删除 `.wiki/pages/` 与 `.wiki/drift/`，
页面内容以账本为准。
