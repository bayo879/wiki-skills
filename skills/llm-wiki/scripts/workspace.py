"""Filesystem boundary: wiki operations stay inside the session workspace."""

import atexit
import errno
import os
import shutil
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path
from stat import S_ISREG

from errors import WikiError

WIKI_DIR = ".wiki"
TMP_SUBDIR = "tmp"
LOCK_NAME = "lock"
LOCK_WAIT_SECONDS = 5.0
LOCK_POLL_SECONDS = 0.05
# Windows 没有安全的进程存活探测：崩掉的锁只能靠时间判断，宁可长一点。
LOCK_STALE_SECONDS = 30 * 60
# 锁里读不出持有者身份（旧版本遗留的空锁，或文件系统不支持硬链接时的兜底窗口）
# 不等于没人持有：先按占用处理，超过这个宽限期才认作陈旧。
LOCK_EMPTY_GRACE_SECONDS = 10.0
# `os.link` 在这些 errno 上表示文件系统不支持硬链接，可以退回独占创建。
_UNSUPPORTED_LINK_ERRNOS = tuple(
    value for value in (
        getattr(errno, name, None)
        for name in ("EPERM", "EOPNOTSUPP", "ENOSYS", "EXDEV", "EMLINK")
    )
    if value is not None
)


def window_safe(root, candidate):
    try:
        candidate.relative_to(root)
        return True
    except ValueError:
        return False


def inside_root(root, path, what, next_step):
    """Resolve a command argument and reject anything outside the wiki root."""
    candidate = Path(str(path))
    if not candidate.is_absolute():
        candidate = root / candidate
    candidate = candidate.resolve()
    if not window_safe(root, candidate):
        raise WikiError("E_PATH", f"{what} escapes the workspace root", next_step, 2)
    return candidate


def dry_store(root):
    """Throwaway store under `<root>/.wiki/tmp/`, removed when the process exits."""
    wiki_dir = root / WIKI_DIR
    existed = wiki_dir.exists()
    parent = wiki_dir / TMP_SUBDIR
    parent.mkdir(parents=True, exist_ok=True)
    store = Path(tempfile.mkdtemp(prefix="dry-", dir=str(parent)))
    atexit.register(cleanup_dry_store, store, root, existed)
    return store


def cleanup_dry_store(store, root, wiki_existed):
    shutil.rmtree(store, ignore_errors=True)
    tmp_dir = root / WIKI_DIR / TMP_SUBDIR
    try:
        tmp_dir.rmdir()
    except OSError:
        return
    if not wiki_existed:
        try:
            (root / WIKI_DIR).rmdir()
        except OSError:
            pass


def lock_path(root):
    return Path(root) / WIKI_DIR / LOCK_NAME


def _pid_alive(pid):
    """Whether ``pid`` still runs; Windows can only answer "unknown" (True)."""
    if os.name != "posix":
        return True
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    except (OverflowError, ValueError):
        return False
    except OSError:
        return False
    return True


def _read_lock_pid(path):
    """Return the recorded pid, or None when the lock carries no usable record."""
    flags = os.O_RDONLY
    if hasattr(os, "O_NONBLOCK"):
        flags |= os.O_NONBLOCK
    handle = os.open(path, flags)
    try:
        data = os.read(handle, 64)
    finally:
        os.close(handle)
    try:
        return int(data.split()[0])
    except (ValueError, IndexError):
        return None


def lock_is_stale(path, stat=None):
    if stat is None:
        try:
            stat = os.lstat(path)
        except OSError:
            return False
    if not S_ISREG(stat.st_mode):
        return False
    age = time.time() - stat.st_mtime
    if age > LOCK_STALE_SECONDS:
        return True
    try:
        pid = _read_lock_pid(path)
    except OSError:
        return True
    if pid is None:
        # 没有身份记录不等于没人持有：先给创建者宽限期，别把刚建好的锁删掉。
        return age > LOCK_EMPTY_GRACE_SECONDS
    return pid <= 0 or not _pid_alive(pid)


def _lstat_or_none(path):
    try:
        return os.lstat(path)
    except OSError:
        return None


def _publish_lock(path):
    """Create `path` with its owner record already inside.

    POSIX 不能在独占创建的同时写入内容，所以身份记录先写进一个私有临时名，
    再用 `os.link` 原子发布：锁要么不存在，要么带着 pid，没有“存在但为空”的中间态。
    `link` 撞上已有名字抛 `FileExistsError`，与 `O_EXCL` 创建失败是同一个判决。
    """
    record = f"{os.getpid()} {int(time.time())}\n".encode("utf-8")
    temp = path.with_name(f"{path.name}.{os.getpid()}.{os.urandom(4).hex()}")
    try:
        handle = os.open(temp, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
        try:
            os.write(handle, record)
        finally:
            os.close(handle)
        try:
            os.link(temp, path)
        except OSError as error:
            # 文件系统不支持硬链接时退回独占创建；空内容窗口由 lock_is_stale 的宽限期兜住。
            if error.errno not in _UNSUPPORTED_LINK_ERRNOS:
                raise
            handle = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
            try:
                os.write(handle, record)
            finally:
                os.close(handle)
    finally:
        try:
            temp.unlink()
        except OSError:
            pass


def _sweep_lock_temp_files(path):
    """清掉创建者在 unlink 之前崩掉时留下的 `lock.<pid>.<rand>`。"""
    prefix = path.name + "."
    try:
        entries = list(path.parent.iterdir())
    except OSError:
        return
    now = time.time()
    for entry in entries:
        if not entry.name.startswith(prefix):
            continue
        parts = entry.name[len(prefix):].split(".")
        if len(parts) != 2 or not parts[0].isdigit() or len(parts[1]) != 8:
            continue
        try:
            stat = os.lstat(entry)
            if S_ISREG(stat.st_mode) and now - stat.st_mtime > LOCK_STALE_SECONDS:
                entry.unlink()
        except OSError:
            continue


@contextmanager
def workspace_lock(root, wait=None):
    """Serialize wiki writers with an exclusive `.wiki/lock` file.

    The lock is published by hard-linking a temp file that already holds the
    owner record, so the file never exists without a pid: every process,
    including a second copy of the CLI, either fails the link or reads the
    owner. A lock whose owner died is broken once, using the recorded pid on
    POSIX and an age threshold where that check is unsafe.
    """
    path = lock_path(root)
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
    except OSError as error:
        raise WikiError(
            "E_HEALTH",
            f"workspace is not writable: {error}",
            "run the command from a writable directory inside the workspace",
            1,
            section="工作区不可写",
        ) from error
    deadline = time.monotonic() + (LOCK_WAIT_SECONDS if wait is None else wait)
    while True:
        # 锁已存在时先不建临时文件：直接进占用分支，才能把“删不掉的陈旧锁”报成
        # E_STATE，而不是把目录不可写误报成 E_HEALTH。
        if _lstat_or_none(path) is None:
            try:
                _publish_lock(path)
            except FileExistsError:
                pass
            except OSError as error:
                if _lstat_or_none(path) is None:
                    raise WikiError(
                        "E_HEALTH",
                        f"workspace is not writable: {error}",
                        "run the command from a writable directory inside the workspace",
                        1,
                        section="工作区不可写",
                    ) from error
            else:
                break

        # 先看时限：锁一直存在时，任何后续分支都不能绕过 deadline。
        if time.monotonic() >= deadline:
            raise WikiError(
                "E_BUSY",
                "another wiki process is working in this workspace",
                "wait for it to finish; if it crashed, delete .wiki/lock",
                4,
            )

        try:
            stat = os.lstat(path)
        except FileNotFoundError:
            continue
        except OSError as error:
            raise WikiError(
                "E_STATE",
                f"cannot inspect workspace lock: {error}",
                "remove .wiki/lock if no wiki process is running",
                3,
                section="工作区被占用",
            ) from error

        # 目录、FIFO、设备文件都不是锁；绝不 read_text，也不尝试删除。
        if not S_ISREG(stat.st_mode):
            raise WikiError(
                "E_STATE",
                f"workspace lock is not a regular file: {path}",
                "remove .wiki/lock if no wiki process is running",
                3,
                section="工作区被占用",
            )

        if lock_is_stale(path, stat):
            try:
                current = os.lstat(path)
            except FileNotFoundError:
                continue
            except OSError as error:
                raise WikiError(
                    "E_STATE",
                    f"cannot inspect workspace lock: {error}",
                    "remove .wiki/lock if no wiki process is running",
                    3,
                    section="工作区被占用",
                ) from error
            identity = (stat.st_dev, stat.st_ino, stat.st_mtime_ns, stat.st_size)
            if (current.st_dev, current.st_ino, current.st_mtime_ns,
                    current.st_size) != identity:
                # 判陈旧期间锁已被替换，重新走一遍，避免删掉新持有者的锁。
                continue
            try:
                path.unlink()
            except FileNotFoundError:
                continue
            except OSError as error:
                raise WikiError(
                    "E_STATE",
                    f"cannot remove stale workspace lock: {error}",
                    "fix the lock file permissions or remove .wiki/lock manually",
                    3,
                    section="工作区被占用",
                ) from error
            continue

        time.sleep(LOCK_POLL_SECONDS)
    _sweep_lock_temp_files(path)
    try:
        yield
    finally:
        try:
            path.unlink()
        except OSError:
            pass
