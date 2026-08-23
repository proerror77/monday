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
RECOVERY_QUEUE_ROOT=/data/monday/spool/binance-lob-recovery
RECOVERY_EVIDENCE_ROOT=/data/monday/evidence/recoveries
HEALTH_TIMEOUT_SECONDS=300
SAFE_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EVIDENCE_DIR="/data/monday/evidence/cutovers/$(date -u +%Y%m%dT%H%M%SZ)-${CANDIDATE_SHA256:0:12}-$$"
DRAIN_REQUIRED=0
DRAIN_ATTEMPTED=0
DRAIN_MAY_HAVE_MUTATED=0
SPOOL_ENV_DEPLOYMENT=
OLD_RECOVERY_TIMERS_ENABLED=0
OLD_SPOT_RESTARTS=
OLD_SPOT_INVOCATION_ID=
MASK_USDM_UPLOAD_FOR_TRANSITION=0

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
RECOVERY_UNITS=(
  binance-lob-archiver-recovery@spot.service
  binance-lob-archiver-recovery@usdm.service
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
BASE_DEPLOYMENT_ASSETS=(
  binance-lob-archiver-production@.service
  binance-lob-archiver-upload@.service
  binance-lob-archiver-production-spot.env
  binance-lob-archiver-production-usdm.env
)
RECOVERY_DEPLOYMENT_ASSETS=(
  binance-lob-archiver-recovery@.service
  binance-lob-archiver-recovery@.timer
  host-rust-lob-recovery-queue.sh
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

for path in \
  /data/monday \
  /data/monday/evidence \
  /data/monday/evidence/cutovers \
  "$RECOVERY_EVIDENCE_ROOT" \
  /data/monday/spool \
  "$RECOVERY_QUEUE_ROOT"; do
  if ! path_is_direct_or_absent "$path"; then
    printf 'evidence path contains a symlink: %s\n' "$path" >&2
    exit 1
  fi
done
install -d -m 0750 -o root -g root \
  /data/monday/evidence/cutovers "$RECOVERY_EVIDENCE_ROOT"
install -d -m 0750 -o root -g hftcollector "$RECOVERY_QUEUE_ROOT"
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
PRODUCTION_USDM_MEMORY_DROPIN=/etc/systemd/system/binance-lob-archiver-production@usdm.service.d/10-memory.conf
PRODUCTION_USDM_MEMORY_DROPIN_PRESENT=0
PRODUCTION_USDM_MEMORY_DROPIN_BACKUP=
PRODUCTION_USDM_MEMORY_DROPIN_MANIFEST=
PRODUCTION_USDM_MEMORY_DROPIN_SHA256=

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

secure_directory() {
  local path=$1 mode owner
  [[ -d $path && ! -L $path ]] || fail "required directory is missing or a symlink: $path"
  owner=$(stat -c %u -- "$path")
  mode=$(stat -c %a -- "$path")
  [[ $owner == 0 ]] || fail "required directory is not root-owned: $path"
  (( (8#$mode & 022) == 0 )) || fail "required directory is group/world writable: $path"
}

systemctl_value() {
  systemctl show "$1" --property="$2" --value
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
    require_env_value "$file" WS_SHARD_SIZE 25
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
  local directory=$1 strict=${2:-false} asset usdm_dataset recovery_assets=0
  [[ -d $directory && ! -L $directory ]] || fail "staged deployment is missing: $directory"
  for asset in "${BASE_DEPLOYMENT_ASSETS[@]}"; do
    secure_regular_file "$directory/$asset"
  done
  for asset in "${RECOVERY_DEPLOYMENT_ASSETS[@]}"; do
    [[ -e $directory/$asset ]] && ((recovery_assets += 1))
  done
  if [[ $strict == true ]] || (( recovery_assets > 0 )); then
    (( recovery_assets == ${#RECOVERY_DEPLOYMENT_ASSETS[@]} )) \
      || fail "staged deployment has partial recovery assets: $directory"
    for asset in "${RECOVERY_DEPLOYMENT_ASSETS[@]}"; do
      secure_regular_file "$directory/$asset"
    done
  fi

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
    grep -Fxq 'ExecStartPre=+/opt/monday/bin/monday-rust-lob-recovery-queue isolate %i' \
      "$directory/binance-lob-archiver-production@.service" \
      || fail 'candidate production unit does not isolate interrupted spools'
    grep -Fxq 'CPUQuota=80%' "$directory/binance-lob-archiver-production@.service" \
      || fail 'candidate production unit has the wrong CPU quota'
    grep -Fxq 'MemoryMax=2560M' "$directory/binance-lob-archiver-production@.service" \
      || fail 'candidate production unit has the wrong memory limit'
    grep -Fxq 'StartLimitIntervalSec=300' "$directory/binance-lob-archiver-production@.service" \
      || fail 'candidate production unit has the wrong restart interval'
    grep -Fxq 'StartLimitBurst=5' "$directory/binance-lob-archiver-production@.service" \
      || fail 'candidate production unit has no bounded restart policy'
    grep -Fxq 'AssertPathIsMountPoint=/data' \
      "$directory/binance-lob-archiver-upload@.service" \
      || fail 'candidate upload unit does not assert the /data mount'
    grep -Fxq 'ExecStart=/opt/monday/bin/binance-lob-archiver --upload-only' \
      "$directory/binance-lob-archiver-upload@.service" \
      || fail 'candidate upload unit is not explicitly upload-only'
    grep -Fxq 'EnvironmentFile=/etc/monday/binance-lob-archiver-production-%i.env' \
      "$directory/binance-lob-archiver-upload@.service" \
      || fail 'candidate upload unit has the wrong environment file'
    grep -Fxq 'ExecStart=/opt/monday/bin/monday-rust-lob-recovery-queue drain %i' \
      "$directory/binance-lob-archiver-recovery@.service" \
      || fail 'candidate recovery unit has the wrong executable'
    grep -Fxq 'CPUQuota=25%' "$directory/binance-lob-archiver-recovery@.service" \
      || fail 'candidate recovery unit has the wrong CPU quota'
    grep -Fxq 'MemoryMax=768M' "$directory/binance-lob-archiver-recovery@.service" \
      || fail 'candidate recovery unit has the wrong memory limit'
    grep -Fxq 'Unit=binance-lob-archiver-recovery@%i.service' \
      "$directory/binance-lob-archiver-recovery@.timer" \
      || fail 'candidate recovery timer has the wrong service target'
  fi
}

validate_memory_only_dropin() {
  local path=$1
  awk '
    BEGIN {
      section = ""
      service_count = 0
      memory_high = 0
      memory_max = 0
      invalid = 0
    }
    /^[[:space:]]*($|#|;)/ { next }
    /^\[Service\][[:space:]]*$/ {
      service_count += 1
      section = "Service"
      next
    }
    /^\[/ { invalid = 1; exit }
    section != "Service" { invalid = 1; exit }
    /^[[:space:]]*MemoryHigh=[^[:space:]].*$/ { memory_high += 1; next }
    /^[[:space:]]*MemoryMax=[^[:space:]].*$/ { memory_max += 1; next }
    { invalid = 1; exit }
    END {
      exit !(invalid == 0 && service_count == 1 && memory_high == 1 && memory_max == 1)
    }
  ' "$path" >/dev/null \
    || fail "production USD-M drop-in must contain only [Service], MemoryHigh, and MemoryMax: $path"
}

atomic_install() {
  local mode=$1 source=$2 destination=$3 temporary
  temporary="${destination}.new.$$"
  install -m "$mode" "$source" "$temporary" || return 1
  mv -Tf "$temporary" "$destination" || return 1
  cmp -s -- "$source" "$destination"
}

install_recovery_deployment() {
  local directory=$1
  if [[ -f $directory/binance-lob-archiver-recovery@.service ]]; then
    atomic_install 0644 "$directory/binance-lob-archiver-recovery@.service" \
      /etc/systemd/system/binance-lob-archiver-recovery@.service || return 1
    atomic_install 0644 "$directory/binance-lob-archiver-recovery@.timer" \
      /etc/systemd/system/binance-lob-archiver-recovery@.timer || return 1
    atomic_install 0755 "$directory/host-rust-lob-recovery-queue.sh" \
      /opt/monday/bin/monday-rust-lob-recovery-queue || return 1
  fi
}

install_deployment() {
  local directory=$1
  install -d -m 0755 /etc/systemd/system || return 1
  install -d -m 0755 /etc/monday || return 1
  atomic_install 0644 "$directory/binance-lob-archiver-production@.service" \
    /etc/systemd/system/binance-lob-archiver-production@.service || return 1
  atomic_install 0644 "$directory/binance-lob-archiver-upload@.service" \
    /etc/systemd/system/binance-lob-archiver-upload@.service || return 1
  install_recovery_deployment "$directory" || return 1
  atomic_install 0640 "$directory/binance-lob-archiver-production-spot.env" \
    /etc/monday/binance-lob-archiver-production-spot.env || return 1
  atomic_install 0640 "$directory/binance-lob-archiver-production-usdm.env" \
    /etc/monday/binance-lob-archiver-production-usdm.env || return 1
}

atomic_symlink() {
  local target=$1 link=$2 temporary resolved_target
  resolved_target=$(readlink -f -- "$target" 2>/dev/null) || return 1
  temporary="${link}.new.$$"
  rm -f "$temporary" || return 1
  ln -s "$target" "$temporary" || return 1
  mv -Tf "$temporary" "$link" || return 1
  [[ -L $link && $(readlink -f -- "$link" 2>/dev/null || true) == "$resolved_target" ]]
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

has_incomplete_segment_artifacts() {
  local spool=$1
  find "$spool" -type f \( \
    -name '*.jsonl.part' -o \
    -name '*.zst.tmp' -o \
    -name '*.part.corrupt' \
  \) -print -quit | grep -q .
}

run_candidate_drain() {
  local deployment=$1 market env_file key value
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
    if has_incomplete_segment_artifacts "$CANONICAL_SPOOL/$market"; then
      /opt/monday/bin/monday-rust-lob-recovery-queue isolate "$market" || return 1
      DRAIN_MAY_HAVE_MUTATED=1
      continue
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
  local existing_base=0 existing_recovery=0 installed_recovery=0
  local asset source installed_source mode source_kind old_usdm_symbols
  local -a rollback_assets
  local release_deployment=$OLD_DEPLOYMENT
  local snapshot="$EVIDENCE_DIR/rollback-deployment"
  local manifest="$EVIDENCE_DIR/rollback-deployment.sha256"
  [[ ! -L $OLD_DEPLOYMENT ]] || fail "old staged deployment is a symlink: $OLD_DEPLOYMENT"
  for asset in "${BASE_DEPLOYMENT_ASSETS[@]}"; do
    if [[ -e $release_deployment/$asset ]]; then
      ((existing_base += 1))
    fi
  done
  for asset in "${RECOVERY_DEPLOYMENT_ASSETS[@]}"; do
    if [[ -e $release_deployment/$asset ]]; then
      ((existing_recovery += 1))
    fi
  done
  if (( existing_base == ${#BASE_DEPLOYMENT_ASSETS[@]} )); then
    (( existing_recovery == 0 \
      || existing_recovery == ${#RECOVERY_DEPLOYMENT_ASSETS[@]} )) \
      || fail "old release has partial recovery assets: $release_deployment"
    validate_deployment "$release_deployment" false
    source_kind=release
    rollback_assets=("${BASE_DEPLOYMENT_ASSETS[@]}")
    if (( existing_recovery )); then
      rollback_assets+=("${RECOVERY_DEPLOYMENT_ASSETS[@]}")
    fi
  elif (( existing_base == 0 && existing_recovery == 0 )); then
    source_kind=installed
    rollback_assets=("${BASE_DEPLOYMENT_ASSETS[@]}")
    for asset in "${RECOVERY_DEPLOYMENT_ASSETS[@]}"; do
      case "$asset" in
        *.service|*.timer) installed_source="/etc/systemd/system/$asset" ;;
        host-rust-lob-recovery-queue.sh)
          installed_source=/opt/monday/bin/monday-rust-lob-recovery-queue ;;
      esac
      [[ -e $installed_source ]] && ((installed_recovery += 1))
    done
    (( installed_recovery == 0 \
      || installed_recovery == ${#RECOVERY_DEPLOYMENT_ASSETS[@]} )) \
      || fail 'installed production has partial recovery assets'
    if (( installed_recovery )); then
      rollback_assets+=("${RECOVERY_DEPLOYMENT_ASSETS[@]}")
    fi
  else
    fail "old release has a partial staged deployment: $release_deployment"
  fi

  [[ ! -e $snapshot && ! -L $snapshot ]] \
    || fail "rollback evidence snapshot already exists: $snapshot"
  install -d -m 0750 "$snapshot"
  for asset in "${rollback_assets[@]}"; do
    case "$asset" in
      *.service|*.timer) installed_source="/etc/systemd/system/$asset"; mode=0644 ;;
      *.env) installed_source="/etc/monday/$asset"; mode=0640 ;;
      host-rust-lob-recovery-queue.sh)
        installed_source=/opt/monday/bin/monday-rust-lob-recovery-queue
        mode=0755
        ;;
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
    sha256sum "${rollback_assets[@]}"
  ) >"$manifest"
  chmod 0640 "$manifest"
  ROLLBACK_DEPLOYMENT_MANIFEST_SHA256=$(sha256sum "$manifest" | awk '{print $1}')
  OLD_DEPLOYMENT=$snapshot
}

capture_existing_production_identity() {
  local timer
  OLD_MODE=$1
  [[ -L $PRODUCTION_LINK ]] || fail 'running production binary must be a release symlink'
  OLD_BINARY=$(readlink -f "$PRODUCTION_LINK")
  [[ $OLD_BINARY =~ ^$RELEASE_ROOT/([a-f0-9]{64})/binance-lob-archiver$ ]] \
    || fail "running production symlink is not digest-addressed: $OLD_BINARY"
  OLD_SHA256=${BASH_REMATCH[1]}
  OLD_RECOVERY_TIMERS_ENABLED=0
  for timer in "${RECOVERY_TIMERS[@]}"; do
    if systemctl is-enabled --quiet "$timer" 2>/dev/null; then
      ((OLD_RECOVERY_TIMERS_ENABLED += 1))
    fi
  done
  (( OLD_RECOVERY_TIMERS_ENABLED == 0 \
    || OLD_RECOVERY_TIMERS_ENABLED == ${#RECOVERY_TIMERS[@]} )) \
    || fail 'old production has partially enabled recovery timers'
  [[ $OLD_SHA256 != "$CANDIDATE_SHA256" ]] || fail 'candidate is already the production release'
  printf '%s  %s\n' "$OLD_SHA256" "$OLD_BINARY" | sha256sum --check --strict
  if [[ $OLD_MODE == partial-contained-spot-live ]]; then
    OLD_SPOT_RESTARTS=$(systemctl_value "${PRODUCTION_UNITS[0]}" NRestarts)
    [[ $OLD_SPOT_RESTARTS =~ ^[0-9]+$ ]] \
      || fail 'running Spot production has an invalid restart baseline'
    OLD_SPOT_INVOCATION_ID=$(systemctl_value "${PRODUCTION_UNITS[0]}" InvocationID)
    [[ $OLD_SPOT_INVOCATION_ID =~ ^[A-Fa-f0-9]{32}$ ]] \
      || fail 'running Spot production has an invalid invocation baseline'
  fi
  OLD_DEPLOYMENT="$RELEASE_ROOT/$OLD_SHA256/deployment"
  stage_existing_deployment_for_rollback
  SPOOL_ENV_DEPLOYMENT="$OLD_DEPLOYMENT"
}

capture_allowlisted_production_usdm_dropin() {
  local dropin_dir
  secure_regular_file "$PRODUCTION_USDM_MEMORY_DROPIN"
  validate_memory_only_dropin "$PRODUCTION_USDM_MEMORY_DROPIN"
  dropin_dir=${PRODUCTION_USDM_MEMORY_DROPIN%/*}
  secure_directory "$dropin_dir"
  PRODUCTION_USDM_MEMORY_DROPIN_PRESENT=1
  PRODUCTION_USDM_MEMORY_DROPIN_BACKUP="$EVIDENCE_DIR/binance-lob-archiver-production-usdm-10-memory.conf"
  PRODUCTION_USDM_MEMORY_DROPIN_MANIFEST="$EVIDENCE_DIR/binance-lob-archiver-production-usdm-10-memory.conf.sha256"
  atomic_install 0640 \
    "$PRODUCTION_USDM_MEMORY_DROPIN" "$PRODUCTION_USDM_MEMORY_DROPIN_BACKUP"
  cmp -s -- "$PRODUCTION_USDM_MEMORY_DROPIN" "$PRODUCTION_USDM_MEMORY_DROPIN_BACKUP" \
    || fail 'production USD-M memory drop-in backup does not match the installed bytes'
  PRODUCTION_USDM_MEMORY_DROPIN_SHA256=$(
    sha256sum "$PRODUCTION_USDM_MEMORY_DROPIN_BACKUP" | awk '{print $1}'
  )
  printf '%s  %s\n' \
    "$PRODUCTION_USDM_MEMORY_DROPIN_SHA256" \
    "$PRODUCTION_USDM_MEMORY_DROPIN_BACKUP" >"$PRODUCTION_USDM_MEMORY_DROPIN_MANIFEST"
  chmod 0640 "$PRODUCTION_USDM_MEMORY_DROPIN_MANIFEST"
}

validate_existing_production_dropins() {
  local dropins
  dropins=$(systemctl_value "${PRODUCTION_UNITS[0]}" DropInPaths) \
    || fail "could not read production drop-ins for ${PRODUCTION_UNITS[0]}"
  [[ -z $dropins ]] \
    || fail "spot production service has an unexpected systemd drop-in: $dropins"

  dropins=$(systemctl_value "${PRODUCTION_UNITS[1]}" DropInPaths) \
    || fail "could not read production drop-ins for ${PRODUCTION_UNITS[1]}"
  if [[ -z $dropins ]]; then
    return 0
  fi
  [[ $dropins == "$PRODUCTION_USDM_MEMORY_DROPIN" ]] \
    || fail "USD-M production service has an unexpected systemd drop-in: $dropins"
  capture_allowlisted_production_usdm_dropin
}

remove_allowlisted_production_dropins_for_candidate() {
  [[ $PRODUCTION_USDM_MEMORY_DROPIN_PRESENT -eq 1 ]] || return 0
  sha256sum --check --strict "$PRODUCTION_USDM_MEMORY_DROPIN_MANIFEST" >/dev/null \
    || fail 'production USD-M memory drop-in backup digest changed before candidate cutover'
  rm -f -- "$PRODUCTION_USDM_MEMORY_DROPIN" \
    || fail 'could not remove the allowlisted production USD-M memory drop-in'
  [[ ! -e $PRODUCTION_USDM_MEMORY_DROPIN && ! -L $PRODUCTION_USDM_MEMORY_DROPIN ]] \
    || fail 'production USD-M memory drop-in remained present after removal'
}

restore_allowlisted_production_dropins() {
  [[ $PRODUCTION_USDM_MEMORY_DROPIN_PRESENT -eq 1 ]] || return 0
  sha256sum --check --strict "$PRODUCTION_USDM_MEMORY_DROPIN_MANIFEST" >/dev/null \
    || return 1
  atomic_install 0644 \
    "$PRODUCTION_USDM_MEMORY_DROPIN_BACKUP" "$PRODUCTION_USDM_MEMORY_DROPIN" \
    || return 1
  cmp -s -- "$PRODUCTION_USDM_MEMORY_DROPIN_BACKUP" "$PRODUCTION_USDM_MEMORY_DROPIN"
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

unit_matches_release() {
  local unit=$1 binary=$2 require_enabled=$3 expected_restarts=${4:-0}
  local expected_invocation_id=${5:-} restarts invocation_id main_pid main_exe
  systemctl is-active --quiet "$unit" || return 1
  restarts=$(systemctl show "$unit" --property=NRestarts --value) || return 1
  [[ $restarts == "$expected_restarts" ]] || return 1
  if [[ -n $expected_invocation_id ]]; then
    invocation_id=$(systemctl show "$unit" --property=InvocationID --value) || return 1
    [[ $invocation_id == "$expected_invocation_id" ]] || return 1
  fi
  main_pid=$(systemctl show "$unit" --property=MainPID --value) || return 1
  [[ $main_pid =~ ^[1-9][0-9]*$ ]] || return 1
  main_exe=$(readlink -f "/proc/$main_pid/exe" 2>/dev/null || true)
  [[ $main_exe == "$binary" ]] || return 1
  if [[ $require_enabled == true ]]; then
    systemctl is-enabled --quiet "$unit" || return 1
  fi
}

runtime_matches_release() {
  local binary=$1 require_enabled=$2 unit
  for unit in "${PRODUCTION_UNITS[@]}"; do
    unit_matches_release "$unit" "$binary" "$require_enabled" || return 1
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

wait_for_spot_release_health() {
  local binary=$1 old_session=$2 minimum_updated_ns=$3 deadline
  deadline=$((SECONDS + HEALTH_TIMEOUT_SECONDS))
  while (( SECONDS < deadline )); do
    systemctl is-active --quiet "${PRODUCTION_UNITS[0]}" || return 1
    if health_ready_for_release spot 1000 "$old_session" "$minimum_updated_ns" \
      && unit_matches_release "${PRODUCTION_UNITS[0]}" "$binary" false; then
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
  for unit in "${RECOVERY_TIMERS[@]}"; do
    systemctl is-active --quiet "$unit" && return 1
    state=$(systemctl is-enabled "$unit" 2>/dev/null || true)
    [[ $state == disabled || $state == masked || $state == masked-runtime \
      || $state == not-found ]] || return 1
  done
  for unit in "${RECOVERY_UNITS[@]}"; do
    systemctl is-active --quiet "$unit" && return 1
  done
  return 0
}

partial_spot_runtime_is_restored() {
  local binary=$1 unit state
  unit_matches_release "${PRODUCTION_UNITS[0]}" "$binary" true || return 1
  systemctl is-active --quiet "${UPLOAD_UNITS[0]}" && return 1
  state=$(systemctl is-enabled "${UPLOAD_UNITS[0]}" 2>/dev/null || true)
  [[ $state == static ]] || return 1
  for unit in "${PRODUCTION_UNITS[1]}" "${UPLOAD_UNITS[1]}" "${LEGACY_UNITS[@]}"; do
    systemctl is-active --quiet "$unit" && return 1
    state=$(systemctl is-enabled "$unit" 2>/dev/null || true)
    [[ $state == masked || $state == masked-runtime ]] || return 1
  done
  [[ $(systemctl show "${PRODUCTION_UNITS[1]}" --property=MainPID --value) == 0 ]] \
    || return 1
  for unit in "${RECOVERY_UNITS[@]}"; do
    systemctl is-active --quiet "$unit" && return 1
  done
  return 0
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
    --arg deployment_source_revision "$DEPLOYMENT_SOURCE_REVISION" \
    --arg deployment_bundle_sha256 "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg previous_sha256 "$OLD_SHA256" \
    --arg previous_spot_restarts "$OLD_SPOT_RESTARTS" \
    --arg previous_spot_invocation_id "$OLD_SPOT_INVOCATION_ID" \
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
      deployment_source_revision:
        (if $deployment_source_revision == "" then null else $deployment_source_revision end),
      deployment_bundle_sha256: (if $deployment_bundle_sha256 == "" then null else $deployment_bundle_sha256 end),
      previous_sha256: (if $previous_sha256 == "" then null else $previous_sha256 end),
      previous_spot_restarts:
        (if $previous_spot_restarts == "" then null
         else ($previous_spot_restarts | tonumber) end),
      previous_spot_invocation_id:
        (if $previous_spot_invocation_id == "" then null
         else $previous_spot_invocation_id end),
      host_mode: $mode,
      current_binary: (if $current_binary == "" then null else $current_binary end),
      production_units_active: {spot: $spot_active, usdm: $usdm_active}
    }' > "$temporary" || return 1
  chmod 0640 "$temporary" || return 1
  mv -Tf "$temporary" "$EVIDENCE_DIR/cutover.json" || return 1
}

rollback_after_failure() {
  local safe_to_restart=1 safe_to_restore=1 partial_restored=0 unit rollback_started_ns=0
  ROLLBACK_RESULT=disabled
  systemctl disable --now "${RECOVERY_TIMERS[@]}" >/dev/null 2>&1 || true
  systemctl stop "${RECOVERY_UNITS[@]}" >/dev/null 2>&1 || true
  systemctl disable --now "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
  systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}" >/dev/null 2>&1 || true
  production_is_fail_closed || safe_to_restart=0
  if (( safe_to_restart == 0 )); then
    if production_is_fail_closed; then
      ROLLBACK_RESULT=production-stop-or-disable-failed-but-contained
    else
      ROLLBACK_RESULT=production-stop-or-disable-containment-failed
    fi
    copy_health_evidence rollback
    return
  fi

  if [[ -d $CANONICAL_SPOOL && $DRAIN_REQUIRED -eq 1 \
    && ( $OLD_MODE == upgrade || $OLD_MODE == partial-contained-spot-live ) ]]; then
    if [[ -z $SPOOL_ENV_DEPLOYMENT \
      || ( $DRAIN_ATTEMPTED -eq 1 && $DRAIN_MAY_HAVE_MUTATED -eq 0 ) ]]; then
      safe_to_restart=0
    else
      DRAIN_ATTEMPTED=1
      if run_candidate_drain "$SPOOL_ENV_DEPLOYMENT"; then
        DRAIN_REQUIRED=0
      else
        safe_to_restart=0
      fi
    fi
  fi

  if [[ $OLD_MODE == upgrade || $OLD_MODE == contained-upgrade \
    || $OLD_MODE == partial-contained-spot-live ]]; then
    if [[ -n $ROLLBACK_DEPLOYMENT_MANIFEST_SHA256 ]]; then
      printf '%s  %s\n' "$ROLLBACK_DEPLOYMENT_MANIFEST_SHA256" \
        "$EVIDENCE_DIR/rollback-deployment.sha256" | sha256sum --check --strict \
        || safe_to_restore=0
      (
        cd "$OLD_DEPLOYMENT"
        sha256sum --check --strict "$EVIDENCE_DIR/rollback-deployment.sha256"
      ) || safe_to_restore=0
    else
      safe_to_restore=0
    fi
    if (( safe_to_restore == 0 )); then
      safe_to_restart=0
      ROLLBACK_RESULT=rollback-evidence-unverified-disabled
    elif ! install_deployment "$OLD_DEPLOYMENT"; then
      safe_to_restart=0
      ROLLBACK_RESULT=restore-assets-failed-disabled
    elif ! atomic_symlink "$OLD_BINARY" "$PRODUCTION_LINK"; then
      safe_to_restart=0
      ROLLBACK_RESULT=restore-symlink-failed-disabled
    elif ! restore_allowlisted_production_dropins; then
      safe_to_restart=0
      ROLLBACK_RESULT=restore-dropin-failed-disabled
    elif ! systemctl daemon-reload; then
      safe_to_restart=0
      ROLLBACK_RESULT=daemon-reload-failed-disabled
    elif [[ $OLD_MODE == contained-upgrade ]]; then
      if production_is_fail_closed; then
        ROLLBACK_RESULT=previous-release-restored-contained
      else
        safe_to_restart=0
        ROLLBACK_RESULT=previous-release-restore-containment-failed
      fi
    elif [[ $OLD_MODE == partial-contained-spot-live ]]; then
      if ! systemctl unmask --runtime "${PRODUCTION_UNITS[0]}" >/dev/null; then
        safe_to_restart=0
        ROLLBACK_RESULT=previous-spot-unmask-failed-disabled
      fi
    elif (( safe_to_restart \
      && OLD_RECOVERY_TIMERS_ENABLED == ${#RECOVERY_TIMERS[@]} )) \
      && ! systemctl enable --now "${RECOVERY_TIMERS[@]}" >/dev/null; then
      safe_to_restart=0
      ROLLBACK_RESULT=recovery-timer-restore-failed-disabled
    elif (( safe_to_restart )); then
      systemctl unmask --runtime "${PRODUCTION_UNITS[@]}" >/dev/null \
        || safe_to_restart=0
    fi

    if [[ $OLD_MODE == upgrade || $OLD_MODE == partial-contained-spot-live ]] \
      && (( safe_to_restart )); then
      copy_health_evidence failed-candidate
      if ! clear_health_before_restart; then
        safe_to_restart=0
        ROLLBACK_RESULT=stale-health-clear-failed-disabled
      else
        rollback_started_ns=$(date +%s%N)
      fi
    fi

    if [[ $OLD_MODE == upgrade ]] && (( safe_to_restart )); then
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
    elif [[ $OLD_MODE == partial-contained-spot-live ]] && (( safe_to_restart )); then
      partial_restored=1
      systemctl reset-failed "${PRODUCTION_UNITS[0]}" >/dev/null 2>&1 || true
      if ! systemctl start "${PRODUCTION_UNITS[0]}" \
        || ! wait_for_spot_release_health \
          "$OLD_BINARY" "$OLD_SESSION_SPOT" "$rollback_started_ns" \
        || ! systemctl enable "${PRODUCTION_UNITS[0]}" >/dev/null \
        || ! unit_matches_release "${PRODUCTION_UNITS[0]}" "$OLD_BINARY" true \
        || ! health_ready_for_release spot 1000 "$OLD_SESSION_SPOT" "$rollback_started_ns"; then
        partial_restored=0
      fi
      if (( partial_restored )) \
        && ! systemctl unmask --runtime "${UPLOAD_UNITS[0]}" >/dev/null; then
        partial_restored=0
      fi
      if (( partial_restored \
        && OLD_RECOVERY_TIMERS_ENABLED == ${#RECOVERY_TIMERS[@]} )) \
        && ! systemctl enable --now "${RECOVERY_TIMERS[@]}" >/dev/null; then
        partial_restored=0
      fi
      if (( partial_restored )) \
        && partial_spot_runtime_is_restored "$OLD_BINARY"; then
        ROLLBACK_RESULT=previous-spot-restored-usdm-contained
      else
        systemctl disable --now "${RECOVERY_TIMERS[@]}" >/dev/null 2>&1 || true
        systemctl disable --now "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
        systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}" >/dev/null 2>&1 || true
        if production_is_fail_closed; then
          ROLLBACK_RESULT=previous-spot-health-unverified-disabled
        else
          ROLLBACK_RESULT=previous-spot-restore-containment-failed
        fi
      fi
    elif (( safe_to_restart == 0 )); then
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
validate_existing_production_dropins
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
spot_active_state=$(systemctl_value "${PRODUCTION_UNITS[0]}" ActiveState)
usdm_active_state=$(systemctl_value "${PRODUCTION_UNITS[1]}" ActiveState)
spot_enabled_state=$(systemctl is-enabled "${PRODUCTION_UNITS[0]}" 2>/dev/null || true)
usdm_enabled_state=$(systemctl is-enabled "${PRODUCTION_UNITS[1]}" 2>/dev/null || true)
spot_upload_enabled_state=$(systemctl is-enabled "${UPLOAD_UNITS[0]}" 2>/dev/null || true)
usdm_upload_enabled_state=$(systemctl is-enabled "${UPLOAD_UNITS[1]}" 2>/dev/null || true)
for unit in "${PRODUCTION_UNITS[@]}"; do
  if systemctl is-active --quiet "$unit"; then
    ((active_count += 1))
  fi
  if systemctl is-enabled --quiet "$unit"; then
    ((enabled_count += 1))
  fi
done

if (( active_count == 2 && enabled_count == 2 )); then
  capture_existing_production_identity upgrade
  DRAIN_REQUIRED=1
elif (( active_count == 0 && enabled_count == 2 )) && [[ -L $PRODUCTION_LINK ]]; then
  capture_existing_production_identity contained-upgrade
  DRAIN_REQUIRED=1
elif (( active_count == 1 && enabled_count == 1 )) \
  && [[ -L $PRODUCTION_LINK \
    && $spot_active_state == active && $spot_enabled_state == enabled \
    && $usdm_active_state == inactive \
    && ( $usdm_enabled_state == masked || $usdm_enabled_state == masked-runtime ) \
    && ( $spot_upload_enabled_state == static \
      || $spot_upload_enabled_state == masked-runtime ) \
    && ( $usdm_upload_enabled_state == masked \
      || $usdm_upload_enabled_state == masked-runtime \
      || $usdm_upload_enabled_state == static ) ]]; then
  [[ $(systemctl_value "${PRODUCTION_UNITS[1]}" SubState) == dead \
    && $(systemctl_value "${PRODUCTION_UNITS[1]}" MainPID) == 0 ]] \
    || fail 'contained USD-M production is not inactive/dead with MainPID=0'
  if [[ $usdm_upload_enabled_state == static ]]; then
    MASK_USDM_UPLOAD_FOR_TRANSITION=1
  fi
  capture_existing_production_identity partial-contained-spot-live
  unit_matches_release "${PRODUCTION_UNITS[0]}" "$OLD_BINARY" true \
    "$OLD_SPOT_RESTARTS" "$OLD_SPOT_INVOCATION_ID" \
    || fail 'running Spot production does not match the previous release identity'
  spot_health_min_updated_ns=$(( \
    $(date +%s%N) - HEALTH_TIMEOUT_SECONDS * 1000000000 \
  ))
  health_ready_for_release spot 1000 "" "$spot_health_min_updated_ns" \
    || fail 'running Spot production health is not ready for partial-contained cutover'
  DRAIN_REQUIRED=1
elif (( active_count == 0 && enabled_count == 0 )) && [[ ! -e $PRODUCTION_LINK && ! -L $PRODUCTION_LINK ]]; then
  OLD_MODE=new-host
  (( PRODUCTION_USDM_MEMORY_DROPIN_PRESENT == 0 )) \
    || fail 'new host must not retain a production USD-M drop-in'
  require_empty_segment_spool || fail 'new host canonical spool contains segment artifacts'
else
  fail "ambiguous production state: active=$active_count enabled=$enabled_count spot=$spot_active_state/$spot_enabled_state/$spot_upload_enabled_state usdm=$usdm_active_state/$usdm_enabled_state/$usdm_upload_enabled_state symlink=$PRODUCTION_LINK"
fi

TRANSITION_STARTED=1
if (( MASK_USDM_UPLOAD_FOR_TRANSITION )); then
  STEP=contain-usdm-uploader
  systemctl mask --runtime "${UPLOAD_UNITS[1]}" >/dev/null \
    || fail 'could not runtime-mask the contained USD-M uploader'
  usdm_upload_enabled_state=$(systemctl is-enabled "${UPLOAD_UNITS[1]}" 2>/dev/null || true)
  [[ $usdm_upload_enabled_state == masked \
    || $usdm_upload_enabled_state == masked-runtime ]] \
    || fail 'contained USD-M uploader did not become masked'
fi
STEP=stop-production
systemctl disable --now "${LEGACY_UNITS[@]}" >/dev/null 2>&1 || true
for unit in "${LEGACY_UNITS[@]}"; do
  systemctl is-active --quiet "$unit" && fail "legacy collector unit did not stop: $unit"
  systemctl is-enabled --quiet "$unit" && fail "legacy collector unit remained enabled: $unit"
done
if [[ $OLD_MODE == upgrade ]]; then
  systemctl disable --now "${PRODUCTION_UNITS[@]}"
elif [[ $OLD_MODE == partial-contained-spot-live ]]; then
  systemctl disable --now "${PRODUCTION_UNITS[0]}"
  systemctl disable "${PRODUCTION_UNITS[1]}" >/dev/null 2>&1 || true
else
  systemctl disable "${PRODUCTION_UNITS[@]}" >/dev/null 2>&1 || true
fi
for unit in "${PRODUCTION_UNITS[@]}"; do
  systemctl is-active --quiet "$unit" && fail "production unit did not stop: $unit"
  [[ $(systemctl show --property MainPID --value "$unit") == 0 ]] \
    || fail "production unit retained a MainPID after stop: $unit"
  systemctl is-enabled --quiet "$unit" \
    && fail "production unit remained enabled after disable: $unit"
done
systemctl mask --runtime "${TRANSITION_MASK_UNITS[@]}" >/dev/null
canonical_spool_paths_safe || fail 'canonical spool path changed during production stop'

STEP=stage-candidate-recovery-assets
validate_deployment "$CANDIDATE_DEPLOYMENT" true
install_recovery_deployment "$CANDIDATE_DEPLOYMENT"
systemctl daemon-reload

if [[ $OLD_MODE == upgrade || $OLD_MODE == contained-upgrade \
  || $OLD_MODE == partial-contained-spot-live ]]; then
  STEP=drain-old-production-with-candidate
  DRAIN_ATTEMPTED=1
  run_candidate_drain "$OLD_DEPLOYMENT"
  DRAIN_REQUIRED=0
  DRAIN_ATTEMPTED=0
  DRAIN_MAY_HAVE_MUTATED=0
else
  STEP=verify-new-host-spool
  require_empty_segment_spool || fail 'new host canonical spool contains segment artifacts'
fi

STEP=install-candidate-production-assets
install_deployment "$CANDIDATE_DEPLOYMENT"
remove_allowlisted_production_dropins_for_candidate
install -d -m 0750 -o hftcollector -g hftcollector \
  "$CANONICAL_SPOOL/spot" "$CANONICAL_SPOOL/usdm"
systemctl daemon-reload
for unit in "${PRODUCTION_UNITS[@]}"; do
  unit_dropins=$(systemctl_value "$unit" DropInPaths) \
    || fail "could not read production drop-ins for $unit after daemon-reload"
  [[ -z $unit_dropins ]] \
    || fail "candidate production service retained an unexpected systemd drop-in: $unit_dropins"
done

STEP=switch-production-symlink
atomic_symlink "$CANDIDATE_BINARY" "$PRODUCTION_LINK"
printf '%s  %s\n' "$CANDIDATE_SHA256" "$PRODUCTION_LINK" | sha256sum --check --strict

STEP=clear-stale-candidate-health
copy_health_evidence previous-production
clear_health_before_restart \
  || fail 'could not clear stale production health before starting the candidate'
CANDIDATE_STARTED_NS=$(date +%s%N)
SPOOL_ENV_DEPLOYMENT="$CANDIDATE_DEPLOYMENT"
DRAIN_REQUIRED=1
DRAIN_ATTEMPTED=0
DRAIN_MAY_HAVE_MUTATED=0

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

STEP=enable-recovery-timers
systemctl enable --now "${RECOVERY_TIMERS[@]}" >/dev/null
for timer in "${RECOVERY_TIMERS[@]}"; do
  systemctl is-enabled --quiet "$timer" \
    || fail "recovery timer did not enable: $timer"
  systemctl is-active --quiet "$timer" \
    || fail "recovery timer did not start: $timer"
done

STEP=write-cutover-evidence
RESULT=passed
ROLLBACK_RESULT=not-needed
write_evidence
SUCCESS=1
trap - EXIT ERR
printf 'Rust collector cutover passed: %s\nEvidence: %s/cutover.json\n' \
  "$CANDIDATE_SHA256" "$EVIDENCE_DIR"
