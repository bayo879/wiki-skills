# 开发方向与决策

本文件记录用户已确认的方向与关键取舍。新决策由用户拍板后追加；未确认的候选方案写进
`docs/issues/`，不要写在这里。

## 2026-11-13：转型为 skills-only

用户原话："这个项目要开始转型了，把重心放在skills内的能力开发上，把之前使用rust的设定
和坑点，可以借鉴的借鉴，不可以借鉴的删除，最终只会有python skill这版了"。

- 交付物收敛为 `skills/llm-wiki/`（Python，标准库）与 `skills/llm-wiki-session/`。
- 删除 Rust 源码、`output/` 发布包、`.github` 的 release 构建、`scripts/build-output.sh`。
- 归档：分支 `rust-final`、tag `rust-archive-2026-11-13`；旧解析器分支
  `fix/parser-content-reliability` 留 tag `rust-parser-archive`。
- 保留并已落地的借鉴项：
  - 解析严格性：PDF 先 `pdftotext` 后 `pypdf`，都不出文本进 `unsupported`；
  - `ingest.max_source_bytes` 超限分类为 `oversize`，不解析、不中止整批；
  - 来源变化/消失的漂移处理与 `stale` 状态；
  - 结构化错误码与 `error.next`；
  - `scan.ignore` 与 `ingest.batch` 配置键。
- 不借鉴：任务树（根/子任务与合并阶段）、模型执行器、单二进制分发、发布包目录约定。

## 2026-11-13：账本为源，镜像保留

- agent 不能直接写页面：写入必须经 `submit` 校验，或 `page import` 人工接管。
- `.wiki/pages/*.md` 作为自动镜像保留（用户："可以拿保留自动镜像吧"），用于浏览、diff、
  迁移；被外部修改时先存 `.wiki/drift/` 再按账本还原。
- `query`/`read page` 只读账本，不读镜像文件。

## 2026-11-13：任务协议

- 对外只暴露"任务 + 语义载荷 + 证据"，内部表结构、租约、epoch、哈希不写进技能文档。
- 任务只有 `route` 与 `page` 两种；一个来源一条 route 任务，一页一条 page 任务。
- todo 只作为显示层投影，账本是唯一进度权威；没有 todo 工具的运行时不受影响。
- 默认每批 20 条；租约 15 分钟；提交失败零写入并保留 `last_error`。
- 已取消 `reconcile` 任务类型：镜像漂移自动留证并还原，页面回滚用 `page restore`。

## 2026-11-13：环境边界

- 不 OCR、不下载模型/语言包、不联网、不装依赖。扫描件只报 `unsupported`。
- 触发事件：旧技能副本的 agent 为处理 9 份扫描件自行下载 `chi_sim.traineddata`，被用户
  中止。技能文档中"安装 OCR 后再摄入"的表述已删除，避免再次诱导。
