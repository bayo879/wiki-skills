# 断点注入：断电、写盘失败与读取期间并发变化

登记：2026-11-10　状态：未完成　优先级：P4

## 问题

现在的「转换与入队同事务」只用 SQLite 触发器验证过数据库内的失败：

- `tests/worklist.rs::failed_enqueue_rolls_back_conversion_baseline_and_can_retry` 用
  `CREATE TRIGGER fail_work BEFORE INSERT ON work_items ... RAISE(ABORT)` 注入入队失败，
  断言旧 `sources` / `work_items` 完整保留，解除故障后重试成功。

它覆盖不到下面三类，文档里也没有把它们说成已覆盖：

1. 进程在「raw 文件已落盘、账本还没登记」之间被杀（`src/convert.rs` 先 `write_atomic`
   写 raw，再调 `Ledger::record_conversion_input`）。
2. 文件系统写失败（磁盘满、只读挂载、rename 失败），而不是数据库写失败。
3. 扫描/转换读取源文件期间，文件被另一个进程改写或截断——`hash::read_source` 只在
   读前读后比较长度与 mtime，能否稳定拒绝取决于平台与写入方式，没有确定性注入验证。

## 影响

- 上述窗口的中断只能靠下一轮对账暴露（`doctor` 的 lint、`scan` 的 raw 对账），
  「对账能修」与「对账一定修得干净」不是同一句话。
- 若某类失败下待办丢失而源又不再报 `changed`，作者不会再收到这条来源的提醒。

## 候选方案（未定案）

- 在 `convert`/`sync` 的写盘边界抽一个可注入的 IO 层，测试里注入 `ENOSPC`、
  `EACCES` 与 rename 失败；不改生产路径的默认行为。
- 用子进程 + `kill -9` 在「raw 已写、账本未写」之间杀进程，重启后跑 `doctor` 与
  `ingest`，断言要么重新转换、要么待办可见，不出现静默丢失。
- 并发读取：一个进程持续改写源文件，另一个跑 `ingest`，断言不交付摘要与字节不匹配的
  解析产物（`Unstable` 报错是可接受的结局）。

## 待确认

- 故障注入是否值得进常规 `cargo test`（可能需要 Unix-only 的测试分组）。
- 断电窗口的承诺边界：只承诺「对账能发现」，还是要求「自动恢复」。

## 验收条件

- [ ] 三类故障各有至少一条确定性测试，且断言是「数据仍可恢复」，不是「命令退出码非零」。
- [ ] 文档按实际结论改写，不把对账能发现写成自动修复。
