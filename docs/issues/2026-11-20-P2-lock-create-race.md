# 锁创建到写入 pid 之间的窗口会被判成陈旧锁，两个写者可能同时进场

**状态：未修。**

## 问题

`workspace_lock` 用 `os.open(..., O_CREAT|O_EXCL|O_WRONLY)` 独占创建 `.wiki/lock`，但 pid
等到**抢到锁之后、离开循环时**才写入（[workspace.py:141](../../skills/llm-wiki/scripts/workspace.py#L141)
创建，[workspace.py:235-236](../../skills/llm-wiki/scripts/workspace.py#L235-L236) 写入）。
两条语句之间没有原子性，文件会短暂处于“存在且为空”的状态。

判陈旧这一侧把空内容当成陈旧：`_read_lock_pid` 读 0 字节后 `int(b"".split()[0])` 抛
`ValueError`，`lock_is_stale` 在
[workspace.py:113-114](../../skills/llm-wiki/scripts/workspace.py#L113-L114) 把它归入
“读不出 pid 就是陈旧”。于是另一个并发写者会走到
[workspace.py:201-231](../../skills/llm-wiki/scripts/workspace.py#L201-L231) 的接管分支，
把刚建好的锁删掉、自己再创建一把。

复现（2026-11-20，v0.5.3，macOS + Python 3.15.2）：先 `init`，再模拟“已 `O_EXCL` 创建、
尚未写 pid”的状态——建一个空的 `.wiki/lock`——然后跑 `status`：

```text
lock_is_stale(empty fresh lock) = True
second command exit: 0 | lock still exists: False
```

第二个命令把空锁判陈旧、删除并正常执行，第一个持有者仍在跑，两者都认为自己持锁。注意
反方向的检查不成立：`stat.st_mtime` 是新鲜的，30 分钟阈值救不了这个窗口；inode/mtime_ns/
size 比对也救不了，因为窗口里锁根本没被替换过。

窗口极短（创建到写入之间只有一次 `os.write` 的距离），但 `query --task`、`next`、`submit`
这类会改账本的命令一旦撞上就是并发写账本，不是简单的卡顿。

## 影响

- 两个写命令同时持有“锁”，`tasks` 状态、`queries.jsonl` 配额、观看区渲染都可能并发写。
  SQLite 自身能挡住部分损坏，但状态机上的“单页单任务”“租约唯一”这类约束靠锁而非事务
  兜底，撞上时表现为重复派发或配额被绕过，而不是立即报错。
- 窗口越短越难复现，属于“偶发、无日志”的一类问题，现场很难归因到锁；本次是靠直接构造
  空锁状态才稳定复现。
- 与已修复的“异常锁卡死”不同：那条是挂住，这条是静默失去互斥，后者更隐蔽。

## 相关文件

- `skills/llm-wiki/scripts/workspace.py`（`workspace_lock` 的创建/写入顺序、
  `lock_is_stale`、`_read_lock_pid`）
- `skills/llm-wiki/scripts/cli.py`（`LOCKED_COMMANDS`、`main()` 的加锁入口）
- `tests/selftest.py`（现有锁用例在「写命令排队」一节，未覆盖该窗口）

## 候选方案

未定案。方向是让“锁已存在”从第一刻起就等于“持有者已登记”：

1. **临时文件 + 原子 link**：写 `<root>/.wiki/lock.<pid>.<rand>` 写入 pid、`fsync` 后
   `os.link()` 到 `.wiki/lock`；`link` 失败即 `FileExistsError`，等价于抢锁失败。成功后删除
   临时名。`.wiki/lock` 从出现的那一瞬就带完整内容，无空文件窗口。
2. **创建即写内容，判陈旧不认空文件**：`os.open` 后立刻 `os.write`，同时把“空内容”从
   陈旧判据里摘出去——空文件在 `LOCK_POLL_SECONDS` 级别的时间窗内重试而不是直接接管，
   超时仍按陈旧处理。改动更小，但窗口没有真正消除，只是缩到一次轮询之内。
3. **创建时写全内容**：POSIX 下没法在 `O_EXCL` 创建的同时写数据，只能配合方案 1 的
   `link` 变体；Windows 可用 `CreateFile` + 先写后关，但要与现有跨平台路径分别处理。

倾向方案 1：语义最干净，`E_BUSY`/`E_STATE` 的错误码与现有 `lock_is_stale` 分支都不用改；
代价是多一个临时名，需要处理 `link` 成功后临时文件删除失败（残留 `lock.<pid>.<rand>`，
无害但不该堆积）。

## 待确认与验收条件

- [ ] 空内容的新鲜锁不再被立即判陈旧：构造该状态后，第二个写命令在 5 秒内以 `E_BUSY`
      退出，而不是接管并执行
- [ ] `.wiki/lock` 出现时即带 pid；不存在“存在但为空”的可观察中间态
- [ ] 正常崩溃锁（pid 已死、目录可写）仍被接管；正常竞争仍是 5 秒后 `E_BUSY`
- [ ] 异常锁（目录、FIFO、不可删陈旧锁、`.wiki` 不可写）仍按现有 `E_STATE`/`E_HEALTH`
      快速失败，已修的卡死行为不回归
- [ ] `tests/selftest.py` 补该窗口用例；`python3 -m py_compile skills/llm-wiki/scripts/*.py`
      与 `python3 tests/selftest.py` 通过
- [ ] `engine.md` 第 18 条与 `cli.md` 的锁说明同步为新创建方式
