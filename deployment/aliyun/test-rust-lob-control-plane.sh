#!/usr/bin/env bash
# shellcheck disable=SC2016
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
CUTOVER="$SCRIPT_DIR/host-rust-lob-cutover.sh"
INVOKE="$SCRIPT_DIR/invoke-rust-lob-operation.sh"
POLICY="$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq"
RUNTIME_POLICY="$SCRIPT_DIR/rust-lob-runtime-health-policy.jq"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"

for command in awk base64 cut grep jq mktemp sed seq; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'missing test dependency: %s\n' "$command" >&2
    exit 2
  }
done

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

last_updated_ns=1
last_advance_mono=0
max_gap=0
health_sample_increments=0
for current_mono in $(seq 30 30 3600); do
  current_updated_ns=$((current_mono * 1000000000))
  read -r last_updated_ns last_advance_mono max_gap sample_increment < <(
    monday_observe_health_freshness \
      "$last_updated_ns" "$last_advance_mono" "$max_gap" \
      "$current_updated_ns" "$current_mono" 90
  )
  health_sample_increments=$((health_sample_increments + sample_increment))
done
((health_sample_increments == 120 && max_gap <= 90)) || {
  printf 'fresh one-hour health sequence did not pass the monotonic observer\n' >&2
  exit 1
}
if monday_observe_health_freshness \
  "$last_updated_ns" "$last_advance_mono" "$max_gap" \
  "$last_updated_ns" "$((last_advance_mono + 91))" 90 >/dev/null; then
  printf 'monotonic observer accepted a 91-second health freeze\n' >&2
  exit 1
fi

artifact=$(printf 'a%.0s' {1..64})
bundle=$(printf 'b%.0s' {1..64})
source_revision=$(printf 'c%.0s' {1..40})
catalog=$(printf 'd%.0s' {1..64})

market_json=$(jq -cn \
  --arg catalog "$catalog" \
  '{symbol_count:1200,snapshot_ready_count:1200,sequence_gaps:0,
    upload_failure_count:0,health_samples:121,max_health_silence_seconds:30,
    catalog_sha256:$catalog,
    session_id:"session-1",oss_roundtrips:2}')
jq -n \
  --arg artifact "$artifact" \
  --arg bundle "$bundle" \
  --arg source "$source_revision" \
  --argjson market "$market_json" \
  '{schema:"monday.rust_lob_shadow_gate.v2",candidate_sha256:$artifact,
    deployment_bundle_sha256:$bundle,deployment_source_revision:$source,
    passed:true,production_eligible:true,checks_passed:true,duration_seconds:3600,
    markets:{spot:$market,usdm:($market + {symbol_count:500,snapshot_ready_count:500})}}' \
  >"$tmp_dir/gate.json"

jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null

wrong_bundle=$(printf 'e%.0s' {1..64})
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$wrong_bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null; then
  printf 'gate policy accepted evidence from a different deployment bundle\n' >&2
  exit 1
fi

wrong_artifact=$(printf 'f%.0s' {1..64})
if jq -e \
  --arg candidate_sha256 "$wrong_artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null; then
  printf 'gate policy accepted evidence from a different binary artifact\n' >&2
  exit 1
fi

wrong_source=$(printf '9%.0s' {1..40})
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$wrong_source" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null; then
  printf 'gate policy accepted evidence from a different source revision\n' >&2
  exit 1
fi

jq '.markets.spot.health_samples = 1' "$tmp_dir/gate.json" >"$tmp_dir/short-sampling.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/short-sampling.json" >/dev/null; then
  printf 'gate policy accepted insufficient continuous health samples\n' >&2
  exit 1
fi

jq '.markets.usdm.max_health_silence_seconds = 91' \
  "$tmp_dir/gate.json" >"$tmp_dir/stale-health.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/stale-health.json" >/dev/null; then
  printf 'gate policy accepted a health freshness gap over 90 seconds\n' >&2
  exit 1
fi

jq -n '{status:"synced",sequence_gaps:0,symbol_count:1200,
  snapshot_ready_count:1200,pending_upload_segments:0,queue_saturated:false,
  disk_warning:false,upload_warning:false,updated_at_ns:200,session_id:"new-session"}' \
  >"$tmp_dir/runtime-health.json"
jq -e \
  --arg old_session old-session \
  --argjson minimum_symbols 1000 \
  --argjson minimum_updated_ns 100 \
  -f "$RUNTIME_POLICY" "$tmp_dir/runtime-health.json" >/dev/null
if jq -e \
  --arg old_session old-session \
  --argjson minimum_symbols 1000 \
  --argjson minimum_updated_ns 200 \
  -f "$RUNTIME_POLICY" "$tmp_dir/runtime-health.json" >/dev/null; then
  printf 'runtime policy accepted health that was not newer than restart\n' >&2
  exit 1
fi
if jq -e \
  --arg old_session new-session \
  --argjson minimum_symbols 1000 \
  --argjson minimum_updated_ns 100 \
  -f "$RUNTIME_POLICY" "$tmp_dir/runtime-health.json" >/dev/null; then
  printf 'runtime policy accepted a stale session\n' >&2
  exit 1
fi

rollback_body="$tmp_dir/rollback.sh"
sed -n '/^rollback_after_failure()/,/^}/p' "$CUTOVER" >"$rollback_body"
start_line=$(grep -n 'systemctl start "${PRODUCTION_UNITS\[@\]}"' "$rollback_body" | tail -1 | cut -d: -f1)
clear_line=$(grep -n 'clear_health_before_restart' "$rollback_body" | cut -d: -f1)
health_line=$(grep -n 'wait_for_release_health' "$rollback_body" | cut -d: -f1)
enable_line=$(grep -n 'systemctl enable "${PRODUCTION_UNITS\[@\]}"' "$rollback_body" | cut -d: -f1)
((clear_line < start_line && start_line < health_line && health_line < enable_line)) || {
  printf 'rollback no longer follows clear stale health -> start -> verify -> enable\n' >&2
  exit 1
}
grep -Fq 'runtime_matches_release "$OLD_BINARY" true' "$rollback_body"
grep -Fq '"$rollback_started_ns"' "$rollback_body"
grep -Fq 'previous-release-health-unverified-disabled' "$rollback_body"
grep -Fq 'systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}"' "$rollback_body"

mock_bin="$tmp_dir/bin"
mock_state="$tmp_dir/mock-state"
mkdir -p "$mock_bin" "$mock_state"
cat >"$mock_bin/aliyun" <<'MOCK_ALIYUN'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$MOCK_STATE_DIR/calls.log"
case "${1:-} ${2:-}" in
  'ecs RunCommand')
    printf '{"InvokeId":"mock-invoke"}\n'
    ;;
  'ecs DescribeInvocationResults')
    if [[ ${MOCK_TRANSIENT_ONCE:-0} == 1 && ! -f $MOCK_STATE_DIR/transient-seen ]]; then
      : >"$MOCK_STATE_DIR/transient-seen"
      exit 1
    elif [[ -f $MOCK_STATE_DIR/stopped && ${MOCK_IGNORE_STOP:-0} != 1 ]]; then
      status=Stopped
      exit_code=-1
    else
      status=${MOCK_STATUS:-Success}
      exit_code=${MOCK_EXIT_CODE:-0}
    fi
    printf '{"Invocation":{"InvocationStatus":"%s","ExitCode":"%s"}}\n' \
      "$status" "$exit_code"
    ;;
  'ecs StopInvocation')
    : >"$MOCK_STATE_DIR/stopped"
    printf '{}\n'
    ;;
  *)
    printf 'unexpected aliyun call: %s\n' "$*" >&2
    exit 2
    ;;
esac
MOCK_ALIYUN
cat >"$mock_bin/sleep" <<'MOCK_SLEEP'
#!/usr/bin/env bash
exit 0
MOCK_SLEEP
chmod +x "$mock_bin/aliyun" "$mock_bin/sleep"

common_env=(
  PATH="$mock_bin:$PATH"
  MOCK_STATE_DIR="$mock_state"
  ACTION=gate
  INSTANCE_ID=i-test123
  ARTIFACT_SHA256="$artifact"
  MONDAY_ALLOW_SHORT_OPERATION_TEST=1
  MONDAY_OPERATION_TEST_POLLS=2
  MONDAY_OPERATION_TEST_CANCEL_POLLS=2
)

run_commands_before=$(grep -c 'ecs RunCommand' "$mock_state/calls.log" 2>/dev/null || true)
if env \
  PATH="$mock_bin:$PATH" \
  MOCK_STATE_DIR="$mock_state" \
  ACTION=cutover \
  INSTANCE_ID=i-test123 \
  ARTIFACT_SHA256="$artifact" \
  MONDAY_OPERATION_TEST_POLLS=invalid \
  "$INVOKE" >"$tmp_dir/preflight.out" 2>&1; then
  printf 'operation wrapper accepted unauthorized test polling parameters\n' >&2
  exit 1
fi
run_commands_after=$(grep -c 'ecs RunCommand' "$mock_state/calls.log" 2>/dev/null || true)
[[ $run_commands_after == "$run_commands_before" ]] || {
  printf 'operation wrapper launched a remote command before validating test parameters\n' >&2
  exit 1
}

env "${common_env[@]}" MOCK_STATUS=Success MOCK_EXIT_CODE=0 "$INVOKE" \
  >"$tmp_dir/success.out"
grep -Fq 'gate completed successfully: mock-invoke' "$tmp_dir/success.out"

rm -f "$mock_state/stopped" "$mock_state/transient-seen"
env "${common_env[@]}" MOCK_TRANSIENT_ONCE=1 MOCK_STATUS=Success MOCK_EXIT_CODE=0 \
  "$INVOKE" >"$tmp_dir/transient.out"
grep -Fq 'gate completed successfully: mock-invoke' "$tmp_dir/transient.out"

rm -f "$mock_state/stopped"
if env "${common_env[@]}" MOCK_STATUS=PartialFailed MOCK_EXIT_CODE=1 "$INVOKE" \
  >"$tmp_dir/failed.out" 2>&1; then
  printf 'operation wrapper accepted PartialFailed\n' >&2
  exit 1
fi

rm -f "$mock_state/stopped"
if env "${common_env[@]}" MOCK_STATUS=Running MOCK_EXIT_CODE=0 "$INVOKE" \
  >"$tmp_dir/timeout.out" 2>&1; then
  printf 'operation wrapper accepted a locally timed-out invocation\n' >&2
  exit 1
fi
grep -Fq 'ecs StopInvocation' "$mock_state/calls.log"
grep -Fq 'invocation reached terminal state after cancellation: Stopped' "$tmp_dir/timeout.out"

rm -f "$mock_state/stopped"
if env "${common_env[@]}" MOCK_STATUS=Running MOCK_IGNORE_STOP=1 "$INVOKE" \
  >"$tmp_dir/unconfirmed.out" 2>&1; then
  printf 'operation wrapper accepted an unconfirmed cancellation\n' >&2
  exit 1
fi
grep -Fq 'invocation did not confirm cancellation' "$tmp_dir/unconfirmed.out"

printf 'Rust collector control-plane contracts passed\n'
