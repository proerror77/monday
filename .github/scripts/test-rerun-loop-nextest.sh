#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/bin"
cat >"$work/bin/gh" <<'MOCK'
#!/usr/bin/env bash
set -euo pipefail
if [[ $1 == run ]]; then printf '%s\n' "$*" >"$FIXTURE/invocation"; exit; fi
case ${!#} in
  */jobs\?*) cat "$FIXTURE/jobs" ;;
  */artifacts\?*) cat "$FIXTURE/artifacts" ;;
  */runs/99) cat "$FIXTURE/run" ;;
  *) exit 91 ;;
esac
MOCK
chmod +x "$work/bin/gh"
export PATH="$work/bin:$PATH" FIXTURE="$work" GITHUB_REPOSITORY=proerror77/monday
printf '%s\n' '{"status":"completed","conclusion":"failure"}' >"$work/run"
printf '%s\n' '{"total_count":1,"jobs":[{"name":"Loop nextest archive","run_attempt":1,"conclusion":"success"}]}' >"$work/jobs"
printf '%s\n' '{"total_count":2,"artifacts":[{"name":"loop-nextest-archive-99-1","expired":false},{"name":"loop-nextest-expected-99-1","expired":false}]}' >"$work/artifacts"
bash "$root/.github/scripts/rerun-loop-nextest.sh" 99
grep -Fqx 'run rerun 99 --repo proerror77/monday --failed' "$work/invocation"
for mutation in '.artifacts[0].expired=true' '.artifacts|=map(select(.name!="loop-nextest-archive-99-1"))|.total_count=1' '.artifacts[1].expired=true'; do
  cp "$work/artifacts" "$work/original"
  jq "$mutation" "$work/original" >"$work/artifacts"
  bash "$root/.github/scripts/rerun-loop-nextest.sh" 99
  grep -Fqx 'run rerun 99 --repo proerror77/monday' "$work/invocation"
  mv "$work/original" "$work/artifacts"
done
rm "$work/invocation"
jq '.status="in_progress"' "$work/run" >"$work/edit"
mv "$work/edit" "$work/run"
if bash "$root/.github/scripts/rerun-loop-nextest.sh" 99; then exit 1; fi
[[ ! -e $work/invocation ]]
printf 'PASS: fresh producer partial retry, expired/missing archive or plan full producer retry, active run rejection\n'
