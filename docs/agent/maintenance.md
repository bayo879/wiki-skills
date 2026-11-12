# 文档组织与提交检查

项目级 `AGENTS.md` 是项目开发指导的索引中心，与 `README.md` 的入口定位类似：组织文档、提示按任务查找的方向，不罗列全部事实。根页只保留定位、必要的关键拐点与模块入口；详细决策和项目特有问题按主题放在 `docs/agent/`，需要时再读。用户决定开发方向，agent 不擅自增删；未定案问题仍放在 `docs/issues/`。产品使用与实现事实归 `README.md` / `docs/readme/`。

## 提交前自查

```bash
cargo test
cargo clippy --all-targets
cargo fmt --check
```

文档与代码同一次提交；只改文档的提交也要在 message 里说明为什么改。
