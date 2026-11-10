# 锁只罩住 ingest 与 done，其余写库命令不受约束

登记：2026-11-10　状态：未完成　优先级：P2

## 问题

`lock::acquire` 只在 `ingest`（`src/sync.rs:139`）与 `done`（`src/main.rs:505`）里调用。
`query` 会写 FTS 与 `page_meta`，`doctor --fix` 会改布局、索引与页面，`ignore` 会写
`.wiki/ignore`，三者都不取锁；`store.rs` 只设置 WAL 与 `foreign_keys`，没有
`busy_timeout`（SQLite 默认 0）。

于是 `ingest` 持锁期间跑 `query`，撞到写锁会直接 `SQLITE_BUSY` 失败，而不是排队。
文档里「CLI 锁约束工具命令」的表述比实现宽。

另一处相关的口径不一致：`done` 取锁失败时非 `E_LOCKED` 的错误映射到 exit 2
（`src/main.rs:505-517`），而 `done` 运行期读文件的 IO 错误映射到 exit 1
（`src/main.rs:532-540`）。README 的退出码表把「环境错误」写在 2，实际同一类错误
有两种出口。

## 影响

- 并发时 `query`/`doctor --fix` 可能随机失败，容易被读成数据损坏。
- 退出码不一致会让脚本误判「用法错误」与「环境错误」。

## 候选方案（未定案）

1. 所有写库命令共用同一把仓库锁；只读命令也取共享锁。
2. 只设 `busy_timeout`（例如 5s），让 SQLite 自己排队；锁仍只保护多步写的命令。
3. 两者都做：锁保证跨步骤顺序，`busy_timeout` 兜住漏网的短写。

## 待确认

- `query` 是高频只读命令，取锁是否会让日常查询排队到不可接受。
- 退出码表的「环境错误 = 2」是否要按命令细化，还是统一把 IO 失败改为 1。

## 验收条件

- [ ] 两个进程同时 ingest + query 不出现 `SQLITE_BUSY`，或明确返回可重试的错误。
- [ ] 锁与 IO 的退出码在 README、`report.rs` 与测试里是同一套口径。
