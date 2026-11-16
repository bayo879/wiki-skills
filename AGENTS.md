# AGENTS

本页是项目开发指导的索引中心，与 [README](README.md) 的入口定位类似：提供查找方向，
不汇总全部事实。按本次工作读取相关主题，不默认加载全部指导文档。

## 按工作主题查找

- **确认开发边界或方向**：读 [开发方向与决策](docs/agent/development-decisions.md)。
  记录 skills-only 转型、账本为唯一事实源、保留自动镜像、不引入 OCR/联网等已定决策。
- **改引擎（`scripts/wiki.py`）**：读 [引擎内部](docs/agent/engine.md)，查任务状态机、
  epoch/lease、修订与渲染、drift 处理、错误码约定与单文件规模边界。
- **改话题探索或沉淀流程**：读 [话题沉淀](docs/readme/topics.md) 与内置场景
  `skills/llm-wiki/scenarios/话题探索/SCENARIO.md`，查 `scenario search` 的显式检索、
  `query` 的 `topics` 字段与 `ingest` 跳过 `话题/` 的约束。
- **改技能文字、命令契约或输出格式**：读 [技能与契约维护](docs/agent/maintenance.md)，
  查 SKILL.md 与 references 的同步范围、`tests/selftest.py` 守卫、文档编辑的核验要求。
- **改文档或准备提交**：读 [技能与契约维护](docs/agent/maintenance.md)；
  文字编辑必须用"找不到原文就报错"的 `edit`，改完当轮检索核实。

## 其他入口

产品说明与使用从 [README](README.md) 进入；设计原则见
[docs/readme/design-principles.md](docs/readme/design-principles.md)；未决设计统一放
[docs/issues/](docs/issues/)，不能把候选方案写成已实现事实。

## 概念边界：wiki、话题、素材、开发文档

- **wiki 正式层**：工作区 `.wiki/` 下的 raw、pages 与账本，由 `ingest`/`next`/`submit`
  维护；页面必须能回到 raw 引文。
- **话题（wiki 探索层）**：`话题/<主题>.md`，记录一次探索的主题、结论、依据、未决问题
  与下一步，`query` 可直接检索。它属于 wiki 内容，不是 `wiki-sources/` 素材；不参与
  `ingest`、不进账本、不要求逐字引文。
- **素材**：`wiki-sources/`（项目按需使用）是待摄入的原始材料，由 wiki 内部完成摄入、
  去重与知识页维护；素材不是 wiki 内容，也不等于话题。
- **开发文档**：`README.md`、`docs/readme/`、`docs/agent/`、`docs/issues/`，记录
  llm-wiki 本项目自己的产品事实、开发决策与待办。
- 四者不要互相替代：本项目产品事实与开发决策写开发文档，不写进话题；素材不要写成
  wiki 内容或话题；话题也不要当素材摄入。归属拿不准时先问用户。
