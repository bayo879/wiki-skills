"""Filesystem boundary: wiki operations stay inside the session workspace."""

import atexit
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
    """Read the first token without blocking on a FIFO or slurping a huge file."""
    flags = os.O_RDONLY
    if hasattr(os, "O_NONBLOCK"):
        flags |= os.O_NONBLOCK
    handle = os.open(path, flags)
    try:
        data = os.read(handle, 64)
    finally:
        os.close(handle)
    return int(data.split()[0])


def lock_is_stale(path, stat=None):
    if stat is None:
        try:
            stat = os.lstat(path)
        except OSError:
            return False
    if not S_ISREG(stat.st_mode):
        return False
    if time.time() - stat.st_mtime > LOCK_STALE_SECONDS:
        return True
    try:
        pid = _read_lock_pid(path)
    except (OSError, ValueError, IndexError):
        return True
    return pid <= 0 or not _pid_alive(pid)


@contextmanager
def workspace_lock(root, wait=None):
    """Serialize wiki writers with an exclusive `.wiki/lock` file.

    The lock is a plain file created with O_EXCL so every process, including
    a second copy of the CLI, sees the same mutex. A lock whose owner died is
    broken once, using the recorded pid on POSIX and an age threshold where
    that check is unsafe.
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
        try:
            handle = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
        except FileExistsError:
            pass
        except OSError as error:
            # Windows 对已存在的目录可能报 EACCES 而不是 EEXIST；先看锁路径本身，
            # 非普通文件仍按损坏锁处理，避免误报成工作区不可写。
            try:
                existing = os.lstat(path)
            except OSError:
                existing = None
            if existing is not None and not S_ISREG(existing.st_mode):
                raise WikiError(
                    "E_STATE",
                    f"workspace lock is not a regular file: {path}",
                    "remove .wiki/lock if no wiki process is running",
                    3,
                    section="工作区被占用",
                ) from error
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
    try:
        os.write(handle, f"{os.getpid()} {int(time.time())}\n".encode("utf-8"))
        os.close(handle)
        yield
    finally:
        try:
            path.unlink()
        except OSError:
            pass
