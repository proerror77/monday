#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

# V2 shadow Gate. Candidate controller C1 owns this script and its policy.
readonly REQUIRED_DURATION_SECONDS=240
readonly HEALTH_SETTLE_SECONDS=240
readonly MAX_HEALTH_SILENCE_SECONDS=120
readonly MAX_SEGMENT_GAP_NS=90000000000
readonly HOST_MEMORY_RESERVE_BYTES=1073741824
readonly PRODUCTION_MEMORY_GROWTH_MARGIN_BYTES=268435456
readonly STRICT_VERIFIER_MEMORY_MAX_BYTES=1610612736
readonly IO_PSI_WINDOW_SECONDS=15
readonly IO_PSI_WINDOW_US=15000000
readonly IO_PSI_FULL_DELTA_LIMIT_US=150000
readonly IO_PSI_CONSECUTIVE_HIT_LIMIT=3
readonly UPLOAD_DRAIN_MEMORY_MAX_BYTES=536870912
readonly SERVICE_USER=hftcollector
readonly SERVICE_HOME=/var/lib/hft-collector
readonly SAFE_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
readonly GATE_UNIT_PREFIX=monday-rust-lob-gate-
readonly -a SHADOW_ASSETS=(
  binance-lob-archiver-rust@.service
  binance-lob-archiver-rust-upload@.service
  binance-lob-archiver-rust-spot.env
  binance-lob-archiver-rust-usdm.env
)
readonly -a PRODUCTION_ASSETS=(
  binance-lob-archiver-production@.service
  binance-lob-archiver-upload@.service
  binance-lob-archiver-production-spot.env
  binance-lob-archiver-production-usdm.env
)

die() { printf 'shadow gate failed: %s\n' "$*"; exit 1; }
usage() {
  cat >&2 <<'EOF'
Usage: host-rust-lob-shadow-gate.sh --from-controller <direct|sha256> \
  --candidate-controller <sha256> [--root <fixture-root>]
EOF
}

ROOT=${MONDAY_ROOT:-/}; FROM_CONTROLLER=; CANDIDATE_CONTROLLER=
while (($#)); do
  case "$1" in
    --from-controller) (($# >= 2)) || { usage; exit 2; }; FROM_CONTROLLER=$2; shift 2 ;;
    --candidate-controller) (($# >= 2)) || { usage; exit 2; }; CANDIDATE_CONTROLLER=$2; shift 2 ;;
    --root) (($# >= 2)) || { usage; exit 2; }; ROOT=$2; shift 2 ;;
    --help|-h) usage >&1; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done
ROOT=${ROOT%/}; [[ -n $ROOT ]] || ROOT=/
FROM_CONTROLLER=$(printf '%s' "$FROM_CONTROLLER" | tr '[:upper:]' '[:lower:]')
CANDIDATE_CONTROLLER=$(printf '%s' "$CANDIDATE_CONTROLLER" | tr '[:upper:]' '[:lower:]')
[[ $FROM_CONTROLLER == direct || $FROM_CONTROLLER =~ ^[a-f0-9]{64}$ ]] ||
  die 'before controller must be direct or a 64-character SHA-256'
[[ $CANDIDATE_CONTROLLER =~ ^[a-f0-9]{64}$ ]] ||
  die 'candidate controller must be a 64-character SHA-256'
TEST_ONLY=false; [[ ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] && TEST_ONLY=true
[[ $TEST_ONLY == false || $ROOT != / ]] || die 'test mode requires an isolated fixture root'

SCRIPT_DIR=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
# shellcheck disable=SC1090,SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
monday_control_plane_validate_mode "$ROOT" "$TEST_ONLY" \
  || die 'production uses canonical root or fixture mode lacks an explicit sentinel'

GATE_DURATION_SECONDS=$REQUIRED_DURATION_SECONDS
HEALTH_SETTLE_DURATION_SECONDS=$HEALTH_SETTLE_SECONDS
resolve_test_duration() {
  local name value current formal
  for name in MONDAY_GATE_TEST_SECONDS MONDAY_TEST_HEALTH_SETTLE_SECONDS; do
    value=${!name:-}
    [[ -n $value ]] || continue
    [[ $TEST_ONLY == true && ${MONDAY_ALLOW_SHORT_GATE_FOR_TESTS:-0} == 1 ]] \
      || die "$name is only allowed for an explicitly authorised fixture Gate"
    [[ $value =~ ^[1-9][0-9]*$ ]] || die "$name must be a positive integer"
    if [[ $name == MONDAY_GATE_TEST_SECONDS ]]; then current=$value; formal=$REQUIRED_DURATION_SECONDS
    else current=$value; formal=$HEALTH_SETTLE_SECONDS; fi
    (( current < formal )) || die "$name must be shorter than the formal Gate contract"
    if [[ $name == MONDAY_GATE_TEST_SECONDS ]]; then GATE_DURATION_SECONDS=$current
    else HEALTH_SETTLE_DURATION_SECONDS=$current; fi
  done
}
resolve_test_duration

OPT_ROOT=$(monday_root_join "$ROOT" opt/monday); RELEASE_ROOT="$OPT_ROOT/releases/binance-lob-archiver"
CONTROLLER_ROOT="$OPT_ROOT/releases/binance-lob-controller"; BIN_ROOT="$OPT_ROOT/bin"
SYSTEMD_ROOT=$(monday_root_join "$ROOT" etc/systemd/system); CONFIG_ROOT=$(monday_root_join "$ROOT" etc/monday)
LOCK_FILE=$(monday_root_join "$ROOT" run/lock/monday-rust-lob-control-plane.lock)
OVERRIDE_ROOT=$(monday_root_join "$ROOT" run/monday)
DATA_ROOT=$(monday_root_join "$ROOT" data/monday); EVIDENCE_ROOT="$DATA_ROOT/evidence/shadow-gates"
# Gate writers are always run-scoped.  Nothing under /etc or the stable
# /opt/monday/bin projection is mutated by this operation.
RUN_SPOOL_ROOT="$DATA_ROOT/spool/binance-lob-rust-shadow/gate"; PROC_ROOT=$(monday_root_join "$ROOT" proc)
GATE_UNIT_ROOT="$OVERRIDE_ROOT/rust-lob-gate"; GATE_SYSTEMD_ROOT=$(monday_root_join "$ROOT" run/systemd/system); SHADOW_BINARY="$BIN_ROOT/binance-lob-archiver-shadow"
PSI_SOURCE="$PROC_ROOT/pressure/io"
PRODUCTION_BINARY="$BIN_ROOT/binance-lob-archiver"
LIB_SOURCE="$SCRIPT_DIR/rust-lob-control-plane-lib.sh"; POLICY_SOURCE="$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq"
[[ -f $LIB_SOURCE && -f $POLICY_SOURCE ]] || die 'V2 control-plane assets are missing'

# Hold the release lock before reading candidate, active, or production
# identities.  The fixture skips flock but still exercises the same ordering.
mkdir -p "$(dirname -- "$LOCK_FILE")"
if [[ $TEST_ONLY == true ]]; then
  true
else
  exec 9>"$LOCK_FILE"
  flock -n 9 || die 'another collector control-plane action is running'
fi

for command in awk bash chmod cmp cp date dirname find grep install jq mkdir mktemp mv readlink rm sed sha256sum sleep sort stat tr wc zstd; do
  command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done
if [[ $TEST_ONLY != true ]]; then
  for command in aliyun flock id mountpoint runuser systemctl systemd-analyze systemd-run; do
    command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
  done
  mountpoint -q "$(monday_root_join "$ROOT" data)" || die 'data filesystem must be a mount point'
  [[ -r "$PROC_ROOT/uptime" && -r $PSI_SOURCE ]] || die 'proc timing/PSI sources are unavailable'
  id "$SERVICE_USER" >/dev/null 2>&1 || die "missing service user: $SERVICE_USER"
fi

# The offline fixture supplies a tiny systemd double.  Production always uses
# the real systemctl binary; the double only models the state fields consumed
# by this action and cannot mutate a host unit.
if [[ $TEST_ONLY == true ]]; then
  declare -A fixture_unit_state=()
  systemctl() {
    local action=${1:-} unit_name=${2:-} property value
    case "$action" in
      start) fixture_unit_state[$unit_name]=active; return 0 ;;
      stop) fixture_unit_state[$unit_name]=inactive; return 0 ;;
      reset-failed|daemon-reload) return 0 ;;
      is-active)
        if [[ $2 == --quiet ]]; then unit_name=$3; else unit_name=$2; fi
        [[ ${fixture_unit_state[$unit_name]:-inactive} == active ]] && { [[ $2 == --quiet ]] || printf 'active\n'; return 0; }
        [[ $2 == --quiet ]] && return 3; printf 'inactive\n'; return 3 ;;
      show)
        unit_name=$2; property=${3#--property=}; property=${property#--property};
        if [[ $property == *=* ]]; then property=${property#*=}; fi
        case "$property" in
          ActiveState) value=${fixture_unit_state[$unit_name]:-inactive} ;;
          SubState) [[ ${fixture_unit_state[$unit_name]:-inactive} == active ]] && value=running || value=dead ;;
          NRestarts) [[ ${MONDAY_GATE_FIXTURE_FAIL_RESTART:-0} == 1 ]] && value=1 || value=0 ;;
          MainPID) value=$$ ;;
          MemoryCurrent) value=1048576 ;;
          MemoryPeak) value=1048576 ;;
          MemoryMax) value=2147483648 ;;
          MemoryHigh) value=1879048192 ;;
          CPUUsageNSec) value=1000000 ;;
          CPUQuotaPerSecUSec) value=800ms ;;
          DropInPaths) value= ;;
          OOMScoreAdjust) value=500 ;;
          *) value= ;;
        esac
        printf '%s\n' "$value"; return 0 ;;
      *) return 0 ;;
    esac
  }
  aliyun() {
    local tool=${1:-} action=${2:-} source target object
    [[ $tool == ossutil ]] || return 2
    shift 2
    case "$action" in
      ls)
        fixture_date=2026-08-28
        fixture_hour=05
        for object in "${spool_dir[$OSS_FIXTURE_MARKET]}"/*.manifest.json; do
          [[ -f $object ]] || continue
          printf 'oss://fixture/lake/raw/venue=binance/market=%s/dataset=%s/shard=all/date=%s/hour=%s/%s\n' \
            "$OSS_FIXTURE_MARKET" "${dataset[$OSS_FIXTURE_MARKET]}" "$fixture_date" "$fixture_hour" "${object##*/}"
          if [[ ${MONDAY_GATE_FIXTURE_EXTRA_NESTED:-0} == 1 ]]; then
            printf 'oss://fixture/lake/raw/venue=binance/market=%s/dataset=%s/shard=all/extra/date=%s/hour=%s/%s\n' \
              "$OSS_FIXTURE_MARKET" "${dataset[$OSS_FIXTURE_MARKET]}" "$fixture_date" "$fixture_hour" "${object##*/}"
          fi
        done
        ;;
      cp)
        source=${1:-}; target=${2:-}
        object=${source##*/}
        [[ $object != "$source" && -n $target ]] || return 2
        cp -p -- "${spool_dir[$OSS_FIXTURE_MARKET]}/$object" "$target"
        if [[ ${MONDAY_GATE_FIXTURE_TAMPER_OSS:-0} == 1 && $object == *.jsonl.zst ]]; then
          printf '\n' >>"$target"
        fi
        ;;
      *) return 2 ;;
    esac
  }
fi

direct_directory() { local path=$1; [[ -d $path && ! -L $path && $(readlink -f -- "$path") == "$path" ]]; }
direct_directory_or_absent() { local path=$1; [[ ! -e $path && ! -L $path ]] || direct_directory "$path"; }
regular_file() { [[ -f $1 && ! -L $1 ]]; }
directory_mode() {
  local path=$1 value
  if value=$(stat -c '%a' -- "$path" 2>/dev/null); then printf '%s\n' "$value"; else stat -f '%Lp' -- "$path"; fi
}
directory_owner_group() {
  local path=$1 value
  if value=$(stat -c '%U:%G' -- "$path" 2>/dev/null); then printf '%s\n' "$value"; else stat -f '%Su:%Sg' -- "$path"; fi
}
ensure_run_spool_dir() {
  local path=$1 owner_group
  direct_directory_or_absent "$path" || die "run spool parent is indirect: $path"
  if [[ $TEST_ONLY == true ]]; then
    install -d -m 0750 "$path"
  else
    install -d -o "$SERVICE_USER" -g "$SERVICE_USER" -m 0750 "$path"
  fi
  direct_directory "$path" || die "run spool parent is not a direct directory: $path"
  [[ $(directory_mode "$path") == 750 ]] || die "run spool parent mode is not 0750: $path"
  if [[ $TEST_ONLY != true ]]; then
    owner_group=$(directory_owner_group "$path")
    [[ $owner_group == "$SERVICE_USER:$SERVICE_USER" || $owner_group == "root:$SERVICE_USER" ]] \
      || die "run spool parent ownership is not safe: $path ($owner_group)"
  fi
}
secure_file() {
  local path=$1 mode owner; regular_file "$path" || die "required regular file is missing: $path"
  if [[ $TEST_ONLY != true ]]; then owner=$(stat -c %u -- "$path"); mode=$(stat -c %a -- "$path")
    [[ $owner == 0 ]] || die "required file is not root-owned: $path"
    (( (8#$mode & 022) == 0 )) || die "required file is writable by group/world: $path"; fi
}
sha256_file() { monday_sha256_file "$1"; }

for path in "$OPT_ROOT" "$RELEASE_ROOT" "$CONTROLLER_ROOT" "$BIN_ROOT" \
  "$SYSTEMD_ROOT" "$CONFIG_ROOT" "$DATA_ROOT" "$DATA_ROOT/spool" \
  "$DATA_ROOT/spool/binance-lob-rust-shadow"; do
  direct_directory_or_absent "$path" || die "control-plane path is indirect: $path"
done
direct_directory_or_absent "$(dirname -- "$LOCK_FILE")" || die 'control-plane lock path is indirect'
direct_directory_or_absent "$OVERRIDE_ROOT" || die 'shadow override path is indirect'
direct_directory_or_absent "$(monday_root_join "$ROOT" run/systemd)" || die 'systemd runtime path is indirect'
direct_directory_or_absent "$GATE_SYSTEMD_ROOT" || die 'systemd runtime unit path is indirect'

meminfo_bytes() {
  local field=$1 source="$PROC_ROOT/meminfo" value
  if [[ ! -f $source && $TEST_ONLY == true ]]; then case "$field" in
    MemTotal) printf '8589934592\n';; MemAvailable) printf '6442450944\n';; SwapTotal) printf '0\n';; esac; return; fi
  value=$(awk -v key="$field:" '$1 == key { count++; value=$2 } END { if (count != 1 || value !~ /^[0-9]+$/) exit 1; print value }' "$source") || return 1
  printf '%s\n' "$((value * 1024))"
}
monotonic_seconds() { if [[ $TEST_ONLY == true && ! -r "$PROC_ROOT/uptime" ]]; then printf '%s\n' "$(date +%s)"; else awk '{print int($1)}' "$PROC_ROOT/uptime"; fi; }
proc_starttime() {
  local pid=$1 stat_file
  [[ $pid =~ ^[1-9][0-9]*$ ]] || return 1
  stat_file="$PROC_ROOT/$pid/stat"
  [[ -r $stat_file ]] || return 1
  awk '{ if ($22 !~ /^[0-9]+$/) exit 1; print $22 }' "$stat_file"
}
io_total_us() { if [[ $TEST_ONLY == true && ! -f $PSI_SOURCE ]]; then printf '0\n'; else monday_io_full_psi_total_us "$PSI_SOURCE"; fi; }
systemctl_show() { systemctl show "$1" --property="$2" --value 2>/dev/null; }
systemctl_active() { systemctl is-active --quiet "$1"; }
env_value() {
  local file=$1 key=$2 count value; count=$(grep -c "^${key}=" "$file" || true)
  [[ $count -eq 1 ]] || die "$file must contain exactly one ${key}= entry"
  value=$(sed -n "s/^${key}=//p" "$file"); [[ -n $value ]] || die "$file has an empty $key"; printf '%s\n' "$value"
}
run_spool_dir() {
  local run_id=$1 market=$2
  [[ $run_id =~ ^[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*$ ]] || return 1
  [[ $market == spot || $market == usdm ]] || return 1
  printf '%s/%s/%s\n' "$RUN_SPOOL_ROOT" "$run_id" "$market"
}
is_usdm_top100() {
  local value=$1 unique; [[ $value =~ ^[A-Z0-9]+(,[A-Z0-9]+)*$ ]] || return 1
  local -a values; IFS=, read -r -a values <<<"$value"; (( ${#values[@]} == 100 )) || return 1
  unique=$(printf '%s\n' "${values[@]}" | sort -u | wc -l); ((unique == 100))
}

candidate_release="$CONTROLLER_ROOT/$CANDIDATE_CONTROLLER"; candidate_deployment="$candidate_release/deployment"
candidate_manifest="$candidate_release/release.json"
monday_verify_controller_release "$ROOT" "$CANDIDATE_CONTROLLER" || die 'candidate controller release is not an exact immutable V2 release'
[[ $TEST_ONLY == true || $(readlink -f -- "${BASH_SOURCE[0]}") == "$candidate_deployment/host-rust-lob-shadow-gate.sh" ]] || die 'Gate must execute from candidate controller bytes'
candidate_payload=$(monday_manifest_field "$candidate_manifest" artifact_sha256)
candidate_runtime=$(monday_manifest_field "$candidate_manifest" runtime_contract_sha256)
candidate_bundle=$(monday_manifest_field "$candidate_manifest" deployment_bundle_sha256)
candidate_source=$(monday_manifest_field "$candidate_manifest" deployment_source_revision)
candidate_payload_dir="$RELEASE_ROOT/$candidate_payload"; candidate_binary="$candidate_payload_dir/binance-lob-archiver"
secure_file "$candidate_binary"; [[ -x $candidate_binary && $(sha256_file "$candidate_binary") == "$candidate_payload" ]] || die 'candidate payload identity failed'
# The production unit/env pair is part of the Gate contract even though the
# Gate process itself runs only the isolated shadow pair.
candidate_production_runtime=$(monday_verify_production_runtime_assets \
  "$ROOT" "$candidate_deployment" "$candidate_payload") \
  || die 'candidate production runtime contract failed static verification'
candidate_control_bytes_sha=$(sha256_file "$candidate_release/deployment.sha256")
candidate_control_assets='{}'
while IFS= read -r control_asset; do
  [[ -n $control_asset ]] || continue
  control_sha=$(sha256_file "$candidate_deployment/$control_asset")
  candidate_control_assets=$(jq -cn --argjson values "$candidate_control_assets" \
    --arg asset "$control_asset" --arg sha "$control_sha" '$values + {($asset):$sha}')
done < <(monday_controller_assets)

active_before=direct; before_controller=; source_mode=stable; legacy_controller=; legacy_target=; legacy_payload=; legacy_runtime=; before_payload=; before_runtime=; before_bundle=; before_source=; before_deployment=; before_production_projection=
if [[ $FROM_CONTROLLER != direct ]]; then
  active_before=$(monday_active_controller_sha "$ROOT") || die 'requested before controller is not active'
  [[ $active_before == "$FROM_CONTROLLER" ]] || die 'active pair differs from requested before controller'
  before_controller=$FROM_CONTROLLER
  before_release="$CONTROLLER_ROOT/$FROM_CONTROLLER"; before_deployment="$before_release/deployment"
  monday_verify_controller_release "$ROOT" "$FROM_CONTROLLER" || die 'before controller release is invalid'
  monday_verify_controller_projections "$ROOT" "$FROM_CONTROLLER" || die 'before controller projections are not stable'
  before_payload=$(monday_manifest_field "$before_release/release.json" artifact_sha256)
  before_runtime=$(monday_manifest_field "$before_release/release.json" runtime_contract_sha256)
  before_bundle=$(monday_manifest_field "$before_release/release.json" deployment_bundle_sha256)
  before_source=$(monday_manifest_field "$before_release/release.json" deployment_source_revision)
  [[ -L $PRODUCTION_BINARY && $(readlink -- "$PRODUCTION_BINARY") == "$CONTROLLER_ROOT/active/binance-lob-archiver" ]] \
    || die 'production binary is not the stable active projection'
  before_production_projection="$CONTROLLER_ROOT/active/binance-lob-archiver"
else
  source_mode=direct
  [[ -L $CONTROLLER_ROOT/active ]] || die 'direct bootstrap requires an existing legacy active controller'
  legacy_target=$(readlink -f -- "$CONTROLLER_ROOT/active") || die 'legacy active controller is dangling'
  legacy_controller=${legacy_target##*/}
  [[ $legacy_target == "$CONTROLLER_ROOT/$legacy_controller" ]] \
    || die 'legacy active controller is not digest-addressed'
  monday_verify_legacy_controller_release "$ROOT" "$legacy_controller" "$PRODUCTION_BINARY" \
    || die 'direct bootstrap requires an immutable v1 active controller'
  before_controller=$legacy_controller
  before_release="$legacy_target"
  # Never source or execute the legacy deployment.  C0 contributes only its
  # immutable manifest identity; all candidate control bytes come from C1.
  before_deployment=$candidate_deployment
  production_target=$(readlink -f -- "$PRODUCTION_BINARY") || die 'direct bootstrap requires a production binary'
  before_payload=$(sha256_file "$production_target") || die 'direct bootstrap requires a production binary'
  legacy_payload=$(jq -er '.artifact_sha256' "$legacy_target/release.json") || die 'legacy controller payload is invalid'
  legacy_runtime=$(jq -er '.runtime_contract_sha256' "$legacy_target/release.json") || die 'legacy controller runtime is invalid'
  [[ $before_payload == "$legacy_payload" ]] || die 'direct production does not match the legacy controller payload'
  [[ $before_payload == "$candidate_payload" ]] || die 'direct bootstrap requires P0 equal to P1'
  before_runtime=$legacy_runtime
  before_bundle=$(jq -er '.deployment_bundle_sha256' "$legacy_target/release.json") || die 'legacy controller bundle is invalid'
  before_source=$(jq -er '.deployment_source_revision' "$legacy_target/release.json") || die 'legacy controller source is invalid'
  [[ $candidate_runtime == "$before_runtime" ]] || die 'direct bootstrap requires R0 equal to R1'
  [[ -L $PRODUCTION_BINARY && $(readlink -f -- "$PRODUCTION_BINARY") == "$production_target" ]] \
    || die 'direct production identity differs'
  before_production_projection=$(readlink -- "$PRODUCTION_BINARY")
fi

# The before runtime contract is established from every live unit/env byte,
# never from the candidate manifest.  This is especially important for direct
# bootstrap: C0's R0 must be true of the installed P0 topology before any
# shadow staging occurs.
live_runtime=$(monday_rust_lob_live_runtime_contract_sha256 "$ROOT") \
  || die 'before runtime contract is missing or indirect'
[[ $live_runtime == "$before_runtime" ]] \
  || die 'before runtime bytes differ from the immutable before controller'

production_asset_json='{}'
for asset in "${PRODUCTION_ASSETS[@]}"; do
  if [[ $asset == *.service ]]; then production_target="$SYSTEMD_ROOT/$asset"; else production_target="$CONFIG_ROOT/$asset"; fi
  if [[ -L $production_target ]]; then
    [[ $(readlink -- "$production_target") == "$CONTROLLER_ROOT/active/deployment/$asset" ]] \
      || die "installed production asset is not the stable projection: $asset"
    production_resolved=$(readlink -f -- "$production_target") \
      || die "installed production asset projection is dangling: $asset"
  else
    production_resolved=$production_target
  fi
  regular_file "$production_resolved" || die "installed production asset is missing: $production_target"
  cmp -s "$before_deployment/$asset" "$production_resolved" \
    || die "installed production asset differs from before controller: $asset"
  production_asset_json=$(jq -cn --argjson values "$production_asset_json" --arg asset "$asset" --arg sha "$(sha256_file "$production_resolved")" '$values + {($asset):$sha}')
done

declare -A installed_asset saved_state saved_sha saved_target
declare -A candidate_asset_sha restored_asset_sha
tmp_dir=$(mktemp -d)
for asset in "${SHADOW_ASSETS[@]}"; do
  if [[ $asset == *.service ]]; then installed_asset[$asset]="$SYSTEMD_ROOT/$asset"
  else installed_asset[$asset]="$CONFIG_ROOT/$asset"; fi
  if regular_file "${installed_asset[$asset]}"; then
    saved_state[$asset]=present; saved_sha[$asset]=$(sha256_file "${installed_asset[$asset]}")
  elif [[ -L ${installed_asset[$asset]} ]]; then
    saved_target[$asset]=$(readlink -- "${installed_asset[$asset]}") || die "shadow projection is unreadable: $asset"
    [[ ${saved_target[$asset]} == "$CONTROLLER_ROOT/active/deployment/$asset" ]] \
      || die "shadow asset path is not the stable projection: $asset"
    saved_resolved=$(readlink -f -- "${installed_asset[$asset]}") || die "shadow projection is dangling: $asset"
    regular_file "$saved_resolved" || die "shadow projection target is not a file: $asset"
    saved_state[$asset]=projection; saved_sha[$asset]=$(sha256_file "$saved_resolved")
  else saved_state[$asset]=absent; saved_sha[$asset]=; fi
done
old_shadow_target=; old_shadow_target_sha256=; old_shadow_present=false
if [[ -L $SHADOW_BINARY ]]; then old_shadow_target=$(readlink -- "$SHADOW_BINARY"); old_shadow_present=true
elif [[ -e $SHADOW_BINARY ]]; then die 'shadow binary path is not a symlink'; fi
if [[ $old_shadow_present == true ]]; then
  old_shadow_target_resolved=$(readlink -f -- "$SHADOW_BINARY") || die 'existing shadow binary link is dangling'
  secure_file "$old_shadow_target_resolved"
  old_shadow_target_sha256=$(sha256_file "$old_shadow_target_resolved")
fi

# Candidate shadow units are rendered into a run-scoped /run directory.  The
# source template may only contribute the reviewed security/resource fields;
# all identity, spool, restart, and lifetime fields are rewritten below.
verify_shadow_unit_template() {
  local file=$1
  monday_file_direct "$file" || return 1
  monday_validate_unit_allowlist "$file" shadow || return 1
  monday_unit_exact_line "$file" Type simple || return 1
  monday_unit_exact_line "$file" User hftcollector || return 1
  monday_unit_exact_line "$file" Group hftcollector || return 1
  monday_unit_exact_line "$file" Restart always || return 1
  monday_unit_exact_line "$file" RestartSec 5 || return 1
  monday_unit_exact_line "$file" RuntimeMaxSec 21600 || return 1
  monday_unit_exact_line "$file" KillMode mixed || return 1
  monday_unit_exact_line "$file" TimeoutStopSec 600 || return 1
  monday_unit_exact_line "$file" NoNewPrivileges true || return 1
  monday_unit_exact_line "$file" PrivateTmp true || return 1
  monday_unit_exact_line "$file" ProtectSystem strict || return 1
  monday_unit_exact_line "$file" ProtectHome true || return 1
  monday_unit_exact_line "$file" ProtectKernelTunables true || return 1
  monday_unit_exact_line "$file" ProtectKernelModules true || return 1
  monday_unit_exact_line "$file" ProtectControlGroups true || return 1
  monday_unit_exact_line "$file" LockPersonality true || return 1
  monday_unit_exact_line "$file" RestrictSUIDSGID true || return 1
  monday_unit_exact_line "$file" StateDirectory hft-collector || return 1
  monday_unit_exact_line "$file" ReadWritePaths /data/monday/spool/binance-lob-rust-shadow || return 1
  monday_unit_exact_line "$file" CPUQuota '80%' || return 1
  monday_unit_exact_line "$file" OOMScoreAdjust 500 || return 1
  monday_unit_exact_line "$file" MemoryHigh '1792M' || return 1
  monday_unit_exact_line "$file" MemoryMax '2048M' || return 1
  [[ $(grep -c '^ExecStart=' "$file" || true) -eq 1 ]] || return 1
  [[ $(grep -Fxc 'ExecStart=/opt/monday/bin/binance-lob-archiver-shadow' "$file" || true) -eq 1 ]] || return 1
  [[ $(grep -Fxc 'ExecStartPre=/opt/monday/bin/binance-lob-archiver-shadow --self-test' "$file" || true) -eq 1 ]] || return 1
  [[ $(grep -Fxc 'EnvironmentFile=/etc/monday/binance-lob-archiver-rust-%i.env' "$file" || true) -eq 1 ]] || return 1
  [[ $(grep -Fxc 'EnvironmentFile=-/run/monday/binance-lob-archiver-rust-%i-soak.env' "$file" || true) -eq 1 ]] || return 1
  [[ $(grep -c '^EnvironmentFile=' "$file" || true) -eq 2 ]] || return 1
}

declare -A market_env spool_dir candidate_shadow_spool dataset symbols unit upload_unit expected_oss_prefix
declare -A candidate_upload_unit_sha
declare -A oss_bucket oss_endpoint oss_region aliyun_profile
declare -A phase_segments_json phase_triplets_json phase_health_json
markets=(spot usdm)
for market in "${markets[@]}"; do
  market_env[$market]="$candidate_deployment/binance-lob-archiver-rust-${market}.env"
  dataset[$market]=$(env_value "${market_env[$market]}" DATASET); symbols[$market]=$(env_value "${market_env[$market]}" SYMBOLS)
  oss_bucket[$market]=$(env_value "${market_env[$market]}" OSS_BUCKET)
  oss_endpoint[$market]=$(env_value "${market_env[$market]}" OSS_ENDPOINT)
  oss_region[$market]=$(env_value "${market_env[$market]}" OSS_REGION)
  aliyun_profile[$market]=$(env_value "${market_env[$market]}" ALIYUN_PROFILE)
  [[ $(env_value "${market_env[$market]}" MARKET) == "$market" ]] || die "$market env has wrong market"
  candidate_shadow_spool[$market]=$(env_value "${market_env[$market]}" SPOOL_DIR)
  [[ ${candidate_shadow_spool[$market]} != /data/monday/spool/binance-lob \
    && ${candidate_shadow_spool[$market]} != /data/monday/spool/binance-lob/* ]] \
    || die "$market shadow spool overlaps production spool"
  [[ $(env_value "${market_env[$market]}" SHARD_ID) == all ]] \
    || die "$market shadow SHARD_ID must be all"
  [[ ${oss_bucket[$market]} == monday-lob-apne1-1045353359 ]] \
    || die "$market shadow OSS bucket is not the production bucket"
  [[ ${oss_endpoint[$market]} == oss-ap-northeast-1-internal.aliyuncs.com ]] \
    || die "$market shadow OSS endpoint is not the internal Tokyo endpoint"
  [[ ${oss_region[$market]} == ap-northeast-1 ]] || die "$market shadow OSS region is not Tokyo"
  [[ ${aliyun_profile[$market]} == ecs-role ]] || die "$market shadow OSS profile is not ecs-role"
  verify_shadow_unit_template "$candidate_deployment/binance-lob-archiver-rust@.service" \
    || die 'candidate shadow service template failed security/resource verification'
  spool_dir[$market]=""
  unit[$market]=""
done
if [[ $TEST_ONLY != true ]]; then
  [[ ${symbols[spot]} == ALL && ${dataset[spot]} == spot_all_rust_shadow ]] || die 'Spot identity is invalid'
  is_usdm_top100 "${symbols[usdm]}" || die 'USD-M catalog is not frozen'
  [[ ${dataset[usdm]} == usdm_perpetual_top100_lob_rust_shadow ]] || die 'USD-M dataset identity is invalid'
fi
for market in "${markets[@]}"; do
  phase_segments_json[$market]='[]'
  phase_triplets_json[$market]='[]'
  phase_health_json[$market]='{}'
done

host_memory_total=$(meminfo_bytes MemTotal) || die 'MemTotal is unavailable'; host_memory_available=$(meminfo_bytes MemAvailable) || die 'MemAvailable is unavailable'; host_swap_total=$(meminfo_bytes SwapTotal) || die 'SwapTotal is unavailable'
declare -A production_growth; production_memory_json='{}'
declare -A production_pid production_exe_sha
production_process_json='{}'
if [[ $TEST_ONLY != true ]]; then
  declare -A production_state production_current production_peak production_max
  for market in "${markets[@]}"; do
    production_unit="binance-lob-archiver-production@${market}.service"; production_state[$market]=$(systemctl_show "$production_unit" ActiveState)
    case "${production_state[$market]}" in
      active) [[ $(systemctl_show "$production_unit" SubState) == running ]] || die "$market production is not running"
        production_current[$market]=$(systemctl_show "$production_unit" MemoryCurrent); production_peak[$market]=$(systemctl_show "$production_unit" MemoryPeak); production_max[$market]=$(systemctl_show "$production_unit" MemoryMax)
        production_growth[$market]=$(monday_production_memory_growth_headroom "${production_current[$market]}" "${production_peak[$market]}" "${production_max[$market]}" "$PRODUCTION_MEMORY_GROWTH_MARGIN_BYTES") || die "$market production memory accounting is invalid" ;;
      inactive) production_growth[$market]=0 ;;
      *) die "$market production state is ambiguous" ;;
    esac
    [[ ${production_state[$market]} == active ]] || die "$market production is not active for pair Gate"
    production_pid[$market]=$(systemctl_show "$production_unit" MainPID)
    [[ ${production_pid[$market]} =~ ^[1-9][0-9]*$ ]] || die "$market production MainPID is unavailable"
    production_exe_sha[$market]=$(sha256_file "$(readlink -f -- "$PROC_ROOT/${production_pid[$market]}/exe")") || die "$market production executable is unavailable"
    [[ ${production_exe_sha[$market]} == "$before_payload" ]] || die "$market production executable differs from before pair"
    production_process_json=$(jq -cn --argjson values "$production_process_json" --arg market "$market" --argjson pid "${production_pid[$market]}" --arg exe "${production_exe_sha[$market]}" '$values + {($market):{main_pid:$pid,process_exe_sha256:$exe,active:true}}')
  done
  production_memory_json=$(jq -cn --arg spot "${production_state[spot]}" --arg usdm "${production_state[usdm]}" '{spot:{active_state:$spot},usdm:{active_state:$usdm}}')
else production_growth[spot]=0; production_growth[usdm]=0; production_process_json='{}'; fi

resource_samples='[]'; psi_windows='[]'; resource_monitor_pid=; resource_monitor_control=; resource_monitor_log=; resource_monitor_phase=
strict_unit_seq=0
declare -A resource_phase_required resource_phase_limit
record_resource() {
  local phase=$1 phase_max=$2 required sample now
  host_memory_available=$(meminfo_bytes MemAvailable) || die 'MemAvailable became unavailable during Gate'
  required=$(monday_shadow_memory_admission "$host_memory_available" "$HOST_MEMORY_RESERVE_BYTES" "$phase_max" "${production_growth[spot]}" "${production_growth[usdm]}") || die "insufficient memory for $phase"
  resource_phase_required[$phase]=$required
  resource_phase_limit[$phase]=$phase_max
  now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  sample=$(jq -cn --arg phase "$phase" --argjson available "$host_memory_available" --argjson required "$required" --argjson phase_max "$phase_max" --arg now "$now" \
    '{phase:$phase,started_at:$now,ended_at:$now,samples:1,host_memory_available_bytes:$available,max_memory_available_bytes:$available,current_memory_available_bytes:$available,breach:false,required_bytes:$required,phase_memory_max_bytes:$phase_max}')
  resource_samples=$(jq -cn --argjson values "$resource_samples" --argjson value "$sample" '$values + [$value]')
}
resource_monitor_start() {
  local phase=$1 phase_max=$2 initial_available initial_psi parent_pid parent_starttime
  resource_monitor_phase=$phase
  record_resource "$phase" "$phase_max"
  [[ $TEST_ONLY == true ]] && return 0
  resource_monitor_control="$tmp_dir/resource-monitor-$phase.running"
  resource_monitor_log="$tmp_dir/resource-monitor-$phase.tsv"
  : >"$resource_monitor_control"; : >"$resource_monitor_log"
  initial_available=$(meminfo_bytes MemAvailable) || die 'MemAvailable became unavailable during Gate'
  initial_psi=$(io_total_us 2>/dev/null || printf 0)
  printf '%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$initial_available" "$initial_psi" >"$resource_monitor_log"
  parent_pid=$$
  parent_starttime=$(proc_starttime "$parent_pid") || die 'resource monitor parent starttime is unavailable'
  printf '%s %s\n' "$parent_pid" "$parent_starttime" >"$tmp_dir/resource-monitor-$phase.parent"
  (
    local previous_psi=$initial_psi available current_psi consecutive_hits=0 delta current_parent_starttime
    while [[ -e $resource_monitor_control ]]; do
      current_parent_starttime=$(proc_starttime "$parent_pid" 2>/dev/null || true)
      if ! kill -0 "$parent_pid" 2>/dev/null || [[ -z $current_parent_starttime || $current_parent_starttime != "$parent_starttime" ]]; then
        printf 'parent-disappeared\n' >"$tmp_dir/resource-monitor-parent-exit"
        break
      fi
      available=$(meminfo_bytes MemAvailable 2>/dev/null || printf 0)
      current_psi=$(io_total_us 2>/dev/null || printf 0)
      printf '%s\t%s\t%s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$available" "$current_psi" >>"$resource_monitor_log"
      if (( available < HOST_MEMORY_RESERVE_BYTES )); then
        printf 'memory-breach\n' >"$tmp_dir/resource-monitor-breach"
        kill -TERM "$parent_pid" 2>/dev/null || true
        break
      fi
      if (( current_psi < previous_psi )); then
        printf 'psi-regressed\n' >"$tmp_dir/resource-monitor-breach"
        kill -TERM "$parent_pid" 2>/dev/null || true
        break
      fi
      delta=$((current_psi - previous_psi))
      if (( delta * 3 >= IO_PSI_FULL_DELTA_LIMIT_US )); then
        consecutive_hits=$((consecutive_hits + 1))
      else
        consecutive_hits=0
      fi
      if (( consecutive_hits >= IO_PSI_CONSECUTIVE_HIT_LIMIT )); then
        printf 'psi-stop-rule\n' >"$tmp_dir/resource-monitor-breach"
        kill -TERM "$parent_pid" 2>/dev/null || true
        break
      fi
      previous_psi=$current_psi
      sleep 5
    done
  ) &
  resource_monitor_pid=$!
}
resource_monitor_stop() {
  local phase=$resource_monitor_phase started ended samples max_available current_available breach required phase_max parent_pid parent_starttime
  [[ -n ${resource_monitor_pid:-} ]] || return 0
  if [[ $TEST_ONLY == true ]]; then
    resource_monitor_pid=; resource_monitor_phase=; return 0
  fi
  rm -f -- "$resource_monitor_control"
  wait "$resource_monitor_pid" 2>/dev/null || true
  started=$(head -n1 "$resource_monitor_log" | cut -f1)
  ended=$(tail -n1 "$resource_monitor_log" | cut -f1)
  samples=$(wc -l <"$resource_monitor_log" | tr -d ' ')
  read -r parent_pid parent_starttime <"$tmp_dir/resource-monitor-$phase.parent" || true
  max_available=$(awk -F '\t' 'BEGIN{m=0} $2>m{m=$2} END{print m+0}' "$resource_monitor_log")
  current_available=$(tail -n1 "$resource_monitor_log" | cut -f2)
  required=${resource_phase_required[$phase]:-1}
  phase_max=${resource_phase_limit[$phase]:-1}
  breach=false; [[ -f $tmp_dir/resource-monitor-breach ]] && breach=true
  [[ $breach == false ]] || die "resource monitor breached during $phase"
  resource_samples=$(jq -cn --argjson values "$resource_samples" --arg phase "$phase" --arg started "$started" --arg ended "$ended" \
    --argjson samples "${samples:-0}" --argjson max "${max_available:-0}" --argjson current "${current_available:-0}" \
    --argjson required "${required:-1}" --argjson phase_max "${phase_max:-1}" \
    --arg parent_pid "${parent_pid:-}" --arg parent_starttime "${parent_starttime:-}" \
    '{phase:$phase,started_at:$started,ended_at:$ended,samples:$samples,host_memory_available_bytes:$current,max_memory_available_bytes:$max,current_memory_available_bytes:$current,breach:false,required_bytes:$required,phase_memory_max_bytes:$phase_max,parent_pid:($parent_pid|if length == 0 then null else tonumber end),parent_proc_starttime:($parent_starttime|if length == 0 then null else tonumber end)}' \
    | jq -s --argjson prior "$resource_samples" '($prior + .)')
  unset "resource_phase_required[$phase]" "resource_phase_limit[$phase]"
  resource_monitor_pid=; resource_monitor_phase=
}
calibrate_psi() {
  local phase=$1 previous current transition delta ratio hit consecutive=0 i
  if [[ $TEST_ONLY == true ]]; then psi_windows=$(jq -cn --argjson values "$psi_windows" --arg phase "$phase" '$values + [{phase:$phase,stage:"fixture",hit:false,consecutive_hits:0}]'); return; fi
  previous=$(io_total_us) || die "I/O PSI unavailable before $phase"
  for i in 1 2 3; do sleep "$IO_PSI_WINDOW_SECONDS"; current=$(io_total_us) || die "I/O PSI unavailable during $phase"
    transition=$(monday_io_full_psi_window "$previous" "$current" "$IO_PSI_WINDOW_US" "$IO_PSI_WINDOW_US" "$IO_PSI_FULL_DELTA_LIMIT_US" "$consecutive") || die 'I/O PSI moved backwards'
    read -r delta ratio hit consecutive <<<"$transition"; [[ $hit == false || $consecutive -lt $IO_PSI_CONSECUTIVE_HIT_LIMIT ]] || die 'I/O PSI threshold exceeded'
    psi_windows=$(jq -cn --argjson values "$psi_windows" --arg phase "$phase" --argjson delta "$delta" --argjson ratio "$ratio" --argjson hit "$hit" --argjson consecutive "$consecutive" '$values + [{phase:$phase,stage:"calibration",delta_us:$delta,ratio:$ratio,hit:$hit,consecutive_hits:$consecutive}]'); previous=$current
  done
}
assert_host_memory_reserve() {
  local available
  [[ $TEST_ONLY == true ]] && return 0
  available=$(meminfo_bytes MemAvailable) || die 'MemAvailable became unavailable during Gate'
  ((available >= HOST_MEMORY_RESERVE_BYTES)) || die "host memory reserve was consumed: available=$available reserve=$HOST_MEMORY_RESERVE_BYTES"
}

run_id=$(date -u +%Y%m%dT%H%M%SZ)-$$
evidence_dir="$EVIDENCE_ROOT/$CANDIDATE_CONTROLLER/$candidate_runtime/runs/$run_id"; gate_json="$evidence_dir/gate.json"; passed_marker="$evidence_dir/PASSED.sha256"; run_spool="$RUN_SPOOL_ROOT/$run_id"; run_json="$evidence_dir/run.json"
gate_unit_dir="$GATE_UNIT_ROOT/$run_id"

# BSD/GNU install only applies -m to the leaf when creating a nested path.
# Create each shadow spool component explicitly so the collector user can
# traverse a fresh tree without widening the parent permissions.
ensure_run_spool_dir "$DATA_ROOT/spool/binance-lob-rust-shadow"
ensure_run_spool_dir "$RUN_SPOOL_ROOT"
ensure_run_spool_dir "$run_spool"
ensure_run_spool_dir "$GATE_UNIT_ROOT"

# A killed Gate cannot run its EXIT trap.  On the next serialized Gate, only
# our own run-scoped names are stopped and removed; production units, links,
# and /etc bytes are never addressed by this cleanup.
valid_gate_transient_unit() {
  local candidate_unit_name=$1
  [[ $candidate_unit_name =~ ^${GATE_UNIT_PREFIX}[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*-(spot|usdm|spot-upload|usdm-upload|strict-[1-9][0-9]*)\.service$ ]]
}
cleanup_stale_gate_units() {
  local listed unit unit_file
  [[ $TEST_ONLY == true ]] && return 0
  listed=$(systemctl list-units --all --type=service --no-legend --plain \
    "${GATE_UNIT_PREFIX}*.service") || return 1
  while read -r unit _; do
    [[ -n ${unit:-} ]] || continue
    valid_gate_transient_unit "$unit" || continue
    systemctl stop "$unit" >/dev/null 2>&1 || true
    systemctl reset-failed "$unit" >/dev/null 2>&1 || true
  done <<<"$listed"
  direct_directory "$GATE_SYSTEMD_ROOT" || return 1
  while IFS= read -r unit_file; do
    unit=${unit_file##*/}
    valid_gate_transient_unit "$unit" || continue
    rm -f -- "$unit_file"
  done < <(find "$GATE_SYSTEMD_ROOT" -maxdepth 1 -type f -name 'monday-rust-lob-gate-*.service' -print)
  systemctl daemon-reload >/dev/null 2>&1 || return 1
}
cleanup_stale_gate_runs() {
  local dir old old_spool unit_file unit market
  [[ -n $RUN_SPOOL_ROOT ]] || return 1
  [[ -d $GATE_UNIT_ROOT ]] || return 0
  while IFS= read -r dir; do
    [[ -n $dir && $dir != "$gate_unit_dir" ]] || continue
    old=${dir##*/}
    [[ $old =~ ^[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*$ ]] || continue
    direct_directory "$dir" || return 1
    old_spool="$RUN_SPOOL_ROOT/$old"
    if [[ -e $old_spool || -L $old_spool ]]; then
      direct_directory "$old_spool" || return 1
    fi
    while IFS= read -r unit_file; do
      unit=${unit_file##*/}; unit=${unit%.service}
      valid_gate_transient_unit "$unit.service" || continue
      systemctl stop "$unit.service" >/dev/null 2>&1 || true
      systemctl reset-failed "$unit.service" >/dev/null 2>&1 || true
    done < <(find "$dir" -maxdepth 1 -type f -name 'monday-rust-lob-gate-*.service' -print)
    for market in spot usdm; do
      rm -f -- "$GATE_SYSTEMD_ROOT/monday-rust-lob-gate-${old}-${market}.service" \
        "$GATE_SYSTEMD_ROOT/monday-rust-lob-gate-${old}-${market}-upload.service"
    done
    rm -rf -- "$dir" "$old_spool"
  done < <(find "$GATE_UNIT_ROOT" -mindepth 1 -maxdepth 1 -type d -print)
  [[ $TEST_ONLY == true ]] || systemctl daemon-reload >/dev/null 2>&1 || return 1
}
cleanup_stale_gate_units
cleanup_stale_gate_runs
# Every Gate, including production, writes only to this run-scoped spool.  A
# prior test-only conditional left production's spool_dir empty and made the
# first install attempt fail before any market work; keep construction before
# the common install path so both modes exercise the same topology.
for market in "${markets[@]}"; do
  spool_dir[$market]=$(run_spool_dir "$run_id" "$market")
done
install -d -m 0750 "$EVIDENCE_ROOT" "$evidence_dir"
install -d -m 0755 "$GATE_SYSTEMD_ROOT"
ensure_run_spool_dir "$gate_unit_dir"
for market in "${markets[@]}"; do
  ensure_run_spool_dir "${spool_dir[$market]}"
done
if [[ ${MONDAY_GATE_FIXTURE_PATH_ONLY:-0} != 1 ]]; then
  while IFS= read -r prior_receipt; do
    if jq -e '.schema == "monday.rust_lob_shadow_gate.v5" and .passed == true' "$prior_receipt" >/dev/null 2>&1; then
      die 'a passed Gate receipt already exists for this controller identity'
    fi
  done < <(find "$EVIDENCE_ROOT/$CANDIDATE_CONTROLLER/$candidate_runtime" -type f -name gate.json -print)
fi
gate_started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ); gate_finished=false
declare -A phase_pid phase_exe_sha phase_session phase_segments phase_oss phase_runtime
declare -A phase_strict_lob phase_strict_aggregate phase_strict_raw
declare -A market_gate_started_ns market_observation_started_ns frozen_symbol_count frozen_catalog_sha256
declare -A market_observed_at_ns
declare -A initial_upload_failure_count last_health_updated_ns last_health_advance_mono
declare -A max_health_silence_seconds health_samples
write_run_json() {
  jq -cn --arg run "$run_id" --arg controller "$CANDIDATE_CONTROLLER" --arg payload "$candidate_payload" --arg runtime "$candidate_runtime" --arg spool "$run_spool" --argjson requested "$GATE_DURATION_SECONDS" --argjson settle "$HEALTH_SETTLE_DURATION_SECONDS" --argjson resources "$resource_samples" --argjson psi "$psi_windows" \
    '{schema:"monday.rust_lob_shadow_gate_run.v3",control_plane_version:2,run_id:$run,candidate_controller_sha256:$controller,candidate_payload_sha256:$payload,candidate_runtime_contract_sha256:$runtime,run_spool:$spool,segment_seconds:120,requested_duration_seconds:$requested,health_settle_seconds:$settle,resource_admission:$resources,io_full_psi_windows:$psi}' >"$run_json.tmp"
  chmod 0640 "$run_json.tmp"; mv -f -- "$run_json.tmp" "$run_json"
}
cleanup() {
  local status=$? cleanup_failed=false; set +e
  resource_monitor_stop >/dev/null 2>&1 || cleanup_failed=true
  for market in "${markets[@]}"; do
    [[ -n ${unit[$market]:-} ]] || continue
    systemctl stop "${unit[$market]}" >/dev/null 2>&1 || true
    rm -f -- "$GATE_SYSTEMD_ROOT/${unit[$market]}" \
      "$GATE_SYSTEMD_ROOT/monday-rust-lob-gate-${run_id}-${market}-upload.service"
  done
  [[ $TEST_ONLY == true ]] || systemctl daemon-reload >/dev/null 2>&1 || cleanup_failed=true
  rm -rf -- "$gate_unit_dir" "$run_spool" "$tmp_dir"
  [[ $gate_finished == true ]] || rm -f -- "$passed_marker" "$evidence_dir/.PASSED.sha256.tmp"
  [[ $cleanup_failed == false ]] || { printf 'run-scoped Gate cleanup was incomplete\n' >&2; status=1; }; exit "$status"
}
trap cleanup EXIT; trap 'exit 143' HUP INT TERM

# Fixture-only path coverage reaches the same run-scoped preparation without
# opening sockets, invoking OSS, or starting an external market process.
if [[ $TEST_ONLY == true && ${MONDAY_GATE_FIXTURE_PATH_ONLY:-0} == 1 ]]; then
  for path in "$DATA_ROOT/spool/binance-lob-rust-shadow" "$RUN_SPOOL_ROOT" \
    "$run_spool" "$gate_unit_dir" "${spool_dir[spot]}" "${spool_dir[usdm]}"; do
    [[ $(directory_mode "$path") == 750 ]] || die "fixture run directory mode is not 0750: $path"
  done
  for market in "${markets[@]}"; do
    [[ ${spool_dir[$market]} == "$run_spool/$market" ]] \
      || die "$market fixture Gate spool path is not run-scoped"
  done
  printf 'V2 Gate spool preparation: %s permissions=750\n' "$run_spool"
  exit 0
fi

resource_monitor_start preflight "$STRICT_VERIFIER_MEMORY_MAX_BYTES"; calibrate_psi preflight; resource_monitor_stop; write_run_json
if [[ $FROM_CONTROLLER != direct ]]; then
  for asset in "${SHADOW_ASSETS[@]}"; do
    [[ ${saved_state[$asset]} == present || ${saved_state[$asset]} == projection ]] || die "before shadow asset is absent: $asset"
    shadow_resolved=${installed_asset[$asset]}
    [[ ${saved_state[$asset]} == projection ]] && shadow_resolved=$(readlink -f -- "$shadow_resolved")
    cmp -s "$before_deployment/$asset" "$shadow_resolved" || die "installed shadow asset differs from before controller: $asset"
  done
else
  for asset in "${SHADOW_ASSETS[@]}"; do
    [[ ${saved_state[$asset]} == present || ${saved_state[$asset]} == projection ]] || die "direct bootstrap shadow asset is absent: $asset"
    shadow_resolved=${installed_asset[$asset]}
    [[ ${saved_state[$asset]} == projection ]] && shadow_resolved=$(readlink -f -- "$shadow_resolved")
    cmp -s "$candidate_deployment/$asset" "$shadow_resolved" || die "direct bootstrap installed shadow asset differs: $asset"
  done
fi

fixture_seed_market() {
  local market=$1 dir="${spool_dir[$1]}" i file data_sha now; [[ $TEST_ONLY == true ]] || return 0
  now=$(monotonic_seconds); mkdir -p "$dir"
  jq -cn --arg market "$market" --arg dataset "${dataset[$market]}" --arg session "fixture-$run_id-$market" --argjson updated "$((now * 1000000000))" '{market:$market,dataset:$dataset,updated_at_ns:$updated,status:"synced",sequence_gaps:0,symbol_count:1,symbols:{FIXTURE:{}},snapshot_ready_count:1,bridged_count:1,stream_coverage_verified_count:1,snapshot_only_symbols:[],all_symbols_bridged:true,all_stream_coverage_verified:true,full_stream_coverage_verified:true,queue_saturated:false,disk_warning:false,upload_warning:false,upload_failure_count:0,session_id:$session}' >"$dir/health.json"
  for i in 1 2; do
    file="part-$((now+i)).jsonl"; printf '{"schema":"binance.market_tape.v2","type":"session_start"}\n' >"$dir/$file"; zstd -q -f "$dir/$file" -o "$dir/$file.zst"; rm -f -- "$dir/$file"; file="$file.zst"; data_sha=$(sha256_file "$dir/$file")
    jq -cn --arg market "$market" --arg dataset "${dataset[$market]}" --arg file "$file" --arg sha "$data_sha" --arg session "fixture-$run_id-$market" --argjson start "$((now+i*1000))" --argjson end "$((now+i*1000+900))" '{schema:"binance.market_tape.v2",market:$market,dataset:$dataset,shard_id:"all",start_received_at_ns:$start,end_received_at_ns:$end,file:$file,sha256:$sha,symbols:["FIXTURE"],stream_types:["depth@100ms"],event_types:{agg_trade:0,raw_trade:0,book_ticker:0,force_order:0},has_replay_safe_checkpoint:true,lob_continuity:{sequence_gaps:0,reconnect_boundary:false,capture_session_id:$session}}' >"$dir/$file.manifest.json"
    printf '%s\n' "$data_sha" >"$dir/$file._SUCCESS"
  done
}
render_shadow_unit() {
  local market=$1 source_unit source_upload source_env rendered_unit rendered_upload rendered_env spool canonical_upload
  source_unit="$candidate_deployment/binance-lob-archiver-rust@.service"
  source_upload="$candidate_deployment/binance-lob-archiver-rust-upload@.service"
  source_env="$candidate_deployment/binance-lob-archiver-rust-${market}.env"
  rendered_unit="$gate_unit_dir/monday-rust-lob-gate-${run_id}-${market}.service"
  rendered_upload="$gate_unit_dir/monday-rust-lob-gate-${run_id}-${market}-upload.service"
  rendered_env="$gate_unit_dir/monday-rust-lob-gate-${run_id}-${market}.env"
  spool="${spool_dir[$market]}"
  monday_validate_unit_allowlist "$source_upload" shadow_upload \
    || die 'candidate shadow upload template failed security/resource verification'
  [[ -n $spool && $spool == "$run_spool/$market" ]] || die "$market Gate spool is not run-scoped"
  sed -e '/^EnvironmentFile=-\/run\/monday\/binance-lob-archiver-rust-%i-soak.env$/d' \
      -e "s|^EnvironmentFile=/etc/monday/binance-lob-archiver-rust-%i.env$|EnvironmentFile=$rendered_env|" \
      -e "s|^ExecStartPre=.*$|ExecStartPre=$candidate_binary --self-test|" \
      -e "s|^ExecStart=.*$|ExecStart=$candidate_binary|" \
      -e 's|^Restart=.*$|Restart=no|' \
      -e 's|^RuntimeMaxSec=.*$|RuntimeMaxSec=1800|' \
      -e "s|^ReadWritePaths=.*$|ReadWritePaths=$spool|" \
      "$source_unit" >"$rendered_unit"
  sed -e '/^\[Service\]$/a\
Restart=no\
RuntimeMaxSec=1800' \
      -e "s|^EnvironmentFile=.*$|EnvironmentFile=$rendered_env|" \
      -e "s|^ExecStart=.*$|ExecStart=$candidate_binary --upload-only|" \
      -e "s|^ReadWritePaths=.*$|ReadWritePaths=$spool|" \
      "$source_upload" >"$rendered_upload"
  sed -e "s|^SPOOL_DIR=.*$|SPOOL_DIR=$spool|" "$source_env" >"$rendered_env"
  chmod 0640 "$rendered_unit" "$rendered_upload" "$rendered_env"
  [[ $(grep -Fxc "EnvironmentFile=$rendered_env" "$rendered_unit" || true) -eq 1 ]] || die "$market Gate unit env path is not exact"
  [[ $(grep -Fxc 'Restart=no' "$rendered_unit" || true) -eq 1 ]] || die "$market Gate unit restart policy is not bounded"
  [[ $(grep -Fxc 'RuntimeMaxSec=1800' "$rendered_unit" || true) -eq 1 ]] || die "$market Gate unit runtime is not bounded"
  [[ $(grep -Fxc "ReadWritePaths=$spool" "$rendered_unit" || true) -eq 1 ]] || die "$market Gate unit spool is not exact"
  [[ $(grep -Fxc "EnvironmentFile=$rendered_env" "$rendered_upload" || true) -eq 1 ]] || die "$market Gate upload env path is not exact"
  [[ $(grep -Fxc "ExecStart=$candidate_binary --upload-only" "$rendered_upload" || true) -eq 1 ]] || die "$market Gate upload identity is not exact"
  [[ $(grep -Fxc "ReadWritePaths=$spool" "$rendered_upload" || true) -eq 1 ]] || die "$market Gate upload spool is not exact"
  [[ $(grep -Fxc 'Restart=no' "$rendered_upload" || true) -eq 1 ]] || die "$market Gate upload restart policy is not bounded"
  [[ $(grep -Fxc 'RuntimeMaxSec=1800' "$rendered_upload" || true) -eq 1 ]] || die "$market Gate upload runtime is not bounded"
  canonical_upload="$tmp_dir/$market-shadow-upload-source.service"
  sed -e "s|^EnvironmentFile=$rendered_env$|EnvironmentFile=/etc/monday/binance-lob-archiver-rust-%i.env|" \
      -e "s|^ExecStart=$candidate_binary --upload-only$|ExecStart=/opt/monday/bin/binance-lob-archiver-shadow --upload-only|" \
      -e "s|^ReadWritePaths=$spool$|ReadWritePaths=/data/monday/spool/binance-lob-rust-shadow|" \
      "$rendered_upload" >"$canonical_upload"
  monday_validate_unit_allowlist "$canonical_upload" shadow_upload_run \
    || die "$market rendered shadow upload unit failed security/resource verification"
  [[ $(grep -Fxc "SPOOL_DIR=$spool" "$rendered_env" || true) -eq 1 ]] || die "$market Gate env spool is not exact"
  [[ $TEST_ONLY == true ]] || systemd-analyze verify "$rendered_unit" "$rendered_upload" || die "$market Gate unit failed systemd-analyze verify"
  unit[$market]="monday-rust-lob-gate-${run_id}-${market}.service"
  # systemd does not search the private evidence directory.  Install a
  # run-scoped copy under its runtime search path, then remove it in the EXIT
  # trap; the rendered bytes remain bound by candidate_asset_sha above.
  local search_unit="$GATE_SYSTEMD_ROOT/${unit[$market]}"
  local search_upload="$GATE_SYSTEMD_ROOT/monday-rust-lob-gate-${run_id}-${market}-upload.service"
  [[ ! -e $search_unit && ! -L $search_unit ]] \
    || die "$market run-scoped Gate unit already exists"
  [[ ! -e $search_upload && ! -L $search_upload ]] \
    || die "$market run-scoped upload unit already exists"
  install -m 0644 "$rendered_unit" "$search_unit"
  install -m 0644 "$rendered_upload" "$search_upload"
  upload_unit[$market]="monday-rust-lob-gate-${run_id}-${market}-upload.service"
  candidate_upload_unit_sha[$market]=$(sha256_file "$rendered_upload")
  candidate_asset_sha["binance-lob-archiver-rust@.service"]=$(sha256_file "$rendered_unit")
  candidate_asset_sha["binance-lob-archiver-rust-upload@.service"]=$(sha256_file "$rendered_upload")
  candidate_asset_sha["binance-lob-archiver-rust-${market}.env"]=$(sha256_file "$rendered_env")
}
run_strict_verifier() {
  if [[ $TEST_ONLY == true ]]; then
    local expect_path=false argument
    for argument in "$@"; do
      if [[ $expect_path == true ]]; then
        [[ -f $argument && ! -L $argument ]] || return 1
        expect_path=false
      elif [[ $argument == --verify-segment ]]; then
        expect_path=true
      fi
    done
    [[ $expect_path == false ]]
    return
  fi
  strict_unit_seq=$((strict_unit_seq + 1))
  systemd-run --quiet --wait --collect \
    --unit="${GATE_UNIT_PREFIX}${run_id}-strict-${strict_unit_seq}.service" \
    --property=MemoryMax=1536M --property=MemoryHigh=1280M \
    --property=OOMScoreAdjust=500 --property=Restart=no --property=RuntimeMaxSec=1800 \
    --uid="$SERVICE_USER" -- "$candidate_binary" "$@"
}
run_strict_verifier_pair() { run_strict_verifier --require-lob-continuity "$@"; }
verify_adjacent_segments() {
  local -a args=(); local path digest manifest_digest
  (($# > 0 && $# % 3 == 0)) || return 1
  while (($#)); do
    path=$1; digest=$2; manifest_digest=$3; shift 3
    args+=(--verify-segment "$path" --segment-content-sha256 "$digest" --segment-manifest-sha256 "$manifest_digest")
  done
  run_strict_verifier_pair "${args[@]}"
}
verify_aggregate_trade_continuity() {
  local -a args=(--verify-aggregate-trade-continuity); local path digest manifest_digest
  (($# > 0 && $# % 3 == 0)) || return 1
  while (($#)); do path=$1; digest=$2; manifest_digest=$3; shift 3; args+=(--verify-segment "$path" --segment-content-sha256 "$digest" --segment-manifest-sha256 "$manifest_digest"); done
  run_strict_verifier "${args[@]}"
}
verify_raw_trade_continuity() {
  local -a args=(--verify-raw-trade-continuity); local path digest manifest_digest
  (($# > 0 && $# % 3 == 0)) || return 1
  while (($#)); do path=$1; digest=$2; manifest_digest=$3; shift 3; args+=(--verify-segment "$path" --segment-content-sha256 "$digest" --segment-manifest-sha256 "$manifest_digest"); done
  run_strict_verifier "${args[@]}"
}
systemctl_value() { systemctl_show "${unit[$1]}" "$2"; }
health_ok() {
  local market=$1 health="${spool_dir[$1]}/health.json"; [[ -f $health ]] || return 1
  if [[ $TEST_ONLY == true ]]; then jq -e --arg market "$market" '.market == $market and .status == "synced" and .sequence_gaps == 0' "$health" >/dev/null; return; fi
  local minimum_symbols=1000
  [[ $market == usdm ]] && minimum_symbols=100
  jq -e --arg market "$market" --arg dataset "${dataset[$market]}" \
    --arg symbols "${symbols[$market]}" --argjson minimum "$minimum_symbols" \
    --argjson started "${market_gate_started_ns[$market]}" \
    '.market == $market
      and .dataset == $dataset
      and (.updated_at_ns | type) == "number"
      and .updated_at_ns >= $started
      and .status == "synced"
      and .sequence_gaps == 0
      and (.symbol_count | type) == "number"
      and .symbol_count >= $minimum
      and .snapshot_ready_count == .symbol_count
      and .bridged_count == .symbol_count
      and .stream_coverage_verified_count == .symbol_count
      and .snapshot_only_symbols == []
      and .all_symbols_bridged == true
      and .all_stream_coverage_verified == true
      and (.full_stream_coverage_verified == null or .full_stream_coverage_verified == true)
      and .queue_saturated == false
      and .disk_warning == false
      and .upload_warning == false
      and .upload_failure_count == 0
      and (.session_id | type) == "string"
      and (.session_id | length) > 0
      and (if $market == "usdm" then (.symbols | keys | sort) == ($symbols | split(",") | sort) else true end)' \
    "$health" >/dev/null
}

health_catalog_sha256() {
  local market=$1
  jq -c '.symbols | keys | sort' "${spool_dir[$market]}/health.json" \
    | sha256sum | awk '{print $1}'
}

validate_observation_sample() {
  local market=$1 health="${spool_dir[$1]}/health.json" session symbols_now catalog upload_failures updated_ns current_mono
  health_ok "$market" || die "$market health failed during observation"
  session=$(jq -er '.session_id' "$health")
  [[ $session == "${phase_session[$market]}" ]] || die "$market collector session changed during observation"
  symbols_now=$(jq -er '.symbol_count' "$health")
  [[ $symbols_now == "${frozen_symbol_count[$market]}" ]] || die "$market symbol catalog changed during observation"
  catalog=$(health_catalog_sha256 "$market")
  [[ $catalog == "${frozen_catalog_sha256[$market]}" ]] || die "$market catalog membership changed during observation"
  upload_failures=$(jq -er '.upload_failure_count' "$health")
  [[ $upload_failures == "${initial_upload_failure_count[$market]}" ]] || die "$market upload failures changed during observation"
  updated_ns=$(jq -er '.updated_at_ns' "$health")
  current_mono=$(monotonic_seconds)
  if [[ $TEST_ONLY == true ]]; then
    last_health_updated_ns[$market]=$updated_ns
    last_health_advance_mono[$market]=$current_mono
    return 0
  fi
  local next_updated next_mono next_gap increment
  read -r next_updated next_mono next_gap increment < <(
    monday_observe_health_freshness \
      "${last_health_updated_ns[$market]}" \
      "${last_health_advance_mono[$market]}" \
      "${max_health_silence_seconds[$market]}" \
      "$updated_ns" "$current_mono" "$MAX_HEALTH_SILENCE_SECONDS"
  ) || die "$market health timestamp regressed or stopped advancing"
  last_health_updated_ns[$market]=$next_updated
  last_health_advance_mono[$market]=$next_mono
  max_health_silence_seconds[$market]=$next_gap
  health_samples[$market]=$((health_samples[$market] + increment))
}
verify_segments() {
  local market=$1 dir="${spool_dir[$1]}" path file digest manifest_digest success_digest expected_success count=0 previous_end=0 start end segment_json
  local -a segment_records=()
  phase_segments_json[$market]='[]'
  while IFS= read -r path; do
    file=${path##*/}; digest=$(sha256_file "$path"); expected_success="$tmp_dir/$market-$file.expected-success"; printf '%s\n' "$digest" >"$expected_success"; cmp -s "$path._SUCCESS" "$expected_success" || die "$market _SUCCESS digest mismatch"
    success_digest=$(sha256_file "$path._SUCCESS")
    manifest_digest=$(sha256_file "$path.manifest.json")
    jq -e --arg market "$market" --arg digest "$digest" --arg session "${phase_session[$market]}" \
      '.schema == "binance.market_tape.v2" and .market == $market and .sha256 == $digest
       and .has_replay_safe_checkpoint == true and .lob_continuity.sequence_gaps == 0
       and .lob_continuity.reconnect_boundary == false
       and .lob_continuity.capture_session_id == $session' \
      "$path.manifest.json" >/dev/null || die "$market manifest failed strict checks"
    start=$(jq -er '.start_received_at_ns' "$path.manifest.json"); end=$(jq -er '.end_received_at_ns' "$path.manifest.json"); ((previous_end == 0 || start >= previous_end)) || die "$market segments overlap"; ((previous_end == 0 || start-previous_end <= MAX_SEGMENT_GAP_NS)) || die "$market segment gap is too large"; previous_end=$end; count=$((count+1))
    segment_json=$(jq -cn --arg file "$file" --arg path "$path" --arg data_sha "$digest" \
      --arg manifest_sha "$manifest_digest" --arg success_sha "$success_digest" \
      --argjson start "$start" --argjson end "$end" --arg session "${phase_session[$market]}" \
      '{file:$file,path:$path,data_sha256:$data_sha,manifest_sha256:$manifest_sha,
        success_sha256:$success_sha,start_received_at_ns:$start,end_received_at_ns:$end,
        session_id:$session}')
    phase_segments_json[$market]=$(jq -cn --argjson values "${phase_segments_json[$market]}" \
      --argjson value "$segment_json" '$values + [$value]')
    segment_records+=("$path" "$digest" "$manifest_digest")
  done < <(find "$dir" -maxdepth 1 -type f -name '*.jsonl.zst' | sort)
  ((count >= 2)) || die "$market has fewer than two complete segments"
  verify_adjacent_segments "${segment_records[@]}" || die "$market strict LOB continuity verifier failed"
  if [[ $market == spot ]]; then
    verify_aggregate_trade_continuity "${segment_records[@]}" || die "$market strict aggregate-trade continuity verifier failed"
    verify_raw_trade_continuity "${segment_records[@]}" || die "$market strict raw-trade continuity verifier failed"
    phase_strict_aggregate[$market]=true
    phase_strict_raw[$market]=true
  else
    phase_strict_aggregate[$market]=false
    phase_strict_raw[$market]=false
  fi
  phase_strict_lob[$market]=true
  phase_segments["$market"]=$count
}
run_market_gate_phase() {
  local market=$1 settle observation pid started_ns
  resource_monitor_start "shadow-$market" 2147483648; calibrate_psi "shadow-$market"; fixture_seed_market "$market"; systemctl reset-failed "${unit[$market]}" >/dev/null 2>&1 || true
  started_ns=$(date +%s%N); market_gate_started_ns[$market]=$started_ns
  systemctl start "${unit[$market]}"; systemctl_active "${unit[$market]}" || die "$market shadow did not start"; [[ $(systemctl_value "$market" NRestarts) == 0 ]] || die "$market shadow restarted"; pid=$(systemctl_value "$market" MainPID); [[ $pid =~ ^[1-9][0-9]*$ ]] || die "$market MainPID unavailable"; phase_pid["$market"]=$pid; phase_exe_sha["$market"]=$candidate_payload
  if [[ $TEST_ONLY == true && ( ${MONDAY_GATE_FIXTURE_SIGKILL:-0} == 1 || ${MONDAY_GATE_HARD_CRASH_AFTER_SHADOW_START:-0} == 1 ) && $market == spot ]]; then
    kill -KILL "$$"
  fi
  if [[ $TEST_ONLY != true ]]; then
    exe_path=$(readlink -f -- "$PROC_ROOT/$pid/exe") || die "$market process executable is unavailable"
    [[ $(sha256_file "$exe_path") == "$candidate_payload" ]] || die "$market process executable identity differs from P1"
  fi
  settle=$(( $(monotonic_seconds) + HEALTH_SETTLE_DURATION_SECONDS )); while ! health_ok "$market"; do (( $(monotonic_seconds) < settle )) || die "$market health did not settle"; [[ $(systemctl_value "$market" NRestarts) == 0 ]] || die "$market restarted while settling"; [[ $(systemctl_value "$market" MainPID) == "$pid" ]] || die "$market MainPID changed while settling"; assert_host_memory_reserve; sleep 1; done
  phase_session["$market"]=$(jq -er '.session_id' "${spool_dir[$market]}/health.json"); frozen_symbol_count[$market]=$(jq -er '.symbol_count' "${spool_dir[$market]}/health.json"); frozen_catalog_sha256[$market]=$(health_catalog_sha256 "$market"); initial_upload_failure_count[$market]=$(jq -er '.upload_failure_count' "${spool_dir[$market]}/health.json"); last_health_updated_ns[$market]=$(jq -er '.updated_at_ns' "${spool_dir[$market]}/health.json"); last_health_advance_mono[$market]=$(monotonic_seconds); max_health_silence_seconds[$market]=0; health_samples[$market]=1; market_observation_started_ns[$market]=$(date +%s%N)
  phase_health_json[$market]=$(jq -cn --arg sha "$(sha256_file "${spool_dir[$market]}/health.json")" \
    --arg session "${phase_session[$market]}" --argjson symbols "${frozen_symbol_count[$market]}" \
    --arg catalog "${frozen_catalog_sha256[$market]}" --argjson silence "${max_health_silence_seconds[$market]}" \
    --argjson samples "${health_samples[$market]}" \
    '{sha256:$sha,session_id:$session,frozen_symbol_count:$symbols,
      frozen_catalog_sha256:$catalog,max_health_silence_seconds:$silence,samples:$samples}')
  observation=$(( $(monotonic_seconds) + GATE_DURATION_SECONDS )); while (( $(monotonic_seconds) < observation )); do validate_observation_sample "$market"; assert_host_memory_reserve; [[ $(systemctl_value "$market" NRestarts) == 0 ]] || die "$market restarted"; [[ $(systemctl_value "$market" MainPID) == "$pid" ]] || die "$market MainPID changed"; [[ $TEST_ONLY == true ]] && break; sleep 15; done
  phase_runtime["$market"]=$GATE_DURATION_SECONDS; systemctl stop "${unit[$market]}"; systemctl_active "${unit[$market]}" && die "$market shadow remained active"; resource_monitor_stop; resource_monitor_start "strict-verifier-$market" "$STRICT_VERIFIER_MEMORY_MAX_BYTES"; verify_segments "$market"; resource_monitor_stop; calibrate_psi "shadow-$market-tail"
}
DRAIN_ENV_KEYS=(
  MARKET DATASET SHARD_ID SYMBOLS DEPTH_MODE WS_SHARD_SIZE SNAPSHOT_LIMIT
  SNAPSHOT_REQUESTS_PER_SECOND SNAPSHOT_RETRY_ATTEMPTS
  SYNC_TIMEOUT_SECONDS STALL_TIMEOUT_SECONDS PROCESS_WATCHDOG_SECONDS
  TASK_CANCEL_TIMEOUT_SECONDS
  MAX_BUFFERED_DIFFS MAX_PENDING_DIFFS_TOTAL MIN_FREE_GB ZSTD_TIMEOUT_SECONDS
  OSS_COPY_TIMEOUT_SECONDS SEGMENT_SECONDS OSS_BUCKET OSS_ENDPOINT OSS_REGION
  ALIYUN_PROFILE BINANCE_REST_BASE LOG_LEVEL
)
declare -a candidate_drain_env_args=()
load_candidate_drain_env() {
  local market=$1 file=${market_env[$1]} spool=${spool_dir[$1]} key value
  candidate_drain_env_args=()
  [[ -n $spool && $spool == "$run_spool/$market" ]] || die "$market drain spool is not run-scoped"
  for key in "${DRAIN_ENV_KEYS[@]}"; do
    value=$(env_value "$file" "$key") || die "$market candidate env has no unique $key"
    candidate_drain_env_args+=("$key=$value")
  done
  if grep -q '^SNAPSHOT_PRODUCERS=' "$file"; then
    value=$(env_value "$file" SNAPSHOT_PRODUCERS) || die "$market candidate env has no unique SNAPSHOT_PRODUCERS"
    candidate_drain_env_args+=("SNAPSHOT_PRODUCERS=$value")
  fi
  # SPOOL_DIR is the one deliberate projection: the candidate env is used as
  # the source of every identity/timeout value, while the drain is forced into
  # this Gate's isolated spool instead of inheriting production's default.
  candidate_drain_env_args+=("SPOOL_DIR=$spool")
}
run_candidate_drain() {
  local market=$1
  resource_monitor_start "upload-drain-$market" "$UPLOAD_DRAIN_MEMORY_MAX_BYTES"
  # Build and validate the exact environment even in fixtures.  Returning
  # before this step previously masked a production-only default-spool bug.
  load_candidate_drain_env "$market"
  if [[ $TEST_ONLY == true ]]; then
    printf '%s\n' "${candidate_drain_env_args[@]}" >"$tmp_dir/$market-drain-env"
    jq -cn --arg market "$market" --arg dataset "${dataset[$market]}" \
      --arg session "${phase_session[$market]}" \
      '{market:$market,dataset:$dataset,last_error:null,session_id:$session,uploaded_triplets:1}' \
      >"${spool_dir[$market]}/upload-status.json"
    resource_monitor_stop
    return 0
  fi
  systemctl reset-failed "${upload_unit[$market]}" >/dev/null 2>&1 || true
  systemctl start "${upload_unit[$market]}" \
    || { resource_monitor_stop; die "$market run-scoped upload drain failed"; }
  [[ $(systemctl show "${upload_unit[$market]}" --property=Result --value) == success ]] \
    || { resource_monitor_stop; die "$market run-scoped upload drain did not complete successfully"; }
  resource_monitor_stop
}
assert_spool_drained() {
  local market=$1 remaining
  remaining=$(find "${spool_dir[$market]}" \( -type f -o -type l \) \( \
    -name '*.jsonl.part' -o -name '*.zst.tmp' -o -name '*.part.corrupt' -o \
    -name '*.uploaded-cleanup.json' -o -name '*.uploaded-cleanup.json.tmp' \
  \) -print -quit)
  [[ -z $remaining ]] || die "$market spool contains an unsealed or pending artifact: $remaining"
}
run_oss() {
  local market=$1; shift
  if [[ $TEST_ONLY == true ]]; then
    OSS_FIXTURE_MARKET=$market aliyun ossutil "$@" --profile "${aliyun_profile[$market]}" --endpoint fixture --region ap-northeast-1
  else
    [[ -x /usr/local/bin/aliyun ]] || die 'trusted OSS CLI is missing: /usr/local/bin/aliyun'
    runuser --user "$SERVICE_USER" -- env -i HOME="$SERVICE_HOME" PATH="$SAFE_PATH" \
      ALIYUN_PROFILE="${aliyun_profile[$market]}" /usr/local/bin/aliyun ossutil "$@" \
      --profile "${aliyun_profile[$market]}" --endpoint "${oss_endpoint[$market]}" \
      --region "${oss_region[$market]}"
  fi
}
verify_oss_roundtrips() {
  local market=$1 readback="$tmp_dir/oss-$1" listing="$tmp_dir/oss-$1.list" uri manifest final_manifest data success expected_success file digest manifest_digest success_digest line token replay_safe triplet_json observed_at observed_cutoff_ns
  local expected_bucket manifest_name object_prefix data_uri success_uri expected_file
  local candidates_file unsafe_file
  resource_monitor_start "oss-readback-$market" "$STRICT_VERIFIER_MEMORY_MAX_BYTES"
  observed_cutoff_ns=$(date +%s%N)
  [[ $observed_cutoff_ns =~ ^[0-9]+$ ]] || die "$market OSS observation clock is unavailable"
  local start end previous_end=0 count=0; local -a roundtrip_records=(); mkdir -p "$readback"
  phase_triplets_json[$market]='[]'
  candidates_file="$readback/replay-safe.tsv"; unsafe_file="$readback/replay-unsafe.tsv"
  : >"$candidates_file"; : >"$unsafe_file"
  local prefix
  if [[ $TEST_ONLY == true ]]; then
    prefix="oss://fixture/lake/raw/venue=binance/market=$market/dataset=${dataset[$market]}/shard=all/"
  else
    prefix="oss://${oss_bucket[$market]}/lake/raw/venue=binance/market=$market/dataset=${dataset[$market]}/shard=$(env_value "${market_env[$market]}" SHARD_ID)/"
  fi
  if [[ $TEST_ONLY == true ]]; then
    expected_oss_prefix[$market]=${prefix#oss://fixture/}
  else
    expected_oss_prefix[$market]=${prefix#oss://"${oss_bucket[$market]}"/}
  fi
  expected_oss_prefix[$market]=${expected_oss_prefix[$market]%/}
  monday_validate_oss_prefix "$market" "${dataset[$market]}" "${expected_oss_prefix[$market]}" \
    || die "$market OSS base prefix is invalid"
  expected_bucket=${oss_bucket[$market]}
  [[ $TEST_ONLY == true ]] && expected_bucket=fixture
  run_oss "$market" ls "$prefix" --recursive --short-format >"$listing"
  : >"$readback/manifest-uris"
  while IFS= read -r line; do
    line=${line%$'\r'}
    if [[ $line =~ (oss://[^[:space:]]+\.manifest\.json) ]]; then
      printf '%s\n' "${BASH_REMATCH[1]}" >>"$readback/manifest-uris"
      continue
    fi
    token=${line##*[$' \t']}; token=${token#/}
    [[ $token == *.manifest.json ]] && printf 'oss://%s/%s\n' "${oss_bucket[$market]}" "$token" >>"$readback/manifest-uris"
  done <"$listing"
  sort -u -o "$readback/manifest-uris" "$readback/manifest-uris"
  while IFS= read -r uri; do
    [[ -n $uri ]] || continue
    manifest_name=${uri##*/}
    monday_validate_lob_object_uri "$market" "${dataset[$market]}" "$expected_bucket" "$uri" manifest \
      || die "$market OSS manifest URI failed strict validation: $uri"
    object_prefix=${uri#"oss://$expected_bucket/"}
    object_prefix=${object_prefix%/"$manifest_name"}
    [[ $object_prefix == "${expected_oss_prefix[$market]}"/* ]] \
      || die "$market OSS manifest URI is outside the configured base prefix: $uri"
    manifest="$readback/discovered-$count.json"; run_oss "$market" cp "$uri" "$manifest" --force --no-progress >/dev/null
    jq -e --arg market "$market" \
      '.market == $market
       and (.start_received_at_ns | type == "number" and floor == . and . >= 0)
       and (.end_received_at_ns | type == "number" and floor == . and . >= 0)
       and .end_received_at_ns >= .start_received_at_ns
       and (.sha256 | type == "string" and test("^[a-f0-9]{64}$"))' \
      "$manifest" >/dev/null || die "$market OSS manifest failed strict verification"
    start=$(jq -er '.start_received_at_ns' "$manifest"); end=$(jq -er '.end_received_at_ns' "$manifest")
    [[ $start =~ ^[0-9]+$ && $end =~ ^[0-9]+$ && $end -le $observed_cutoff_ns ]] \
      || die "$market OSS manifest is future-dated"
    if [[ $TEST_ONLY != true ]] && ((end <= market_observation_started_ns[$market])); then
      continue
    fi
    jq -e --arg session "${phase_session[$market]}" \
      '.schema == "binance.market_tape.v2"
       and (.has_replay_safe_checkpoint | type == "boolean") and .lob_continuity.sequence_gaps == 0
       and .lob_continuity.reconnect_boundary == false
       and .lob_continuity.capture_session_id == $session
       and (.file | type == "string" and test("^part-[0-9]+\\.jsonl\\.zst$"))' \
      "$manifest" >/dev/null || die "$market OSS manifest failed strict verification"
    replay_safe=$(jq -er '.has_replay_safe_checkpoint' "$manifest")
    if [[ $replay_safe != true ]]; then
      printf '%s\t%s\t%s\n' "$start" "$end" "$uri" >>"$unsafe_file"
      continue
    fi
    printf '%s\t%s\t%s\n' "$start" "$end" "$uri" >>"$candidates_file"
    ((previous_end == 0 || start >= previous_end)) || die "$market OSS segments overlap"; ((previous_end == 0 || start - previous_end <= MAX_SEGMENT_GAP_NS)) || die "$market OSS continuity gap exceeded"; previous_end=$end
    expected_file=${manifest_name%.manifest.json}
    file=$(jq -er '.file' "$manifest"); [[ $file == "$expected_file" ]] \
      || die "$market OSS manifest filename does not match its object URI"
    data_uri="oss://$expected_bucket/$object_prefix/$file"
    success_uri="oss://$expected_bucket/$object_prefix/$file._SUCCESS"
    monday_validate_lob_object_uri "$market" "${dataset[$market]}" "$expected_bucket" "$data_uri" data \
      || die "$market OSS data URI failed strict validation: $data_uri"
    monday_validate_lob_object_uri "$market" "${dataset[$market]}" "$expected_bucket" "$success_uri" success \
      || die "$market OSS success URI failed strict validation: $success_uri"
    digest=$(jq -er '.sha256' "$manifest"); manifest_digest=$(sha256_file "$manifest")
    data="$readback/$file"; success="$data._SUCCESS"; final_manifest="$data.manifest.json"
    run_oss "$market" cp "$uri" "$final_manifest" --force --no-progress >/dev/null
    [[ $(sha256_file "$final_manifest") == "$manifest_digest" ]] || die "$market OSS manifest changed between reads"
    run_oss "$market" cp "$data_uri" "$data" --force --no-progress >/dev/null; run_oss "$market" cp "$success_uri" "$success" --force --no-progress >/dev/null
    expected_success="$readback/$file.expected-success"; printf '%s\n' "$digest" >"$expected_success"; [[ $(sha256_file "$data") == "$digest" ]] || die "$market OSS data digest mismatch"; cmp -s "$success" "$expected_success" || die "$market OSS success marker mismatch"
    success_digest=$(sha256_file "$success")
    observed_at=$(monday_epoch_ns_rfc3339 "$observed_cutoff_ns")
    triplet_json=$(jq -cn --arg market "$market" --arg dataset "${dataset[$market]}" \
      --arg data_uri "$data_uri" --arg manifest_uri "$uri" \
      --arg success_uri "$success_uri" --arg data_sha "$digest" \
      --arg manifest_sha "$manifest_digest" --arg success_sha "$success_digest" \
      --arg prefix "$object_prefix" --arg observed "$observed_at" \
      --arg session "${phase_session[$market]}" --arg catalog "${frozen_catalog_sha256[$market]}" \
      --argjson start "$start" --argjson end "$end" \
      --argjson observed_ns "$observed_cutoff_ns" \
      '{market:$market,dataset:$dataset,data_uri:$data_uri,manifest_uri:$manifest_uri,success_uri:$success_uri,
        data_sha256:$data_sha,manifest_sha256:$manifest_sha,success_sha256:$success_sha,
        success_content:($data_sha + "\n"),object_prefix:$prefix,observed_at:$observed,
        observed_at_ns:$observed_ns,
        start_received_at_ns:$start,end_received_at_ns:$end,session_id:$session,
        catalog_sha256:$catalog}')
    phase_triplets_json[$market]=$(jq -cn --argjson values "${phase_triplets_json[$market]}" \
      --argjson value "$triplet_json" '$values + [$value]')
    roundtrip_records+=("$data" "$digest" "$manifest_digest")
    count=$((count + 1))
  done <"$readback/manifest-uris"
  ((count >= 2)) || die "$market OSS readback has fewer than two triplets"
  monday_validate_replay_safe_manifest_order "$market" "$candidates_file" "$unsafe_file" \
    || die "$market replay-safe manifest ordering failed"
  verify_adjacent_segments "${roundtrip_records[@]}" || die "$market OSS strict LOB continuity verifier failed"
  if [[ $market == spot ]]; then
    verify_aggregate_trade_continuity "${roundtrip_records[@]}" || die "$market OSS strict aggregate-trade continuity verifier failed"
    verify_raw_trade_continuity "${roundtrip_records[@]}" || die "$market OSS strict raw-trade continuity verifier failed"
  fi
  market_observed_at_ns[$market]=$observed_cutoff_ns
  phase_oss[$market]=$count
  resource_monitor_stop
}

for market in "${markets[@]}"; do render_shadow_unit "$market"; done
[[ $TEST_ONLY == true ]] || systemctl daemon-reload
for market in "${markets[@]}"; do run_market_gate_phase "$market"; run_candidate_drain "$market"; assert_spool_drained "$market"; done
for market in "${markets[@]}"; do verify_oss_roundtrips "$market"; done
for market in "${markets[@]}"; do systemctl stop "${unit[$market]}" >/dev/null 2>&1 || true; done
for asset in "${SHADOW_ASSETS[@]}"; do
  target=${installed_asset[$asset]}
  if [[ ${saved_state[$asset]} == projection ]]; then
    [[ -L $target && $(readlink -- "$target") == "${saved_target[$asset]}" ]] \
      || die "shadow asset changed during Gate: $asset"
    resolved=$(readlink -f -- "$target") || die "shadow projection disappeared: $asset"
    [[ $(sha256_file "$resolved") == "${saved_sha[$asset]}" ]] || die "shadow asset bytes changed: $asset"
  elif [[ ${saved_state[$asset]} == present ]]; then
    [[ -f $target && ! -L $target && $(sha256_file "$target") == "${saved_sha[$asset]}" ]] \
      || die "shadow asset changed during Gate: $asset"
  else
    [[ ! -e $target && ! -L $target ]] || die "shadow asset appeared during Gate: $asset"
  fi
done
if [[ $FROM_CONTROLLER != direct ]]; then
  [[ $(monday_active_controller_sha "$ROOT") == "$FROM_CONTROLLER" ]] || die 'active controller changed during Gate'
  monday_verify_controller_projections "$ROOT" "$FROM_CONTROLLER" || die 'before controller projections changed during Gate'
  [[ -L $PRODUCTION_BINARY && $(readlink -- "$PRODUCTION_BINARY") == "$CONTROLLER_ROOT/active/binance-lob-archiver" ]] || die 'production identity changed during Gate'
else
  [[ $(monday_active_controller_sha "$ROOT") == "$legacy_controller" ]] || die 'legacy active controller changed during Gate'
  monday_verify_legacy_controller_release "$ROOT" "$legacy_controller" "$PRODUCTION_BINARY" \
    || die 'legacy controller identity changed during Gate'
  [[ $TEST_ONLY == true || $(readlink -f -- "$PRODUCTION_BINARY") == "$candidate_binary" ]] || die 'direct production identity changed during Gate'
fi
if [[ $old_shadow_present == true ]]; then [[ $(readlink -- "$SHADOW_BINARY") == "$old_shadow_target" ]] || die 'shadow link was not restored'; else [[ ! -e $SHADOW_BINARY && ! -L $SHADOW_BINARY ]] || die 'shadow link was not removed'; fi
final_payload=$(monday_manifest_field "$candidate_manifest" artifact_sha256) || die 'candidate payload identity disappeared during Gate'
final_runtime=$(monday_manifest_field "$candidate_manifest" runtime_contract_sha256) || die 'candidate runtime identity disappeared during Gate'
[[ $final_payload == "$candidate_payload" && $final_runtime == "$candidate_runtime" ]] \
  || die 'candidate C/P/R changed during Gate'
monday_verify_controller_release "$ROOT" "$CANDIDATE_CONTROLLER" \
  || die 'candidate controller failed final identity verification'
[[ $(sha256_file "$candidate_binary") == "$candidate_payload" ]] \
  || die 'candidate payload changed during Gate'
[[ $(sha256_file "$candidate_release/deployment.sha256") == "$candidate_control_bytes_sha" ]] \
  || die 'candidate control bytes changed during Gate'
if [[ $old_shadow_present == true ]]; then
  restored_target=$(readlink -f -- "$SHADOW_BINARY") || die 'restored shadow link is unresolved'
  [[ $(sha256_file "$restored_target") == "$old_shadow_target_sha256" ]] \
    || die 'restored shadow binary bytes changed during Gate'
fi

checks=$(jq -cn '{before_pair_unchanged:true,production_runtime_verified:true,shadow_staging_verified:true,shadow_assets_restored:true,resource_preflight:true,oss_triplets:true,strict_segment_verifier:true,final_identity:true,controller_control_bytes:true,shadow_link_restored:true,health_freshness:true}')
before_assets_json='{}'; staged_assets_json='{}'; restored_assets_json='{}'
for asset in "${SHADOW_ASSETS[@]}"; do
  restored_asset_sha[$asset]="${saved_sha[$asset]:-}"
  before_assets_json=$(jq -cn --argjson values "$before_assets_json" --arg asset "$asset" \
    --arg state "${saved_state[$asset]}" --arg sha "${saved_sha[$asset]:-}" \
    --arg target "${saved_target[$asset]:-}" \
    '$values + {($asset):{state:$state,sha256:(if $sha == "" then null else $sha end),target:(if $state == "projection" then $target else null end)}}')
  staged_assets_json=$(jq -cn --argjson values "$staged_assets_json" --arg asset "$asset" \
    --arg sha "${candidate_asset_sha[$asset]:-}" \
    '$values + {($asset):$sha}')
  restored_assets_json=$(jq -cn --argjson values "$restored_assets_json" --arg asset "$asset" \
    --arg state "${saved_state[$asset]}" --arg sha "${restored_asset_sha[$asset]:-}" \
    --arg target "${saved_target[$asset]:-}" \
    '$values + {($asset):{state:$state,sha256:(if $sha == "" then null else $sha end),target:(if $state == "projection" then $target else null end)}}')
done
markets_json='{}'
for market in "${markets[@]}"; do
  health_sha=$(sha256_file "${spool_dir[$market]}/health.json")
  receipt_bucket=${oss_bucket[$market]}
  [[ $TEST_ONLY == true ]] && receipt_bucket=fixture
  phase_health_json[$market]=$(jq -cn --argjson value "${phase_health_json[$market]}" \
    --argjson silence "${max_health_silence_seconds[$market]}" \
    --argjson samples "${health_samples[$market]}" \
    '$value | .max_health_silence_seconds=$silence | .samples=$samples')
  market_json=$(jq -cn --arg market "$market" --arg unit "${unit[$market]}" \
    --arg dataset "${dataset[$market]}" --arg session "${phase_session[$market]}" \
    --arg bucket "$receipt_bucket" --arg configured_bucket "${oss_bucket[$market]}" \
    --arg endpoint "${oss_endpoint[$market]}" --arg region "${oss_region[$market]}" \
    --arg profile "${aliyun_profile[$market]}" --arg shard all \
    --arg spool "${spool_dir[$market]}" --arg prefix "${expected_oss_prefix[$market]}" \
    --argjson pid "${phase_pid[$market]}" --arg exe "${phase_exe_sha[$market]}" \
    --argjson runtime "${phase_runtime[$market]}" --argjson segments "${phase_segments[$market]}" \
    --argjson oss "${phase_oss[$market]}" --arg health "$health_sha" \
    --argjson segment_evidence "${phase_segments_json[$market]}" \
    --argjson triplet_evidence "${phase_triplets_json[$market]}" \
    --argjson health_evidence "${phase_health_json[$market]}" \
    --argjson strict_lob "${phase_strict_lob[$market]:-false}" \
    --argjson strict_aggregate "${phase_strict_aggregate[$market]:-false}" \
    --argjson strict_raw "${phase_strict_raw[$market]:-false}" \
    --argjson observed_at_ns "${market_observed_at_ns[$market]}" \
    '{market:$market,unit:$unit,dataset:$dataset,session_id:$session,main_pid:$pid,
      process_exe_sha256:$exe,n_restarts:0,observed_runtime_seconds:$runtime,
      spool_dir:$spool,shard_id:$shard,oss_bucket:$configured_bucket,oss_endpoint:$endpoint,
      oss_region:$region,aliyun_profile:$profile,segment_count:$segments,oss_triplet_count:$oss,health_sha256:$health,
      expected_oss_bucket:$bucket,expected_oss_prefix:$prefix,observed_at_ns:$observed_at_ns,segments:$segment_evidence,
      triplets:$triplet_evidence,health:$health_evidence,
      process_identity_verified:true,installed_shadow_assets_verified:true,
      strict_lob_continuity_readback:$strict_lob,
      strict_aggregate_trade_continuity_readback:$strict_aggregate,
      strict_raw_trade_continuity_readback:$strict_raw}')
  markets_json=$(jq -cn --argjson values "$markets_json" --arg market "$market" --argjson value "$market_json" '$values + {($market):$value}')
done
run_units_json=$(jq -cn --arg spot "${unit[spot]}" --arg usdm "${unit[usdm]}" \
  '{spot:$spot,usdm:$usdm}')
run_upload_units_json=$(jq -cn \
  --arg spot "${upload_unit[spot]}" --arg usdm "${upload_unit[usdm]}" \
  --arg spot_sha "${candidate_upload_unit_sha[spot]}" \
  --arg usdm_sha "${candidate_upload_unit_sha[usdm]}" \
  '{spot:{unit:$spot,sha256:$spot_sha},usdm:{unit:$usdm,sha256:$usdm_sha}}')
gate_finished_at=$(date -u +%Y-%m-%dT%H:%M:%SZ); production_eligible=true; [[ $TEST_ONLY == true ]] && production_eligible=false
jq -cn --arg schema monday.rust_lob_shadow_gate.v5 --arg from "$before_controller" --arg source_mode "$source_mode" --arg after "$CANDIDATE_CONTROLLER" --arg candidate "$CANDIDATE_CONTROLLER" --arg payload "$candidate_payload" --arg runtime "$candidate_runtime" --arg bundle "$candidate_bundle" --arg source "$candidate_source" --arg before_payload "$before_payload" --arg before_runtime "$before_runtime" --arg before_bundle "$before_bundle" --arg before_source "$before_source" --arg before_projection "$before_production_projection" --arg control_sha "$candidate_control_bytes_sha" --argjson control_assets "$candidate_control_assets" --argjson production_runtime "$candidate_production_runtime" --arg run "$run_id" --arg spool "$run_spool" --arg run_unit_root "$gate_unit_dir" --argjson units "$run_units_json" --argjson upload_units "$run_upload_units_json" --arg started "$gate_started_at" --arg finished "$gate_finished_at" --argjson host_total "$host_memory_total" --argjson host_swap "$host_swap_total" --argjson production_memory "$production_memory_json" --argjson production_process "$production_process_json" --argjson production_assets "$production_asset_json" --argjson resources "$resource_samples" --argjson psi "$psi_windows" --argjson checks "$checks" --argjson markets "$markets_json" --argjson eligible "$production_eligible" --argjson test_only "$TEST_ONLY" --argjson before_assets "$before_assets_json" --argjson staged_assets "$staged_assets_json" --argjson restored_assets "$restored_assets_json" --arg shadow_binary "$SHADOW_BINARY" --arg candidate_binary "$candidate_binary" --arg old_shadow_target "$old_shadow_target" --arg old_shadow_target_sha "$old_shadow_target_sha256" --argjson old_shadow_present "$old_shadow_present" \
  '{schema:$schema,control_plane_version:2,passed:true,production_eligible:$eligible,test_only:$test_only,source_mode:$source_mode,from_controller_sha256:$from,transition:{before:$from,after:$after,topology:(if $source_mode == "direct" then "direct-bootstrap" else "stable" end)},candidate_controller_sha256:$candidate,candidate_payload_sha256:$payload,candidate_runtime_contract_sha256:$runtime,candidate_deployment_bundle_sha256:$bundle,candidate_deployment_source_revision:$source,candidate_control_bytes:{sha256:$control_sha,assets:$control_assets},production_runtime:$production_runtime,before:{controller:$from,payload_sha256:$before_payload,runtime_contract_sha256:$before_runtime,deployment_bundle_sha256:$before_bundle,deployment_source_revision:$before_source,production_projection:$before_projection,production_assets:$production_assets},run_id:$run,run_spool:$spool,started_at:$started,finished_at:$finished,required_duration_seconds:240,health_settle_seconds:240,segment_seconds:120,host_memory_total_bytes:$host_total,host_swap_total_bytes:$host_swap,production_memory:$production_memory,production_process:$production_process,production_assets:$production_assets,resource_admission:$resources,io_full_psi_windows:$psi,shadow_staging:{mode:"run-scoped",run_unit_root:$run_unit_root,spool_root:$spool,units:$units,upload_units:$upload_units,candidate_assets:$staged_assets,restored_assets:$restored_assets,before_assets:$before_assets,binary:{path:$run_unit_root,candidate_target:$candidate_binary,restored_target:(if $old_shadow_present then $old_shadow_target else null end),restored_target_sha256:(if $old_shadow_present then $old_shadow_target_sha else null end),restored_present:$old_shadow_present}},checks:$checks,markets:$markets}' >"$gate_json.tmp"
chmod 0640 "$gate_json.tmp"; [[ ! -e $gate_json ]] || die 'gate receipt already exists'; mv -f -- "$gate_json.tmp" "$gate_json"
if ! jq -e -f "$POLICY_SOURCE" "$gate_json" >/dev/null; then die 'V2 Gate policy rejected the receipt'; fi
if [[ $production_eligible == true ]]; then gate_sha=$(sha256_file "$gate_json"); printf '%s  gate.json\n' "$gate_sha" >"$passed_marker.tmp"; chmod 0640 "$passed_marker.tmp"; mv -f -- "$passed_marker.tmp" "$passed_marker"; fi
gate_finished=true; printf 'V2 Gate receipt: %s\nSHA-256: %s\n' "$gate_json" "$(sha256_file "$gate_json")"; [[ $production_eligible == true ]] && printf 'production shadow gate passed\n' || printf 'fixture Gate completed; not eligible for cutover\n'
