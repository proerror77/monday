#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf 'Usage: %s <isolate|drain> <spot|usdm>\n' "${0##*/}" >&2
}

root_join() {
  local root=${1:-/} suffix=${2#/}
  root=${root%/}
  [[ -n $root ]] || root=/
  if [[ $root == / ]]; then printf '/%s\n' "$suffix"; else printf '%s/%s\n' "$root" "$suffix"; fi
}

configure_paths() {
  local root=${1:-/}
  root=${root%/}
  [[ -n $root ]] || root=/
  ROOT_PREFIX="$root"
  OPT_ROOT=$(root_join "$root" opt/monday)
  BIN_DIR="$OPT_ROOT/bin"
  RELEASE_ROOT="$OPT_ROOT/releases/binance-lob-archiver"
  CONTROLLER_RELEASE_ROOT="$OPT_ROOT/releases/binance-lob-controller"
  ACTIVE_CONTROLLER="$CONTROLLER_RELEASE_ROOT/active"
  PRODUCTION_LINK="$BIN_DIR/binance-lob-archiver"
  INSTALLED_RECOVERY="$BIN_DIR/monday-rust-lob-recovery-queue"
  CONFIG_ROOT=$(root_join "$root" etc/monday)
  DATA_ROOT=$(root_join "$root" data)
  LOCK_ROOT=$(root_join "$root" run/lock)
  CANONICAL_ROOT="$DATA_ROOT/monday/spool/binance-lob"
  QUEUE_ROOT="$DATA_ROOT/monday/spool/binance-lob-recovery"
  EVIDENCE_ROOT="$DATA_ROOT/monday/evidence/recoveries/lob-queue"
  SAFE_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
  RECOVERY_SERVICE=binance-lob-archiver-recovery
}

fail() {
  printf '%s\n' "$*" >&2
  exit 1
}

CURRENT_ACTION=
CURRENT_ISOLATION_PHASE=idle
CURRENT_ISOLATION_PHASE_STARTED=0
CURRENT_ISOLATION_JOB_ID=unknown
CURRENT_ISOLATION_SIGNAL=none
ISOLATION_READY_DIR=

isolation_path_state() {
  [[ -e $1 || -L $1 ]] && printf 'present\n' || printf 'absent\n'
}

log_isolation_phase() {
  local event=$1 elapsed_seconds=$2 ready_path=${ISOLATION_READY_DIR:-}
  [[ -n $ready_path ]] || ready_path="$QUEUE_MARKET_ROOT/${CURRENT_ISOLATION_JOB_ID}.ready"
  printf 'monday_lob_isolation event=%s phase=%s elapsed_seconds=%s market=%s job_id=%s marker=%s ready=%s canonical=%s\n' \
    "$event" "$CURRENT_ISOLATION_PHASE" "$elapsed_seconds" "${MARKET:-unknown}" \
    "$CURRENT_ISOLATION_JOB_ID" "$(isolation_path_state "$ISOLATION_MARKER")" \
    "$(isolation_path_state "$ready_path")" "$(isolation_path_state "$CANONICAL_SPOOL")" >&2
}

isolation_phase_begin() {
  CURRENT_ISOLATION_PHASE=$1
  CURRENT_ISOLATION_PHASE_STARTED=$SECONDS
  log_isolation_phase begin 0
}

isolation_phase_finish() {
  log_isolation_phase "$1" "$((SECONDS - CURRENT_ISOLATION_PHASE_STARTED))"
  CURRENT_ISOLATION_PHASE=idle
  CURRENT_ISOLATION_PHASE_STARTED=0
}

isolation_phase_done() {
  isolation_phase_finish 'done'
}

isolation_phase_deferred() {
  isolation_phase_finish deferred
}

log_isolation_exit() {
  local status=$1 signal=${2:-none} ready_path=${ISOLATION_READY_DIR:-}
  [[ ${CURRENT_ACTION:-} == isolate ]] || return 0
  [[ -n $ready_path ]] || ready_path="$QUEUE_MARKET_ROOT/${CURRENT_ISOLATION_JOB_ID}.ready"
  printf 'monday_lob_isolation event=exit status=%s signal=%s current_isolation_phase=%s market=%s job_id=%s marker=%s ready=%s canonical=%s\n' \
    "$status" "$signal" "$CURRENT_ISOLATION_PHASE" "${MARKET:-unknown}" \
    "$CURRENT_ISOLATION_JOB_ID" "$(isolation_path_state "$ISOLATION_MARKER")" \
    "$(isolation_path_state "$ready_path")" "$(isolation_path_state "$CANONICAL_SPOOL")" >&2
}

on_exit() {
  local status=$1
  log_isolation_exit "$status" "$CURRENT_ISOLATION_SIGNAL"
}

path_is_direct_or_absent() {
  local path=$1 resolved
  [[ -e $path || -L $path ]] || return 0
  [[ ! -L $path ]] || return 1
  resolved=$(readlink -f -- "$path") || return 1
  [[ $resolved == "$path" ]]
}

secure_regular_file() {
  local path=$1 expected_uid=${2:-0} mode owner
  [[ -f $path && ! -L $path ]] || fail "required regular file is missing or a symlink: $path"
  owner=$(stat -c %u -- "$path")
  mode=$(stat -c %a -- "$path")
  [[ $owner == "$expected_uid" ]] || fail "required file has wrong owner: $path"
  (( (8#$mode & 022) == 0 )) || fail "required file is group/world writable: $path"
}

secure_directory() {
  local path=$1 expected_uid=$2 expected_gid=$3 mode owner gid
  [[ -d $path && ! -L $path ]] || fail "required directory is missing or a symlink: $path"
  owner=$(stat -c %u -- "$path")
  gid=$(stat -c %g -- "$path")
  mode=$(stat -c %a -- "$path")
  [[ $owner == "$expected_uid" ]] || fail "required directory has wrong owner: $path"
  [[ $gid == "$expected_gid" ]] || fail "required directory has wrong group: $path"
  (( (8#$mode & 022) == 0 )) || fail "required directory is group/world writable: $path"
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
  actual=$(env_value "$file" "$key") \
    || fail "$file must contain exactly one $key setting"
  [[ $actual == "$expected" ]] || fail "$file has unsafe $key=$actual (expected $expected)"
}

ALLOWLISTED_ENV_ARGS=()

load_allowlisted_env_args() {
  local file=$1 spool_dir=$2 key value
  ALLOWLISTED_ENV_ARGS=()
  require_env_value "$file" MARKET "$MARKET"
  require_env_value "$file" SHARD_ID all
  require_env_value "$file" SPOOL_DIR "$CANONICAL_SPOOL"
  require_env_value "$file" OSS_BUCKET monday-lob-apne1-1045353359
  require_env_value "$file" OSS_ENDPOINT oss-ap-northeast-1-internal.aliyuncs.com
  require_env_value "$file" OSS_REGION ap-northeast-1
  require_env_value "$file" ALIYUN_PROFILE ecs-role
  for key in \
    MARKET DATASET SHARD_ID SNAPSHOT_LIMIT ZSTD_TIMEOUT_SECONDS \
    OSS_BUCKET OSS_ENDPOINT OSS_REGION ALIYUN_PROFILE OSS_COPY_TIMEOUT_SECONDS; do
    value=$(env_value "$file" "$key") \
      || fail "$file must contain exactly one $key setting"
    ALLOWLISTED_ENV_ARGS+=("$key=$value")
  done
  ALLOWLISTED_ENV_ARGS+=("SPOOL_DIR=$spool_dir")
}

canonical_paths_safe() {
  local path
  for path in \
    "$ROOT_PREFIX" \
    "$(root_join "$ROOT_PREFIX" opt)" \
    "$OPT_ROOT" \
    "$(root_join "$ROOT_PREFIX" opt/monday/releases)" \
    "$CONTROLLER_RELEASE_ROOT" \
    "$(root_join "$ROOT_PREFIX" etc)" \
    "$CONFIG_ROOT" \
    "$DATA_ROOT" \
    "$DATA_ROOT/monday" \
    "$DATA_ROOT/monday/evidence" \
    "$DATA_ROOT/monday/evidence/recoveries" \
    "$DATA_ROOT/monday/spool" \
    "$CANONICAL_ROOT" \
    "$QUEUE_ROOT" \
    "$EVIDENCE_ROOT" \
    "$(root_join "$ROOT_PREFIX" run)" \
    "$LOCK_ROOT"; do
    path_is_direct_or_absent "$path" \
      || fail "path contains a symlink: $path"
  done
}

ensure_root_directory() {
  local path=$1 expected_gid=${2:-0}
  if [[ -e $path || -L $path ]]; then
    secure_directory "$path" 0 "$expected_gid"
  else
    install -d -m 0750 -o root -g "$expected_gid" -- "$path"
  fi
}

ensure_queue_directory() {
  local path=$1 hft_gid=$2 mode
  ensure_root_directory "$path" "$hft_gid"
  mode=$(stat -c %a -- "$path")
  (( (8#$mode & 010) != 0 )) \
    || fail "collector group cannot traverse recovery queue directory: $path"
}

market_paths() {
  CANONICAL_SPOOL="$CANONICAL_ROOT/$MARKET"
  QUEUE_MARKET_ROOT="$QUEUE_ROOT/$MARKET"
  ISOLATION_MARKER="$QUEUE_MARKET_ROOT/isolation.json"
  ENV_FILE="$CONFIG_ROOT/binance-lob-archiver-production-$MARKET.env"
}

same_filesystem() {
  local left=$1 right=$2 left_dev right_dev
  left_dev=$(stat -c %d -- "$left")
  right_dev=$(stat -c %d -- "$right")
  [[ $left_dev == "$right_dev" ]]
}

has_incomplete_parts() {
  local spool=$1
  [[ -d $spool && ! -L $spool ]] || return 1
  [[ -n $(find "$spool" -type f \( \
    -name '*.jsonl.part' -o \
    -name '*.zst.tmp' -o \
    -name '*.part.corrupt' \
  \) -print -quit) ]]
}

segment_artifacts() {
  local spool=$1
  [[ -d $spool && ! -L $spool ]] || return 0
  find "$spool" \( -type f -o -type l \) \( \
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
  local spool=$1 remaining
  remaining=$(segment_artifacts "$spool") || return 1
  [[ -z $remaining ]] || {
    printf '%s\n' "$remaining" >&2
    return 1
  }
}

secure_release_identity() {
  local release_json artifact_json runtime_contract env_sha release_env_sha
  local controller_release controller_sha controller_manifest controller_deployment installed_recovery
  path_is_direct_or_absent "$OPT_ROOT" || fail "release path contains a symlink: $OPT_ROOT"
  path_is_direct_or_absent "$BIN_DIR" || fail "release path contains a symlink: $BIN_DIR"
  path_is_direct_or_absent "$RELEASE_ROOT" || fail "release path contains a symlink: $RELEASE_ROOT"
  [[ -L $PRODUCTION_LINK ]] || fail "production link is missing or not a symlink: $PRODUCTION_LINK"
  [[ $(readlink -- "$PRODUCTION_LINK") == "$ACTIVE_CONTROLLER/binance-lob-archiver" ]] \
    || fail "production link is not the stable active projection: $PRODUCTION_LINK"
  RELEASE_BINARY=$(readlink -f -- "$PRODUCTION_LINK") || fail "could not resolve production link"
  RELEASE_SHA256=$(sha256sum "$RELEASE_BINARY" | awk '{print $1}')
  RELEASE_DIR=$(cd -- "$(dirname -- "$RELEASE_BINARY")" && pwd)
  [[ $RELEASE_DIR == "$RELEASE_ROOT/$RELEASE_SHA256" ]] \
    || fail "production link does not resolve to a digest-addressed release: $RELEASE_BINARY"
  path_is_direct_or_absent "$RELEASE_DIR" || fail "release path contains a symlink: $RELEASE_DIR"
  secure_regular_file "$RELEASE_BINARY" 0
  [[ -x $RELEASE_BINARY ]] || fail "release binary is not executable: $RELEASE_BINARY"
  printf '%s  %s\n' "$RELEASE_SHA256" "$RELEASE_BINARY" | sha256sum --check --strict >/dev/null
  artifact_json="$RELEASE_DIR/release.json"
  release_json=$artifact_json
  secure_regular_file "$artifact_json" 0
  if [[ -e $ACTIVE_CONTROLLER || -L $ACTIVE_CONTROLLER ]]; then
    [[ -L $ACTIVE_CONTROLLER ]] \
      || fail "active controller identity is not a symlink: $ACTIVE_CONTROLLER"
    controller_release=$(readlink -f -- "$ACTIVE_CONTROLLER") \
      || fail 'active controller identity is dangling'
    controller_sha=${controller_release##*/}
    [[ $controller_sha =~ ^[a-f0-9]{64}$ \
      && $controller_release == "$CONTROLLER_RELEASE_ROOT/$controller_sha" ]] \
      || fail 'active controller identity is not digest-addressed'
    path_is_direct_or_absent "$controller_release" \
      || fail "controller release path is indirect: $controller_release"
    controller_manifest="$controller_release/release.json"
    controller_deployment="$controller_release/deployment"
    path_is_direct_or_absent "$controller_deployment" \
      || fail "controller deployment path is indirect: $controller_deployment"
    secure_regular_file "$controller_manifest" 0
    runtime_contract=$(jq -er '.runtime_contract_sha256' "$controller_manifest")
    [[ $runtime_contract =~ ^[a-f0-9]{64}$ ]] \
      || fail "active controller has an invalid runtime contract SHA-256: $controller_manifest"
    secure_regular_file "$controller_release/release.json.sha256" 0
    secure_regular_file "$controller_release/deployment.sha256" 0
    [[ $(sha256sum "$controller_manifest" | awk '{print $1}') == "$controller_sha" ]] \
      || fail 'active controller manifest digest mismatch'
    (cd "$controller_release" \
      && sha256sum --check --strict release.json.sha256 >/dev/null \
      && sha256sum --check --strict deployment.sha256 >/dev/null) \
      || fail 'active controller checksum verification failed'
    jq -e --arg artifact "$RELEASE_SHA256" --arg runtime "$runtime_contract" '
        .schema == "monday.rust_lob_controller_release.v2"
        and .control_plane_version == 2
        and .artifact_sha256 == $artifact
        and .runtime_contract_sha256 == $runtime' \
      "$controller_manifest" >/dev/null \
      || fail 'active controller does not bind the production artifact and runtime contract'
    ACTIVE_CONTROLLER_SHA256=$controller_sha
    ACTIVE_RUNTIME_CONTRACT_SHA256=$runtime_contract
    [[ -L $INSTALLED_RECOVERY ]] \
      || fail "installed recovery controller is not the active projection: $INSTALLED_RECOVERY"
    [[ $(readlink -- "$INSTALLED_RECOVERY") == \
      "$ACTIVE_CONTROLLER/deployment/host-rust-lob-recovery-queue.sh" ]] \
      || fail 'installed recovery controller does not resolve through active controller'
    installed_recovery=$(readlink -f -- "$INSTALLED_RECOVERY") \
      || fail 'installed recovery controller projection is dangling'
    secure_regular_file "$installed_recovery" 0
    cmp -s -- "$installed_recovery" \
      "$controller_deployment/host-rust-lob-recovery-queue.sh" \
      || fail 'installed recovery controller differs from the active controller release'
    release_json=$controller_manifest
    RELEASE_ENV_FILE="$controller_deployment/binance-lob-archiver-production-$MARKET.env"
    [[ -f "$controller_deployment/rust-lob-control-plane-lib.sh" ]] \
      || fail 'active controller shared control-plane library is missing'
    # Recovery helpers and runtime contract are sourced only from the active
    # ControllerRelease; a payload release never supplies control bytes.
    # shellcheck disable=SC1090,SC1091
    . "$controller_deployment/rust-lob-control-plane-lib.sh"
  else
    fail 'active V2 controller is required for recovery operations'
  fi
  RELEASE_BUNDLE_SHA256=$(jq -er '.deployment_bundle_sha256' "$release_json")
  RELEASE_SOURCE_REVISION=$(jq -er '.deployment_source_revision' "$release_json")
  [[ $RELEASE_BUNDLE_SHA256 =~ ^[a-f0-9]{64}$ ]] \
    || fail "release has an invalid deployment bundle SHA-256: $release_json"
  [[ $RELEASE_SOURCE_REVISION =~ ^[a-f0-9]{40,64}$ ]] \
    || fail "release has an invalid deployment source revision: $release_json"
  jq -e --arg sha "$RELEASE_SHA256" --arg bundle "$RELEASE_BUNDLE_SHA256" \
    --arg runtime "$runtime_contract" \
    '.artifact_sha256 == $sha and .deployment_bundle_sha256 == $bundle
      and .runtime_contract_sha256 == $runtime' \
    "$release_json" >/dev/null \
    || fail "release metadata does not match the production binary: $release_json"
  secure_regular_file "$ENV_FILE" 0
  secure_regular_file "$RELEASE_ENV_FILE" 0
  env_sha=$(sha256sum "$ENV_FILE" | awk '{print $1}')
  release_env_sha=$(sha256sum "$RELEASE_ENV_FILE" | awk '{print $1}')
  [[ $env_sha == "$release_env_sha" ]] \
    || fail "installed env drifted from digest-addressed release env: $ENV_FILE"
  ENV_SHA256=$env_sha
}

queue_lock() {
  install -d -m 0755 -- "$LOCK_ROOT"
  exec 9>"$LOCK_ROOT/monday-rust-lob-recovery-queue-$MARKET.lock"
  flock -n 9 || fail "another recovery queue operation holds the $MARKET lock"
}

copy_active_oss() {
  local uri=$1 target=$2 profile endpoint region cli=/usr/local/bin/aliyun
  profile=$(env_value "$RELEASE_ENV_FILE" ALIYUN_PROFILE) \
    || fail 'active recovery environment has no OSS profile'
  endpoint=$(env_value "$RELEASE_ENV_FILE" OSS_ENDPOINT) \
    || fail 'active recovery environment has no OSS endpoint'
  region=$(env_value "$RELEASE_ENV_FILE" OSS_REGION) \
    || fail 'active recovery environment has no OSS region'
  [[ $uri == oss://* && -n $target ]] || return 1
  [[ -x $cli ]] || fail "trusted OSS CLI is missing: $cli"
  runuser --user hftcollector -- env -i \
    HOME=/var/lib/hft-collector PATH="$SAFE_PATH" ALIYUN_PROFILE="$profile" \
    "$cli" ossutil cp "$uri" "$target" --profile "$profile" \
    --endpoint "$endpoint" --region "$region" --force --no-progress >/dev/null
}

verify_upload_triplet_readback() {
  local spool=$1 minimum_success_at=$2 dataset bucket shard prefix tmp triplet status
  status="$spool/upload-status.json"
  dataset=$(env_value "$RELEASE_ENV_FILE" DATASET) \
    || fail 'active recovery environment has no dataset'
  bucket=$(env_value "$RELEASE_ENV_FILE" OSS_BUCKET) \
    || fail 'active recovery environment has no OSS bucket'
  shard=$(env_value "$RELEASE_ENV_FILE" SHARD_ID) \
    || fail 'active recovery environment has no shard'
  prefix="lake/raw/venue=binance/market=$MARKET/dataset=$dataset/shard=$shard"
  tmp=$(mktemp -d "$(root_join "$ROOT_PREFIX" tmp)/monday-recovery-readback.XXXXXX") \
    || fail 'could not create recovery readback temp directory'
  if ! triplet=$(monday_verify_upload_triplet_readback "$status" "$MARKET" "$dataset" \
      "$bucket" "$prefix" "$tmp" "$minimum_success_at" copy_active_oss); then
    rm -rf -- "$tmp"
    fail 'independent OSS triplet readback failed after recovery upload'
  fi
  rm -rf -- "$tmp"
  UPLOAD_TRIPLET_READBACK=$triplet
}

drain_lock() {
  exec 7>"$LOCK_ROOT/monday-rust-lob-recovery-drain.lock"
  flock -n 7
}

write_job_receipt() {
  local job_id=$1 queue_unit=$2 tmp env_copy env_tmp
  CURRENT_ISOLATION_JOB_ID=$job_id
  isolation_phase_begin receipt-file
  env_copy="$CANONICAL_SPOOL/recovery.env"
  env_tmp="$env_copy.tmp.$$"
  install -m 0640 -o root -g root -- "$RELEASE_ENV_FILE" "$env_tmp"
  mv -Tf "$env_tmp" "$env_copy"
  # A path operand uses fsync(2); -f uses syncfs(2) and can stall on all of /data.
  sync "$env_copy"
  tmp="$CANONICAL_SPOOL/job.json.tmp.$$"
  jq -n \
    --arg schema monday.rust_lob_recovery_queue.v1 \
    --arg job_id "$job_id" \
    --arg queued_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg market "$MARKET" \
    --arg canonical_spool "$CANONICAL_SPOOL" \
    --arg env_sha256 "$ENV_SHA256" \
    --arg release_sha256 "$RELEASE_SHA256" \
    --arg payload_sha256 "$RELEASE_SHA256" \
    --arg controller_sha256 "${ACTIVE_CONTROLLER_SHA256:-}" \
    --arg runtime_contract_sha256 "${ACTIVE_RUNTIME_CONTRACT_SHA256:-}" \
    --arg deployment_bundle_sha256 "$RELEASE_BUNDLE_SHA256" \
    --arg deployment_source_revision "$RELEASE_SOURCE_REVISION" \
    --arg release_env recovery.env \
    --arg recovery_unit "$queue_unit" \
    '{schema:$schema,job_id:$job_id,queued_at:$queued_at,market:$market,
      canonical_spool:$canonical_spool,env_sha256:$env_sha256,
      release_sha256:$release_sha256,payload_sha256:$payload_sha256,
      controller_sha256:$controller_sha256,runtime_contract_sha256:$runtime_contract_sha256,
      deployment_bundle_sha256:$deployment_bundle_sha256,
      deployment_source_revision:$deployment_source_revision,
      release_env:$release_env,recovery_unit:$recovery_unit}' >"$tmp"
  chmod 0640 "$tmp"
  mv -Tf "$tmp" "$CANONICAL_SPOOL/job.json"
  sync "$CANONICAL_SPOOL/job.json"
  isolation_phase_done
  isolation_phase_begin receipt-dir
  sync "$CANONICAL_SPOOL"
  isolation_phase_done
}

job_dir_id() {
  local name=${1##*/}
  case "$name" in
    *.ready|*.running|*.failed|*.stale) printf '%s\n' "${name%.*}" ;;
    *) return 1 ;;
  esac
}

write_isolation_marker() {
  local job_id=$1 receipt_sha256=$2 ready_dir tmp
  ready_dir="$QUEUE_MARKET_ROOT/$job_id.ready"
  ISOLATION_READY_DIR=$ready_dir
  isolation_phase_begin marker-file
  tmp="$QUEUE_MARKET_ROOT/.isolation.json.tmp.$$"
  [[ ! -e $ISOLATION_MARKER && ! -L $ISOLATION_MARKER ]] \
    || fail "an isolation transaction is already active: $ISOLATION_MARKER"
  [[ ! -e $tmp && ! -L $tmp ]] || fail "refusing to reuse isolation marker temp path: $tmp"
  jq -n \
    --arg schema monday.rust_lob_recovery_isolation.v1 \
    --arg job_id "$job_id" \
    --arg market "$MARKET" \
    --arg canonical_spool "$CANONICAL_SPOOL" \
    --arg ready_dir "$ready_dir" \
    --arg receipt_sha256 "$receipt_sha256" \
    '{schema:$schema,job_id:$job_id,market:$market,
      canonical_spool:$canonical_spool,ready_dir:$ready_dir,
      receipt_sha256:$receipt_sha256}' >"$tmp"
  chmod 0640 "$tmp"
  mv -Tf "$tmp" "$ISOLATION_MARKER"
  secure_regular_file "$ISOLATION_MARKER" 0
  sync "$ISOLATION_MARKER"
  isolation_phase_done
  isolation_phase_begin marker-dir
  sync "$QUEUE_MARKET_ROOT"
  isolation_phase_done
}

load_isolation_marker() {
  secure_regular_file "$ISOLATION_MARKER" 0
  ISOLATION_JOB_ID=$(jq -er '.job_id' "$ISOLATION_MARKER")
  ISOLATION_MARKET=$(jq -er '.market' "$ISOLATION_MARKER")
  ISOLATION_CANONICAL_SPOOL=$(jq -er '.canonical_spool' "$ISOLATION_MARKER")
  ISOLATION_READY_DIR=$(jq -er '.ready_dir' "$ISOLATION_MARKER")
  ISOLATION_RECEIPT_SHA256=$(jq -er '.receipt_sha256' "$ISOLATION_MARKER")
  CURRENT_ISOLATION_JOB_ID=$ISOLATION_JOB_ID
  jq -e '.schema == "monday.rust_lob_recovery_isolation.v1"' \
    "$ISOLATION_MARKER" >/dev/null \
    || fail "isolation marker has an invalid schema: $ISOLATION_MARKER"
  [[ $ISOLATION_JOB_ID =~ ^[0-9]{8}T[0-9]{6}Z-${MARKET}-[a-f0-9]{12}-[0-9]+$ ]] \
    || fail "isolation marker has an invalid job id: $ISOLATION_MARKER"
  [[ $ISOLATION_MARKET == "$MARKET" ]] \
    || fail "isolation marker market mismatch: $ISOLATION_MARKER"
  [[ $ISOLATION_CANONICAL_SPOOL == "$CANONICAL_SPOOL" ]] \
    || fail "isolation marker canonical spool mismatch: $ISOLATION_MARKER"
  [[ $ISOLATION_READY_DIR == "$QUEUE_MARKET_ROOT/$ISOLATION_JOB_ID.ready" ]] \
    || fail "isolation marker ready path mismatch: $ISOLATION_MARKER"
  [[ $ISOLATION_RECEIPT_SHA256 =~ ^[a-f0-9]{64}$ ]] \
    || fail "isolation marker has an invalid receipt digest: $ISOLATION_MARKER"
}

validate_isolation_receipt() {
  local spool=$1 actual_sha256
  load_job "$spool" "$ISOLATION_JOB_ID"
  [[ $ISOLATION_JOB_ID == *-"${JOB_RELEASE_SHA256:0:12}"-* ]] \
    || fail "isolation transaction job id does not match its release: $spool/job.json"
  actual_sha256=$(sha256sum "$spool/job.json" | awk '{print $1}')
  [[ $actual_sha256 == "$ISOLATION_RECEIPT_SHA256" ]] \
    || fail "isolation transaction receipt drifted: $spool/job.json"
}

require_recreated_canonical_spool() {
  local spool=$1 expected_uid=$2 remaining status
  status="$spool/upload-status.json"
  if [[ -e $status || -L $status ]]; then
    secure_regular_file "$status" "$expected_uid"
  fi
  remaining=$(find "$spool" -mindepth 1 -maxdepth 1 \
    ! -name upload-status.json -print)
  [[ -z $remaining ]] || {
    printf '%s\n' "$remaining" >&2
    return 1
  }
}

complete_isolation_transaction() {
  local hft_uid=$1 hft_gid=$2 spool_lock_held=${3:-0}
  local prior_upload_status spool_lock queue_unit start_status
  isolation_phase_begin validation
  load_isolation_marker
  queue_unit="$RECOVERY_SERVICE@$MARKET.service"
  if [[ -e $ISOLATION_READY_DIR || -L $ISOLATION_READY_DIR ]]; then
    secure_directory "$ISOLATION_READY_DIR" "$hft_uid" "$hft_gid"
    validate_isolation_receipt "$ISOLATION_READY_DIR"
    isolation_phase_done
    isolation_phase_begin canonical-recreate
    if [[ -e $CANONICAL_SPOOL || -L $CANONICAL_SPOOL ]]; then
      secure_directory "$CANONICAL_SPOOL" "$hft_uid" "$hft_gid"
      require_recreated_canonical_spool "$CANONICAL_SPOOL" "$hft_uid" \
        || fail "recreated canonical spool drifted: $CANONICAL_SPOOL"
    else
      install -d -m 0750 -o "$hft_uid" -g "$hft_gid" -- "$CANONICAL_SPOOL"
    fi
  else
    [[ -e $CANONICAL_SPOOL || -L $CANONICAL_SPOOL ]] \
      || fail "isolation marker has neither canonical nor ready spool: $ISOLATION_MARKER"
    secure_directory "$CANONICAL_SPOOL" "$hft_uid" "$hft_gid"
    validate_isolation_receipt "$CANONICAL_SPOOL"
    if (( ! spool_lock_held )); then
      spool_lock="$CANONICAL_SPOOL/.binance-lob-archiver.lock"
      secure_regular_file "$spool_lock" "$hft_uid"
      exec 8<>"$spool_lock"
      flock -n 8 || fail "collector spool lock is already held: $spool_lock"
      spool_lock_held=1
    fi
    isolation_phase_done
    isolation_phase_begin rename
    mv -T -- "$CANONICAL_SPOOL" "$ISOLATION_READY_DIR"
    isolation_phase_done
    isolation_phase_begin canonical-parent
    sync "$CANONICAL_ROOT"
    isolation_phase_done
    isolation_phase_begin queue-parent
    sync "$QUEUE_MARKET_ROOT"
    isolation_phase_done
    isolation_phase_begin canonical-recreate
    install -d -m 0750 -o "$hft_uid" -g "$hft_gid" -- "$CANONICAL_SPOOL"
  fi
  prior_upload_status="$ISOLATION_READY_DIR/upload-status.json"
  if [[ -f $prior_upload_status && ! -L $prior_upload_status ]]; then
    install -m 0640 -o "$hft_uid" -g "$hft_gid" -- \
      "$prior_upload_status" "$CANONICAL_SPOOL/upload-status.json"
    sync "$CANONICAL_SPOOL/upload-status.json"
  fi
  sync "$CANONICAL_SPOOL"
  isolation_phase_done
  isolation_phase_begin canonical-parent
  sync "$CANONICAL_ROOT"
  isolation_phase_done
  isolation_phase_begin marker-remove
  rm -f -- "$ISOLATION_MARKER"
  isolation_phase_done
  isolation_phase_begin queue-parent
  sync "$QUEUE_MARKET_ROOT"
  isolation_phase_done
  isolation_phase_begin handoff
  if (( spool_lock_held )); then
    flock -u 8
    exec 8>&-
  fi
  flock -u 9
  exec 9>&-
  start_status=0
  systemctl start --no-block "$queue_unit" >/dev/null 2>&1 || start_status=$?
  if (( start_status == 0 )); then
    isolation_phase_done
  else
    isolation_phase_deferred
  fi
}

run_isolate() {
  CURRENT_ISOLATION_PHASE=idle
  CURRENT_ISOLATION_PHASE_STARTED=$SECONDS
  CURRENT_ISOLATION_JOB_ID=unknown
  CURRENT_ISOLATION_SIGNAL=none
  ISOLATION_READY_DIR=
  isolation_phase_begin release-identity
  secure_release_identity
  isolation_phase_done
  isolate_market
}

isolate_market() {
  local hft_uid hft_gid job_id queue_unit spool_lock receipt_sha256
  hft_uid=$(id -u hftcollector)
  hft_gid=$(id -g hftcollector)
  ensure_queue_directory "$QUEUE_ROOT" "$hft_gid"
  ensure_queue_directory "$QUEUE_MARKET_ROOT" "$hft_gid"
  if [[ -e $ISOLATION_MARKER || -L $ISOLATION_MARKER ]]; then
    complete_isolation_transaction "$hft_uid" "$hft_gid"
    exit 0
  fi
  if [[ ! -e $CANONICAL_SPOOL && ! -L $CANONICAL_SPOOL ]]; then
    fail "canonical spool is missing; refusing recovery fallback: $CANONICAL_SPOOL"
  fi
  secure_directory "$CANONICAL_SPOOL" "$hft_uid" "$hft_gid"
  isolation_phase_begin incomplete-scan
  if ! has_incomplete_parts "$CANONICAL_SPOOL"; then
    isolation_phase_done
    exit 0
  fi
  isolation_phase_done
  spool_lock="$CANONICAL_SPOOL/.binance-lob-archiver.lock"
  secure_regular_file "$spool_lock" "$hft_uid"
  exec 8<>"$spool_lock"
  flock -n 8 || fail "collector spool lock is already held: $spool_lock"
  same_filesystem "$CANONICAL_ROOT" "$QUEUE_MARKET_ROOT" \
    || fail "queue root must share a filesystem with the canonical spool: $QUEUE_MARKET_ROOT"
  job_id="$(date -u +%Y%m%dT%H%M%SZ)-${MARKET}-${RELEASE_SHA256:0:12}-$$"
  [[ ! -e $QUEUE_MARKET_ROOT/$job_id.ready && ! -L $QUEUE_MARKET_ROOT/$job_id.ready ]] \
    || fail "refusing to reuse queued recovery path: $QUEUE_MARKET_ROOT/$job_id.ready"
  queue_unit="$RECOVERY_SERVICE@$MARKET.service"
  write_job_receipt "$job_id" "$queue_unit"
  receipt_sha256=$(sha256sum "$CANONICAL_SPOOL/job.json" | awk '{print $1}')
  write_isolation_marker "$job_id" "$receipt_sha256"
  complete_isolation_transaction "$hft_uid" "$hft_gid" 1
}

oldest_ready_dir() {
  [[ -d $QUEUE_MARKET_ROOT && ! -L $QUEUE_MARKET_ROOT ]] || return 0
  find "$QUEUE_MARKET_ROOT" -mindepth 1 -maxdepth 1 -type d -name '*.ready' -print \
    | sort | sed -n '1p'
}

running_dirs() {
  [[ -d $QUEUE_MARKET_ROOT && ! -L $QUEUE_MARKET_ROOT ]] || return 0
  find "$QUEUE_MARKET_ROOT" -mindepth 1 -maxdepth 1 -type d -name '*.running' -print | sort
}

job_evidence_root() {
  local job_id=$1
  printf '%s/%s\n' "$EVIDENCE_ROOT" "$job_id"
}

write_result() {
  local path=$1 result=$2 step=$3 message=$4
  jq -n \
    --arg schema monday.rust_lob_recovery_queue_result.v1 \
    --arg job_id "$JOB_ID" \
    --arg market "$MARKET" \
    --arg release_sha256 "$JOB_RELEASE_SHA256" \
    --arg payload_sha256 "${JOB_PAYLOAD_SHA256:-$JOB_RELEASE_SHA256}" \
    --arg controller_sha256 "${JOB_CONTROLLER_SHA256:-}" \
    --arg runtime_contract_sha256 "${JOB_RUNTIME_CONTRACT_SHA256:-}" \
    --arg deployment_bundle_sha256 "$JOB_BUNDLE_SHA256" \
    --arg deployment_source_revision "$JOB_SOURCE_REVISION" \
    --arg env_sha256 "$JOB_ENV_SHA256" \
    --arg started_at "$JOB_STARTED_AT" \
    --arg completed_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg result "$result" \
    --arg step "$step" \
    --arg message "$message" \
    --argjson upload_triplet_readback "${UPLOAD_TRIPLET_READBACK:-{}}" \
    '{schema:$schema,job_id:$job_id,market:$market,
      release_sha256:$release_sha256,deployment_bundle_sha256:$deployment_bundle_sha256,
      payload_sha256:$payload_sha256,controller_sha256:$controller_sha256,
      runtime_contract_sha256:$runtime_contract_sha256,
      deployment_source_revision:$deployment_source_revision,env_sha256:$env_sha256,
      started_at:$started_at,completed_at:$completed_at,result:$result,
      step:$step,message:$message,upload_triplet_readback:$upload_triplet_readback}' >"$path.tmp"
  chmod 0640 "$path.tmp"
  mv -Tf "$path.tmp" "$path"
  sync "$path"
  sync "${path%/*}"
}

load_job() {
  local queue_dir=$1 expected_id=${2:-} job_json queue_id job_schema job_env
  job_json="$queue_dir/job.json"
  secure_regular_file "$job_json" 0
  if [[ -n $expected_id ]]; then
    queue_id=$expected_id
  else
    queue_id=$(job_dir_id "$queue_dir") \
      || fail "queued job directory has an invalid name: $queue_dir"
  fi
  job_schema=$(jq -er '.schema' "$job_json")
  JOB_ID=$(jq -er '.job_id' "$job_json")
  JOB_QUEUED_AT=$(jq -er '.queued_at' "$job_json")
  JOB_MARKET=$(jq -er '.market' "$job_json")
  JOB_CANONICAL_SPOOL=$(jq -er '.canonical_spool' "$job_json")
  JOB_RELEASE_SHA256=$(jq -er '.release_sha256' "$job_json")
  JOB_PAYLOAD_SHA256=$(jq -er '.payload_sha256 // .release_sha256' "$job_json")
  JOB_BUNDLE_SHA256=$(jq -er '.deployment_bundle_sha256' "$job_json")
  JOB_SOURCE_REVISION=$(jq -er '.deployment_source_revision' "$job_json")
  JOB_ENV_SHA256=$(jq -er '.env_sha256' "$job_json")
  JOB_CONTROLLER_SHA256=$(jq -er '.controller_sha256 // empty' "$job_json") || true
  JOB_RUNTIME_CONTRACT_SHA256=$(jq -er '.runtime_contract_sha256 // empty' "$job_json") || true
  job_env=$(jq -er '.release_env' "$job_json")
  JOB_RECOVERY_UNIT=$(jq -er '.recovery_unit' "$job_json")
  [[ $job_schema == monday.rust_lob_recovery_queue.v1 ]] || fail "queued job has an invalid schema: $queue_dir"
  [[ $JOB_ID == "$queue_id" ]] || fail "queued job id does not match its directory: $queue_dir"
  [[ $JOB_QUEUED_AT =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]] \
    || fail "queued job has an invalid queued timestamp: $queue_dir"
  [[ $JOB_MARKET == "$MARKET" ]] || fail "queued job market mismatch: $queue_dir"
  [[ $JOB_CANONICAL_SPOOL == "$CANONICAL_SPOOL" ]] || fail "queued job canonical spool mismatch: $queue_dir"
  [[ $JOB_RELEASE_SHA256 =~ ^[a-f0-9]{64}$ ]] || fail "queued job has an invalid release sha: $queue_dir"
  [[ $JOB_PAYLOAD_SHA256 =~ ^[a-f0-9]{64}$ ]] || fail "queued job has an invalid payload sha: $queue_dir"
  [[ $JOB_BUNDLE_SHA256 =~ ^[a-f0-9]{64}$ ]] || fail "queued job has an invalid bundle sha: $queue_dir"
  [[ $JOB_SOURCE_REVISION =~ ^[a-f0-9]{40,64}$ ]] || fail "queued job has an invalid source revision: $queue_dir"
  [[ $JOB_ENV_SHA256 =~ ^[a-f0-9]{64}$ ]] || fail "queued job has an invalid env sha: $queue_dir"
  [[ $job_env == recovery.env ]] \
    || fail "queued job release env mismatch: $queue_dir"
  [[ $JOB_RECOVERY_UNIT == "$RECOVERY_SERVICE@$MARKET.service" ]] \
    || fail "queued job recovery unit mismatch: $queue_dir"
  [[ -z $JOB_CONTROLLER_SHA256 || $JOB_CONTROLLER_SHA256 =~ ^[a-f0-9]{64}$ ]] \
    || fail "queued job has an invalid controller sha: $queue_dir"
  [[ -z $JOB_RUNTIME_CONTRACT_SHA256 || $JOB_RUNTIME_CONTRACT_SHA256 =~ ^[a-f0-9]{64}$ ]] \
    || fail "queued job has an invalid runtime contract sha: $queue_dir"
}

job_identity_matches_active() {
  [[ -n ${JOB_CONTROLLER_SHA256:-} && -n ${JOB_RUNTIME_CONTRACT_SHA256:-} ]] || return 1
  [[ ${JOB_RELEASE_SHA256:-} == "${RELEASE_SHA256:-}" \
    && ${JOB_PAYLOAD_SHA256:-} == "${RELEASE_SHA256:-}" \
    && ${JOB_BUNDLE_SHA256:-} == "${RELEASE_BUNDLE_SHA256:-}" \
    && ${JOB_SOURCE_REVISION:-} == "${RELEASE_SOURCE_REVISION:-}" \
    && ${JOB_ENV_SHA256:-} == "${ENV_SHA256:-}" \
    && $JOB_CONTROLLER_SHA256 == "${ACTIVE_CONTROLLER_SHA256:-}" \
    && $JOB_RUNTIME_CONTRACT_SHA256 == "${ACTIVE_RUNTIME_CONTRACT_SHA256:-}" ]]
}

mark_stale() {
  local running_dir=$1 step=$2 message=$3 stale_dir evidence_root result_path
  load_job "$running_dir"
  JOB_STARTED_AT=${JOB_STARTED_AT:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}
  stale_dir="${running_dir%.running}.stale"
  evidence_root=$(job_evidence_root "$JOB_ID")
  ensure_root_directory "$DATA_ROOT/monday/evidence"
  ensure_root_directory "$DATA_ROOT/monday/evidence/recoveries"
  ensure_root_directory "$EVIDENCE_ROOT"
  ensure_root_directory "$evidence_root"
  result_path="$evidence_root/result.json"
  [[ ! -e $result_path && ! -L $result_path ]] || fail "refusing to overwrite stale recovery result: $result_path"
  write_result "$result_path" stale "$step" "$message"
  [[ ! -e $stale_dir && ! -L $stale_dir ]] || fail "refusing to reuse stale recovery path: $stale_dir"
  mv -T -- "$running_dir" "$stale_dir"
  sync "$QUEUE_MARKET_ROOT"
  sync "$evidence_root"
}

finalize_passed_running() {
  local running_dir=$1 result evidence_root completed_at
  load_job "$running_dir"
  secure_release_identity
  if ! job_identity_matches_active; then
    mark_stale "$running_dir" stale-identity 'queued recovery identity is not the active ControllerRelease'
    return 0
  fi
  evidence_root=$(job_evidence_root "$JOB_ID")
  [[ -d $evidence_root && ! -L $evidence_root ]] || return 1
  secure_directory "$evidence_root" 0 0
  result="$evidence_root/result.json"
  secure_regular_file "$result" 0
  jq -e \
    --arg job_id "$JOB_ID" \
    --arg market "$MARKET" \
    --arg release_sha256 "$JOB_RELEASE_SHA256" \
    '.result == "passed"
      and .job_id == $job_id
      and .market == $market
      and .release_sha256 == $release_sha256' \
    "$result" >/dev/null || return 1
  completed_at=$(jq -er '.completed_at' "$result") || return 1
  verify_upload_triplet_readback "$running_dir" "$completed_at" || return 1
  [[ ! -e $evidence_root/spool.done && ! -L $evidence_root/spool.done ]] \
    || fail "refusing to reuse evidence spool path: $evidence_root/spool.done"
  mv -T -- "$running_dir" "$evidence_root/spool.done"
  sync "$QUEUE_MARKET_ROOT"
  sync "$evidence_root"
}

check_upload_readback() {
  local spool=$1 status
  status="$spool/upload-status.json"
  [[ -f $status && ! -L $status ]] || fail "upload status missing after drain: $status"
  jq -e '.last_error == null' "$status" >/dev/null \
    || fail "upload status reports an error: $status"
  require_empty_segment_spool "$spool" || fail "detached spool still contains segment artifacts"
}

run_drain_job() {
  local running_dir=$1 release_dir release_binary release_env evidence_root backup_dir result_path
  local -a env_args
  load_job "$running_dir"
  JOB_STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  # The failed job identity is evidence only.  Executable and runtime bytes
  # always come from the currently active ControllerRelease.
  secure_release_identity
  if ! job_identity_matches_active; then
    mark_stale "$running_dir" stale-identity 'queued recovery identity is not the active ControllerRelease'
    return 0
  fi
  release_dir="$RELEASE_ROOT/$RELEASE_SHA256"
  path_is_direct_or_absent "$release_dir" || fail "release path contains a symlink: $release_dir"
  release_binary="$release_dir/binance-lob-archiver"
  release_env="$RELEASE_ENV_FILE"
  secure_regular_file "$release_binary" 0
  secure_regular_file "$release_env" 0
  [[ $(sha256sum "$release_env" | awk '{print $1}') == "$ENV_SHA256" ]] \
    || fail "active runtime env does not match the active controller: $release_env"
  printf '%s  %s\n' "$RELEASE_SHA256" "$release_binary" | sha256sum --check --strict >/dev/null
  evidence_root=$(job_evidence_root "$JOB_ID")
  ensure_root_directory "$DATA_ROOT/monday/evidence"
  ensure_root_directory "$DATA_ROOT/monday/evidence/recoveries"
  ensure_root_directory "$EVIDENCE_ROOT"
  ensure_root_directory "$evidence_root"
  backup_dir="$evidence_root/recovery-input"
  result_path="$evidence_root/result.json"
  load_allowlisted_env_args "$release_env" "$running_dir"
  env_args=("${ALLOWLISTED_ENV_ARGS[@]}")
  env -i \
    HOME=/root \
    PATH="$SAFE_PATH" \
    RUST_LOG=info \
    "${env_args[@]}" \
    RECOVERY_ARTIFACT_SHA256="$RELEASE_SHA256" \
    RECOVERY_DEPLOYMENT_SOURCE_REVISION="$RELEASE_SOURCE_REVISION" \
    RECOVERY_DEPLOYMENT_BUNDLE_SHA256="$RELEASE_BUNDLE_SHA256" \
    RECOVERY_UID="$(id -u hftcollector)" \
    RECOVERY_GID="$(id -g hftcollector)" \
    RECOVERY_BACKUP_DIR="$backup_dir" \
    "$release_binary" --recover-parts-only
  runuser --user hftcollector -- env -i \
    HOME=/var/lib/hft-collector \
    PATH="$SAFE_PATH" \
    RUST_LOG=info \
    "${env_args[@]}" \
    "$release_binary" --upload-only
  check_upload_readback "$running_dir"
  verify_upload_triplet_readback "$running_dir" "$JOB_STARTED_AT"
  write_result "$result_path" passed upload-readback-ok "detached spool recovered, uploaded, and archived"
  [[ ! -e $evidence_root/spool.done && ! -L $evidence_root/spool.done ]] \
    || fail "refusing to reuse evidence spool path: $evidence_root/spool.done"
  mv -T -- "$running_dir" "$evidence_root/spool.done"
  sync "$QUEUE_MARKET_ROOT"
  sync "$evidence_root"
}

mark_failed() {
  local running_dir=$1 step=$2 message=$3 failed_dir evidence_root result_path
  load_job "$running_dir"
  JOB_STARTED_AT=${JOB_STARTED_AT:-$(date -u +%Y-%m-%dT%H:%M:%SZ)}
  failed_dir="${running_dir%.running}.failed"
  evidence_root=$(job_evidence_root "$JOB_ID")
  ensure_root_directory "$DATA_ROOT/monday/evidence"
  ensure_root_directory "$DATA_ROOT/monday/evidence/recoveries"
  ensure_root_directory "$EVIDENCE_ROOT"
  ensure_root_directory "$evidence_root"
  result_path="$evidence_root/result.json"
  write_result "$result_path" failed "$step" "$message"
  if [[ $running_dir != "$failed_dir" ]]; then
    mv -T -- "$running_dir" "$failed_dir"
    sync "$QUEUE_MARKET_ROOT"
  fi
}

CURRENT_RUNNING_DIR=
CURRENT_STEP=idle
JOB_STARTED_AT=
UPLOAD_TRIPLET_READBACK='{}'

on_signal() {
  local signal=$1
  CURRENT_ISOLATION_SIGNAL=$signal
  if [[ -n ${CURRENT_RUNNING_DIR:-} && -d ${CURRENT_RUNNING_DIR:-} ]]; then
    mark_failed "$CURRENT_RUNNING_DIR" "$CURRENT_STEP" "interrupted by $signal"
  fi
  exit 1
}

drain_market() {
  local ready_dir running_dir hft_gid drain_status
  local -a running=()
  if ! drain_lock; then
    printf 'another market recovery is active; deferred %s drain\n' "$MARKET"
    exit 0
  fi
  hft_gid=$(id -g hftcollector)
  ensure_queue_directory "$QUEUE_ROOT" "$hft_gid"
  ensure_queue_directory "$QUEUE_MARKET_ROOT" "$hft_gid"
  ensure_root_directory "$DATA_ROOT/monday/evidence"
  ensure_root_directory "$DATA_ROOT/monday/evidence/recoveries"
  ensure_root_directory "$EVIDENCE_ROOT"
  same_filesystem "$QUEUE_MARKET_ROOT" "$EVIDENCE_ROOT" \
    || fail "recovery queue and evidence root must share a filesystem"
  mapfile -t running < <(running_dirs)
  if (( ${#running[@]} > 1 )); then
    fail "multiple running recovery jobs require manual intervention: $QUEUE_MARKET_ROOT"
  fi
  running_dir=${running[0]:-}
  if [[ -n $running_dir ]]; then
    if finalize_passed_running "$running_dir"; then
      exit 0
    fi
    fail "unfinished running recovery job requires manual intervention: $running_dir"
  fi
  ready_dir=$(oldest_ready_dir)
  [[ -n $ready_dir ]] || exit 0
  load_job "$ready_dir"
  running_dir="${ready_dir%.ready}.running"
  mv -T -- "$ready_dir" "$running_dir"
  sync "$QUEUE_MARKET_ROOT"
  CURRENT_RUNNING_DIR=$running_dir
  CURRENT_STEP=recover-upload
  set +e
  (
    set -Eeuo pipefail
    run_drain_job "$running_dir"
  )
  drain_status=$?
  set -e
  if (( drain_status == 0 )); then
    CURRENT_RUNNING_DIR=
    exit 0
  fi
  mark_failed "$running_dir" "$CURRENT_STEP" "drain failed"
  CURRENT_RUNNING_DIR=
  exit 1
}

main() {
  local action=${1:-} market=${2:-} command
  export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
  local inherited_name
  while IFS= read -r inherited_name; do
    [[ -n $inherited_name ]] || continue
    fail "production recovery refuses inherited control-plane override: $inherited_name"
  done < <(compgen -v MONDAY_)
  if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
    printf 'must run as root\n' >&2
    exit 2
  fi
  if [[ ! $market =~ ^(spot|usdm)$ ]] \
    || { [[ $action =~ ^(isolate|drain)$ ]] && [[ $# -ne 2 ]]; } \
    || [[ ! $action =~ ^(isolate|drain)$ ]]; then
    usage
    exit 2
  fi
  for command in awk chmod date env find flock grep head id install jq mktemp mv readlink rm runuser sed sha256sum sort stat sync systemctl wc; do
    command -v "$command" >/dev/null 2>&1 \
      || fail "missing required command: $command"
  done
  configure_paths /
  MARKET=$market
  canonical_paths_safe
  market_paths
  queue_lock
  if [[ $action == drain ]]; then
    secure_release_identity
  fi
  CURRENT_ACTION=$action
  trap 'on_exit $?' EXIT
  trap 'on_signal INT' INT
  trap 'on_signal TERM' TERM
  case "$action" in
    isolate) run_isolate ;;
    drain) drain_market ;;
  esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
