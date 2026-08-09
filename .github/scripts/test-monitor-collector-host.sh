#!/usr/bin/env bash
# shellcheck disable=SC2016
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
workflow="$script_dir/../workflows/monitor-collector-host.yml"

remote_assignment=$(sed -n '/^          remote_script=/,/^          content=/p' "$workflow" | sed '$d;s/^          //')
expected_remote_assignment=$(printf '%s\n' \
  "remote_script=\$(printf '%s\\n' \\" \
  "  '#!/usr/bin/env bash' \\" \
  "  'set -euo pipefail' \\" \
  "  'exec /opt/monday/bin/monday-collector-health.sh --json')")
if [ "$remote_assignment" != "$expected_remote_assignment" ]; then
  printf 'remote script is not assembled from three newline-delimited lines\n' >&2
  exit 1
fi

health_validation=$(sed -n \
  '/^          if \[ -n "\$health_json" \]; then$/,/^          echo "invocation_failed=/p' \
  "$workflow")
ok_filter='if (.ok | type) == "boolean" then (.ok | tostring) else error(".ok must be boolean") end'
grep -Fqx '            if ok=$(printf '\''%s\n'\'' "$health_json" | jq -er '\''if (.ok | type) == "boolean" then (.ok | tostring) else error(".ok must be boolean") end'\''); then' \
  <<<"$health_validation" || {
  printf 'health snapshot ok is not validated as a boolean\n' >&2
  exit 1
}
grep -Fqx '              invocation_failed=1' <<<"$health_validation" || {
  printf 'invalid health snapshot does not route to invocation_failed\n' >&2
  exit 1
}
test "$(printf '%s\n' '{"ok":true}' | jq -er "$ok_filter")" = true
test "$(printf '%s\n' '{"ok":false}' | jq -er "$ok_filter")" = false
for invalid_health in '{}' '{"ok":null}' '{"ok":"false"}'; do
  if printf '%s\n' "$invalid_health" | jq -er "$ok_filter" >/dev/null 2>&1; then
    printf 'invalid health snapshot passed boolean ok validation: %s\n' "$invalid_health" >&2
    exit 1
  fi
done

grep -Fqx '      - name: Fail unhealthy monitor run' "$workflow"
grep -Fqx "        if: always() && steps.health.outputs.ok != 'true'" "$workflow"
grep -Fqx '        run: exit 1' "$workflow"
grep -Fqx "        if: steps.health.outputs.invocation_failed == '1'" "$workflow"

breach_alert=$(grep -nF '      - name: Open or append a needs-triage issue on breach' "$workflow" | cut -d: -f1)
monitor_alert=$(grep -nF '      - name: Open or append an issue when the monitor cannot run' "$workflow" | cut -d: -f1)
failure=$(grep -nF '      - name: Fail unhealthy monitor run' "$workflow" | cut -d: -f1)
test "$breach_alert" -lt "$failure"
test "$monitor_alert" -lt "$failure"

echo 'collector host monitor contract: ok'
