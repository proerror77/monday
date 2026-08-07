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

for command in awk chmod cmp date env find flock grep id install jq ln mkdir mountpoint mv readlink rm runuser sed sha256sum sleep stat systemctl tr wc; do
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

run_candidate_drain() {
  local unit_template="$1/bybit-options-archiver.service"
  local key value
  local -a env_args
  canonical_spool_paths_safe || return 1
  env_args=()
  for key in "${DRAIN_ENV_KEYS[@]}"; do
    value=$(env_value_from_unit "$unit_template" "$key") || return 1
    [[ -n $value ]] || return 1
    env_args+=("$key=$value")
  done
  runuser --user hftcollector -- env -i \
    HOME=/var/lib/hft-collector \
    PATH="$SAFE_PATH" \
    RUST_LOG=info \
    "${env_args[@]}" \
    "$CANDIDATE_BINARY" --upload-only || return 1
  jq -e '.failure_count == 0' "$CANONICAL_SPOOL/upload-status.json" >/dev/null \
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
}

health_ready_for_release() {
  local minimum_symbols=$1 minimum_updated_ms=$2 old_updated_ms=${3:-0}
  local health="$CANONICAL_SPOOL/health.json"
  [[ -f $health && ! -L $health ]] || return 1
  jq -e \
    --argjson minimum_symbols "$minimum_symbols" \
    --argjson minimum_updated_ms "$minimum_updated_ms" \
    --argjson old_updated_ms "$old_updated_ms" \
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
        && wait_for_release_health "$OLD_BINARY" "$rollback_started_ms" \
        && systemctl enable "${PRODUCTION_UNITS[@]}" >/dev/null \
        && runtime_matches_release "$OLD_BINARY" true \
        && health_ready_for_release "$MINIMUM_SYMBOLS" "$rollback_started_ms"; then
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
if systemctl is-active --quiet "$UNIT"; then
  systemctl cat "$UNIT" | grep -Fqx "ExecStart=$RELEASE_ROOT/" \
    || fail 'active production unit ExecStart is not digest-addressed'
fi

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
  systemctl cat "$UNIT" | grep -Fqx "ExecStart=$OLD_BINARY" \
    || fail 'active production unit ExecStart does not match the release symlink'
  OLD_DEPLOYMENT="$RELEASE_ROOT/$OLD_SHA256/deployment"
  validate_deployment "$OLD_DEPLOYMENT" false
  stage_existing_deployment_for_rollback
elif (( active_count == 0 && enabled_count == 0 )) && [[ ! -e $PRODUCTION_LINK && ! -L $PRODUCTION_LINK ]]; then
  OLD_MODE=new-host
  require_empty_segment_spool || fail 'new host canonical spool contains segment artifacts'
else
  fail "ambiguous production state: active=$active_count enabled=$enabled_count symlink=$PRODUCTION_LINK"
fi

STEP=stop-production
TRANSITION_STARTED=1
if [[ $OLD_MODE == upgrade ]]; then
  systemctl disable --now "${PRODUCTION_UNITS[@]}" "$TIMER"
else
  systemctl disable "${PRODUCTION_UNITS[@]}" "$TIMER" >/dev/null 2>&1 || true
fi
for unit in "${PRODUCTION_UNITS[@]}"; do
  systemctl is-active --quiet "$unit" && fail "production unit did not stop: $unit"
done
systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}" >/dev/null
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
  STEP=verify-new-host-spool
  require_empty_segment_spool || fail 'new host canonical spool contains segment artifacts'
fi

STEP=switch-production-symlink
atomic_symlink "$CANDIDATE_BINARY" "$PRODUCTION_LINK"
printf '%s  %s\n' "$CANDIDATE_SHA256" "$PRODUCTION_LINK" | sha256sum --check --strict

STEP=clear-stale-candidate-health
copy_health_evidence previous-production
clear_health_before_restart \
  || fail 'could not clear stale production health before starting the candidate'
CANDIDATE_STARTED_MS=$(( $(date +%s) * 1000 ))

STEP=start-candidate-production
systemctl reset-failed "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
systemctl unmask --runtime "${PRODUCTION_UNITS[@]}" >/dev/null
systemctl start "${PRODUCTION_UNITS[@]}"

STEP=verify-candidate-production
wait_for_release_health "$CANDIDATE_BINARY" "$CANDIDATE_STARTED_MS" \
  || fail 'candidate production did not reach verified full-catalog health'
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

STEP=write-cutover-evidence
RESULT=passed
ROLLBACK_RESULT=not-needed
write_evidence
SUCCESS=1
trap - EXIT ERR
printf 'Bybit Options collector cutover passed: %s\nEvidence: %s/cutover.json\n' \
  "$CANDIDATE_SHA256" "$EVIDENCE_DIR"
