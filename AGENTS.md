# AGENTS

本页是项目开发指导的索引中心，与 [README](README.md) 的入口定位类似：提供查找方向，不汇总全部事实。按本次工作读取相关主题，不默认加载全部指导文档。具体组织规则见 [文档组织与提交检查](docs/agent/maintenance.md)。

## 按工作主题查找

- **确认开发边界或关键决策**：读 [开发方向详细决策](docs/agent/development-decisions.md)。涉及 CLI 与 agent 分工、任务树取代发布、单机边界、出参自解释及分发方向；开发方向由用户决定。
- **改账本、存储、索引、目录或并发写入**：读 [存储、写入边界与迁移](docs/agent/storage.md)，查唯一入口、数据保护、迁移和索引失效约束。
- **改摄入、来源处理或任务推进**：读 [摄入、任务推进与来源结算](docs/agent/ingest-tasks.md)，查收尾、漂移、失败恢复及基线规则。
- **改技能、命令或输出契约**：读 [技能、CLI 契约与文档维护](docs/agent/skills-and-contracts.md)，查分工、机器守卫及删改入口时的同步范围。
- **改文档或准备提交**：读 [文档组织与提交检查](docs/agent/maintenance.md)；文字编辑的核验要求见 [文档维护指导](docs/agent/skills-and-contracts.md#223-改文档用会报错的工具不用静默替换)。

## 其他入口

产品说明、部署与使用从 [README](README.md) 进入；模块结构查 [架构说明](docs/readme/architecture.md)，CLI 与技能分工查 [设计原则](docs/readme/design-principles.md)。未定案设计与待判别问题统一查 [未决问题目录](docs/issues/)，不能把候选方案当成已实现事实。
