#!/usr/bin/env bash
# Keep the complete static gate; bound independent ShellCheck work to four CPUs.
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")/../.." && pwd)
if [[ ${1:-} == --root && $# == 2 ]]; then
  root=$(cd -- "$2" && pwd)
elif (( $# != 0 )); then
  printf 'usage: %s [--root directory]\n' "$0" >&2
  exit 2
fi
cd -- "$root/rust_hft"
scripts=(../deployment/aliyun/*.sh)
[[ -f ${scripts[0]} ]] || { echo 'collector shell input set is empty' >&2; exit 1; }
began=$SECONDS
for script in "${scripts[@]}"; do bash -n "$script"; done
printf 'collector_shell_phase name=syntax files=%s elapsed_seconds=%s\n' \
  "${#scripts[@]}" "$((SECONDS - began))"
began=$SECONDS
# Each input is checked once. xargs retains a failing exit status after all workers.
printf '%s\0' "${scripts[@]}" | xargs -0 -P 4 -n 1 shellcheck --
printf 'collector_shell_phase name=shellcheck workers=4 files=%s elapsed_seconds=%s\n' \
  "${#scripts[@]}" "$((SECONDS - began))"
