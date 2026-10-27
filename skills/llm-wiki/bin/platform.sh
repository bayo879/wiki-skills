#!/bin/sh
# Prints the release target triple for this machine, or explains why not.
#
# The skill package ships prebuilt binaries named after Rust target triples.
# Both the launcher and the installer ask this script which one to use, so the
# mapping lives in exactly one place.
set -eu

os=$(uname -s 2>/dev/null) || {
  echo "platform.sh: cannot detect the operating system (uname failed)" >&2
  exit 1
}
arch=$(uname -m 2>/dev/null) || {
  echo "platform.sh: cannot detect the CPU architecture (uname failed)" >&2
  exit 1
}

case "$os:$arch" in
  Darwin:arm64)              echo aarch64-apple-darwin ;;
  Darwin:x86_64)             echo x86_64-apple-darwin ;;
  Linux:x86_64|Linux:amd64)  echo x86_64-unknown-linux-gnu ;;
  Linux:aarch64|Linux:arm64) echo aarch64-unknown-linux-gnu ;;
  *)
    echo "platform.sh: no prebuilt binary for $os/$arch" >&2
    exit 1
    ;;
esac
