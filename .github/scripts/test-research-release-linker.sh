#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/links"
export MONDAY_RESEARCH_LINK_TIMINGS_DIR="$work/links" MONDAY_LINKER_TEST_LOG="$work/arguments"
cat >"$work/bin/time" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
while [[ $1 == --* ]]; do
  case $1 in
    --quiet) shift ;;
    --format) shift 2 ;;
    --output) output=$2; shift 2 ;;
    *) exit 91 ;;
  esac
done
status=0
"$@" || status=$?
printf '{"wall_seconds":0.25,"user_seconds":0.1,"system_seconds":0.02,"max_rss_kib":4096,"exit_code":%s}\n' "$status" >"$output"
exit "$status"
MOCK
cat >"$work/bin/clang" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$@" >"${MONDAY_LINKER_TEST_LOG:?}"
exit "${MONDAY_LINKER_TEST_EXIT:-0}"
MOCK
printf '#!/usr/bin/env bash\nexit 0\n' >"$work/bin/mold"
chmod +x "$work/bin/"*
export PATH="$work/bin:$PATH"
linker="$root/.github/scripts/research-release-linker.sh"
bash "$linker" 'object with spaces.o' -fuse-ld=lld -o 'output with spaces'
jq -se 'length==1 and .[0].linker=="mold" and .[0].output=="output with spaces" and .[0].exit_code==0 and .[0].max_rss_kib==4096' "$work/links/"*.json >/dev/null
printf '%s\n' 'object with spaces.o' -fuse-ld=lld -o 'output with spaces' -fuse-ld=mold >"$work/expected"
cmp "$work/expected" "$MONDAY_LINKER_TEST_LOG"
status=0
MONDAY_LINKER_TEST_EXIT=37 bash "$linker" failed.o -o failed || status=$?
[[ $status == 37 ]]
jq -se 'length==2 and any(.[]; .output=="failed" and .exit_code==37)' "$work/links/"*.json >/dev/null
if bash "$linker" --ld-path=/unadmitted/linker -o wrong; then
  echo 'linker path override accepted' >&2; exit 1
fi
[[ $(find "$work/links" -name '*.json' | wc -l) -eq 2 ]]
bash "$linker" first.o -o first & first=$!
bash "$linker" second.o -o second & second=$!
wait "$first"
wait "$second"
jq -se 'length==4 and any(.[]; .output=="first") and any(.[]; .output=="second")' "$work/links/"*.json >/dev/null
printf 'PASS: mold selection, exact argument forwarding, failure status and concurrent timing records\n'
