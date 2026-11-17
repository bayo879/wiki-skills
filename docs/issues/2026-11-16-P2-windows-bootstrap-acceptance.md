# Windows 引导脚本尚未实机验收

## 问题

`skills/llm-wiki/scripts/bootstrap.ps1` 是唯一的 Windows 入口（技能包不附带
`bootstrap.cmd`：下游分发禁止 `.cmd`），在 cmd 与 Windows Terminal 里执行
`powershell -NoProfile -ExecutionPolicy Bypass -File scripts\bootstrap.ps1`，参数原样追加。

2026-11-17 精简：下游 agent 都自带 Python 运行时，「下载并安装 Python」这条路无人走，
镜像列表、安装器参数与 `winget` 回退已删除。脚本现在只做两件事——查找已有的 Python 3.12+
（`-Python <python.exe>` 可直接指定运行时），把参数透传给 `install.py` 并照传退出码。

当前开发环境是 macOS，没有 PowerShell，无法执行脚本；以下行为都还没有实机证据：

- `%LOCALAPPDATA%\Programs\Python\Python3*`、`py -3`、`python`、`python3` 的查找顺序，
  以及跳过 `*\WindowsApps\*` 转发桩；
- 版本判定：3.12 及以上都接受（3.13、3.14 不强制降到 3.12），3.11 必须拒绝；
- 找不到运行时时 `E_PYTHON` 的 JSON 形状（含 `doc`、`section = 运行时与安装`）与退出码 1；
- `install.py --target auto` 在 Windows 技能根下的放置结果，`-Target`/`-DryRun`/`-Force`/`-All`
  的透传与退出码；
- `-NoProfile -ExecutionPolicy Bypass -File` 这种调用形式下，参数与退出码在 cmd 里能否原样穿透。

`tests/selftest.py` 只覆盖 Python 引擎，不覆盖 PowerShell 引导脚本。

## 影响

文档把 `bootstrap.ps1` 写成 Windows 首次使用入口。查找顺序、版本判定或技能根识别任一处
出错，用户首次使用就会失败，错误信息也未必足以定位。实机验收前不能对外声称 Windows
安装路径已经可用。

## 相关文件

- `skills/llm-wiki/scripts/bootstrap.ps1`（本次精简后 194 行）
- `skills/llm-wiki/scripts/install.py`（退出码 0/1/3，`--target auto`）
- `skills/llm-wiki/references/recovery.md`（`E_PYTHON` 指向「运行时与安装」小节）
- `README.md`、`docs/readme/install.md`、`skills/llm-wiki/SKILL.md`（Windows 入口描述）
- `docs/agent/verification.md`（验收结论落点）

## 候选方案

在 Windows 10/11 上按以下场景各跑一次，记录命令、退出码与 stdout/stderr：

1. 有 3.12+ 运行时（PATH 上的 `python` 或 `py` 启动器）：直接找到并放置技能。
2. 只有 Python 3.11：拒绝，返回 `E_PYTHON`，不拿旧版本去跑 `install.py`。
3. 无运行时时用 `-Python <python.exe>` 指定 agent 自带的解释器：可用则继续放置。
4. 无运行时时不带 `-Python`：`E_PYTHON`、退出码 1，stdout 只有单个 JSON 对象，
   `doc` 指向 `references/recovery.md`、`section` 为「运行时与安装」。
5. 没有任何已知技能根：`install.py` 返回 `E_NO_TARGET`、退出码 3；用 `-Target` 指定目录后成功。
6. `-DryRun` 只打印计划、不复制；`-Force` 生成 `llm-wiki.bak-<时间戳>` 备份目录。
7. 在 cmd 里执行 `powershell -NoProfile -ExecutionPolicy Bypass -File scripts\bootstrap.ps1 -DryRun`，
   确认参数与退出码原样传递。

根据结果修正脚本与文档。

## 待确认与验收

- [ ] Windows 11 + PowerShell 5.1 完成场景 1、4、6。
- [ ] Windows 11 + PowerShell 7 完成场景 1、4、6。
- [ ] 场景 2、3、5、7 至少各完成一次（场景 3 需要 agent 自带的运行时）。
- [ ] 实机输出补进 `docs/agent/verification.md`，或关闭本 issue 并在提交信息中说明。
