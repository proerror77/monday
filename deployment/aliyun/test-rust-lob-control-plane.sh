#!/usr/bin/env bash
# Dynamically sourced production functions consume fixture globals and mocks.
# shellcheck disable=SC1090,SC2016,SC2034,SC2154,SC2317,SC2329
set -euo pipefail

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
CUTOVER="$SCRIPT_DIR/host-rust-lob-cutover.sh"
RESTORE="$SCRIPT_DIR/host-rust-lob-restore.sh"
GATE="$SCRIPT_DIR/host-rust-lob-shadow-gate.sh"
SOAK="$SCRIPT_DIR/host-rust-lob-shadow-soak.sh"
INSTALL_RELEASE="$SCRIPT_DIR/deploy-rust-lob-release.sh"
CONTROLLER_RELEASE="$SCRIPT_DIR/host-rust-lob-controller-release.sh"
CONTROLLER_APPLY="$SCRIPT_DIR/host-rust-lob-controller-apply.sh"
SHADOW_UNIT="$SCRIPT_DIR/binance-lob-archiver-rust@.service"
INVOKE="$SCRIPT_DIR/invoke-rust-lob-operation.sh"
COLLECTOR_DOCKERFILE="$SCRIPT_DIR/../../rust_hft/deployment/docker/Dockerfile.binance-lob-archiver"
ARTIFACT_VERIFIER="$SCRIPT_DIR/../../rust_hft/data-pipelines/core/src/binance_market_tape_artifact.rs"
COLLECTOR="$SCRIPT_DIR/../../rust_hft/tools/collector/src/bin/binance-lob-archiver.rs"
LOB_ARCHIVER="$SCRIPT_DIR/../../rust_hft/tools/collector/src/lob_archiver.rs"
ACR_WORKFLOW="$SCRIPT_DIR/../../.github/workflows/acr-publish.yml"
POLICY="$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq"
RUNTIME_POLICY="$SCRIPT_DIR/rust-lob-runtime-health-policy.jq"
SHADOW_USDM_ENV="$SCRIPT_DIR/binance-lob-archiver-rust-usdm.env"
PRODUCTION_SPOT_ENV="$SCRIPT_DIR/binance-lob-archiver-production-spot.env"
PRODUCTION_USDM_ENV="$SCRIPT_DIR/binance-lob-archiver-production-usdm.env"
LIB="$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
# shellcheck disable=SC1090,SC1091
. "$LIB"
"$SCRIPT_DIR/test-rust-lob-shadow-soak.sh"
"$SCRIPT_DIR/test-rust-lob-controller-release.sh"
"$SCRIPT_DIR/test-rust-lob-controller-apply.sh"

psi_tmp_dir=$(mktemp -d)
trap 'rm -rf "$psi_tmp_dir"' EXIT
psi_fixture="$psi_tmp_dir/pressure"
printf 'some avg10=0.00 avg60=0.00 avg300=0.00 total=7\nfull avg10=0.00 avg60=0.00 avg300=0.00 total=100\n' \
  >"$psi_fixture"
[[ $(monday_memory_full_psi_total_us "$psi_fixture") == 100 ]]
read -r delta ratio hit consecutive < <(
  monday_memory_full_psi_window 100 150099 15000000 150000 0)
[[ $delta == 149999 && $ratio == 0.009999933 && $hit == false && $consecutive == 0 ]]
read -r delta ratio hit consecutive < <(
  monday_memory_full_psi_window 100 150100 15000000 150000 0)
[[ $delta == 150000 && $ratio == 0.010000000 && $hit == true && $consecutive == 1 ]]
read -r _ _ hit consecutive < <(
  monday_memory_full_psi_window 150100 300100 15000000 150000 "$consecutive")
read -r _ _ hit consecutive < <(
  monday_memory_full_psi_window 300100 450100 15000000 150000 "$consecutive")
[[ $hit == true && $consecutive == 3 ]]
read -r _ _ _ reset_hits < <(
  monday_memory_full_psi_window 300100 300101 15000000 150000 2)
[[ $reset_hits == 0 ]]
read -r _ _ _ consecutive < <(
  monday_memory_full_psi_window 450100 450101 15000000 150000 "$consecutive")
[[ $consecutive == 0 ]]
printf 'some avg10=0.00 avg60=0.00 avg300=0.00 total=7\n' >"$psi_fixture"
if monday_memory_full_psi_total_us "$psi_fixture" >/dev/null 2>&1; then
  printf 'memory PSI parser accepted a missing full row\n' >&2
  exit 1
fi
printf 'full avg10=0.00 avg60=0.00 avg300=0.00 total=bad\n' >"$psi_fixture"
if monday_memory_full_psi_total_us "$psi_fixture" >/dev/null 2>&1; then
  printf 'memory PSI parser accepted a non-integer total\n' >&2
  exit 1
fi
if monday_memory_full_psi_window 2 1 15000000 150000 0 >/dev/null 2>&1; then
  printf 'memory PSI window accepted a regressed total\n' >&2
  exit 1
else
  regression_status=$?
fi
[[ $regression_status == 2 ]]
rm -f "$psi_fixture"

monitor_body="$psi_tmp_dir/monitor"
sed -n '/^run_active_memory_psi_command()/,/^}/p' "$GATE" >"$monitor_body"
run_psi_kill_fixture() (
  # shellcheck disable=SC1090
  . "$monitor_body"
  marker="$psi_tmp_dir/cleanup"
  child_marker="$psi_tmp_dir/child-stopped"
  MEMORY_PSI_WINDOW_SECONDS=15
  MEMORY_PSI_SOURCE=/fixture
  MEMORY_PSI_FULL_DELTA_LIMIT_US=150000
  MEMORY_PSI_CONSECUTIVE_HIT_LIMIT=3
  memory_psi_phase=fixture
  sleep() { :; }
  monday_memory_full_psi_total_us() { printf '0\n'; }
  read_memory_psi_window() { return 1; }
  stop_fixture() { : >"$marker"; }
  blocking_child() {
    trap ': >"$child_marker"; exit 143' TERM
    while :; do sleep 1; done
  }
  if run_active_memory_psi_command stop_fixture blocking_child; then
    exit 1
  else
    status=$?
  fi
  [[ $status == 75 && -f $marker && -f $child_marker ]]
  rm -f "$marker" "$child_marker"
)
run_psi_kill_fixture 2>"$psi_tmp_dir/threshold"
grep -Fq 'memory full PSI exceeded' "$psi_tmp_dir/threshold"
run_psi_invalid_fixture() (
  # shellcheck disable=SC1090
  . "$monitor_body"
  MEMORY_PSI_WINDOW_SECONDS=15
  MEMORY_PSI_SOURCE=/fixture
  MEMORY_PSI_FULL_DELTA_LIMIT_US=150000
  MEMORY_PSI_CONSECUTIVE_HIT_LIMIT=3
  memory_psi_phase=fixture
  sleep() { :; }
  monday_memory_full_psi_total_us() { printf '0\n'; }
  read_memory_psi_window() { return 2; }
  stop_fixture() { :; }
  blocking_child() { while :; do sleep 1; done; }
  if run_active_memory_psi_command stop_fixture blocking_child; then
    exit 1
  else
    status=$?
  fi
  [[ $status == 76 ]]
)
run_psi_invalid_fixture 2>"$psi_tmp_dir/invalid"
grep -Fq 'missing, invalid, or regressed' "$psi_tmp_dir/invalid"
rm -rf "$psi_tmp_dir"
trap - EXIT
grep -Fq 'begin_memory_psi_phase "shadow-$market"' "$GATE"
grep -Fq 'run_memory_psi_phase_command "upload-drain-$market"' "$GATE"
grep -Fq 'run_memory_psi_phase_command "strict-verifier-$strict_verifier_counter"' "$GATE"
grep -Fq 'begin_memory_psi_phase "oss-roundtrip-$market"' "$GATE"
[[ $(monday_rust_lob_runtime_contract_sha256 "$SCRIPT_DIR") \
  == 1a9618e19552f482d83789580bd82b0ae4a59adb875f477133230a3fd3031dcd ]]
if grep -Eq 'polymarket|upload.*timer|timer.*upload' "$GATE"; then
  printf 'LOB Gate unexpectedly references Polymarket or external upload timers\n' >&2
  exit 1
fi

required_memory=11559501824
[[ $(monday_shadow_memory_admission \
  "$required_memory" 1073741824 10485760000 0) == "$required_memory" ]] || {
  printf 'shadow gate memory admission rejected exact headroom\n' >&2
  exit 1
}
if monday_shadow_memory_admission \
  "$((required_memory - 1))" 1073741824 10485760000 0 >/dev/null; then
  printf 'shadow gate memory admission accepted one byte below the requirement\n' >&2
  exit 1
fi
if rejected_required=$(monday_shadow_memory_admission \
  "$((required_memory - 1))" 1073741824 10485760000 0); then
  printf 'shadow gate memory admission accepted a rejected fixture\n' >&2
  exit 1
else
  rejected_status=$?
fi
[[ $rejected_status == 1 && $rejected_required == "$required_memory" ]] || {
  printf 'shadow gate memory admission lost the rejected requirement or status\n' >&2
  exit 1
}
[[ $(monday_production_memory_growth_headroom 5 9 25 3) == 7 ]]
[[ $(monday_production_memory_growth_headroom 5 24 25 3) == 20 ]]
if monday_production_memory_growth_headroom 10 9 25 3 >/dev/null 2>&1; then
  printf 'production memory growth accepted a peak below current usage\n' >&2
  exit 1
fi
if monday_production_memory_growth_headroom 5 26 25 3 >/dev/null 2>&1; then
  printf 'production memory growth accepted a peak above MemoryMax\n' >&2
  exit 1
fi
if monday_shadow_memory_admission 1 1 invalid 0 >/dev/null 2>&1; then
  printf 'shadow gate memory admission accepted an invalid phase limit\n' >&2
  exit 1
fi
if monday_shadow_memory_admission 999999999999999999999999999999 \
  1 1 0 >/dev/null 2>&1; then
  printf 'shadow gate memory admission accepted an overflowing host value\n' >&2
  exit 1
fi
if monday_shadow_memory_admission 1 1 9223372036854775808 \
  0 >/dev/null 2>&1; then
  printf 'shadow gate memory admission accepted an overflowing component\n' >&2
  exit 1
fi
if monday_shadow_memory_admission 1 9223372036854775807 \
  1 >/dev/null 2>&1; then
  printf 'shadow gate memory admission accepted an overflowing sum\n' >&2
  exit 1
fi
if monday_shadow_memory_admission 1 0 0 >/dev/null 2>&1; then
  printf 'shadow gate memory admission accepted a zero requirement\n' >&2
  exit 1
fi
calibrated_gate_bytes=3221225472
[[ $(monday_shadow_memory_admission \
  "$calibrated_gate_bytes" 1073741824 2147483648) == "$calibrated_gate_bytes" ]] || {
  printf 'calibrated sequential gate does not fit its largest Shadow phase\n' >&2
  exit 1
}
if monday_shadow_memory_admission "$((calibrated_gate_bytes - 1))" \
  1073741824 2147483648 >/dev/null; then
  printf 'calibrated sequential gate accepted one byte below its phase reserve\n' >&2
  exit 1
fi

for command in awk base64 cmp cut grep install jq mktemp sed seq sha256sum sort tail; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'missing test dependency: %s\n' "$command" >&2
    exit 2
  }
done

grep -Fq '.trade_summary_contract == "binance.aggregate_trade_summary.v1"' "$GATE"
grep -Fq 'verify_adjacent_segments' "$GATE"
grep -Fq 'run_strict_verifier_pair' "$GATE"
grep -Fq 'run_strict_verifier' "$GATE"
grep -Fq 'verify_aggregate_trade_continuity' "$GATE"
grep -Fq -- '--verify-segment' "$GATE"
grep -Fq -- '--segment-content-sha256' "$GATE"
grep -Fq -- '--segment-manifest-sha256' "$GATE"
grep -Fq -- '--require-lob-continuity' "$GATE"
grep -Fq -- '--verify-aggregate-trade-continuity' "$GATE"
grep -Fq 'verify_raw_trade_continuity' "$GATE"
grep -Fq -- '--verify-raw-trade-continuity' "$GATE"
grep -Fq 'BinanceRawTradeContinuityVerifier' "$COLLECTOR"
grep -Fq 'verify_raw_trade_continuity "${strict_verifier_segments[@]}"' "$GATE"
grep -Fq 'strict_raw_trade_continuity_readback' "$GATE"
grep -Fq 'raw_trade_segments' "$GATE"
grep -Fq 'book_ticker_count' "$GATE"
grep -Fq 'force_order_count' "$GATE"
grep -Fq 'tape_schema' "$GATE"
grep -Fq 'USD-M LOB stream family contract' "$GATE"
grep -Fq 'usdm_perpetual_top100_lob' "$CUTOVER"
grep -Fq 'usdm_perpetual_top100_lob_rust_shadow' "$GATE"
book_ticker_validator=$(sed -n \
  '/^[[:space:]]*def valid_book_ticker:/,/;[[:space:]]*$/p' "$GATE")
spot_book_ticker='{"received_at_ns":1,"frame":{"data":{"u":1,"s":"CATIUSDT","b":"0.1","B":"2","a":"0.2","A":"3"}}}'
usdm_book_ticker='{"received_at_ns":1,"frame":{"data":{"e":"bookTicker","E":2,"T":1,"u":1,"s":"BTCUSDT","b":"0.1","B":"2","a":"0.2","A":"3"}}}'
jq -en --arg market spot --argjson row "$spot_book_ticker" \
  "$book_ticker_validator \$row | valid_book_ticker" >/dev/null
jq -en --arg market usdm --argjson row "$usdm_book_ticker" \
  "$book_ticker_validator \$row | valid_book_ticker" >/dev/null
if jq -en --arg market usdm --argjson row "$spot_book_ticker" \
  "$book_ticker_validator \$row | valid_book_ticker" >/dev/null; then
  printf 'shadow gate accepted spot bookTicker shape for USD-M\n' >&2
  exit 1
fi
if jq -en --arg market spot --argjson row "$usdm_book_ticker" \
  "$book_ticker_validator \$row | valid_book_ticker" >/dev/null; then
  printf 'shadow gate accepted USD-M bookTicker shape for spot\n' >&2
  exit 1
fi
grep -Fq 'full_stream_coverage_verified' "$GATE"
grep -Fq 'or (.full_stream_coverage_verified == true))' "$RUNTIME_POLICY"
grep -Fq '"full_stream_coverage_verified"' "$LOB_ARCHIVER"
grep -Fq -- '--unit="$strict_verifier_unit"' "$GATE"
grep -Fxq 'KillMode=mixed' "$SHADOW_UNIT"
grep -Fxq 'KillMode=mixed' "$SCRIPT_DIR/binance-lob-archiver-production@.service"
grep -Fq -- '--property=KillMode=control-group' "$GATE"
grep -Fq 'MemoryHigh=1280M' "$GATE"
grep -Fq 'MemoryMax=1536M' "$GATE"
grep -Fq 'OOMScoreAdjust=500' "$GATE"
grep -Fq 'verify_oss_round_trips "$market" >"$round_trips_path"' "$GATE"
if grep -Fq 'round_trips=$(verify_oss_round_trips "$market")' "$GATE"; then
  printf 'shadow gate still runs OSS verification in a command-substitution subshell\n' >&2
  exit 1
fi
grep -Fq 'pub fn verify_binance_market_tape_for_strict_gate' "$ARTIFACT_VERIFIER"
grep -Fq 'verify_binance_market_tape_for_strict_gate(sealed)?' "$COLLECTOR"
if grep -Fq '"$candidate_binary" "${strict_verifier_args[@]}"' "$GATE"; then
  printf 'shadow gate still gives every segment to one unbounded strict verifier\n' >&2
  exit 1
fi
grep -Fq '.lob_continuity.contract == "binance.lob_continuity.v1"' "$GATE"
grep -Fq 'jq -e --arg session_id "${observed_session[$market]}"' "$GATE"
grep -Fq -- '--slurpfile manifest "$manifest_path"' "$GATE"
if grep -Fq -- '--argjson lob_continuity' "$GATE"; then
  printf 'shadow gate passes the full-catalog LOB summary through argv\n' >&2
  exit 1
fi
grep -Fq 'manifest changed between discovery and readback' "$GATE"
grep -Fq 'has_replay_safe_checkpoint' "$GATE"
grep -Fq 'unsafe_candidates' "$GATE"
grep -Fq 'monday_validate_replay_safe_manifest_order' "$GATE"
grep -Fq 'fewer than two replay-safe complete OSS manifests' "$GATE"
grep -Fq 'replay-unsafe manifest before a later replay-safe manifest' "$LIB"
grep -Fq 'install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$segment_dir"' "$GATE"
grep -Fq 'manifest_sha256:$manifest_sha256' "$GATE"
grep -Fq 'readonly REQUIRED_DURATION_SECONDS=240' "$GATE"
grep -Fq 'readonly HEALTH_SETTLE_SECONDS=240' "$GATE"
grep -Fq 'Production gates wait up to 240 seconds for health' "$GATE"
grep -Fq 'USD-M shadow and production WS_SHARD_SIZE differ' "$GATE"
grep -Fq 'require_env_value "$file" WS_SHARD_SIZE 25' "$CUTOVER"
grep -Fq 'readonly GATE_SEGMENT_SECONDS=120' "$GATE"
grep -Fq 'readonly RUN_SPOOL_ROOT=/data/monday/spool/binance-lob-rust-shadow/runs' "$GATE"
shadow_usdm_symbols=$(sed -n 's/^SYMBOLS=//p' "$SHADOW_USDM_ENV")
production_usdm_symbols=$(sed -n 's/^SYMBOLS=//p' "$PRODUCTION_USDM_ENV")
[[ $shadow_usdm_symbols == "$production_usdm_symbols" ]] || {
  printf 'shadow and production USD-M symbol lists differ\n' >&2
  exit 1
}
IFS=, read -r -a usdm_symbols <<<"$shadow_usdm_symbols"
[[ ${#usdm_symbols[@]} -eq 100 ]] || {
  printf 'USD-M catalog is not exactly 100 symbols\n' >&2
  exit 1
}
[[ $(printf '%s\n' "${usdm_symbols[@]}" | sort -u | wc -l) -eq 100 ]] || {
  printf 'USD-M catalog contains duplicate symbols\n' >&2
  exit 1
}
[[ $(sed -n 's/^WS_SHARD_SIZE=//p' "$SHADOW_USDM_ENV") == 25
  && $(sed -n 's/^WS_SHARD_SIZE=//p' "$PRODUCTION_USDM_ENV") == 25 ]] || {
  printf 'USD-M websocket shards must contain exactly 25 symbols\n' >&2
  exit 1
}
cutover_symbol_validator=$(sed -n '/^is_usdm_top100()/,/^}/p' "$CUTOVER")
eval "$cutover_symbol_validator"
is_usdm_top100 "$shadow_usdm_symbols"
if is_usdm_top100 ALL; then
  printf 'cutover accepted SYMBOLS=ALL as the candidate USD-M scope\n' >&2
  exit 1
fi
grep -Fq 'min_symbols[usdm]=100' "$GATE"
grep -Fq 'and .markets.usdm.symbol_count == 100' "$POLICY"
grep -Fq '"$CANDIDATE_STARTED_NS" 100' "$CUTOVER"
grep -Fq '"$OLD_USDM_MINIMUM_SYMBOLS"' "$CUTOVER"
startup_body=$(sed -n '/^async fn main()/,/^fn recover_parts_only()/p' "$COLLECTOR")
if grep -Fq 'recover_parts(&config.segment_config())' <<<"$startup_body"; then
  printf 'normal collector startup may not recover interrupted parts\n' >&2
  exit 1
fi
grep -Fq 'ensure_startup_spool_ready(&spool_dir)?' <<<"$startup_body"
grep -Fxq 'ExecStartPre=+/opt/monday/bin/monday-rust-lob-recovery-queue isolate %i' \
  "$SCRIPT_DIR/binance-lob-archiver-production@.service"
grep -Fxq 'TimeoutStartSec=120' \
  "$SCRIPT_DIR/binance-lob-archiver-production@.service"
grep -Fq "grep -Fxq 'StartLimitIntervalSec=7200'" "$CUTOVER"
grep -Fq "grep -Fxq 'TimeoutStartSec=120'" "$CUTOVER"
grep -Fxq 'ExecStart=/opt/monday/bin/monday-rust-lob-recovery-queue drain %i' \
  "$SCRIPT_DIR/binance-lob-archiver-recovery@.service"
grep -Fxq 'Unit=binance-lob-archiver-recovery@%i.service' \
  "$SCRIPT_DIR/binance-lob-archiver-recovery@.timer"
grep -Fq 'host-rust-lob-recovery-queue.sh' "$INSTALL_RELEASE"
sed -n '/^assets=(/,/^)/p' "$INSTALL_RELEASE" \
  | grep -Fq 'monday-collector-health.sh'
grep -Fq 'HEALTH_DEPLOYMENT_ASSET=monday-collector-health.sh' "$CUTOVER"
grep -Fq 'atomic_install 0755 "$directory/$HEALTH_DEPLOYMENT_ASSET"' "$CUTOVER"
queue_root_line=$(grep -n 'install -d -m 0750 -o root -g hftcollector "$RECOVERY_QUEUE_ROOT"' \
  "$CUTOVER" | cut -d: -f1)
candidate_start_line=$(grep -n '^STEP=start-candidate-production$' "$CUTOVER" | cut -d: -f1)
[[ -n $queue_root_line && -n $candidate_start_line \
  && $queue_root_line -lt $candidate_start_line ]] || {
  printf 'cutover does not create the recovery queue root before entering the systemd sandbox\n' >&2
  exit 1
}
grep -Fq '"$RECOVERY_EVIDENCE_ROOT"' "$CUTOVER"
drain_body=$(sed -n '/^run_candidate_drain()/,/^}/p' "$CUTOVER")
incomplete_body=$(sed -n '/^has_incomplete_segment_artifacts()/,/^}/p' "$CUTOVER")
for suffix in '*.jsonl.part' '*.zst.tmp' '*.part.corrupt'; do
  grep -Fq -- "-name '$suffix'" <<<"$incomplete_body" || {
    printf 'cutover does not detect interrupted %s artifacts\n' "$suffix" >&2
    exit 1
  }
done
backup_line=$(grep -n -- 'RECOVERY_BACKUP_DIR=' <<<"$drain_body" | cut -d: -f1 || true)
isolate_line=$(grep -n -- 'monday-rust-lob-recovery-queue isolate "$market"' <<<"$drain_body" | cut -d: -f1 || true)
upload_line=$(grep -n -- '--upload-only' <<<"$drain_body" | cut -d: -f1 || true)
[[ -z ${backup_line:-} && -n $isolate_line && -n $upload_line \
  && $isolate_line -lt $upload_line ]] || {
  printf 'cutover does not detach interrupted spools before upload-only drain\n' >&2
  exit 1
}
for property in \
  '--property=KillMode=control-group' \
  '--property=OOMScoreAdjust=500' \
  '--property=CPUQuota=80%' \
  '--property=MemoryHigh=384M' \
  '--property=MemoryMax=512M'; do
  grep -Fq -- "$property" <<<"$drain_body" || {
    printf 'cutover candidate drain is missing %s\n' "$property" >&2
    exit 1
  }
done
grep -Fq 'CANDIDATE_DRAIN_UNIT="monday-rust-cutover-upload-drain-' <<<"$drain_body"
grep -Fq 'stop_candidate_drain' \
  <<<"$(sed -n '/^on_exit()/,/^}/p' "$CUTOVER")"
grep -Fq "trap 'exit 143' HUP INT TERM" "$CUTOVER"
recover_body=$(sed -n '/^fn recover_parts_only()/,/^fn stream_types_for_market/p' "$COLLECTOR")
grep -Fq '/opt/monday/bin/monday-rust-lob-recovery-queue isolate "$market"' "$CUTOVER"
grep -Fq 'spool_lock.owner()' <<<"$recover_body"
grep -Fq 'validated_nonempty_recovery_parts' <<<"$recover_body"
grep -Fq 'validated_recovery_temporaries' <<<"$recover_body"
backup_line=$(grep -n 'backup_recovery_inputs' <<<"$recover_body" | head -1 | cut -d: -f1)
drop_line=$(grep -n 'drop_recovery_privileges' <<<"$recover_body" | head -1 | cut -d: -f1)
catalog_line=$(grep -n 'prepare_recovery_batches' <<<"$recover_body" | head -1 | cut -d: -f1)
remove_temporary_line=$(grep -n 'remove_recovery_temporaries' <<<"$recover_body" | head -1 | cut -d: -f1)
recover_line=$(grep -n 'recover_recovery_batches' <<<"$recover_body" | head -1 | cut -d: -f1)
[[ -n $backup_line && -n $drop_line && -n $catalog_line \
  && -n $remove_temporary_line && -n $recover_line \
  && $backup_line -lt $drop_line && $drop_line -lt $catalog_line \
  && $catalog_line -lt $remove_temporary_line && $remove_temporary_line -lt $recover_line ]] || {
  printf 'recovery evidence, catalog validation, temporary removal, and recompression are out of order\n' >&2
  exit 1
}
grep -Fq 'production unit retained a MainPID after stop' "$CUTOVER"
grep -Fq 'run_candidate_drain "$OLD_DEPLOYMENT"' "$CUTOVER"
grep -Fq 'SPOOL_ENV_DEPLOYMENT="$OLD_DEPLOYMENT"' "$CUTOVER"
grep -Fq 'SPOOL_ENV_DEPLOYMENT="$CANDIDATE_DEPLOYMENT"' "$CUTOVER"
grep -Fq 'run_candidate_drain "$SPOOL_ENV_DEPLOYMENT"' "$CUTOVER"
grep -Fq '$DRAIN_ATTEMPTED -eq 1 && $DRAIN_MAY_HAVE_MUTATED -eq 0' "$CUTOVER"
recovery_stage_line=$(grep -n '^STEP=stage-candidate-recovery-assets$' "$CUTOVER" | head -n1 | cut -d: -f1)
controller_clear_line=$(grep -n '^STEP=clear-previous-controller-override$' "$CUTOVER" | head -n1 | cut -d: -f1)
old_drain_line=$(grep -n '^  STEP=drain-old-production-with-candidate$' "$CUTOVER" | head -n1 | cut -d: -f1)
candidate_env_install_line=$(grep -n '^STEP=install-candidate-production-assets$' "$CUTOVER" | head -n1 | cut -d: -f1)
[[ -n $controller_clear_line && -n $recovery_stage_line && -n $old_drain_line \
  && -n $candidate_env_install_line \
  && $controller_clear_line -lt $recovery_stage_line \
  && $recovery_stage_line -lt $old_drain_line \
  && $old_drain_line -lt $candidate_env_install_line ]] || {
  printf 'cutover controller identity, recovery assets, drain, and production env are out of order\n' >&2
  exit 1
}
grep -Fq 'spool_dir[$market]=$(run_spool_dir "$candidate_sha" "$gate_run_id" "$market")' "$GATE"
grep -Fq 'install -d -m 0755 -o root -g root' "$GATE"
grep -Fq '"$RUN_SPOOL_ROOT" "$RUN_SPOOL_ROOT/$candidate_sha"' "$GATE"
grep -Fq '"$run_spool_path" "${spool_dir[spot]}" "${spool_dir[usdm]}"' "$GATE"
grep -Fq 'printf '\''SEGMENT_SECONDS=%s\n'\'' "$GATE_SEGMENT_SECONDS"' "$GATE"
[[ $(grep -Fc 'run_candidate_drain "$market"' "$GATE") -eq 1 ]] || {
  printf 'shadow gate drains a fixed or pre-existing spool before the run\n' >&2
  exit 1
}
if grep -Fq 'monday-rust-lob-shadow-gate.lock' "$CUTOVER" "$RESTORE"; then
  printf 'cutover or restore still acquires the duplicate shadow-gate lock\n' >&2
  exit 1
fi
grep -Fq 'monday-rust-lob-release.lock' "$CUTOVER"
grep -Fq 'monday-rust-lob-release.lock' "$RESTORE"
grep -Fq 'readonly MAX_HEALTH_SILENCE_SECONDS=120' "$GATE"
grep -Fq 'MONDAY_TEST_HEALTH_SETTLE_SECONDS' "$GATE"
grep -Fq 'short health settles require a test-only gate' "$GATE"
grep -Fq 'test health settle duration is too large' "$GATE"
grep -Fq 'MONDAY_TEST_HEALTH_SETTLE_SECONDS < HEALTH_SETTLE_SECONDS' "$GATE"
grep -Fq 'health_settle_seconds=$HEALTH_SETTLE_SECONDS' "$GATE"
grep -Fq 'settle_deadline=$(( $(monotonic_seconds) + health_settle_seconds ))' "$GATE"
grep -Fq 'max_age_seconds=$((gate_seconds + health_settle_seconds + 3600))' "$GATE"
[[ $(grep -Fc -- '--argjson health_settle_seconds "$health_settle_seconds"' "$GATE") -eq 2 ]] || {
  printf 'run and final gate evidence do not both record the effective health settle duration\n' >&2
  exit 1
}
[[ $(grep -Fc 'health_settle_seconds:$health_settle_seconds' "$GATE") -eq 2 ]] || {
  printf 'run and final gate evidence do not both expose the effective health settle duration\n' >&2
  exit 1
}
grep -Fq 'and .all_symbols_bridged == true' "$GATE"
grep -Fq 'and .bridged_count == .symbol_count' "$GATE"
grep -Fq 'and .snapshot_only_symbols == []' "$GATE"
grep -Fq 'and .stream_coverage_verified_count == .symbol_count' "$GATE"
grep -Fq 'and .all_stream_coverage_verified == true' "$GATE"
grep -Fq 'then (.symbols | keys | sort) == ($symbols_config | split(",") | sort)' "$GATE"
grep -Fq 'then (.symbols | keys | sort) == ($symbols_config | split(",") | sort)' "$SOAK"
grep -Fq 'configured_catalog_sha256:$configured_catalog_sha256' "$GATE"
grep -Fq 'candidate shadow gate USD-M symbols differ from the deployment bundle' "$CUTOVER"
grep -Fq 'candidate shadow gate USD-M symbols differ from the deployment bundle' "$RESTORE"
grep -Fq 'or (.diff_count == 0' "$GATE"
grep -Fq 'and .first_update_id == null' "$GATE"
grep -Fq 'and .last_update_id == null' "$GATE"
grep -Fq 'market_observation_started_ns[$market]=$(date +%s%N)' "$GATE"
grep -Fq '((end_ns <= market_observation_started_ns[$market])) && continue' "$GATE"
grep -Fq 'shadow segments did not rotate after health settled' "$GATE"
grep -Fq 'end_received_at_ns > $gate.markets.spot.observation_started_ns' "$POLICY"
grep -Fq 'end_received_at_ns > $gate.markets.usdm.observation_started_ns' "$POLICY"
[[ $(grep -Fc 'end_received_at_ns > $gate.observation_started_ns' "$POLICY") -eq 0 ]] || {
  printf 'gate policy does not bind both market tapes across observation start\n' >&2
  exit 1
}
if grep -Fq '((start_ns < gate_started_ns)) && continue' "$GATE"; then
  printf 'manifest discovery still admits health-settle warmup segments\n' >&2
  exit 1
fi
if grep -Fq '((start_ns < market_observation_started_ns[$market])) && continue' "$GATE"; then
  printf 'manifest discovery still drops the segment overlapping observation start\n' >&2
  exit 1
fi
grep -Fq 'market_gate_started_ns[$market]=$(date +%s%N)' "$GATE"
grep -Fq 'all(.[].lob_reconnect_boundary; . == false)' "$GATE"
grep -Fq 'ARG SOURCE_REVISION' "$COLLECTOR_DOCKERFILE"
grep -Fq 'MONDAY_SOURCE_REVISION="$SOURCE_REVISION" cargo' "$COLLECTOR_DOCKERFILE"
grep -Fq 'SOURCE_REVISION=${{ needs.selector.outputs.source_sha }}' "$ACR_WORKFLOW"
grep -Fq "grep -Fqx 'binance-lob-archiver \${{ needs.selector.outputs.source_sha }}'" "$ACR_WORKFLOW"
grep -Fxq 'MemoryHigh=1792M' "$SHADOW_UNIT"
grep -Fxq 'MemoryMax=2048M' "$SHADOW_UNIT"
grep -Fxq 'OOMScoreAdjust=500' "$SHADOW_UNIT"
grep -Fq 'systemctl_value "$market" DropInPaths' "$GATE"
grep -Fq 'systemctl_value "$market" MemoryHigh' "$GATE"
grep -Fq 'memory_max_bytes[$market] == 2147483648' "$GATE"
grep -Fq 'readonly HOST_MEMORY_RESERVE_BYTES=1073741824' "$GATE"
grep -Fq 'readonly PRODUCTION_MEMORY_GROWTH_MARGIN_BYTES=268435456' "$GATE"
grep -Fq 'readonly STRICT_VERIFIER_MEMORY_MAX_BYTES=1610612736' "$GATE"
grep -Fq 'readonly UPLOAD_DRAIN_MEMORY_MAX_BYTES=536870912' "$GATE"
grep -Fxq 'MemoryHigh=384M' "$SCRIPT_DIR/binance-lob-archiver-rust-upload@.service"
grep -Fxq 'MemoryMax=512M' "$SCRIPT_DIR/binance-lob-archiver-rust-upload@.service"
grep -Fxq 'MemoryHigh=384M' "$SCRIPT_DIR/binance-lob-archiver-upload@.service"
grep -Fxq 'MemoryMax=512M' "$SCRIPT_DIR/binance-lob-archiver-upload@.service"
grep -Fq 'monday_shadow_memory_admission' "$GATE"
grep -Fq 'host_swap_total_bytes' "$GATE"
grep -Fq 'production_memory_current_bytes' "$GATE"
grep -Fq 'maximum_sequential_phase_memory_bytes' "$GATE"
grep -Fq 'resource_preflight' "$GATE"
grep -Fq 'resource_admission_samples' "$GATE"
grep -Fq 'systemctl_value "$market" OOMScoreAdjust' "$GATE"
for obsolete in \
  production_memory_headroom_bytes \
  production_memory_soft_headroom_bytes \
  production_memory_reserve_burst_bytes \
  minimum_host_memory_reserve_bytes \
  host_memory_headroom_ok \
  host_memory_shortfall_bytes; do
  if grep -Fq "$obsolete" "$GATE"; then
    printf 'shadow gate still records obsolete resource field %s\n' "$obsolete" >&2
    exit 1
  fi
done
admission_body=$(sed -n '/^admit_resource_phase()/,/^}/p' "$GATE")
grep -Fq '"$available" "$HOST_MEMORY_RESERVE_BYTES" "$phase_memory_max_bytes"' \
  <<<"$admission_body"
grep -Fq '"$production_memory_growth_headroom_bytes"' <<<"$admission_body"
if grep -Fq 'if ! required=$(monday_shadow_memory_admission' <<<"$admission_body"; then
  printf 'phase admission still destroys the rejected helper status with !\n' >&2
  exit 1
fi
grep -Fq 'assert_host_memory_reserve' "$GATE"
grep -Fq 'admit_resource_phase "shadow-$market" 2147483648' "$GATE"
grep -Fq 'admit_resource_phase "upload-drain-$market" "$UPLOAD_DRAIN_MEMORY_MAX_BYTES"' \
  "$GATE"
grep -Fq 'admit_resource_phase "strict-verifier-$strict_verifier_counter"' "$GATE"
preflight_admission_line=$(grep -n '^admit_resource_phase resource-preflight ' "$GATE" \
  | cut -d: -f1)
preflight_exit_line=$(grep -n '^  exit 0$' "$GATE" | cut -d: -f1)
evidence_mutation_line=$(grep -n '^install -d -m 0755 /data/monday$' "$GATE" \
  | cut -d: -f1)
[[ -n $preflight_admission_line && -n $preflight_exit_line \
  && -n $evidence_mutation_line \
  && $preflight_admission_line -lt $preflight_exit_line \
  && $preflight_exit_line -lt $evidence_mutation_line ]] || {
  printf 'resource preflight is not complete before Gate evidence mutation\n' >&2
  exit 1
}
preflight_lock_guard=$(sed -n \
  '/^if \[\[ \$resource_preflight_only != true \]\]; then$/,/^fi$/p' "$GATE")
[[ $preflight_lock_guard == *'install -d -m 0755'* \
  && $preflight_lock_guard == *'flock -n 9'* ]] || {
  printf 'formal Gate lock is not isolated from the non-mutating preflight\n' >&2
  exit 1
}
if grep -Fq 'release lock must already exist for a non-mutating resource preflight' \
  "$GATE"; then
  printf 'resource preflight still depends on a volatile pre-existing lock file\n' >&2
  exit 1
fi
shadow_phase_body=$(sed -n '/^run_market_gate_phase()/,/^}/p' "$GATE")
shadow_admission_line=$(grep -nF 'admit_resource_phase "shadow-$market"' \
  <<<"$shadow_phase_body" | cut -d: -f1)
shadow_start_line=$(grep -nF 'systemctl start "${unit[$market]}"' \
  <<<"$shadow_phase_body" | cut -d: -f1)
[[ -n $shadow_admission_line && -n $shadow_start_line \
  && $shadow_admission_line -lt $shadow_start_line ]] || {
  printf 'shadow phase does not refresh resource admission before start\n' >&2
  exit 1
}
if grep -Fq 'systemctl start "${unit[spot]}" "${unit[usdm]}"' "$GATE"; then
  printf 'shadow gate still starts Spot and USD-M concurrently\n' >&2
  exit 1
fi
grep -Fq 'readonly -a markets=(spot usdm)' "$GATE"
grep -Fq 'run_market_gate_phase "$market"' "$GATE"
grep -Fq 'for other in "${markets[@]}"; do' "$GATE"
grep -Fq 'shadow service is active before the $market phase' "$GATE"
if grep -Fq 'binance-lob-archiver-rust-usdm-memory.conf' "$INSTALL_RELEASE" "$GATE"; then
  printf 'shadow memory contract still depends on a persistent USD-M drop-in\n' >&2
  exit 1
fi

required_duration_seconds=$(sed -n 's/^readonly REQUIRED_DURATION_SECONDS=//p' "$GATE")
[[ $required_duration_seconds =~ ^[1-9][0-9]*$ ]] || {
  printf 'gate has no positive REQUIRED_DURATION_SECONDS\n' >&2
  exit 1
}
gate_segment_seconds=$(sed -n 's/^readonly GATE_SEGMENT_SECONDS=//p' "$GATE")
[[ $gate_segment_seconds =~ ^[1-9][0-9]*$ ]] || {
  printf 'gate has no positive GATE_SEGMENT_SECONDS\n' >&2
  exit 1
}
(( required_duration_seconds >= 2 * gate_segment_seconds )) || {
  printf 'formal Gate cannot produce two run-scoped segments (obs %ss, segment %ss)\n' \
    "$required_duration_seconds" "$gate_segment_seconds" >&2
  exit 1
}
for shadow_env in \
  "$SCRIPT_DIR/binance-lob-archiver-rust-spot.env" \
  "$SCRIPT_DIR/binance-lob-archiver-rust-usdm.env"; do
  segment_seconds=$(sed -n 's/^SEGMENT_SECONDS=//p' "$shadow_env")
  [[ $segment_seconds =~ ^[1-9][0-9]*$ ]] || {
    printf 'shadow env has no positive SEGMENT_SECONDS: %s\n' "$shadow_env" >&2
    exit 1
  }
  ((segment_seconds == 300)) || {
    printf 'committed stability cadence changed unexpectedly: %s (%ss)\n' \
      "$shadow_env" "$segment_seconds" >&2
    exit 1
  }
done
shadow_spot_snapshot_producers=$(sed -n 's/^SNAPSHOT_PRODUCERS=//p' \
  "$SCRIPT_DIR/binance-lob-archiver-rust-spot.env")
production_spot_snapshot_producers=$(sed -n 's/^SNAPSHOT_PRODUCERS=//p' \
  "$SCRIPT_DIR/binance-lob-archiver-production-spot.env")
[[ $shadow_spot_snapshot_producers == 16 \
  && $production_spot_snapshot_producers == "$shadow_spot_snapshot_producers" ]] || {
  printf 'Spot shadow and production must pin SNAPSHOT_PRODUCERS=16\n' >&2
  exit 1
}
grep -Fq 'Spot shadow SNAPSHOT_PRODUCERS must be 16' "$GATE"
grep -Fq 'Spot shadow and production SNAPSHOT_PRODUCERS differ' "$GATE"

tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' EXIT

runtime_contract_dir="$tmp_dir/runtime-contract"
mkdir -p "$runtime_contract_dir"
for asset in \
  binance-lob-archiver-production@.service \
  binance-lob-archiver-rust@.service \
  binance-lob-archiver-upload@.service \
  binance-lob-archiver-rust-upload@.service \
  binance-lob-archiver-production-spot.env \
  binance-lob-archiver-production-usdm.env \
  binance-lob-archiver-rust-spot.env \
  binance-lob-archiver-rust-usdm.env; do
  cp "$SCRIPT_DIR/$asset" "$runtime_contract_dir/$asset"
done
cp "$CUTOVER" "$runtime_contract_dir/host-rust-lob-cutover.sh"
runtime_contract_before=$(monday_rust_lob_runtime_contract_sha256 "$runtime_contract_dir")
printf '\n# controller-only fixture\n' >>"$runtime_contract_dir/host-rust-lob-cutover.sh"
[[ $(monday_rust_lob_runtime_contract_sha256 "$runtime_contract_dir") \
  == "$runtime_contract_before" ]] || {
  printf 'controller-only bytes changed the runtime contract\n' >&2
  exit 1
}
printf '\n# runtime fixture\n' >>"$runtime_contract_dir/binance-lob-archiver-production-spot.env"
[[ $(monday_rust_lob_runtime_contract_sha256 "$runtime_contract_dir") \
  != "$runtime_contract_before" ]] || {
  printf 'runtime bytes did not change the runtime contract\n' >&2
  exit 1
}

resource_admission_body="$tmp_dir/resource-admission.sh"
sed -n '/^admit_resource_phase()/,/^}/p' "$GATE" >"$resource_admission_body"
resource_admission_fixture() (
  fixture_available=$1
  HOST_MEMORY_RESERVE_BYTES=1073741824
  PRODUCTION_MEMORY_GROWTH_MARGIN_BYTES=268435456
  production_memory_growth_headroom_bytes=268435456
  resource_admission_samples_json='[]'
  latest_resource_admission_sample_json=null
  meminfo_bytes() { [[ $1 == MemAvailable ]] && printf '%s\n' "$fixture_available"; }
  die() { printf '%s\n' "$*" >&2; exit 1; }
  # shellcheck disable=SC1090
  . "$resource_admission_body"
  admit_resource_phase shadow-spot 2147483648
  printf '%s\n' "$latest_resource_admission_sample_json"
)
resource_sample=$(resource_admission_fixture 3489660928)
jq -e '.phase == "shadow-spot"
  and .host_memory_available_bytes == 3489660928
  and .host_memory_reserve_bytes == 1073741824
  and .phase_memory_max_bytes == 2147483648
  and .production_memory_growth_margin_bytes == 268435456
  and .production_memory_growth_headroom_bytes == 268435456
  and .required_bytes == 3489660928' <<<"$resource_sample" >/dev/null
if resource_admission_fixture 3489660927 >/dev/null 2>&1; then
  printf 'phase admission accepted one byte below reserve, phase max, and production growth\n' >&2
  exit 1
fi

cutover_drain_fixture_body="$tmp_dir/cutover-drain.sh"
sed -n '/^stop_candidate_drain()/,/^}/p;/^run_candidate_drain()/,/^}/p' \
  "$CUTOVER" >"$cutover_drain_fixture_body"
run_cutover_drain_fixture() (
  local -a invocations=() stopped_units=()
  CANDIDATE_DRAIN_UNIT=
  CANDIDATE_DRAIN_COUNTER=0
  CANDIDATE_SHA256=$(printf 'a%.0s' {1..64})
  CANDIDATE_BINARY=/candidate/binance-lob-archiver
  CANONICAL_SPOOL=/spool
  SAFE_PATH=/usr/bin:/bin
  DRAIN_MAY_HAVE_MUTATED=0
  DRAIN_ENV_KEYS=(MARKET)
  canonical_spool_paths_safe() { return 0; }
  env_value() { printf 'value\n'; }
  has_incomplete_segment_artifacts() { return 1; }
  require_empty_segment_spool() { return 0; }
  jq() { return 0; }
  systemctl() { [[ $1 == stop ]] && stopped_units+=("$2"); }
  systemd-run() { invocations+=("$*"); }
  # shellcheck disable=SC1090
  . "$cutover_drain_fixture_body"
  run_candidate_drain /deployment
  [[ ${#invocations[@]} -eq 2 && ${#stopped_units[@]} -eq 0 ]]
  for invocation in "${invocations[@]}"; do
    [[ $invocation == *'--property=KillMode=control-group'* ]]
    [[ $invocation == *'--property=OOMScoreAdjust=500'* ]]
    [[ $invocation == *'--property=CPUQuota=80%'* ]]
    [[ $invocation == *'--property=MemoryHigh=384M'* ]]
    [[ $invocation == *'--property=MemoryMax=512M'* ]]
  done
  [[ -z $CANDIDATE_DRAIN_UNIT ]]
)
run_cutover_drain_fixture
run_cutover_drain_failure_fixture() (
  local -a stopped_units=()
  CANDIDATE_DRAIN_UNIT=
  CANDIDATE_DRAIN_COUNTER=0
  CANDIDATE_SHA256=$(printf 'a%.0s' {1..64})
  CANDIDATE_BINARY=/candidate/binance-lob-archiver
  CANONICAL_SPOOL=/spool
  SAFE_PATH=/usr/bin:/bin
  DRAIN_MAY_HAVE_MUTATED=0
  DRAIN_ENV_KEYS=(MARKET)
  canonical_spool_paths_safe() { return 0; }
  env_value() { printf 'value\n'; }
  has_incomplete_segment_artifacts() { return 1; }
  require_empty_segment_spool() { return 0; }
  jq() { return 0; }
  systemctl() { [[ $1 == stop ]] && stopped_units+=("$2"); }
  systemd-run() { return 17; }
  # shellcheck disable=SC1090
  . "$cutover_drain_fixture_body"
  if run_candidate_drain /deployment; then
    printf 'failed cutover drain fixture unexpectedly passed\n' >&2
    exit 1
  fi
  [[ ${#stopped_units[@]} -eq 1 ]]
  [[ ${stopped_units[0]} == monday-rust-cutover-upload-drain-*.service ]]
  [[ -z $CANDIDATE_DRAIN_UNIT ]]
)
run_cutover_drain_failure_fixture

active_segment_body=$(sed -n '/^active_segment_start_ns()/,/^}/p' "$GATE")
eval "$active_segment_body"
active_segment_fixture="$tmp_dir/active-segment"
mkdir -p "$active_segment_fixture"
touch "$active_segment_fixture/part-100.jsonl.part" \
  "$active_segment_fixture/part-200.jsonl.part"
ln -s part-200.jsonl.part "$active_segment_fixture/part-300.jsonl.part"
[[ $(active_segment_start_ns "$active_segment_fixture") == 200 ]] || {
  printf 'active segment discovery did not select the newest direct part\n' >&2
  exit 1
}

strict_verifier_body="$tmp_dir/strict-verifier.sh"
sed -n '/^stop_strict_verifier()/,/^}/p;/^run_strict_verifier()/,/^}/p;/^run_strict_verifier_pair()/,/^}/p;/^verify_adjacent_segments()/,/^}/p;/^verify_aggregate_trade_continuity()/,/^}/p;/^verify_raw_trade_continuity()/,/^}/p' \
  "$GATE" >"$strict_verifier_body"
run_strict_verifier_fixture() (
  local -a verifier_units=()
  local -a verifier_invocations=()
  strict_verifier_unit=
  strict_verifier_counter=0
  STRICT_VERIFIER_MEMORY_MAX_BYTES=1610612736
  candidate_binary=candidate_binary
  die() { printf '%s\n' "$*" >&2; exit 1; }
  admit_resource_phase() {
    [[ $1 == strict-verifier-* && $2 == "$STRICT_VERIFIER_MEMORY_MAX_BYTES" ]]
  }
  run_memory_psi_phase_command() { shift 2; "$@"; }
  systemd-run() {
    verifier_units+=("$*")
    while (($#)); do
      if [[ $1 == -- ]]; then
        shift
        break
      fi
      shift
    done
    "$@"
  }
  candidate_binary() {
    verifier_invocations+=("$*")
  }
  # shellcheck disable=SC1090
  . "$strict_verifier_body"
  verify_adjacent_segments \
    first.zst first-content first-manifest \
    second.zst second-content second-manifest \
    third.zst third-content third-manifest
  verify_aggregate_trade_continuity \
    first.zst first-content first-manifest \
    second.zst second-content second-manifest \
    third.zst third-content third-manifest
  verify_raw_trade_continuity \
    first.zst first-content first-manifest \
    second.zst second-content second-manifest \
    third.zst third-content third-manifest
  [[ ${#verifier_invocations[@]} -eq 4 ]] || {
    printf 'strict verifier did not run adjacent pairs plus one continuity pass per trade family\n' >&2
    exit 1
  }
  [[ ${verifier_invocations[0]} == \
    '--require-lob-continuity --verify-segment first.zst --segment-content-sha256 first-content --segment-manifest-sha256 first-manifest --verify-segment second.zst --segment-content-sha256 second-content --segment-manifest-sha256 second-manifest' ]]
  [[ ${verifier_invocations[1]} == \
    '--require-lob-continuity --verify-segment second.zst --segment-content-sha256 second-content --segment-manifest-sha256 second-manifest --verify-segment third.zst --segment-content-sha256 third-content --segment-manifest-sha256 third-manifest' ]]
  [[ ${verifier_invocations[2]} == \
    '--verify-aggregate-trade-continuity --verify-segment first.zst --segment-content-sha256 first-content --segment-manifest-sha256 first-manifest --verify-segment second.zst --segment-content-sha256 second-content --segment-manifest-sha256 second-manifest --verify-segment third.zst --segment-content-sha256 third-content --segment-manifest-sha256 third-manifest' ]] || {
    printf 'aggregate continuity verifier lost segment trust-anchor flags\n' >&2
    exit 1
  }
  [[ ${verifier_invocations[3]} == \
    '--verify-raw-trade-continuity --verify-segment first.zst --segment-content-sha256 first-content --segment-manifest-sha256 first-manifest --verify-segment second.zst --segment-content-sha256 second-content --segment-manifest-sha256 second-manifest --verify-segment third.zst --segment-content-sha256 third-content --segment-manifest-sha256 third-manifest' ]] || {
    printf 'raw-trade continuity verifier lost segment trust-anchor flags\n' >&2
    exit 1
  }
  [[ ${#verifier_units[@]} -eq 4 ]] || {
    printf 'strict verifier did not isolate every verification pass\n' >&2
    exit 1
  }
  for verifier_unit in "${verifier_units[@]}"; do
    [[ $verifier_unit == *'--property=OOMScoreAdjust=500'* ]] || exit 1
    [[ $verifier_unit == *'--property=MemoryHigh=1280M'* ]] || exit 1
    [[ $verifier_unit == *'--property=MemoryMax=1536M'* ]] || exit 1
  done
)
run_strict_verifier_fixture

run_strict_verifier_failure_fixture() (
  local -a stopped_units=()
  strict_verifier_unit=
  strict_verifier_counter=0
  STRICT_VERIFIER_MEMORY_MAX_BYTES=1610612736
  candidate_binary=candidate_binary
  admit_resource_phase() { :; }
  run_memory_psi_phase_command() { shift 2; "$@"; }
  systemd-run() {
    while (($#)); do
      if [[ $1 == -- ]]; then
        shift
        break
      fi
      shift
    done
    "$@"
    return 17
  }
  systemctl() {
    [[ $1 == stop ]] || exit 1
    stopped_units+=("$2")
  }
  candidate_binary() { :; }
  # shellcheck disable=SC1090
  . "$strict_verifier_body"
  if run_strict_verifier_pair \
    --verify-segment first.zst \
    --segment-content-sha256 first-content \
    --segment-manifest-sha256 first-manifest; then
    printf 'failed strict verifier fixture unexpectedly passed\n' >&2
    exit 1
  fi
  [[ ${#stopped_units[@]} -eq 1 ]] || {
    printf 'failed strict verifier did not stop its transient unit\n' >&2
    exit 1
  }
  [[ ${stopped_units[0]} == monday-rust-strict-verifier-*.service ]] || {
    printf 'failed strict verifier stopped the wrong unit: %s\n' "${stopped_units[0]}" >&2
    exit 1
  }
)
run_strict_verifier_failure_fixture

upload_drain_body="$tmp_dir/upload-drain.sh"
sed -n '/^stop_upload_drain()/,/^}/p;/^run_candidate_drain()/,/^}/p' \
  "$GATE" >"$upload_drain_body"
run_upload_drain_fixture() (
  local -a invocations=()
  declare -A spool_dir oss_bucket oss_endpoint oss_region aliyun_profile oss_copy_timeout
  upload_drain_unit=
  upload_drain_counter=0
  UPLOAD_DRAIN_MEMORY_MAX_BYTES=536870912
  SERVICE_USER=hftcollector
  SERVICE_HOME=/var/lib/hft-collector
  SAFE_PATH=/usr/bin:/bin
  candidate_binary=/candidate/binance-lob-archiver
  spool_dir[spot]=/spool/spot
  oss_bucket[spot]=bucket
  oss_endpoint[spot]=endpoint
  oss_region[spot]=region
  aliyun_profile[spot]=profile
  oss_copy_timeout[spot]=60
  admit_resource_phase() {
    [[ $1 == upload-drain-spot && $2 == "$UPLOAD_DRAIN_MEMORY_MAX_BYTES" ]]
  }
  run_memory_psi_phase_command() { shift 2; "$@"; }
  systemd-run() { invocations+=("$*"); }
  assert_spool_drained() { [[ $1 == spot ]]; }
  # shellcheck disable=SC1090
  . "$upload_drain_body"
  run_candidate_drain spot
  [[ ${#invocations[@]} -eq 1 ]] || exit 1
  [[ ${invocations[0]} == *'--property=CPUQuota=80%'* ]] || exit 1
  [[ ${invocations[0]} == *'--property=MemoryHigh=384M'* ]] || exit 1
  [[ ${invocations[0]} == *'--property=MemoryMax=512M'* ]] || exit 1
  [[ ${invocations[0]} == *'/candidate/binance-lob-archiver --upload-only'* ]] || exit 1
  [[ -z $upload_drain_unit ]]
)
run_upload_drain_fixture

health_settle_body="$tmp_dir/resolve-health-settle.sh"
sed -n '/^resolve_health_settle_seconds()/,/^}/p' "$GATE" >"$health_settle_body"
resolve_health_settle() (
  HEALTH_SETTLE_SECONDS=240
  gate_seconds=$1
  test_only=$2
  MONDAY_ALLOW_SHORT_GATE_FOR_TESTS=$3
  MONDAY_TEST_HEALTH_SETTLE_SECONDS=$4
  die() { printf '%s\n' "$*" >&2; exit 1; }
  # shellcheck disable=SC1090
  . "$health_settle_body"
  resolve_health_settle_seconds
  printf '%s\n' "$health_settle_seconds"
)
[[ $(resolve_health_settle 120 true 1 60) == 60 ]] || {
  printf 'authorized short health settle was not applied\n' >&2
  exit 1
}
[[ $(resolve_health_settle 120 true 1 '') == 240 ]] || {
  printf 'test-only gate without an override did not keep the formal settle\n' >&2
  exit 1
}
for fixture in \
  '240 false 1 60' \
  '120 true 0 60' \
  '120 true 1 invalid' \
  '120 true 1 240' \
  '120 true 1 241' \
  "120 true 1 $(printf '9%.0s' {1..100})"; do
  read -r fixture_gate fixture_test fixture_auth fixture_value <<<"$fixture"
  if resolve_health_settle "$fixture_gate" "$fixture_test" "$fixture_auth" \
    "$fixture_value" >/dev/null 2>&1; then
    printf 'invalid short health settle fixture was accepted: %s\n' "$fixture" >&2
    exit 1
  fi
done

safe_candidates="$tmp_dir/safe-candidates.tsv"
unsafe_candidates="$tmp_dir/unsafe-candidates.tsv"
printf '100\t200\tsafe-1\n200\t300\tsafe-2\n' >"$safe_candidates"
printf '300\t360\tunsafe-tail\n' >"$unsafe_candidates"
monday_validate_replay_safe_manifest_order test "$safe_candidates" "$unsafe_candidates"

printf '100\t200\tsafe-1\n300\t400\tsafe-2\n' >"$safe_candidates"
printf '200\t300\tunsafe-middle\n' >"$unsafe_candidates"
if monday_validate_replay_safe_manifest_order test "$safe_candidates" "$unsafe_candidates" \
  2>/dev/null; then
  printf 'replay-unsafe middle manifest was accepted\n' >&2
  exit 1
fi

printf '100\t200\tsafe-1\n' >"$safe_candidates"
printf '150\t250\tunsafe-overlap\n' >"$unsafe_candidates"
if monday_validate_replay_safe_manifest_order test "$safe_candidates" "$unsafe_candidates" \
  2>/dev/null; then
  printf 'replay-unsafe overlapping manifest was accepted\n' >&2
  exit 1
fi

printf '100\t200\tsafe-1\n' >"$safe_candidates"
printf '200\t300\tunsafe-tail\n' >"$unsafe_candidates"
monday_validate_replay_safe_manifest_order test "$safe_candidates" "$unsafe_candidates"
safe_manifest_count=$(wc -l <"$safe_candidates" | tr -d ' ')
((safe_manifest_count < 2)) || {
  printf 'trailing replay-unsafe fixture incorrectly counted as a second safe manifest\n' >&2
  exit 1
}

last_updated_ns=1
last_advance_mono=0
max_gap=0
health_sample_increments=0
for current_mono in $(seq 30 30 3600); do
  current_updated_ns=$((current_mono * 1000000000))
  read -r last_updated_ns last_advance_mono max_gap sample_increment < <(
    monday_observe_health_freshness \
      "$last_updated_ns" "$last_advance_mono" "$max_gap" \
      "$current_updated_ns" "$current_mono" 120
  )
  health_sample_increments=$((health_sample_increments + sample_increment))
done
((health_sample_increments == 120 && max_gap <= 120)) || {
  printf 'fresh one-hour health sequence did not pass the monotonic observer\n' >&2
  exit 1
}
read -r jitter_updated_ns jitter_advance_mono jitter_max_gap jitter_increment < <(
  monday_observe_health_freshness 1 0 0 2 91 120
)
[[ $jitter_updated_ns == 2 && $jitter_advance_mono == 91 \
  && $jitter_max_gap == 91 && $jitter_increment == 1 ]] || {
  printf 'monotonic observer rejected an advancing 91-second jitter sample\n' >&2
  exit 1
}
if monday_observe_health_freshness \
  "$jitter_updated_ns" "$jitter_advance_mono" "$jitter_max_gap" \
  "$jitter_updated_ns" "$((jitter_advance_mono + 121))" 120 >/dev/null; then
  printf 'monotonic observer accepted a 121-second health freeze\n' >&2
  exit 1
fi

artifact=$(printf 'a%.0s' {1..64})
bundle=$(printf 'b%.0s' {1..64})
source_revision=$(printf 'c%.0s' {1..40})
catalog=$(printf 'd%.0s' {1..64})
runtime_contract=$(printf 'e%.0s' {1..64})
gate_run_id=20260820T000000Z-1
run_spool="/data/monday/spool/binance-lob-rust-shadow/runs/$artifact/$gate_run_id"
usdm_symbols_config=$(sed -n 's/^SYMBOLS=//p' "$SHADOW_USDM_ENV")
usdm_catalog=$(jq -cn --arg symbols "$usdm_symbols_config" \
  '$symbols | split(",") | sort' | sha256sum | awk '{print $1}')

market_json=$(jq -cn \
  --arg catalog "$catalog" \
  '{observation_started_ns:150,
    symbol_count:1200,snapshot_ready_count:1200,bridged_count:1200,
    stream_coverage_verified_count:1200,all_stream_coverage_verified:true,sequence_gaps:0,
    upload_failure_count:0,health_samples:121,max_health_silence_seconds:30,
    symbols_config:"ALL",catalog_sha256:$catalog,configured_catalog_sha256:$catalog,
    session_id:"session-1",oss_roundtrips:2,
    tape_schema:"binance.market_tape.v2",
    stream_types:["aggTrade","bookTicker","depth@100ms","trade"],
    agg_trade_segments:2,agg_trade_count:2,
    raw_trade_segments:2,raw_trade_count:2,book_ticker_count:2,
    strict_trade_summary_readback:true,
    strict_lob_continuity_readback:true,
    strict_raw_trade_continuity_readback:true,
    full_stream_coverage_verified:true,
    lob_reconnect_boundaries:0,
    min_lob_source_latency_ms:0,max_lob_source_latency_ms:0,
    min_lob_bid_levels:1,min_lob_ask_levels:1,
    max_segment_gap_ns:0,
    oss_roundtrip_evidence:[
      {success_uri:"oss://bucket/part-1.jsonl.zst._SUCCESS",sha256:$catalog,manifest_sha256:$catalog,
       gap_from_previous_ns:0,start_received_at_ns:100,end_received_at_ns:200,agg_trade_count:1,
       raw_trade_count:1,book_ticker_count:1,
       lob_capture_session_id:"session-1",lob_reconnect_boundary:false,lob_sequence_gaps:0,
       lob_source_time_rollbacks:0,lob_declared_symbol_count:1200,lob_covered_symbol_count:1200,
       stream_coverage_verified_count:1200,all_stream_coverage_verified:true,
       lob_min_source_latency_ms:0,lob_max_source_latency_ms:0,
       lob_min_bid_levels:1,lob_min_ask_levels:1},
      {success_uri:"oss://bucket/part-2.jsonl.zst._SUCCESS",sha256:$catalog,manifest_sha256:$catalog,
       gap_from_previous_ns:0,start_received_at_ns:200,end_received_at_ns:300,agg_trade_count:1,
       raw_trade_count:1,book_ticker_count:1,
       lob_capture_session_id:"session-1",lob_reconnect_boundary:false,lob_sequence_gaps:0,
       lob_source_time_rollbacks:0,lob_declared_symbol_count:1200,lob_covered_symbol_count:1200,
       stream_coverage_verified_count:1200,all_stream_coverage_verified:true,
       lob_min_source_latency_ms:0,lob_max_source_latency_ms:0,
       lob_min_bid_levels:1,lob_min_ask_levels:1}
    ]}')
usdm_market=$(jq -c --arg symbols_config "$usdm_symbols_config" \
  --arg catalog_sha256 "$usdm_catalog" '
  .symbol_count = 100
  | .snapshot_ready_count = 100
  | .bridged_count = 100
  | .stream_coverage_verified_count = 100
  | .symbols_config = $symbols_config
  | .catalog_sha256 = $catalog_sha256
  | .configured_catalog_sha256 = $catalog_sha256
    | .stream_types = ["depth@100ms"]
    | .agg_trade_segments = 0
    | .agg_trade_count = 0
    | .raw_trade_segments = 0
    | .raw_trade_count = 0
    | .book_ticker_count = 0
    | .strict_trade_summary_readback = false
    | .strict_raw_trade_continuity_readback = false
    | .force_order_count = 0
    | .oss_roundtrip_evidence |= map(
      .lob_declared_symbol_count = 100 | .lob_covered_symbol_count = 100
      | .stream_coverage_verified_count = 100
      | .agg_trade_count = 0 | .raw_trade_count = 0
      | .book_ticker_count = 0 | .force_order_count = 0)' \
  <<<"$market_json")
psi_windows=$(jq -cn '
  ["resource-preflight","shadow-spot","upload-drain-spot","shadow-usdm",
    "upload-drain-usdm","oss-roundtrip-spot","strict-verifier-1",
    "oss-roundtrip-usdm"] as $phases
  | [$phases[] as $phase | range(0;3) as $index
    | {phase:$phase,started_at:"2026-08-28T00:00:00Z",
       finished_at:"2026-08-28T00:00:15Z",
       previous_total_us:0,current_total_us:0,
       delta_us:0,window_us:15000000,ratio:0,hit:false,consecutive_hits:0}]')
jq -n \
  --arg artifact "$artifact" \
  --arg runtime_contract "$runtime_contract" \
  --arg bundle "$bundle" \
  --arg source "$source_revision" \
  --arg run_id "$gate_run_id" \
  --arg run_spool "$run_spool" \
  --argjson market "$market_json" \
  --argjson usdm_market "$usdm_market" \
  --argjson psi_windows "$psi_windows" \
  '{schema:"monday.rust_lob_shadow_gate.v4",candidate_sha256:$artifact,
    runtime_contract_sha256:$runtime_contract,
    deployment_bundle_sha256:$bundle,deployment_source_revision:$source,
    run_id:$run_id,run_spool:$run_spool,
    required_duration_seconds:240,requested_duration_seconds:240,
    health_settle_seconds:240,segment_seconds:120,test_only:false,
    memory_full_psi_windows:$psi_windows,
    observation_started_ns:150,
    passed:true,production_eligible:true,checks_passed:true,duration_seconds:240,
    markets:{spot:$market,usdm:$usdm_market}}' \
  >"$tmp_dir/gate.json"

jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null

jq 'del(.memory_full_psi_windows)' "$tmp_dir/gate.json" \
  >"$tmp_dir/missing-psi.json"
if jq -e --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  -f "$POLICY" "$tmp_dir/missing-psi.json" >/dev/null; then
  printf 'gate policy accepted missing memory PSI evidence\n' >&2
  exit 1
fi
jq '.memory_full_psi_windows[0:3] |=
  (.[0] += {current_total_us:150000,delta_us:150000,ratio:0.01,hit:true,consecutive_hits:1}
  | .[1] += {previous_total_us:150000,current_total_us:300000,delta_us:150000,
      ratio:0.01,hit:true,consecutive_hits:2}
  | .[2] += {previous_total_us:300000,current_total_us:450000,delta_us:150000,
      ratio:0.01,hit:true,consecutive_hits:3})' "$tmp_dir/gate.json" \
  >"$tmp_dir/three-psi-hits.json"
if jq -e --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  -f "$POLICY" "$tmp_dir/three-psi-hits.json" >/dev/null; then
  printf 'gate policy accepted three consecutive memory PSI hits\n' >&2
  exit 1
fi

jq 'del(.observation_started_ns)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-observation-boundary.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/missing-observation-boundary.json" >/dev/null; then
  printf 'gate policy accepted evidence without an observation boundary\n' >&2
  exit 1
fi
jq 'del(.markets.spot.observation_started_ns)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-market-observation-boundary.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/missing-market-observation-boundary.json" >/dev/null; then
  printf 'gate policy accepted Spot evidence without its observation boundary\n' >&2
  exit 1
fi
jq 'del(.markets.usdm.observation_started_ns)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-usdm-observation-boundary.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/missing-usdm-observation-boundary.json" >/dev/null; then
  printf 'gate policy accepted USD-M evidence without its observation boundary\n' >&2
  exit 1
fi
jq '.markets.spot.observation_started_ns = 99' \
  "$tmp_dir/gate.json" >"$tmp_dir/late-evidence-start.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/late-evidence-start.json" >/dev/null; then
  printf 'gate policy accepted evidence that starts after observation\n' >&2
  exit 1
fi
jq '.markets.usdm.observation_started_ns = 200' \
  "$tmp_dir/gate.json" >"$tmp_dir/early-evidence-end.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/early-evidence-end.json" >/dev/null; then
  printf 'gate policy accepted evidence ending before observation\n' >&2
  exit 1
fi

jq '.markets.usdm.stream_types = ["aggTrade","bookTicker","depth@100ms","forceOrder","trade"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/usdm-legacy-stream-contract.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/usdm-legacy-stream-contract.json" >/dev/null; then
  printf 'gate policy accepted the legacy USD-M full-tape stream contract\n' >&2
  exit 1
fi
jq '.markets.usdm.book_ticker_count = 1
    | .markets.usdm.oss_roundtrip_evidence |= map(.book_ticker_count = 1)' \
  "$tmp_dir/gate.json" >"$tmp_dir/usdm-book-ticker-rows.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/usdm-book-ticker-rows.json" >/dev/null; then
  printf 'gate policy accepted USD-M evidence with bookTicker rows\n' >&2
  exit 1
fi

jq '.markets.usdm.symbol_count = 101
    | .markets.usdm.snapshot_ready_count = 101
    | .markets.usdm.bridged_count = 101
    | .markets.usdm.stream_coverage_verified_count = 101' \
  "$tmp_dir/gate.json" >"$tmp_dir/usdm-101-symbols.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/usdm-101-symbols.json" >/dev/null; then
  printf 'gate policy accepted 101 USD-M symbols\n' >&2
  exit 1
fi

jq '.markets.usdm.symbols_config = "ALL"' \
  "$tmp_dir/gate.json" >"$tmp_dir/usdm-all-symbols.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/usdm-all-symbols.json" >/dev/null; then
  printf 'gate policy accepted SYMBOLS=ALL for USD-M\n' >&2
  exit 1
fi

jq '.markets.usdm.configured_catalog_sha256 =
      "eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee"' \
  "$tmp_dir/gate.json" >"$tmp_dir/usdm-catalog-mismatch.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/usdm-catalog-mismatch.json" >/dev/null; then
  printf 'gate policy accepted a USD-M configured/runtime catalog mismatch\n' >&2
  exit 1
fi

jq '.run_spool = "/data/monday/spool/binance-lob-rust-shadow/spot"' \
  "$tmp_dir/gate.json" >"$tmp_dir/fixed-spool-gate.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/fixed-spool-gate.json" >/dev/null; then
  printf 'gate policy accepted a fixed shared shadow spool\n' >&2
  exit 1
fi

v1_market=$(jq -c '
  del(.stream_types, .raw_trade_segments, .raw_trade_count, .book_ticker_count,
      .force_order_count, .strict_raw_trade_continuity_readback)
  | .tape_schema = "binance.market_tape.v1"
  | .full_stream_coverage_verified = null
  | .oss_roundtrip_evidence |= map(
      del(.raw_trade_count, .book_ticker_count, .force_order_count))' \
  <<<"$market_json")
v1_usdm_market=$(jq -c --arg symbols_config "$usdm_symbols_config" '
  .symbol_count = 100
  | .snapshot_ready_count = 100
  | .bridged_count = 100
  | .stream_coverage_verified_count = 100
  | .symbols_config = $symbols_config
  | .stream_types = ["depth@100ms"]
  | .agg_trade_segments = 0
  | .agg_trade_count = 0
  | .raw_trade_segments = 0
  | .raw_trade_count = 0
  | .book_ticker_count = 0
  | .strict_trade_summary_readback = false
  | .strict_raw_trade_continuity_readback = false
  | .force_order_count = 0
  | .oss_roundtrip_evidence |= map(
      .lob_declared_symbol_count = 100 | .lob_covered_symbol_count = 100
      | .stream_coverage_verified_count = 100
      | .agg_trade_count = 0 | .raw_trade_count = 0
      | .book_ticker_count = 0 | .force_order_count = 0)' \
  <<<"$v1_market")
jq -n \
  --arg artifact "$artifact" \
  --arg runtime_contract "$runtime_contract" \
  --arg bundle "$bundle" \
  --arg source "$source_revision" \
  --arg run_id "$gate_run_id" \
  --arg run_spool "$run_spool" \
  --argjson market "$v1_market" \
  --argjson usdm_market "$v1_usdm_market" \
  '{schema:"monday.rust_lob_shadow_gate.v4",candidate_sha256:$artifact,
    runtime_contract_sha256:$runtime_contract,
    deployment_bundle_sha256:$bundle,deployment_source_revision:$source,
    run_id:$run_id,run_spool:$run_spool,
    required_duration_seconds:240,requested_duration_seconds:240,
    health_settle_seconds:240,segment_seconds:120,test_only:false,
    observation_started_ns:150,
    passed:true,production_eligible:true,checks_passed:true,duration_seconds:240,
    markets:{spot:$market,usdm:$usdm_market}}' \
  >"$tmp_dir/gate-v1.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/gate-v1.json" >/dev/null; then
  printf 'gate policy accepted a v1 USD-M candidate outside the LOB-first contract\n' >&2
  exit 1
fi

jq '.markets.spot.raw_trade_count = 1' \
  "$tmp_dir/gate-v1.json" >"$tmp_dir/v1-with-raw-trades.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/v1-with-raw-trades.json" >/dev/null; then
  printf 'gate policy accepted v2 family evidence on a v1 tape candidate\n' >&2
  exit 1
fi

jq 'del(.markets.spot.tape_schema)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-tape-schema.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/missing-tape-schema.json" >/dev/null; then
  printf 'gate policy accepted evidence without a tape schema\n' >&2
  exit 1
fi

jq '.markets.spot.stream_types = ["aggTrade","depth@100ms"]' \
  "$tmp_dir/gate.json" >"$tmp_dir/legacy-stream-types.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/legacy-stream-types.json" >/dev/null; then
  printf 'gate policy accepted a v2 candidate declaring legacy stream types\n' >&2
  exit 1
fi

jq '.markets.spot.raw_trade_segments = 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/non-continuous-raw-trades.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/non-continuous-raw-trades.json" >/dev/null; then
  printf 'gate policy accepted raw trades from fewer than two segments\n' >&2
  exit 1
fi

jq '.markets.spot.raw_trade_count = 0' \
  "$tmp_dir/gate.json" >"$tmp_dir/zero-raw-trades.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/zero-raw-trades.json" >/dev/null; then
  printf 'gate policy accepted zero raw trades\n' >&2
  exit 1
fi

jq '.markets.spot.book_ticker_count = 0' \
  "$tmp_dir/gate.json" >"$tmp_dir/zero-book-tickers.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/zero-book-tickers.json" >/dev/null; then
  printf 'gate policy accepted zero book tickers\n' >&2
  exit 1
fi

jq 'del(.markets.spot.strict_raw_trade_continuity_readback)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-strict-raw-trade-readback.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/missing-strict-raw-trade-readback.json" >/dev/null; then
  printf 'gate policy accepted evidence without strict raw-trade continuity readback\n' >&2
  exit 1
fi

jq 'del(.markets.usdm.force_order_count)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-force-order-count.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/missing-force-order-count.json" >/dev/null; then
  printf 'gate policy accepted USD-M evidence without a force-order count\n' >&2
  exit 1
fi

jq '.markets.spot.force_order_count = 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/spot-force-orders.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/spot-force-orders.json" >/dev/null; then
  printf 'gate policy accepted force-order evidence on a spot candidate\n' >&2
  exit 1
fi

jq '.markets.spot.full_stream_coverage_verified = false' \
  "$tmp_dir/gate.json" >"$tmp_dir/unverified-full-stream-coverage.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/unverified-full-stream-coverage.json" >/dev/null; then
  printf 'gate policy accepted unverified full stream coverage\n' >&2
  exit 1
fi

jq '.markets.spot.all_stream_coverage_verified = false' \
  "$tmp_dir/gate.json" >"$tmp_dir/unverified-stream-coverage.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/unverified-stream-coverage.json" >/dev/null; then
  printf 'gate policy accepted unverified market stream coverage\n' >&2
  exit 1
fi

jq '.markets.spot.oss_roundtrip_evidence[0].stream_coverage_verified_count = 1199' \
  "$tmp_dir/gate.json" >"$tmp_dir/incomplete-segment-stream-coverage.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/incomplete-segment-stream-coverage.json" >/dev/null; then
  printf 'gate policy accepted incomplete segment stream coverage\n' >&2
  exit 1
fi

jq '.markets.spot.lob_reconnect_boundaries = 1
    | .markets.spot.oss_roundtrip_evidence[0].lob_reconnect_boundary = true' \
  "$tmp_dir/gate.json" >"$tmp_dir/pre-observation-reconnect.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/pre-observation-reconnect.json" >/dev/null; then
  printf 'gate policy accepted a pre-observation reconnect boundary\n' >&2
  exit 1
fi

wrong_bundle=$(printf '8%.0s' {1..64})
jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$wrong_bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null || {
  printf 'gate policy coupled evidence to the transition controller bundle\n' >&2
  exit 1
}

wrong_runtime_contract=$(printf '7%.0s' {1..64})
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$wrong_runtime_contract" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null; then
  printf 'gate policy accepted evidence from a different runtime contract\n' >&2
  exit 1
fi

jq '.markets.spot.oss_roundtrip_evidence[1].lob_capture_session_id = "session-2"' \
  "$tmp_dir/gate.json" >"$tmp_dir/mixed-lob-session.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/mixed-lob-session.json" >/dev/null; then
  printf 'gate policy accepted LOB evidence across a reconnect boundary\n' >&2
  exit 1
fi

jq '.markets.spot.oss_roundtrip_evidence[1] |=
      (.start_received_at_ns = 90000000300
       | .end_received_at_ns = 90000000400
       | .gap_from_previous_ns = 90000000100)
    | .markets.spot.max_segment_gap_ns = 90000000100' \
  "$tmp_dir/gate.json" >"$tmp_dir/excessive-segment-gap.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/excessive-segment-gap.json" >/dev/null; then
  printf 'gate policy accepted a segment gap over the continuity bound\n' >&2
  exit 1
fi

jq 'del(.markets.spot.oss_roundtrip_evidence[0].manifest_sha256)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-manifest-anchor.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/missing-manifest-anchor.json" >/dev/null; then
  printf 'gate policy accepted evidence without a manifest SHA anchor\n' >&2
  exit 1
fi

jq '.markets.usdm.oss_roundtrip_evidence[1].start_received_at_ns = 199' \
  "$tmp_dir/gate.json" >"$tmp_dir/overlapping-agg-trades.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/overlapping-agg-trades.json" >/dev/null; then
  printf 'gate policy accepted overlapping aggregate-trade segments\n' >&2
  exit 1
fi

wrong_artifact=$(printf 'f%.0s' {1..64})
if jq -e \
  --arg candidate_sha256 "$wrong_artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null; then
  printf 'gate policy accepted evidence from a different binary artifact\n' >&2
  exit 1
fi

wrong_source=$(printf '9%.0s' {1..40})
jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$wrong_source" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null || {
  printf 'gate policy coupled evidence to the transition controller source\n' >&2
  exit 1
}

jq '.markets.spot.health_samples = 1' "$tmp_dir/gate.json" >"$tmp_dir/short-sampling.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/short-sampling.json" >/dev/null; then
  printf 'gate policy accepted insufficient continuous health samples\n' >&2
  exit 1
fi

for market in spot usdm; do
  jq --arg market "$market" \
    '.markets[$market].max_health_silence_seconds = 91' \
    "$tmp_dir/gate.json" >"$tmp_dir/rotation-jitter-health-$market.json"
  jq -e \
    --arg candidate_sha256 "$artifact" \
    --arg runtime_contract_sha256 "$runtime_contract" \
    --arg deployment_bundle_sha256 "$bundle" \
    --arg deployment_source_revision "$source_revision" \
    -f "$POLICY" "$tmp_dir/rotation-jitter-health-$market.json" >/dev/null || {
    printf 'gate policy rejected a 91-second %s rotation jitter gap\n' "$market" >&2
    exit 1
  }

  jq --arg market "$market" \
    '.markets[$market].max_health_silence_seconds = 121' \
    "$tmp_dir/gate.json" >"$tmp_dir/stale-health-$market.json"
  if jq -e \
    --arg candidate_sha256 "$artifact" \
    --arg runtime_contract_sha256 "$runtime_contract" \
    --arg deployment_bundle_sha256 "$bundle" \
    --arg deployment_source_revision "$source_revision" \
    -f "$POLICY" "$tmp_dir/stale-health-$market.json" >/dev/null; then
    printf 'gate policy accepted a %s health freshness gap over 120 seconds\n' "$market" >&2
    exit 1
  fi
done

jq '.markets.spot.agg_trade_count = 0' \
  "$tmp_dir/gate.json" >"$tmp_dir/zero-agg-trades.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/zero-agg-trades.json" >/dev/null; then
  printf 'gate policy accepted zero aggregate trades\n' >&2
  exit 1
fi

jq 'del(.markets.spot.strict_trade_summary_readback)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-strict-trade-summary-readback.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/missing-strict-trade-summary-readback.json" >/dev/null; then
  printf 'gate policy accepted evidence without strict trade-summary readback\n' >&2
  exit 1
fi

jq 'del(.markets.spot.oss_roundtrip_evidence[0].success_uri)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-success-marker.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/missing-success-marker.json" >/dev/null; then
  printf 'gate policy accepted aggregate-trade evidence without a success marker\n' >&2
  exit 1
fi

jq '.markets.usdm.agg_trade_segments = 1' \
  "$tmp_dir/gate.json" >"$tmp_dir/non-continuous-agg-trades.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg runtime_contract_sha256 "$runtime_contract" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/non-continuous-agg-trades.json" >/dev/null; then
  printf 'gate policy accepted aggregate trades from fewer than two segments\n' >&2
  exit 1
fi

jq -n '{market:"spot",dataset:"spot_all",status:"synced",sequence_gaps:0,symbol_count:1200,
  snapshot_ready_count:1200,bridged_count:1200,stream_coverage_verified_count:1200,
  snapshot_only_symbols:[],all_symbols_bridged:true,all_stream_coverage_verified:true,
  full_stream_coverage_verified:true,
  pending_upload_segments:0,queue_saturated:false,
  disk_warning:false,upload_warning:false,updated_at_ns:200,session_id:"new-session"}' \
  >"$tmp_dir/runtime-health.json"
runtime_policy_accepts() {
  local health=$1 old_session=$2 minimum_updated_ns=$3
  local expected_market=${4:-spot} expected_dataset=${5:-spot_all}
  local minimum_symbols=${6:-1000}
  jq -e \
    --arg expected_market "$expected_market" \
    --arg expected_dataset "$expected_dataset" \
    --arg old_session "$old_session" \
    --argjson minimum_symbols "$minimum_symbols" \
    --argjson minimum_updated_ns "$minimum_updated_ns" \
    -f "$RUNTIME_POLICY" "$health" >/dev/null
}
runtime_policy_accepts "$tmp_dir/runtime-health.json" old-session 100
if runtime_policy_accepts "$tmp_dir/runtime-health.json" old-session 200; then
  printf 'runtime policy accepted health that was not newer than restart\n' >&2
  exit 1
fi
if runtime_policy_accepts "$tmp_dir/runtime-health.json" new-session 100; then
  printf 'runtime policy accepted a stale session\n' >&2
  exit 1
fi
jq '.all_stream_coverage_verified = false' \
  "$tmp_dir/runtime-health.json" >"$tmp_dir/unverified-runtime-stream-coverage.json"
if runtime_policy_accepts "$tmp_dir/unverified-runtime-stream-coverage.json" old-session 100; then
  printf 'runtime policy accepted unverified stream coverage\n' >&2
  exit 1
fi
jq '.stream_coverage_verified_count = 1199' \
  "$tmp_dir/runtime-health.json" >"$tmp_dir/incomplete-runtime-stream-coverage.json"
if runtime_policy_accepts "$tmp_dir/incomplete-runtime-stream-coverage.json" old-session 100; then
  printf 'runtime policy accepted incomplete stream coverage\n' >&2
  exit 1
fi
jq '.full_stream_coverage_verified = false' \
  "$tmp_dir/runtime-health.json" >"$tmp_dir/unverified-full-runtime-coverage.json"
if runtime_policy_accepts "$tmp_dir/unverified-full-runtime-coverage.json" old-session 100; then
  printf 'runtime policy accepted unverified full stream coverage\n' >&2
  exit 1
fi
jq 'del(.full_stream_coverage_verified)' \
  "$tmp_dir/runtime-health.json" >"$tmp_dir/v1-runtime-coverage.json"
runtime_policy_accepts "$tmp_dir/v1-runtime-coverage.json" old-session 100 || {
  printf 'runtime policy rejected a v1 collector without the full coverage field\n' >&2
  exit 1
}
jq '.market = "usdm"
    | .dataset = "usdm_perpetual_all"
    | .symbol_count = 100
    | .snapshot_ready_count = 100
    | .bridged_count = 100
    | .stream_coverage_verified_count = 100' \
  "$tmp_dir/runtime-health.json" >"$tmp_dir/usdm-runtime-health.json"
runtime_policy_accepts "$tmp_dir/usdm-runtime-health.json" old-session 100 \
  usdm usdm_perpetual_all 100
jq '.symbol_count = 101
    | .snapshot_ready_count = 101
    | .bridged_count = 101
    | .stream_coverage_verified_count = 101' \
  "$tmp_dir/usdm-runtime-health.json" >"$tmp_dir/usdm-101-runtime-health.json"
if runtime_policy_accepts "$tmp_dir/usdm-101-runtime-health.json" old-session 100 \
  usdm usdm_perpetual_all 100; then
  printf 'runtime policy accepted 101 USD-M symbols\n' >&2
  exit 1
fi
for field in symbol_count snapshot_ready_count bridged_count stream_coverage_verified_count; do
  jq --arg field "$field" '.[$field] = "1200"' \
    "$tmp_dir/runtime-health.json" >"$tmp_dir/quoted-count.json"
  if runtime_policy_accepts "$tmp_dir/quoted-count.json" old-session 100; then
    printf 'runtime policy accepted quoted %s\n' "$field" >&2
    exit 1
  fi
  jq --arg field "$field" '.[$field] = 1200.5' \
    "$tmp_dir/runtime-health.json" >"$tmp_dir/fractional-count.json"
  if runtime_policy_accepts "$tmp_dir/fractional-count.json" old-session 100; then
    printf 'runtime policy accepted fractional %s\n' "$field" >&2
    exit 1
  fi
done
jq '.market = "usdm"' "$tmp_dir/runtime-health.json" >"$tmp_dir/cross-market.json"
if runtime_policy_accepts "$tmp_dir/cross-market.json" old-session 100; then
  printf 'runtime policy accepted a cross-market health payload\n' >&2
  exit 1
fi
jq '.dataset = "usdm_perpetual_all"' \
  "$tmp_dir/runtime-health.json" >"$tmp_dir/cross-dataset.json"
if runtime_policy_accepts "$tmp_dir/cross-dataset.json" old-session 100; then
  printf 'runtime policy accepted a cross-dataset health payload\n' >&2
  exit 1
fi

restore_recovery_scheduler_body="$tmp_dir/restore-recovery-scheduler.sh"
{
  sed -n '/^reset_failed_recovery_units()/,/^}/p' "$CUTOVER"
  sed -n '/^restore_previous_recovery_scheduler()/,/^}/p' "$CUTOVER"
} >"$restore_recovery_scheduler_body"
enable_recovery_scheduler_body="$tmp_dir/enable-recovery-scheduler.sh"
{
  sed -n '/^reset_failed_recovery_units()/,/^}/p' "$CUTOVER"
  sed -n '/^enable_candidate_recovery_scheduler()/,/^}/p' "$CUTOVER"
} >"$enable_recovery_scheduler_body"
quiesce_recovery_scheduler_body="$tmp_dir/quiesce-recovery-scheduler.sh"
sed -n '/^quiesce_recovery_scheduler()/,/^}/p' "$CUTOVER" \
  >"$quiesce_recovery_scheduler_body"
rollback_body="$tmp_dir/rollback.sh"
sed -n '/^rollback_after_failure()/,/^}/p' "$CUTOVER" >"$rollback_body"
remove_health_body="$tmp_dir/remove-installed-health.sh"
sed -n '/^remove_installed_health_script()/,/^}/p' "$CUTOVER" \
  >"$remove_health_body"
stage_new_host_health_body="$tmp_dir/stage-new-host-health.sh"
sed -n '/^stage_new_host_health_for_rollback()/,/^}/p' "$CUTOVER" \
  >"$stage_new_host_health_body"
restore_new_host_health_body="$tmp_dir/restore-new-host-health.sh"
sed -n '/^restore_new_host_health_after_failure()/,/^}/p' "$CUTOVER" \
  >"$restore_new_host_health_body"
production_predicate_body="$tmp_dir/production-is-fail-closed.sh"
sed -n '/^production_is_fail_closed()/,/^}/p' "$CUTOVER" \
  >"$production_predicate_body"
recovery_predicate_body="$tmp_dir/recovery-is-fail-closed.sh"
sed -n '/^recovery_is_fail_closed()/,/^}/p' "$CUTOVER" \
  >"$recovery_predicate_body"
partial_predicate_body="$tmp_dir/partial-spot-is-restored.sh"
sed -n '/^partial_spot_runtime_is_restored()/,/^}/p' "$CUTOVER" \
  >"$partial_predicate_body"
partial_usdm_predicate_body="$tmp_dir/partial-usdm-is-restored.sh"
sed -n '/^partial_usdm_runtime_is_restored()/,/^}/p' "$CUTOVER" \
  >"$partial_usdm_predicate_body"
release_match_body="$tmp_dir/unit-matches-release.sh"
sed -n '/^unit_matches_release()/,/^}/p' "$CUTOVER" >"$release_match_body"
run_release_match_fixture() (
  local current_restarts=$1 expected_restarts=$2
  local current_invocation=11111111111111111111111111111111
  systemctl() {
    case "$1:$2:${3:-}:${4:-}" in
      is-active:--quiet:production-spot:) return 0 ;;
      is-enabled:--quiet:production-spot:) return 0 ;;
      show:production-spot:--property=NRestarts:--value) printf '%s\n' "$current_restarts" ;;
      show:production-spot:--property=InvocationID:--value) printf '%s\n' "$current_invocation" ;;
      show:production-spot:--property=MainPID:--value) printf '123\n' ;;
      *) return 1 ;;
    esac
  }
  readlink() { [[ $1 == -f && $2 == /proc/123/exe ]] && printf '/old-binary\n'; }
  # shellcheck disable=SC1090
  . "$release_match_body"
  if [[ $expected_restarts == default ]]; then
    unit_matches_release production-spot /old-binary true
  else
    unit_matches_release production-spot /old-binary true \
      "$expected_restarts" "$current_invocation"
  fi
)
run_release_match_fixture 0 default
run_release_match_fixture 1 1
if run_release_match_fixture 1 default; then
  printf 'release matcher weakened the default zero-restart contract\n' >&2
  exit 1
fi
if run_release_match_fixture 2 1; then
  printf 'release matcher accepted a changed restart baseline\n' >&2
  exit 1
fi
spot_wait_body="$tmp_dir/wait-for-spot-release-health.sh"
sed -n '/^wait_for_spot_release_health()/,/^}/p' "$CUTOVER" >"$spot_wait_body"
usdm_wait_body="$tmp_dir/wait-for-usdm-release-health.sh"
sed -n '/^wait_for_usdm_release_health()/,/^}/p' "$CUTOVER" >"$usdm_wait_body"
run_spot_wait_fixture() (
  PRODUCTION_UNITS=(production-spot production-usdm)
  HEALTH_TIMEOUT_SECONDS=20
  SECONDS=0
  health_checks=0
  systemctl() { [[ $1 == is-active && ${!#} == production-spot ]]; }
  health_ready_for_release() {
    health_checks=$((health_checks + 1))
    ((health_checks >= 3))
  }
  unit_matches_release() { return 0; }
  sleep() { SECONDS=$((SECONDS + $1)); }
  # shellcheck disable=SC1090
  . "$spot_wait_body"
  wait_for_spot_release_health old-binary old-session 123
  printf '%s\n' "$health_checks"
)
[[ $(run_spot_wait_fixture) == 3 ]]
run_usdm_wait_fixture() (
  PRODUCTION_UNITS=(production-spot production-usdm)
  HEALTH_TIMEOUT_SECONDS=20
  SECONDS=0
  health_checks=0
  systemctl() { [[ $1 == is-active && ${!#} == production-usdm ]]; }
  health_ready_for_release() {
    health_checks=$((health_checks + 1))
    ((health_checks >= 3))
  }
  unit_matches_release() { return 0; }
  sleep() { SECONDS=$((SECONDS + $1)); }
  # shellcheck disable=SC1090
  . "$usdm_wait_body"
  wait_for_usdm_release_health old-binary 400 old-session 123
  printf '%s\n' "$health_checks"
)
[[ $(run_usdm_wait_fixture) == 3 ]]
start_line=$(grep -n 'systemctl start "${PRODUCTION_UNITS\[@\]}"' "$rollback_body" | tail -1 | cut -d: -f1)
clear_line=$(grep -n 'clear_health_before_restart' "$rollback_body" | cut -d: -f1)
health_line=$(grep -n 'wait_for_release_health' "$rollback_body" | cut -d: -f1)
enable_line=$(grep -n 'systemctl enable "${PRODUCTION_UNITS\[@\]}"' "$rollback_body" | cut -d: -f1)
recovery_restore_line=$(grep -n 'if restore_previous_recovery_scheduler; then' \
  "$rollback_body" | cut -d: -f1)
((clear_line < start_line && start_line < health_line && health_line < enable_line)) || {
  printf 'rollback no longer follows clear stale health -> start -> verify -> enable\n' >&2
  exit 1
}
(( enable_line < recovery_restore_line )) || {
  printf 'rollback restores recovery scheduling before old production health is verified\n' >&2
  exit 1
}
grep -Fq 'runtime_matches_release "$OLD_BINARY" true' "$rollback_body"
grep -Fq '"$rollback_started_ns"' "$rollback_body"
grep -Fq 'previous-release-health-unverified-disabled' "$rollback_body"
grep -Fq 'previous-release-restored-contained' "$rollback_body"
grep -Fq 'systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}"' "$rollback_body"
grep -Fq 'ROLLBACK_RESULT=new-host-containment-failed' "$rollback_body"
grep -Fq 'restore_allowlisted_production_dropins' "$rollback_body"
grep -Fq 'binance-lob-archiver@spot.service' "$CUTOVER"
grep -Fq 'binance-lob-archiver@usdm.service' "$CUTOVER"
grep -Fq 'contained-upgrade' "$CUTOVER"
grep -Fq 'capture_existing_production_identity contained-upgrade' "$CUTOVER"
new_host_health_stage_line=$(grep -nF '  stage_new_host_health_for_rollback' "$CUTOVER" | cut -d: -f1)
transition_started_line=$(grep -nF 'TRANSITION_STARTED=1' "$CUTOVER" | cut -d: -f1)
[[ -n $new_host_health_stage_line && -n $transition_started_line \
  && $new_host_health_stage_line -lt $transition_started_line ]] || {
  printf 'new-host health snapshot no longer completes before transition mutation\n' >&2
  exit 1
}
grep -Fq 'partial-contained-spot-live' "$CUTOVER"
grep -Fq 'capture_existing_production_identity partial-contained-spot-live' "$CUTOVER"
grep -Fq 'partial-contained-usdm-live' "$CUTOVER"
grep -Fq 'capture_existing_production_identity partial-contained-usdm-live' "$CUTOVER"
grep -Fq 'partial_spot_runtime_is_restored' "$CUTOVER"
grep -Fq 'partial_usdm_runtime_is_restored' "$CUTOVER"
grep -Fq 'wait_for_spot_release_health' "$CUTOVER"
grep -Fq 'wait_for_usdm_release_health' "$CUTOVER"
grep -Fq 'previous-spot-restored-usdm-contained' "$CUTOVER"
grep -Fq 'previous-usdm-restored-spot-contained' "$CUTOVER"
grep -Fq "fail 'new host must not retain a production USD-M drop-in'" "$CUTOVER"
grep -Fq 'binance-lob-archiver-production@usdm.service.d/10-memory.conf' "$CUTOVER"
grep -Fq 'legacy collector unit must be disabled before cutover' "$CUTOVER"
grep -Fq 'production unit remained enabled after disable' "$CUTOVER"
grep -Fq 'candidate production service retained an unexpected systemd drop-in' "$CUTOVER"
grep -Fq 'validate_existing_production_dropins' "$CUTOVER"
grep -Fq 'remove_allowlisted_production_dropins_for_candidate' "$CUTOVER"

atomic_helpers_body="$tmp_dir/atomic-cutover-helpers.sh"
{
  sed -n '/^atomic_install()/,/^}/p' "$CUTOVER"
  sed -n '/^atomic_symlink()/,/^}/p' "$CUTOVER"
} >"$atomic_helpers_body"
atomic_helpers_root="$tmp_dir/atomic-cutover-helpers"
mkdir -p "$atomic_helpers_root"
printf 'old bytes\n' >"$atomic_helpers_root/source"
printf 'new bytes\n' >"$atomic_helpers_root/target"
run_atomic_helpers_success_fixture() (
  mv() { command mv -f "$2" "$3"; }
  # shellcheck disable=SC1090
  . "$atomic_helpers_body"
  atomic_install 0640 "$atomic_helpers_root/source" "$atomic_helpers_root/installed"
  cmp -s "$atomic_helpers_root/source" "$atomic_helpers_root/installed"
  atomic_symlink "$atomic_helpers_root/target" "$atomic_helpers_root/current"
  [[ $(readlink -f "$atomic_helpers_root/current") \
    == "$(readlink -f "$atomic_helpers_root/target")" ]]
)
run_atomic_helpers_fail_closed_fixture() (
  mv() { return 0; }
  # shellcheck disable=SC1090
  . "$atomic_helpers_body"
  printf 'stale bytes\n' >"$atomic_helpers_root/stale-installed"
  if atomic_install 0640 \
    "$atomic_helpers_root/source" "$atomic_helpers_root/stale-installed"; then
    printf 'atomic install accepted unread-back bytes\n' >&2
    exit 1
  fi
  printf 'wrong target\n' >"$atomic_helpers_root/wrong-target"
  ln -s "$atomic_helpers_root/wrong-target" "$atomic_helpers_root/stale-current"
  if atomic_symlink "$atomic_helpers_root/target" "$atomic_helpers_root/stale-current"; then
    printf 'atomic symlink accepted an unread-back target\n' >&2
    exit 1
  fi
)
run_atomic_helpers_success_fixture
run_atomic_helpers_fail_closed_fixture

host_state_dispatch=$(sed -n '/^if (( active_count == 2/,/^fi$/p' "$CUTOVER")
grep -Fq 'OLD_MODE=new-host' <<<"$host_state_dispatch"
grep -Fq '(( PRODUCTION_USDM_MEMORY_DROPIN_PRESENT == 0 ))' <<<"$host_state_dispatch"
grep -Fq $'capture_existing_production_identity contained-upgrade\n  DRAIN_REQUIRED=1' \
  <<<"$host_state_dispatch"
grep -Fq 'capture_existing_production_identity partial-contained-spot-live' \
  <<<"$host_state_dispatch"
grep -Fq 'capture_existing_production_identity partial-contained-usdm-live' \
  <<<"$host_state_dispatch"
grep -Fq '$spot_active_state == active && $spot_enabled_state == enabled' \
  <<<"$host_state_dispatch"
grep -Fq '$usdm_enabled_state == masked || $usdm_enabled_state == masked-runtime' \
  <<<"$host_state_dispatch"
grep -Fq '$spot_upload_enabled_state == static' <<<"$host_state_dispatch"
grep -Fq '$spot_upload_enabled_state == masked' <<<"$host_state_dispatch"
grep -Fq '$spot_upload_enabled_state == masked-runtime' <<<"$host_state_dispatch"
grep -Fq '$usdm_upload_enabled_state == masked' <<<"$host_state_dispatch"
grep -Fq '$usdm_upload_enabled_state == static' <<<"$host_state_dispatch"
grep -Fq 'previous_spot_restarts' "$CUTOVER"
grep -Fq 'previous_spot_invocation_id' "$CUTOVER"
grep -Fq 'previous_usdm_restarts' "$CUTOVER"
grep -Fq 'previous_usdm_invocation_id' "$CUTOVER"
grep -Fq 'previous_recovery_timers_enabled' "$CUTOVER"
grep -Fq 'previous_recovery_timers_masked_runtime' "$CUTOVER"
grep -Fq 'previous_recovery_services_masked_runtime' "$CUTOVER"
grep -Fq 'OLD_RECOVERY_TIMER_SPOT_ENABLED=0' "$CUTOVER"
grep -Fq 'OLD_RECOVERY_TIMER_USDM_ENABLED=0' "$CUTOVER"
grep -Fq 'OLD_RECOVERY_TIMER_SPOT_MASKED_RUNTIME=0' "$CUTOVER"
grep -Fq 'OLD_RECOVERY_TIMER_USDM_MASKED_RUNTIME=0' "$CUTOVER"
grep -Fq 'OLD_RECOVERY_UNIT_SPOT_MASKED_RUNTIME=0' "$CUTOVER"
grep -Fq 'OLD_RECOVERY_UNIT_USDM_MASKED_RUNTIME=0' "$CUTOVER"
grep -Fq 'old production recovery timers do not match partial-contained-spot-live' "$CUTOVER"
grep -Fq 'old production recovery timers do not match partial-contained-usdm-live' "$CUTOVER"
host_state_dispatch_file="$tmp_dir/host-state-dispatch.sh"
printf '%s\n' "$host_state_dispatch" >"$host_state_dispatch_file"
capture_existing_production_identity_body="$tmp_dir/capture-existing-production-identity.sh"
sed -n '/^capture_existing_production_identity()/,/^}/p' \
  "$CUTOVER" >"$capture_existing_production_identity_body"
run_capture_existing_production_identity_fixture() (
  local mode=$1
  local spot_timer_state=${2:-disabled}
  local usdm_timer_state=${3:-disabled}
  local spot_service_state=${4:-static}
  local usdm_service_state=${5:-static}
  local old_sha runtime_contract state
  old_sha=$(printf 'a%.0s' {1..64})
  RELEASE_ROOT=$(mkdir -p "$tmp_dir/capture-release-root" && cd "$tmp_dir/capture-release-root" && pwd -P)
  CONTROLLER_RELEASE_ROOT="$tmp_dir/capture-controller-root"
  ACTIVE_CONTROLLER_LINK="$CONTROLLER_RELEASE_ROOT/active"
  PRODUCTION_LINK="$tmp_dir/capture-production-link-$mode-$spot_timer_state-$usdm_timer_state"
  PRODUCTION_UNITS=(production-spot production-usdm)
  RECOVERY_TIMERS=(recovery-spot recovery-usdm)
  RECOVERY_UNITS=(recovery-spot-service recovery-usdm-service)
  CANDIDATE_SHA256=$(printf 'c%.0s' {1..64})
  OLD_RECOVERY_TIMERS_ENABLED=0
  OLD_RECOVERY_TIMER_SPOT_ENABLED=0
  OLD_RECOVERY_TIMER_USDM_ENABLED=0
  OLD_RECOVERY_TIMER_SPOT_MASKED_RUNTIME=0
  OLD_RECOVERY_TIMER_USDM_MASKED_RUNTIME=0
  OLD_RECOVERY_UNIT_SPOT_MASKED_RUNTIME=0
  OLD_RECOVERY_UNIT_USDM_MASKED_RUNTIME=0
  runtime_contract=$(printf 'd%.0s' {1..64})
  mkdir -p "$RELEASE_ROOT/$old_sha/deployment"
  : >"$RELEASE_ROOT/$old_sha/binance-lob-archiver"
  jq -n --arg runtime "$runtime_contract" \
    '{runtime_contract_sha256:$runtime}' >"$RELEASE_ROOT/$old_sha/release.json"
  ln -sf "$RELEASE_ROOT/$old_sha/binance-lob-archiver" \
    "$PRODUCTION_LINK"
  systemctl() {
    if [[ $1 == is-enabled ]]; then
      case "${!#}" in
        recovery-spot) state=$spot_timer_state ;;
        recovery-usdm) state=$usdm_timer_state ;;
        recovery-spot-service) state=$spot_service_state ;;
        recovery-usdm-service) state=$usdm_service_state ;;
        *) return 1 ;;
      esac
      if [[ ${2:-} == --quiet ]]; then
        [[ $state == enabled ]]
        return
      fi
      printf '%s\n' "$state"
      [[ $state == enabled ]]
      return
    fi
    return 1
  }
  systemctl_value() {
    case "$1:$2" in
      production-spot:NRestarts) printf '1\n' ;;
      production-spot:InvocationID) printf '11111111111111111111111111111111\n' ;;
      production-usdm:NRestarts) printf '2\n' ;;
      production-usdm:InvocationID) printf '22222222222222222222222222222222\n' ;;
      *) return 1 ;;
    esac
  }
  stage_existing_deployment_for_rollback() {
    OLD_DEPLOYMENT="$tmp_dir/capture-old-deployment"
  }
  sha256sum() { return 0; }
  fail() { printf '%s\n' "$*" >&2; exit 1; }
  # shellcheck disable=SC1090
  . "$capture_existing_production_identity_body"
  capture_existing_production_identity "$mode"
  printf '%s %s %s %s %s %s %s\n' \
    "$OLD_RECOVERY_TIMERS_ENABLED" \
    "$OLD_RECOVERY_TIMER_SPOT_ENABLED" \
    "$OLD_RECOVERY_TIMER_USDM_ENABLED" \
    "$OLD_RECOVERY_TIMER_SPOT_MASKED_RUNTIME" \
    "$OLD_RECOVERY_TIMER_USDM_MASKED_RUNTIME" \
    "$OLD_RECOVERY_UNIT_SPOT_MASKED_RUNTIME" \
    "$OLD_RECOVERY_UNIT_USDM_MASKED_RUNTIME"
)
[[ $(run_capture_existing_production_identity_fixture contained-upgrade) == '0 0 0 0 0 0 0' ]]
[[ $(run_capture_existing_production_identity_fixture contained-upgrade enabled enabled) == '2 1 1 0 0 0 0' ]]
[[ $(run_capture_existing_production_identity_fixture upgrade masked-runtime enabled masked-runtime) == '1 0 1 1 0 1 0' ]]
[[ $(run_capture_existing_production_identity_fixture partial-contained-spot-live) == '0 0 0 0 0 0 0' ]]
[[ $(run_capture_existing_production_identity_fixture partial-contained-spot-live enabled) == '1 1 0 0 0 0 0' ]]
[[ $(run_capture_existing_production_identity_fixture partial-contained-spot-live masked-runtime) == '0 0 0 1 0 0 0' ]]
[[ $(run_capture_existing_production_identity_fixture partial-contained-spot-live enabled enabled) == '2 1 1 0 0 0 0' ]]
[[ $(run_capture_existing_production_identity_fixture partial-contained-usdm-live) == '0 0 0 0 0 0 0' ]]
[[ $(run_capture_existing_production_identity_fixture partial-contained-usdm-live disabled enabled) == '1 0 1 0 0 0 0' ]]
[[ $(run_capture_existing_production_identity_fixture partial-contained-usdm-live masked-runtime enabled) == '1 0 1 1 0 0 0' ]]
[[ $(run_capture_existing_production_identity_fixture partial-contained-usdm-live enabled enabled) == '2 1 1 0 0 0 0' ]]
if run_capture_existing_production_identity_fixture partial-contained-spot-live disabled enabled \
  >"$tmp_dir/capture-spot-wrong-timer.out" 2>&1; then
  printf 'partial-contained spot cutover accepted the wrong recovery timer\n' >&2
  exit 1
fi
grep -Fq 'old production recovery timers do not match partial-contained-spot-live' \
  "$tmp_dir/capture-spot-wrong-timer.out"
if run_capture_existing_production_identity_fixture partial-contained-usdm-live enabled disabled \
  >"$tmp_dir/capture-usdm-wrong-timer.out" 2>&1; then
  printf 'partial-contained USD-M cutover accepted the wrong recovery timer\n' >&2
  exit 1
fi
grep -Fq 'old production recovery timers do not match partial-contained-usdm-live' \
  "$tmp_dir/capture-usdm-wrong-timer.out"
write_evidence_body="$tmp_dir/write-evidence.sh"
sed -n '/^write_evidence()/,/^}/p' "$CUTOVER" >"$write_evidence_body"
run_write_evidence_fixture() (
  local target="$tmp_dir/write-evidence-target"
  local link="$tmp_dir/write-evidence-link"
  EVIDENCE_DIR="$tmp_dir/write-evidence"
  STARTED_AT=2026-08-24T00:00:00Z
  RESULT=passed
  STEP='done'
  FAILURE_REASON=
  ROLLBACK_RESULT=not-needed
  ROLLBACK_DEPLOYMENT_MANIFEST_SHA256=
  HEALTH_SCRIPT_ROLLBACK_RESULT=not-needed
  OLD_HEALTH_ASSET_PRESENT=1
  OLD_HEALTH_SCRIPT_SHA256=$(printf 'a%.0s' {1..64})
  OLD_HEALTH_SCRIPT_SNAPSHOT=/evidence/rollback-health.sh
  OLD_HEALTH_ROLLBACK_SOURCE=rollback-deployment
  CANDIDATE_SHA256=$(printf 'c%.0s' {1..64})
  DEPLOYMENT_SOURCE_REVISION=$(printf 'd%.0s' {1..40})
  DEPLOYMENT_BUNDLE_SHA256=$(printf 'e%.0s' {1..64})
  RUNTIME_CONTRACT_SHA256=$(printf 'b%.0s' {1..64})
  OLD_SHA256=$(printf 'f%.0s' {1..64})
  OLD_SPOT_RESTARTS=1
  OLD_SPOT_INVOCATION_ID=11111111111111111111111111111111
  OLD_USDM_RESTARTS=2
  OLD_USDM_INVOCATION_ID=22222222222222222222222222222222
  OLD_RECOVERY_TIMER_SPOT_ENABLED=1
  OLD_RECOVERY_TIMER_USDM_ENABLED=0
  OLD_RECOVERY_TIMER_SPOT_MASKED_RUNTIME=0
  OLD_RECOVERY_TIMER_USDM_MASKED_RUNTIME=1
  OLD_RECOVERY_UNIT_SPOT_MASKED_RUNTIME=1
  OLD_RECOVERY_UNIT_USDM_MASKED_RUNTIME=0
  OLD_MODE=partial-contained-spot-live
  PRODUCTION_UNITS=(production-spot production-usdm)
  PRODUCTION_LINK="$link"
  mkdir -p "$EVIDENCE_DIR"
  : >"$target"
  ln -sf "$target" "$link"
  unit_active_json() {
    if [[ $1 == production-spot ]]; then
      printf '{"active":true}\n'
    else
      printf '{"active":false}\n'
    fi
  }
  date() { [[ $1 == -u && $2 == +%Y-%m-%dT%H:%M:%SZ ]] && printf '2026-08-24T00:05:00Z\n'; }
  mv() { command mv -f "$2" "$3"; }
  # shellcheck disable=SC1090
  . "$write_evidence_body"
  write_evidence
  jq -e '
    .previous_recovery_timers_enabled == {spot:true, usdm:false}
    and .previous_recovery_timers_masked_runtime == {spot:false, usdm:true}
    and .previous_recovery_services_masked_runtime == {spot:true, usdm:false}
    and .previous_health_script.present == true
    and .previous_health_script.rollback_source == "rollback-deployment"
    and .previous_health_script.sha256 == "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    and .health_script_rollback_result == "not-needed"
    and .runtime_contract_sha256 == "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
    and .host_mode == "partial-contained-spot-live"
  ' "$EVIDENCE_DIR/cutover.json" >/dev/null
)
run_write_evidence_fixture

run_restore_recovery_scheduler_fixture() (
  local spot_enabled=${1:-0}
  local usdm_enabled=${2:-0}
  local spot_masked_runtime=${3:-0}
  local usdm_masked_runtime=${4:-0}
  local spot_service_masked_runtime=${5:-0}
  local usdm_service_masked_runtime=${6:-0}
  local broken_masked_runtime_unit=${7:-}
  local unit
  declare -A enabled_now=() active_now=() masked_runtime_now=()
  RECOVERY_TIMERS=(recovery-spot recovery-usdm)
  RECOVERY_UNITS=(recovery-spot-service recovery-usdm-service)
  OLD_RECOVERY_TIMER_SPOT_ENABLED=$spot_enabled
  OLD_RECOVERY_TIMER_USDM_ENABLED=$usdm_enabled
  OLD_RECOVERY_TIMER_SPOT_MASKED_RUNTIME=$spot_masked_runtime
  OLD_RECOVERY_TIMER_USDM_MASKED_RUNTIME=$usdm_masked_runtime
  OLD_RECOVERY_UNIT_SPOT_MASKED_RUNTIME=$spot_service_masked_runtime
  OLD_RECOVERY_UNIT_USDM_MASKED_RUNTIME=$usdm_service_masked_runtime
  for unit in "${RECOVERY_TIMERS[@]}" "${RECOVERY_UNITS[@]}"; do
    enabled_now[$unit]=0
    active_now[$unit]=0
    masked_runtime_now[$unit]=1
  done
  systemctl() {
    case "$1" in
      disable)
        shift
        [[ ${1:-} == --now ]] && shift
        for unit in "$@"; do
          enabled_now[$unit]=0
          active_now[$unit]=0
        done
        ;;
      stop)
        shift
        for unit in "$@"; do active_now[$unit]=0; done
        ;;
      unmask)
        [[ ${2:-} == --runtime ]] || return 1
        shift 2
        for unit in "$@"; do masked_runtime_now[$unit]=0; done
        ;;
      is-failed) return 1 ;;
      reset-failed) return 1 ;;
      enable)
        shift
        [[ ${1:-} == --now ]] && shift
        for unit in "$@"; do
          enabled_now[$unit]=1
          active_now[$unit]=1
          masked_runtime_now[$unit]=0
        done
        ;;
      mask)
        [[ ${2:-} == --runtime ]] || return 1
        shift 2
        for unit in "$@"; do
          enabled_now[$unit]=0
          active_now[$unit]=0
          masked_runtime_now[$unit]=1
        done
        ;;
      is-enabled)
        if [[ ${2:-} == --quiet ]]; then
          (( enabled_now[${!#}] ))
          return $?
        fi
        unit=${!#}
        if (( enabled_now[$unit] )); then
          printf 'enabled\n'
          return 0
        fi
        if (( masked_runtime_now[$unit] )) \
          && [[ $broken_masked_runtime_unit != "$unit" ]]; then
          printf 'masked-runtime\n'
          return 1
        fi
        printf 'disabled\n'
        return 1
        ;;
      is-active)
        (( active_now[${!#}] ))
        ;;
      *) return 0 ;;
    esac
  }
  # shellcheck disable=SC1090
  . "$restore_recovery_scheduler_body"
  restore_previous_recovery_scheduler || return 1
  (( masked_runtime_now[recovery-spot-service] == spot_service_masked_runtime ))
  (( masked_runtime_now[recovery-usdm-service] == usdm_service_masked_runtime ))
  (( enabled_now[recovery-spot] == spot_enabled ))
  (( enabled_now[recovery-usdm] == usdm_enabled ))
  (( masked_runtime_now[recovery-spot] == spot_masked_runtime ))
  (( masked_runtime_now[recovery-usdm] == usdm_masked_runtime ))
)
run_restore_recovery_scheduler_fixture 0 1 1 0 1 0
if run_restore_recovery_scheduler_fixture 0 1 1 0 1 0 recovery-spot-service; then
  printf 'restore_previous_recovery_scheduler accepted a broken service mask readback\n' >&2
  exit 1
fi

run_enable_candidate_recovery_scheduler_fixture() (
  local unit
  declare -A enabled_now=() active_now=() masked_runtime_now=() loaded_now=() reset_now=()
  RECOVERY_TIMERS=(recovery-spot recovery-usdm)
  RECOVERY_UNITS=(recovery-spot-service recovery-usdm-service)
  for unit in "${RECOVERY_TIMERS[@]}" "${RECOVERY_UNITS[@]}"; do
    enabled_now[$unit]=0
    active_now[$unit]=0
    masked_runtime_now[$unit]=1
    loaded_now[$unit]=0
    reset_now[$unit]=0
  done
  loaded_now[recovery-spot-service]=1
  systemctl() {
    case "$1" in
      unmask)
        shift 2
        for unit in "$@"; do masked_runtime_now[$unit]=0; done
        ;;
      list-units)
        unit=${!#}
        (( loaded_now[$unit] )) \
          && printf '%s loaded inactive dead fixture\n' "$unit"
        return 0
        ;;
      reset-failed)
        unit=$2
        (( loaded_now[$unit] )) || return 1
        reset_now[$unit]=1
        ;;
      enable)
        shift 2
        for unit in "$@"; do
          enabled_now[$unit]=1
          active_now[$unit]=1
        done
        ;;
      is-enabled)
        unit=${!#}
        if [[ ${2:-} == --quiet ]]; then
          (( enabled_now[$unit] ))
          return $?
        fi
        if (( masked_runtime_now[$unit] )); then
          printf 'masked-runtime\n'
        else
          printf 'static\n'
        fi
        return 1
        ;;
      is-active) (( active_now[${!#}] )) ;;
      *) return 1 ;;
    esac
  }
  # shellcheck disable=SC1090
  . "$enable_recovery_scheduler_body"
  enable_candidate_recovery_scheduler
  (( masked_runtime_now[recovery-spot-service] == 0 ))
  (( masked_runtime_now[recovery-usdm-service] == 0 ))
  (( reset_now[recovery-spot-service] == 1 ))
  (( reset_now[recovery-usdm-service] == 0 ))
  (( enabled_now[recovery-spot] == 1 && active_now[recovery-spot] == 1 ))
  (( enabled_now[recovery-usdm] == 1 && active_now[recovery-usdm] == 1 ))
)
run_enable_candidate_recovery_scheduler_fixture

run_partial_host_state_fixture() (
  local usdm_enabled_state=$1 usdm_main_pid=${2:-0}
  local spot_upload_enabled_state=${3:-masked-runtime}
  local initial_usdm_upload_enabled_state=${4:-masked-runtime}
  local fixed_now_ns=2000000000000
  PRODUCTION_UNITS=(production-spot production-usdm)
  UPLOAD_UNITS=(upload-spot upload-usdm)
  PRODUCTION_LINK="$tmp_dir/partial-production-$usdm_enabled_state-$usdm_main_pid-$spot_upload_enabled_state-$initial_usdm_upload_enabled_state"
  ln -s "$tmp_dir/old-production-binary" "$PRODUCTION_LINK"
  active_count=1
  enabled_count=1
  spot_active_state=active
  spot_enabled_state=enabled
  usdm_active_state=inactive
  usdm_upload_enabled_state=$initial_usdm_upload_enabled_state
  MASK_USDM_UPLOAD_FOR_TRANSITION=0
  HEALTH_TIMEOUT_SECONDS=300
  PRODUCTION_USDM_MEMORY_DROPIN_PRESENT=0
  DRAIN_REQUIRED=0
  OLD_MODE=
  OLD_BINARY=
  systemctl_value() {
    case "$1:$2" in
      production-usdm:SubState) printf 'dead\n' ;;
      production-usdm:MainPID) printf '%s\n' "$usdm_main_pid" ;;
      *) return 1 ;;
    esac
  }
  capture_existing_production_identity() {
    OLD_MODE=$1
    OLD_BINARY="$tmp_dir/old-production-binary"
    OLD_SPOT_RESTARTS=1
    OLD_SPOT_INVOCATION_ID=11111111111111111111111111111111
  }
  unit_matches_release() {
    [[ $1 == production-spot && $2 == "$OLD_BINARY" && $3 == true \
      && $4 == 1 && $5 == "$OLD_SPOT_INVOCATION_ID" ]]
  }
  date() { [[ $1 == +%s%N ]] && printf '%s\n' "$fixed_now_ns"; }
  health_ready_for_release() {
    [[ $1 == spot && $2 == 1000 && -z $3 \
      && $4 == $((fixed_now_ns - HEALTH_TIMEOUT_SECONDS * 1000000000)) ]]
  }
  require_empty_segment_spool() { return 1; }
  fail() { printf '%s\n' "$*" >&2; exit 1; }
  if ! . "$host_state_dispatch_file"; then
    return 1
  fi
  printf '%s %s %s\n' \
    "$OLD_MODE" "$DRAIN_REQUIRED" "$MASK_USDM_UPLOAD_FOR_TRANSITION"
)
[[ $(run_partial_host_state_fixture masked-runtime) \
  == 'partial-contained-spot-live 1 0' ]]
[[ $(run_partial_host_state_fixture masked-runtime 0 masked-runtime static) \
  == 'partial-contained-spot-live 1 1' ]]
run_partial_usdm_host_state_fixture() (
  local spot_enabled_state=$1 spot_main_pid=${2:-0}
  local usdm_upload_enabled_state=${3:-masked-runtime}
  local initial_spot_upload_enabled_state=${4:-masked-runtime}
  local fixed_now_ns=2000000000000
  PRODUCTION_UNITS=(production-spot production-usdm)
  UPLOAD_UNITS=(upload-spot upload-usdm)
  PRODUCTION_LINK="$tmp_dir/partial-usdm-production-$spot_enabled_state-$spot_main_pid-$usdm_upload_enabled_state-$initial_spot_upload_enabled_state"
  ln -s "$tmp_dir/old-production-binary" "$PRODUCTION_LINK"
  active_count=1
  enabled_count=1
  spot_active_state=inactive
  usdm_active_state=active
  usdm_enabled_state=enabled
  spot_upload_enabled_state=$initial_spot_upload_enabled_state
  MASK_SPOT_UPLOAD_FOR_TRANSITION=0
  HEALTH_TIMEOUT_SECONDS=300
  PRODUCTION_USDM_MEMORY_DROPIN_PRESENT=0
  DRAIN_REQUIRED=0
  OLD_MODE=
  OLD_BINARY=
  OLD_USDM_MINIMUM_SYMBOLS=400
  systemctl_value() {
    case "$1:$2" in
      production-spot:SubState) printf 'dead\n' ;;
      production-spot:MainPID) printf '%s\n' "$spot_main_pid" ;;
      *) return 1 ;;
    esac
  }
  capture_existing_production_identity() {
    OLD_MODE=$1
    OLD_BINARY="$tmp_dir/old-production-binary"
    OLD_USDM_RESTARTS=2
    OLD_USDM_INVOCATION_ID=22222222222222222222222222222222
  }
  unit_matches_release() {
    [[ $1 == production-usdm && $2 == "$OLD_BINARY" && $3 == true \
      && $4 == 2 && $5 == "$OLD_USDM_INVOCATION_ID" ]]
  }
  date() { [[ $1 == +%s%N ]] && printf '%s\n' "$fixed_now_ns"; }
  health_ready_for_release() {
    [[ $1 == usdm && $2 == 400 && -z $3 \
      && $4 == $((fixed_now_ns - HEALTH_TIMEOUT_SECONDS * 1000000000)) ]]
  }
  require_empty_segment_spool() { return 1; }
  fail() { printf '%s\n' "$*" >&2; exit 1; }
  if ! . "$host_state_dispatch_file"; then
    return 1
  fi
  printf '%s %s %s\n' \
    "$OLD_MODE" "$DRAIN_REQUIRED" "$MASK_SPOT_UPLOAD_FOR_TRANSITION"
)
[[ $(run_partial_usdm_host_state_fixture masked-runtime) \
  == 'partial-contained-usdm-live 1 0' ]]
[[ $(run_partial_usdm_host_state_fixture masked-runtime 0 masked-runtime static) \
  == 'partial-contained-usdm-live 1 1' ]]

transition_mask_body="$tmp_dir/transition-mask-usdm-uploader.sh"
sed -n '/^if (( MASK_USDM_UPLOAD_FOR_TRANSITION )); then$/,/^fi$/p' \
  "$CUTOVER" >"$transition_mask_body"
run_transition_mask_fixture() (
  local readback=$1
  MASK_USDM_UPLOAD_FOR_TRANSITION=1
  UPLOAD_UNITS=(upload-spot upload-usdm)
  STEP=
  systemctl() {
    if [[ $1 == mask && $2 == --runtime && $3 == upload-usdm ]]; then
      return 0
    fi
    if [[ $1 == is-enabled && $2 == upload-usdm ]]; then
      printf '%s\n' "$readback"
      return 1
    fi
    return 1
  }
  fail() { exit 1; }
  # shellcheck disable=SC1090
  . "$transition_mask_body"
  [[ $STEP == contain-usdm-uploader ]]
)
run_transition_mask_fixture masked-runtime
if run_transition_mask_fixture static; then
  printf 'transition accepted an unmasked USD-M uploader readback\n' >&2
  exit 1
fi
transition_spot_mask_body="$tmp_dir/transition-mask-spot-uploader.sh"
sed -n '/^if (( MASK_SPOT_UPLOAD_FOR_TRANSITION )); then$/,/^fi$/p' \
  "$CUTOVER" >"$transition_spot_mask_body"
run_transition_spot_mask_fixture() (
  local readback=$1
  MASK_SPOT_UPLOAD_FOR_TRANSITION=1
  UPLOAD_UNITS=(upload-spot upload-usdm)
  STEP=
  systemctl() {
    if [[ $1 == mask && $2 == --runtime && $3 == upload-spot ]]; then
      return 0
    fi
    if [[ $1 == is-enabled && $2 == upload-spot ]]; then
      printf '%s\n' "$readback"
      return 1
    fi
    return 1
  }
  fail() { exit 1; }
  # shellcheck disable=SC1090
  . "$transition_spot_mask_body"
  [[ $STEP == contain-spot-uploader ]]
)
run_transition_spot_mask_fixture masked-runtime
if run_transition_spot_mask_fixture static; then
  printf 'transition accepted an unmasked Spot uploader readback\n' >&2
  exit 1
fi
transition_started_line=$(grep -n '^TRANSITION_STARTED=1$' "$CUTOVER" | cut -d: -f1)
recovery_lock_line=$(grep -n '^acquire_recovery_transition_locks [^()]' \
  "$CUTOVER" | cut -d: -f1)
recovery_quiesce_line=$(grep -n '^quiesce_recovery_scheduler [^()]' \
  "$CUTOVER" | cut -d: -f1)
transition_mask_line=$(grep -n '^if (( MASK_USDM_UPLOAD_FOR_TRANSITION )); then$' \
  "$CUTOVER" | cut -d: -f1)
(( recovery_lock_line < transition_started_line \
  && transition_started_line < recovery_quiesce_line \
  && recovery_quiesce_line < transition_mask_line )) || {
  printf 'USD-M uploader mask moved before the governed transition\n' >&2
  exit 1
}
transition_spot_mask_line=$(grep -n '^if (( MASK_SPOT_UPLOAD_FOR_TRANSITION )); then$' \
  "$CUTOVER" | cut -d: -f1)
(( transition_started_line < transition_spot_mask_line )) || {
  printf 'Spot uploader mask moved before the governed transition\n' >&2
  exit 1
}
recovery_service_mask_line=$(grep -n 'systemctl mask --runtime "${RECOVERY_UNITS\[@\]}"' \
  "$quiesce_recovery_scheduler_body" | cut -d: -f1)
recovery_service_stop_line=$(grep -n 'systemctl stop "${RECOVERY_UNITS\[@\]}"' \
  "$quiesce_recovery_scheduler_body" | cut -d: -f1)
recovery_timer_stop_line=$(grep -n 'systemctl stop "${RECOVERY_TIMERS\[@\]}"' \
  "$quiesce_recovery_scheduler_body" | cut -d: -f1)
recovery_timer_mask_line=$(grep -n 'systemctl mask --runtime "${RECOVERY_TIMERS\[@\]}"' \
  "$quiesce_recovery_scheduler_body" | cut -d: -f1)
(( recovery_service_mask_line < recovery_service_stop_line \
  && recovery_service_stop_line < recovery_timer_stop_line \
  && recovery_timer_stop_line < recovery_timer_mask_line )) || {
  printf 'recovery scheduler quiescence actions are out of order\n' >&2
  exit 1
}

recovery_lock_helpers_body="$tmp_dir/recovery-lock-helpers.sh"
{
  sed -n '/^acquire_recovery_transition_locks()/,/^}/p' "$CUTOVER"
  sed -n '/^release_recovery_queue_locks()/,/^}/p' "$CUTOVER"
  sed -n '/^release_recovery_transition_locks()/,/^}/p' "$CUTOVER"
} >"$recovery_lock_helpers_body"
run_recovery_lock_contention_fixture() (
  local blocked_fd=$1
  LOCK_ROOT="$tmp_dir/recovery-locks-$blocked_fd"
  RECOVERY_DRAIN_LOCK_HELD=0
  RECOVERY_QUEUE_LOCKS_HELD=0
  mkdir -p "$LOCK_ROOT"
  flock() {
    if [[ $1 == -n && $2 == "$blocked_fd" ]]; then return 1; fi
    return 0
  }
  # shellcheck disable=SC1090
  . "$recovery_lock_helpers_body"
  ! acquire_recovery_transition_locks
)
run_recovery_lock_contention_fixture 7
run_recovery_lock_contention_fixture 6
run_recovery_lock_contention_fixture 5
if run_partial_host_state_fixture disabled >"$tmp_dir/partial-disabled.out" 2>&1; then
  printf 'partial classifier accepted disabled instead of masked USD-M\n' >&2
  exit 1
fi
grep -Fq 'ambiguous production state' "$tmp_dir/partial-disabled.out"
if run_partial_host_state_fixture masked-runtime 1 \
  >"$tmp_dir/partial-main-pid.out" 2>&1; then
  printf 'partial classifier accepted a live USD-M MainPID\n' >&2
  exit 1
fi
grep -Fq 'contained USD-M production is not inactive/dead with MainPID=0' \
  "$tmp_dir/partial-main-pid.out"
if run_partial_host_state_fixture masked-runtime 0 masked \
  >"$tmp_dir/partial-persistent-spot-upload-mask.out" 2>&1; then
  printf 'partial classifier accepted a persistently masked Spot uploader\n' >&2
  exit 1
fi
grep -Fq 'ambiguous production state' \
  "$tmp_dir/partial-persistent-spot-upload-mask.out"
if run_partial_usdm_host_state_fixture disabled \
  >"$tmp_dir/partial-usdm-disabled.out" 2>&1; then
  printf 'partial classifier accepted disabled instead of masked Spot\n' >&2
  exit 1
fi
grep -Fq 'ambiguous production state' "$tmp_dir/partial-usdm-disabled.out"
if run_partial_usdm_host_state_fixture masked-runtime 1 \
  >"$tmp_dir/partial-usdm-main-pid.out" 2>&1; then
  printf 'partial classifier accepted a live Spot MainPID\n' >&2
  exit 1
fi
grep -Fq 'contained Spot production is not inactive/dead with MainPID=0' \
  "$tmp_dir/partial-usdm-main-pid.out"
if run_partial_usdm_host_state_fixture masked-runtime 0 masked \
  >"$tmp_dir/partial-persistent-usdm-upload-mask.out" 2>&1; then
  printf 'partial classifier accepted a persistently masked USD-M uploader\n' >&2
  exit 1
fi
grep -Fq 'ambiguous production state' \
  "$tmp_dir/partial-persistent-usdm-upload-mask.out"

new_host_dropin_guard="$tmp_dir/new-host-dropin-guard.sh"
sed -n '/^  (( PRODUCTION_USDM_MEMORY_DROPIN_PRESENT == 0 )) \\/,+1p' "$CUTOVER" \
  | sed 's/^  //' >"$new_host_dropin_guard"
run_new_host_dropin_guard() (
  PRODUCTION_USDM_MEMORY_DROPIN_PRESENT=$1
  fail() { printf '%s\n' "$*" >&2; return 1; }
  # shellcheck disable=SC1090
  . "$new_host_dropin_guard"
)
run_new_host_dropin_guard 0
if run_new_host_dropin_guard 1 >"$tmp_dir/new-host-dropin.out" 2>&1; then
  printf 'new-host classification accepted a production drop-in\n' >&2
  exit 1
fi
grep -Fq 'new host must not retain a production USD-M drop-in' \
  "$tmp_dir/new-host-dropin.out"

drain_dispatch_body="$tmp_dir/drain-dispatch.sh"
sed -n \
  '/^if \[\[ \$OLD_MODE == upgrade || \$OLD_MODE == contained-upgrade/,/^fi$/p' \
  "$CUTOVER" >"$drain_dispatch_body"
run_drain_dispatch_fixture() (
  local calls=$1
  OLD_MODE=$2
  OLD_DEPLOYMENT=old-deployment
  DRAIN_REQUIRED=1
  DRAIN_ATTEMPTED=0
  DRAIN_MAY_HAVE_MUTATED=0
  run_candidate_drain() { printf 'drain %s\n' "$1" >>"$calls"; }
  require_empty_segment_spool() { printf 'require-empty\n' >>"$calls"; }
  fail() { return 1; }
  # shellcheck disable=SC1090
  . "$drain_dispatch_body"
)
contained_drain_calls="$tmp_dir/contained-drain.calls"
partial_drain_calls="$tmp_dir/partial-drain.calls"
new_host_drain_calls="$tmp_dir/new-host-drain.calls"
run_drain_dispatch_fixture "$contained_drain_calls" contained-upgrade
run_drain_dispatch_fixture "$partial_drain_calls" partial-contained-spot-live
run_drain_dispatch_fixture "$partial_drain_calls" partial-contained-usdm-live
run_drain_dispatch_fixture "$new_host_drain_calls" new-host
grep -Fxq 'drain old-deployment' "$contained_drain_calls"
grep -Fxq 'drain old-deployment' "$partial_drain_calls"
if grep -Fq 'require-empty' "$contained_drain_calls"; then
  printf 'contained upgrade used the new-host spool invariant\n' >&2
  exit 1
fi
grep -Fxq 'require-empty' "$new_host_drain_calls"
if grep -Fq 'drain ' "$new_host_drain_calls"; then
  printf 'new-host cutover tried to drain an old deployment\n' >&2
  exit 1
fi

grep -Fq 'release_staging=$(mktemp -d "$release_root/.${artifact_sha256}.new.XXXXXX")' \
  "$INSTALL_RELEASE"
grep -Fq 'COPYFILE_DISABLE=1 tar -C "$SCRIPT_DIR" -cf "$BUNDLE_PATH" "${assets[@]}"' \
  "$INSTALL_RELEASE"
grep -Fq 'host-rust-lob-controller-release.sh' \
  < <(sed -n '/^assets=(/,/^)/p' "$INSTALL_RELEASE")
grep -Fq 'host-rust-lob-controller-apply.sh' \
  < <(sed -n '/^assets=(/,/^)/p' "$INSTALL_RELEASE")
grep -Fq 'BUNDLE_ONLY and CONTROLLER_ONLY are mutually exclusive' "$INSTALL_RELEASE"
grep -Fq 'monday.rust_lob_controller_release.v1' "$INSTALL_RELEASE"
grep -Fq 'production unchanged' "$CONTROLLER_RELEASE"
grep -Fq 'controller release applied without collector restart' "$CONTROLLER_APPLY"
if grep -Eq 'systemctl[[:space:]]+(start|stop|restart|enable|disable|kill)' \
  "$CONTROLLER_RELEASE"; then
  printf 'controller release publisher may not mutate service state\n' >&2
  exit 1
fi
shadow_spool_install=$(sed -n \
  '/^install -d -m 0750 -o hftcollector -g hftcollector \\/,/^  \/data\/monday\/spool\/binance-lob-rust-shadow\/usdm$/p' \
  "$INSTALL_RELEASE")
grep -Fxq "  /data/monday/spool/binance-lob-rust-shadow \\" <<<"$shadow_spool_install"
grep -Fq 'install -d -m 0755 /opt/monday/releases' "$INSTALL_RELEASE"
grep -Fq 'chmod 0755 "$release_staging"' "$INSTALL_RELEASE"
grep -Fq 'release directory must be traversable with mode 0755' "$INSTALL_RELEASE"
grep -Fq 'runuser -u hftcollector -- "$release_binary" --self-test' "$INSTALL_RELEASE"
grep -Fq 'existing release identity does not match requested artifact, bundle, and source' \
  "$INSTALL_RELEASE"
grep -Fq 'existing release deployment differs from the requested bundle' "$INSTALL_RELEASE"
grep -Fq 'runtime_evidence_dir="$binary_evidence_dir/$runtime_contract_sha256"' "$GATE"
grep -Fq 'evidence_dir="$runs_dir/$gate_run_id"' "$GATE"
grep -Fq 'an immutable production-eligible gate already exists' "$GATE"
grep -Fq 'for candidate_unit in "${candidate_units[@]}"; do' "$GATE"
grep -Fq 'systemctl reset-failed "$candidate_unit" >/dev/null 2>&1 || true' "$GATE"
if grep -Fq 'rm -f "$gate_json"' "$GATE"; then
  printf 'shadow gate still deletes immutable gate evidence\n' >&2
  exit 1
fi
grep -Fq 'gate_markers=("$GATE_RUNTIME_DIR"/runs/*/PASSED.sha256)' "$CUTOVER"
grep -Fq 'rollback-deployment.sha256' "$CUTOVER"
grep -Fq 'ROLLBACK_DEPLOYMENT_MANIFEST_SHA256' "$rollback_body"
grep -Fq 'installed production asset drifted from the active immutable release' "$CUTOVER"
grep -Fq 'cmp -s -- "$source" "$snapshot_source"' "$CUTOVER"
grep -Fq 'monday_rust_lob_active_controller_deployment' "$CUTOVER"
grep -Fq 'STEP=clear-previous-controller-override' "$CUTOVER"
grep -Fq 'restore_previous_controller_link' "$CUTOVER"
grep -Fq 'mkdir -m 0750 -- "$EVIDENCE_DIR"' "$CUTOVER"
grep -Fq 'mkdir -m 0750 -- "$evidence_dir"' "$GATE"
grep -Fq '\( -type f -o -type l \)' "$GATE"
grep -Fxq 'TimeoutStartSec=0' "$SCRIPT_DIR/binance-lob-archiver-upload@.service"
grep -Fxq 'TimeoutStartSec=0' "$SCRIPT_DIR/binance-lob-archiver-rust-upload@.service"

candidate_start_body="$tmp_dir/candidate-start.sh"
sed -n '/^STEP=clear-stale-candidate-health/,/^STEP=write-cutover-evidence/p' \
  "$CUTOVER" >"$candidate_start_body"
candidate_clear_line=$(grep -n '^clear_health_before_restart' "$candidate_start_body" | cut -d: -f1)
candidate_timestamp_line=$(grep -n '^CANDIDATE_STARTED_NS=' "$candidate_start_body" | cut -d: -f1)
candidate_start_line=$(grep -n 'systemctl start "${PRODUCTION_UNITS\[@\]}"' \
  "$candidate_start_body" | cut -d: -f1)
candidate_health_line=$(grep -n '^wait_for_release_health' "$candidate_start_body" | cut -d: -f1)
candidate_enable_line=$(grep -n 'systemctl enable "${PRODUCTION_UNITS\[@\]}"' \
  "$candidate_start_body" | cut -d: -f1)
candidate_recovery_enable_line=$(grep -n '^enable_candidate_recovery_scheduler' \
  "$candidate_start_body" | cut -d: -f1)
((candidate_clear_line < candidate_timestamp_line \
  && candidate_timestamp_line < candidate_start_line \
  && candidate_start_line < candidate_health_line \
  && candidate_health_line < candidate_enable_line)) || {
  printf 'candidate no longer follows clear stale health -> timestamp -> start -> verify -> enable\n' >&2
  exit 1
}
((candidate_enable_line < candidate_recovery_enable_line)) || {
  printf 'candidate recovery scheduler starts before production is verified and enabled\n' >&2
  exit 1
}
candidate_recovery_service_unmask_line=$(grep -n 'systemctl unmask --runtime "${RECOVERY_UNITS\[@\]}"' \
  "$enable_recovery_scheduler_body" | cut -d: -f1)
candidate_recovery_service_reset_line=$(grep -n 'reset_failed_recovery_units "${RECOVERY_UNITS\[@\]}"' \
  "$enable_recovery_scheduler_body" | cut -d: -f1)
candidate_recovery_timer_unmask_line=$(grep -n 'systemctl unmask --runtime "${RECOVERY_TIMERS\[@\]}"' \
  "$enable_recovery_scheduler_body" | cut -d: -f1)
candidate_recovery_timer_reset_line=$(grep -n 'reset_failed_recovery_units "${RECOVERY_TIMERS\[@\]}"' \
  "$enable_recovery_scheduler_body" | cut -d: -f1)
candidate_recovery_timer_enable_line=$(grep -n 'systemctl enable --now "${RECOVERY_TIMERS\[@\]}"' \
  "$enable_recovery_scheduler_body" | cut -d: -f1)
((candidate_recovery_service_unmask_line < candidate_recovery_service_reset_line \
  && candidate_recovery_service_reset_line < candidate_recovery_timer_unmask_line \
  && candidate_recovery_timer_unmask_line < candidate_recovery_timer_reset_line \
  && candidate_recovery_timer_reset_line < candidate_recovery_timer_enable_line)) || {
  printf 'candidate recovery services and timers are enabled out of order\n' >&2
  exit 1
}
grep -Fq '"$CANDIDATE_STARTED_NS"' "$candidate_start_body"
grep -Fq 'trap on_exit EXIT' "$CUTOVER"
grep -Fq 'if (( TRANSITION_STARTED )); then' "$CUTOVER"
grep -Fq 'rollback_after_failure' "$CUTOVER"

dropin_body="$tmp_dir/production-dropins.sh"
{
  sed -n '/^systemctl_value()/,/^}/p' "$CUTOVER"
  sed -n '/^validate_memory_only_dropin()/,/^}/p' "$CUTOVER"
  sed -n '/^capture_allowlisted_production_usdm_dropin()/,/^}/p' "$CUTOVER"
  sed -n '/^validate_existing_production_dropins()/,/^}/p' "$CUTOVER"
  sed -n '/^remove_allowlisted_production_dropins_for_candidate()/,/^}/p' "$CUTOVER"
  sed -n '/^restore_allowlisted_production_dropins()/,/^}/p' "$CUTOVER"
} >"$dropin_body"

dropin_root="$tmp_dir/dropin-fixture"
dropin_path="$dropin_root/etc/systemd/system/binance-lob-archiver-production@usdm.service.d/10-memory.conf"
install -d -m 0755 "$dropin_root/etc/systemd/system/binance-lob-archiver-production@usdm.service.d"
cat >"$dropin_path" <<'EOF'
[Service]
MemoryHigh=4096M
MemoryMax=5120M
EOF

run_dropin_roundtrip_fixture() (
  local spot_dropins=${1:-}
  local usdm_dropins=${2:-$dropin_path}
  EVIDENCE_DIR="$dropin_root/evidence"
  PRODUCTION_UNITS=(production-spot production-usdm)
  PRODUCTION_USDM_MEMORY_DROPIN="$dropin_path"
  PRODUCTION_USDM_MEMORY_DROPIN_PRESENT=0
  PRODUCTION_USDM_MEMORY_DROPIN_BACKUP=
  PRODUCTION_USDM_MEMORY_DROPIN_MANIFEST=
  PRODUCTION_USDM_MEMORY_DROPIN_SHA256=
  rm -rf "$EVIDENCE_DIR"
  install -d -m 0755 "$EVIDENCE_DIR"
  fail() { printf '%s\n' "$*" >&2; exit 1; }
  secure_regular_file() { [[ -f $1 && ! -L $1 ]]; }
  secure_directory() { [[ -d $1 && ! -L $1 ]]; }
  atomic_install() { install -m "$1" "$2" "$3"; }
  systemctl() {
    if [[ $1 == show && $3 == --property=DropInPaths && $4 == --value ]]; then
      case "$2" in
        production-spot) printf '%s\n' "$spot_dropins" ;;
        production-usdm) printf '%s\n' "$usdm_dropins" ;;
      esac
      return 0
    fi
    return 1
  }
  # shellcheck disable=SC1090
  . "$dropin_body"
  validate_existing_production_dropins
  remove_allowlisted_production_dropins_for_candidate
  [[ ! -e $PRODUCTION_USDM_MEMORY_DROPIN && ! -L $PRODUCTION_USDM_MEMORY_DROPIN ]]
  restore_allowlisted_production_dropins
  cmp -s "$PRODUCTION_USDM_MEMORY_DROPIN_BACKUP" "$PRODUCTION_USDM_MEMORY_DROPIN"
)

run_dropin_roundtrip_fixture
cat >"$dropin_path" <<'EOF'
[Service]
MemoryHigh=4096M
MemoryMax=5120M
ExecStart=/bin/false
EOF
if run_dropin_roundtrip_fixture >"$tmp_dir/dropin-invalid.out" 2>&1; then
  printf 'production drop-in fixture accepted a non-memory directive\n' >&2
  exit 1
fi
grep -Fq 'must contain only [Service], MemoryHigh, and MemoryMax' \
  "$tmp_dir/dropin-invalid.out"
cat >"$dropin_path" <<'EOF'
[Service]
MemoryHigh=4096M
MemoryMax=5120M
EOF
if run_dropin_roundtrip_fixture /tmp/spot-memory.conf >"$tmp_dir/dropin-spot.out" 2>&1; then
  printf 'production drop-in fixture accepted a spot drop-in\n' >&2
  exit 1
fi
grep -Fq 'spot production service has an unexpected systemd drop-in' \
  "$tmp_dir/dropin-spot.out"

# Execute the rollback snapshot logic against isolated fixture roots. This catches
# content drift and manifest-tamper regressions that static contract greps miss.
installed_root="$tmp_dir/installed"
release_deployment="$tmp_dir/old-release/deployment"
stage_body="$tmp_dir/stage-existing-deployment.sh"
mkdir -p "$installed_root/systemd" "$installed_root/monday" "$installed_root/bin" \
  "$release_deployment"
sed -n '/^stage_existing_deployment_for_rollback()/,/^}/p' "$CUTOVER" \
  | sed \
      -e "s#/etc/systemd/system#$installed_root/systemd#g" \
      -e "s#/etc/monday#$installed_root/monday#g" \
  >"$stage_body"
deployment_assets=(
  binance-lob-archiver-production@.service
  binance-lob-archiver-upload@.service
  binance-lob-archiver-production-spot.env
  binance-lob-archiver-production-usdm.env
)
for asset in "${deployment_assets[@]}"; do
  case "$asset" in
    *.service) installed="$installed_root/systemd/$asset" ;;
    *.env) installed="$installed_root/monday/$asset" ;;
  esac
  if [[ $asset == binance-lob-archiver-production@.service ]]; then
    printf '%s\n' \
      '[Service]' \
      'ReadWritePaths=/data/monday/spool/binance-lob -/data/monday/spool/binance-lob-recovery' \
      >"$release_deployment/$asset"
  else
    printf 'fixture:%s\n' "$asset" >"$release_deployment/$asset"
  fi
  install -m 0644 "$release_deployment/$asset" "$installed"
done
installed_health_script="$installed_root/bin/monday-collector-health.sh"
printf '#!/bin/sh\nprintf "legacy installed health\\n"\n' >"$installed_health_script"
chmod 0755 "$installed_health_script"

run_stage_fixture() (
  BASE_DEPLOYMENT_ASSETS=("${deployment_assets[@]}")
  RECOVERY_DEPLOYMENT_ASSETS=()
  HEALTH_DEPLOYMENT_ASSET=monday-collector-health.sh
  INSTALLED_HEALTH_SCRIPT="$installed_health_script"
  OLD_DEPLOYMENT="$release_deployment"
  EVIDENCE_DIR=$1
  OLD_MODE=${2:-upgrade}
  ROLLBACK_DEPLOYMENT_MANIFEST_SHA256=
  fail() { printf '%s\n' "$*" >&2; exit 1; }
  validate_deployment() { return 0; }
  secure_regular_file() { [[ -f $1 && ! -L $1 ]]; }
  atomic_install() {
    if [[ ${3:-} == */binance-lob-archiver-production@.service \
      && ${MUTATE_INSTALLED_BEFORE_SNAPSHOT:-0} == 1 ]]; then
      printf 'ProtectSystem=false\n' >>"$2"
    fi
    install -m "$1" "$2" "$3"
  }
  # shellcheck disable=SC1090
  . "$stage_body"
  stage_existing_deployment_for_rollback
)

snapshot_evidence="$tmp_dir/snapshot-evidence"
mkdir -p "$snapshot_evidence"
run_stage_fixture "$snapshot_evidence"
cmp -s "$installed_health_script" \
  "$snapshot_evidence/rollback-deployment/monday-collector-health.sh"
(
  cd "$snapshot_evidence/rollback-deployment"
  sha256sum --check --strict "$snapshot_evidence/rollback-deployment.sha256" >/dev/null
)
install -m 0755 "$installed_health_script" \
  "$release_deployment/monday-collector-health.sh"
rm -f "$installed_health_script"
missing_installed_health_evidence="$tmp_dir/missing-installed-health-evidence"
mkdir -p "$missing_installed_health_evidence"
if run_stage_fixture "$missing_installed_health_evidence" \
  >"$tmp_dir/missing-installed-health.out" 2>&1; then
  printf 'rollback snapshot accepted an immutable health script missing from the host\n' >&2
  exit 1
fi
grep -Fq \
  "installed production is missing the health script from the active immutable release: $installed_health_script" \
  "$tmp_dir/missing-installed-health.out"
mv "$release_deployment/monday-collector-health.sh" "$installed_health_script"
printf 'tampered\n' >> \
  "$snapshot_evidence/rollback-deployment/binance-lob-archiver-production@.service"
if (
  cd "$snapshot_evidence/rollback-deployment"
  sha256sum --check --strict "$snapshot_evidence/rollback-deployment.sha256" >/dev/null 2>&1
); then
  printf 'rollback manifest accepted a tampered snapshot\n' >&2
  exit 1
fi

installed_production_unit="$installed_root/systemd/binance-lob-archiver-production@.service"
sed -i.bak \
  's#^ReadWritePaths=/data/monday/spool/binance-lob -/data/monday/spool/binance-lob-recovery$#ReadWritePaths=/data/monday/spool#' \
  "$installed_production_unit"
rm -f "$installed_production_unit.bak"
legacy_scope_evidence="$tmp_dir/legacy-scope-evidence"
mkdir -p "$legacy_scope_evidence"
run_stage_fixture "$legacy_scope_evidence" partial-contained-spot-live
grep -Fxq 'ReadWritePaths=/data/monday/spool' \
  "$legacy_scope_evidence/rollback-deployment/binance-lob-archiver-production@.service"
(
  cd "$legacy_scope_evidence/rollback-deployment"
  sha256sum --check --strict "$legacy_scope_evidence/rollback-deployment.sha256" >/dev/null
)
legacy_scope_race_evidence="$tmp_dir/legacy-scope-race-evidence"
mkdir -p "$legacy_scope_race_evidence"
if MUTATE_INSTALLED_BEFORE_SNAPSHOT=1 \
  run_stage_fixture "$legacy_scope_race_evidence" partial-contained-spot-live \
  >"$tmp_dir/legacy-scope-race.out" 2>&1; then
  printf 'rollback snapshot accepted bytes changed after the drift precheck\n' >&2
  exit 1
fi
grep -Fq 'installed production asset drifted from the active immutable release' \
  "$tmp_dir/legacy-scope-race.out"
install -m 0644 \
  "$release_deployment/binance-lob-archiver-production@.service" \
  "$installed_production_unit"
sed -i.bak \
  's#^ReadWritePaths=/data/monday/spool/binance-lob -/data/monday/spool/binance-lob-recovery$#ReadWritePaths=/data/monday/spool#' \
  "$installed_production_unit"
rm -f "$installed_production_unit.bak"
legacy_scope_upgrade_evidence="$tmp_dir/legacy-scope-upgrade-evidence"
mkdir -p "$legacy_scope_upgrade_evidence"
if run_stage_fixture "$legacy_scope_upgrade_evidence" upgrade \
  >"$tmp_dir/legacy-scope-upgrade.out" 2>&1; then
  printf 'rollback snapshot accepted legacy scope drift outside partial-contained mode\n' >&2
  exit 1
fi
grep -Fq 'installed production asset drifted from the active immutable release' \
  "$tmp_dir/legacy-scope-upgrade.out"
printf 'ProtectSystem=false\n' >>"$installed_production_unit"
legacy_scope_extra_evidence="$tmp_dir/legacy-scope-extra-evidence"
mkdir -p "$legacy_scope_extra_evidence"
if run_stage_fixture "$legacy_scope_extra_evidence" partial-contained-spot-live \
  >"$tmp_dir/legacy-scope-extra.out" 2>&1; then
  printf 'rollback snapshot accepted an extra partial-contained unit drift\n' >&2
  exit 1
fi
grep -Fq 'installed production asset drifted from the active immutable release' \
  "$tmp_dir/legacy-scope-extra.out"
install -m 0644 \
  "$release_deployment/binance-lob-archiver-production@.service" \
  "$installed_production_unit"

printf 'drifted\n' >>"$installed_root/monday/binance-lob-archiver-production-spot.env"
drift_evidence="$tmp_dir/drift-evidence"
mkdir -p "$drift_evidence"
if run_stage_fixture "$drift_evidence" >"$tmp_dir/drift.out" 2>&1; then
  printf 'rollback snapshot accepted installed configuration drift\n' >&2
  exit 1
fi
grep -Fq 'installed production asset drifted from the active immutable release' \
  "$tmp_dir/drift.out"

run_contained_upgrade_rollback_fixture() (
  local expect_contained=${1:-1}
  local pending_drain=${2:-0}
  local mode=${3:-contained-upgrade}
  local old_recovery_timers_enabled=${4:-0}
  local active_recovery_timer=${5:-}
  local enabled_recovery_timer=${6:-}
  local active_recovery_unit=${7:-}
  local old_recovery_timer_spot_enabled=${8:-}
  local old_recovery_timer_usdm_enabled=${9:-}
  local old_recovery_timer_spot_masked_runtime=${10:-}
  local old_recovery_timer_usdm_masked_runtime=${11:-}
  local old_health_asset_present=${12:-1}
  local old_recovery_unit_spot_masked_runtime=${13:-0}
  local old_recovery_unit_usdm_masked_runtime=${14:-0}
  local fail_recovery_restore=${15:-0}
  local spot_upload_unmasked=0
  local usdm_upload_unmasked=0
  local recovery_spot_enabled_now=0
  local recovery_usdm_enabled_now=0
  local recovery_spot_active_now=0
  local recovery_usdm_active_now=0
  local recovery_spot_masked_runtime_now=0
  local recovery_usdm_masked_runtime_now=0
  local recovery_spot_service_masked_runtime_now=1
  local recovery_usdm_service_masked_runtime_now=1
  local calls="$tmp_dir/contained-rollback.calls"
  PRODUCTION_UNITS=(production-spot production-usdm)
  UPLOAD_UNITS=(upload-spot upload-usdm)
  RECOVERY_TIMERS=(recovery-spot recovery-usdm)
  RECOVERY_UNITS=(recovery-spot-service recovery-usdm-service)
  LEGACY_UNITS=(legacy-spot legacy-usdm)
  TRANSITION_MASK_UNITS=("${PRODUCTION_UNITS[@]}" "${UPLOAD_UNITS[@]}" "${LEGACY_UNITS[@]}")
  CANONICAL_SPOOL="$tmp_dir/rollback-spool"
  CANDIDATE_BINARY="$tmp_dir/candidate-binary"
  PRODUCTION_LINK="$tmp_dir/contained-production-link"
  OLD_MODE=$mode
  OLD_DEPLOYMENT="$tmp_dir/contained-old-deployment"
  OLD_BINARY="$tmp_dir/contained-old-binary"
  ROLLBACK_DEPLOYMENT_MANIFEST_SHA256=fixture
  ROLLBACK_RESULT=
  EVIDENCE_DIR="$tmp_dir/contained-evidence"
  DRAIN_REQUIRED=$pending_drain
  DRAIN_ATTEMPTED=0
  DRAIN_MAY_HAVE_MUTATED=0
  OLD_RECOVERY_TIMERS_ENABLED=$old_recovery_timers_enabled
  if [[ -n $old_recovery_timer_spot_enabled ]]; then
    OLD_RECOVERY_TIMER_SPOT_ENABLED=$old_recovery_timer_spot_enabled
  elif (( old_recovery_timers_enabled == 2 || ( old_recovery_timers_enabled == 1 && mode == partial-contained-spot-live ) )); then
    OLD_RECOVERY_TIMER_SPOT_ENABLED=1
  else
    OLD_RECOVERY_TIMER_SPOT_ENABLED=0
  fi
  if [[ -n $old_recovery_timer_usdm_enabled ]]; then
    OLD_RECOVERY_TIMER_USDM_ENABLED=$old_recovery_timer_usdm_enabled
  elif (( old_recovery_timers_enabled == 2 || ( old_recovery_timers_enabled == 1 && mode == partial-contained-usdm-live ) )); then
    OLD_RECOVERY_TIMER_USDM_ENABLED=1
  else
    OLD_RECOVERY_TIMER_USDM_ENABLED=0
  fi
  OLD_RECOVERY_TIMER_SPOT_MASKED_RUNTIME=${old_recovery_timer_spot_masked_runtime:-0}
  OLD_RECOVERY_TIMER_USDM_MASKED_RUNTIME=${old_recovery_timer_usdm_masked_runtime:-0}
  OLD_RECOVERY_UNIT_SPOT_MASKED_RUNTIME=$old_recovery_unit_spot_masked_runtime
  OLD_RECOVERY_UNIT_USDM_MASKED_RUNTIME=$old_recovery_unit_usdm_masked_runtime
  OLD_SESSION_SPOT=old-spot-session
  OLD_SESSION_USDM=old-usdm-session
  OLD_USDM_MINIMUM_SYMBOLS=400
  OLD_USDM_RESTARTS=2
  OLD_USDM_INVOCATION_ID=22222222222222222222222222222222
  OLD_HEALTH_ASSET_PRESENT=$old_health_asset_present
  INSTALLED_HEALTH_SCRIPT="$tmp_dir/contained-installed-health"
  SPOOL_ENV_DEPLOYMENT=$OLD_DEPLOYMENT
  mkdir -p "$CANONICAL_SPOOL" "$OLD_DEPLOYMENT" "$EVIDENCE_DIR" \
    "$(dirname "$PRODUCTION_LINK")"
  : >"$calls"
  rm -f "$INSTALLED_HEALTH_SCRIPT"
  if (( OLD_HEALTH_ASSET_PRESENT == 0 )); then
    printf 'candidate installed health\n' >"$INSTALLED_HEALTH_SCRIPT"
  fi
  systemctl() {
    printf '%s %s\n' "$1" "${*:2}" >>"$calls"
    case "$1" in
      is-active)
        case "${!#}" in
          recovery-spot)
            [[ ${!#} == "$active_recovery_timer" || $recovery_spot_active_now -eq 1 ]]
            ;;
          recovery-usdm)
            [[ ${!#} == "$active_recovery_timer" || $recovery_usdm_active_now -eq 1 ]]
            ;;
          *)
            [[ ${!#} == "$active_recovery_unit" ]]
            ;;
        esac
        ;;
      is-enabled)
        if [[ ${!#} == upload-spot && $spot_upload_unmasked -eq 1 ]]; then
          printf 'static\n'
          return 1
        fi
        if [[ ${!#} == upload-usdm && $usdm_upload_unmasked -eq 1 ]]; then
          printf 'static\n'
          return 1
        fi
        if [[ ${2:-} == --quiet ]]; then
          case "${!#}" in
            recovery-spot)
              [[ ${!#} == "$enabled_recovery_timer" || $recovery_spot_enabled_now -eq 1 ]]
              ;;
            recovery-usdm)
              [[ ${!#} == "$enabled_recovery_timer" || $recovery_usdm_enabled_now -eq 1 ]]
              ;;
            *)
              [[ ${!#} == "$enabled_recovery_timer" ]]
              ;;
          esac
          return
        fi
        if [[ ${!#} == "$enabled_recovery_timer" \
          || ( ${!#} == recovery-spot && $recovery_spot_enabled_now -eq 1 ) \
          || ( ${!#} == recovery-usdm && $recovery_usdm_enabled_now -eq 1 ) ]]; then
          printf 'enabled\n'
          return 0
        fi
        if [[ ${!#} == recovery-spot && $recovery_spot_masked_runtime_now -eq 1 ]]; then
          printf 'masked-runtime\n'
          return 1
        fi
        if [[ ${!#} == recovery-usdm && $recovery_usdm_masked_runtime_now -eq 1 ]]; then
          printf 'masked-runtime\n'
          return 1
        fi
        if [[ ${!#} == recovery-spot-service ]]; then
          if (( recovery_spot_service_masked_runtime_now )); then
            printf 'masked-runtime\n'
          else
            printf 'static\n'
          fi
          return 1
        fi
        if [[ ${!#} == recovery-usdm-service ]]; then
          if (( recovery_usdm_service_masked_runtime_now )); then
            printf 'masked-runtime\n'
          else
            printf 'static\n'
          fi
          return 1
        fi
        if (( expect_contained )); then
          printf 'masked-runtime\n'
        else
          printf 'disabled\n'
        fi
        return 1
        ;;
      show)
        if [[ $* == *'--property=MainPID'* ]]; then
          printf '0\n'
        fi
        ;;
      unmask)
        shift
        [[ ${1:-} == --runtime ]] && shift
        while (($#)); do
          case "$1" in
            upload-spot) spot_upload_unmasked=1 ;;
            upload-usdm) usdm_upload_unmasked=1 ;;
            recovery-spot)
              (( fail_recovery_restore )) && return 1
              recovery_spot_masked_runtime_now=0
              ;;
            recovery-usdm)
              (( fail_recovery_restore )) && return 1
              recovery_usdm_masked_runtime_now=0
              ;;
            recovery-spot-service)
              (( fail_recovery_restore )) && return 1
              recovery_spot_service_masked_runtime_now=0
              ;;
            recovery-usdm-service)
              (( fail_recovery_restore )) && return 1
              recovery_usdm_service_masked_runtime_now=0
              ;;
          esac
          shift
        done
        ;;
      enable)
        shift
        if [[ ${1:-} == --now ]]; then
          shift
        fi
        while (($#)); do
          case "$1" in
            recovery-spot)
              recovery_spot_enabled_now=1
              recovery_spot_active_now=1
              ;;
            recovery-usdm)
              recovery_usdm_enabled_now=1
              recovery_usdm_active_now=1
              ;;
          esac
          shift
        done
        ;;
      disable)
        shift
        if [[ ${1:-} == --now ]]; then
          shift
        fi
        while (($#)); do
          case "$1" in
            recovery-spot)
              recovery_spot_enabled_now=0
              recovery_spot_active_now=0
              ;;
            recovery-usdm)
              recovery_usdm_enabled_now=0
              recovery_usdm_active_now=0
              ;;
          esac
          shift
        done
        ;;
      mask)
        if [[ ${2:-} == --runtime ]]; then
          shift 2
          while (($#)); do
            case "$1" in
              recovery-spot)
              recovery_spot_enabled_now=0
              recovery_spot_active_now=0
              recovery_spot_masked_runtime_now=1
                ;;
              recovery-usdm)
              recovery_usdm_enabled_now=0
              recovery_usdm_active_now=0
              recovery_usdm_masked_runtime_now=1
                ;;
              recovery-spot-service) recovery_spot_service_masked_runtime_now=1 ;;
              recovery-usdm-service) recovery_usdm_service_masked_runtime_now=1 ;;
            esac
            shift
          done
          (( expect_contained ))
          return $?
        fi
        (( expect_contained ))
        ;;
      *) return 0 ;;
    esac
  }
  sha256sum() { return 0; }
  copy_health_evidence() { return 0; }
  clear_health_before_restart() { return 0; }
  health_ready_for_release() { return 0; }
  wait_for_spot_release_health() { return 0; }
  wait_for_usdm_release_health() { return 0; }
  wait_for_release_health() { printf 'wait-old-health\n' >>"$calls"; return 0; }
  runtime_matches_release() { printf 'verify-old-runtime-health\n' >>"$calls"; return 0; }
  unit_matches_release() {
    if [[ $mode == partial-contained-usdm-live && $1 == production-usdm && $# -ne 3 ]]; then
      printf 'partial USD-M rollback compared stale activation identity\n' >&2
      return 1
    fi
    return 0
  }
  run_candidate_drain() { printf 'drain %s\n' "$1" >>"$calls"; return 1; }
  install_deployment() { printf 'install %s\n' "$1" >>"$calls"; return 0; }
  atomic_symlink() { printf 'symlink %s %s\n' "$1" "$2" >>"$calls"; return 0; }
  restore_previous_controller_link() { return 0; }
  restore_allowlisted_production_dropins() { printf 'restore-dropin\n' >>"$calls"; return 0; }
  # shellcheck disable=SC1090
  . "$remove_health_body"
  # shellcheck disable=SC1090
  . "$restore_recovery_scheduler_body"
  # shellcheck disable=SC1090
  . "$recovery_predicate_body"
  # shellcheck disable=SC1090
  . "$production_predicate_body"
  # shellcheck disable=SC1090
  . "$partial_predicate_body"
  # shellcheck disable=SC1090
  . "$partial_usdm_predicate_body"
  # shellcheck disable=SC1090
  . "$rollback_body"
  rollback_after_failure
  if (( expect_contained )) \
    && [[ -z $active_recovery_timer && -z $enabled_recovery_timer \
      && -z $active_recovery_unit ]]; then
    grep -Fq "install $OLD_DEPLOYMENT" "$calls"
    grep -Fq "symlink $OLD_BINARY $PRODUCTION_LINK" "$calls"
    grep -Fq 'restore-dropin' "$calls"
    grep -Eq '^daemon-reload( |$)' "$calls"
  elif grep -Eq '^(install|symlink|restore-dropin|daemon-reload)( |$)' "$calls"; then
    printf 'uncontained rollback tried to restore the previous release\n' >&2
    exit 1
  fi
  if [[ $mode == partial-contained-spot-live ]]; then
    grep -Fxq 'start production-spot' "$calls"
    grep -Fxq 'enable production-spot' "$calls"
    grep -Fxq 'unmask --runtime production-spot' "$calls"
    grep -Fxq 'unmask --runtime upload-spot' "$calls"
    if grep -Eq '^enable --now .*recovery-' "$calls" \
      || grep -Eq '^unmask --runtime .*recovery-' "$calls"; then
      printf 'partial Spot rollback restored recovery scheduling\n' >&2
      exit 1
    fi
    if grep -Eq '^(start|enable|unmask) .*(production-usdm|upload-usdm)' "$calls"; then
      printf 'partial rollback tried to start, enable, or unmask old USD-M\n' >&2
      exit 1
    fi
  elif [[ $mode == partial-contained-usdm-live ]]; then
    grep -Fxq 'start production-usdm' "$calls"
    grep -Fxq 'enable production-usdm' "$calls"
    grep -Fxq 'unmask --runtime production-usdm' "$calls"
    grep -Fxq 'unmask --runtime upload-usdm' "$calls"
    if grep -Eq '^enable --now .*recovery-' "$calls" \
      || grep -Eq '^unmask --runtime .*recovery-' "$calls"; then
      printf 'partial USD-M rollback restored recovery scheduling\n' >&2
      exit 1
    fi
    if grep -Eq '^(start|enable|unmask) .*(production-spot|upload-spot)' "$calls"; then
      printf 'partial rollback tried to start, enable, or unmask old Spot\n' >&2
      exit 1
    fi
  elif [[ $mode == upgrade ]]; then
    if [[ $ROLLBACK_RESULT == previous-release-health-verified ]]; then
      local old_health_line recovery_restore_line
      old_health_line=$(grep -n '^wait-old-health$' "$calls" | cut -d: -f1)
      recovery_restore_line=$(grep -n \
        '^unmask --runtime recovery-spot-service recovery-usdm-service recovery-spot recovery-usdm$' \
        "$calls" | cut -d: -f1)
      (( old_health_line < recovery_restore_line ))
      (( recovery_spot_enabled_now == OLD_RECOVERY_TIMER_SPOT_ENABLED ))
      (( recovery_usdm_enabled_now == OLD_RECOVERY_TIMER_USDM_ENABLED ))
      (( recovery_spot_masked_runtime_now == OLD_RECOVERY_TIMER_SPOT_MASKED_RUNTIME ))
      (( recovery_usdm_masked_runtime_now == OLD_RECOVERY_TIMER_USDM_MASKED_RUNTIME ))
      (( recovery_spot_service_masked_runtime_now \
        == OLD_RECOVERY_UNIT_SPOT_MASKED_RUNTIME ))
      (( recovery_usdm_service_masked_runtime_now \
        == OLD_RECOVERY_UNIT_USDM_MASKED_RUNTIME ))
    elif [[ $ROLLBACK_RESULT == recovery-scheduler-restore-failed-disabled ]]; then
      (( recovery_spot_enabled_now == 0 && recovery_usdm_enabled_now == 0 ))
      (( recovery_spot_masked_runtime_now == 1 \
        && recovery_usdm_masked_runtime_now == 1 ))
      (( recovery_spot_service_masked_runtime_now == 1 \
        && recovery_usdm_service_masked_runtime_now == 1 ))
    fi
  else
    if (( OLD_RECOVERY_TIMER_SPOT_MASKED_RUNTIME )); then
      grep -Fxq 'mask --runtime recovery-spot' "$calls"
    fi
    if (( OLD_RECOVERY_TIMER_USDM_MASKED_RUNTIME )); then
      grep -Fxq 'mask --runtime recovery-usdm' "$calls"
    fi
    if grep -Eq '^(start|enable|unmask) ' "$calls"; then
      printf 'contained rollback tried to restart or unmask the previous release\n' >&2
      exit 1
    fi
  fi
  if [[ $mode == contained-upgrade ]] && grep -Fq 'drain ' "$calls"; then
    printf 'contained rollback retried a failed canonical spool drain\n' >&2
    exit 1
  fi
  if [[ $mode == upgrade && $pending_drain == 1 ]] \
    && ! grep -Fq "drain $OLD_DEPLOYMENT" "$calls"; then
    printf 'upgrade rollback did not attempt its required canonical spool drain\n' >&2
    exit 1
  fi
  if (( OLD_HEALTH_ASSET_PRESENT == 0 )) \
    && [[ -e $INSTALLED_HEALTH_SCRIPT || -L $INSTALLED_HEALTH_SCRIPT ]]; then
    printf 'rollback retained a candidate health script absent from the previous deployment\n' >&2
    exit 1
  fi
  printf '%s\n' "$ROLLBACK_RESULT"
)

[[ $(run_contained_upgrade_rollback_fixture) == previous-release-restored-contained ]]
[[ $(run_contained_upgrade_rollback_fixture 0) \
  == production-stop-or-disable-containment-failed ]]
[[ $(run_contained_upgrade_rollback_fixture 1 1) == previous-release-restored-contained ]]
[[ $(run_contained_upgrade_rollback_fixture 1 0 contained-upgrade 2) \
  == previous-release-restored-contained ]]
[[ $(run_contained_upgrade_rollback_fixture \
  1 0 contained-upgrade 0 '' '' '' '' '' '' '' 0) \
  == previous-release-restored-contained ]]
[[ $(run_contained_upgrade_rollback_fixture \
  1 0 contained-upgrade 0 '' '' '' 0 0 1 1) \
  == previous-release-restored-contained ]]
[[ $(run_contained_upgrade_rollback_fixture 1 0 contained-upgrade 0 recovery-spot) \
  == production-stop-or-disable-containment-failed ]]
[[ $(run_contained_upgrade_rollback_fixture 1 0 contained-upgrade 0 '' recovery-spot) \
  == production-stop-or-disable-containment-failed ]]
[[ $(run_contained_upgrade_rollback_fixture \
  1 0 contained-upgrade 0 '' '' recovery-spot-service) \
  == production-stop-or-disable-containment-failed ]]
[[ $(run_contained_upgrade_rollback_fixture 1 1 upgrade) \
  == previous-release-restored-disabled ]]
[[ $(run_contained_upgrade_rollback_fixture \
  1 0 upgrade 1 '' '' '' 0 1 1 0 1 1 0 0) \
  == previous-release-health-verified ]]
[[ $(run_contained_upgrade_rollback_fixture \
  1 0 upgrade 1 '' '' '' 0 1 1 0 1 1 0 1) \
  == recovery-scheduler-restore-failed-disabled ]]
[[ $(run_contained_upgrade_rollback_fixture 1 0 partial-contained-spot-live) \
  == previous-spot-restored-usdm-contained ]]
[[ $(run_contained_upgrade_rollback_fixture \
  1 0 partial-contained-spot-live 1 '' '' '' 1 0) \
  == previous-spot-restored-usdm-contained ]]
[[ $(run_contained_upgrade_rollback_fixture 1 0 partial-contained-spot-live 2) \
  == previous-spot-restored-usdm-contained ]]
[[ $(run_contained_upgrade_rollback_fixture 1 0 partial-contained-usdm-live) \
  == previous-usdm-restored-spot-contained ]]
[[ $(run_contained_upgrade_rollback_fixture \
  1 0 partial-contained-usdm-live 1 '' '' '' 0 1) \
  == previous-usdm-restored-spot-contained ]]
[[ $(run_contained_upgrade_rollback_fixture \
  1 0 partial-contained-usdm-live 1 '' '' '' 0 1 1 0) \
  == previous-usdm-restored-spot-contained ]]
[[ $(run_contained_upgrade_rollback_fixture 1 0 partial-contained-usdm-live 2) \
  == previous-usdm-restored-spot-contained ]]

run_new_host_rollback_fixture() (
  local active_unit=${1:-} prior_health=${2:-absent} unit previous_health_sha=
  PRODUCTION_UNITS=(production-spot production-usdm)
  UPLOAD_UNITS=(upload-spot upload-usdm)
  RECOVERY_TIMERS=(recovery-spot recovery-usdm)
  RECOVERY_UNITS=(recovery-spot-service recovery-usdm-service)
  LEGACY_UNITS=(legacy-spot legacy-usdm)
  TRANSITION_MASK_UNITS=("${PRODUCTION_UNITS[@]}" "${UPLOAD_UNITS[@]}" "${LEGACY_UNITS[@]}")
  CANONICAL_SPOOL="$tmp_dir/nonexistent-spool"
  CANDIDATE_DEPLOYMENT="$tmp_dir/candidate-deployment"
  CANDIDATE_BINARY="$tmp_dir/candidate-binary"
  PRODUCTION_LINK="$tmp_dir/nonexistent-production-link"
  INSTALLED_HEALTH_SCRIPT="$tmp_dir/new-host-installed-health.sh"
  EVIDENCE_DIR="$tmp_dir/new-host-health-evidence"
  OLD_MODE=new-host
  ROLLBACK_RESULT=
  HEALTH_SCRIPT_ROLLBACK_RESULT=not-needed
  OLD_HEALTH_ASSET_PRESENT=0
  OLD_HEALTH_SCRIPT_SHA256=
  OLD_HEALTH_SCRIPT_SNAPSHOT=
  OLD_HEALTH_ROLLBACK_SOURCE=absent
  DRAIN_REQUIRED=0
  DRAIN_ATTEMPTED=0
  DRAIN_MAY_HAVE_MUTATED=0
  OLD_RECOVERY_TIMERS_ENABLED=0
  SPOOL_ENV_DEPLOYMENT=
  rm -rf "$EVIDENCE_DIR"
  rm -f "$INSTALLED_HEALTH_SCRIPT"
  mkdir -p "$EVIDENCE_DIR"
  sha256sum() {
    local -a args=()
    local arg check=0 checksum_file
    for arg in "$@"; do
      case "$arg" in
        --strict) ;;
        --check) check=1 ;;
        *) args+=("$arg") ;;
      esac
    done
    if (( check )); then
      if (( ${#args[@]} )); then
        command sha256sum -c "${args[@]}"
      else
        checksum_file=$(mktemp)
        command cat >"$checksum_file"
        command sha256sum -c "$checksum_file"
        command rm -f "$checksum_file"
      fi
    else
      command sha256sum "${args[@]}"
    fi
  }
  if [[ $prior_health == present ]]; then
    printf 'preexisting new-host health\n' >"$INSTALLED_HEALTH_SCRIPT"
    previous_health_sha=$(sha256sum "$INSTALLED_HEALTH_SCRIPT" | awk '{print $1}')
  fi
  fail() { printf '%s\n' "$*" >&2; exit 1; }
  secure_regular_file() { [[ -f $1 && ! -L $1 ]]; }
  atomic_install() {
    install -m "$1" "$2" "$3"
    cmp -s -- "$2" "$3"
  }
  systemctl() {
    case "$1" in
      is-active)
        unit=${!#}
        [[ -n $active_unit && $unit == "$active_unit" ]]
        ;;
      is-enabled)
        unit=${!#}
        if [[ ${2:-} == --quiet ]]; then
          return 1
        fi
        printf 'masked-runtime\n'
        return 1
        ;;
      *) return 0 ;;
    esac
  }
  copy_health_evidence() { return 0; }
  run_candidate_drain() { return 0; }
  restore_previous_controller_link() { return 0; }
  # shellcheck disable=SC1090
  . "$remove_health_body"
  # shellcheck disable=SC1090
  . "$stage_new_host_health_body"
  # shellcheck disable=SC1090
  . "$restore_new_host_health_body"
  stage_new_host_health_for_rollback
  printf 'candidate installed health\n' >"$INSTALLED_HEALTH_SCRIPT"
  # shellcheck disable=SC1090
  . "$recovery_predicate_body"
  # shellcheck disable=SC1090
  . "$production_predicate_body"
  # shellcheck disable=SC1090
  . "$rollback_body"
  rollback_after_failure
  if [[ $prior_health == present ]]; then
    [[ $HEALTH_SCRIPT_ROLLBACK_RESULT == previous-bytes-restored ]]
    [[ $(sha256sum "$INSTALLED_HEALTH_SCRIPT" | awk '{print $1}') \
      == "$previous_health_sha" ]]
  else
    [[ $HEALTH_SCRIPT_ROLLBACK_RESULT == candidate-removed ]]
    [[ ! -e $INSTALLED_HEALTH_SCRIPT && ! -L $INSTALLED_HEALTH_SCRIPT ]]
  fi
  printf '%s\n' "$ROLLBACK_RESULT"
)

[[ $(run_new_host_rollback_fixture) == new-host-disabled ]]
[[ $(run_new_host_rollback_fixture '' present) == new-host-disabled ]]
[[ $(run_new_host_rollback_fixture legacy-spot) \
  == production-stop-or-disable-containment-failed ]]
[[ $(run_new_host_rollback_fixture upload-usdm) \
  == production-stop-or-disable-containment-failed ]]
[[ $(run_new_host_rollback_fixture recovery-spot-service) \
  == production-stop-or-disable-containment-failed ]]

mock_bin="$tmp_dir/bin"
mock_state="$tmp_dir/mock-state"
mkdir -p "$mock_bin" "$mock_state"
cat >"$mock_bin/aliyun" <<'MOCK_ALIYUN'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$MOCK_STATE_DIR/calls.log"
case "${1:-} ${2:-}" in
  'ecs RunCommand')
    capture_next=0
    for argument in "$@"; do
      if ((capture_next)); then
        printf '%s\n' "$argument" >"$MOCK_STATE_DIR/last-command-content"
        break
      fi
      [[ $argument == --CommandContent ]] && capture_next=1
    done
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
    printf '{"Invocation":{"InvocationStatus":"%s","ExitCode":"%s","Output":"%s"}}\n' \
      "$status" "$exit_code" "${MOCK_OUTPUT_B64:-}"
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

preflight_bundle=$(printf 'b%.0s' {1..64})
preflight_source=$(printf 'c%.0s' {1..40})
preflight_runtime_contract=$(printf 'd%.0s' {1..64})
preflight_payload=$(jq -cn \
  --arg artifact "$artifact" \
  --arg bundle "$preflight_bundle" \
  --arg source "$preflight_source" \
  --arg runtime_contract "$preflight_runtime_contract" \
  '{schema:"monday.rust_lob_gate_resource_preflight.v1",
    candidate_sha256:$artifact,
    runtime_contract_sha256:$runtime_contract,
    deployment_bundle_sha256:$bundle,
    deployment_source_revision:$source,
    host_memory_total_bytes:8589934592,
    host_swap_total_bytes:0,
    production_memory_current_bytes:{
      spot:{active_state:"active",current_bytes:2147483648,
        peak_bytes:2147483648,memory_max_bytes:2684354560,
        growth_target_bytes:2415919104},
      usdm:{active_state:"active",current_bytes:536870912,
        peak_bytes:805306368,memory_max_bytes:2684354560,
        growth_target_bytes:1073741824}},
    maximum_sequential_phase_memory_bytes:2147483648,
    resource_preflight:{
      phase:"resource-preflight",
      sampled_at:"2026-08-26T00:00:00Z",
      host_memory_available_bytes:4294967296,
      host_memory_reserve_bytes:1073741824,
      phase_memory_max_bytes:2147483648,
      production_memory_growth_margin_bytes:268435456,
      production_memory_growth_headroom_bytes:805306368,
      required_bytes:4026531840},
    memory_full_psi_windows:[
      {phase:"resource-preflight",started_at:"2026-08-28T00:00:00Z",
       finished_at:"2026-08-28T00:00:15Z",previous_total_us:0,current_total_us:0,
       delta_us:0,window_us:15000000,ratio:0,hit:false,consecutive_hits:0},
      {phase:"resource-preflight",started_at:"2026-08-28T00:00:15Z",
       finished_at:"2026-08-28T00:00:30Z",previous_total_us:0,current_total_us:0,
       delta_us:0,window_us:15000000,ratio:0,hit:false,consecutive_hits:0},
      {phase:"resource-preflight",started_at:"2026-08-28T00:00:30Z",
       finished_at:"2026-08-28T00:00:45Z",previous_total_us:0,current_total_us:0,
       delta_us:0,window_us:15000000,ratio:0,hit:false,consecutive_hits:0}],
    passed:true}')
preflight_output_b64=$(printf '%s\n' "$preflight_payload" | base64 | tr -d '\n')
env "${common_env[@]}" \
  ACTION=gate-preflight \
  MOCK_STATUS=Success \
  MOCK_EXIT_CODE=0 \
  MOCK_OUTPUT_B64="$preflight_output_b64" \
  "$INVOKE" >"$tmp_dir/gate-preflight.json" 2>"$tmp_dir/gate-preflight.err"
jq -e '.schema == "monday.rust_lob_gate_resource_preflight.v1" and .passed == true' \
  "$tmp_dir/gate-preflight.json" >/dev/null
base64 --decode <"$mock_state/last-command-content" >"$tmp_dir/gate-preflight-command.sh"
grep -Fq -- '--resource-preflight' "$tmp_dir/gate-preflight-command.sh"
grep -Fq 'gate-preflight completed successfully: mock-invoke' \
  "$tmp_dir/gate-preflight.err"
invalid_preflight_output_b64=$(printf '{}\n' | base64 | tr -d '\n')
if env "${common_env[@]}" \
  ACTION=gate-preflight \
  MOCK_STATUS=Success \
  MOCK_EXIT_CODE=0 \
  MOCK_OUTPUT_B64="$invalid_preflight_output_b64" \
  "$INVOKE" >"$tmp_dir/invalid-gate-preflight.out" 2>&1; then
  printf 'operation wrapper accepted invalid gate-preflight JSON\n' >&2
  exit 1
fi
grep -Fq 'gate-preflight returned invalid JSON evidence' \
  "$tmp_dir/invalid-gate-preflight.out"

controller_release_sha=$(printf 'e%.0s' {1..64})
controller_output_b64=$(printf 'controller applied\n' | base64 | tr -d '\n')
env "${common_env[@]}" \
  ACTION=controller-apply \
  CONTROLLER_RELEASE_SHA256="$controller_release_sha" \
  MOCK_STATUS=Success \
  MOCK_EXIT_CODE=0 \
  MOCK_OUTPUT_B64="$controller_output_b64" \
  "$INVOKE" >"$tmp_dir/controller-apply.out"
base64 --decode <"$mock_state/last-command-content" \
  >"$tmp_dir/controller-apply-command.sh"
grep -Fq "/opt/monday/releases/binance-lob-controller/$controller_release_sha/deployment/host-rust-lob-controller-apply.sh" \
  "$tmp_dir/controller-apply-command.sh"
grep -Fq "$controller_release_sha $artifact" \
  "$tmp_dir/controller-apply-command.sh"

run_commands_before=$(grep -c 'ecs RunCommand' "$mock_state/calls.log")
if env "${common_env[@]}" ACTION=controller-apply "$INVOKE" \
  >"$tmp_dir/controller-missing.out" 2>&1; then
  printf 'controller apply accepted a missing controller release identity\n' >&2
  exit 1
fi
run_commands_after=$(grep -c 'ecs RunCommand' "$mock_state/calls.log")
[[ $run_commands_after == "$run_commands_before" ]]

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
