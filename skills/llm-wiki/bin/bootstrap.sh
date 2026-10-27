#!/bin/sh
# Copies the package's prebuilt llm-wiki binary onto PATH.
#
#   sh bootstrap.sh            first run: install, then bare `llm-wiki` works
#   sh bootstrap.sh --force    refresh an older install after a package update
#
# Offline, no compiler, no sudo: it copies one file that already sits next to
# this script. Honours WIKI_BIN_DIR (default ~/.local/bin) and refuses to
# overwrite a working install unless --force is given.
set -eu

self=$0
case $self in
  */*) dir=$(CDPATH= cd -- "$(dirname -- "$self")" && pwd) ;;
  *)   dir=$(pwd) ;;
esac

force=0
for arg in "$@"; do
  case $arg in
    --force) force=1 ;;
    -h|--help) sed -n '2,8p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "bootstrap.sh: unknown argument: $arg" >&2; exit 2 ;;
  esac
done

if ! triple=$(sh "$dir/platform.sh"); then
  echo "bootstrap.sh: no prebuilt binary for this machine." >&2
  echo "bootstrap.sh: build one on it with \`scripts/vendor.sh\` in the source repository," >&2
  echo "bootstrap.sh: or ask whoever syncs the package for that platform's build." >&2
  exit 2
fi

src=$dir/vendor/$triple/llm-wiki
if [ ! -x "$src" ]; then
  echo "bootstrap.sh: package has no $triple binary (looked in $src)." >&2
  echo "bootstrap.sh: run \`scripts/vendor.sh $triple <path-to-binary>\` where the package is built." >&2
  exit 2
fi

# HOME is absent in some sandboxes and scheduled-task environments; failing with
# `HOME: unbound variable` would hide the real instruction, which is to set
# WIKI_BIN_DIR explicitly.
if [ -z "${WIKI_BIN_DIR:-}" ] && [ -z "${HOME:-}" ]; then
  echo "bootstrap.sh: neither WIKI_BIN_DIR nor HOME is set; export one of them first." >&2
  exit 2
fi

destdir=${WIKI_BIN_DIR:-${HOME:-}/.local/bin}
dest=$destdir/llm-wiki

if [ -x "$dest" ] && [ "$force" = 0 ] && "$dest" version >/dev/null 2>&1; then
  echo "llm-wiki is already installed at $dest (re-run with --force to refresh)"
  exit 0
fi

mkdir -p "$destdir"
cp "$src" "$dest"
chmod 755 "$dest"

if ! "$dest" version >/dev/null 2>&1; then
  echo "bootstrap.sh: copied $dest but it does not run on this machine." >&2
  echo "bootstrap.sh: the packaged $triple build may be for a different libc." >&2
  exit 2
fi

echo "installed llm-wiki -> $dest"
case ":${PATH:-}:" in
  *":$destdir:"*) ;;
  *) echo "note: $destdir is not on PATH; add it with:"; echo "  export PATH=\"$destdir:\$PATH\"" ;;
esac
