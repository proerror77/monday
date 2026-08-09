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

for command in awk chmod cmp date dirname env find flock grep id install jq ln mkdir mountpoint mv readlink rm runuser sha256sum sleep stat systemctl tr wc; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'missing required command: %s\n' "$command" >&2
    exit 2
  fi
done

CANDIDATE_SHA256=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')
RELEASE_ROOT=/opt/monday/releases/binance-usdm-reference-collector
CANDIDATE_RELEASE="$RELEASE_ROOT/$CANDIDATE_SHA256"
CANDIDATE_COLLECTOR="$CANDIDATE_RELEASE/binance-usdm-reference-collector"
CANDIDATE_VERIFIER="$CANDIDATE_RELEASE/binance-usdm-reference-artifact-verifier"
CANDIDATE_UPLOADER="$CANDIDATE_RELEASE/binance-usdm-reference-upload"
CANDIDATE_UPLOADER_SIDECAR="$CANDIDATE_RELEASE/binance-usdm-reference-upload.sha256"
CANDIDATE_DEPLOYMENT="$CANDIDATE_RELEASE/deployment"
GATE_POLICY="$CANDIDATE_DEPLOYMENT/binance-usdm-reference-shadow-gate-policy.jq"
PRODUCTION_MANIFEST="$CANDIDATE_RELEASE/binance-usdm-reference-production-control-assets.sha256"
GATE_ROOT=/data/monday/evidence/binance-usdm-reference-shadow-gates
GATE_BUNDLE_DIR=
GATE_DIR=
GATE_JSON=
GATE_MARKER=
DEPLOYMENT_BUNDLE_SHA256=
DEPLOYMENT_SOURCE_REVISION=
COLLECTOR_LINK=/opt/monday/bin/binance-usdm-reference-collector
UPLOADER_LINK=/opt/monday/bin/binance-usdm-reference-upload
CANONICAL_SPOOL=/data/monday/spool/binance-usdm-reference
UPLOAD_ENV=/etc/monday/binance-usdm-reference-upload.env
HEALTH_TIMEOUT_SECONDS=300
SAFE_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EVIDENCE_ROOT=/data/monday/evidence/binance-usdm-reference-cutovers
EVIDENCE_DIR="$EVIDENCE_ROOT/$(date -u +%Y%m%dT%H%M%SZ)-${CANDIDATE_SHA256:0:12}-$$"

COLLECTOR_UNIT=binance-usdm-reference-collector.service
UPLOAD_SERVICE=binance-usdm-reference-upload.service
UPLOAD_TIMER=binance-usdm-reference-upload.timer
SHADOW_UNIT="binance-usdm-reference-collector-shadow@$CANDIDATE_SHA256.service"
TRANSITION_MASK_UNITS=(
  "$COLLECTOR_UNIT"
  "$UPLOAD_SERVICE"
  "$UPLOAD_TIMER"
  "$SHADOW_UNIT"
)
DEPLOYMENT_ASSETS=(
  binance-usdm-reference-collector.service
  binance-usdm-reference-upload.service
  binance-usdm-reference-upload.timer
  binance-usdm-reference-upload.env
  binance-usdm-reference-cutover.sh
)
DRAIN_ENV_KEYS=(
  OSS_BUCKET
  OSS_ENDPOINT
  OSS_REGION
  ALIYUN_PROFILE
  OSS_COPY_TIMEOUT_SECONDS
)

install -d -m 0755 /run/lock
exec 9>/run/lock/monday-binance-usdm-reference-production.lock
if ! flock -n 9; then
  printf 'another USD-M reference production operation holds the host lock\n' >&2
  exit 1
fi
install -d -m 0755 /run/monday
exec 8>/run/monday/binance-usdm-reference-release.lock
if ! flock -n 8; then
  printf 'a USD-M reference shadow gate is still running\n' >&2
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

for path in /data/monday /data/monday/evidence "$EVIDENCE_ROOT"; do
  if ! path_is_direct_or_absent "$path"; then
    printf 'evidence path contains a symlink: %s\n' "$path" >&2
    exit 1
  fi
done
install -d -m 0750 "$EVIDENCE_ROOT"
mkdir -m 0750 -- "$EVIDENCE_DIR" \
  || { printf 'refusing to reuse cutover evidence directory: %s\n' "$EVIDENCE_DIR" >&2; exit 1; }

STEP=preflight
RESULT=preflight
FAILURE_REASON=
ROLLBACK_RESULT=not-needed
TRANSITION_STARTED=0
SUCCESS=0
CANDIDATE_STARTED_NS=0
DRAIN_DONE_NS=0
OLD_MODE=not-determined
OLD_COLLECTOR=
OLD_UPLOADER=
OLD_RELEASE_SHA256=
CANDIDATE_STARTED=0

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

env_value() {
  local file=$1 key=$2
  awk -F= -v key="$key" '
      $1 == key { count += 1; value = substr($0, length(key) + 2) }
      END { if (count != 1) exit 1; print value }
    ' "$file"
}

require_env_value() {
  local file=$1 key=$2 expected=$3 actual
  if ! actual=$(env_value "$file" "$key"); then
    fail "$file must contain exactly one $key setting"
  fi
  [[ $actual == "$expected" ]] || fail "$file has unsafe $key=$actual (expected $expected)"
}

validate_upload_env() {
  local file=$1
  require_env_value "$file" OSS_BUCKET monday-lob-apne1-1045353359
  require_env_value "$file" OSS_ENDPOINT oss-ap-northeast-1-internal.aliyuncs.com
  require_env_value "$file" OSS_REGION ap-northeast-1
  require_env_value "$file" ALIYUN_PROFILE ecs-role
  require_env_value "$file" OSS_COPY_TIMEOUT_SECONDS 300
}

validate_deployment() {
  local directory=$1 asset
  [[ -d $directory && ! -L $directory ]] || fail "staged deployment is missing: $directory"
  for asset in "${DEPLOYMENT_ASSETS[@]}"; do
    secure_regular_file "$directory/$asset"
  done
  validate_upload_env "$directory/binance-usdm-reference-upload.env"
  grep -Fxq 'ConditionPathIsMountPoint=/data' \
    "$directory/binance-usdm-reference-collector.service" \
    || fail 'candidate collector unit does not assert the /data mount'
  grep -Fxq 'ExecStart=/opt/monday/bin/binance-usdm-reference-collector --output-root /data/monday/spool/binance-usdm-reference --interval-seconds 30 --request-timeout-seconds 10 --oi-concurrency 8 --max-staleness-ms 30000' \
    "$directory/binance-usdm-reference-collector.service" \
    || fail 'candidate collector unit has the wrong executable or arguments'
  grep -Fxq 'ReadWritePaths=/data/monday/spool/binance-usdm-reference' \
    "$directory/binance-usdm-reference-collector.service" \
    || fail 'candidate collector unit has the wrong writable path'
  grep -Fxq 'Type=oneshot' "$directory/binance-usdm-reference-upload.service" \
    || fail 'candidate upload unit is not a oneshot'
  grep -Fxq 'EnvironmentFile=/etc/monday/binance-usdm-reference-upload.env' \
    "$directory/binance-usdm-reference-upload.service" \
    || fail 'candidate upload unit has the wrong environment file'
  grep -Fxq 'ExecStart=/opt/monday/bin/binance-usdm-reference-upload --output-root /data/monday/spool/binance-usdm-reference' \
    "$directory/binance-usdm-reference-upload.service" \
    || fail 'candidate upload unit has the wrong executable or arguments'
  grep -Fxq 'ReadWritePaths=/data/monday/spool/binance-usdm-reference' \
    "$directory/binance-usdm-reference-upload.service" \
    || fail 'candidate upload unit has the wrong writable path'
  grep -Fxq 'Unit=binance-usdm-reference-upload.service' \
    "$directory/binance-usdm-reference-upload.timer" \
    || fail 'candidate upload timer targets the wrong service'
  grep -Fxq 'OnUnitActiveSec=5min' "$directory/binance-usdm-reference-upload.timer" \
    || fail 'candidate upload timer does not retry every five minutes'
  cmp -s "$directory/binance-usdm-reference-cutover.sh" "$CANDIDATE_DEPLOYMENT/binance-usdm-reference-cutover.sh" \
    || fail 'candidate cutover script drifted inside the deployment bundle'
}

atomic_install() {
  local mode=$1 source=$2 destination=$3 temporary
  temporary="${destination}.new.$$"
  install -m "$mode" "$source" "$temporary" || return 1
  mv -Tf "$temporary" "$destination" || return 1
}

install_deployment() {
  local directory=$1
  install -d -m 0755 /etc/systemd/system || return 1
  install -d -m 0755 /etc/monday || return 1
  atomic_install 0644 "$directory/binance-usdm-reference-collector.service" \
    /etc/systemd/system/binance-usdm-reference-collector.service || return 1
  atomic_install 0644 "$directory/binance-usdm-reference-upload.service" \
    /etc/systemd/system/binance-usdm-reference-upload.service || return 1
  atomic_install 0644 "$directory/binance-usdm-reference-upload.timer" \
    /etc/systemd/system/binance-usdm-reference-upload.timer || return 1
  atomic_install 0640 "$directory/binance-usdm-reference-upload.env" \
    /etc/monday/binance-usdm-reference-upload.env || return 1
}

atomic_symlink() {
  local target=$1 link=$2 temporary
  temporary="${link}.new.$$"
  rm -f "$temporary" || return 1
  ln -s "$target" "$temporary" || return 1
  mv -Tf "$temporary" "$link" || return 1
}

canonical_spool_paths_safe() {
  local path
  for path in /data/monday /data/monday/spool "$CANONICAL_SPOOL"; do
    path_is_direct_or_absent "$path" || return 1
  done
}

lake_artifacts() {
  canonical_spool_paths_safe || return 1
  [[ -d $CANONICAL_SPOOL/lake ]] || return 0
  find "$CANONICAL_SPOOL/lake" \( -type f -o -type l \) -print
}

require_empty_lake() {
  local remaining
  remaining=$(lake_artifacts) || return 1
  if [[ -n $remaining ]]; then
    printf '%s\n' "$remaining" >&2
    return 1
  fi
}

run_uploader() {
  local uploader=$1 key value
  local -a env_args
  canonical_spool_paths_safe || return 1
  env_args=()
  for key in "${DRAIN_ENV_KEYS[@]}"; do
    value=$(env_value "$UPLOAD_ENV" "$key") || return 1
    [[ -n $value ]] || return 1
    env_args+=("$key=$value")
  done
  runuser --user hftcollector -- env -i \
    HOME=/var/lib/hft-collector \
    PATH="$SAFE_PATH" \
    RUST_LOG=info \
    "${env_args[@]}" \
    "$uploader" --output-root "$CANONICAL_SPOOL" || return 1
}

run_candidate_drain() {
  run_uploader "$CANDIDATE_UPLOADER" || return 1
  jq -e '.last_error == null and (.uploaded_batches + .retried_batches) >= 1' \
    "$CANONICAL_SPOOL/upload-status.json" >/dev/null || return 1
}

stage_rollback_assets() {
  local rollback="$EVIDENCE_DIR/rollback-assets" asset source
  install -d -m 0750 "$rollback" || return 1
  for asset in binance-usdm-reference-collector.service \
    binance-usdm-reference-upload.service binance-usdm-reference-upload.timer; do
    source="/etc/systemd/system/$asset"
    secure_regular_file "$source"
    install -m 0640 "$source" "$rollback/$asset" || return 1
  done
  secure_regular_file "$UPLOAD_ENV"
  install -m 0640 "$UPLOAD_ENV" "$rollback/binance-usdm-reference-upload.env" || return 1
}

restore_old_production() {
  local rollback="$EVIDENCE_DIR/rollback-assets"
  if (( CANDIDATE_STARTED )); then
    run_uploader "$CANDIDATE_UPLOADER" || return 1
    require_empty_lake || return 1
  fi
  atomic_install 0644 "$rollback/binance-usdm-reference-collector.service" \
    /etc/systemd/system/binance-usdm-reference-collector.service || return 1
  atomic_install 0644 "$rollback/binance-usdm-reference-upload.service" \
    /etc/systemd/system/binance-usdm-reference-upload.service || return 1
  atomic_install 0644 "$rollback/binance-usdm-reference-upload.timer" \
    /etc/systemd/system/binance-usdm-reference-upload.timer || return 1
  atomic_install 0640 "$rollback/binance-usdm-reference-upload.env" "$UPLOAD_ENV" || return 1
  atomic_symlink "$OLD_COLLECTOR" "$COLLECTOR_LINK" || return 1
  atomic_symlink "$OLD_UPLOADER" "$UPLOADER_LINK" || return 1
  systemctl daemon-reload || return 1
  systemctl unmask --runtime "$COLLECTOR_UNIT" "$UPLOAD_SERVICE" "$UPLOAD_TIMER" >/dev/null \
    || return 1
  systemctl reset-failed "$COLLECTOR_UNIT" "$UPLOAD_SERVICE" >/dev/null 2>&1 || true
  systemctl start "$COLLECTOR_UNIT" || return 1
  systemctl enable "$COLLECTOR_UNIT" >/dev/null || return 1
  systemctl enable --now "$UPLOAD_TIMER" >/dev/null || return 1
  systemctl is-active --quiet "$COLLECTOR_UNIT" || return 1
  systemctl is-active --quiet "$UPLOAD_TIMER" || return 1
  [[ $(readlink -f "$COLLECTOR_LINK") == "$OLD_COLLECTOR" ]] || return 1
  [[ $(readlink -f "$UPLOADER_LINK") == "$OLD_UPLOADER" ]] || return 1
}

health_ready_for_release() {
  local minimum_success_ns=$1 health
  health="$CANONICAL_SPOOL/health.json"
  [[ -f $health && ! -L $health ]] || return 1
  jq -e --argjson minimum_success "$minimum_success_ns" '
    .schema == "binance.usdm_reference_health.v1"
    and .status == "healthy"
    and .source_origin == "https://fapi.binance.com"
    and .api_error_count == 0 and .total_api_errors == 0
    and .artifact_error_count == 0 and .total_artifact_errors == 0
    and .last_error == null
    and (.last_success_at_ns | type == "number" and . == floor)
    and .last_success_at_ns >= $minimum_success
    and (.data_path | type == "string" and startswith("/data/monday/spool/binance-usdm-reference/"))
    and (.data_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
    and (.manifest_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
  ' "$health" >/dev/null || return 1
  local data_path data_sha manifest_sha
  data_path=$(jq -er .data_path "$health") || return 1
  data_sha=$(jq -er .data_sha256 "$health") || return 1
  manifest_sha=$(jq -er .manifest_sha256 "$health") || return 1
  [[ -f $data_path && ! -L $data_path ]] || return 1
  "$CANDIDATE_VERIFIER" --data-path "$data_path" --data-sha256 "$data_sha" \
    --manifest-sha256 "$manifest_sha" >/dev/null || return 1
}

runtime_matches_release() {
  local require_enabled=$1 restarts main_pid main_exe
  systemctl is-active --quiet "$COLLECTOR_UNIT" || return 1
  restarts=$(systemctl show "$COLLECTOR_UNIT" --property=NRestarts --value) || return 1
  [[ $restarts == 0 ]] || return 1
  main_pid=$(systemctl show "$COLLECTOR_UNIT" --property=MainPID --value) || return 1
  [[ $main_pid =~ ^[1-9][0-9]*$ ]] || return 1
  main_exe=$(readlink -f "/proc/$main_pid/exe" 2>/dev/null || true)
  [[ $main_exe == "$CANDIDATE_COLLECTOR" ]] || return 1
  if [[ $require_enabled == true ]]; then
    systemctl is-enabled --quiet "$COLLECTOR_UNIT" || return 1
    systemctl is-enabled --quiet "$UPLOAD_TIMER" || return 1
  fi
}

wait_for_release_health() {
  local deadline=$((SECONDS + HEALTH_TIMEOUT_SECONDS))
  while (( SECONDS < deadline )); do
    systemctl is-active --quiet "$COLLECTOR_UNIT" || return 1
    if health_ready_for_release "$CANDIDATE_STARTED_NS" && runtime_matches_release false; then
      return 0
    fi
    sleep 5
  done
  return 1
}

copy_health_evidence() {
  local label=$1 source="$CANONICAL_SPOOL/health.json"
  if [[ -f $source && ! -L $source ]]; then
    install -m 0640 "$source" "$EVIDENCE_DIR/$label-health.json"
  fi
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
  local temporary collector_target uploader_target collector_active timer_enabled
  temporary="$EVIDENCE_DIR/cutover.json.tmp"
  collector_target=$(readlink -f "$COLLECTOR_LINK" 2>/dev/null || true)
  uploader_target=$(readlink -f "$UPLOADER_LINK" 2>/dev/null || true)
  if systemctl is-active --quiet "$COLLECTOR_UNIT"; then
    collector_active=true
  else
    collector_active=false
  fi
  if systemctl is-enabled --quiet "$UPLOAD_TIMER"; then
    timer_enabled=true
  else
    timer_enabled=false
  fi
  jq -n \
    --arg started_at "$STARTED_AT" \
    --arg completed_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg result "$RESULT" \
    --arg step "$STEP" \
    --arg failure_reason "$FAILURE_REASON" \
    --arg rollback_result "$ROLLBACK_RESULT" \
    --arg candidate_sha256 "$CANDIDATE_SHA256" \
    --arg deployment_bundle_sha256 "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg deployment_source_revision "$DEPLOYMENT_SOURCE_REVISION" \
    --arg host_mode "$OLD_MODE" \
    --arg previous_release_sha256 "$OLD_RELEASE_SHA256" \
    --arg collector_binary "$collector_target" \
    --arg uploader_binary "$uploader_target" \
    --argjson collector_active "$collector_active" \
    --argjson upload_timer_enabled "$timer_enabled" \
    '{
      schema: "monday.binance_usdm_reference_cutover.v1",
      started_at: $started_at,
      completed_at: $completed_at,
      result: $result,
      last_step: $step,
      failure_reason: (if $failure_reason == "" then null else $failure_reason end),
      rollback_result: $rollback_result,
      candidate_sha256: $candidate_sha256,
      deployment_bundle_sha256:
        (if $deployment_bundle_sha256 == "" then null else $deployment_bundle_sha256 end),
      deployment_source_revision:
        (if $deployment_source_revision == "" then null else $deployment_source_revision end),
      host_mode: $host_mode,
      previous_release_sha256:
        (if $previous_release_sha256 == "" then null else $previous_release_sha256 end),
      collector_binary: (if $collector_binary == "" then null else $collector_binary end),
      uploader_binary: (if $uploader_binary == "" then null else $uploader_binary end),
      production_units: {collector_active: $collector_active, upload_timer_enabled: $upload_timer_enabled}
    }' > "$temporary" || return 1
  chmod 0640 "$temporary" || return 1
  mv -Tf "$temporary" "$EVIDENCE_DIR/cutover.json" || return 1
}

rollback_after_failure() {
  ROLLBACK_RESULT=disabled
  systemctl disable --now "$COLLECTOR_UNIT" "$UPLOAD_TIMER" >/dev/null 2>&1 || true
  systemctl stop "$UPLOAD_SERVICE" >/dev/null 2>&1 || true
  systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}" >/dev/null 2>&1 || true
  if [[ $OLD_MODE == upgrade ]]; then
    if restore_old_production; then
      ROLLBACK_RESULT=previous-release-restored
    elif production_is_fail_closed; then
      ROLLBACK_RESULT=previous-release-restore-failed-but-contained
    else
      ROLLBACK_RESULT=previous-release-restore-containment-failed
    fi
    copy_health_evidence rollback
    return
  fi
  if systemctl is-active --quiet "$COLLECTOR_UNIT" \
    || systemctl is-enabled --quiet "$COLLECTOR_UNIT" \
    || systemctl is-enabled --quiet "$UPLOAD_TIMER"; then
    if production_is_fail_closed; then
      ROLLBACK_RESULT=production-stop-or-disable-failed-but-contained
    else
      ROLLBACK_RESULT=production-stop-or-disable-containment-failed
    fi
    copy_health_evidence rollback
    return
  fi
  if [[ $(readlink -f "$COLLECTOR_LINK" 2>/dev/null || true) == "$CANDIDATE_COLLECTOR" ]]; then
    rm -f "$COLLECTOR_LINK"
  fi
  if [[ $(readlink -f "$UPLOADER_LINK" 2>/dev/null || true) == "$CANDIDATE_UPLOADER" ]]; then
    rm -f "$UPLOADER_LINK"
  fi
  if production_is_fail_closed; then
    ROLLBACK_RESULT=new-host-disabled
  else
    ROLLBACK_RESULT=new-host-containment-failed
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

script_dir=$(cd -- "$(dirname -- "$0")" && pwd -P)
[[ $script_dir == "$CANDIDATE_DEPLOYMENT" ]] \
  || fail 'cutover runner is outside the candidate deployment bundle'

STEP=validate-candidate-release
for path in /opt/monday /opt/monday/bin "$RELEASE_ROOT" "$CANDIDATE_RELEASE" "$CANDIDATE_DEPLOYMENT"; do
  path_is_direct_or_absent "$path" || fail "release path contains a symlink: $path"
done
for binary in "$CANDIDATE_COLLECTOR" "$CANDIDATE_VERIFIER" "$CANDIDATE_UPLOADER"; do
  secure_regular_file "$binary"
  [[ -x $binary ]] || fail "candidate binary is not executable: $binary"
done
printf '%s  %s\n' "$CANDIDATE_SHA256" "$CANDIDATE_COLLECTOR" | sha256sum --check --strict
secure_regular_file "$CANDIDATE_UPLOADER_SIDECAR"
[[ $(wc -l < "$CANDIDATE_UPLOADER_SIDECAR") -eq 1 ]] \
  || fail 'uploader sidecar must contain exactly one entry'
uploader_entry=$(<"$CANDIDATE_UPLOADER_SIDECAR")
[[ $uploader_entry =~ ^[a-f0-9]{64}[[:space:]]+binance-usdm-reference-upload$ ]] \
  || fail 'uploader sidecar must name only binance-usdm-reference-upload'
(cd "$CANDIDATE_RELEASE" && sha256sum --check --strict binance-usdm-reference-upload.sha256)
secure_regular_file "$CANDIDATE_RELEASE/release.json"
secure_regular_file "$GATE_POLICY"
secure_regular_file "$PRODUCTION_MANIFEST"
manifest_sha=$(sha256sum "$CANDIDATE_RELEASE/binance-usdm-reference-control-assets.sha256" \
  | awk '{print $1}')
archive_sha=$(sha256sum "$CANDIDATE_RELEASE/binance-usdm-reference-control.tar.gz" \
  | awk '{print $1}')
collector_sha=$(sha256sum "$CANDIDATE_COLLECTOR" | awk '{print $1}')
verifier_sha=$(sha256sum "$CANDIDATE_VERIFIER" | awk '{print $1}')
[[ $collector_sha == "$CANDIDATE_SHA256" ]] || fail 'candidate SHA-256 drifted'
jq -e --arg candidate "$CANDIDATE_SHA256" --arg verifier "$verifier_sha" \
  --arg manifest "$manifest_sha" --arg archive "$archive_sha" \
  --arg schema monday.binance_usdm_reference_release.v1 '
  .schema == $schema
  and (keys | sort) == (["candidate","control_archive","control_manifest",
    "schema","source_revision","verifier"] | sort)
  and .candidate == {file:"binance-usdm-reference-collector",sha256:$candidate}
  and .verifier == {file:"binance-usdm-reference-artifact-verifier",sha256:$verifier}
  and .control_manifest == {
    file:"binance-usdm-reference-control-assets.sha256",sha256:$manifest}
  and .control_archive == {
    file:"binance-usdm-reference-control.tar.gz",sha256:$archive}
' "$CANDIDATE_RELEASE/release.json" >/dev/null \
  || fail 'candidate release identity or asset binding is invalid'
DEPLOYMENT_BUNDLE_SHA256=$manifest_sha
DEPLOYMENT_SOURCE_REVISION=$(jq -er '.source_revision | select(type == "string"
  and test("^[a-f0-9]{40,64}$"))' "$CANDIDATE_RELEASE/release.json") \
  || fail 'candidate release has an invalid deployment source revision'
(
  cd "$CANDIDATE_DEPLOYMENT"
  sha256sum --check --strict "$PRODUCTION_MANIFEST" >/dev/null
) || fail 'production control bundle asset digest mismatch'
production_assets=$(awk 'NF == 2 && $1 ~ /^[a-f0-9]{64}$/ {print $2}' "$PRODUCTION_MANIFEST" | sort)
expected_assets=$(printf '%s\n' "${DEPLOYMENT_ASSETS[@]}" | sort)
[[ $production_assets == "$expected_assets" ]] \
  || fail 'production control manifest has an unexpected asset set'
validate_deployment "$CANDIDATE_DEPLOYMENT"
id hftcollector >/dev/null 2>&1 || fail 'service account hftcollector is missing'

STEP=validate-shadow-gate
GATE_BUNDLE_DIR="$GATE_ROOT/$CANDIDATE_SHA256/$DEPLOYMENT_BUNDLE_SHA256"
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
install -d -m 0750 "$EVIDENCE_DIR/shadow-gate"
install -m 0640 "$GATE_JSON" "$EVIDENCE_DIR/shadow-gate/gate.json"
install -m 0640 "$GATE_MARKER" "$EVIDENCE_DIR/shadow-gate/PASSED.sha256"

STEP=validate-host-state
canonical_spool_paths_safe || fail 'canonical spool path contains a symlink or escapes /data'
systemctl is-active --quiet "$SHADOW_UNIT" \
  && fail "candidate shadow unit must be inactive before cutover: $SHADOW_UNIT"
if systemctl is-active --quiet "$COLLECTOR_UNIT" \
  && systemctl is-active --quiet "$UPLOAD_TIMER" \
  && ! systemctl is-active --quiet "$UPLOAD_SERVICE" \
  && systemctl is-enabled --quiet "$COLLECTOR_UNIT" \
  && systemctl is-enabled --quiet "$UPLOAD_TIMER"; then
  OLD_MODE=upgrade
  [[ -L $COLLECTOR_LINK && -L $UPLOADER_LINK ]] \
    || fail 'running production binaries must be release symlinks'
  OLD_COLLECTOR=$(readlink -f "$COLLECTOR_LINK")
  OLD_UPLOADER=$(readlink -f "$UPLOADER_LINK")
  [[ $OLD_COLLECTOR =~ ^$RELEASE_ROOT/([a-f0-9]{64})/binance-usdm-reference-collector$ ]] \
    || fail "production collector is not digest-addressed: $OLD_COLLECTOR"
  OLD_RELEASE_SHA256=${BASH_REMATCH[1]}
  [[ $OLD_RELEASE_SHA256 != "$CANDIDATE_SHA256" ]] \
    || fail 'candidate is already the production release'
  [[ $OLD_UPLOADER == "$RELEASE_ROOT/$OLD_RELEASE_SHA256/binance-usdm-reference-upload" ]] \
    || fail "production uploader does not belong to the collector release: $OLD_UPLOADER"
  printf '%s  %s\n' "$OLD_RELEASE_SHA256" "$OLD_COLLECTOR" | sha256sum --check --strict
  secure_regular_file "$OLD_UPLOADER"
  [[ -x $OLD_UPLOADER ]] || fail 'production uploader is not executable'
  stage_rollback_assets
elif ! systemctl is-active --quiet "$COLLECTOR_UNIT" \
  && ! systemctl is-active --quiet "$UPLOAD_TIMER" \
  && ! systemctl is-active --quiet "$UPLOAD_SERVICE" \
  && ! systemctl is-enabled --quiet "$COLLECTOR_UNIT" \
  && ! systemctl is-enabled --quiet "$UPLOAD_TIMER" \
  && [[ ! -e $COLLECTOR_LINK && ! -L $COLLECTOR_LINK ]] \
  && [[ ! -e $UPLOADER_LINK && ! -L $UPLOADER_LINK ]]; then
  OLD_MODE=new-host
  require_empty_lake || fail 'new host canonical spool contains reference lake artifacts'
else
  fail 'production state is neither a healthy existing release nor an empty new host'
fi

STEP=stop-production
TRANSITION_STARTED=1
if [[ $OLD_MODE == upgrade ]]; then
  systemctl stop "$UPLOAD_TIMER" "$COLLECTOR_UNIT"
  systemctl stop "$UPLOAD_SERVICE" >/dev/null 2>&1 || true
fi
systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}" >/dev/null

if [[ $OLD_MODE == upgrade ]]; then
  STEP=drain-v2-with-old-uploader
  run_uploader "$OLD_UPLOADER" || fail 'old uploader could not drain the V2 backlog'
  require_empty_lake || fail 'V2 backlog remains after the old uploader drain'
  install -m 0640 "$CANONICAL_SPOOL/upload-status.json" \
    "$EVIDENCE_DIR/pre-upgrade-upload-status.json"
fi

STEP=install-candidate-production-assets
validate_deployment "$CANDIDATE_DEPLOYMENT"
install_deployment "$CANDIDATE_DEPLOYMENT"
install -d -m 0750 -o hftcollector -g hftcollector "$CANONICAL_SPOOL"
install -d -m 0750 -o hftcollector -g hftcollector /var/lib/hft-collector
systemctl daemon-reload

STEP=verify-empty-spool-before-v3
require_empty_lake || fail 'canonical spool contains artifacts before the V3 switch'

STEP=switch-production-symlink
atomic_symlink "$CANDIDATE_COLLECTOR" "$COLLECTOR_LINK"
printf '%s  %s\n' "$CANDIDATE_SHA256" "$COLLECTOR_LINK" | sha256sum --check --strict
atomic_symlink "$CANDIDATE_UPLOADER" "$UPLOADER_LINK"
(
  cd "$CANDIDATE_RELEASE"
  sha256sum --check --strict binance-usdm-reference-upload.sha256 >/dev/null
)

STEP=start-candidate-production
systemctl reset-failed "$COLLECTOR_UNIT" >/dev/null 2>&1 || true
systemctl unmask --runtime "$COLLECTOR_UNIT" "$UPLOAD_SERVICE" "$UPLOAD_TIMER" >/dev/null
CANDIDATE_STARTED_NS=$(date +%s%N)
systemctl start "$COLLECTOR_UNIT"
CANDIDATE_STARTED=1

STEP=verify-candidate-production
wait_for_release_health \
  || fail 'candidate production did not reach verified reference health'
copy_health_evidence production

STEP=verify-candidate-upload
run_candidate_drain \
  || fail 'candidate upload drain did not complete a verified OSS round trip'
DRAIN_DONE_NS=$(date +%s%N)
install -m 0640 "$CANONICAL_SPOOL/upload-status.json" \
  "$EVIDENCE_DIR/production-upload-status.json"

STEP=verify-post-drain-health
post_drain_deadline=$((SECONDS + HEALTH_TIMEOUT_SECONDS))
while (( SECONDS < post_drain_deadline )); do
  systemctl is-active --quiet "$COLLECTOR_UNIT" \
    || fail 'candidate production stopped after the upload drain'
  if health_ready_for_release "$DRAIN_DONE_NS" && runtime_matches_release false; then
    break
  fi
  sleep 5
done
health_ready_for_release "$DRAIN_DONE_NS" \
  || fail 'candidate production did not publish fresh health after the upload drain'
copy_health_evidence post-drain

STEP=enable-verified-candidate
systemctl enable "$COLLECTOR_UNIT" >/dev/null
systemctl enable --now "$UPLOAD_TIMER" >/dev/null
runtime_matches_release true \
  || fail 'candidate runtime identity changed while enabling production'
health_ready_for_release "$DRAIN_DONE_NS" \
  || fail 'reference health changed while enabling production'

STEP=write-cutover-evidence
RESULT=passed
ROLLBACK_RESULT=not-needed
write_evidence
cutover_sha=$(sha256sum "$EVIDENCE_DIR/cutover.json" | awk '{print $1}')
(set -C; printf '%s  cutover.json\n' "$cutover_sha" >"$EVIDENCE_DIR/PASSED.sha256") \
  || fail 'could not publish immutable PASSED marker'
chmod 0640 "$EVIDENCE_DIR/PASSED.sha256"
SUCCESS=1
trap - EXIT ERR
printf 'USD-M reference cutover passed: %s\nEvidence: %s/cutover.json\n' \
  "$CANDIDATE_SHA256" "$EVIDENCE_DIR"
