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

for command in awk chmod cmp date env find flock grep id install jq ln mkdir mountpoint mv readlink rm runuser sha256sum sleep sort stat systemctl tr wc; do
  if ! command -v "$command" >/dev/null 2>&1; then
    printf 'missing required command: %s\n' "$command" >&2
    exit 2
  fi
done

CANDIDATE_SHA256=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')
RELEASE_ROOT=/opt/monday/releases/binance-lob-archiver
CANDIDATE_RELEASE="$RELEASE_ROOT/$CANDIDATE_SHA256"
CANDIDATE_BINARY="$CANDIDATE_RELEASE/binance-lob-archiver"
CANDIDATE_DEPLOYMENT="$CANDIDATE_RELEASE/deployment"
GATE_POLICY="$CANDIDATE_DEPLOYMENT/rust-lob-shadow-gate-policy.jq"
RUNTIME_HEALTH_POLICY="$CANDIDATE_DEPLOYMENT/rust-lob-runtime-health-policy.jq"
GATE_ROOT=/data/monday/evidence/shadow-gates
GATE_BUNDLE_DIR=
GATE_DIR=
GATE_JSON=
GATE_MARKER=
DEPLOYMENT_BUNDLE_SHA256=
DEPLOYMENT_SOURCE_REVISION=
PRODUCTION_LINK=/opt/monday/bin/binance-lob-archiver
SHADOW_LINK=/opt/monday/bin/binance-lob-archiver-shadow
CANONICAL_SPOOL=/data/monday/spool/binance-lob
HEALTH_TIMEOUT_SECONDS=300
SAFE_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EVIDENCE_DIR="/data/monday/evidence/cutovers/$(date -u +%Y%m%dT%H%M%SZ)-${CANDIDATE_SHA256:0:12}-$$"
DRAIN_MAY_HAVE_MUTATED=0

PRODUCTION_UNITS=(
  binance-lob-archiver-production@spot.service
  binance-lob-archiver-production@usdm.service
)
UPLOAD_UNITS=(
  binance-lob-archiver-upload@spot.service
  binance-lob-archiver-upload@usdm.service
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
QUIESCENT_UNITS=(
  binance-lob-archiver-rust@spot.service
  binance-lob-archiver-rust@usdm.service
  binance-lob-archiver-rust-upload@spot.service
  binance-lob-archiver-rust-upload@usdm.service
  binance-lob-archiver-upload@spot.service
  binance-lob-archiver-upload@usdm.service
  "${LEGACY_UNITS[@]}"
)
DEPLOYMENT_ASSETS=(
  binance-lob-archiver-production@.service
  binance-lob-archiver-upload@.service
  binance-lob-archiver-production-spot.env
  binance-lob-archiver-production-usdm.env
)
DRAIN_ENV_KEYS=(
  MARKET
  DATASET
  SHARD_ID
  SNAPSHOT_LIMIT
  ZSTD_TIMEOUT_SECONDS
  SPOOL_DIR
  OSS_BUCKET
  OSS_ENDPOINT
  OSS_REGION
  ALIYUN_PROFILE
  OSS_COPY_TIMEOUT_SECONDS
)

install -d -m 0755 /run/lock
exec 9>/run/lock/monday-rust-lob-release.lock
if ! flock -n 9; then
  printf 'another Rust collector release operation holds the host lock\n' >&2
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

for path in /data/monday /data/monday/evidence /data/monday/evidence/cutovers; do
  if ! path_is_direct_or_absent "$path"; then
    printf 'evidence path contains a symlink: %s\n' "$path" >&2
    exit 1
  fi
done
install -d -m 0750 /data/monday/evidence/cutovers
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
OLD_SESSION_SPOT=
OLD_SESSION_USDM=
OLD_USDM_MINIMUM_SYMBOLS=400
CANDIDATE_STARTED_NS=0

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

is_usdm_top100() {
  local value=$1 unique
  local -a symbols
  [[ $value =~ ^[A-Z0-9]+(,[A-Z0-9]+)*$ ]] || return 1
  IFS=, read -r -a symbols <<<"$value"
  (( ${#symbols[@]} == 100 )) || return 1
  unique=$(printf '%s\n' "${symbols[@]}" | sort -u | wc -l)
  (( unique == 100 ))
}

validate_production_env() {
  local file=$1 market=$2 dataset=$3 spool=$4 strict=$5 symbols
  require_env_value "$file" MARKET "$market"
  require_env_value "$file" DATASET "$dataset"
  require_env_value "$file" SHARD_ID all
  symbols=$(env_value "$file" SYMBOLS) \
    || fail "$file must contain exactly one SYMBOLS setting"
  if [[ $market == spot ]]; then
    [[ $symbols == ALL ]] || fail "$file must set SYMBOLS=ALL"
  elif [[ $strict == true || $symbols != ALL ]]; then
    is_usdm_top100 "$symbols" \
      || fail "$file must set SYMBOLS=ALL or 100 unique explicit symbols"
  fi
  if [[ $market == usdm && $strict == true ]]; then
    require_env_value "$file" WS_SHARD_SIZE 100
  fi
  require_env_value "$file" DEPTH_MODE diff
  require_env_value "$file" SEGMENT_SECONDS 3600
  require_env_value "$file" SPOOL_DIR "$spool"
  require_env_value "$file" OSS_BUCKET monday-lob-apne1-1045353359
  require_env_value "$file" OSS_ENDPOINT oss-ap-northeast-1-internal.aliyuncs.com
  require_env_value "$file" OSS_REGION ap-northeast-1
  require_env_value "$file" ALIYUN_PROFILE ecs-role
}

validate_deployment() {
  local directory=$1 strict=${2:-false} asset usdm_dataset
  [[ -d $directory && ! -L $directory ]] || fail "staged deployment is missing: $directory"
  for asset in "${DEPLOYMENT_ASSETS[@]}"; do
    secure_regular_file "$directory/$asset"
  done

  validate_production_env \
    "$directory/binance-lob-archiver-production-spot.env" \
    spot spot_all /data/monday/spool/binance-lob/spot "$strict"
  if [[ $strict == true ]]; then
    usdm_dataset=usdm_perpetual_top100_lob
  else
    usdm_dataset=$(env_value \
      "$directory/binance-lob-archiver-production-usdm.env" DATASET) \
      || fail 'rollback USD-M deployment has no DATASET'
    [[ $usdm_dataset == usdm_perpetual_all || $usdm_dataset == usdm_perpetual_top100_lob ]] \
      || fail "rollback USD-M deployment has an unsupported DATASET=$usdm_dataset"
  fi
  validate_production_env \
    "$directory/binance-lob-archiver-production-usdm.env" \
    usdm "$usdm_dataset" /data/monday/spool/binance-lob/usdm "$strict"

  if [[ $strict == true ]]; then
    grep -Fxq 'AssertPathIsMountPoint=/data' \
      "$directory/binance-lob-archiver-production@.service" \
      || fail 'candidate production unit does not assert the /data mount'
    grep -Fxq 'EnvironmentFile=/etc/monday/binance-lob-archiver-production-%i.env' \
      "$directory/binance-lob-archiver-production@.service" \
      || fail 'candidate production unit has the wrong environment file'
    grep -Fxq 'ExecStart=/opt/monday/bin/binance-lob-archiver' \
      "$directory/binance-lob-archiver-production@.service" \
      || fail 'candidate production unit has the wrong executable'
    grep -Fxq 'ExecStartPre=/opt/monday/bin/binance-lob-archiver --self-test' \
      "$directory/binance-lob-archiver-production@.service" \
      || fail 'candidate production unit does not run the binary self-test'
    grep -Fxq 'AssertPathIsMountPoint=/data' \
      "$directory/binance-lob-archiver-upload@.service" \
      || fail 'candidate upload unit does not assert the /data mount'
    grep -Fxq 'ExecStart=/opt/monday/bin/binance-lob-archiver --upload-only' \
      "$directory/binance-lob-archiver-upload@.service" \
      || fail 'candidate upload unit is not explicitly upload-only'
    grep -Fxq 'EnvironmentFile=/etc/monday/binance-lob-archiver-production-%i.env' \
      "$directory/binance-lob-archiver-upload@.service" \
      || fail 'candidate upload unit has the wrong environment file'
  fi
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
  atomic_install 0644 "$directory/binance-lob-archiver-production@.service" \
    /etc/systemd/system/binance-lob-archiver-production@.service || return 1
  atomic_install 0644 "$directory/binance-lob-archiver-upload@.service" \
    /etc/systemd/system/binance-lob-archiver-upload@.service || return 1
  atomic_install 0640 "$directory/binance-lob-archiver-production-spot.env" \
    /etc/monday/binance-lob-archiver-production-spot.env || return 1
  atomic_install 0640 "$directory/binance-lob-archiver-production-usdm.env" \
    /etc/monday/binance-lob-archiver-production-usdm.env || return 1
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
  for path in \
    /data/monday \
    /data/monday/spool \
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

run_candidate_drain() {
  local deployment=$1 market env_file key value recovery_parent recovery_dir
  local -a env_args
  canonical_spool_paths_safe || return 1
  for market in spot usdm; do
    canonical_spool_paths_safe || return 1
    env_file="$deployment/binance-lob-archiver-production-$market.env"
    env_args=()
    for key in "${DRAIN_ENV_KEYS[@]}"; do
      value=$(env_value "$env_file" "$key") || return 1
      [[ -n $value ]] || return 1
      env_args+=("$key=$value")
    done
    if find "$CANONICAL_SPOOL/$market" -type f -name '*.jsonl.part' -print -quit \
      | grep -q .; then
      recovery_parent="$EVIDENCE_DIR/recovery-input/$market"
      install -d -m 0750 -o root -g root -- "$recovery_parent" || return 1
      recovery_dir="$recovery_parent/$(date -u +%Y%m%dT%H%M%S%NZ)-$$"
      [[ ! -e $recovery_dir && ! -L $recovery_dir ]] || return 1
      if ! env -i \
        HOME=/root \
        PATH="$SAFE_PATH" \
        RUST_LOG=info \
        "${env_args[@]}" \
        RECOVERY_UID="$(id -u hftcollector)" \
        RECOVERY_GID="$(id -g hftcollector)" \
        RECOVERY_BACKUP_DIR="$recovery_dir" \
        "$CANDIDATE_BINARY" --recover-parts-only; then
        [[ -f $recovery_dir/receipt.json ]] && DRAIN_MAY_HAVE_MUTATED=1
        return 1
      fi
      DRAIN_MAY_HAVE_MUTATED=1
    fi
    DRAIN_MAY_HAVE_MUTATED=1
    runuser --user hftcollector -- env -i \
      HOME=/var/lib/hft-collector \
      PATH="$SAFE_PATH" \
      RUST_LOG=info \
      "${env_args[@]}" \
      "$CANDIDATE_BINARY" --upload-only || return 1
    jq -e '.last_error == null' "$CANONICAL_SPOOL/$market/upload-status.json" >/dev/null \
      || return 1
  done
  require_empty_segment_spool || return 1
}

stage_existing_deployment_for_rollback() {
  local existing=0 asset source installed_source mode source_kind old_usdm_symbols
  local release_deployment=$OLD_DEPLOYMENT
  local snapshot="$EVIDENCE_DIR/rollback-deployment"
  local manifest="$EVIDENCE_DIR/rollback-deployment.sha256"
  [[ ! -L $OLD_DEPLOYMENT ]] || fail "old staged deployment is a symlink: $OLD_DEPLOYMENT"
  for asset in "${DEPLOYMENT_ASSETS[@]}"; do
    if [[ -e $release_deployment/$asset ]]; then
      ((existing += 1))
    fi
  done
  if (( existing == ${#DEPLOYMENT_ASSETS[@]} )); then
    validate_deployment "$release_deployment" false
    source_kind=release
  elif (( existing == 0 )); then
    source_kind=installed
  else
    fail "old release has a partial staged deployment: $release_deployment"
  fi

  [[ ! -e $snapshot && ! -L $snapshot ]] \
    || fail "rollback evidence snapshot already exists: $snapshot"
  install -d -m 0750 "$snapshot"
  for asset in "${DEPLOYMENT_ASSETS[@]}"; do
    case "$asset" in
      *.service) installed_source="/etc/systemd/system/$asset"; mode=0644 ;;
      *.env) installed_source="/etc/monday/$asset"; mode=0640 ;;
    esac
    secure_regular_file "$installed_source"
    if [[ $source_kind == release ]]; then
      source="$release_deployment/$asset"
      secure_regular_file "$source"
      cmp -s -- "$source" "$installed_source" \
        || fail "installed production asset drifted from the active immutable release: $installed_source"
    else
      source=$installed_source
    fi
    atomic_install "$mode" "$source" "$snapshot/$asset"
  done
  validate_deployment "$snapshot" false
  old_usdm_symbols=$(awk -F= '$1 == "SYMBOLS" { print substr($0, 9) }' \
    "$snapshot/binance-lob-archiver-production-usdm.env")
  if [[ $old_usdm_symbols == ALL ]]; then
    OLD_USDM_MINIMUM_SYMBOLS=400
  else
    OLD_USDM_MINIMUM_SYMBOLS=100
  fi
  (
    cd "$snapshot"
    sha256sum "${DEPLOYMENT_ASSETS[@]}"
  ) >"$manifest"
  chmod 0640 "$manifest"
  ROLLBACK_DEPLOYMENT_MANIFEST_SHA256=$(sha256sum "$manifest" | awk '{print $1}')
  OLD_DEPLOYMENT=$snapshot
}

unit_active_json() {
  local unit=$1
  if systemctl is-active --quiet "$unit"; then
    printf true
  else
    printf false
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
  local health expected_dataset
  health="$CANONICAL_SPOOL/$market/health.json"
  [[ -f $health && ! -L $health ]] || return 1
  case "$market" in
    spot) expected_dataset=spot_all ;;
    usdm)
      expected_dataset=$(env_value \
        /etc/monday/binance-lob-archiver-production-usdm.env DATASET) \
        || return 1
      [[ $expected_dataset == usdm_perpetual_all \
        || $expected_dataset == usdm_perpetual_top100_lob ]] || return 1
      ;;
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
    main_exe=$(readlink -f "/proc/$main_pid/exe" 2>/dev/null || true)
    [[ $main_exe == "$binary" ]] || return 1
    if [[ $require_enabled == true ]]; then
      systemctl is-enabled --quiet "$unit" || return 1
    fi
  done
}

wait_for_release_health() {
  local binary=$1 old_spot_session=$2 old_usdm_session=$3
  local minimum_updated_ns=${4:-0} usdm_minimum_symbols=${5:-400} deadline unit
  deadline=$((SECONDS + HEALTH_TIMEOUT_SECONDS))
  while (( SECONDS < deadline )); do
    for unit in "${PRODUCTION_UNITS[@]}"; do
      systemctl is-active --quiet "$unit" || return 1
    done
    if health_ready_for_release spot 1000 "$old_spot_session" "$minimum_updated_ns" \
      && health_ready_for_release usdm "$usdm_minimum_symbols" "$old_usdm_session" "$minimum_updated_ns" \
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

write_evidence() {
  local temporary current_target spot_active usdm_active
  temporary="$EVIDENCE_DIR/cutover.json.tmp"
  current_target=$(readlink -f "$PRODUCTION_LINK" 2>/dev/null || true)
  spot_active=$(unit_active_json "${PRODUCTION_UNITS[0]}")
  usdm_active=$(unit_active_json "${PRODUCTION_UNITS[1]}")
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
    --argjson spot_active "$spot_active" \
    --argjson usdm_active "$usdm_active" \
    '{
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
      production_units_active: {spot: $spot_active, usdm: $usdm_active}
    }' > "$temporary" || return 1
  chmod 0640 "$temporary" || return 1
  mv -Tf "$temporary" "$EVIDENCE_DIR/cutover.json" || return 1
}

rollback_after_failure() {
  local safe_to_restart=1 unit rollback_started_ns=0
  ROLLBACK_RESULT=disabled
  systemctl disable --now "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
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

  if [[ -d $CANONICAL_SPOOL \
    && ( $STEP != drain-old-production-with-candidate || $DRAIN_MAY_HAVE_MUTATED -eq 1 ) ]]; then
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
    elif ! install_deployment "$OLD_DEPLOYMENT"; then
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
        rollback_started_ns=$(date +%s%N)
      fi
    fi

    if (( safe_to_restart )); then
      systemctl reset-failed "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
      if systemctl start "${PRODUCTION_UNITS[@]}" \
        && wait_for_release_health \
          "$OLD_BINARY" "$OLD_SESSION_SPOT" "$OLD_SESSION_USDM" "$rollback_started_ns" \
          "$OLD_USDM_MINIMUM_SYMBOLS" \
        && systemctl enable "${PRODUCTION_UNITS[@]}" >/dev/null \
        && runtime_matches_release "$OLD_BINARY" true \
        && health_ready_for_release spot 1000 "$OLD_SESSION_SPOT" "$rollback_started_ns" \
        && health_ready_for_release usdm "$OLD_USDM_MINIMUM_SYMBOLS" \
          "$OLD_SESSION_USDM" "$rollback_started_ns"; then
        ROLLBACK_RESULT=previous-release-health-verified
        systemctl unmask --runtime "${UPLOAD_UNITS[@]}" >/dev/null 2>&1 || true
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
validate_deployment "$CANDIDATE_DEPLOYMENT" true
id hftcollector >/dev/null 2>&1 || fail 'service account hftcollector is missing'
runuser -u hftcollector -- "$CANDIDATE_BINARY" --self-test
"$CANDIDATE_BINARY" --help | grep -Fq -- '--upload-only'
"$CANDIDATE_BINARY" --help | grep -Fq -- '--recover-parts-only'
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
  -f "$GATE_POLICY" "$GATE_JSON" >/dev/null \
  || fail 'candidate shadow gate does not meet production thresholds'
GATE_USDM_SYMBOLS=$(jq -er '.markets.usdm.symbols_config' "$GATE_JSON")
CANDIDATE_USDM_SYMBOLS=$(env_value \
  "$CANDIDATE_DEPLOYMENT/binance-lob-archiver-production-usdm.env" SYMBOLS)
[[ $GATE_USDM_SYMBOLS == "$CANDIDATE_USDM_SYMBOLS" ]] \
  || fail 'candidate shadow gate USD-M symbols differ from the deployment bundle'
install -d -m 0750 "$EVIDENCE_DIR/shadow-gate"
install -m 0640 "$GATE_JSON" "$EVIDENCE_DIR/shadow-gate/gate.json"
install -m 0640 "$GATE_MARKER" "$EVIDENCE_DIR/shadow-gate/PASSED.sha256"

STEP=validate-host-state
canonical_spool_paths_safe || fail 'canonical spool path contains a symlink or escapes /data'
for unit in "${QUIESCENT_UNITS[@]}"; do
  systemctl is-active --quiet "$unit" && fail "unit must be inactive before cutover: $unit"
done
for unit in "${LEGACY_UNITS[@]}"; do
  systemctl is-enabled --quiet "$unit" \
    && fail "legacy collector unit must be disabled before cutover: $unit"
done

active_count=0
enabled_count=0
OLD_SESSION_SPOT=$(jq -r '.session_id // empty' "$CANONICAL_SPOOL/spot/health.json" 2>/dev/null || true)
OLD_SESSION_USDM=$(jq -r '.session_id // empty' "$CANONICAL_SPOOL/usdm/health.json" 2>/dev/null || true)
for unit in "${PRODUCTION_UNITS[@]}"; do
  if systemctl is-active --quiet "$unit"; then
    ((active_count += 1))
  fi
  if systemctl is-enabled --quiet "$unit"; then
    ((enabled_count += 1))
  fi
done

if (( active_count == 2 && enabled_count == 2 )); then
  OLD_MODE=upgrade
  [[ -L $PRODUCTION_LINK ]] || fail 'running production binary must be a release symlink'
  OLD_BINARY=$(readlink -f "$PRODUCTION_LINK")
  [[ $OLD_BINARY =~ ^$RELEASE_ROOT/([a-f0-9]{64})/binance-lob-archiver$ ]] \
    || fail "running production symlink is not digest-addressed: $OLD_BINARY"
  OLD_SHA256=${BASH_REMATCH[1]}
  [[ $OLD_SHA256 != "$CANDIDATE_SHA256" ]] || fail 'candidate is already the production release'
  printf '%s  %s\n' "$OLD_SHA256" "$OLD_BINARY" | sha256sum --check --strict
  OLD_DEPLOYMENT="$RELEASE_ROOT/$OLD_SHA256/deployment"
  stage_existing_deployment_for_rollback
elif (( active_count == 0 && enabled_count == 0 )) && [[ ! -e $PRODUCTION_LINK && ! -L $PRODUCTION_LINK ]]; then
  OLD_MODE=new-host
  require_empty_segment_spool || fail 'new host canonical spool contains segment artifacts'
else
  fail "ambiguous production state: active=$active_count enabled=$enabled_count symlink=$PRODUCTION_LINK"
fi

STEP=stop-production
TRANSITION_STARTED=1
systemctl disable --now "${LEGACY_UNITS[@]}" >/dev/null 2>&1 || true
for unit in "${LEGACY_UNITS[@]}"; do
  systemctl is-active --quiet "$unit" && fail "legacy collector unit did not stop: $unit"
  systemctl is-enabled --quiet "$unit" && fail "legacy collector unit remained enabled: $unit"
done
if [[ $OLD_MODE == upgrade ]]; then
  systemctl disable --now "${PRODUCTION_UNITS[@]}"
else
  systemctl disable "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
fi
for unit in "${PRODUCTION_UNITS[@]}"; do
  systemctl is-active --quiet "$unit" && fail "production unit did not stop: $unit"
  [[ $(systemctl show --property MainPID --value "$unit") == 0 ]] \
    || fail "production unit retained a MainPID after stop: $unit"
done
systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}" >/dev/null
canonical_spool_paths_safe || fail 'canonical spool path changed during production stop'

STEP=install-candidate-production-assets
validate_deployment "$CANDIDATE_DEPLOYMENT" true
install_deployment "$CANDIDATE_DEPLOYMENT"
install -d -m 0750 -o hftcollector -g hftcollector \
  "$CANONICAL_SPOOL/spot" "$CANONICAL_SPOOL/usdm"
systemctl daemon-reload

if [[ $OLD_MODE == upgrade ]]; then
  STEP=drain-old-production-with-candidate
  run_candidate_drain "$OLD_DEPLOYMENT"
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
CANDIDATE_STARTED_NS=$(date +%s%N)

STEP=start-candidate-production
systemctl reset-failed "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
systemctl unmask --runtime "${PRODUCTION_UNITS[@]}" >/dev/null
systemctl start "${PRODUCTION_UNITS[@]}"

STEP=verify-candidate-production
wait_for_release_health \
  "$CANDIDATE_BINARY" "$OLD_SESSION_SPOT" "$OLD_SESSION_USDM" "$CANDIDATE_STARTED_NS" 100 \
  || fail 'candidate production did not reach verified catalog health'
copy_health_evidence production

STEP=enable-verified-candidate
systemctl enable "${PRODUCTION_UNITS[@]}" >/dev/null
runtime_matches_release "$CANDIDATE_BINARY" true \
  || fail 'candidate runtime identity changed while enabling production'
health_ready_for_release spot 1000 "$OLD_SESSION_SPOT" "$CANDIDATE_STARTED_NS" \
  || fail 'Spot health changed while enabling production'
health_ready_for_release usdm 100 "$OLD_SESSION_USDM" "$CANDIDATE_STARTED_NS" \
  || fail 'USD-M health changed while enabling production'
systemctl unmask --runtime "${UPLOAD_UNITS[@]}" >/dev/null

STEP=write-cutover-evidence
RESULT=passed
ROLLBACK_RESULT=not-needed
write_evidence
SUCCESS=1
trap - EXIT ERR
printf 'Rust collector cutover passed: %s\nEvidence: %s/cutover.json\n' \
  "$CANDIDATE_SHA256" "$EVIDENCE_DIR"
