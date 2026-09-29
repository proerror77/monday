#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf 'Usage: %s <candidate-binary-sha256>\n' "${0##*/}" >&2
}

if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
  printf 'must run as root\n' >&2
  exit 2
fi
if [[ $# -ne 1 || ! $1 =~ ^[A-Fa-f0-9]{64}$ ]]; then
  usage
  exit 2
fi

for command in awk chmod cmp date env find flock grep id install jq ln mkdir mountpoint mv readlink rm runuser sed sha256sum sleep stat systemctl timeout tr wc; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'missing required command: %s\n' "$command" >&2
    exit 2
  fi
done

CANDIDATE_SHA256=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')
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

for path in /data/monday /data/monday/evidence /data/monday/evidence/bybit-options-cutovers; do
  if ! path_is_direct_or_absent "$path"; then
    printf 'evidence path contains a symlink: %s\n' "$path" >&2
    exit 1
  fi
done
install -d -m 0750 /data/monday/evidence/bybit-options-cutovers
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
  install -m "$mode" "$source" "$temporary" || return 1
  mv -Tf "$temporary" "$destination" || return 1
}

render_unit() {
  local template=$1 destination=$2 binary=$3
  sed "s|/opt/monday/releases/bybit-options-archiver/@BYBIT_OPTIONS_ARCHIVER_SHA256@/bybit-options-archiver|$binary|g" \
    "$template" >"$destination"
  chown root:root "$destination"
  chmod 0444 "$destination"
}

atomic_symlink() {
  local target=$1 link=$2 temporary
  temporary="${link}.new.$$"
  rm -f "$temporary" || return 1
  ln -s "$target" "$temporary" || return 1
  mv -Tf "$temporary" "$link" || return 1
}

install_deployment() {
  local directory=$1 binary=$2
  install -d -m 0755 /etc/systemd/system || return 1
  render_unit "$directory/bybit-options-archiver.service" "/etc/systemd/system/$UNIT" "$binary" || return 1
  render_unit "$directory/bybit-options-upload.service" "/etc/systemd/system/$UPLOAD_UNIT" "$binary" || return 1
  atomic_install 0644 "$directory/$TIMER" "/etc/systemd/system/$TIMER" || return 1
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
    state=$(systemctl is-enabled "$unit" 2>/dev/null || true)
    [[ $state == masked || $state == masked-runtime ]] || return 1
  done
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
      current_binary: (if $current_binary == "" then null else $current_binary end),
      production_active: $production_active
    }' > "$temporary" || return 1
  chmod 0640 "$temporary" || return 1
  mv -Tf "$temporary" "$EVIDENCE_DIR/cutover.json" || return 1
}

rollback_after_failure() {
  local safe_to_restart=1 unit rollback_started_ms=0
  ROLLBACK_RESULT=disabled
  systemctl disable --now "${PRODUCTION_UNITS[@]}" "$TIMER" >/dev/null 2>&1 || true
  systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}" >/dev/null 2>&1 || true
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
        systemctl unmask --runtime "${UPLOAD_UNITS[@]}" >/dev/null 2>&1 || true
        systemctl start "$TIMER" >/dev/null 2>&1 || true
        systemctl enable "$TIMER" >/dev/null 2>&1 || true
      else
        systemctl disable --now "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
        systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}" >/dev/null 2>&1 || true
        if production_is_fail_closed; then
          ROLLBACK_RESULT=previous-release-health-unverified-disabled
        else
          ROLLBACK_RESULT=previous-release-health-unverified-containment-failed
        fi
      fi
    else
      systemctl disable --now "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
      systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}" >/dev/null 2>&1 || true
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
      rollback_after_failure
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
systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}" >/dev/null
systemctl is-active --quiet "$TIMER" && fail 'upload timer remained active during transition'
systemctl is-active --quiet "$UPLOAD_UNIT" && fail 'uploader became active during transition'
canonical_spool_paths_safe || fail 'canonical spool path changed during production stop'

STEP=install-candidate-production-assets
validate_deployment "$CANDIDATE_DEPLOYMENT" true
install_deployment "$CANDIDATE_DEPLOYMENT" "$CANDIDATE_BINARY"
install -d -m 0750 -o hftcollector -g hftcollector "$CANONICAL_SPOOL"
systemctl daemon-reload

if [[ $OLD_MODE == upgrade ]]; then
  STEP=drain-old-production-with-candidate
  run_candidate_drain "$CANDIDATE_DEPLOYMENT"
else
  STEP=initialize-new-host-upload-status
  require_empty_segment_spool || fail 'new host canonical spool contains segment artifacts'
  run_candidate_drain "$CANDIDATE_DEPLOYMENT"
fi

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
