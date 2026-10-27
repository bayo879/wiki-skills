#!/usr/bin/env bash
# Copies a release binary into the skill package so the package can be synced
# to another agent as a self-contained directory.
#
#   ./scripts/vendor.sh                    vendor the host platform
#   ./scripts/vendor.sh <triple> <path>    vendor a binary cross-built for <triple>
#
# The result is skills/llm-wiki/bin/vendor/<triple>/llm-wiki. Binaries are
# stripped in place: the package is copied between machines, so its size matters
# more than local debugging symbols.
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDOR="$REPO_DIR/skills/llm-wiki/bin/vendor"

triple=${1:-}
if [ -z "$triple" ]; then
  triple=$(rustc -vV | sed -n 's/^host: //p')
fi
if [ -z "$triple" ]; then
  echo "无法确定 target triple：请显式传入，例如 ./scripts/vendor.sh aarch64-apple-darwin" >&2
  exit 2
fi

src=${2:-}
if [ -z "$src" ]; then
  case "$triple" in
    *windows*) src="$REPO_DIR/target/release/llm-wiki.exe" ;;
    *)         src="$REPO_DIR/target/release/llm-wiki" ;;
  esac
fi
if [ ! -f "$src" ]; then
  echo "找不到二进制 $src，先跑 cargo build --release" >&2
  exit 2
fi

case "$triple" in
  *windows*) name=llm-wiki.exe ;;
  *)         name=llm-wiki ;;
esac

mkdir -p "$VENDOR/$triple"
dst="$VENDOR/$triple/$name"
cp "$src" "$dst"
chmod 755 "$dst"

# Best effort: a stripped binary saves several megabytes per platform, and
# nothing in this project reads its own symbols.
if command -v strip >/dev/null 2>&1; then
  strip -x "$dst" 2>/dev/null || true
fi

printf 'vendored %s -> %s (%s)\n' "$triple" "${dst#"$REPO_DIR"/}" "$(du -h "$dst" | cut -f1)"
