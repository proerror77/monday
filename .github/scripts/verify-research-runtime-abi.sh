#!/usr/bin/env bash
# Released Linux/amd64 executables must run in the existing bookworm runtime.
set -euo pipefail
directory=${1:?release binary directory required}
script_dir=$(cd "$(dirname "$0")" && pwd)
bash "$script_dir/verify-research-runner-binaries.sh" "$directory" "${2:-all}"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export LC_ALL=C
for binary in "$directory"/*; do
  readelf --file-header "$binary" > "$work/header"
  grep -Eq 'Class:[[:space:]]+ELF64' "$work/header"
  grep -Eq 'Machine:[[:space:]]+Advanced Micro Devices X86-64' "$work/header"
  readelf --version-info "$binary" > "$work/versions"
  if grep -Fq 'GLIBC_PRIVATE' "$work/versions"; then
    echo "research runtime ABI rejected: $(basename "$binary") requires private glibc symbols" >&2; exit 1
  fi
  while IFS=. read -r major minor _patch; do
    [[ $major =~ ^[0-9]+$ && $minor =~ ^[0-9]+$ ]]
    if ((major > 2 || (major == 2 && minor > 36))); then
      echo "research runtime ABI rejected: $(basename "$binary") needs GLIBC_${major}.${minor}; bookworm baseline is 2.36" >&2
      exit 1
    fi
  done < <(grep -Eo 'GLIBC_[0-9]+([.][0-9]+)+' "$work/versions" | sed 's/^GLIBC_//' | sort -u || true)
done
echo 'PASS: research release ELF64/amd64 requirements fit bookworm GLIBC_2.36; runtime smoke remains required'
