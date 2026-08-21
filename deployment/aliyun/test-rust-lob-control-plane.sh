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
PRODUCTION_USDM_ENV="$SCRIPT_DIR/binance-lob-archiver-production-usdm.env"
LIB="$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
# shellcheck disable=SC1090,SC1091
. "$LIB"
"$SCRIPT_DIR/test-rust-lob-shadow-soak.sh"

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
grep -Fq -- '--property=KillMode=control-group' "$GATE"
grep -Fq 'MemoryHigh=5000M' "$GATE"
grep -Fq 'MemoryMax=6400M' "$GATE"
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
drain_body=$(sed -n '/^run_candidate_drain()/,/^}/p' "$CUTOVER")
backup_line=$(grep -n -- 'RECOVERY_BACKUP_DIR=' <<<"$drain_body" | cut -d: -f1)
recover_line=$(grep -n -- '--recover-parts-only' <<<"$drain_body" | cut -d: -f1)
upload_line=$(grep -n -- '--upload-only' <<<"$drain_body" | cut -d: -f1)
[[ -n $backup_line && -n $recover_line && -n $upload_line \
  && $backup_line -lt $recover_line && $recover_line -lt $upload_line ]] || {
  printf 'cutover does not recover interrupted parts before upload-only drain\n' >&2
  exit 1
}
recover_body=$(sed -n '/^fn recover_parts_only()/,/^fn stream_types_for_market/p' "$COLLECTOR")
grep -Fq 'RECOVERY_UID="$(id -u hftcollector)"' "$CUTOVER"
grep -Fq 'RECOVERY_GID="$(id -g hftcollector)"' "$CUTOVER"
grep -Fq 'RECOVERY_ARTIFACT_SHA256="$CANDIDATE_SHA256"' "$CUTOVER"
grep -Fq 'RECOVERY_DEPLOYMENT_SOURCE_REVISION="$DEPLOYMENT_SOURCE_REVISION"' "$CUTOVER"
grep -Fq 'RECOVERY_DEPLOYMENT_BUNDLE_SHA256="$DEPLOYMENT_BUNDLE_SHA256"' "$CUTOVER"
grep -Fq -- '--arg deployment_source_revision "$DEPLOYMENT_SOURCE_REVISION"' "$CUTOVER"
grep -Fq 'deployment_source_revision:' "$CUTOVER"
grep -Fq 'spool_lock.owner()' <<<"$recover_body"
grep -Fq 'validated_nonempty_recovery_parts' <<<"$recover_body"
grep -Fq 'validated_recovery_temporaries' <<<"$recover_body"
backup_line=$(grep -n 'backup_recovery_inputs' <<<"$recover_body" | head -1 | cut -d: -f1)
drop_line=$(grep -n 'drop_recovery_privileges' <<<"$recover_body" | head -1 | cut -d: -f1)
catalog_line=$(grep -n 'discover_recovery_catalog' <<<"$recover_body" | head -1 | cut -d: -f1)
remove_temporary_line=$(grep -n 'remove_recovery_temporaries' <<<"$recover_body" | head -1 | cut -d: -f1)
recover_line=$(grep -n 'recover_parts(&config)' <<<"$recover_body" | head -1 | cut -d: -f1)
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
grep -Fq 'observation_started_ns=$(date +%s%N)' "$GATE"
grep -Fq '((end_ns <= observation_started_ns)) && continue' "$GATE"
grep -Fq 'shadow segments did not rotate after health settled' "$GATE"
[[ $(grep -Fc 'end_received_at_ns > $gate.observation_started_ns' "$POLICY") -eq 2 ]] || {
  printf 'gate policy does not bind both market tapes across observation start\n' >&2
  exit 1
}
if grep -Fq '((start_ns < gate_started_ns)) && continue' "$GATE"; then
  printf 'manifest discovery still admits health-settle warmup segments\n' >&2
  exit 1
fi
if grep -Fq '((start_ns < observation_started_ns)) && continue' "$GATE"; then
  printf 'manifest discovery still drops the segment overlapping observation start\n' >&2
  exit 1
fi
grep -Fq 'gate_started_ns=$(date +%s%N)' "$GATE"
grep -Fq 'all(.[].lob_reconnect_boundary; . == false)' "$GATE"
grep -Fq 'ARG SOURCE_REVISION' "$COLLECTOR_DOCKERFILE"
grep -Fq 'MONDAY_SOURCE_REVISION="$SOURCE_REVISION" cargo' "$COLLECTOR_DOCKERFILE"
grep -Fq 'SOURCE_REVISION=${{ needs.selector.outputs.source_sha }}' "$ACR_WORKFLOW"
grep -Fq "grep -Fqx 'binance-lob-archiver \${{ needs.selector.outputs.source_sha }}'" "$ACR_WORKFLOW"
grep -Fxq 'MemoryHigh=4400M' "$SHADOW_UNIT"
grep -Fxq 'MemoryMax=5000M' "$SHADOW_UNIT"
grep -Fq 'systemctl_value "$market" DropInPaths' "$GATE"
grep -Fq 'systemctl_value "$market" MemoryHigh' "$GATE"
grep -Fq 'memory_max_bytes[$market] == 5242880000' "$GATE"
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
  candidate_binary=candidate_binary
  die() { printf '%s\n' "$*" >&2; exit 1; }
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
    [[ $verifier_unit == *'--property=MemoryHigh=5000M'* ]] || exit 1
    [[ $verifier_unit == *'--property=MemoryMax=6400M'* ]] || exit 1
  done
)
run_strict_verifier_fixture

run_strict_verifier_failure_fixture() (
  local -a stopped_units=()
  strict_verifier_unit=
  strict_verifier_counter=0
  candidate_binary=candidate_binary
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
gate_run_id=20260820T000000Z-1
run_spool="/data/monday/spool/binance-lob-rust-shadow/runs/$artifact/$gate_run_id"
usdm_symbols_config=$(sed -n 's/^SYMBOLS=//p' "$SHADOW_USDM_ENV")
usdm_catalog=$(jq -cn --arg symbols "$usdm_symbols_config" \
  '$symbols | split(",") | sort' | sha256sum | awk '{print $1}')

market_json=$(jq -cn \
  --arg catalog "$catalog" \
  '{symbol_count:1200,snapshot_ready_count:1200,bridged_count:1200,
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
    | .stream_types = ["bookTicker","depth@100ms"]
    | .agg_trade_segments = 0
    | .agg_trade_count = 0
    | .raw_trade_segments = 0
    | .raw_trade_count = 0
    | .strict_trade_summary_readback = false
    | .strict_raw_trade_continuity_readback = false
    | .force_order_count = 0
    | .oss_roundtrip_evidence |= map(
      .lob_declared_symbol_count = 100 | .lob_covered_symbol_count = 100
      | .stream_coverage_verified_count = 100
      | .agg_trade_count = 0 | .raw_trade_count = 0
      | .book_ticker_count = 1 | .force_order_count = 0)' \
  <<<"$market_json")
jq -n \
  --arg artifact "$artifact" \
  --arg bundle "$bundle" \
  --arg source "$source_revision" \
  --arg run_id "$gate_run_id" \
  --arg run_spool "$run_spool" \
  --argjson market "$market_json" \
  --argjson usdm_market "$usdm_market" \
  '{schema:"monday.rust_lob_shadow_gate.v3",candidate_sha256:$artifact,
    deployment_bundle_sha256:$bundle,deployment_source_revision:$source,
    run_id:$run_id,run_spool:$run_spool,
    required_duration_seconds:240,requested_duration_seconds:240,
    health_settle_seconds:240,segment_seconds:120,test_only:false,
    observation_started_ns:150,
    passed:true,production_eligible:true,checks_passed:true,duration_seconds:240,
    markets:{spot:$market,usdm:$usdm_market}}' \
  >"$tmp_dir/gate.json"

jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null

jq 'del(.observation_started_ns)' \
  "$tmp_dir/gate.json" >"$tmp_dir/missing-observation-boundary.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/missing-observation-boundary.json" >/dev/null; then
  printf 'gate policy accepted evidence without an observation boundary\n' >&2
  exit 1
fi
jq '.observation_started_ns = 99' \
  "$tmp_dir/gate.json" >"$tmp_dir/late-evidence-start.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/late-evidence-start.json" >/dev/null; then
  printf 'gate policy accepted evidence that starts after observation\n' >&2
  exit 1
fi
jq '.observation_started_ns = 200' \
  "$tmp_dir/gate.json" >"$tmp_dir/early-evidence-end.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
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
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/usdm-legacy-stream-contract.json" >/dev/null; then
  printf 'gate policy accepted the legacy USD-M full-tape stream contract\n' >&2
  exit 1
fi
jq '.markets.usdm.book_ticker_count = 0' \
  "$tmp_dir/gate.json" >"$tmp_dir/usdm-missing-book-ticker.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/usdm-missing-book-ticker.json" >/dev/null; then
  printf 'gate policy accepted USD-M evidence without bookTicker rows\n' >&2
  exit 1
fi

jq '.markets.usdm.symbol_count = 101
    | .markets.usdm.snapshot_ready_count = 101
    | .markets.usdm.bridged_count = 101
    | .markets.usdm.stream_coverage_verified_count = 101' \
  "$tmp_dir/gate.json" >"$tmp_dir/usdm-101-symbols.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
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
  | .stream_types = ["bookTicker","depth@100ms"]
  | .agg_trade_segments = 0
  | .agg_trade_count = 0
  | .raw_trade_segments = 0
  | .raw_trade_count = 0
  | .strict_trade_summary_readback = false
  | .strict_raw_trade_continuity_readback = false
  | .force_order_count = 0
  | .oss_roundtrip_evidence |= map(
      .lob_declared_symbol_count = 100 | .lob_covered_symbol_count = 100
      | .stream_coverage_verified_count = 100
      | .agg_trade_count = 0 | .raw_trade_count = 0
      | .book_ticker_count = 1 | .force_order_count = 0)' \
  <<<"$v1_market")
jq -n \
  --arg artifact "$artifact" \
  --arg bundle "$bundle" \
  --arg source "$source_revision" \
  --arg run_id "$gate_run_id" \
  --arg run_spool "$run_spool" \
  --argjson market "$v1_market" \
  --argjson usdm_market "$v1_usdm_market" \
  '{schema:"monday.rust_lob_shadow_gate.v3",candidate_sha256:$artifact,
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
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/pre-observation-reconnect.json" >/dev/null; then
  printf 'gate policy accepted a pre-observation reconnect boundary\n' >&2
  exit 1
fi

wrong_bundle=$(printf 'e%.0s' {1..64})
if jq -e \
  --arg candidate_sha256 "$artifact" \
  --arg deployment_bundle_sha256 "$wrong_bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/gate.json" >/dev/null; then
  printf 'gate policy accepted evidence from a different deployment bundle\n' >&2
  exit 1
fi

jq '.markets.spot.oss_roundtrip_evidence[1].lob_capture_session_id = "session-2"' \
  "$tmp_dir/gate.json" >"$tmp_dir/mixed-lob-session.json"
if jq -e \
  --arg candidate_sha256 "$artifact" \
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
  --arg deployment_bundle_sha256 "$bundle" \
  --arg deployment_source_revision "$source_revision" \
  -f "$POLICY" "$tmp_dir/overlapping-agg-trades.json" >/dev/null; then
  printf 'gate policy accepted overlapping aggregate-trade segments\n' >&2
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

for market in spot usdm; do
  jq --arg market "$market" \
    '.markets[$market].max_health_silence_seconds = 91' \
    "$tmp_dir/gate.json" >"$tmp_dir/rotation-jitter-health-$market.json"
  jq -e \
    --arg candidate_sha256 "$artifact" \
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

rollback_body="$tmp_dir/rollback.sh"
sed -n '/^rollback_after_failure()/,/^}/p' "$CUTOVER" >"$rollback_body"
production_predicate_body="$tmp_dir/production-is-fail-closed.sh"
sed -n '/^production_is_fail_closed()/,/^}/p' "$CUTOVER" \
  >"$production_predicate_body"
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
grep -Fq 'ROLLBACK_RESULT=new-host-containment-failed' "$rollback_body"
grep -Fq 'binance-lob-archiver@spot.service' "$CUTOVER"
grep -Fq 'binance-lob-archiver@usdm.service' "$CUTOVER"
grep -Fq 'legacy collector unit must be disabled before cutover' "$CUTOVER"
grep -Fq 'release_staging=$(mktemp -d "$release_root/.${artifact_sha256}.new.XXXXXX")' \
  "$INSTALL_RELEASE"
grep -Fq 'COPYFILE_DISABLE=1 tar -C "$SCRIPT_DIR" -cf "$BUNDLE_PATH" "${assets[@]}"' \
  "$INSTALL_RELEASE"
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
grep -Fq 'bundle_evidence_dir="$binary_evidence_dir/$deployment_bundle_sha256"' "$GATE"
grep -Fq 'evidence_dir="$runs_dir/$gate_run_id"' "$GATE"
grep -Fq 'an immutable production-eligible gate already exists' "$GATE"
grep -Fq 'for candidate_unit in "${candidate_units[@]}"; do' "$GATE"
grep -Fq 'systemctl reset-failed "$candidate_unit" >/dev/null 2>&1 || true' "$GATE"
if grep -Fq 'rm -f "$gate_json"' "$GATE"; then
  printf 'shadow gate still deletes immutable gate evidence\n' >&2
  exit 1
fi
grep -Fq 'gate_markers=("$GATE_BUNDLE_DIR"/runs/*/PASSED.sha256)' "$CUTOVER"
grep -Fq 'rollback-deployment.sha256' "$CUTOVER"
grep -Fq 'ROLLBACK_DEPLOYMENT_MANIFEST_SHA256' "$rollback_body"
grep -Fq 'installed production asset drifted from the active immutable release' "$CUTOVER"
grep -Fq 'cmp -s -- "$source" "$installed_source"' "$CUTOVER"
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
((candidate_clear_line < candidate_timestamp_line \
  && candidate_timestamp_line < candidate_start_line \
  && candidate_start_line < candidate_health_line \
  && candidate_health_line < candidate_enable_line)) || {
  printf 'candidate no longer follows clear stale health -> timestamp -> start -> verify -> enable\n' >&2
  exit 1
}
grep -Fq '"$CANDIDATE_STARTED_NS"' "$candidate_start_body"

# Execute the rollback snapshot logic against isolated fixture roots. This catches
# content drift and manifest-tamper regressions that static contract greps miss.
installed_root="$tmp_dir/installed"
release_deployment="$tmp_dir/old-release/deployment"
stage_body="$tmp_dir/stage-existing-deployment.sh"
mkdir -p "$installed_root/systemd" "$installed_root/monday" "$release_deployment"
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
  printf 'fixture:%s\n' "$asset" >"$release_deployment/$asset"
  install -m 0644 "$release_deployment/$asset" "$installed"
done

run_stage_fixture() (
  DEPLOYMENT_ASSETS=("${deployment_assets[@]}")
  OLD_DEPLOYMENT="$release_deployment"
  EVIDENCE_DIR=$1
  ROLLBACK_DEPLOYMENT_MANIFEST_SHA256=
  fail() { printf '%s\n' "$*" >&2; exit 1; }
  validate_deployment() { return 0; }
  secure_regular_file() { [[ -f $1 && ! -L $1 ]]; }
  atomic_install() { install -m "$1" "$2" "$3"; }
  # shellcheck disable=SC1090
  . "$stage_body"
  stage_existing_deployment_for_rollback
)

snapshot_evidence="$tmp_dir/snapshot-evidence"
mkdir -p "$snapshot_evidence"
run_stage_fixture "$snapshot_evidence"
(
  cd "$snapshot_evidence/rollback-deployment"
  sha256sum --check --strict "$snapshot_evidence/rollback-deployment.sha256" >/dev/null
)
printf 'tampered\n' >> \
  "$snapshot_evidence/rollback-deployment/binance-lob-archiver-production@.service"
if (
  cd "$snapshot_evidence/rollback-deployment"
  sha256sum --check --strict "$snapshot_evidence/rollback-deployment.sha256" >/dev/null 2>&1
); then
  printf 'rollback manifest accepted a tampered snapshot\n' >&2
  exit 1
fi

printf 'drifted\n' >>"$installed_root/monday/binance-lob-archiver-production-spot.env"
drift_evidence="$tmp_dir/drift-evidence"
mkdir -p "$drift_evidence"
if run_stage_fixture "$drift_evidence" >"$tmp_dir/drift.out" 2>&1; then
  printf 'rollback snapshot accepted installed configuration drift\n' >&2
  exit 1
fi
grep -Fq 'installed production asset drifted from the active immutable release' \
  "$tmp_dir/drift.out"

run_new_host_rollback_fixture() (
  local active_unit=${1:-} unit
  PRODUCTION_UNITS=(production-spot production-usdm)
  UPLOAD_UNITS=(upload-spot upload-usdm)
  LEGACY_UNITS=(legacy-spot legacy-usdm)
  TRANSITION_MASK_UNITS=("${PRODUCTION_UNITS[@]}" "${UPLOAD_UNITS[@]}" "${LEGACY_UNITS[@]}")
  CANONICAL_SPOOL="$tmp_dir/nonexistent-spool"
  CANDIDATE_DEPLOYMENT="$tmp_dir/candidate-deployment"
  CANDIDATE_BINARY="$tmp_dir/candidate-binary"
  PRODUCTION_LINK="$tmp_dir/nonexistent-production-link"
  OLD_MODE=new-host
  ROLLBACK_RESULT=
  DRAIN_REQUIRED=0
  DRAIN_ATTEMPTED=0
  DRAIN_MAY_HAVE_MUTATED=0
  SPOOL_ENV_DEPLOYMENT=
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
  # shellcheck disable=SC1090
  . "$production_predicate_body"
  # shellcheck disable=SC1090
  . "$rollback_body"
  rollback_after_failure
  printf '%s\n' "$ROLLBACK_RESULT"
)

[[ $(run_new_host_rollback_fixture) == new-host-disabled ]]
[[ $(run_new_host_rollback_fixture legacy-spot) == new-host-containment-failed ]]
[[ $(run_new_host_rollback_fixture upload-usdm) == new-host-containment-failed ]]

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
