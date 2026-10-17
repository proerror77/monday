#!/usr/bin/env bash
# Preserve source companions while bounding independent ShellCheck groups.
set -euo pipefail
root=$(cd -- "$(dirname -- "$0")/../.." && pwd)
if [[ ${1:-} == --root && $# == 2 ]]; then
  root=$(cd -- "$2" && pwd)
elif (( $# != 0 )); then
  printf 'usage: %s [--root directory]\n' "$0" >&2
  exit 2
fi
groups="$root/.github/scripts/collector-shellcheck-groups.json"
cd -- "$root/rust_hft"
scripts=(../deployment/aliyun/*.sh)
[[ -f ${scripts[0]} ]] || { echo 'collector shell input set is empty' >&2; exit 1; }
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
jq -e 'type=="array" and length>0 and all(.[]; type=="array" and length>0
  and all(.[]; type=="string" and startswith("../deployment/aliyun/") and endswith(".sh")))' "$groups" >/dev/null
printf '%s\0' "${scripts[@]}" | sort -z >"$work/expected"
jq -j '.[][] | . + "\u0000"' "$groups" | sort -zu >"$work/actual"
cmp -s "$work/expected" "$work/actual" || {
  echo 'ShellCheck groups must cover the exact collector script set' >&2; exit 1;
}
began=$SECONDS
for script in "${scripts[@]}"; do bash -n "$script"; done
printf 'collector_shell_phase name=syntax files=%s elapsed_seconds=%s\n' \
  "${#scripts[@]}" "$((SECONDS - began))"
began=$SECONDS
# Keep the original cwd, source context and flags. No -x or warning suppression.
jq -c '.[]' "$groups" | while IFS= read -r group; do printf '%s\0' "$group"; done \
  | xargs -0 -P 4 -n 1 bash -c '
      mapfile -d "" -t scripts < <(jq -j ".[] | . + \"\\u0000\"" <<<"$1")
      shellcheck -- "${scripts[@]}"
    ' _
printf 'collector_shell_phase name=shellcheck workers=4 groups=%s files=%s elapsed_seconds=%s\n' \
  "$(jq length "$groups")" "${#scripts[@]}" "$((SECONDS - began))"
