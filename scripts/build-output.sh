#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"
# Refuse unknown files and symlinks recursively; never erase user data.
validate_tree() {
  local entry
  for entry in "$1"/* "$1"/.[!.]* "$1"/..?*; do
    [ -e "$entry" ] || [ -L "$entry" ] || continue
    [ ! -L "$entry" ] || { echo "Symlink in distribution: $entry" >&2; return 1; }
    case "$entry" in
      output/.wiki|output/.wiki/cli|output/.wiki/skills|output/.wiki/skills/llm-wiki|output/.wiki/skills/llm-wiki-session)
        [ -d "$entry" ] && validate_tree "$entry" || return 1 ;;
      output/.wiki/readme.md|output/.wiki/skills/llm-wiki/SKILL.md|output/.wiki/skills/llm-wiki/init.md|output/.wiki/skills/llm-wiki/content.md|output/.wiki/skills/llm-wiki/ops.md|output/.wiki/skills/llm-wiki/relations.md|output/.wiki/skills/llm-wiki-session/SKILL.md|output/.wiki/cli/llm-wiki|output/.wiki/cli/llm-wiki.exe)
        [ -f "$entry" ] || return 1 ;;
      *) echo "Unknown distribution entry: $entry" >&2; return 1 ;;
    esac
  done
}
[ -d output ] && [ ! -L output ]
validate_tree output
for required in output/.wiki/readme.md output/.wiki/skills/llm-wiki/{SKILL,init,content,ops,relations}.md output/.wiki/skills/llm-wiki-session/SKILL.md; do
  [ -f "$required" ] || { echo "Missing installation resource: $required" >&2; exit 1; }
done
host=$(rustc -vV | sed -n 's/^host: //p')
case "$host" in
  *windows*) binary=llm-wiki.exe ;;
  *) binary=llm-wiki ;;
esac
for entry in output/.wiki/cli/* output/.wiki/cli/.[!.]* output/.wiki/cli/..?*; do
  [ -e "$entry" ] || [ -L "$entry" ] || continue
  [ "$entry" = "output/.wiki/cli/$binary" ] && [ -f "$entry" ] && [ ! -L "$entry" ] || {
    echo "Unexpected CLI entry: $entry" >&2; exit 1;
  }
done
# Pin both host and destination so Cargo environment/config overrides cannot
# make us copy an old binary from a different build directory.
cargo build --release --locked --target "$host" --target-dir "$root/target"
mkdir -p output/.wiki/cli
cp "target/$host/release/$binary" "output/.wiki/cli/$binary"
chmod 755 "output/.wiki/cli/$binary"
