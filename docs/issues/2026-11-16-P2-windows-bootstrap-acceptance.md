# Windows 引导脚本尚未实机验收

## 问题

`skills/llm-wiki/scripts/bootstrap.ps1` 与 `bootstrap.cmd` 已加入，用于在 Windows 上检测/
安装 Python 3.12、按国内镜像下载 Python 3.12.8，并调用 `install.py` 放置技能。当前开发
环境是 macOS，没有 PowerShell，无法执行脚本；因此以下行为都还没有实机证据：

- `py -3.12` / `python` / `python3` 的检测与 3.12 版本判断；
- 华为云、清华 TUNA、npmmirror、官方源的下载与 MZ 校验；
- per-user 静默安装、PATH 刷新、`winget` 回退；
- `install.py --target auto` 在 Windows 技能根下的放置结果。

`tests/selftest.py` 只覆盖 Python 引擎，不覆盖 PowerShell 引导脚本；`docs/agent/verification.md`
的 40/40 不能作为 Windows 可用的依据。

## 影响

文档已把 `bootstrap.ps1` 写成 Windows 首次使用入口。若镜像 URL 失效、安装器被 SmartScreen
拦截、PATH 未刷新或技能根不存在，用户会在首次安装时失败，且错误信息可能不足以定位。
在实机验收前，不能对外声称 Windows 安装路径已经可用。

## 候选方案

在 Windows 10/11 上按以下场景各跑一次，记录命令、退出码和 JSON 输出：

1. 机器没有 Python：完整下载、静默安装、刷新 PATH、放置技能。
2. 只有 Python 3.11（低于 3.12 基线）：不误用旧版本，仍安装 3.12.8。
3. 已有 Python 3.12：跳过安装，直接放置。
4. 没有任何已知技能根：返回 `E_NO_TARGET`，并验证 `-Target` 指定目录可成功。
5. 断网/镜像 404：按顺序回退，最终给出可执行的手动安装提示。
6. `-DryRun`：不下载、不复制，只输出计划。

根据结果修正脚本和文档；若某些镜像长期不可用，调整镜像顺序或版本。

## 待确认与验收

- [ ] Windows 11 + PowerShell 5.1 完成场景 1、4、6。
- [ ] Windows 11 + PowerShell 7 完成场景 1、4、6。
- [ ] 场景 2、3、5 至少各完成一次。
- [ ] 实机输出补进 `docs/agent/verification.md`，或把本 issue 关闭并在提交信息中说明。
- [ ] 确认 `bootstrap.cmd` 双击和带参数调用都能正确传递退出码。
