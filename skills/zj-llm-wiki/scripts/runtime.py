"""Runtime baseline shared by the wiki entry points."""

import sys

MINIMUM = (3, 12)
MINIMUM_TEXT = "3.12"


def require_python():
    """Exit with a readable message when the interpreter is older than 3.12."""
    if sys.version_info < MINIMUM:
        found = "%d.%d.%d" % tuple(sys.version_info[:3])
        sys.stderr.write(
            "llm-wiki requires Python %s or newer; found %s\n"
            % (MINIMUM_TEXT, found)
        )
        raise SystemExit(2)
