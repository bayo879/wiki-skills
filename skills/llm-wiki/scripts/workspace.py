"""Filesystem boundary: wiki operations stay inside the session workspace."""

import atexit
import shutil
import tempfile
from pathlib import Path

from errors import WikiError

WIKI_DIR = ".wiki"
TMP_SUBDIR = "tmp"


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
