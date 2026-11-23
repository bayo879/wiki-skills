#!/usr/bin/env python3
"""Place this skill directory into an agent's skill root.

The skill itself needs no installation: it is a directory with SKILL.md and a
stdlib-only script. This helper only copies it to a location an agent scans, so
the human does not have to know any runtime's layout.

Usage:
    python3 install.py --target auto
    python3 install.py --target ~/.agents/skills
    python3 install.py --all
    python3 install.py --target auto --dry-run --json
"""

import argparse
import json
import os
import shutil
import subprocess
import sys
import time
from pathlib import Path

sys.dont_write_bytecode = True

from runtime import require_python

require_python()

SKILL_NAME = "llm-wiki"

PROJECT_TARGETS = [".agents/skills", ".claude/skills", ".dsh/skills"]
GLOBAL_TARGETS = [
    "~/.dsh/skills",
    "~/.claude/skills",
    "~/.agents/skills",
    "~/.codex/skills",
    "~/.config/opencode/skills",
]
PRIORITY = PROJECT_TARGETS + GLOBAL_TARGETS


def skill_dir():
    return Path(__file__).resolve().parent.parent


def candidate_paths():
    paths = []
    for raw in PRIORITY:
        expanded = Path(os.path.expanduser(raw))
        if not expanded.is_absolute():
            expanded = Path.cwd() / expanded
        paths.append(expanded)
    return paths


def copy_skill(source, destination):
    if destination.exists():
        stamp = time.strftime("%Y%m%d-%H%M%S")
        backup = destination.with_name(f"{destination.name}.bak-{stamp}")
        destination.rename(backup)
    ignore = shutil.ignore_patterns("__pycache__", "*.pyc", ".DS_Store", ".git")
    shutil.copytree(source, destination, ignore=ignore)


def verify(destination):
    entry = destination / "scripts" / "wiki.py"
    completed = subprocess.run(
        [sys.executable, str(entry), "--version"],
        capture_output=True,
        text=True,
        check=False,
    )
    return completed.returncode == 0, (completed.stdout or completed.stderr).strip()


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--target", default="auto", help="auto, an explicit directory, or a parent skills dir")
    parser.add_argument("--all", action="store_true", help="install into every detected skill root")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--force", action="store_true", help="replace an existing copy (keeps a timestamped backup)")
    parser.add_argument("--json", action="store_true")
    args = parser.parse_args(argv)

    source = skill_dir()
    checked = candidate_paths()
    existing = [path for path in checked if path.is_dir()]

    if args.all:
        targets = existing
    elif args.target and args.target != "auto":
        explicit = Path(os.path.expanduser(args.target)).resolve()
        if explicit.name == SKILL_NAME:
            explicit = explicit.parent
        targets = [explicit]
    else:
        targets = existing[:1]

    installed, skipped, errors = [], [], []
    for parent in targets:
        destination = parent / SKILL_NAME
        if destination.exists() and not args.force:
            skipped.append(str(destination))
            continue
        if args.dry_run:
            installed.append(str(destination))
            continue
        try:
            parent.mkdir(parents=True, exist_ok=True)
            copy_skill(source, destination)
        except OSError as error:
            errors.append({"path": str(destination), "message": str(error)})
            continue
        ok, detail = verify(destination)
        if ok:
            installed.append(str(destination))
        else:
            errors.append({"path": str(destination), "message": f"copied but failed to start: {detail}"})

    if args.json:
        payload = {
            "ok": not errors and bool(installed or skipped),
            "skill": str(source),
            "checked": [str(path) for path in checked],
            "installed": installed,
            "skipped": skipped,
            "errors": errors,
        }
        if not payload["ok"] and not errors:
            payload["error"] = {
                "code": "E_NO_TARGET",
                "message": "no known agent skill directory exists",
                "next": "keep the directory where it is and load SKILL.md directly, or pass --target <skills dir>",
            }
        sys.stdout.write(json.dumps(payload, ensure_ascii=False) + "\n")
    else:
        for path in installed:
            sys.stdout.write(f"installed: {path}\n")
        for path in skipped:
            sys.stdout.write(f"already present: {path} (use --force to replace)\n")
        for item in errors:
            sys.stdout.write(f"error: {item['path']}: {item['message']}\n")
        if not installed and not skipped and not errors:
            sys.stdout.write(
                "no known agent skill directory found.\n"
                "Keep this directory as-is and load SKILL.md directly, or pass --target <skills dir>.\n"
            )
    if errors:
        return 1
    if not installed and not skipped:
        return 3
    return 0


if __name__ == "__main__":
    sys.exit(main())
