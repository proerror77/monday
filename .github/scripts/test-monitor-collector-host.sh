#!/usr/bin/env bash
# shellcheck disable=SC2016
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
workflow="$script_dir/../workflows/monitor-collector-host.yml"

oidc_contract_failed=0
grep -Fqx '  id-token: write' "$workflow" || {
  printf 'collector monitor cannot request a GitHub OIDC token\n' >&2
  oidc_contract_failed=1
}
grep -Fqx "    if: github.ref == 'refs/heads/main'" "$workflow" || {
  printf 'collector monitor does not reject non-main OIDC use\n' >&2
  oidc_contract_failed=1
}
grep -Fqx '# OIDC issuer: https://token.actions.githubusercontent.com' "$workflow" || oidc_contract_failed=1
grep -Fqx '# OIDC audience: sts.aliyuncs.com' "$workflow" || oidc_contract_failed=1
grep -Fqx '# OIDC subject: repo:proerror77/monday:ref:refs/heads/main' "$workflow" || oidc_contract_failed=1
grep -Fqx '        uses: aliyun/configure-aliyun-credentials-action@1e5248c8d5d93a8781ac344a68e19a43341e79e6 # v1.1.0' "$workflow" || {
  printf 'collector monitor does not use the pinned Aliyun OIDC credential exchange\n' >&2
  oidc_contract_failed=1
}
grep -Fqx '          oidc-provider-arn: ${{ vars.ALIYUN_COLLECTOR_MONITOR_OIDC_PROVIDER_ARN }}' "$workflow" || oidc_contract_failed=1
grep -Fqx '          role-to-assume: ${{ vars.ALIYUN_COLLECTOR_MONITOR_ROLE_ARN }}' "$workflow" || oidc_contract_failed=1
grep -Fqx '          role-session-expiration: 900' "$workflow" || {
  printf 'collector monitor OIDC session is not fixed at 900 seconds\n' >&2
  oidc_contract_failed=1
}
grep -Fqx '          audience: sts.aliyuncs.com' "$workflow" || oidc_contract_failed=1
grep -Fqx '        id: aliyun-auth' "$workflow" || oidc_contract_failed=1
grep -Fqx '      - name: Record OIDC authentication failure' "$workflow" || oidc_contract_failed=1
grep -Fqx "        if: always() && steps.aliyun-auth.outcome == 'failure'" "$workflow" || oidc_contract_failed=1
grep -Fqx "          printf '%s\\n' 'Aliyun OIDC credential exchange failed before the collector health check.' >> \"\$GITHUB_STEP_SUMMARY\"" "$workflow" || oidc_contract_failed=1
if grep -Eqi '(ALIYUN|ALIBABA_CLOUD|ALIBABACLOUD)_ACCESS_KEY_(ID|SECRET)|ALICLOUD_(ACCESS_KEY|SECRET_KEY)|aliyun[[:space:]]+configure|--access-key-(id|secret)|"mode": "AK"|"access_key_(id|secret)"' "$workflow"; then
  printf 'collector monitor still depends on long-term Aliyun AccessKeys\n' >&2
  oidc_contract_failed=1
fi
command_contract_failed=0
grep -Fqx '  COMMAND_ID: ${{ vars.ALIYUN_COLLECTOR_MONITOR_COMMAND_ID }}' "$workflow" || {
  printf 'collector monitor does not bind a fixed Cloud Assistant CommandId\n' >&2
  command_contract_failed=1
}
grep -Fqx $'          if ! run_json=$(aliyun ecs InvokeCommand \\' "$workflow" || {
  printf 'collector monitor does not invoke the fixed Cloud Assistant command\n' >&2
  command_contract_failed=1
}
grep -Fqx $'            --RegionId "${{ env.REGION_ID }}" \\' "$workflow" || command_contract_failed=1
grep -Fqx $'            --InstanceId.1 "${{ env.INSTANCE_ID }}" \\' "$workflow" || command_contract_failed=1
grep -Fqx '            --CommandId "$COMMAND_ID" 2>&1); then' "$workflow" || command_contract_failed=1
grep -Fqx $'              --CommandId "$COMMAND_ID" \\' "$workflow" || command_contract_failed=1
if grep -Fq '${{ env.COMMAND_ID }}' "$workflow"; then
  printf 'collector monitor interpolates CommandId into the shell program\n' >&2
  command_contract_failed=1
fi
if grep -Eq 'RunCommand|CommandContent|ContentEncoding|KeepCommand|remote_script' "$workflow"; then
  printf 'collector monitor still exposes dynamic Cloud Assistant command execution\n' >&2
  command_contract_failed=1
fi
test "$oidc_contract_failed" -eq 0
test "$command_contract_failed" -eq 0

grep -Fqx '                  health_json=$(printf '\''%s'\'' "$output" | base64 --decode 2>/dev/null | jq -ce '\''select(type == "object")'\'' 2>/dev/null || true)' "$workflow" || {
  printf 'collector monitor does not parse the complete multiline health JSON\n' >&2
  exit 1
}
if grep -Fq "grep -m1 '^{'" "$workflow"; then
  printf 'collector monitor still truncates multiline health JSON at the opening brace\n' >&2
  exit 1
fi

decode_health_output() {
  printf '%s' "$1" | base64 --decode 2>/dev/null | jq -ce 'select(type == "object")' 2>/dev/null || true
}

classify_health_output() {
  local encoded_output=$1
  local health_json invocation_failed ok
  health_json=$(decode_health_output "$encoded_output")
  invocation_failed=0
  ok=unknown
  if [ -z "$health_json" ]; then
    invocation_failed=1
  elif ok=$(printf '%s\n' "$health_json" | jq -er 'if (.ok | type) == "boolean" then (.ok | tostring) else error(".ok must be boolean") end'); then
    :
  else
    invocation_failed=1
    ok=unknown
  fi
  printf '%s %s\n' "$ok" "$invocation_failed"
}

healthy_output=$(printf '%s\n' '{' '  "ok": true,' '  "breaches": []' '}' | base64 | tr -d '\n')
unhealthy_output=$(printf '%s\n' '{' '  "ok": false,' '  "breaches": ["disk warning"]' '}' | base64 | tr -d '\n')
test "$(classify_health_output "$healthy_output")" = 'true 0'
test "$(classify_health_output "$unhealthy_output")" = 'false 0'

for invalid_output in \
  '' \
  'not-base64' \
  "$(printf '%s' 'not-json' | base64 | tr -d '\n')" \
  "$(printf '%s' '{"ok":' | base64 | tr -d '\n')"; do
  test "$(classify_health_output "$invalid_output")" = 'unknown 1'
done

grep -Fqx '              echo "exit_code=invokecommand-error"' "$workflow"
grep -Fqx '              echo "exit_code=no-invoke-id"' "$workflow"

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
grep -Fqx "        if: steps.health.outputs.invocation_failed == '0' && steps.health.outputs.ok == 'false'" "$workflow"
grep -Fqx "        if: always() && steps.health.outputs.ok != 'true'" "$workflow"
grep -Fqx '        run: exit 1' "$workflow"
grep -Fqx "        if: steps.health.outputs.invocation_failed == '1'" "$workflow"

breach_alert=$(grep -nF '      - name: Open or append a needs-triage issue on breach' "$workflow" | cut -d: -f1)
monitor_alert=$(grep -nF '      - name: Open or append an issue when the monitor cannot run' "$workflow" | cut -d: -f1)
failure=$(grep -nF '      - name: Fail unhealthy monitor run' "$workflow" | cut -d: -f1)
test "$breach_alert" -lt "$failure"
test "$monitor_alert" -lt "$failure"

echo 'collector host monitor contract: ok'
