#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf 'Usage: %s <candidate-binary-sha256>\n' "${0##*/}" >&2
  printf '       %s resume-rollback --failed-receipt PATH --failed-receipt-sha256 SHA --rollback-snapshot-sha256 SHA --orphan-inventory PATH --orphan-inventory-sha256 SHA --request-id ID\n' "${0##*/}" >&2
}

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  printf 'must run as root\n' >&2
  exit 2
fi
OPERATION=cutover
FAILED_RECEIPT='' FAILED_RECEIPT_SHA256='' RESUME_SNAPSHOT_SHA256=''
ORPHAN_INVENTORY='' ORPHAN_INVENTORY_SHA256='' RECOVERY_REQUEST_ID=''
if [[ ${1:-} == resume-rollback ]]; then
  OPERATION=resume-rollback
  shift
  parsed_options=' '
  while (($#)); do
    [[ $# -ge 2 && $parsed_options != *" $1 "* ]] || { usage; exit 2; }
    parsed_options+="$1 "
    case $1 in
      --failed-receipt) FAILED_RECEIPT=$2 ;;
      --failed-receipt-sha256) FAILED_RECEIPT_SHA256=$2 ;;
      --rollback-snapshot-sha256) RESUME_SNAPSHOT_SHA256=$2 ;;
      --orphan-inventory) ORPHAN_INVENTORY=$2 ;;
      --orphan-inventory-sha256) ORPHAN_INVENTORY_SHA256=$2 ;;
      --request-id) RECOVERY_REQUEST_ID=$2 ;;
      *) usage; exit 2 ;;
    esac
    shift 2
  done
  [[ $FAILED_RECEIPT =~ ^/data/monday/evidence/bybit-options-cutovers/[0-9]{8}T[0-9]{6}Z-[a-f0-9]{12}-[0-9]+/cutover\.json$ \
    && $FAILED_RECEIPT_SHA256 =~ ^[a-f0-9]{64}$ && $RESUME_SNAPSHOT_SHA256 =~ ^[a-f0-9]{64}$ \
    && $ORPHAN_INVENTORY_SHA256 =~ ^[a-f0-9]{64}$ && $ORPHAN_INVENTORY == /* \
    && $RECOVERY_REQUEST_ID =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,79}$ ]] || { usage; exit 2; }
else
  [[ $# == 1 && $1 =~ ^[A-Fa-f0-9]{64}$ ]] || { usage; exit 2; }
fi

for command in awk chmod chown cmp date env find flock grep id install jq ln mkdir mountpoint mv paste readlink rm runuser sed sha256sum sleep stat systemctl timeout tr wc; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'missing required command: %s\n' "$command" >&2
    exit 2
  fi
done

if [[ $OPERATION == resume-rollback ]]; then
  [[ -f $FAILED_RECEIPT && ! -L $FAILED_RECEIPT \
    && $(stat -c %s "$FAILED_RECEIPT") -le 65536 \
    && $(sha256sum "$FAILED_RECEIPT" | awk '{print $1}') == "$FAILED_RECEIPT_SHA256" ]] || exit 2
  CANDIDATE_SHA256=$(jq -er '.candidate_sha256|select(test("^[a-f0-9]{64}$"))' "$FAILED_RECEIPT")
else
  CANDIDATE_SHA256=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')
fi
RELEASE_ROOT=/opt/monday/releases/bybit-options-archiver
CANDIDATE_RELEASE="$RELEASE_ROOT/$CANDIDATE_SHA256"
CANDIDATE_BINARY="$CANDIDATE_RELEASE/bybit-options-archiver"
CANDIDATE_DEPLOYMENT="$CANDIDATE_RELEASE/deployment"
GATE_POLICY="$CANDIDATE_DEPLOYMENT/bybit-options-shadow-gate-policy.jq"
RUNTIME_HEALTH_POLICY="$CANDIDATE_DEPLOYMENT/bybit-options-runtime-health-policy.jq"
CONTROL_PLANE_LIB="$CANDIDATE_DEPLOYMENT/bybit-options-control-plane-lib.sh"
GATE_ROOT=/data/monday/evidence/bybit-options-shadow-gates
GATE_BUNDLE_DIR=
GATE_DIR=
GATE_JSON=
GATE_MARKER=
DEPLOYMENT_BUNDLE_SHA256=
DEPLOYMENT_SOURCE_REVISION=
PRODUCTION_LINK=/opt/monday/bin/bybit-options-archiver
SHADOW_LINK=/opt/monday/bin/bybit-options-archiver-shadow
CANONICAL_SPOOL=/data/monday/spool/bybit-options
HEALTH_TIMEOUT_SECONDS=300
BOOTSTRAP_TIMEOUT_SECONDS=1500
BOOTSTRAP_DEADLINE=0
MINIMUM_SYMBOLS=500
SAFE_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EVIDENCE_DIR="/data/monday/evidence/bybit-options-cutovers/$(date -u +%Y%m%dT%H%M%SZ)-${CANDIDATE_SHA256:0:12}-$$"
SYSTEMD_DIR=/etc/systemd/system
PROC_ROOT=/proc
RECOVERY_REQUEST_ROOT='' CUSTODY_DIR=''
MASK_OWNED_UNITS=' ' MASK_SEQUENCE=0
if [[ $OPERATION == resume-rollback ]]; then
  RECOVERY_REQUEST_ROOT="${FAILED_RECEIPT%/*}/rollback-recoveries/$RECOVERY_REQUEST_ID"
  CUSTODY_DIR="$RECOVERY_REQUEST_ROOT/custody"
  EVIDENCE_DIR="$RECOVERY_REQUEST_ROOT/runs/$(date -u +%Y%m%dT%H%M%SZ)-$$"
fi

UNIT=bybit-options-archiver.service
UPLOAD_UNIT=bybit-options-upload.service
TIMER=bybit-options-upload.timer
PRODUCTION_UNITS=("$UNIT")
UPLOAD_UNITS=("$UPLOAD_UNIT" "$TIMER")
TRANSITION_MASK_UNITS=("$UNIT" "$UPLOAD_UNIT" "$TIMER")
DEPLOYMENT_ASSETS=(
  bybit-options-archiver.service
  bybit-options-upload.service
  bybit-options-upload.timer
  bybit-options-runtime-health-policy.jq
  bybit-options-shadow-gate-policy.jq
  bybit-options-control-plane-lib.sh
)
DRAIN_ENV_KEYS=(
  BYBIT_OPTIONS_SPOOL_DIR
  BYBIT_OPTIONS_SEGMENT_SECONDS
  BYBIT_OPTIONS_MAX_SEGMENT_BYTES
  BYBIT_OPTIONS_LOCAL_ZST_RETENTION_SECONDS
  MIN_FREE_GB
  BYBIT_OPTIONS_SPOOL_MAX_BYTES
  OSS_BUCKET
  OSS_ENDPOINT
  OSS_REGION
  ALIYUN_PROFILE
)

install -d -m 0755 /run/lock
exec 9>/run/lock/monday-bybit-options-release.lock
if ! flock -n 9; then
  printf 'another Bybit Options release operation holds the host lock\n' >&2
  exit 1
fi
exec 8>/run/lock/monday-bybit-options-shadow-gate.lock
if ! flock -n 8; then
  printf 'a Bybit Options shadow gate is still running\n' >&2
  exit 1
fi
if [[ ! -d /data || -L /data ]] || ! mountpoint -q /data; then
  printf '/data must be a mounted filesystem\n' >&2
  exit 1
fi

path_is_direct_or_absent() {
  local path=$1 resolved
  [[ -e $path || -L $path ]] || return 0
  [[ -d $path && ! -L $path ]] || return 1
  resolved=$(readlink -f -- "$path") || return 1
  [[ $resolved == "$path" ]]
}

protected_root_directory() {
  [[ -d $1 && ! -L $1 && $(readlink -f "$1") == "$1" && $(stat -c %u "$1") == 0 ]] || return 1
  (( (8#$(stat -c %a "$1") & 022) == 0 ))
}

for path in /data/monday /data/monday/evidence /data/monday/evidence/bybit-options-cutovers; do
  if ! path_is_direct_or_absent "$path"; then
    printf 'evidence path contains a symlink: %s\n' "$path" >&2
    exit 1
  fi
done
install -d -m 0750 /data/monday/evidence/bybit-options-cutovers
if [[ $OPERATION == resume-rollback ]]; then
  for path in "${FAILED_RECEIPT%/*}" "${FAILED_RECEIPT%/*}/rollback-recoveries" "$RECOVERY_REQUEST_ROOT" "$RECOVERY_REQUEST_ROOT/runs"; do
    path_is_direct_or_absent "$path" || { printf 'indirect recovery evidence path: %s\n' "$path" >&2; exit 1; }
    if [[ -e $path ]]; then protected_root_directory "$path" || exit 1; fi
  done
  [[ ! -e $RECOVERY_REQUEST_ROOT/completed.json && ! -L $RECOVERY_REQUEST_ROOT/completed.json ]] \
    || { printf 'rollback recovery already completed; inspect its immutable receipt\n' >&2; exit 1; }
  install -d -m 0750 "$RECOVERY_REQUEST_ROOT/runs"
fi
mkdir -m 0750 -- "$EVIDENCE_DIR" \
  || { printf 'refusing to reuse cutover evidence directory: %s\n' "$EVIDENCE_DIR" >&2; exit 1; }

STEP=preflight
RESULT=preflight
FAILURE_REASON=
ROLLBACK_RESULT=not-needed
ROLLBACK_DEPLOYMENT_MANIFEST_SHA256=
OLD_SHA256=
OLD_BINARY=
OLD_DEPLOYMENT=
OLD_MODE=new-host
TRANSITION_STARTED=0
SUCCESS=0
CANDIDATE_STARTED_MS=0
UPLOAD_FAILURE_BASELINE=
ROLLBACK_MAIN_PID=
ROLLBACK_INVOCATION_ID=
ROLLBACK_UPLOAD_WARNING=null
ROLLBACK_WARNING_INTERPRETED=false
BOOTSTRAP_STARTED_MS=0
BOOTSTRAP_PREVIOUS_SUCCESS_MS=0
BOOTSTRAP_UPLOAD_VERIFIED=false

fail() {
  FAILURE_REASON=$*
  printf '%s\n' "$FAILURE_REASON" >&2
  exit 1
}

secure_regular_file() {
  local path=$1 mode owner
  [[ -f $path && ! -L $path ]] || fail "required regular file is missing or a symlink: $path"
  owner=$(stat -c %u -- "$path")
  mode=$(stat -c %a -- "$path")
  [[ $owner == 0 ]] || fail "required file is not root-owned: $path"
  (( (8#$mode & 022) == 0 )) || fail "required file is group/world writable: $path"
}

env_value_from_unit() {
  local file=$1
  local key=$2
  local prefix="Environment=$key="
  local line value count=0
  while IFS= read -r line; do
    if [[ $line == "$prefix"* ]]; then
      value=${line#"$prefix"}
      count=$((count + 1))
    fi
  done < "$file"
  (( count == 1 )) || return 1
  printf '%s\n' "$value"
}

require_env_value() {
  local file=$1 key=$2 expected=$3 actual
  if ! actual=$(env_value_from_unit "$file" "$key"); then
    fail "$file must contain exactly one $key environment"
  fi
  [[ $actual == "$expected" ]] || fail "$file has unsafe $key=$actual (expected $expected)"
}

validate_deployment() {
  local directory=$1 strict=${2:-false} asset
  [[ -d $directory && ! -L $directory ]] || fail "staged deployment is missing: $directory"
  for asset in "${DEPLOYMENT_ASSETS[@]}"; do
    secure_regular_file "$directory/$asset"
  done

  require_env_value "$directory/bybit-options-archiver.service" BYBIT_OPTIONS_SPOOL_DIR "$CANONICAL_SPOOL"
  require_env_value "$directory/bybit-options-archiver.service" BYBIT_OPTIONS_SEGMENT_SECONDS 3600
  require_env_value "$directory/bybit-options-archiver.service" BYBIT_OPTIONS_MAX_SEGMENT_BYTES 4294967296
  require_env_value "$directory/bybit-options-archiver.service" BYBIT_OPTIONS_LOCAL_ZST_RETENTION_SECONDS 172800
  require_env_value "$directory/bybit-options-archiver.service" MIN_FREE_GB 20.0
  require_env_value "$directory/bybit-options-archiver.service" BYBIT_OPTIONS_SPOOL_MAX_BYTES 53687091200
  require_env_value "$directory/bybit-options-archiver.service" OSS_BUCKET monday-lob-apne1-1045353359
  require_env_value "$directory/bybit-options-archiver.service" OSS_ENDPOINT oss-ap-northeast-1-internal.aliyuncs.com
  require_env_value "$directory/bybit-options-archiver.service" OSS_REGION ap-northeast-1
  require_env_value "$directory/bybit-options-archiver.service" ALIYUN_PROFILE ecs-role

  if [[ $strict == true ]]; then
    grep -Fxq 'AssertPathIsMountPoint=/data' \
      "$directory/bybit-options-archiver.service" \
      || fail 'candidate collector unit does not assert the /data mount'
    grep -Fxq 'RuntimeMaxSec=21600' \
      "$directory/bybit-options-archiver.service" \
      || fail 'candidate collector unit lacks RuntimeMaxSec'
    grep -Fq 'ExecStart=/opt/monday/releases/bybit-options-archiver/' \
      "$directory/bybit-options-archiver.service" \
      || fail 'candidate collector unit has the wrong executable'
    grep -Fxq 'AssertPathIsMountPoint=/data' \
      "$directory/bybit-options-upload.service" \
      || fail 'candidate upload unit does not assert the /data mount'
    grep -Fq -- '--upload-only' \
      "$directory/bybit-options-upload.service" \
      || fail 'candidate upload unit is not explicitly upload-only'
    grep -Fxq 'Unit=bybit-options-upload.service' \
      "$directory/bybit-options-upload.timer" \
      || fail 'candidate upload timer targets the wrong unit'
  fi
}

atomic_install() {
  local mode=$1 source=$2 destination=$3 temporary
  temporary="${destination}.new.$$"
  [[ ! -e $temporary && ! -L $temporary ]] || return 1
  install -m "$mode" "$source" "$temporary" || return 1
  mv -Tf "$temporary" "$destination" || return 1
}

render_unit() {
  local template=$1 destination=$2 binary=$3 temporary="$2.new.$$"
  [[ ! -e $temporary && ! -L $temporary ]] || return 1
  sed "s|/opt/monday/releases/bybit-options-archiver/@BYBIT_OPTIONS_ARCHIVER_SHA256@/bybit-options-archiver|$binary|g" \
    "$template" >"$temporary" || return 1
  chown root:root "$temporary" || return 1
  chmod 0444 "$temporary" || return 1
  mv -Tf -- "$temporary" "$destination"
}

atomic_symlink() {
  local target=$1 link=$2 temporary
  temporary="${link}.symlink.$$"
  [[ ! -e $temporary && ! -L $temporary ]] || return 1
  ln -s "$target" "$temporary" || return 1
  mv -Tf "$temporary" "$link" || return 1
}

install_deployment() {
  local directory=$1 binary=$2
  install -d -m 0755 "$SYSTEMD_DIR" || return 1
  render_unit "$directory/bybit-options-archiver.service" "$SYSTEMD_DIR/$UNIT" "$binary" || return 1
  render_unit "$directory/bybit-options-upload.service" "$SYSTEMD_DIR/$UPLOAD_UNIT" "$binary" || return 1
  atomic_install 0644 "$directory/$TIMER" "$SYSTEMD_DIR/$TIMER" || return 1
}

rendered_unit_sha() {
  local directory=$1 binary=$2 unit=$3
  if [[ $unit == "$TIMER" ]]; then sha256sum "$directory/$unit" | awk '{print $1}'; return; fi
  sed "s|/opt/monday/releases/bybit-options-archiver/@BYBIT_OPTIONS_ARCHIVER_SHA256@/bybit-options-archiver|$binary|g" \
    "$directory/$unit" | sha256sum | awk '{print $1}'
}

mask_transition_units() {
  local unit path actual allowed backup
  # Admit all fragments before replacing any. Preserve exact bytes, then put
  # the mask at /etc precedence; /run masks do not mask these local fragments.
  for unit in "${TRANSITION_MASK_UNITS[@]}"; do
    path="$SYSTEMD_DIR/$unit"
    if [[ -L $path ]]; then
      [[ $(readlink "$path") == /dev/null && $MASK_OWNED_UNITS == *" $unit "* ]] || return 1
    elif [[ -e $path ]]; then
      [[ -f $path && ! -L $path && $(stat -c %u "$path") == 0 ]] || return 1
      (( (8#$(stat -c %a "$path") & 022) == 0 )) || return 1
      actual=$(sha256sum "$path" | awk '{print $1}') || return 1
      allowed=$(rendered_unit_sha "$CANDIDATE_DEPLOYMENT" "$CANDIDATE_BINARY" "$unit") || return 1
      if [[ $actual != "$allowed" ]]; then
        [[ $OLD_MODE == upgrade && -n $OLD_DEPLOYMENT ]] || return 1
        allowed=$(rendered_unit_sha "$OLD_DEPLOYMENT" "$OLD_BINARY" "$unit") || return 1
        [[ $actual == "$allowed" ]] || return 1
      fi
    elif [[ $OLD_MODE == upgrade && $MASK_OWNED_UNITS != *" $unit "* ]]; then
      return 1
    fi
  done
  MASK_SEQUENCE=$((MASK_SEQUENCE + 1))
  backup="$EVIDENCE_DIR/mask-$MASK_SEQUENCE"
  mkdir -m 0750 "$backup" || return 1
  for unit in "${TRANSITION_MASK_UNITS[@]}"; do
    path="$SYSTEMD_DIR/$unit"
    if [[ -f $path && ! -L $path ]]; then
      install -m 0440 "$path" "$backup/$unit" || return 1
      sha256sum "$backup/$unit" >>"$backup/files.sha256" || return 1
      bybit_path_sync "$backup/$unit" "$backup" || return 1
    fi
    atomic_symlink /dev/null "$path" || return 1
    MASK_OWNED_UNITS+="$unit "
  done
  systemctl daemon-reload || return 1
  production_is_fail_closed
}

canonical_spool_paths_safe() {
  local path
  for path in \
    /data/monday \
    /data/monday/spool \
    "$CANONICAL_SPOOL"; do
    path_is_direct_or_absent "$path" || return 1
  done
}

segment_artifacts() {
  canonical_spool_paths_safe || return 1
  [[ -d $CANONICAL_SPOOL ]] || return 0
  find "$CANONICAL_SPOOL" \( -type f -o -type l \) \( \
    -name '*.ndjson' -o \
    -name '*.ndjson.active' -o \
    -name '*.zst.tmp' -o \
    -name '*.uploaded.json.tmp' \
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

capture_upload_failure_baseline() {
  local status="$CANONICAL_SPOOL/upload-status.json"
  case "$OLD_MODE" in
    new-host)
      require_empty_segment_spool || return 1
      if [[ ! -e $status && ! -L $status ]]; then
        printf '0\n'
        return
      fi
      ;;
    upgrade) ;;
    *) return 1 ;;
  esac
  bybit_options_upload_failure_count "$status"
}

run_candidate_drain() {
  local unit_template="$1/bybit-options-archiver.service"
  local key value drain_seconds=900 remaining
  local -a env_args
  canonical_spool_paths_safe || return 1
  env_args=()
  for key in "${DRAIN_ENV_KEYS[@]}"; do
    value=$(env_value_from_unit "$unit_template" "$key") || return 1
    [[ -n $value ]] || return 1
    env_args+=("$key=$value")
  done
  if (( BOOTSTRAP_DEADLINE > 0 )); then
    remaining=$(bootstrap_remaining_seconds) || return 1
    (( remaining >= drain_seconds )) || drain_seconds=$remaining
  fi
  timeout --signal=TERM --kill-after=10s "$drain_seconds" \
    runuser --user hftcollector -- env -i \
    HOME=/var/lib/hft-collector \
    PATH="$SAFE_PATH" \
    RUST_LOG=info \
    "${env_args[@]}" \
    "$CANDIDATE_BINARY" --upload-only || return 1
  bybit_options_upload_status_ready "$CANONICAL_SPOOL/upload-status.json" "$UPLOAD_FAILURE_BASELINE" \
    || return 1
  require_empty_segment_spool || return 1
}

stage_existing_deployment_for_rollback() {
  local asset source mode
  local snapshot="$EVIDENCE_DIR/rollback-deployment"
  local manifest="$EVIDENCE_DIR/rollback-deployment.sha256"
  [[ ! -e $snapshot && ! -L $snapshot ]] \
    || fail "rollback evidence snapshot already exists: $snapshot"
  install -d -m 0750 "$snapshot"
  for asset in "${DEPLOYMENT_ASSETS[@]}"; do
    case "$asset" in
      *.service | *.timer) source="/etc/systemd/system/$asset"; mode=0644 ;;
      *.jq | *.sh) source="$OLD_DEPLOYMENT/$asset"; mode=0444 ;;
    esac
    secure_regular_file "$source"
    atomic_install "$mode" "$source" "$snapshot/$asset"
  done
  validate_deployment "$snapshot" false
  (
    cd "$snapshot"
    sha256sum "${DEPLOYMENT_ASSETS[@]}"
  ) >"$manifest"
  chmod 0640 "$manifest"
  ROLLBACK_DEPLOYMENT_MANIFEST_SHA256=$(sha256sum "$manifest" | awk '{print $1}')
  OLD_DEPLOYMENT=$snapshot
}

unit_active_json() {
  if systemctl is-active --quiet "$UNIT"; then
    printf true
  else
    printf false
  fi
}

copy_health_evidence() {
  local label=$1 source="$CANONICAL_SPOOL/health.json"
  if [[ -f $source && ! -L $source ]]; then
    install -m 0640 "$source" "$EVIDENCE_DIR/$label-health.json"
  fi
  source="$CANONICAL_SPOOL/upload-status.json"
  if [[ -f $source && ! -L $source ]]; then
    install -m 0640 "$source" "$EVIDENCE_DIR/$label-upload-status.json"
  fi
}

health_ready_for_release() {
  local minimum_symbols=$1 minimum_updated_ms=$2 old_updated_ms=${3:-0}
  local health="$CANONICAL_SPOOL/health.json"
  [[ -f $health && ! -L $health ]] || return 1
  bybit_options_upload_status_ready "$CANONICAL_SPOOL/upload-status.json" "$UPLOAD_FAILURE_BASELINE" \
    || return 1
  jq -e \
    --argjson minimum_symbols "$minimum_symbols" \
    --argjson minimum_updated_ms "$minimum_updated_ms" \
    --argjson old_updated_ms "$old_updated_ms" \
    --argjson upload_failure_baseline "$UPLOAD_FAILURE_BASELINE" \
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
    main_exe=$(readlink -f "/proc/$main_pid/exe" 2>/dev/null || true)
    [[ $main_exe == "$binary" ]] || return 1
    if [[ $require_enabled == true ]]; then
      systemctl is-enabled --quiet "$unit" || return 1
    fi
  done
}

wait_for_release_health() {
  local binary=$1 minimum_updated_ms=${2:-0}
  local deadline=$((SECONDS + HEALTH_TIMEOUT_SECONDS))
  if (( BOOTSTRAP_DEADLINE > 0 && BOOTSTRAP_DEADLINE < deadline )); then
    deadline=$BOOTSTRAP_DEADLINE
  fi
  while (( SECONDS < deadline )); do
    systemctl is-active --quiet "$UNIT" || return 1
    if health_ready_for_release "$MINIMUM_SYMBOLS" "$minimum_updated_ms" \
      && runtime_matches_release "$binary" false; then
      return 0
    fi
    sleep 5
  done
  return 1
}

# Only the captured previous release can use the historical-warning
# interpretation. Its process invocation must remain stable throughout readback.
capture_rollback_runtime_identity() {
  [[ $OLD_MODE == upgrade && -n $OLD_SHA256 ]] || return 1
  [[ $OLD_BINARY == "$RELEASE_ROOT/$OLD_SHA256/bybit-options-archiver" ]] || return 1
  [[ $(readlink -f "$PRODUCTION_LINK") == "$OLD_BINARY" ]] || return 1
  printf '%s  %s\n' "$OLD_SHA256" "$OLD_BINARY" | sha256sum --check --strict >/dev/null || return 1
  runtime_matches_release "$OLD_BINARY" false || return 1
  ROLLBACK_MAIN_PID=$(systemctl show "$UNIT" --property=MainPID --value) || return 1
  ROLLBACK_INVOCATION_ID=$(systemctl show "$UNIT" --property=InvocationID --value) || return 1
  [[ $ROLLBACK_MAIN_PID =~ ^[1-9][0-9]*$ && $ROLLBACK_INVOCATION_ID =~ ^[a-f0-9]{32}$ ]]
}

rollback_runtime_identity_matches() {
  [[ $OLD_MODE == upgrade && -n $ROLLBACK_MAIN_PID && -n $ROLLBACK_INVOCATION_ID ]] || return 1
  [[ $OLD_BINARY == "$RELEASE_ROOT/$OLD_SHA256/bybit-options-archiver" ]] || return 1
  [[ $(readlink -f "$PRODUCTION_LINK") == "$OLD_BINARY" ]] || return 1
  printf '%s  %s\n' "$OLD_SHA256" "$OLD_BINARY" | sha256sum --check --strict >/dev/null || return 1
  runtime_matches_release "$OLD_BINARY" false || return 1
  [[ $(systemctl show "$UNIT" --property=MainPID --value) == "$ROLLBACK_MAIN_PID" \
    && $(systemctl show "$UNIT" --property=InvocationID --value) == "$ROLLBACK_INVOCATION_ID" ]]
}

health_ready_for_rollback() {
  local minimum_updated_ms=$1 health="$CANONICAL_SPOOL/health.json" snapshot original_warning
  rollback_runtime_identity_matches || return 1
  bybit_options_upload_status_ready "$CANONICAL_SPOOL/upload-status.json" "$UPLOAD_FAILURE_BASELINE" || return 1
  [[ -f $health && ! -L $health ]] || return 1
  snapshot=$(<"$health") || return 1
  # The old schema derived this bit from the lifetime counter. Interpret only
  # that bit, after checking the underlying active-error fields and count; all
  # ordinary freshness, disk, catalog and worker gates still apply unchanged.
  printf '%s\n' "$snapshot" | jq --argjson baseline "$UPLOAD_FAILURE_BASELINE" '
    if .upload_warning == true and $baseline > 0
      and .upload_failure_count == $baseline and .last_upload_error_at == null
    then .upload_warning = false else . end' \
    | jq -e --argjson minimum_symbols "$MINIMUM_SYMBOLS" \
      --argjson minimum_updated_ms "$minimum_updated_ms" \
      --argjson old_updated_ms "$minimum_updated_ms" \
      --argjson upload_failure_baseline "$UPLOAD_FAILURE_BASELINE" \
      -f "$RUNTIME_HEALTH_POLICY" >/dev/null || return 1
  rollback_runtime_identity_matches || return 1
  bybit_options_upload_status_ready "$CANONICAL_SPOOL/upload-status.json" "$UPLOAD_FAILURE_BASELINE" || return 1
  original_warning=$(jq -c '.upload_warning' <<<"$snapshot") || return 1
  ROLLBACK_UPLOAD_WARNING=$original_warning
  ROLLBACK_WARNING_INTERPRETED=$original_warning
  printf '%s\n' "$snapshot" >"$EVIDENCE_DIR/rollback-verified-health.json"
}

wait_for_rollback_health() {
  local minimum_updated_ms=$1 deadline=$((SECONDS + HEALTH_TIMEOUT_SECONDS))
  while (( SECONDS < deadline )); do
    rollback_runtime_identity_matches || return 1
    if health_ready_for_rollback "$minimum_updated_ms"; then
      return 0
    fi
    sleep 5
  done
  return 1
}

bootstrap_remaining_seconds() {
  local remaining=$((BOOTSTRAP_DEADLINE - SECONDS))
  (( BOOTSTRAP_DEADLINE > 0 && remaining > 0 )) || return 1
  printf '%s\n' "$remaining"
}

capture_bootstrap_segments() {
  local data name manifest digest bytes count=0
  install -d -m 0750 "$EVIDENCE_DIR/bootstrap"
  : >"$EVIDENCE_DIR/bootstrap/segments.ndjson"
  for data in "$CANONICAL_SPOOL"/*.ndjson; do
    [[ -e $data || -L $data ]] || continue
    [[ -f $data && ! -L $data ]] || return 1
    name=${data##*/}
    [[ $name =~ ^bybit-options\.[0-9]+\.ndjson$ ]] || return 1
    manifest="$data.manifest.json"
    [[ -f $manifest && ! -L $manifest && -f $data._SUCCESS && ! -L $data._SUCCESS ]] || return 1
    digest=$(sha256sum "$data" | awk '{print $1}') || return 1
    bytes=$(wc -c <"$data") || return 1
    jq -ec --arg file "$name" --arg digest "$digest" --argjson bytes "$bytes" \
      --argjson started "$BOOTSTRAP_STARTED_MS" '
      select(.schema == "monday.bybit_options_quote.v1" and .file == $file
        and .sha256 == $digest and .bytes == $bytes and $bytes > 0
        and .start_received_at_ms >= $started and .end_received_at_ms >= $started
        and .end_received_at_ms >= .start_received_at_ms and .events > 1
        and ((.event_types.orderbook // 0) + (.event_types.ticker // 0)) > 0)
      | {file,sha256,bytes,start_received_at_ms,end_received_at_ms,events,source_revision}' \
      "$manifest" >>"$EVIDENCE_DIR/bootstrap/segments.ndjson" || return 1
    install -m 0640 "$manifest" "$EVIDENCE_DIR/bootstrap/$name.manifest.json" || return 1
    count=$((count + 1))
  done
  (( count > 0 ))
}

verify_bootstrap_upload() {
  local require_drained=${1:-true} entries name source_sha bytes marker compressed_sha now_ms
  bootstrap_remaining_seconds >/dev/null || return 1
  bybit_options_upload_status_ready "$CANONICAL_SPOOL/upload-status.json" "$UPLOAD_FAILURE_BASELINE" || return 1
  if [[ $require_drained == true ]]; then
    require_empty_segment_spool || return 1
  fi
  now_ms=$(( $(date +%s) * 1000 + 999 ))
  jq -e --argjson started "$BOOTSTRAP_STARTED_MS" \
    --argjson previous "$BOOTSTRAP_PREVIOUS_SUCCESS_MS" --argjson now "$now_ms" '
    (.last_success_at | type) == "number" and .last_success_at == (.last_success_at | floor)
    and .last_success_at >= $started and .last_success_at > $previous
    and .last_success_at <= $now' "$CANONICAL_SPOOL/upload-status.json" >/dev/null || return 1
  entries=$(jq -ser 'if length > 0 then .[] | [.file,.sha256,.bytes] | @tsv
    else error("no bootstrap segments") end' "$EVIDENCE_DIR/bootstrap/segments.ndjson") || return 1
  while IFS=$'\t' read -r name source_sha bytes; do
    [[ $name =~ ^bybit-options\.[0-9]+\.ndjson$ && $source_sha =~ ^[a-f0-9]{64}$ ]] || return 1
    marker="$CANONICAL_SPOOL/$name.uploaded.json"
    [[ ! -e $CANONICAL_SPOOL/$name && ! -L $CANONICAL_SPOOL/$name ]] || return 1
    [[ -f $marker && ! -L $marker \
      && -f $CANONICAL_SPOOL/$name.zst && ! -L $CANONICAL_SPOOL/$name.zst ]] || return 1
    compressed_sha=$(sha256sum "$CANONICAL_SPOOL/$name.zst" | awk '{print $1}') || return 1
    jq -e --arg source "$source_sha" --arg compressed "$compressed_sha" --arg file "$name" \
      --argjson started "$BOOTSTRAP_STARTED_MS" --argjson now "$now_ms" '
      .schema == "monday.bybit_options_upload.v1" and .source_sha256 == $source
      and .compressed_sha256 == $compressed and .uploaded_at_ms >= $started
      and .uploaded_at_ms <= $now
      and (.object | startswith("oss://monday-lob-apne1-1045353359/lake/raw/venue=bybit/market=option/dataset=options_quotes/"))
      and (.object | endswith("/sha256=" + $compressed + "/" + $file + ".zst"))' \
      "$marker" >/dev/null || return 1
    install -m 0640 "$marker" "$EVIDENCE_DIR/bootstrap/$name.uploaded.json" || return 1
  done <<<"$entries"
  install -m 0640 "$CANONICAL_SPOOL/upload-status.json" "$EVIDENCE_DIR/bootstrap/upload-status.json" || return 1
  BOOTSTRAP_UPLOAD_VERIFIED=true
}

complete_new_host_bootstrap() {
  local remaining
  [[ $OLD_MODE == new-host ]] || return 1
  bootstrap_remaining_seconds >/dev/null || return 1
  systemctl is-active --quiet "$TIMER" && return 1
  systemctl is-active --quiet "$UPLOAD_UNIT" && return 1
  runtime_matches_release "$CANDIDATE_BINARY" false || return 1
  copy_health_evidence bootstrap-collection || return 1
  remaining=$(bootstrap_remaining_seconds) || return 1
  timeout --signal=TERM --kill-after=10s "$remaining" systemctl stop "$UNIT" || return 1
  systemctl is-active --quiet "$UNIT" && return 1
  capture_bootstrap_segments || return 1
  run_candidate_drain "$CANDIDATE_DEPLOYMENT" || return 1
  verify_bootstrap_upload || return 1
  clear_health_before_restart || return 1
  CANDIDATE_STARTED_MS=$(( $(date +%s) * 1000 ))
  remaining=$(bootstrap_remaining_seconds) || return 1
  timeout --signal=TERM --kill-after=10s "$remaining" systemctl start "$UNIT" || return 1
  wait_for_release_health "$CANDIDATE_BINARY" "$CANDIDATE_STARTED_MS" || return 1
  verify_bootstrap_upload false || return 1
}

clear_health_before_restart() {
  canonical_spool_paths_safe || return 1
  rm -f -- "$CANONICAL_SPOOL/health.json" || return 1
  [[ ! -e "$CANONICAL_SPOOL/health.json" && ! -L "$CANONICAL_SPOOL/health.json" ]] || return 1
}

production_is_fail_closed() {
  local unit state
  for unit in "${TRANSITION_MASK_UNITS[@]}"; do
    systemctl is-active --quiet "$unit" && return 1
    [[ $(systemctl show "$unit" --property=LoadState --value) == masked ]] || return 1
    state=$(systemctl is-enabled "$unit" 2>/dev/null || true)
    [[ $state == masked || $state == masked-runtime ]] || return 1
  done
}

bybit_path_sync() {
  local program version
  program=$(command -v gnusync || command -v sync) || return 1
  version=$("$program" --version) || return 1
  [[ $version == 'sync (GNU coreutils)'* ]] || return 1
  "$program" -- "$@"
}

bybit_immutable_json() {
  local path=$1 json=$2 temporary="$1.tmp.$$"
  if [[ -e $path || -L $path ]]; then
    [[ -f $path && ! -L $path && $(stat -c %u "$path") == 0 \
      && $(stat -c %h "$path") == 1 && $(stat -c %a "$path") == 440 \
      && $(<"$path") == "$json" ]] || return 1
    return
  fi
  protected_root_directory "${path%/*}" || return 1
  (set -o noclobber; printf '%s\n' "$json" >"$temporary") || return 1
  chmod 0440 "$temporary" || return 1
  bybit_path_sync "$temporary" || return 1
  ln -- "$temporary" "$path" || return 1
  rm -- "$temporary" || return 1
  bybit_path_sync "${path%/*}"
}

bybit_file_fingerprint() {
  local path=$1 values dev inode links bytes uid gid mode ms cs mt ct mf cf
  [[ -f $path && ! -L $path ]] || return 1
  values=$(TZ=UTC stat -c '%d|%i|%h|%s|%u|%g|%a|%Y|%Z|%y|%z' "$path") || return 1
  IFS='|' read -r dev inode links bytes uid gid mode ms cs mt ct <<<"$values"
  [[ $mt =~ \.([0-9]{9})\ \+0000$ ]] || return 1; mf=${BASH_REMATCH[1]}
  [[ $ct =~ \.([0-9]{9})\ \+0000$ ]] || return 1; cf=${BASH_REMATCH[1]}
  [[ $links == 1 && $mode =~ ^[0-7]{3,4}$ && $ms =~ ^[0-9]{10}$ && $cs =~ ^[0-9]{10}$ ]] || return 1
  (( inode <= 9007199254740991 && dev <= 9007199254740991 && (8#$mode & 022) == 0 )) || return 1
  jq -cnS --arg name "${path##*/}" --argjson bytes "$bytes" --argjson device "$dev" --argjson inode "$inode" \
    --argjson links "$links" --argjson uid "$uid" --argjson gid "$gid" --arg mode "$mode" \
    --arg mtime_ns "$ms$mf" --arg ctime_ns "$cs$cf" \
    '{name:$name,bytes:$bytes,device:$device,inode:$inode,links:$links,uid:$uid,gid:$gid,
      mode:$mode,mtime_ns:$mtime_ns,ctime_ns:$ctime_ns}'
}

# Return 0 only for an actual writable open descriptor of this filesystem
# object. PID disappearance is rechecked by callers; no pathname guess suffices.
bybit_pid_writes_file() {
  local path=$1 pid=$2 fd flags identity observed
  identity=$(stat -c '%d:%i' "$path") || return 2
  [[ -d $PROC_ROOT/$pid/fd && -r $PROC_ROOT/$pid/fd ]] || return 2
  for fd in "$PROC_ROOT/$pid/fd"/*; do
    [[ -e $fd || -L $fd ]] || continue
    if ! observed=$(stat -Lc '%d:%i' "$fd" 2>/dev/null); then
      if [[ -d $PROC_ROOT/$pid && ( -e $fd || -L $fd ) ]]; then return 2; fi
      continue
    fi
    [[ $observed == "$identity" ]] || continue
    flags=$(awk '$1=="flags:" {print $2}' "$PROC_ROOT/$pid/fdinfo/${fd##*/}" 2>/dev/null) || return 2
    [[ $flags =~ ^[0-7]+$ ]] || return 2
    (( (8#$flags & 3) == 0 )) || return 0
  done
  return 1
}

bybit_no_writer() {
  local path directory pid fd observed flags identities=$'\n' inspected=0
  (( $# > 0 )) || return 1
  [[ -d $PROC_ROOT/self/fd && -r $PROC_ROOT/self/fd && -x $PROC_ROOT/self/fd ]] || return 1
  for path in "$@"; do
    observed=$(stat -c '%d:%i' "$path") || return 1
    identities+="$observed"$'\n'
  done
  for directory in "$PROC_ROOT"/[0-9]*/fd; do
    [[ -d $directory ]] || continue
    [[ -r $directory && -x $directory ]] || return 1
    pid=${directory%/fd}; pid=${pid##*/}
    inspected=$((inspected + 1))
    for fd in "$directory"/*; do
      [[ -e $fd || -L $fd ]] || continue
      if ! observed=$(stat -Lc '%d:%i' "$fd" 2>/dev/null); then
        if [[ -d $PROC_ROOT/$pid && ( -e $fd || -L $fd ) ]]; then return 1; fi
        continue
      fi
      [[ $identities == *$'\n'"$observed"$'\n'* ]] || continue
      flags=$(awk '$1=="flags:" {print $2}' "$PROC_ROOT/$pid/fdinfo/${fd##*/}" 2>/dev/null) || return 1
      [[ $flags =~ ^[0-7]+$ ]] || return 1
      (( (8#$flags & 3) == 0 )) || return 1
    done
  done
  (( inspected > 0 ))
}

preflight_active_segments() {
  local pid invocation path before after status
  pid=$(systemctl show "$UNIT" --property=MainPID --value) || return 1
  invocation=$(systemctl show "$UNIT" --property=InvocationID --value) || return 1
  [[ $pid =~ ^[1-9][0-9]*$ && $invocation =~ ^[a-f0-9]{32}$ \
    && $(readlink -f "$PROC_ROOT/$pid/exe") == "$OLD_BINARY" ]] || return 1
  for path in "$CANONICAL_SPOOL"/*.ndjson.active; do
    [[ -e $path || -L $path ]] || continue
    before=$(bybit_file_fingerprint "$path") || return 1
    if bybit_pid_writes_file "$path" "$pid"; then
      after=$(bybit_file_fingerprint "$path") || return 1
      [[ $(jq -c '[.device,.inode,.links,.uid,.gid,.mode]' <<<"$before") == \
        "$(jq -c '[.device,.inode,.links,.uid,.gid,.mode]' <<<"$after")" ]] || return 1
    else
      status=$?
      printf 'pre-stop refusal: orphan or uninspectable active segment %s (fd_status=%s)\n' "$path" "$status" >&2
      return 1
    fi
  done
  [[ $(systemctl show "$UNIT" --property=MainPID --value) == "$pid" \
    && $(systemctl show "$UNIT" --property=InvocationID --value) == "$invocation" ]]
}

resume_units_inactive() {
  local unit state
  for unit in "${TRANSITION_MASK_UNITS[@]}"; do
    state=$(systemctl show "$unit" --property=ActiveState --value) || return 1
    [[ $state == inactive ]] || return 1
    state=$(systemctl show "$unit" --property=MainPID --value) || return 1
    [[ -z $state || $state == 0 ]] || return 1
    systemctl is-enabled --quiet "$unit" && return 1
  done
  return 0
}

resume_failed_state_matches() {
  local unit entry path actual drop_paths masked disk_drop_paths expected_drop_paths
  resume_units_inactive || return 1
  [[ $(readlink -f "$PRODUCTION_LINK") == "$OLD_BINARY" \
    && $(jq -er .failed_state.production_link "$ORPHAN_INVENTORY") == "$OLD_BINARY" ]] || return 1
  [[ $(sha256sum "$CANONICAL_SPOOL/upload-status.json" | awk '{print $1}') == \
    "$(jq -er .failed_state.upload_status_sha256 "$ORPHAN_INVENTORY")" ]] || return 1
  bybit_options_upload_status_ready "$CANONICAL_SPOOL/upload-status.json" "$UPLOAD_FAILURE_BASELINE" || return 1
  for unit in "${TRANSITION_MASK_UNITS[@]}"; do
    masked=false
    path="$SYSTEMD_DIR/$unit"
    entry=$(jq -ce --arg unit "$unit" '.failed_state.unit_fragments[$unit]' "$ORPHAN_INVENTORY") || return 1
    [[ $(jq -er .path <<<"$entry") == "$path" ]] || return 1
    if [[ -L $path && $(readlink "$path") == /dev/null && -f $RECOVERY_REQUEST_ROOT/masks-authorized.json ]]; then
      MASK_OWNED_UNITS+="$unit "
      masked=true
    else
      [[ -f $path && ! -L $path ]] || return 1
      actual=$(sha256sum "$path" | awk '{print $1}') || return 1
      [[ $actual == "$(jq -er .sha256 <<<"$entry")" \
        && $actual == "$(rendered_unit_sha "$CANDIDATE_DEPLOYMENT" "$CANDIDATE_BINARY" "$unit")" \
        && $(stat -c %u "$path") == "$(jq -er .uid <<<"$entry")" \
        && $(stat -c %g "$path") == "$(jq -er .gid <<<"$entry")" \
        && $(stat -c %a "$path") == "$(jq -er .mode <<<"$entry")" ]] || return 1
    fi
    drop_paths=$(systemctl show "$unit" --property=DropInPaths --value) || return 1
    expected_drop_paths=$(jq -er --arg unit "$unit" '.failed_state.drop_in_paths[$unit]|select(type=="string")' "$ORPHAN_INVENTORY") || return 1
    if [[ $masked == false ]]; then [[ $drop_paths == "$expected_drop_paths" ]] || return 1
    else
      [[ -z $drop_paths || $drop_paths == "$expected_drop_paths" ]] || return 1
      disk_drop_paths=''
      if [[ -e $SYSTEMD_DIR/$unit.d || -L $SYSTEMD_DIR/$unit.d ]]; then
        path_is_direct_or_absent "$SYSTEMD_DIR/$unit.d" || return 1
        disk_drop_paths=$(find "$SYSTEMD_DIR/$unit.d" -maxdepth 1 -name '*.conf' -print | sort | paste -sd ' ' -) || return 1
      fi
      [[ $disk_drop_paths == "$expected_drop_paths" ]] || return 1
    fi
  done
  while IFS= read -r entry; do
    path=$(jq -er .path <<<"$entry") || return 1
    [[ $path == "$SYSTEMD_DIR/$UNIT.d/"*.conf && -f $path && ! -L $path ]] || return 1
    [[ $(sha256sum "$path" | awk '{print $1}') == "$(jq -er .sha256 <<<"$entry")" \
      && $(stat -c %u "$path") == "$(jq -er .uid <<<"$entry")" \
      && $(stat -c %g "$path") == "$(jq -er .gid <<<"$entry")" \
      && $(stat -c %a "$path") == "$(jq -er .mode <<<"$entry")" ]] || return 1
  done < <(jq -c '.failed_state.drop_ins[]' "$ORPHAN_INVENTORY")
  return 0
}

admit_resume_rollback() {
  local original=${FAILED_RECEIPT%/*} fields expected_assets actual_assets
  secure_regular_file "$FAILED_RECEIPT"
  secure_regular_file "$ORPHAN_INVENTORY"
  [[ $(readlink -f "$ORPHAN_INVENTORY") == "$ORPHAN_INVENTORY" \
    && $(sha256sum "$FAILED_RECEIPT" | awk '{print $1}') == "$FAILED_RECEIPT_SHA256" \
    && $(sha256sum "$ORPHAN_INVENTORY" | awk '{print $1}') == "$ORPHAN_INVENTORY_SHA256" ]] || return 1
  OLD_SHA256=$(jq -er '.previous_sha256|select(test("^[a-f0-9]{64}$"))' "$FAILED_RECEIPT") || return 1
  OLD_BINARY="$RELEASE_ROOT/$OLD_SHA256/bybit-options-archiver"
  OLD_DEPLOYMENT="$original/rollback-deployment"
  OLD_MODE=upgrade
  UPLOAD_FAILURE_BASELINE=$(jq -er '.upload_failure_baseline|select(type=="number" and .>=0 and .==floor)' "$FAILED_RECEIPT") || return 1
  jq -e --arg old "$OLD_BINARY" --arg candidate "$CANDIDATE_SHA256" --arg bundle "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg snapshot "$RESUME_SNAPSHOT_SHA256" '
    .schema=="monday.bybit_options_cutover.v1" and .result=="failed" and .host_mode=="upgrade"
    and .last_step=="drain-old-production-with-candidate" and .production_active==false
    and .candidate_sha256==$candidate and .deployment_bundle_sha256==$bundle and .current_binary==$old
    and .rollback_deployment_manifest_sha256==$snapshot and .bootstrap.started_at_ms==0
    and .bootstrap.upload_verified==false and .rollback_health.main_pid=="" and .rollback_health.invocation_id==""' \
    "$FAILED_RECEIPT" >/dev/null || return 1
  secure_regular_file "$original/rollback-deployment.sha256"
  [[ $(sha256sum "$original/rollback-deployment.sha256" | awk '{print $1}') == "$RESUME_SNAPSHOT_SHA256" ]] || return 1
  expected_assets=$(printf '%s\n' "${DEPLOYMENT_ASSETS[@]}" | sort)
  actual_assets=$(awk '$1 !~ /^[a-f0-9]+$/ || length($1)!=64 || NF!=2 {exit 1} {print $2}' "$original/rollback-deployment.sha256" | sort) || return 1
  [[ $actual_assets == "$expected_assets" ]] || return 1
  (cd "$OLD_DEPLOYMENT" && sha256sum --check --strict "$original/rollback-deployment.sha256") || return 1
  validate_deployment "$OLD_DEPLOYMENT" false
  secure_regular_file "$OLD_BINARY"
  [[ $(sha256sum "$OLD_BINARY" | awk '{print $1}') == "$OLD_SHA256" ]] || return 1
  jq -e --arg receipt "$FAILED_RECEIPT" --arg sha "$FAILED_RECEIPT_SHA256" --arg spool "$CANONICAL_SPOOL" '
    .schema=="monday.bybit_orphan_inventory.v1" and .failed_receipt==$receipt
    and .failed_receipt_sha256==$sha and .spool==$spool
    and (.files|type)=="array" and (.files|length)>0 and (.files|length)<=32
    and ([.files[].name]|unique|length)==(.files|length)
    and all(.files[]; (.name|test("^bybit-options[.][0-9]+[.]ndjson[.]active$"))
      and (.sha256|test("^[a-f0-9]{64}$")) and .links==1 and .data_source_revision==null
      and (.bytes|type)=="number" and .bytes>=0 and .bytes==(.bytes|floor)
      and (.mtime_ns|test("^[0-9]{19}$")) and (.ctime_ns|test("^[0-9]{19}$")))
    and ([.files[].bytes]|add)<=17179869184
    and (.failed_state.drop_ins|type)=="array" and (.failed_state.drop_in_paths|type)=="object"' \
    "$ORPHAN_INVENTORY" >/dev/null || return 1
  fields=$(jq -cnS --arg failed "$FAILED_RECEIPT" --arg sha "$FAILED_RECEIPT_SHA256" \
    --arg snapshot "$RESUME_SNAPSHOT_SHA256" --arg inventory "$ORPHAN_INVENTORY_SHA256" \
    --arg request "$RECOVERY_REQUEST_ID" --arg destination "$CUSTODY_DIR" \
    '{schema:"monday.bybit_rollback_recovery_intent.v1",failed_receipt:$failed,failed_receipt_sha256:$sha,
      rollback_snapshot_sha256:$snapshot,orphan_inventory_sha256:$inventory,request_id:$request,custody_directory:$destination}') || return 1
  bybit_immutable_json "$RECOVERY_REQUEST_ROOT/intent.json" "$fields" || return 1
  if [[ -e $RECOVERY_REQUEST_ROOT/masks-authorized.json || -L $RECOVERY_REQUEST_ROOT/masks-authorized.json ]]; then
    [[ -f $RECOVERY_REQUEST_ROOT/masks-authorized.json && ! -L $RECOVERY_REQUEST_ROOT/masks-authorized.json \
      && $(<"$RECOVERY_REQUEST_ROOT/masks-authorized.json") == "$fields" ]] || return 1
  fi
  resume_failed_state_matches || return 1
  resume_segment_set_matches || return 1
  install -m 0440 "$original/rollback-deployment.sha256" "$EVIDENCE_DIR/rollback-deployment.sha256" || return 1
  ROLLBACK_DEPLOYMENT_MANIFEST_SHA256=$RESUME_SNAPSHOT_SHA256
}

resume_segment_set_matches() {
  local artifacts path
  artifacts=$(segment_artifacts) || return 1
  while IFS= read -r path; do
    [[ -n $path ]] || continue
    [[ ${path%/*} == "$CANONICAL_SPOOL" && ${path##*/} =~ ^bybit-options\.[0-9]+\.ndjson\.active$ ]] || return 1
  done <<<"$artifacts"
}

orphan_locations_match_inventory() {
  local expected actual path names=''
  expected=$(jq -cS '[.files[].name]|sort' "$ORPHAN_INVENTORY") || return 1
  for path in "$CANONICAL_SPOOL"/*.ndjson.active "$CUSTODY_DIR"/*; do
    [[ -e $path || -L $path ]] || continue
    [[ -f $path && ! -L $path ]] || return 1
    names+="${path##*/}"$'\n'
  done
  actual=$(jq -Rcs 'split("\n")|map(select(length>0))|sort' <<<"$names") || return 1
  [[ $actual == "$expected" ]]
}

custody_orphans() {
  local entry name source destination location before after expected expected_sha actual_sha remaining moved
  local failed_started_ns deadline=$((SECONDS + 360)) move_receipt verified='[]'
  local -a locations=()
  failed_started_ns="$(date -u -d "$(jq -er .started_at "$FAILED_RECEIPT")" +%s)000000000" || return 1
  [[ $failed_started_ns =~ ^[0-9]{19}$ ]] || return 1
  path_is_direct_or_absent "$CUSTODY_DIR" || return 1
  install -d -m 0750 "$CUSTODY_DIR" "$RECOVERY_REQUEST_ROOT/moves" || return 1
  protected_root_directory "$CUSTODY_DIR" && protected_root_directory "$RECOVERY_REQUEST_ROOT/moves" || return 1
  [[ $(stat -c %d "$CANONICAL_SPOOL") == "$(stat -c %d "$CUSTODY_DIR")" ]] || return 1
  orphan_locations_match_inventory || return 1
  resume_segment_set_matches || return 1
  while IFS= read -r name; do
    if [[ -e $CANONICAL_SPOOL/$name ]]; then locations+=("$CANONICAL_SPOOL/$name")
    else locations+=("$CUSTODY_DIR/$name"); fi
  done < <(jq -r '.files[].name' "$ORPHAN_INVENTORY")
  bybit_no_writer "${locations[@]}" || return 1
  while IFS= read -r entry; do
    name=$(jq -er .name <<<"$entry") || return 1
    source="$CANONICAL_SPOOL/$name"; destination="$CUSTODY_DIR/$name"
    expected=$(jq -cS 'del(.sha256,.data_source_revision)' <<<"$entry") || return 1
    expected_sha=$(jq -er .sha256 <<<"$entry") || return 1
    [[ $(jq -r .mtime_ns <<<"$entry") < $failed_started_ns \
      && $(jq -r .ctime_ns <<<"$entry") < $failed_started_ns ]] || return 1
    moved=false
    if [[ -e $source || -L $source ]]; then
      [[ ! -e $destination && ! -L $destination ]] || return 1
      location=$source
      before=$(bybit_file_fingerprint "$location") || return 1
      [[ $before == "$expected" ]] || return 1
    else
      location=$destination; moved=true
      before=$(bybit_file_fingerprint "$location") || return 1
      [[ $(jq -cS 'del(.ctime_ns)' <<<"$before") == "$(jq -cS 'del(.ctime_ns)' <<<"$expected")" ]] || return 1
    fi
    remaining=$((deadline - SECONDS)); (( remaining > 0 )) || return 1
    actual_sha=$(timeout --signal=TERM --kill-after=2s "$remaining" sha256sum "$location" | awk '{print $1}') || return 1
    [[ $actual_sha == "$expected_sha" && $(bybit_file_fingerprint "$location") == "$before" ]] || return 1
    verified=$(jq -cnS --argjson all "$verified" --argjson original "$entry" --argjson before "$before" \
      --arg location "$location" --argjson moved "$moved" '$all+[{original:$original,before:$before,location:$location,moved:$moved}]') || return 1
  done < <(jq -c '.files[]' "$ORPHAN_INVENTORY")
  # One batch scan before hashing and one immediately before the short rename
  # transaction cover the exact inode set without ten full /proc walks.
  bybit_no_writer "${locations[@]}" || return 1
  resume_units_inactive || return 1
  while IFS= read -r entry; do
    before=$(jq -cS .before <<<"$entry"); location=$(jq -r .location <<<"$entry")
    moved=$(jq -r .moved <<<"$entry"); entry=$(jq -cS .original <<<"$entry")
    name=$(jq -r .name <<<"$entry"); source="$CANONICAL_SPOOL/$name"; destination="$CUSTODY_DIR/$name"
    expected=$(jq -cS 'del(.sha256,.data_source_revision)' <<<"$entry")
    [[ $(bybit_file_fingerprint "$location") == "$before" ]] || return 1
    (( SECONDS < deadline )) || return 1
    if [[ $moved == false ]]; then
      [[ ! -e $destination && ! -L $destination ]] || return 1
      mv -T -n -- "$source" "$destination" || return 1
      [[ ! -e $source && ! -L $source ]] || return 1
      bybit_path_sync "$CANONICAL_SPOOL" "$CUSTODY_DIR" || return 1
    fi
    after=$(bybit_file_fingerprint "$destination") || return 1
    [[ $(jq -cS 'del(.ctime_ns)' <<<"$after") == "$(jq -cS 'del(.ctime_ns)' <<<"$expected")" ]] || return 1
    move_receipt=$(jq -cnS --arg inventory "$ORPHAN_INVENTORY_SHA256" --arg source "$source" \
      --arg destination "$destination" --argjson original "$entry" --argjson observed "$after" \
      '{schema:"monday.bybit_orphan_custody_move.v1",inventory_sha256:$inventory,source:$source,
        destination:$destination,original:$original,observed:$observed,data_recovered:false,
        delivery_verified:false,replay_eligibility:"not_assessed"}') || return 1
    bybit_immutable_json "$RECOVERY_REQUEST_ROOT/moves/$name.json" "$move_receipt" || return 1
  done < <(jq -c '.[]' <<<"$verified")
  orphan_locations_match_inventory || return 1
  [[ -z $(find "$CANONICAL_SPOOL" -maxdepth 1 -name '*.ndjson.active' -print -quit) ]] || return 1
  bybit_immutable_json "$RECOVERY_REQUEST_ROOT/custody.json" "$(jq -cnS --arg inventory "$ORPHAN_INVENTORY_SHA256" \
    --arg directory "$CUSTODY_DIR" --arg failed "$FAILED_RECEIPT_SHA256" \
    '{schema:"monday.bybit_orphan_custody.v1",inventory_sha256:$inventory,failed_receipt_sha256:$failed,
      directory:$directory,disposition:"retained_orphan_evidence",data_recovered:false,
      delivery_verified:false,replay_eligibility:"not_assessed"}')"
}

resume_rollback() {
  STEP=admit-resume-rollback
  admit_resume_rollback || fail 'failed receipt, snapshot, inventory or failed host state drifted'
  canonical_spool_paths_safe || fail 'unsafe canonical spool'
  [[ -f $CANONICAL_SPOOL/.bybit-options.lock && ! -L $CANONICAL_SPOOL/.bybit-options.lock ]] \
    || fail 'existing Bybit spool lock required'
  exec 7<"$CANONICAL_SPOOL/.bybit-options.lock"
  flock -n 7 || fail 'Bybit uploader still owns the spool'
  bybit_immutable_json "$RECOVERY_REQUEST_ROOT/masks-authorized.json" "$(<"$RECOVERY_REQUEST_ROOT/intent.json")" \
    || fail 'cannot bind owned recovery masks'
  TRANSITION_STARTED=1
  STEP=contain-resume-rollback
  mask_transition_units || fail 'could not effectively mask all Bybit units'
  STEP=custody-historical-orphans
  custody_orphans || fail 'bounded orphan custody failed; originals or custody copies preserved'
  [[ $(sha256sum "$FAILED_RECEIPT" | awk '{print $1}') == "$FAILED_RECEIPT_SHA256" ]] || fail 'original failed receipt changed'
  # The uploader takes this same lock. Keep release/Gate ownership but release
  # and close the custody FD before real drain or any restored writer starts.
  flock -u 7 || fail 'cannot release custody spool lock'
  exec 7<&-
  STEP=restore-previous-production
  rollback_after_failure
  [[ $ROLLBACK_RESULT == previous-release-health-verified ]] || fail 'previous production was not restored and verified'
  [[ $(sha256sum "$FAILED_RECEIPT" | awk '{print $1}') == "$FAILED_RECEIPT_SHA256" ]] || fail 'original failed receipt changed'
  RESULT=restored
  STEP=write-resume-rollback-evidence
  write_evidence || fail 'cannot write recovery attempt receipt'
  bybit_immutable_json "$RECOVERY_REQUEST_ROOT/completed.json" "$(jq -cnS --arg receipt "$EVIDENCE_DIR/cutover.json" \
    --arg sha "$(sha256sum "$EVIDENCE_DIR/cutover.json" | awk '{print $1}')" --arg old "$OLD_SHA256" \
    '{schema:"monday.bybit_rollback_recovery_completed.v1",receipt:$receipt,receipt_sha256:$sha,restored_old_payload_sha256:$old}')" \
    || fail 'cannot commit rollback recovery completion'
  SUCCESS=1
  trap - EXIT ERR
  printf 'Bybit rollback recovery restored old payload %s\nEvidence: %s/cutover.json\n' "$OLD_SHA256" "$EVIDENCE_DIR"
}

write_evidence() {
  local temporary current_target
  temporary="$EVIDENCE_DIR/cutover.json.tmp"
  current_target=$(readlink -f "$PRODUCTION_LINK" 2>/dev/null || true)
  jq -n \
    --arg started_at "$STARTED_AT" \
    --arg completed_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg result "$RESULT" \
    --arg step "$STEP" \
    --arg failure_reason "$FAILURE_REASON" \
    --arg rollback_result "$ROLLBACK_RESULT" \
    --arg rollback_deployment_manifest_sha256 "$ROLLBACK_DEPLOYMENT_MANIFEST_SHA256" \
    --arg candidate_sha256 "$CANDIDATE_SHA256" \
    --arg deployment_bundle_sha256 "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg previous_sha256 "$OLD_SHA256" \
    --arg mode "$OLD_MODE" \
    --arg current_binary "$current_target" \
    --argjson production_active "$(unit_active_json)" \
    --argjson upload_failure_baseline "${UPLOAD_FAILURE_BASELINE:-null}" \
    --argjson rollback_upload_warning "$ROLLBACK_UPLOAD_WARNING" \
    --argjson rollback_warning_interpreted "$ROLLBACK_WARNING_INTERPRETED" \
    --arg rollback_main_pid "$ROLLBACK_MAIN_PID" \
    --arg rollback_invocation_id "$ROLLBACK_INVOCATION_ID" \
    --argjson bootstrap_upload_verified "$BOOTSTRAP_UPLOAD_VERIFIED" \
    --argjson bootstrap_started_ms "$BOOTSTRAP_STARTED_MS" \
    --arg operation "$OPERATION" --arg failed_receipt "$FAILED_RECEIPT" --arg failed_sha "$FAILED_RECEIPT_SHA256" \
    --arg inventory_sha "$ORPHAN_INVENTORY_SHA256" --arg custody "$CUSTODY_DIR" \
    '{
      schema: "monday.bybit_options_cutover.v1",
      started_at: $started_at,
      completed_at: $completed_at,
      result: $result,
      last_step: $step,
      failure_reason: (if $failure_reason == "" then null else $failure_reason end),
      rollback_result: $rollback_result,
      rollback_deployment_manifest_sha256:
        (if $rollback_deployment_manifest_sha256 == "" then null
         else $rollback_deployment_manifest_sha256 end),
      candidate_sha256: $candidate_sha256,
      deployment_bundle_sha256: (if $deployment_bundle_sha256 == "" then null else $deployment_bundle_sha256 end),
      previous_sha256: (if $previous_sha256 == "" then null else $previous_sha256 end),
      host_mode: $mode,
      upload_failure_baseline: $upload_failure_baseline,
      rollback_health: {main_pid:$rollback_main_pid,invocation_id:$rollback_invocation_id,
        original_upload_warning:$rollback_upload_warning,
        historical_warning_interpreted:$rollback_warning_interpreted},
      bootstrap: {started_at_ms:$bootstrap_started_ms,upload_verified:$bootstrap_upload_verified},
      operation:$operation,
      recovery:(if $operation=="resume-rollback" then {failed_receipt:$failed_receipt,
        failed_receipt_sha256:$failed_sha,orphan_inventory_sha256:$inventory_sha,custody_directory:$custody,
        restored_old_payload_sha256:$previous_sha256,data_recovered:false,delivery_verified:false,
        replay_eligibility:"not_assessed"} else null end),
      current_binary: (if $current_binary == "" then null else $current_binary end),
      production_active: $production_active
    }' > "$temporary" || return 1
  chmod 0640 "$temporary" || return 1
  mv -Tf "$temporary" "$EVIDENCE_DIR/cutover.json" || return 1
}

rollback_after_failure() {
  local safe_to_restart=1 unit rollback_started_ms=0
  ROLLBACK_RESULT=disabled
  systemctl disable --now "${TRANSITION_MASK_UNITS[@]}" >/dev/null 2>&1 || true
  mask_transition_units || safe_to_restart=0
  for unit in "${PRODUCTION_UNITS[@]}"; do
    if systemctl is-active --quiet "$unit" || systemctl is-enabled --quiet "$unit"; then
      safe_to_restart=0
    fi
  done
  if (( safe_to_restart == 0 )); then
    if production_is_fail_closed; then
      ROLLBACK_RESULT=production-stop-or-disable-failed-but-contained
    else
      ROLLBACK_RESULT=production-stop-or-disable-containment-failed
    fi
    copy_health_evidence rollback
    return
  fi

  if [[ -d $CANONICAL_SPOOL ]]; then
    if ! run_candidate_drain "$CANDIDATE_DEPLOYMENT"; then
      safe_to_restart=0
    fi
  fi

  if [[ $OLD_MODE == upgrade ]]; then
    if [[ -n $ROLLBACK_DEPLOYMENT_MANIFEST_SHA256 ]]; then
      printf '%s  %s\n' "$ROLLBACK_DEPLOYMENT_MANIFEST_SHA256" \
        "$EVIDENCE_DIR/rollback-deployment.sha256" | sha256sum --check --strict \
        || safe_to_restart=0
      (
        cd "$OLD_DEPLOYMENT"
        sha256sum --check --strict "$EVIDENCE_DIR/rollback-deployment.sha256"
      ) || safe_to_restart=0
    else
      safe_to_restart=0
    fi
    if (( safe_to_restart == 0 )); then
      ROLLBACK_RESULT=rollback-evidence-unverified-disabled
    elif ! install_deployment "$OLD_DEPLOYMENT" "$OLD_BINARY"; then
      safe_to_restart=0
      ROLLBACK_RESULT=restore-assets-failed-disabled
    elif ! atomic_symlink "$OLD_BINARY" "$PRODUCTION_LINK"; then
      safe_to_restart=0
      ROLLBACK_RESULT=restore-symlink-failed-disabled
    else
      systemctl daemon-reload || safe_to_restart=0
      systemctl unmask --runtime "${PRODUCTION_UNITS[@]}" >/dev/null \
        || safe_to_restart=0
    fi

    if (( safe_to_restart )); then
      copy_health_evidence failed-candidate
      if ! clear_health_before_restart; then
        safe_to_restart=0
        ROLLBACK_RESULT=stale-health-clear-failed-disabled
      else
        rollback_started_ms=$(( $(date +%s) * 1000 ))
      fi
    fi

    if (( safe_to_restart )); then
      systemctl reset-failed "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
      if systemctl start "${PRODUCTION_UNITS[@]}" \
        && capture_rollback_runtime_identity \
        && wait_for_rollback_health "$rollback_started_ms" \
        && systemctl enable "${PRODUCTION_UNITS[@]}" >/dev/null \
        && runtime_matches_release "$OLD_BINARY" true \
        && health_ready_for_rollback "$rollback_started_ms"; then
        ROLLBACK_RESULT=previous-release-health-verified
        if ! systemctl unmask --runtime "${UPLOAD_UNITS[@]}" >/dev/null \
          || ! systemctl start "$TIMER" >/dev/null || ! systemctl enable "$TIMER" >/dev/null \
          || ! systemctl is-active --quiet "$TIMER" || ! systemctl is-enabled --quiet "$TIMER"; then
          ROLLBACK_RESULT=previous-release-timer-unverified-disabled
          systemctl disable --now "${TRANSITION_MASK_UNITS[@]}" >/dev/null 2>&1 || true
          mask_transition_units || ROLLBACK_RESULT=previous-release-timer-containment-failed
        fi
      else
        systemctl disable --now "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
        mask_transition_units || true
        if production_is_fail_closed; then
          ROLLBACK_RESULT=previous-release-health-unverified-disabled
        else
          ROLLBACK_RESULT=previous-release-health-unverified-containment-failed
        fi
      fi
    else
      systemctl disable --now "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
      mask_transition_units || true
      if ! production_is_fail_closed; then
        ROLLBACK_RESULT=previous-release-restore-containment-failed
      elif [[ $ROLLBACK_RESULT == disabled ]]; then
        ROLLBACK_RESULT=previous-release-restored-disabled
      fi
    fi
  else
    if [[ $(readlink -f "$PRODUCTION_LINK" 2>/dev/null || true) == "$CANDIDATE_BINARY" ]]; then
      rm -f "$PRODUCTION_LINK"
    fi
    if production_is_fail_closed; then
      ROLLBACK_RESULT=new-host-disabled
    else
      ROLLBACK_RESULT=new-host-containment-failed
    fi
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
    if (( TRANSITION_STARTED )); then
      if [[ $OPERATION == resume-rollback ]]; then
        systemctl disable --now "${TRANSITION_MASK_UNITS[@]}" >/dev/null 2>&1 || true
        mask_transition_units || ROLLBACK_RESULT=resume-containment-failed
      else
        rollback_after_failure
      fi
    fi
    if write_evidence; then
      printf 'cutover failed; evidence: %s/cutover.json\n' "$EVIDENCE_DIR" >&2
    else
      printf 'cutover failed and evidence could not be written under %s\n' "$EVIDENCE_DIR" >&2
    fi
  fi
  exit "$rc"
}

trap on_error ERR
trap on_exit EXIT

STEP=validate-candidate-release
for path in /opt/monday /opt/monday/bin "$RELEASE_ROOT" "$CANDIDATE_RELEASE" "$CANDIDATE_DEPLOYMENT"; do
  path_is_direct_or_absent "$path" || fail "release path contains a symlink: $path"
done
secure_regular_file "$CANDIDATE_BINARY"
[[ -x $CANDIDATE_BINARY ]] || fail "candidate binary is not executable: $CANDIDATE_BINARY"
printf '%s  %s\n' "$CANDIDATE_SHA256" "$CANDIDATE_BINARY" | sha256sum --check --strict
secure_regular_file "$CANDIDATE_RELEASE/release.json"
secure_regular_file "$GATE_POLICY"
secure_regular_file "$RUNTIME_HEALTH_POLICY"
secure_regular_file "$CONTROL_PLANE_LIB"
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
( cd "$CANDIDATE_DEPLOYMENT" && sha256sum --check --strict DEPLOYMENT_BUNDLE.sha256 ) \
  || fail 'candidate deployment bundle failed its digest check'
# shellcheck disable=SC1090,SC1091
. "$CONTROL_PLANE_LIB"
GATE_BUNDLE_DIR="$GATE_ROOT/$CANDIDATE_SHA256/$DEPLOYMENT_BUNDLE_SHA256"
validate_deployment "$CANDIDATE_DEPLOYMENT" true
id hftcollector >/dev/null 2>&1 || fail 'service account hftcollector is missing'
runuser -u hftcollector -- "$CANDIDATE_BINARY" --self-test
"$CANDIDATE_BINARY" --help | grep -Fq -- '--upload-only'
[[ $(readlink -f "$SHADOW_LINK" 2>/dev/null || true) == "$CANDIDATE_BINARY" ]] \
  || fail 'shadow symlink does not point to the gated candidate binary'

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
  --argjson minimum_symbols "$MINIMUM_SYMBOLS" \
  --argjson test_only false \
  -f "$GATE_POLICY" "$GATE_JSON" >/dev/null \
  || fail 'candidate shadow gate does not meet production thresholds'
install -d -m 0750 "$EVIDENCE_DIR/shadow-gate"
install -m 0640 "$GATE_JSON" "$EVIDENCE_DIR/shadow-gate/gate.json"
install -m 0640 "$GATE_MARKER" "$EVIDENCE_DIR/shadow-gate/PASSED.sha256"

if [[ $OPERATION == resume-rollback ]]; then
  resume_rollback
  exit 0
fi

STEP=validate-host-state
canonical_spool_paths_safe || fail 'canonical spool path contains a symlink or escapes /data'
systemctl is-active --quiet "$UPLOAD_UNIT" && fail "upload unit must be inactive before cutover: $UPLOAD_UNIT"

active_count=0
enabled_count=0
if systemctl is-active --quiet "$UNIT"; then
  active_count=1
fi
if systemctl is-enabled --quiet "$UNIT"; then
  enabled_count=1
fi

if (( active_count == 1 && enabled_count == 1 )); then
  OLD_MODE=upgrade
  [[ -L $PRODUCTION_LINK ]] || fail 'running production binary must be a release symlink'
  OLD_BINARY=$(readlink -f "$PRODUCTION_LINK")
  [[ $OLD_BINARY =~ ^$RELEASE_ROOT/([a-f0-9]{64})/bybit-options-archiver$ ]] \
    || fail "running production symlink is not digest-addressed: $OLD_BINARY"
  OLD_SHA256=${BASH_REMATCH[1]}
  [[ $OLD_SHA256 != "$CANDIDATE_SHA256" ]] || fail 'candidate is already the production release'
  printf '%s  %s\n' "$OLD_SHA256" "$OLD_BINARY" | sha256sum --check --strict
  current_unit=$(systemctl cat "$UNIT") || fail 'could not read active production unit'
  bybit_options_unit_exec_start_matches "$OLD_BINARY" "$current_unit" \
    || fail 'active production unit ExecStart does not match the release symlink'
  OLD_DEPLOYMENT="$RELEASE_ROOT/$OLD_SHA256/deployment"
  validate_deployment "$OLD_DEPLOYMENT" false
  preflight_active_segments || fail 'pre-stop active-segment admission failed; production untouched'
  stage_existing_deployment_for_rollback
elif (( active_count == 0 && enabled_count == 0 )) && [[ ! -e $PRODUCTION_LINK && ! -L $PRODUCTION_LINK ]]; then
  OLD_MODE=new-host
  BOOTSTRAP_DEADLINE=$((SECONDS + BOOTSTRAP_TIMEOUT_SECONDS))
  require_empty_segment_spool || fail 'new host canonical spool contains segment artifacts'
else
  fail "ambiguous production state: active=$active_count enabled=$enabled_count symlink=$PRODUCTION_LINK"
fi

UPLOAD_FAILURE_BASELINE=$(capture_upload_failure_baseline) \
  || fail 'production upload failure baseline is invalid'
if [[ $OLD_MODE == new-host && -f $CANONICAL_SPOOL/upload-status.json ]]; then
  BOOTSTRAP_PREVIOUS_SUCCESS_MS=$(jq -er '(.last_success_at // 0)
    | select(type == "number" and . >= 0 and . == floor)' "$CANONICAL_SPOOL/upload-status.json") \
    || fail 'new host previous upload success is invalid'
fi
copy_health_evidence before-transition

STEP=stop-production
TRANSITION_STARTED=1
if [[ $OLD_MODE == upgrade ]]; then
  systemctl disable --now "${PRODUCTION_UNITS[@]}" "$TIMER"
else
  systemctl disable --now "${PRODUCTION_UNITS[@]}" "$TIMER" >/dev/null 2>&1 || true
fi
for unit in "${PRODUCTION_UNITS[@]}"; do
  systemctl is-active --quiet "$unit" && fail "production unit did not stop: $unit"
done
mask_transition_units || fail 'effective Bybit containment failed'
systemctl is-active --quiet "$TIMER" && fail 'upload timer remained active during transition'
systemctl is-active --quiet "$UPLOAD_UNIT" && fail 'uploader became active during transition'
canonical_spool_paths_safe || fail 'canonical spool path changed during production stop'

install -d -m 0750 -o hftcollector -g hftcollector "$CANONICAL_SPOOL"

if [[ $OLD_MODE == upgrade ]]; then
  STEP=drain-old-production-with-candidate
  run_candidate_drain "$CANDIDATE_DEPLOYMENT"
else
  STEP=initialize-new-host-upload-status
  require_empty_segment_spool || fail 'new host canonical spool contains segment artifacts'
  run_candidate_drain "$CANDIDATE_DEPLOYMENT"
fi

STEP=install-candidate-production-assets
validate_deployment "$CANDIDATE_DEPLOYMENT" true
install_deployment "$CANDIDATE_DEPLOYMENT" "$CANDIDATE_BINARY"
systemctl daemon-reload

STEP=switch-production-symlink
atomic_symlink "$CANDIDATE_BINARY" "$PRODUCTION_LINK"
printf '%s  %s\n' "$CANDIDATE_SHA256" "$PRODUCTION_LINK" | sha256sum --check --strict

STEP=clear-stale-candidate-health
copy_health_evidence previous-production
clear_health_before_restart \
  || fail 'could not clear stale production health before starting the candidate'
CANDIDATE_STARTED_MS=$(( $(date +%s) * 1000 ))
if [[ $OLD_MODE == new-host ]]; then
  BOOTSTRAP_STARTED_MS=$CANDIDATE_STARTED_MS
fi

STEP=start-candidate-production
systemctl reset-failed "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
systemctl unmask --runtime "${PRODUCTION_UNITS[@]}" >/dev/null
if [[ $OLD_MODE == new-host ]]; then
  remaining=$(bootstrap_remaining_seconds) || fail 'new host bootstrap deadline expired'
  timeout --signal=TERM --kill-after=10s "$remaining" systemctl start "${PRODUCTION_UNITS[@]}"
else
  systemctl start "${PRODUCTION_UNITS[@]}"
fi

STEP=verify-candidate-production
wait_for_release_health "$CANDIDATE_BINARY" "$CANDIDATE_STARTED_MS" \
  || fail 'candidate production did not reach verified full-catalog health'
if [[ $OLD_MODE == new-host ]]; then
  STEP=verify-new-host-first-upload
  complete_new_host_bootstrap || fail 'new host did not complete a real first upload and healthy restart'
fi
copy_health_evidence production

STEP=enable-verified-candidate
systemctl enable "${PRODUCTION_UNITS[@]}" >/dev/null
runtime_matches_release "$CANDIDATE_BINARY" true \
  || fail 'candidate runtime identity changed while enabling production'
health_ready_for_release "$MINIMUM_SYMBOLS" "$CANDIDATE_STARTED_MS" \
  || fail 'candidate health changed while enabling production'
systemctl unmask --runtime "${UPLOAD_UNITS[@]}" >/dev/null
systemctl start "$TIMER"
systemctl enable "$TIMER" >/dev/null
if [[ $OLD_MODE == new-host ]]; then
  verify_bootstrap_upload false || fail 'new host first-upload evidence changed before completion'
fi

STEP=write-cutover-evidence
RESULT=passed
ROLLBACK_RESULT=not-needed
write_evidence
SUCCESS=1
trap - EXIT ERR
printf 'Bybit Options collector cutover passed: %s\nEvidence: %s/cutover.json\n' \
  "$CANDIDATE_SHA256" "$EVIDENCE_DIR"
