# 异常 `.wiki/lock` 会让加锁命令静默无限卡住

**状态：锁卡死已修（2026-11-20）；issue 保留跟踪普通 query 只读免锁与残余锁加固。**

## 2026-11-20 更新：普通 query 改走只读免锁

用户确认：常规非 task 的 `query` 按只读快照处理，原则上不加锁——搜到当下结果即可，下一秒
变化不影响判断。这条会消掉本 issue 里 `query` 的卡死症状，但不解决写命令的锁问题，issue
保留。

- 普通 `query` / `query --page`：只读打开账本，不跑 `ensure(push=True)`/`sync_view`，不建
  `.wiki` 目录、不补 schema、不创建 `.wiki/lock`；空工作区如何提示待定（建议明确报错
  要求先 `init`）。
- `query --task` 仍写 `queries.jsonl` 并做配额读改写，保留锁。
- 锁卡死修复后，下面三种异常锁不再让 `query` 卡住，会快速返回 `E_STATE`/`E_HEALTH`；
  但普通 `query` 仍走整命令锁，正常竞争时最多等 5 秒，只读免锁尚未实现。
- 锁的长期分层（硬租约、树级互斥、观看区单写者）见 `docs/agent/development-decisions.md`，
  不在本 issue 的验收范围内。

### 已落地（2026-11-20）

- `workspace_lock` 先判 deadline，再判/删陈旧锁；只接受普通文件，非普通文件直接报
  `E_STATE`；pid 用非阻塞、限长读取；陈旧锁 `unlink` 失败报 `E_STATE`；创建锁失败报
  `E_HEALTH`。
- `tests/selftest.py` 新增目录锁、FIFO、不可删陈旧锁、不可写 `.wiki` 四个用例，
  `python3 tests/selftest.py` 332/332 通过。
- 普通 `query` 只读免锁尚未实现，相关验收项仍未完成。

## 问题

修复前，`workspace_lock` 的 5 秒等待上限只在“锁存在且未判陈旧”这一支生效。一旦锁文件被判
陈旧、但 `unlink` 失败，循环会 `continue` 回到开头，**绕过 deadline 判断**，也不 sleep；
此时 `query`、`status`、`export` 等所有加锁命令无输出、无错误、无限运行。

根因集中在两处：

- 修复前的 `lock_is_stale` 把“读不出 pid”一律当成陈旧锁，包括 `read_text()` 抛出的
  `IsADirectoryError` 等异常；
- 删除陈旧锁时的 `unlink` 失败被 `except OSError: pass` 吞掉，随后直接 `continue`，
  而 deadline 检查在 `continue` 之后，永远轮不到。

已复现三种触发形态（2026-11-19，v0.5.3，macOS + Python 3.15.2）：

1. **`.wiki/lock` 是目录**：`read_text()` 抛 `IsADirectoryError` 被判陈旧，`unlink()` 删目录
   失败又被吞，进入无 sleep 紧循环。实测 `query 测试` 6 秒无输出，2 秒内子进程消耗约
   2.0 秒 CPU（满核空转）。
2. **锁文件不可删**：`.wiki` 目录只读、锁被 `chattr +i` / `chflags uchg` 固定、或锁属于
   其他用户时，pid 已死的锁会被反复判陈旧，`unlink` 一直失败。实测 `.wiki` 为 0555、
   `lock` 内容为 `999999 0`，`query` 6 秒无输出、持续空转。
3. **`.wiki/lock` 是 FIFO**：mtime 未超过 30 分钟时 `lock_is_stale` 会执行 `path.read_text()`，
   对 FIFO `open()` 会阻塞等待写端。实测 `query` 6 秒无输出、CPU 近似为 0，进程挂起。

复现入口是 `skills/llm-wiki/scripts/wiki.py`（`cli.py` 本身没有 `__main__`）：

```bash
tmp=$(mktemp -d)
mkdir -p "$tmp/.wiki/lock"     # 或 mkfifo "$tmp/.wiki/lock"
cd "$tmp" && python3 <repo>/skills/llm-wiki/scripts/wiki.py query 测试 --json
```

对照：同一入口在正常目录上返回 JSON 立即退出；新增用例之前的 selftest 只覆盖了
“持有锁 → `E_BUSY`”和“pid 失效 → 接管”，没有覆盖异常锁文件。

## 影响

- 修复前单次 `query` 就可能把流程无限期停住；没有任何错误输出，调用方只能靠外部超时杀
  进程。上层若自动重试，会放大成持续占用。
- 修复前目录锁形态满核空转，抬高机器负载；FIFO 形态 0% CPU，现场更难判断是死锁还是慢查询。
- 修复前恢复必须人工处理 `.wiki/lock`（`rmdir` / `rm` / 改权限）；普通使用者看到的现象
  只是“query 没反应”。

以上卡死已在 2026-11-20 修复：目录/FIFO/不可删锁在 0.1 秒内报 `E_STATE`，`.wiki` 不可写
报 `E_HEALTH`。普通 `query` 仍会拿整命令锁，正常竞争时最多等 5 秒；只读免锁是另一项。
- 排查文档的口径曾与代码不一致：[recovery.md:124](../../skills/llm-wiki/references/recovery.md#L124-L125)
  原让用户在锁“超过十分钟没动”时删除，而代码的陈旧阈值是 30 分钟
  （[workspace.py:19](../../skills/llm-wiki/scripts/workspace.py#L19)、[cli.md:59](../../docs/readme/cli.md#L59)、
  [engine.md:157](../../docs/agent/engine.md#L157)），人工处置时容易误判；2026-11-20 已统一为三十分钟。

## 相关文件

- `skills/llm-wiki/scripts/workspace.py`（`LOCK_*` 常量、`lock_is_stale`、`workspace_lock`）
- `skills/llm-wiki/scripts/cli.py`（`LOCKED_COMMANDS`、`main()` 的加锁入口）
- `skills/llm-wiki/scripts/ledger.py`（`ensure` 的 `sync_view` 调用）
- `skills/llm-wiki/scripts/view.py`（`sync_view` 的写/删）
- `docs/agent/development-decisions.md`（2026-11-20 的 query 只读决策）
- `tests/selftest.py`（现有锁用例）
- `docs/readme/cli.md`、`docs/agent/engine.md`
- `skills/llm-wiki/references/recovery.md`、`skills/llm-wiki/SKILL.md`

## 候选方案

第 1-4、6 项已落地（见上方「已落地」）；第 5 项与 query 只读仍待定。目标不变：**任何分支
都必须在时限内给出结果**，异常锁文件不能变成静默等待或空转。

1. 把 deadline 判断移到 `FileExistsError` 分支开头，先判超时，再判/删陈旧锁；保证 5 秒后
   无论锁处于什么状态都有明确结果。
2. `lock_is_stale` 只接受普通文件：用 `os.lstat` + `stat.S_ISREG` 判定，非普通文件（目录、
   FIFO、设备）直接报 `E_STATE`，或按陈旧锁删除后重试，但绝不对它做 `read_text`。
3. 读 pid 时用 `O_NONBLOCK` 并限制字节数，避免 FIFO 或超大文件把进程挂住。
4. `unlink` 失败不再静默 `continue`：抛 `E_BUSY` / `E_STATE`，或 sleep 后重试并计入 deadline；
   同时可考虑删除前比对 inode/内容，避免接管者互删对方新建的锁。
5. 锁创建改为“临时文件写 pid + 原子 `link` 到 `.wiki/lock`”，消除“空文件被判陈旧”的窗口。
6. 同步文档：`recovery.md` 的十分钟口径已统一为三十分钟，并写清异常锁文件的 `E_STATE`/
   `E_HEALTH` 与人工恢复（2026-11-20）。

## 待确认与验收条件

- [x] `.wiki/lock` 为目录时，仍加锁的命令（如 `status`）在 5 秒内以非 0 退出并给出明确错误，
      不进入热循环
- [x] `.wiki` 不可写或锁不可删时，同样在 5 秒内报错，而不是无限重试
- [x] `.wiki/lock` 为 FIFO 时在 5 秒内报错，不阻塞在 `read_text`
- [x] 正常的崩溃锁（pid 已死、目录可写）仍被接管，现有 selftest 保持通过
- [x] 正常竞争仍是 5 秒后 `E_BUSY`
- [ ] 普通 `query` 在 `.wiki/lock` 为目录/FIFO/不可删时不被阻塞，也不创建 `.wiki/lock`
- [ ] 普通 `query` 不执行 `ensure(push=True)`/`sync_view`，不写观看区
- [ ] 普通 `query` 可在只读工作区返回结果，或按定案给出明确错误
- [ ] `query --task` 仍按写路径加锁，配额校验不变
- [x] `tests/selftest.py` 新增异常锁用例；`python3 -m py_compile skills/llm-wiki/scripts/*.py`
      与 `python3 tests/selftest.py` 通过
- [ ] `tests/selftest.py` 新增 query 只读用例，并保持 selftest 通过
- [x] `cli.md`、`engine.md`、`recovery.md`、`SKILL.md` 的锁说明与当前实现一致
- [ ] query 只读落地后，同步 `cli.md`、`SKILL.md`、`engine.md` 的 query 锁说明
