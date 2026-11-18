# AGENTS

本页是项目开发指导的索引中心，与 [README](README.md) 的入口定位类似：提供查找方向，
不汇总全部事实。按本次工作读取相关主题，不默认加载全部指导文档。

## 按工作主题查找

- **确认开发边界或方向**：读 [开发方向与决策](docs/agent/development-decisions.md)。
  记录 skills-only 转型、账本为唯一事实源、删除页面镜像、不引入 OCR/联网等已定决策。
- **改引擎（`scripts/wiki.py`）**：读 [引擎内部](docs/agent/engine.md)，查任务状态机、
  epoch/lease、修订与渲染、观看区推送、错误码约定与单文件规模边界。
- **改话题探索或沉淀流程**：读 [话题沉淀](docs/readme/topics.md) 与内置工作流
  `skills/llm-wiki/workflows/topic-exploration/WORKFLOW.md`，查 `workflow search` 的显式检索、
  `query` 的 `topics` 字段与 `ingest` 跳过 `topics/` 的约束。
- **改工作流目录、步骤契约或命令**：读 [可执行工作流](docs/readme/workflows.md) 与
  [引擎内部](docs/agent/engine.md)，查三目录优先级、`WORKFLOW.md` 回退、四类步骤标记与
  `workflow list/search/load` 的输出字段。
- **改技能文字、命令契约或输出格式**：读 [技能与契约维护](docs/agent/maintenance.md)，
  查 SKILL.md 与 references 的同步范围、`tests/selftest.py` 守卫、文档编辑的核验要求。
- **改文档或准备提交**：读 [技能与契约维护](docs/agent/maintenance.md)；
  文字编辑必须用"找不到原文就报错"的 `edit`，改完当轮检索核实。

## 其他入口

产品说明与使用从 [README](README.md) 进入；设计原则见
[docs/readme/design-principles.md](docs/readme/design-principles.md)；未决设计统一放
[docs/issues/](docs/issues/)，不能把候选方案写成已实现事实。

## 概念边界：wiki、话题、开发文档

- **wiki 正式层**：`.wiki/` 的账本，加上观看区（默认 `wiki/`）里的 `raw/`、`pages/`，
  由 `ingest`/`next`/`submit` 维护；页面必须能回到 raw 引文。
- **话题（wiki 探索层）**：`topics/<主题>.md`，记录一次探索的主题、结论、依据、未决问题
  与下一步，`query` 可直接检索。它属于 wiki 内容，不是正式知识页；不参与 `ingest`、
  不进账本、不要求逐字引文。
- **开发文档**：`README.md`、`docs/readme/`、`docs/agent/`、`docs/issues/`，记录
  llm-wiki 本项目自己的产品事实、开发决策与待办。
- 三者不要互相替代：产品事实与开发决策写开发文档，不写进话题；话题也不当知识页。
- **全局 `wiki-sources/` 与本项目无关**：它是全局 AGENTS 约定的跨项目开发解决方案沉淀
  处，供各开发项目复用；不是本项目的 wiki、话题或文档，讨论本项目时不要把它算作一层。
