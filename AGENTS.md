# AGENTS

本页是项目开发指导的索引中心，与 [README](README.md) 的入口定位类似：提供查找方向，
不汇总全部事实。按本次工作读取相关主题，不默认加载全部指导文档。

## 按工作主题查找

- **确认开发边界或方向**：读 [开发方向与决策](docs/agent/development-decisions.md)。
  记录 skills-only 转型、账本为唯一事实源、保留自动镜像、不引入 OCR/联网等已定决策。
- **改引擎（`scripts/wiki.py`）**：读 [引擎内部](docs/agent/engine.md)，查任务状态机、
  epoch/lease、修订与渲染、drift 处理、错误码约定与单文件规模边界。
- **改技能文字、命令契约或输出格式**：读 [技能与契约维护](docs/agent/maintenance.md)，
  查 SKILL.md 与 references 的同步范围、`tests/selftest.py` 守卫、文档编辑的核验要求。
- **改文档或准备提交**：读 [技能与契约维护](docs/agent/maintenance.md)；
  文字编辑必须用"找不到原文就报错"的 `edit`，改完当轮检索核实。

## 其他入口

产品说明与使用从 [README](README.md) 进入；设计原则见
[docs/readme/design-principles.md](docs/readme/design-principles.md)；未决设计统一放
[docs/issues/](docs/issues/)，不能把候选方案写成已实现事实。
