#!/usr/bin/env bash
# 安装 llm-wiki 二进制与 6 个 Claude Code skill。
#
#   ./scripts/install.sh               构建 release 二进制并安装
#   ./scripts/install.sh --skip-build  用已构建的 target/release/llm-wiki
#   ./scripts/install.sh --dry-run     只打印将要做什么，不动文件
#
# 可覆盖的环境变量：
#   CLAUDE_SKILLS_DIR  skill 安装目录，默认 ~/.claude/skills
#   WIKI_BIN_DIR       二进制安装目录，默认 ~/.local/bin
set -euo pipefail

REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SKILLS_SRC="$REPO_DIR/skills"
SKILLS_DST="${CLAUDE_SKILLS_DIR:-$HOME/.claude/skills}"
BIN_DST="${WIKI_BIN_DIR:-$HOME/.local/bin}"
BIN_NAME="llm-wiki"

SKIP_BUILD=0
DRY_RUN=0
for arg in "$@"; do
  case "$arg" in
    --skip-build) SKIP_BUILD=1 ;;
    --dry-run) DRY_RUN=1 ;;
    -h|--help) sed -n '2,12p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "未知参数：$arg" >&2; exit 2 ;;
  esac
done

run() {
  if [ "$DRY_RUN" = "1" ]; then
    printf '  [dry-run] %s\n' "$*"
  else
    "$@"
  fi
}

# 1. 校验 skill 源目录
if [ ! -d "$SKILLS_SRC" ]; then
  echo "找不到 $SKILLS_SRC，请在仓库根目录运行" >&2
  exit 2
fi

names=()
while IFS= read -r d; do
  names+=("$(basename "$d")")
done < <(find "$SKILLS_SRC" -mindepth 1 -maxdepth 1 -type d | sort)

if [ "${#names[@]}" -eq 0 ]; then
  echo "$SKILLS_SRC 下没有 skill 目录" >&2
  exit 2
fi

for n in "${names[@]}"; do
  if ! printf '%s' "$n" | grep -Eq '^[a-z0-9-]+$'; then
    echo "skill 目录名不合法：$n" >&2
    exit 2
  fi
  if [ ! -f "$SKILLS_SRC/$n/SKILL.md" ]; then
    echo "缺少 $SKILLS_SRC/$n/SKILL.md" >&2
    exit 2
  fi
done

# 2. 构建二进制
BIN_SRC="$REPO_DIR/target/release/$BIN_NAME"
if [ "$SKIP_BUILD" = "1" ]; then
  if [ ! -x "$BIN_SRC" ]; then
    echo "没有 $BIN_SRC，去掉 --skip-build 再跑" >&2
    exit 2
  fi
else
  echo "==> 构建 release 二进制"
  run cargo build --release --locked --manifest-path "$REPO_DIR/Cargo.toml"
fi

# 3. 安装二进制
echo "==> 安装 $BIN_NAME 到 $BIN_DST"
run mkdir -p "$BIN_DST"
run install -m 0755 "$BIN_SRC" "$BIN_DST/$BIN_NAME"

# 4. 安装 skill（先删旧目录，避免残留已删除的子文件）
echo "==> 安装 ${#names[@]} 个 skill 到 $SKILLS_DST"
run mkdir -p "$SKILLS_DST"
for n in "${names[@]}"; do
  echo "    - $n"
  run rm -rf "$SKILLS_DST/$n"
  run cp -R "$SKILLS_SRC/$n" "$SKILLS_DST/$n"
done

# 5. 收尾检查
echo
if command -v "$BIN_NAME" >/dev/null 2>&1; then
  echo "已就绪：$(command -v "$BIN_NAME")"
else
  echo "二进制已装到 $BIN_DST/$BIN_NAME，但不在 PATH 上。"
  echo "把这一行加进 shell 配置后重开终端："
  echo "    export PATH=\"$BIN_DST:\$PATH\""
fi
echo "skill 目录：$SKILLS_DST"
