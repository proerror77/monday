#!/usr/bin/env bash
set -euo pipefail

umask 027
export LC_ALL=C

readonly REQUIRED_DURATION_SECONDS=3600
readonly PARITY_TAIL_SECONDS=300
readonly MINIMUM_GATE_SECONDS=$((REQUIRED_DURATION_SECONDS + PARITY_TAIL_SECONDS))
readonly HEALTH_SETTLE_SECONDS=180
readonly MAX_HEALTH_SILENCE_SECONDS=90
readonly SAMPLE_SECONDS=30
readonly PARITY_CUTOFF_LAG_SECONDS=60
readonly LEGACY_UNIT=polymarket-reference-collector.service
readonly LEGACY_EXEC='/usr/bin/python3 /opt/monday/bin/polymarket_reference_collector.py'
readonly LEGACY_FRAGMENT=/etc/systemd/system/polymarket-reference-collector.service
readonly SHADOW_FRAGMENT=/etc/systemd/system/polymarket-reference-collector-shadow@.service
readonly LEGACY_SPOOL=/data/monday/spool/polymarket-reference
readonly UPLOAD_ENV=/etc/monday/polymarket-market-tape-upload.env
readonly RELEASE_ROOT=/opt/monday/releases/polymarket-raw-ops
readonly SHADOW_ROOT=/data/monday/spool/polymarket-reference-rust-shadow
readonly EVIDENCE_ROOT=/data/monday/evidence/polymarket-shadow-gates
readonly LOCK_FILE=/run/lock/monday-polymarket-raw-ops.lock
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
readonly SCRIPT_DIR
readonly SERVICE_TEMPLATE="$SCRIPT_DIR/polymarket-reference-collector-shadow@.service"
readonly GATE_POLICY="$SCRIPT_DIR/polymarket-shadow-gate-policy.jq"
readonly LEGACY_HEALTH_POLICY="$SCRIPT_DIR/polymarket-legacy-health-policy.jq"
readonly RUST_HEALTH_POLICY="$SCRIPT_DIR/polymarket-rust-health-policy.jq"
readonly -a BUNDLE_ASSETS=(
  polymarket-raw-ops-shadow-gate.sh
  polymarket-raw-ops-cutover.sh
  polymarket-shadow-gate-policy.jq
  polymarket-legacy-health-policy.jq
  polymarket-rust-health-policy.jq
  polymarket-reference-collector-shadow@.service
  polymarket-reference-collector.service
  polymarket-reference-upload.service
  polymarket-reference-upload.timer
  polymarket-market-tape-upload.service
  polymarket-market-tape-upload.timer
)

die() {
  printf 'Polymarket shadow gate failed: %s\n' "$*" >&2
  exit 1
}

usage() {
  printf '%s\n' \
    'Usage: polymarket-raw-ops-shadow-gate.sh <candidate-binary> <sha256> <source-revision>' \
    '' \
    'A production-eligible gate observes for 3600 seconds plus a 300-second current-hour parity tail.'
}

bundle_sha256() {
  (
    cd "$SCRIPT_DIR"
    sha256sum "${BUNDLE_ASSETS[@]}" | sha256sum | awk '{print $1}'
  )
}

direct_directory() {
  local path=$1
  [[ -d $path && ! -L $path && $(readlink -f -- "$path") == "$path" ]]
}

direct_directory_or_absent() {
  local path=$1
  [[ ! -e $path && ! -L $path ]] || direct_directory "$path"
}

secure_release_directory() {
  local path=$1 owner mode
  direct_directory "$path" || return 1
  owner=$(stat -c %u -- "$path") || return 1
  mode=$(stat -c %a -- "$path") || return 1
  [[ $owner == 0 && $mode == 755 ]]
}

secure_control_file() {
  local path=$1 mode owner
  [[ -f $path && ! -L $path ]] || die "missing direct control-plane file: $path"
  owner=$(stat -c %u -- "$path")
  mode=$(stat -c %a -- "$path")
  [[ $owner == 0 ]] || die "control-plane file is not root-owned: $path"
  (( (8#$mode & 022) == 0 )) || die "control-plane file is group/world writable: $path"
}

effective_exec_argv() {
  local unit=$1 raw argv
  raw=$(systemctl show --property=ExecStart --value "$unit") || return 1
  argv=$(sed -nE 's/^.*argv\[\]=([^;]+);.*$/\1/p' <<<"$raw" \
    | sed -E 's/[[:space:]]+$//')
  [[ -n $argv ]] || return 1
  printf '%s\n' "$argv"
}

proc_cmdline() {
  local pid=$1
  [[ $pid =~ ^[1-9][0-9]*$ && -r /proc/$pid/cmdline ]] || return 1
  tr '\0' ' ' <"/proc/$pid/cmdline"
}

verify_legacy_identity() {
  local expected_pid=$1 pid restarts fragment drop_ins exec_argv cmdline
  systemctl is-active --quiet "$LEGACY_UNIT" || return 1
  fragment=$(systemctl show --property=FragmentPath --value "$LEGACY_UNIT") || return 1
  [[ $fragment == "$LEGACY_FRAGMENT" ]] || return 1
  drop_ins=$(systemctl show --property=DropInPaths --value "$LEGACY_UNIT") || return 1
  [[ -z $drop_ins ]] || return 1
  exec_argv=$(effective_exec_argv "$LEGACY_UNIT") || return 1
  [[ $exec_argv == "$LEGACY_EXEC" ]] || return 1
  pid=$(systemctl show --property=MainPID --value "$LEGACY_UNIT") || return 1
  [[ $pid == "$expected_pid" ]] || return 1
  restarts=$(systemctl show --property=NRestarts --value "$LEGACY_UNIT") || return 1
  [[ $restarts == 0 ]] || return 1
  cmdline=$(proc_cmdline "$pid") || return 1
  [[ $cmdline == "$LEGACY_EXEC " ]]
}

env_value() {
  local key=$1 file=${2:-$UPLOAD_ENV} count value
  count=$(grep -c "^${key}=" "$file" || true)
  [[ $count -eq 1 ]] || die "$file must contain exactly one $key"
  value=$(sed -n "s/^${key}=//p" "$file")
  [[ -n $value ]] || die "$file has an empty $key"
  printf '%s\n' "$value"
}

oss_config_sha256() {
  local file=${1:-$UPLOAD_ENV} key
  for key in OSS_BUCKET OSS_ENDPOINT OSS_REGION ALIYUN_PROFILE \
    ZSTD_TIMEOUT_SECONDS OSS_COPY_TIMEOUT_SECONDS; do
    printf '%s=%s\n' "$key" "$(env_value "$key" "$file")"
  done | sha256sum | awk '{print $1}'
}

load_oss_config_snapshot() {
  oss_bucket=$(env_value OSS_BUCKET)
  oss_endpoint=$(env_value OSS_ENDPOINT)
  oss_region=$(env_value OSS_REGION)
  aliyun_profile=$(env_value ALIYUN_PROFILE)
  zstd_timeout_seconds=$(env_value ZSTD_TIMEOUT_SECONDS)
  oss_copy_timeout_seconds=$(env_value OSS_COPY_TIMEOUT_SECONDS)
  oss_config_sha=$(printf '%s\n' \
    "OSS_BUCKET=$oss_bucket" \
    "OSS_ENDPOINT=$oss_endpoint" \
    "OSS_REGION=$oss_region" \
    "ALIYUN_PROFILE=$aliyun_profile" \
    "ZSTD_TIMEOUT_SECONDS=$zstd_timeout_seconds" \
    "OSS_COPY_TIMEOUT_SECONDS=$oss_copy_timeout_seconds" \
    | sha256sum | awk '{print $1}')
  [[ $(oss_config_sha256) == "$oss_config_sha" ]] \
    || die 'OSS configuration changed while it was being snapshotted'
}

verify_current_oss_config() {
  [[ $(oss_config_sha256) == "$oss_config_sha" ]] \
    || die 'OSS configuration changed during the shadow gate'
}

install_pinned_upload_env() {
  local destination=$1 temporary
  if [[ -e $destination || -L $destination ]]; then
    secure_control_file "$destination"
    [[ $(oss_config_sha256 "$destination") == "$oss_config_sha" ]] \
      || die 'existing pinned OSS environment differs from the gate configuration'
    return 0
  fi
  temporary="${destination}.new.$$"
  (
    umask 077
    printf '%s\n' \
      "OSS_BUCKET=$oss_bucket" \
      "OSS_ENDPOINT=$oss_endpoint" \
      "OSS_REGION=$oss_region" \
      "ALIYUN_PROFILE=$aliyun_profile" \
      "ZSTD_TIMEOUT_SECONDS=$zstd_timeout_seconds" \
      "OSS_COPY_TIMEOUT_SECONDS=$oss_copy_timeout_seconds" >"$temporary"
  )
  chmod 0640 "$temporary"
  chown root:root "$temporary"
  mv -Tf "$temporary" "$destination"
  sync "$destination"
  secure_control_file "$destination"
  [[ $(oss_config_sha256 "$destination") == "$oss_config_sha" ]] \
    || die 'pinned OSS environment identity mismatch'
}

verify_shadow_identity() {
  local expected_pid=$1 pid restarts fragment drop_ins exec_argv cmdline
  local expected_exec_raw expected_exec_expanded
  systemctl is-active --quiet "$shadow_unit" || return 1
  fragment=$(systemctl show --property=FragmentPath --value "$shadow_unit") || return 1
  [[ $fragment == "$SHADOW_FRAGMENT" ]] || return 1
  drop_ins=$(systemctl show --property=DropInPaths --value "$shadow_unit") || return 1
  [[ -z $drop_ins ]] || return 1
  expected_exec_raw="$release_binary collect-reference --spool-dir \${MONDAY_POLYMARKET_SHADOW_SPOOL}"
  expected_exec_expanded="$release_binary collect-reference --spool-dir $shadow_spool"
  exec_argv=$(effective_exec_argv "$shadow_unit") || return 1
  [[ $exec_argv == "$expected_exec_raw" || $exec_argv == "$expected_exec_expanded" ]] \
    || return 1
  pid=$(systemctl show --property=MainPID --value "$shadow_unit") || return 1
  [[ $pid == "$expected_pid" ]] || return 1
  restarts=$(systemctl show --property=NRestarts --value "$shadow_unit") || return 1
  [[ $restarts == 0 ]] || return 1
  [[ $(readlink -f "/proc/$pid/exe") == "$release_binary" ]] || return 1
  cmdline=$(proc_cmdline "$pid") || return 1
  [[ $cmdline == "$release_binary collect-reference --spool-dir $shadow_spool " ]]
}

[[ ${EUID} -eq 0 ]] || die 'must run as root'
[[ $# -eq 3 ]] || {
  usage >&2
  exit 2
}

for command in awk chown chmod date flock grep install jq mkdir mktemp mountpoint mv \
  readlink rm runuser sed sha256sum sleep stat sync systemctl tr; do
  command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done

candidate_source=$1
candidate_sha=$(printf '%s' "$2" | tr '[:upper:]' '[:lower:]')
source_revision=$(printf '%s' "$3" | tr '[:upper:]' '[:lower:]')
[[ $candidate_sha =~ ^[a-f0-9]{64}$ ]] || die 'candidate SHA-256 is invalid'
[[ $source_revision =~ ^[a-f0-9]{40,64}$ ]] || die 'source revision is invalid'
[[ -f $candidate_source && ! -L $candidate_source && -x $candidate_source ]] \
  || die 'candidate must be a direct executable regular file'
printf '%s  %s\n' "$candidate_sha" "$candidate_source" \
  | sha256sum --check --strict >/dev/null || die 'candidate checksum mismatch'

for asset in "${BUNDLE_ASSETS[@]}"; do
  secure_control_file "$SCRIPT_DIR/$asset"
done
secure_control_file "$UPLOAD_ENV"
deployment_bundle_sha=$(bundle_sha256)
load_oss_config_snapshot
mountpoint -q /data || die '/data must be a mount point'

for path in /opt/monday /opt/monday/releases "$RELEASE_ROOT" \
  /data/monday /data/monday/spool "$SHADOW_ROOT" \
  /data/monday/evidence "$EVIDENCE_ROOT"; do
  direct_directory_or_absent "$path" || die "fixed path is indirect or a symlink: $path"
done

install -d -m 0755 "$(dirname "$LOCK_FILE")"
exec 9>"$LOCK_FILE"
flock -n 9 || die 'another Polymarket release operation is running'

legacy_pid=$(systemctl show --property=MainPID --value "$LEGACY_UNIT")
[[ $legacy_pid =~ ^[1-9][0-9]*$ ]] || die 'active Python collector has no verifiable MainPID'
verify_legacy_identity "$legacy_pid" \
  || die 'active reference collector identity is not exact and restart-free'

gate_seconds=${MONDAY_POLYMARKET_GATE_SECONDS:-$MINIMUM_GATE_SECONDS}
[[ $gate_seconds =~ ^[1-9][0-9]*$ ]] || die 'gate duration must be a positive integer'
test_only=false
if ((gate_seconds < MINIMUM_GATE_SECONDS)); then
  [[ ${MONDAY_ALLOW_SHORT_GATE_FOR_TESTS:-0} == 1 ]] \
    || die 'short gates require MONDAY_ALLOW_SHORT_GATE_FOR_TESTS=1'
  test_only=true
fi

release_dir="$RELEASE_ROOT/$candidate_sha"
release_binary="$release_dir/polymarket-raw-ops"
cleanup() {
  local status=$?
  trap - EXIT
  systemctl stop "${shadow_unit:-}" >/dev/null 2>&1 || true
  rm -rf "${staging:-}"
  rm -f "${shadow_env_file:-}" "${shadow_env_tmp:-}"
  exit "$status"
}
trap cleanup EXIT
if [[ -e $release_dir || -L $release_dir ]]; then
  secure_release_directory "$release_dir" \
    || die 'existing candidate release directory is not root-owned mode 0755'
  secure_control_file "$release_binary"
  [[ -x $release_binary ]] || die 'existing release is not executable'
  printf '%s  %s\n' "$candidate_sha" "$release_binary" \
    | sha256sum --check --strict >/dev/null || die 'existing release identity mismatch'
else
  install -d -m 0755 "$RELEASE_ROOT"
  staging=$(mktemp -d "$RELEASE_ROOT/.${candidate_sha}.new.XXXXXX")
  install -m 0755 "$candidate_source" "$staging/polymarket-raw-ops"
  printf '%s  %s\n' "$candidate_sha" "$staging/polymarket-raw-ops" \
    | sha256sum --check --strict >/dev/null
  chown root:root "$staging"
  chmod 0755 "$staging"
  secure_release_directory "$staging" \
    || die 'staged release directory is not root-owned mode 0755'
  mv "$staging" "$release_dir"
  staging=
fi
secure_release_directory "$release_dir" \
  || die 'candidate release directory is not root-owned mode 0755'
secure_control_file "$release_binary"
[[ -x $release_binary ]] || die 'candidate release is not executable'
pinned_upload_env="$release_dir/polymarket-upload-env-$oss_config_sha.env"
install_pinned_upload_env "$pinned_upload_env"

run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
shadow_parent="$SHADOW_ROOT/$candidate_sha"
shadow_spool="$shadow_parent/$run_id"
market_shadow_spool="$shadow_parent/${run_id}-market-upload"
shadow_unit="polymarket-reference-collector-shadow@${candidate_sha}.service"
[[ ! -e $shadow_spool && ! -L $shadow_spool ]] \
  || die 'refusing to reuse a shadow spool run'
[[ ! -e $market_shadow_spool && ! -L $market_shadow_spool ]] \
  || die 'refusing to reuse a market upload shadow spool run'
install -d -m 0755 /data/monday /data/monday/spool "$SHADOW_ROOT" "$shadow_parent"
for path in /data/monday /data/monday/spool "$SHADOW_ROOT" "$shadow_parent"; do
  direct_directory "$path" || die "created shadow path is indirect: $path"
done
install -d -m 0750 -o hftcollector -g hftcollector "$shadow_spool"
install -d -m 0750 -o hftcollector -g hftcollector "$market_shadow_spool"

install -d -m 0755 /run/monday
shadow_env_file="/run/monday/polymarket-reference-shadow-${candidate_sha}.env"
# A killed gate can leave its isolated unit/env behind. The global release lock
# proves there is no live gate owner, so stop only that shadow instance and
# replace its root-owned environment with this run's unique spool.
systemctl stop "$shadow_unit" >/dev/null 2>&1 || true
if [[ -e $shadow_env_file || -L $shadow_env_file ]]; then
  secure_control_file "$shadow_env_file"
  rm -f "$shadow_env_file"
fi
shadow_env_tmp="${shadow_env_file}.new.$$"
printf 'MONDAY_POLYMARKET_SHADOW_SPOOL=%s\n' "$shadow_spool" >"$shadow_env_tmp"
chmod 0644 "$shadow_env_tmp"
mv "$shadow_env_tmp" "$shadow_env_file"
shadow_env_tmp=

install -m 0644 "$SERVICE_TEMPLATE" \
  /etc/systemd/system/polymarket-reference-collector-shadow@.service
systemctl daemon-reload

started_at_unix=$(date -u +%s)
started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
start_uptime=$SECONDS
systemctl start "$shadow_unit"

last_health=
last_health_change=$start_uptime
last_legacy_health=
last_legacy_health_change=$start_uptime
shadow_pid=
initial_shadow_pid=
common_cutoff=
parity_window_started_at=
while :; do
  now_uptime=$SECONDS
  elapsed=$((now_uptime - start_uptime))
  verify_legacy_identity "$legacy_pid" \
    || die 'legacy collector PID, restart count, or effective unit identity changed during gate'
  shadow_pid=$(systemctl show --property=MainPID --value "$shadow_unit")
  [[ $shadow_pid =~ ^[1-9][0-9]*$ ]] || die 'Rust shadow has no MainPID'
  if [[ -z $initial_shadow_pid ]]; then
    initial_shadow_pid=$shadow_pid
  else
    [[ $shadow_pid == "$initial_shadow_pid" ]] || die 'Rust shadow MainPID changed during gate'
  fi
  verify_shadow_identity "$initial_shadow_pid" \
    || die 'Rust shadow systemd identity, PID, or command line changed during gate'
  if ((elapsed >= HEALTH_SETTLE_SECONDS)) || [[ $test_only == true ]]; then
    health="$shadow_spool/health.json"
    [[ -f $health && ! -L $health ]] || die 'Rust shadow health is missing'
    jq -e -f "$RUST_HEALTH_POLICY" "$health" >/dev/null \
      || die 'Rust shadow health is not fail-closed clean'
    current_health=$(jq -r '.updated_at' "$health")
    if [[ $current_health != "$last_health" ]]; then
      last_health=$current_health
      last_health_change=$now_uptime
    fi
    ((now_uptime - last_health_change <= MAX_HEALTH_SILENCE_SECONDS)) \
      || die 'Rust shadow health stopped advancing'

    legacy_health="$LEGACY_SPOOL/health.json"
    [[ -f $legacy_health && ! -L $legacy_health ]] || die 'Python health is missing'
    jq -e -f "$LEGACY_HEALTH_POLICY" "$legacy_health" >/dev/null \
      || die 'Python health is not fail-closed clean during shadow'
    current_legacy_health=$(jq -r '.updated_at' "$legacy_health")
    if [[ $current_legacy_health != "$last_legacy_health" ]]; then
      last_legacy_health=$current_legacy_health
      last_legacy_health_change=$now_uptime
    fi
    ((now_uptime - last_legacy_health_change <= MAX_HEALTH_SILENCE_SECONDS)) \
      || die 'Python health stopped advancing during shadow'

    rust_success_at=$(jq -er '.last_success_at | select(type == "string" and length > 0)' \
      "$health") || die 'Rust health has no last_success_at'
    legacy_success_at=$(jq -er '.last_success_at | select(type == "string" and length > 0)' \
      "$legacy_health") || die 'Python health has no last_success_at'
    rust_success_epoch=$(date -u -d "$rust_success_at" +%s) \
      || die 'Rust last_success_at is invalid'
    legacy_success_epoch=$(date -u -d "$legacy_success_at" +%s) \
      || die 'Python last_success_at is invalid'
    now_epoch=$(date -u +%s)
    ((rust_success_epoch <= now_epoch && now_epoch - rust_success_epoch <= MAX_HEALTH_SILENCE_SECONDS)) \
      || die 'Rust last_success_at is stale or from the future'
    ((legacy_success_epoch <= now_epoch && now_epoch - legacy_success_epoch <= MAX_HEALTH_SILENCE_SECONDS)) \
      || die 'Python last_success_at is stale or from the future'
    common_cutoff=$rust_success_epoch
    ((legacy_success_epoch < common_cutoff)) && common_cutoff=$legacy_success_epoch
    if [[ $test_only == false ]]; then
      common_cutoff=$((common_cutoff - PARITY_CUTOFF_LAG_SECONDS))
    fi
    parity_window_started_at=$((common_cutoff - common_cutoff % 3600))
    ((parity_window_started_at >= started_at_unix)) \
      || parity_window_started_at=$started_at_unix
  fi

  if ((elapsed >= gate_seconds)) && [[ -n $common_cutoff ]]; then
    if [[ $test_only == true ]] \
      || ((common_cutoff - parity_window_started_at >= PARITY_TAIL_SECONDS)); then
      break
    fi
  fi

  sleep_for=$SAMPLE_SECONDS
  if ((elapsed < gate_seconds)); then
    remaining=$((gate_seconds - elapsed))
    ((remaining < sleep_for)) && sleep_for=$remaining
  fi
  sleep "$sleep_for"
done

observed_duration_seconds=$elapsed
[[ -n $common_cutoff && -n $parity_window_started_at ]] \
  || die 'no common successful collection cutoff was observed'
if [[ $test_only == false ]]; then
  ((observed_duration_seconds >= MINIMUM_GATE_SECONDS)) \
    || die 'production shadow duration is shorter than required'
  ((common_cutoff - parity_window_started_at >= PARITY_TAIL_SECONDS)) \
    || die 'production parity window has less than five minutes in one UTC hour'
fi

shadow_pid=$(systemctl show --property=MainPID --value "$shadow_unit")
[[ $shadow_pid =~ ^[1-9][0-9]*$ ]] || die 'Rust shadow has no final MainPID'
shadow_restarts=$(systemctl show --property=NRestarts --value "$shadow_unit")
[[ $shadow_restarts == 0 && $shadow_pid == "$initial_shadow_pid" ]] \
  || die 'Rust shadow did not remain a single continuous process'
verify_shadow_identity "$initial_shadow_pid" \
  || die 'final Rust shadow systemd identity differs from the gated candidate'
verify_legacy_identity "$legacy_pid" \
  || die 'legacy collector identity changed before parity evidence was captured'
shadow_exec_argv=$(effective_exec_argv "$shadow_unit") \
  || die 'could not capture the effective Rust shadow ExecStart'
shadow_cmdline=$(proc_cmdline "$initial_shadow_pid") \
  || die 'could not capture the exact Rust shadow command line'
shadow_cmdline_argv=${shadow_cmdline% }
shadow_fragment_path=$(systemctl show --property=FragmentPath --value "$shadow_unit")
shadow_drop_ins=$(systemctl show --property=DropInPaths --value "$shadow_unit")
shadow_drop_ins_json=$(jq -cn --arg value "$shadow_drop_ins" \
  '$value | split(" ") | map(select(length > 0))')
systemctl stop "$shadow_unit"

evidence_parent="$EVIDENCE_ROOT/$candidate_sha"
install -d -m 0755 /data/monday/evidence "$EVIDENCE_ROOT" "$evidence_parent"
for path in /data/monday/evidence "$EVIDENCE_ROOT" "$evidence_parent"; do
  direct_directory "$path" || die "evidence path is indirect: $path"
done
evidence_dir="$evidence_parent/$run_id"
mkdir -m 0750 "$evidence_dir" || die 'evidence run already exists'
parity_json="$evidence_dir/parity.json"
"$release_binary" verify-shadow-parity \
  --legacy-spool "$LEGACY_SPOOL" \
  --rust-spool "$shadow_spool" \
  --started-at-unix "$parity_window_started_at" \
  --ended-at-unix "$common_cutoff" \
  --output "$parity_json" || die 'byte/field/dedupe/settlement/rotation parity failed'

verify_current_oss_config
upload_json=$(runuser -u hftcollector -- env HOME=/var/lib/hft-collector \
  "$release_binary" upload \
  --spool-dir "$shadow_spool" \
  --dataset crypto_expiry_reference_rust_shadow \
  --quote-depth-levels 0 \
  --quote-sample-ms 0 \
  --bucket "$oss_bucket" \
  --endpoint "$oss_endpoint" \
  --region "$oss_region" \
  --profile "$aliyun_profile" \
  --zstd-timeout "$zstd_timeout_seconds" \
  --oss-timeout "$oss_copy_timeout_seconds") \
  || die 'shadow OSS upload/readback failed'
verify_current_oss_config
uploaded_segments=$(jq -er '.uploaded_segments | select(type == "number" and floor == . and . > 0)' \
  <<<"$upload_json") || die 'shadow uploader did not verify a closed segment'
canonical_uploaded_segments=$(jq -er \
  '.canonical_uploaded_segments | select(type == "number" and floor == . and . > 0)' \
  <<<"$upload_json") || die 'shadow uploader did not verify a canonical closed segment'

# Exercise the market-tape validation/upload/readback path with a deterministic,
# closed fixture. A content-addressed rerun must verify the same remote triplet.
market_fixture="$market_shadow_spool/market-updates.20000101T000003.ndjson"
jq -cn '{sequence:0,recorded_at:"2000-01-01T00:00:00Z",update:{
  kind:"event_discovered",event_id:"shadow-market-upload",symbol:"BTCUSDT",
  up_token:"shadow-up",down_token:"shadow-down",end_time:"2000-01-01T00:05:00Z",
  window_secs:300,price_to_beat:"100",resolved_up_won:null}}' >"$market_fixture"
jq -cn '{sequence:1,recorded_at:"2000-01-01T00:00:01Z",update:{
  kind:"quote",token_id:"shadow-up",bid:"0.49",ask:"0.51",
  bid_size:"10",ask_size:"11",bid_levels:[{price:"0.49",size:"10"}],
  ask_levels:[{price:"0.51",size:"11"}],ts:"2000-01-01T00:00:01Z"}}' \
  >>"$market_fixture"
jq -cn '{sequence:2,recorded_at:"2000-01-01T00:00:02Z",update:{
  kind:"reference_price",symbol:"BTCUSDT",source:"binance",asset_class:"crypto",
  price:"100",full_accuracy_value:null,is_carried_forward:false,
  ts:"2000-01-01T00:00:02Z"}}' >>"$market_fixture"
chown hftcollector:hftcollector "$market_fixture"
chmod 0640 "$market_fixture"
sync "$market_fixture"

verify_current_oss_config
market_upload_json=$(runuser -u hftcollector -- env HOME=/var/lib/hft-collector \
  "$release_binary" upload \
  --spool-dir "$market_shadow_spool" \
  --dataset crypto_expiry_market_rust_shadow \
  --quote-depth-levels 0 \
  --quote-sample-ms 1000 \
  --bucket "$oss_bucket" \
  --endpoint "$oss_endpoint" \
  --region "$oss_region" \
  --profile "$aliyun_profile" \
  --zstd-timeout "$zstd_timeout_seconds" \
  --oss-timeout "$oss_copy_timeout_seconds") \
  || die 'market-tape shadow OSS upload/readback failed'
verify_current_oss_config
market_uploaded_segments=$(jq -er \
  '.uploaded_segments | select(type == "number" and floor == . and . > 0)' \
  <<<"$market_upload_json") \
  || die 'market-tape shadow uploader did not verify a closed segment'
market_canonical_uploaded_segments=$(jq -er \
  '.canonical_uploaded_segments | select(type == "number" and floor == . and . > 0)' \
  <<<"$market_upload_json") \
  || die 'market-tape shadow uploader did not verify a canonical closed segment'

verify_legacy_identity "$legacy_pid" \
  || die 'legacy collector identity changed while parity or OSS readback was running'
verify_current_oss_config
legacy_exec_argv=$(effective_exec_argv "$LEGACY_UNIT") \
  || die 'could not capture the effective legacy ExecStart'
legacy_cmdline=$(proc_cmdline "$legacy_pid") \
  || die 'could not capture the exact legacy command line'
legacy_cmdline_argv=${legacy_cmdline% }
legacy_cmdline_sha=$(printf '%s' "$legacy_cmdline_argv" | sha256sum | awk '{print $1}')
legacy_fragment_path=$(systemctl show --property=FragmentPath --value "$LEGACY_UNIT")
legacy_drop_ins=$(systemctl show --property=DropInPaths --value "$LEGACY_UNIT")
legacy_drop_ins_json=$(jq -cn --arg value "$legacy_drop_ins" \
  '$value | split(" ") | map(select(length > 0))')
legacy_restarts=$(systemctl show --property=NRestarts --value "$LEGACY_UNIT")

completed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
production_eligible=true
[[ $test_only == false ]] || production_eligible=false
gate_tmp="$evidence_dir/.gate.json.tmp"
gate_json="$evidence_dir/gate.json"
jq \
  --arg candidate_sha256 "$candidate_sha" \
  --arg deployment_bundle_sha256 "$deployment_bundle_sha" \
  --arg deployment_source_revision "$source_revision" \
  --arg oss_config_sha256 "$oss_config_sha" \
  --arg started_at "$started_at" \
  --arg completed_at "$completed_at" \
  --arg legacy_exec "$legacy_exec_argv" \
  --arg legacy_cmdline "$legacy_cmdline_argv" \
  --arg legacy_cmdline_sha256 "$legacy_cmdline_sha" \
  --arg legacy_fragment_path "$legacy_fragment_path" \
  --argjson legacy_drop_in_paths "$legacy_drop_ins_json" \
  --argjson legacy_pid "$legacy_pid" \
  --argjson legacy_restarts "$legacy_restarts" \
  --arg shadow_exec "$shadow_exec_argv" \
  --arg shadow_cmdline "$shadow_cmdline_argv" \
  --arg shadow_fragment_path "$shadow_fragment_path" \
  --argjson shadow_drop_in_paths "$shadow_drop_ins_json" \
  --argjson shadow_pid "$shadow_pid" \
  --argjson shadow_restarts "$shadow_restarts" \
  --arg shadow_run_id "$run_id" \
  --argjson duration_seconds "$observed_duration_seconds" \
  --argjson parity_window_started_at_unix "$parity_window_started_at" \
  --argjson parity_window_ended_at_unix "$common_cutoff" \
  --argjson production_eligible "$production_eligible" \
  --argjson uploaded_segments "$uploaded_segments" \
  --argjson canonical_uploaded_segments "$canonical_uploaded_segments" \
  --argjson market_uploaded_segments "$market_uploaded_segments" \
  --argjson market_canonical_uploaded_segments "$market_canonical_uploaded_segments" \
  '. + {
    schema:"monday.polymarket_shadow_gate.v1",
    candidate_sha256:$candidate_sha256,
    deployment_bundle_sha256:$deployment_bundle_sha256,
    deployment_source_revision:$deployment_source_revision,
    oss_config_sha256:$oss_config_sha256,
    started_at:$started_at,
    completed_at:$completed_at,
    shadow_run_id:$shadow_run_id,
    duration_seconds:$duration_seconds,
    parity_window_started_at_unix:$parity_window_started_at_unix,
    parity_window_ended_at_unix:$parity_window_ended_at_unix,
    production_eligible:$production_eligible,
    legacy_runtime:{exec_start:$legacy_exec,cmdline:$legacy_cmdline,
      cmdline_sha256:$legacy_cmdline_sha256,
      fragment_path:$legacy_fragment_path,drop_in_paths:$legacy_drop_in_paths,
      main_pid:$legacy_pid,restarts:$legacy_restarts},
    shadow_runtime:{exec_start:$shadow_exec,cmdline:$shadow_cmdline,
      fragment_path:$shadow_fragment_path,drop_in_paths:$shadow_drop_in_paths,
      main_pid:$shadow_pid,restarts:$shadow_restarts},
    checks:(.checks + {
      health_freshness:true,
      candidate_identity:true,
      oss_readback_parity:true,
      market_oss_readback_parity:true
    }),
    metrics:(.metrics + {
      oss_uploaded_segments:$uploaded_segments,
      oss_canonical_uploaded_segments:$canonical_uploaded_segments,
      market_oss_uploaded_segments:$market_uploaded_segments,
      market_oss_canonical_uploaded_segments:$market_canonical_uploaded_segments
    })
  } | .passed = (.passed and ([.checks[]] | all))' \
  "$parity_json" >"$gate_tmp"
mv "$gate_tmp" "$gate_json"
sync "$gate_json"

if [[ $production_eligible == true ]]; then
  verify_legacy_identity "$legacy_pid" \
    || die 'legacy collector identity changed before the gate marker was published'
  verify_current_oss_config
  jq -e -f "$GATE_POLICY" "$gate_json" >/dev/null \
    || die 'combined gate evidence failed the production policy'
  marker="$evidence_dir/PASSED.sha256"
  (
    cd "$evidence_dir"
    sha256sum gate.json >".${marker##*/}.tmp"
    mv ".${marker##*/}.tmp" "${marker##*/}"
  )
fi

printf '%s\n' "$gate_json"
