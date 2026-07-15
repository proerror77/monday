#!/usr/bin/env bash
set -euo pipefail

# Homebrew llvm/lld can be keg-only on macOS, so cargo's non-login shell will
# not see them unless we add the common formula paths ourselves.
for tool_bin in /opt/homebrew/opt/llvm/bin /usr/local/opt/llvm/bin /opt/homebrew/opt/lld/bin /usr/local/opt/lld/bin; do
  if [[ -d "$tool_bin" ]]; then
    export PATH="$tool_bin:$PATH"
  fi
done

if command -v clang >/dev/null 2>&1; then
  linker_driver="clang"
elif command -v cc >/dev/null 2>&1; then
  linker_driver="cc"
elif command -v gcc >/dev/null 2>&1; then
  linker_driver="gcc"
else
  echo "No C compiler available for linking" >&2
  exit 1
fi

if command -v mold >/dev/null 2>&1; then
  exec "$linker_driver" -fuse-ld=mold "$@"
fi

if command -v ld.lld >/dev/null 2>&1 || command -v ld64.lld >/dev/null 2>&1 || command -v lld >/dev/null 2>&1; then
  exec "$linker_driver" -fuse-ld=lld "$@"
fi

exec "$linker_driver" "$@"
