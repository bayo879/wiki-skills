# 技能与契约维护

文档体系与提交的通用规则见全局 `~/.dsh/AGENTS.md`；本文件只记录这个项目的同步范围与
守卫。

## 一处定义，多处引用

| 内容 | 唯一定义处 | 其他位置只引用 |
| --- | --- | --- |
| 命令、载荷字段、错误码、退出码 | `skills/llm-wiki/references/cli.md` | `SKILL.md` 给主循环与最小示例，不复制字段表 |
| agent 行为约束 | `skills/llm-wiki/SKILL.md` | `README.md` 只讲产品边界 |
| 页面质量与证据标准 | `skills/llm-wiki/references/curation.md` | — |
| 安装与目录约定 | `skills/llm-wiki/references/install.md` | `README.md` 给一行入口 |
| 故障与恢复 | `skills/llm-wiki/references/recovery.md` | — |
| 实测结果与未覆盖项 | `skills/llm-wiki/references/verification.md` | — |
| 内部实现约定 | `docs/agent/engine.md` | — |
| 已定方向 | `docs/agent/development-decisions.md` | — |

改命令、载荷、错误码时，至少同时检查：`references/cli.md`、`SKILL.md`、`selftest.py`、
`README.md` 中的示例。示例里的命令必须能直接跑通。

## 机器守卫

```bash
python3 -m py_compile skills/llm-wiki/scripts/*.py
python3 skills/llm-wiki/scripts/selftest.py
```

`selftest.py` 是端到端契约测试，覆盖 route/page 提交、epoch、镜像漂移、import/restore、
超限与 dry-run。改引擎行为时先补一个检查再改实现；它同时被 CI 调用，不允许跳过或
`--keep` 写入仓库。

新增行为若无法在临时目录验证（真实 PDF、OCR、并发），写进 `verification.md` 的
"未覆盖"，不要写成已验证。

## 改文档

- 用 `edit`（找不到原文就报错），不用 `sed`/`python` 静默替换；改完当轮用 `grep` 或
  `read` 核实新内容，再报告。
- 新增未决问题按 `docs/issues/YYYY-MM-DD-P<1-4>-<标题>.md`，包含问题、影响、候选方案、
  待确认与验收条件；方案确认且实现验收后删除文件，历史留在 Git。
- 不要把候选方案写进 README 或 `docs/readme/` 当作已实现能力。
- 纯文档提交在提交信息里说明原因。

## 提交

- 一个改动一个提交；行为变化与文档同步放在同一提交。
- 不提交 `.wiki/`、`__pycache__/`、`superpowers/`、本地语料或临时验证脚本。
- Rust 相关内容已删除；如果发现残留（`Cargo.toml`、`output/`、workflow 里的 release
  构建），先确认是否遗漏，不要把旧发布流程重新引入。