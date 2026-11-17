"""Filesystem boundary: wiki operations stay inside the session workspace."""

import atexit
import os
import shutil
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path

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
    except OSError:
        return False
    return True


def lock_is_stale(path):
    try:
        stat = path.stat()
    except OSError:
        return False
    if time.time() - stat.st_mtime > LOCK_STALE_SECONDS:
        return True
    try:
        pid = int(path.read_text(encoding="utf-8").split()[0])
    except (OSError, ValueError, IndexError):
        return True
    return not _pid_alive(pid)


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
            if lock_is_stale(path):
                try:
                    path.unlink()
                except OSError:
                    pass
                continue
            if time.monotonic() >= deadline:
                raise WikiError(
                    "E_BUSY",
                    "another wiki process is working in this workspace",
                    "wait for it to finish; if it crashed, delete .wiki/lock",
                    4,
                )
            time.sleep(LOCK_POLL_SECONDS)
            continue
        break
    try:
        os.write(handle, f"{os.getpid()} {int(time.time())}\n".encode("utf-8"))
        os.close(handle)
        yield
    finally:
        try:
            path.unlink()
        except OSError:
            pass
