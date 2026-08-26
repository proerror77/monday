#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf 'Usage: %s <candidate-binary-sha256>\n' "${0##*/}" >&2
}

configure_paths() {
  local root=${1%/}
  OPT_ROOT="$root/opt/monday"
  BIN_DIR="$root/opt/monday/bin"
  RELEASE_ROOT="$OPT_ROOT/releases/binance-lob-archiver"
  PRODUCTION_LINK="$BIN_DIR/binance-lob-archiver"
  SYSTEMD_ROOT="$root/etc/systemd/system"
  CONFIG_ROOT="$root/etc/monday"
  DATA_ROOT="$root/data"
  PROC_ROOT="$root/proc"
  GATE_ROOT="$root/data/monday/evidence/shadow-gates"
  EVIDENCE_ROOT="$root/data/monday/evidence/recoveries"
  LOCK_ROOT="$root/run/lock"
  CANONICAL_SPOOL="$root/data/monday/spool/binance-lob"
  RECOVERY_QUEUE_ROOT="$root/data/monday/spool/binance-lob-recovery"
  HEALTH_TIMEOUT_SECONDS=300
  EXPECTED_ROOT_UID=0
}

PRODUCTION_UNITS=(
  binance-lob-archiver-production@spot.service
  binance-lob-archiver-production@usdm.service
)
UPLOAD_UNITS=(
  binance-lob-archiver-upload@spot.service
  binance-lob-archiver-upload@usdm.service
)
RECOVERY_TIMERS=(
  binance-lob-archiver-recovery@spot.timer
  binance-lob-archiver-recovery@usdm.timer
)
LEGACY_UNITS=(
  binance-lob-archiver@spot.service
  binance-lob-archiver@usdm.service
)
TRANSITION_MASK_UNITS=(
  "${PRODUCTION_UNITS[@]}"
  "${UPLOAD_UNITS[@]}"
  "${LEGACY_UNITS[@]}"
)
RESTORE_ASSETS=(
  binance-lob-archiver-production@.service
  binance-lob-archiver-recovery@.service
  binance-lob-archiver-recovery@.timer
  binance-lob-archiver-production-spot.env
  binance-lob-archiver-production-usdm.env
  host-rust-lob-recovery-queue.sh
)
HEALTH_DEPLOYMENT_ASSET=monday-collector-health.sh

fail() {
  FAILURE_REASON=$*
  printf '%s\n' "$FAILURE_REASON" >&2
  exit 1
}

path_is_direct_or_absent() {
  local path=$1 resolved
  [[ -e $path || -L $path ]] || return 0
  [[ -d $path && ! -L $path ]] || return 1
  resolved=$(readlink -f "$path") || return 1
  [[ $resolved == "$path" ]]
}

secure_regular_file() {
  local path=$1 mode owner
  [[ -f $path && ! -L $path ]] || fail "required regular file is missing or a symlink: $path"
  owner=$(stat -c %u -- "$path")
  mode=$(stat -c %a -- "$path")
  [[ $owner == "$EXPECTED_ROOT_UID" ]] || fail "required file is not root-owned: $path"
  (( (8#$mode & 022) == 0 )) || fail "required file is group/world writable: $path"
}

canonical_spool_paths_safe() {
  local path
  for path in \
    "$DATA_ROOT/monday" \
    "$DATA_ROOT/monday/spool" \
    "$CANONICAL_SPOOL" \
    "$CANONICAL_SPOOL/spot" \
    "$CANONICAL_SPOOL/usdm"; do
    path_is_direct_or_absent "$path" || return 1
  done
}

segment_artifacts() {
  canonical_spool_paths_safe || return 1
  [[ -d $CANONICAL_SPOOL ]] || return 0
  find "$CANONICAL_SPOOL" \( -type f -o -type l \) \( \
    -name 'part-*' -o \
    -name '*.manifest.json' -o \
    -name '*.jsonl.part' -o \
    -name '*.zst.tmp' -o \
    -name '*.part.corrupt' -o \
    -name '*.jsonl.zst' -o \
    -name '*._SUCCESS' -o \
    -name '*.uploaded-cleanup.json' -o \
    -name '*.uploaded-cleanup.json.tmp' \
  \) -print
}

require_empty_segment_spool() {
  local remaining
  remaining=$(segment_artifacts) || return 1
  if [[ -n $remaining ]]; then
    printf '%s\n' "$remaining" >&2
    return 1
  fi
}

copy_health_evidence() {
  local label=$1 market source
  for market in spot usdm; do
    source="$CANONICAL_SPOOL/$market/health.json"
    if [[ -f $source && ! -L $source ]]; then
      install -m 0640 "$source" "$EVIDENCE_DIR/$label-$market-health.json"
    fi
  done
}

health_ready_for_release() {
  local market=$1 minimum_symbols=$2 old_session=$3 minimum_updated_ns=${4:-0}
  local expected_dataset=${5:-} health
  health="$CANONICAL_SPOOL/$market/health.json"
  [[ -f $health && ! -L $health ]] || return 1
  case "$market" in
    spot) expected_dataset=spot_all ;;
    usdm) [[ -n $expected_dataset ]] || return 1 ;;
    *) return 1 ;;
  esac
  jq -e \
    --arg expected_market "$market" \
    --arg expected_dataset "$expected_dataset" \
    --arg old_session "$old_session" \
    --argjson minimum_symbols "$minimum_symbols" \
    --argjson minimum_updated_ns "$minimum_updated_ns" \
    -f "$RUNTIME_HEALTH_POLICY" "$health" >/dev/null
}

runtime_matches_release() {
  local binary=$1 require_enabled=$2 unit restarts main_pid main_exe
  for unit in "${PRODUCTION_UNITS[@]}"; do
    systemctl is-active --quiet "$unit" || return 1
    restarts=$(systemctl show "$unit" --property=NRestarts --value) || return 1
    [[ $restarts == 0 ]] || return 1
    main_pid=$(systemctl show "$unit" --property=MainPID --value) || return 1
    [[ $main_pid =~ ^[1-9][0-9]*$ ]] || return 1
    main_exe=$(readlink -f "$PROC_ROOT/$main_pid/exe" 2>/dev/null || true)
    [[ $main_exe == "$binary" ]] || return 1
    if [[ $require_enabled == true ]]; then
      systemctl is-enabled --quiet "$unit" || return 1
    fi
  done
}

wait_for_release_health() {
  local binary=$1 old_spot_session=$2 old_usdm_session=$3
  local minimum_updated_ns=${4:-0} deadline unit
  deadline=$((SECONDS + HEALTH_TIMEOUT_SECONDS))
  while (( SECONDS < deadline )); do
    for unit in "${PRODUCTION_UNITS[@]}"; do
      systemctl is-active --quiet "$unit" || return 1
    done
    if health_ready_for_release spot 1000 "$old_spot_session" "$minimum_updated_ns" \
      && health_ready_for_release usdm "$USDM_MINIMUM_SYMBOLS" \
        "$old_usdm_session" "$minimum_updated_ns" "$USDM_EXPECTED_DATASET" \
      && runtime_matches_release "$binary" false; then
      return 0
    fi
    sleep 5
  done
  return 1
}

clear_health_before_restart() {
  local market health
  canonical_spool_paths_safe || return 1
  for market in spot usdm; do
    health="$CANONICAL_SPOOL/$market/health.json"
    rm -f -- "$health" || return 1
    [[ ! -e $health && ! -L $health ]] || return 1
  done
}

production_is_fail_closed() {
  local unit state
  for unit in "${TRANSITION_MASK_UNITS[@]}"; do
    systemctl is-active --quiet "$unit" && return 1
    state=$(systemctl is-enabled "$unit" 2>/dev/null || true)
    [[ $state == masked || $state == masked-runtime ]] || return 1
  done
}

unit_active_json() {
  local unit=$1
  if systemctl is-active --quiet "$unit"; then
    printf true
  else
    printf false
  fi
}

write_recovery_evidence() {
  local temporary current_target spot_active usdm_active
  [[ -d $EVIDENCE_DIR ]] || return 1
  temporary="$EVIDENCE_DIR/recovery.json.tmp"
  current_target=$(readlink -f "$PRODUCTION_LINK" 2>/dev/null || true)
  spot_active=$(unit_active_json "${PRODUCTION_UNITS[0]}")
  usdm_active=$(unit_active_json "${PRODUCTION_UNITS[1]}")
  jq -n \
    --arg schema monday.rust_lob_recovery.v1 \
    --arg started_at "$STARTED_AT" \
    --arg completed_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg result "$RESULT" \
    --arg step "$STEP" \
    --arg failure_reason "$FAILURE_REASON" \
    --arg rollback_result "$ROLLBACK_RESULT" \
    --arg candidate_sha256 "$CANDIDATE_SHA256" \
    --arg deployment_bundle_sha256 "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg health_script_status "$HEALTH_SCRIPT_STATUS" \
    --arg previous_session_spot "$OLD_SESSION_SPOT" \
    --arg previous_session_usdm "$OLD_SESSION_USDM" \
    --arg current_binary "$current_target" \
    --argjson spot_active "$spot_active" \
    --argjson usdm_active "$usdm_active" \
    '{
      schema: $schema,
      started_at: $started_at,
      completed_at: $completed_at,
      result: $result,
      last_step: $step,
      failure_reason: (if $failure_reason == "" then null else $failure_reason end),
      rollback_result: $rollback_result,
      candidate_sha256: $candidate_sha256,
      deployment_bundle_sha256: (if $deployment_bundle_sha256 == "" then null else $deployment_bundle_sha256 end),
      health_script_status: $health_script_status,
      previous_session_spot: (if $previous_session_spot == "" then null else $previous_session_spot end),
      previous_session_usdm: (if $previous_session_usdm == "" then null else $previous_session_usdm end),
      current_binary: (if $current_binary == "" then null else $current_binary end),
      production_units_active: {spot: $spot_active, usdm: $usdm_active}
    }' > "$temporary" || return 1
  chmod 0640 "$temporary" || return 1
  mv -Tf "$temporary" "$EVIDENCE_DIR/recovery.json" || return 1
}

write_verification_evidence() {
  local temporary runtime_max_sec
  local spot_unit=${PRODUCTION_UNITS[0]} usdm_unit=${PRODUCTION_UNITS[1]}
  local spot_active spot_enabled usdm_active usdm_enabled
  [[ -d $EVIDENCE_DIR ]] || return 1
  temporary="$EVIDENCE_DIR/verification.json.tmp"
  runtime_max_sec=$(sed -n 's/^RuntimeMaxSec=//p' \
    "$SYSTEMD_ROOT/binance-lob-archiver-production@.service" | sed -n '1p') || return 1
  [[ $runtime_max_sec =~ ^[1-9][0-9]*$ ]] || return 1
  if systemctl is-active --quiet "$spot_unit"; then spot_active=true; else spot_active=false; fi
  if systemctl is-enabled --quiet "$spot_unit"; then spot_enabled=true; else spot_enabled=false; fi
  if systemctl is-active --quiet "$usdm_unit"; then usdm_active=true; else usdm_active=false; fi
  if systemctl is-enabled --quiet "$usdm_unit"; then usdm_enabled=true; else usdm_enabled=false; fi
  jq -n \
    --arg schema monday.rust_lob_recovery_verification.v1 \
    --arg candidate_sha256 "$CANDIDATE_SHA256" \
    --arg deployment_bundle_sha256 "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg health_script_status "$HEALTH_SCRIPT_STATUS" \
    --arg gate_marker "$GATE_MARKER" \
    --arg current_binary "$(readlink -f "$PRODUCTION_LINK" 2>/dev/null || true)" \
    --arg spot_unit "$spot_unit" \
    --arg usdm_unit "$usdm_unit" \
    --argjson runtime_max_sec "$runtime_max_sec" \
    --argjson spot_active "$spot_active" \
    --argjson spot_enabled "$spot_enabled" \
    --argjson usdm_active "$usdm_active" \
    --argjson usdm_enabled "$usdm_enabled" \
    '{schema:$schema,candidate_sha256:$candidate_sha256,
      deployment_bundle_sha256:$deployment_bundle_sha256,
      health_script_status:$health_script_status,
      gate_marker:$gate_marker,current_binary:$current_binary,
      production_units:{
        ($spot_unit):{active:$spot_active,enabled:$spot_enabled,runtime_max_sec:$runtime_max_sec},
        ($usdm_unit):{active:$usdm_active,enabled:$usdm_enabled,runtime_max_sec:$runtime_max_sec}
      },
      verification:{symlink_sha256:true,gate_marker_verified:true,
        installed_assets_match_bundle:($health_script_status == "bundled-verified"),
        health_script_matches_bundle:($health_script_status == "bundled-verified"),
        runtime_max_sec_declared:true}}' \
    > "$temporary" || return 1
  chmod 0640 "$temporary" || return 1
  mv -Tf "$temporary" "$EVIDENCE_DIR/verification.json" || return 1
}

rollback_after_failure() {
  local unit
  ROLLBACK_RESULT=disabled
  systemctl disable --now "${RECOVERY_TIMERS[@]}" >/dev/null 2>&1 || true
  systemctl disable --now "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
  systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}" >/dev/null 2>&1 || true
  for unit in "${PRODUCTION_UNITS[@]}" "${RECOVERY_TIMERS[@]}"; do
    if systemctl is-active --quiet "$unit" || systemctl is-enabled --quiet "$unit"; then
      ROLLBACK_RESULT=production-stop-or-disable-failed-but-contained
    fi
  done
  if [[ $ROLLBACK_RESULT == disabled ]] && ! production_is_fail_closed; then
    ROLLBACK_RESULT=production-stop-or-disable-containment-failed
  fi
  copy_health_evidence rollback
}

on_error() {
  local rc=$?
  if [[ -z $FAILURE_REASON ]]; then
    FAILURE_REASON="command failed with exit $rc during $STEP: $BASH_COMMAND"
  fi
}

on_exit() {
  local rc=$?
  trap - EXIT ERR
  set +e
  if (( SUCCESS == 0 )); then
    RESULT=failed
    if (( UNITS_STARTED )); then
      rollback_after_failure
    fi
    if write_recovery_evidence; then
      printf 'restore failed; evidence: %s/recovery.json\n' "$EVIDENCE_DIR" >&2
    else
      printf 'restore failed and evidence could not be written under %s\n' "$EVIDENCE_DIR" >&2
    fi
  fi
  exit "$rc"
}

restore_release() (
  set -Eeuo pipefail
  CANDIDATE_SHA256=$1
  CANDIDATE_RELEASE="$RELEASE_ROOT/$CANDIDATE_SHA256"
  CANDIDATE_BINARY="$CANDIDATE_RELEASE/binance-lob-archiver"
  CANDIDATE_DEPLOYMENT="$CANDIDATE_RELEASE/deployment"
  GATE_POLICY="$CANDIDATE_DEPLOYMENT/rust-lob-shadow-gate-policy.jq"
  RUNTIME_HEALTH_POLICY="$CANDIDATE_DEPLOYMENT/rust-lob-runtime-health-policy.jq"
  GATE_BUNDLE_DIR=
  GATE_DIR=
  GATE_JSON=
  GATE_MARKER=
  DEPLOYMENT_BUNDLE_SHA256=
  DEPLOYMENT_SOURCE_REVISION=
  STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  EVIDENCE_DIR="$EVIDENCE_ROOT/$(date -u +%Y%m%dT%H%M%SZ)-${CANDIDATE_SHA256:0:12}-$$"
  STEP=preflight
  RESULT=preflight
  FAILURE_REASON=
  ROLLBACK_RESULT=not-needed
  SUCCESS=0
  UNITS_STARTED=0
  RESTART_STARTED_NS=0
  OLD_SESSION_SPOT=
  OLD_SESSION_USDM=
  USDM_MINIMUM_SYMBOLS=400
  USDM_EXPECTED_DATASET=
  HEALTH_SCRIPT_STATUS=unchecked
  trap on_error ERR
  trap on_exit EXIT

  STEP=prepare-recovery-evidence
  for path in \
    "$DATA_ROOT" \
    "$DATA_ROOT/monday" \
    "$DATA_ROOT/monday/evidence" \
    "$EVIDENCE_ROOT" \
    "$DATA_ROOT/monday/spool" \
    "$RECOVERY_QUEUE_ROOT"; do
    path_is_direct_or_absent "$path" || fail "recovery evidence path contains a symlink: $path"
  done
  install -d -m 0750 -o root -g root \
    "$DATA_ROOT/monday/evidence" "$EVIDENCE_ROOT"
  install -d -m 0750 -o root -g hftcollector "$RECOVERY_QUEUE_ROOT"
  mkdir -m 0750 -- "$EVIDENCE_DIR" \
    || fail "refusing to reuse recovery evidence directory: $EVIDENCE_DIR"

  STEP=validate-candidate-release
  for path in "$OPT_ROOT" "$BIN_DIR" "$RELEASE_ROOT" "$CANDIDATE_RELEASE" "$CANDIDATE_DEPLOYMENT"; do
    path_is_direct_or_absent "$path" || fail "release path contains a symlink: $path"
  done
  secure_regular_file "$CANDIDATE_BINARY"
  [[ -x $CANDIDATE_BINARY ]] || fail "candidate binary is not executable: $CANDIDATE_BINARY"
  printf '%s  %s\n' "$CANDIDATE_SHA256" "$CANDIDATE_BINARY" | sha256sum --check --strict
  secure_regular_file "$CANDIDATE_RELEASE/release.json"
  secure_regular_file "$GATE_POLICY"
  secure_regular_file "$RUNTIME_HEALTH_POLICY"
  DEPLOYMENT_BUNDLE_SHA256=$(jq -er '.deployment_bundle_sha256' \
    "$CANDIDATE_RELEASE/release.json")
  DEPLOYMENT_SOURCE_REVISION=$(jq -er '.deployment_source_revision' \
    "$CANDIDATE_RELEASE/release.json")
  [[ $DEPLOYMENT_BUNDLE_SHA256 =~ ^[a-f0-9]{64}$ ]] \
    || fail 'candidate release has an invalid deployment bundle SHA-256'
  [[ $DEPLOYMENT_SOURCE_REVISION =~ ^[a-f0-9]{40,64}$ ]] \
    || fail 'candidate release has an invalid deployment source revision'
  jq -e --arg sha "$CANDIDATE_SHA256" --arg bundle "$DEPLOYMENT_BUNDLE_SHA256" \
    '.artifact_sha256 == $sha and .deployment_bundle_sha256 == $bundle' \
    "$CANDIDATE_RELEASE/release.json" >/dev/null \
    || fail 'candidate release metadata does not match the requested identity'
  GATE_BUNDLE_DIR="$GATE_ROOT/$CANDIDATE_SHA256/$DEPLOYMENT_BUNDLE_SHA256"

  STEP=validate-production-symlink
  [[ -L $PRODUCTION_LINK ]] || fail 'production symlink is missing'
  current_target=$(readlink -f "$PRODUCTION_LINK") \
    || fail 'production symlink is dangling'
  [[ $current_target == "$CANDIDATE_BINARY" ]] \
    || fail "production symlink does not resolve to the candidate release: $current_target"
  printf '%s  %s\n' "$CANDIDATE_SHA256" "$current_target" | sha256sum --check --strict

  STEP=validate-shadow-gate
  for path in "$GATE_ROOT" "$GATE_ROOT/$CANDIDATE_SHA256" "$GATE_BUNDLE_DIR" \
    "$GATE_BUNDLE_DIR/runs"; do
    path_is_direct_or_absent "$path" || fail "shadow gate path contains a symlink: $path"
  done
  shopt -s nullglob
  gate_markers=("$GATE_BUNDLE_DIR"/runs/*/PASSED.sha256)
  shopt -u nullglob
  (( ${#gate_markers[@]} == 1 )) \
    || fail "expected exactly one immutable passed shadow gate, found ${#gate_markers[@]}"
  GATE_MARKER=${gate_markers[0]}
  GATE_DIR=${GATE_MARKER%/*}
  GATE_JSON="$GATE_DIR/gate.json"
  path_is_direct_or_absent "$GATE_DIR" \
    || fail "shadow gate run path contains a symlink: $GATE_DIR"
  secure_regular_file "$GATE_JSON"
  secure_regular_file "$GATE_MARKER"
  [[ $(wc -l < "$GATE_MARKER") -eq 1 ]] || fail 'PASSED.sha256 must contain exactly one entry'
  marker_entry=$(<"$GATE_MARKER")
  [[ $marker_entry =~ ^[A-Fa-f0-9]{64}[[:space:]]+gate\.json$ ]] \
    || fail 'PASSED.sha256 must contain only the gate.json SHA-256 entry'
  (cd "$GATE_DIR" && sha256sum --check --strict PASSED.sha256)
  jq -e \
    --arg candidate_sha256 "$CANDIDATE_SHA256" \
    --arg deployment_bundle_sha256 "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg deployment_source_revision "$DEPLOYMENT_SOURCE_REVISION" \
    -f "$GATE_POLICY" "$GATE_JSON" >/dev/null \
    || fail 'candidate shadow gate does not meet production thresholds'
  GATE_USDM_SYMBOLS=$(jq -er '.markets.usdm.symbols_config' "$GATE_JSON")
  CANDIDATE_USDM_SYMBOLS=$(sed -n 's/^SYMBOLS=//p' \
    "$CANDIDATE_DEPLOYMENT/binance-lob-archiver-production-usdm.env")
  [[ $GATE_USDM_SYMBOLS == "$CANDIDATE_USDM_SYMBOLS" ]] \
    || fail 'candidate shadow gate USD-M symbols differ from the deployment bundle'
  install -d -m 0750 "$EVIDENCE_DIR/shadow-gate"
  install -m 0640 "$GATE_JSON" "$EVIDENCE_DIR/shadow-gate/gate.json"
  install -m 0640 "$GATE_MARKER" "$EVIDENCE_DIR/shadow-gate/PASSED.sha256"

  STEP=validate-production-quiescent
  for unit in "${PRODUCTION_UNITS[@]}"; do
    systemctl is-active --quiet "$unit" \
      && fail "production unit is active; refusing live production: $unit"
  done

  STEP=validate-canonical-spool
  canonical_spool_paths_safe \
    || fail 'canonical spool path contains a symlink or escapes /data'
  if [[ ! -d $CANONICAL_SPOOL/spot || ! -d $CANONICAL_SPOOL/usdm ]]; then
    install -d -m 0750 -o hftcollector -g hftcollector \
      "$CANONICAL_SPOOL/spot" "$CANONICAL_SPOOL/usdm"
  fi

  STEP=validate-recovery-isolation
  grep -Fxq 'ExecStartPre=+/opt/monday/bin/monday-rust-lob-recovery-queue isolate %i' \
    "$SYSTEMD_ROOT/binance-lob-archiver-production@.service" \
    || fail 'installed production unit cannot isolate interrupted spools'

  STEP=validate-installed-production-assets
  for asset in "${RESTORE_ASSETS[@]}"; do
    case "$asset" in
      *.service|*.timer) installed_asset="$SYSTEMD_ROOT/$asset" ;;
      *.env) installed_asset="$CONFIG_ROOT/$asset" ;;
      host-rust-lob-recovery-queue.sh)
        installed_asset="$BIN_DIR/monday-rust-lob-recovery-queue" ;;
    esac
    secure_regular_file "$installed_asset"
    secure_regular_file "$CANDIDATE_DEPLOYMENT/$asset"
    cmp -s -- "$CANDIDATE_DEPLOYMENT/$asset" "$installed_asset" \
      || fail "installed production asset drifted from the gated deployment bundle: $installed_asset"
  done
  candidate_health_script="$CANDIDATE_DEPLOYMENT/$HEALTH_DEPLOYMENT_ASSET"
  installed_health_script="$BIN_DIR/monday-collector-health.sh"
  if [[ -e $candidate_health_script || -L $candidate_health_script ]]; then
    secure_regular_file "$candidate_health_script"
    secure_regular_file "$installed_health_script"
    cmp -s -- "$candidate_health_script" "$installed_health_script" \
      || fail "installed production asset drifted from the gated deployment bundle: $installed_health_script"
    HEALTH_SCRIPT_STATUS=bundled-verified
  elif [[ -e $installed_health_script || -L $installed_health_script ]]; then
    secure_regular_file "$installed_health_script"
    HEALTH_SCRIPT_STATUS=legacy-installed-unbound
  else
    HEALTH_SCRIPT_STATUS=legacy-not-installed
  fi
  if [[ $(sed -n 's/^SYMBOLS=//p' \
    "$CANDIDATE_DEPLOYMENT/binance-lob-archiver-production-usdm.env") != ALL ]]; then
    USDM_MINIMUM_SYMBOLS=100
  fi
  dataset_lines=$(grep -Ec '^DATASET=[^[:space:]]+$' \
    "$CANDIDATE_DEPLOYMENT/binance-lob-archiver-production-usdm.env" || true)
  [[ $dataset_lines == 1 ]] || fail 'candidate USD-M deployment must declare exactly one DATASET'
  USDM_EXPECTED_DATASET=$(sed -n 's/^DATASET=//p' \
    "$CANDIDATE_DEPLOYMENT/binance-lob-archiver-production-usdm.env")
  [[ $USDM_EXPECTED_DATASET == usdm_perpetual_all \
    || $USDM_EXPECTED_DATASET == usdm_perpetual_top100_lob ]] \
    || fail "candidate USD-M deployment has unsupported DATASET=$USDM_EXPECTED_DATASET"
  grep -Fxq 'RuntimeMaxSec=21600' "$SYSTEMD_ROOT/binance-lob-archiver-production@.service" \
    || fail 'installed production unit no longer declares RuntimeMaxSec=21600'

  STEP=read-pre-restore-health
  OLD_SESSION_SPOT=$(jq -r '.session_id // empty' \
    "$CANONICAL_SPOOL/spot/health.json" 2>/dev/null || true)
  OLD_SESSION_USDM=$(jq -r '.session_id // empty' \
    "$CANONICAL_SPOOL/usdm/health.json" 2>/dev/null || true)

  STEP=clear-stale-health
  copy_health_evidence previous
  clear_health_before_restart \
    || fail 'could not clear stale production health before starting the restore'

  STEP=start-restored-production
  UNITS_STARTED=1
  RESTART_STARTED_NS=$(date +%s%N)
  systemctl reset-failed "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
  systemctl unmask --runtime "${PRODUCTION_UNITS[@]}" >/dev/null
  systemctl start "${PRODUCTION_UNITS[@]}"

  STEP=verify-restored-production
  wait_for_release_health \
    "$CANDIDATE_BINARY" "$OLD_SESSION_SPOT" "$OLD_SESSION_USDM" "$RESTART_STARTED_NS" \
    || fail 'restored production did not reach verified catalog health'
  copy_health_evidence production

  STEP=enable-restored-production
  systemctl enable "${PRODUCTION_UNITS[@]}" >/dev/null
  runtime_matches_release "$CANDIDATE_BINARY" true \
    || fail 'restored runtime identity changed while enabling production'
  health_ready_for_release spot 1000 "$OLD_SESSION_SPOT" "$RESTART_STARTED_NS" \
    || fail 'Spot health changed while enabling production'
  health_ready_for_release usdm "$USDM_MINIMUM_SYMBOLS" \
    "$OLD_SESSION_USDM" "$RESTART_STARTED_NS" "$USDM_EXPECTED_DATASET" \
    || fail 'USD-M health changed while enabling production'
  systemctl enable --now "${RECOVERY_TIMERS[@]}" >/dev/null
  for timer in "${RECOVERY_TIMERS[@]}"; do
    systemctl is-enabled --quiet "$timer" \
      || fail "recovery timer did not enable: $timer"
    systemctl is-active --quiet "$timer" \
      || fail "recovery timer did not start: $timer"
  done

  STEP=write-recovery-evidence
  RESULT=passed
  ROLLBACK_RESULT=not-needed
  write_recovery_evidence || fail 'could not write restore evidence'
  write_verification_evidence || fail 'could not write restore verification'
  SUCCESS=1
  trap - EXIT ERR
  printf 'Rust collector restore passed: %s\nEvidence: %s/recovery.json\n' \
    "$CANDIDATE_SHA256" "$EVIDENCE_DIR"
)

main() {
  [[ ${EUID:-$(id -u)} -eq 0 ]] || { printf 'must run as root\n' >&2; exit 2; }
  if [[ $# -ne 1 || ! $1 =~ ^[A-Fa-f0-9]{64}$ ]]; then
    usage
    exit 2
  fi
  for command in awk chmod cmp date env find flock grep id install jq ln mkdir mountpoint \
    mv readlink rm runuser sed sha256sum sleep stat systemctl tr wc; do
    if ! command -v "$command" >/dev/null 2>&1; then
      printf 'missing required command: %s\n' "$command" >&2
      exit 2
    fi
  done
  configure_paths ''
  install -d -m 0755 "$LOCK_ROOT"
  exec 9>"$LOCK_ROOT/monday-rust-lob-release.lock"
  if ! flock -n 9; then
    printf 'another Rust collector release operation holds the host lock\n' >&2
    exit 1
  fi
  if [[ ! -d $DATA_ROOT || -L $DATA_ROOT ]] || ! mountpoint -q "$DATA_ROOT"; then
    printf '/data must be a mounted filesystem\n' >&2
    exit 1
  fi
  restore_release "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')"
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
