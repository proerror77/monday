#!/usr/bin/env bash
# Use the admitted native linker and keep one timing record per invocation.
set -euo pipefail
command -v clang >/dev/null
command -v mold >/dev/null
directory=${MONDAY_RESEARCH_LINK_TIMINGS_DIR:?link timing directory required}
[[ -d $directory ]]
output=''
arguments=("$@")
for ((index = 0; index < ${#arguments[@]}; index++)); do
  case ${arguments[index]} in
    --ld-path|--ld-path=*) echo 'release linker path override is not admitted' >&2; exit 2 ;;
  esac
  if [[ ${arguments[index]} == -o && $((index + 1)) -lt ${#arguments[@]} ]]; then
    output=${arguments[index + 1]}
  fi
done
record=$(mktemp "$directory/link.XXXXXX")
status=0
LC_ALL=C command time --quiet \
  --format '{"wall_seconds":%e,"user_seconds":%U,"system_seconds":%S,"max_rss_kib":%M,"exit_code":%x}' \
  --output "$record" clang "$@" -fuse-ld=mold || status=$?
# Keep the original failure code even if a failed link produced no timing data.
if ! jq -es --arg output "$output" --argjson status "$status" \
  'if length==1 and (.[0]|type=="object") then
    .[0] + {driver:"clang",linker:"mold",output:$output,exit_code:$status}
   else error("missing linker timing") end' "$record" >"$record.json"; then
  [[ $status != 0 ]] || status=1
fi
rm -f "$record"
exit "$status"
