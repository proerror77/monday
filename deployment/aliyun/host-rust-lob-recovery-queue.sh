#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf 'Usage: %s <isolate|drain> <spot|usdm>\n' "${0##*/}" >&2
}

configure_paths() {
  local root=${1:-/}
  root=${root%/}
  ROOT_PREFIX="$root"
  OPT_ROOT="$root/opt/monday"
  BIN_DIR="$OPT_ROOT/bin"
  RELEASE_ROOT="$OPT_ROOT/releases/binance-lob-archiver"
  PRODUCTION_LINK="$BIN_DIR/binance-lob-archiver"
  CONFIG_ROOT="$root/etc/monday"
  DATA_ROOT="$root/data"
  LOCK_ROOT="$root/run/lock"
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
    "${ROOT_PREFIX:-/}" \
    "$ROOT_PREFIX/opt" \
    "$OPT_ROOT" \
    "$ROOT_PREFIX/opt/monday/releases" \
    "$ROOT_PREFIX/etc" \
    "$CONFIG_ROOT" \
    "$DATA_ROOT" \
    "$DATA_ROOT/monday" \
    "$DATA_ROOT/monday/evidence" \
    "$DATA_ROOT/monday/evidence/recoveries" \
    "$DATA_ROOT/monday/spool" \
    "$CANONICAL_ROOT" \
    "$QUEUE_ROOT" \
    "$EVIDENCE_ROOT" \
    "$ROOT_PREFIX/run" \
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
  local release_json env_sha release_env_sha
  path_is_direct_or_absent "$OPT_ROOT" || fail "release path contains a symlink: $OPT_ROOT"
  path_is_direct_or_absent "$BIN_DIR" || fail "release path contains a symlink: $BIN_DIR"
  path_is_direct_or_absent "$RELEASE_ROOT" || fail "release path contains a symlink: $RELEASE_ROOT"
  [[ -L $PRODUCTION_LINK ]] || fail "production link is missing or not a symlink: $PRODUCTION_LINK"
  RELEASE_BINARY=$(readlink -f -- "$PRODUCTION_LINK") || fail "could not resolve production link"
  RELEASE_SHA256=$(sha256sum "$RELEASE_BINARY" | awk '{print $1}')
  RELEASE_DIR=$(cd -- "$(dirname -- "$RELEASE_BINARY")" && pwd)
  [[ $RELEASE_DIR == "$RELEASE_ROOT/$RELEASE_SHA256" ]] \
    || fail "production link does not resolve to a digest-addressed release: $RELEASE_BINARY"
  path_is_direct_or_absent "$RELEASE_DIR" || fail "release path contains a symlink: $RELEASE_DIR"
  secure_regular_file "$RELEASE_BINARY" 0
  [[ -x $RELEASE_BINARY ]] || fail "release binary is not executable: $RELEASE_BINARY"
  printf '%s  %s\n' "$RELEASE_SHA256" "$RELEASE_BINARY" | sha256sum --check --strict >/dev/null
  release_json="$RELEASE_DIR/release.json"
  secure_regular_file "$release_json" 0
  RELEASE_BUNDLE_SHA256=$(jq -er '.deployment_bundle_sha256' "$release_json")
  RELEASE_SOURCE_REVISION=$(jq -er '.deployment_source_revision' "$release_json")
  [[ $RELEASE_BUNDLE_SHA256 =~ ^[a-f0-9]{64}$ ]] \
    || fail "release has an invalid deployment bundle SHA-256: $release_json"
  [[ $RELEASE_SOURCE_REVISION =~ ^[a-f0-9]{40,64}$ ]] \
    || fail "release has an invalid deployment source revision: $release_json"
  jq -e --arg sha "$RELEASE_SHA256" --arg bundle "$RELEASE_BUNDLE_SHA256" \
    '.artifact_sha256 == $sha and .deployment_bundle_sha256 == $bundle' \
    "$release_json" >/dev/null \
    || fail "release metadata does not match the production binary: $release_json"
  RELEASE_ENV_FILE="$RELEASE_DIR/deployment/binance-lob-archiver-production-$MARKET.env"
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

drain_lock() {
  exec 7>"$LOCK_ROOT/monday-rust-lob-recovery-drain.lock"
  flock -n 7
}

write_job_receipt() {
  local job_id=$1 queue_unit=$2 tmp env_copy env_tmp
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
    --arg deployment_bundle_sha256 "$RELEASE_BUNDLE_SHA256" \
    --arg deployment_source_revision "$RELEASE_SOURCE_REVISION" \
    --arg release_env recovery.env \
    --arg recovery_unit "$queue_unit" \
    '{schema:$schema,job_id:$job_id,queued_at:$queued_at,market:$market,
      canonical_spool:$canonical_spool,env_sha256:$env_sha256,
      release_sha256:$release_sha256,deployment_bundle_sha256:$deployment_bundle_sha256,
      deployment_source_revision:$deployment_source_revision,
      release_env:$release_env,recovery_unit:$recovery_unit}' >"$tmp"
  chmod 0640 "$tmp"
  mv -Tf "$tmp" "$CANONICAL_SPOOL/job.json"
  sync "$CANONICAL_SPOOL/job.json"
  sync "$CANONICAL_SPOOL"
}

job_dir_id() {
  local name=${1##*/}
  case "$name" in
    *.ready|*.running|*.failed) printf '%s\n' "${name%.*}" ;;
    *) return 1 ;;
  esac
}

write_isolation_marker() {
  local job_id=$1 receipt_sha256=$2 ready_dir tmp
  ready_dir="$QUEUE_MARKET_ROOT/$job_id.ready"
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
  sync "$QUEUE_MARKET_ROOT"
}

load_isolation_marker() {
  secure_regular_file "$ISOLATION_MARKER" 0
  ISOLATION_JOB_ID=$(jq -er '.job_id' "$ISOLATION_MARKER")
  ISOLATION_MARKET=$(jq -er '.market' "$ISOLATION_MARKER")
  ISOLATION_CANONICAL_SPOOL=$(jq -er '.canonical_spool' "$ISOLATION_MARKER")
  ISOLATION_READY_DIR=$(jq -er '.ready_dir' "$ISOLATION_MARKER")
  ISOLATION_RECEIPT_SHA256=$(jq -er '.receipt_sha256' "$ISOLATION_MARKER")
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
  local prior_upload_status spool_lock queue_unit
  load_isolation_marker
  queue_unit="$RECOVERY_SERVICE@$MARKET.service"
  if [[ -e $ISOLATION_READY_DIR || -L $ISOLATION_READY_DIR ]]; then
    secure_directory "$ISOLATION_READY_DIR" "$hft_uid" "$hft_gid"
    validate_isolation_receipt "$ISOLATION_READY_DIR"
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
    mv -T -- "$CANONICAL_SPOOL" "$ISOLATION_READY_DIR"
    sync "$CANONICAL_ROOT"
    sync "$QUEUE_MARKET_ROOT"
    install -d -m 0750 -o "$hft_uid" -g "$hft_gid" -- "$CANONICAL_SPOOL"
  fi
  prior_upload_status="$ISOLATION_READY_DIR/upload-status.json"
  if [[ -f $prior_upload_status && ! -L $prior_upload_status ]]; then
    install -m 0640 -o "$hft_uid" -g "$hft_gid" -- \
      "$prior_upload_status" "$CANONICAL_SPOOL/upload-status.json"
    sync "$CANONICAL_SPOOL/upload-status.json"
  fi
  sync "$CANONICAL_SPOOL"
  sync "$CANONICAL_ROOT"
  rm -f -- "$ISOLATION_MARKER"
  sync "$QUEUE_MARKET_ROOT"
  if (( spool_lock_held )); then
    flock -u 8
    exec 8>&-
  fi
  flock -u 9
  exec 9>&-
  systemctl start --no-block "$queue_unit" >/dev/null 2>&1 || true
}

recover_single_legacy_isolation() {
  local hft_uid=$1 hft_gid=$2 queue_dir candidate_count=0 valid_count=0
  while IFS= read -r queue_dir; do
    candidate_count=$((candidate_count + 1))
    if ( load_job "$queue_dir" ) >/dev/null 2>&1; then
      valid_count=$((valid_count + 1))
    fi
  done < <(find "$QUEUE_MARKET_ROOT" -mindepth 1 -maxdepth 1 -type d \
    \( -name '*.ready' -o -name '*.running' \) -print | sort)
  (( candidate_count == 1 && valid_count == 1 )) \
    || fail "canonical spool is missing without one exclusive valid legacy recovery job: $CANONICAL_SPOOL"
  install -d -m 0750 -o "$hft_uid" -g "$hft_gid" -- "$CANONICAL_SPOOL"
  sync "$CANONICAL_ROOT"
  flock -u 9
  exec 9>&-
  systemctl start --no-block "$RECOVERY_SERVICE@$MARKET.service" >/dev/null 2>&1 || true
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
    recover_single_legacy_isolation "$hft_uid" "$hft_gid"
    exit 0
  fi
  secure_directory "$CANONICAL_SPOOL" "$hft_uid" "$hft_gid"
  if ! has_incomplete_parts "$CANONICAL_SPOOL"; then
    exit 0
  fi
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
    --arg deployment_bundle_sha256 "$JOB_BUNDLE_SHA256" \
    --arg deployment_source_revision "$JOB_SOURCE_REVISION" \
    --arg env_sha256 "$JOB_ENV_SHA256" \
    --arg started_at "$JOB_STARTED_AT" \
    --arg completed_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg result "$result" \
    --arg step "$step" \
    --arg message "$message" \
    '{schema:$schema,job_id:$job_id,market:$market,
      release_sha256:$release_sha256,deployment_bundle_sha256:$deployment_bundle_sha256,
      deployment_source_revision:$deployment_source_revision,env_sha256:$env_sha256,
      started_at:$started_at,completed_at:$completed_at,result:$result,
      step:$step,message:$message}' >"$path.tmp"
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
  JOB_BUNDLE_SHA256=$(jq -er '.deployment_bundle_sha256' "$job_json")
  JOB_SOURCE_REVISION=$(jq -er '.deployment_source_revision' "$job_json")
  JOB_ENV_SHA256=$(jq -er '.env_sha256' "$job_json")
  job_env=$(jq -er '.release_env' "$job_json")
  JOB_RECOVERY_UNIT=$(jq -er '.recovery_unit' "$job_json")
  [[ $job_schema == monday.rust_lob_recovery_queue.v1 ]] || fail "queued job has an invalid schema: $queue_dir"
  [[ $JOB_ID == "$queue_id" ]] || fail "queued job id does not match its directory: $queue_dir"
  [[ $JOB_QUEUED_AT =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}Z$ ]] \
    || fail "queued job has an invalid queued timestamp: $queue_dir"
  [[ $JOB_MARKET == "$MARKET" ]] || fail "queued job market mismatch: $queue_dir"
  [[ $JOB_CANONICAL_SPOOL == "$CANONICAL_SPOOL" ]] || fail "queued job canonical spool mismatch: $queue_dir"
  [[ $JOB_RELEASE_SHA256 =~ ^[a-f0-9]{64}$ ]] || fail "queued job has an invalid release sha: $queue_dir"
  [[ $JOB_BUNDLE_SHA256 =~ ^[a-f0-9]{64}$ ]] || fail "queued job has an invalid bundle sha: $queue_dir"
  [[ $JOB_SOURCE_REVISION =~ ^[a-f0-9]{40,64}$ ]] || fail "queued job has an invalid source revision: $queue_dir"
  [[ $JOB_ENV_SHA256 =~ ^[a-f0-9]{64}$ ]] || fail "queued job has an invalid env sha: $queue_dir"
  [[ $job_env == recovery.env ]] \
    || fail "queued job release env mismatch: $queue_dir"
  JOB_RELEASE_ENV="$queue_dir/$job_env"
  [[ $JOB_RECOVERY_UNIT == "$RECOVERY_SERVICE@$MARKET.service" ]] \
    || fail "queued job recovery unit mismatch: $queue_dir"
}

finalize_passed_running() {
  local running_dir=$1 result evidence_root
  load_job "$running_dir"
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
  release_dir="$RELEASE_ROOT/$JOB_RELEASE_SHA256"
  path_is_direct_or_absent "$release_dir" || fail "release path contains a symlink: $release_dir"
  release_binary="$release_dir/binance-lob-archiver"
  release_env="$JOB_RELEASE_ENV"
  secure_regular_file "$release_binary" 0
  secure_regular_file "$release_env" 0
  [[ $(sha256sum "$release_env" | awk '{print $1}') == "$JOB_ENV_SHA256" ]] \
    || fail "queued env does not match the recorded digest: $release_env"
  printf '%s  %s\n' "$JOB_RELEASE_SHA256" "$release_binary" | sha256sum --check --strict >/dev/null
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
    RECOVERY_ARTIFACT_SHA256="$JOB_RELEASE_SHA256" \
    RECOVERY_DEPLOYMENT_SOURCE_REVISION="$JOB_SOURCE_REVISION" \
    RECOVERY_DEPLOYMENT_BUNDLE_SHA256="$JOB_BUNDLE_SHA256" \
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

on_signal() {
  local signal=$1
  if [[ -n ${CURRENT_RUNNING_DIR:-} && -d ${CURRENT_RUNNING_DIR:-} ]]; then
    mark_failed "$CURRENT_RUNNING_DIR" "$CURRENT_STEP" "interrupted by $signal"
  fi
  exit 1
}

drain_market() {
  local ready_dir running_dir hft_gid
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
  if ( run_drain_job "$running_dir" ); then
    CURRENT_RUNNING_DIR=
    exit 0
  fi
  mark_failed "$running_dir" "$CURRENT_STEP" "drain failed"
  CURRENT_RUNNING_DIR=
  exit 1
}

main() {
  local action=${1:-} market=${2:-} command
  if [[ ${EUID:-$(id -u)} -ne 0 ]]; then
    printf 'must run as root\n' >&2
    exit 2
  fi
  if [[ $# -ne 2 || ! $action =~ ^(isolate|drain)$ || ! $market =~ ^(spot|usdm)$ ]]; then
    usage
    exit 2
  fi
  for command in awk chmod date env find flock grep id install jq mv readlink rm runuser sed sha256sum sort stat sync systemctl; do
    command -v "$command" >/dev/null 2>&1 \
      || fail "missing required command: $command"
  done
  configure_paths /
  MARKET=$market
  canonical_paths_safe
  market_paths
  queue_lock
  trap 'on_signal INT' INT
  trap 'on_signal TERM' TERM
  case "$action" in
    isolate)
      secure_release_identity
      isolate_market
      ;;
    drain) drain_market ;;
  esac
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
