#!/usr/bin/env bash
set -Eeuo pipefail

umask 027
export LC_ALL=C

readonly RUN_SPOOL_ROOT=/data/monday/spool/binance-lob-rust-shadow/runs
readonly OVERRIDE_ROOT=/run/monday

run_spool_dir() {
  local candidate=$1 run_id=$2 market=$3
  [[ $candidate =~ ^[a-f0-9]{64}$ && $run_id =~ ^[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*$ ]]
  [[ $market == spot || $market == usdm ]]
  printf '%s/%s/%s/%s\n' "$RUN_SPOOL_ROOT" "$candidate" "$run_id" "$market"
}

reanchor_recovery() {
  local market=$1 now_iso=$2 now_mono=$3 now_ns=$4
  recovery_active[$market]=true
  recovery_started_mono[$market]=$now_mono
  recovery_started_iso[$market]=$now_iso
  recovery_started_ns[$market]=$now_ns
  recovery_last_transport_iso[$market]=$now_iso
}

self_test() {
  local candidate=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
  local run_id=20260812T000000Z-1
  declare -gA recovery_active recovery_started_mono recovery_started_iso recovery_started_ns
  declare -gA recovery_last_transport_iso
  [[ $(run_spool_dir "$candidate" "$run_id" usdm) == \
    "$RUN_SPOOL_ROOT/$candidate/$run_id/usdm" ]]
  reanchor_recovery usdm 2026-08-12T00:00:00Z 10 100
  reanchor_recovery usdm 2026-08-12T00:00:05Z 15 150
  [[ ${recovery_started_mono[usdm]} == 15 ]]
  [[ ${recovery_started_ns[usdm]} == 150 ]]
  [[ ${recovery_last_transport_iso[usdm]} == 2026-08-12T00:00:05Z ]]
  printf 'shadow-soak self-test: ok\n'
}

if [[ ${1:-} == --self-test ]]; then
  self_test
  exit 0
fi
readonly CORRECTNESS_SECONDS=300
readonly CORRECTNESS_SEGMENT_SECONDS=90
RUN_MODE=stability
PREFLIGHT_RECEIPT=
if [[ ${1:-} == --correctness ]]; then
  [[ $# -eq 3 ]] || {
    printf 'usage: monday-rust-lob-shadow-soak --correctness <candidate-sha256> <preflight-receipt>\n' >&2
    exit 2
  }
  RUN_MODE=correctness
  CANDIDATE_SHA256=$2
  PREFLIGHT_RECEIPT=$3
  SOAK_SECONDS=$CORRECTNESS_SECONDS
else
  [[ $# -ge 1 && $# -le 2 ]] || {
    printf 'usage: monday-rust-lob-shadow-soak <candidate-sha256> [seconds]\n' >&2
    exit 2
  }
  CANDIDATE_SHA256=$1
  SOAK_SECONDS=${2:-1800}
fi
readonly RUN_MODE CANDIDATE_SHA256 PREFLIGHT_RECEIPT SOAK_SECONDS
readonly MIN_STABILITY_SOAK_SECONDS=1201
readonly MIN_READBACK_SEGMENTS=2
(( CORRECTNESS_SECONDS >= 3 * CORRECTNESS_SEGMENT_SECONDS )) \
  || {
    printf 'correctness observation budget must cover two complete post-start segments\n' >&2
    exit 2
  }
[[ $CANDIDATE_SHA256 =~ ^[a-f0-9]{64}$ ]] || {
  printf 'candidate SHA must be 64 lowercase hexadecimal characters\n' >&2
  exit 2
}
if [[ $RUN_MODE == stability ]]; then
  [[ $SOAK_SECONDS =~ ^[1-9][0-9]*$ && $SOAK_SECONDS -ge $MIN_STABILITY_SOAK_SECONDS \
    && $SOAK_SECONDS -le 7200 ]] || {
    printf 'duration must be %s..7200 seconds for two observation segments\n' \
      "$MIN_STABILITY_SOAK_SECONDS" >&2
    exit 2
  }
fi

readonly BOOTSTRAP_SETTLE_SECONDS=900
readonly RECOVERY_SETTLE_SECONDS=300
readonly SAMPLE_INTERVAL_SECONDS=5
readonly MAX_HEALTH_SILENCE_SECONDS=120
readonly TOTAL_FEED_SECONDS=$((SOAK_SECONDS + BOOTSTRAP_SETTLE_SECONDS + 300))
readonly FATAL_JOURNAL_REGEX='process watchdog|market-data stall|session failed|source-to-receive|raw trade field|parser|sequence gap'
readonly TRANSPORT_JOURNAL_REGEX='websocket.*(reset|disconnect)|websocket.*reconnect|reconnecting'

readonly RELEASE_ROOT=/opt/monday/releases/binance-lob-archiver
readonly SHADOW_LINK=/opt/monday/bin/binance-lob-archiver-shadow
readonly PRODUCTION_LINK=/opt/monday/bin/binance-lob-archiver
readonly EVIDENCE_ROOT=/data/monday/evidence/rust-lob-soaks
readonly PREFLIGHT_EVIDENCE_ROOT=/data/monday/evidence/rust-lob-shadow-preflights
readonly LOCK_FILE=/run/lock/monday-rust-lob-release.lock
readonly SERVICE_USER=hftcollector
readonly SERVICE_HOME=/var/lib/hft-collector
readonly SAFE_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

readonly -a MARKETS=(spot usdm)
readonly -a PRODUCTION_UNITS=(
  binance-lob-archiver-production@spot.service
  binance-lob-archiver-production@usdm.service
)

die() {
  printf 'rust LOB soak failed: %s\n' "$*" >&2
  exit 1
}

for command in aliyun awk chmod chown cmp cp date df dirname env find flock grep head hostname \
  id install jq journalctl mktemp mountpoint readelf readlink rm runuser sed sha256sum sleep \
  sort stat systemctl systemd-run tail tr wc zstd; do
  command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done

[[ ${EUID:-$(id -u)} -eq 0 ]] || die 'must run as root'
[[ -d /data && ! -L /data ]] || die '/data must be a direct directory'
mountpoint -q /data || die '/data must be a mount point'
data_free_kb=$(df -Pk /data | awk 'NR == 2 {print $4}')
[[ $data_free_kb =~ ^[0-9]+$ && $data_free_kb -ge 20971520 ]] \
  || die "/data has less than 20 GiB free: ${data_free_kb:-unknown} KiB"
id "$SERVICE_USER" >/dev/null 2>&1 || die "missing service user: $SERVICE_USER"
[[ -r /proc/uptime ]] || die '/proc/uptime is required for monotonic timing'

tmp_dir=
run_resources_started=false
cleanup_partial() {
  local path
  set +e
  [[ -z ${tmp_dir:-} ]] || rm -rf -- "$tmp_dir"
  if [[ ${run_resources_started:-false} == true ]]; then
    for path in "${evidence_dir:-}" "${run_spool_path:-}"; do
      [[ -n $path ]] || continue
      rm -rf -- "$path"
    done
  fi
}
trap cleanup_partial EXIT
tmp_dir=$(mktemp -d)
chown "$SERVICE_USER:$SERVICE_USER" "$tmp_dir"
chmod 0750 "$tmp_dir"

candidate_release="$RELEASE_ROOT/$CANDIDATE_SHA256"
candidate_binary="$candidate_release/binance-lob-archiver"
candidate_deployment="$candidate_release/deployment"
candidate_release_json="$candidate_release/release.json"
control_plane_lib="$candidate_deployment/rust-lob-control-plane-lib.sh"

direct_directory() {
  local path=$1
  [[ -d $path && ! -L $path ]] || return 1
  [[ $(readlink -f -- "$path") == "$path" ]]
}

assert_no_symlink_ancestors() {
  local cursor=$1
  while [[ $cursor != / ]]; do
    if [[ -e $cursor || -L $cursor ]]; then
      [[ -d $cursor && ! -L $cursor ]] \
        || die "path ancestor is missing, not a directory, or a symlink: $cursor"
    fi
    cursor=${cursor%/*}
    [[ -n $cursor ]] || cursor=/
  done
}

secure_regular_file() {
  local path=$1 mode owner
  [[ -f $path && ! -L $path ]] || die "required regular file is missing or a symlink: $path"
  owner=$(stat -c %u -- "$path")
  mode=$(stat -c %a -- "$path")
  [[ $owner == 0 ]] || die "required file is not root-owned: $path"
  (( (8#$mode & 022) == 0 )) || die "required file is group/world writable: $path"
}

for path in /opt/monday /opt/monday/bin "$RELEASE_ROOT" "$candidate_release" \
  "$candidate_deployment"; do
  direct_directory "$path" || die "release path is missing, indirect, or a symlink: $path"
done
secure_regular_file "$candidate_release_json"
secure_regular_file "$control_plane_lib"
# shellcheck disable=SC1090
. "$control_plane_lib"

BUNDLE_SHA256=$(jq -er '.deployment_bundle_sha256' "$candidate_release_json")
SOURCE_REVISION=$(jq -er '.deployment_source_revision' "$candidate_release_json")
CANDIDATE_SIZE_BYTES=$(stat -c '%s' "$candidate_binary")
CANDIDATE_BUILD_ID=$(readelf -n "$candidate_binary" \
  | sed -n 's/.*Build ID: //p' | head -n1)
readonly BUNDLE_SHA256 SOURCE_REVISION CANDIDATE_SIZE_BYTES CANDIDATE_BUILD_ID
[[ $BUNDLE_SHA256 =~ ^[a-f0-9]{64}$ && $SOURCE_REVISION =~ ^[a-f0-9]{7,64}$ \
  && $CANDIDATE_SIZE_BYTES =~ ^[1-9][0-9]*$ && $CANDIDATE_BUILD_ID =~ ^[a-f0-9]+$ ]] \
  || die 'candidate release identity is malformed'
jq -e --arg artifact "$CANDIDATE_SHA256" --arg bundle "$BUNDLE_SHA256" \
  '.artifact_sha256 == $artifact and .deployment_bundle_sha256 == $bundle' \
  "$candidate_release_json" >/dev/null || die 'candidate release metadata mismatch'
if [[ $RUN_MODE == correctness ]]; then
  preflight_receipt_canonical=$(readlink -f -- "$PREFLIGHT_RECEIPT" 2>/dev/null || true)
  [[ $PREFLIGHT_RECEIPT == "$preflight_receipt_canonical" ]] \
    || die 'preflight receipt path is not canonical'
  [[ $PREFLIGHT_RECEIPT =~ ^$PREFLIGHT_EVIDENCE_ROOT/$CANDIDATE_SHA256/[0-9]{8}T[0-9]{6}Z-[1-9][0-9]*/preflight\.json$ ]] \
    || die 'preflight receipt path is outside the canonical evidence root'
  preflight_receipt_run_root=${PREFLIGHT_RECEIPT%/preflight.json}
  preflight_run_id=${preflight_receipt_run_root##*/}
  preflight_triplet_root=$(jq -er '.triplet_root' "$PREFLIGHT_RECEIPT" 2>/dev/null || true)
  [[ -n $preflight_triplet_root ]] || die 'preflight triplet root is missing'
  preflight_triplet_root_canonical=$(readlink -f -- "$preflight_triplet_root" 2>/dev/null || true)
  [[ $preflight_triplet_root == "$preflight_triplet_root_canonical" ]] \
    || die 'preflight triplet root is not canonical'
  direct_directory "$preflight_triplet_root" \
    || die 'preflight triplet root is not a direct directory'
  assert_no_symlink_ancestors "$preflight_triplet_root"
  [[ -d $preflight_receipt_run_root && ! -L $preflight_receipt_run_root ]] \
    || die 'preflight receipt run root is not a direct directory'
  [[ -f $PREFLIGHT_RECEIPT && ! -L $PREFLIGHT_RECEIPT ]] \
    || die 'correctness preflight receipt is missing or a symlink'
  secure_regular_file "$PREFLIGHT_RECEIPT"
  jq -e --arg candidate "$CANDIDATE_SHA256" --arg source "$SOURCE_REVISION" \
    --arg bundle "$BUNDLE_SHA256" --arg build "$CANDIDATE_BUILD_ID" \
    --arg run_id "$preflight_run_id" \
    --arg triplet_root "$preflight_triplet_root" \
    '.schema == "monday.rust_lob_shadow_preflight.v1" and .result == "passed"
     and .formal_gate == false and .cutover == false and .live == false
     and .candidate_sha256 == $candidate and .source_revision == $source
     and .deployment_bundle_sha256 == $bundle and .build_id == $build
     and .run_id == $run_id
     and .triplet_root == $triplet_root
     and (.expected_replay_identity_sha256 == .replay_identity_sha256)
     and (.replay_identity_sha256|test("^[a-f0-9]{64}$"))
     and (.triplet_identity_sha256|test("^[a-f0-9]{64}$"))
     and .checks.candidate_identity == true and .checks.sealed_triplets == true
     and .checks.strict_verifier == true and .checks.lob_continuity == true
     and .checks.aggregate_trade_continuity == true and .checks.raw_trade_continuity == true' \
    "$PREFLIGHT_RECEIPT" >/dev/null \
    || die 'correctness preflight receipt does not match the frozen candidate identity'
  preflight_replay_identity=$(find "$preflight_triplet_root" -type f \( -name '*.jsonl.zst' -o -name '*.manifest.json' -o -name '*._SUCCESS' \) \
    -print0 | sort -z | xargs -0 sha256sum | sha256sum | awk '{print $1}')
  receipt_replay_identity=$(jq -er '.replay_identity_sha256' "$PREFLIGHT_RECEIPT")
  receipt_expected_replay_identity=$(jq -er '.expected_replay_identity_sha256' "$PREFLIGHT_RECEIPT")
  [[ $preflight_replay_identity == "$receipt_replay_identity" ]] \
    || die 'preflight sealed-triplet identity changed after receipt'
  [[ $receipt_expected_replay_identity == "$receipt_replay_identity" ]] \
    || die 'preflight receipt is not bound to its reviewed corpus digest'
  preflight_triplet_identity=$(printf '%s\n' "$preflight_triplet_root" "$preflight_replay_identity" \
    | sha256sum | awk '{print $1}')
  receipt_triplet_identity=$(jq -er '.triplet_identity_sha256' "$PREFLIGHT_RECEIPT")
  [[ $preflight_triplet_identity == "$receipt_triplet_identity" ]] \
    || die 'preflight sealed-triplet root binding changed after receipt'
  PREFLIGHT_RECEIPT_SHA256=$(sha256sum "$PREFLIGHT_RECEIPT" | awk '{print $1}')
  readonly PREFLIGHT_RECEIPT_SHA256
fi
[[ -f $candidate_binary && -x $candidate_binary ]] || die 'candidate binary is not executable'
secure_regular_file "$candidate_binary"
printf '%s  %s\n' "$CANDIDATE_SHA256" "$candidate_binary" \
  | sha256sum --check --strict >/dev/null
[[ -L $SHADOW_LINK ]] || die 'shadow symlink is missing'
[[ $(readlink -f -- "$SHADOW_LINK") == "$candidate_binary" ]] \
  || die 'shadow symlink does not point to the candidate'
runuser --user "$SERVICE_USER" -- "$candidate_binary" --self-test >/dev/null
"$candidate_binary" --help | grep -Fq -- '--upload-only' \
  || die 'candidate does not expose --upload-only'

env_value() {
  local file=$1 key=$2 count value
  count=$(grep -c "^${key}=" "$file" || true)
  [[ $count -eq 1 ]] || die "$file must contain exactly one ${key}= entry"
  value=$(sed -n "s/^${key}=//p" "$file")
  [[ -n $value ]] || die "$file has an empty $key"
  printf '%s\n' "$value"
}

evidence_run_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
declare -A env_file base_spool_dir spool_dir override_file dataset shard_id oss_bucket oss_endpoint oss_region
declare -A aliyun_profile oss_copy_timeout unit min_symbols expected_stream_types
for market in "${MARKETS[@]}"; do
  env_file[$market]="/etc/monday/binance-lob-archiver-rust-${market}.env"
  [[ -f ${env_file[$market]} ]] || die "missing ${env_file[$market]}"
  [[ $(env_value "${env_file[$market]}" MARKET) == "$market" ]] \
    || die "${env_file[$market]} has the wrong MARKET"
  [[ $(env_value "${env_file[$market]}" SYMBOLS) == ALL ]] \
    || die "${env_file[$market]} must set SYMBOLS=ALL"
  shard_id[$market]=$(env_value "${env_file[$market]}" SHARD_ID)
  [[ ${shard_id[$market]} == all ]] || die "${env_file[$market]} must set SHARD_ID=all"
  base_spool_dir[$market]=$(env_value "${env_file[$market]}" SPOOL_DIR)
  spool_dir[$market]=$(run_spool_dir "$CANDIDATE_SHA256" "$evidence_run_id" "$market")
  override_file[$market]="$OVERRIDE_ROOT/binance-lob-archiver-rust-${market}-soak.env"
  dataset[$market]=$(env_value "${env_file[$market]}" DATASET)
  oss_bucket[$market]=$(env_value "${env_file[$market]}" OSS_BUCKET)
  oss_endpoint[$market]=$(env_value "${env_file[$market]}" OSS_ENDPOINT)
  oss_region[$market]=$(env_value "${env_file[$market]}" OSS_REGION)
  aliyun_profile[$market]=$(env_value "${env_file[$market]}" ALIYUN_PROFILE)
  oss_copy_timeout[$market]=$(env_value "${env_file[$market]}" OSS_COPY_TIMEOUT_SECONDS)
  [[ ${oss_copy_timeout[$market]} =~ ^[1-9][0-9]*$ ]] \
    || die "${env_file[$market]} has invalid OSS_COPY_TIMEOUT_SECONDS"
  [[ ${oss_bucket[$market]} =~ ^[A-Za-z0-9][A-Za-z0-9.-]*$ ]] \
    || die "${env_file[$market]} has invalid OSS_BUCKET"
  [[ ${oss_region[$market]} == ap-northeast-1 ]] \
    || die "${env_file[$market]} must use Tokyo OSS region"
  [[ ${oss_endpoint[$market]} == oss-ap-northeast-1-internal.aliyuncs.com ]] \
    || die "${env_file[$market]} must use Tokyo internal OSS endpoint"
  [[ ${aliyun_profile[$market]} == ecs-role ]] \
    || die "${env_file[$market]} must use the ECS RAM-role profile"
  unit[$market]="binance-lob-archiver-rust@${market}.service"
done
min_symbols[spot]=1000
min_symbols[usdm]=400
expected_stream_types[spot]='["aggTrade","bookTicker","depth@100ms","trade"]'
expected_stream_types[usdm]='["aggTrade","bookTicker","depth@100ms","forceOrder","trade"]'

for asset in \
  binance-lob-archiver-rust@.service \
  binance-lob-archiver-rust-upload@.service \
  binance-lob-archiver-rust-spot.env \
  binance-lob-archiver-rust-usdm.env; do
  secure_regular_file "$candidate_deployment/$asset"
  case "$asset" in
    *.service) installed_asset="/etc/systemd/system/$asset" ;;
    *.env) installed_asset="/etc/monday/$asset" ;;
  esac
  secure_regular_file "$installed_asset"
  cmp -s "$candidate_deployment/$asset" "$installed_asset" \
    || die "installed shadow asset differs: $asset"
done
asset=host-rust-lob-shadow-soak.sh
installed_asset=/opt/monday/bin/monday-rust-lob-shadow-soak
secure_regular_file "$candidate_deployment/$asset"
secure_regular_file "$installed_asset"
cmp -s "$candidate_deployment/$asset" "$installed_asset" \
  || die "installed shadow asset differs: $asset"
asset=host-rust-lob-shadow-preflight.sh
installed_asset=/opt/monday/bin/monday-rust-lob-shadow-preflight
secure_regular_file "$candidate_deployment/$asset"
secure_regular_file "$installed_asset"
cmp -s "$candidate_deployment/$asset" "$installed_asset" \
  || die "installed shadow asset differs: $asset"

[[ ${base_spool_dir[spot]} == /data/monday/spool/binance-lob-rust-shadow/spot ]] \
  || die 'Spot base shadow spool path is not isolated'
[[ ${base_spool_dir[usdm]} == /data/monday/spool/binance-lob-rust-shadow/usdm ]] \
  || die 'USD-M base shadow spool path is not isolated'
for path in /data/monday/spool/binance-lob-rust-shadow \
  "${base_spool_dir[spot]}" "${base_spool_dir[usdm]}"; do
  direct_directory "$path" || die "shadow spool is missing or indirect: $path"
done
[[ ${dataset[spot]} == spot_all_rust_shadow ]] || die 'Spot dataset is not isolated'
[[ ${dataset[usdm]} == usdm_perpetual_all_rust_shadow ]] \
  || die 'USD-M dataset is not isolated'

assert_spool_empty() {
  local market=$1 remaining
  remaining=$(find "${spool_dir[$market]}" \( -type f -o -type l \) \( \
    -name '*.manifest.json' -o -name '*.jsonl.part' -o -name '*.zst.tmp' -o \
    -name '*.part.corrupt' -o -name '*.jsonl.zst' -o -name '*._SUCCESS' -o \
    -name '*.uploaded-cleanup.json' -o -name '*.uploaded-cleanup.json.tmp' \
    \) -print -quit)
  [[ -z $remaining ]] || die "$market shadow spool already contains: $remaining"
}

assert_shadow_units_quiescent() {
  [[ $(systemctl is-active "${unit[spot]}" 2>/dev/null || true) == inactive ]] \
    || die 'Spot shadow primary is not inactive before soak'
  [[ $(systemctl is-active "${unit[usdm]}" 2>/dev/null || true) == inactive ]] \
    || die 'USD-M shadow primary is not inactive before soak'
  [[ $(systemctl is-enabled "${unit[spot]}" 2>/dev/null || true) == disabled ]] \
    || die 'Spot shadow primary is not disabled before soak'
  [[ $(systemctl is-enabled "${unit[usdm]}" 2>/dev/null || true) == disabled ]] \
    || die 'USD-M shadow primary is not disabled before soak'
  for upload_unit in binance-lob-archiver-rust-upload@spot.service \
    binance-lob-archiver-rust-upload@usdm.service; do
    [[ $(systemctl is-active "$upload_unit" 2>/dev/null || true) == inactive ]] \
      || die "$upload_unit is active before soak"
    [[ $(systemctl is-enabled "$upload_unit" 2>/dev/null || true) == static ]] \
      || die "$upload_unit is not static before soak"
  done
}

systemctl_prop() {
  local unit_name=$1 property=$2
  systemctl show "$unit_name" --property="$property" --value
}

capture_production_fingerprint() {
  local output=$1 binary binary_sha production_sha release_json source bundle
  local spot_active spot_enabled spot_pid spot_invocation spot_restarts
  local usdm_active usdm_enabled usdm_pid usdm_invocation usdm_restarts
  [[ -L $PRODUCTION_LINK ]] || die 'production symlink is missing'
  binary=$(readlink -f -- "$PRODUCTION_LINK")
  [[ $binary =~ ^$RELEASE_ROOT/([a-f0-9]{64})/binance-lob-archiver$ ]] \
    || die "production symlink is not digest addressed: $binary"
  production_sha=${BASH_REMATCH[1]}
  [[ -f $binary && -x $binary ]] || die 'production binary is not executable'
  binary_sha=$(sha256sum "$binary" | awk '{print $1}')
  [[ $binary_sha == "$production_sha" ]] || die 'production binary hash mismatch'
  release_json="$RELEASE_ROOT/$production_sha/release.json"
  secure_regular_file "$release_json"
  source=$(jq -er '.deployment_source_revision' "$release_json")
  bundle=$(jq -er '.deployment_bundle_sha256' "$release_json")
  spot_active=$(systemctl is-active "${PRODUCTION_UNITS[0]}" 2>/dev/null || true)
  spot_enabled=$(systemctl is-enabled "${PRODUCTION_UNITS[0]}" 2>/dev/null || true)
  spot_pid=$(systemctl_prop "${PRODUCTION_UNITS[0]}" MainPID)
  spot_invocation=$(systemctl_prop "${PRODUCTION_UNITS[0]}" InvocationID)
  spot_restarts=$(systemctl_prop "${PRODUCTION_UNITS[0]}" NRestarts)
  usdm_active=$(systemctl is-active "${PRODUCTION_UNITS[1]}" 2>/dev/null || true)
  usdm_enabled=$(systemctl is-enabled "${PRODUCTION_UNITS[1]}" 2>/dev/null || true)
  usdm_pid=$(systemctl_prop "${PRODUCTION_UNITS[1]}" MainPID)
  usdm_invocation=$(systemctl_prop "${PRODUCTION_UNITS[1]}" InvocationID)
  usdm_restarts=$(systemctl_prop "${PRODUCTION_UNITS[1]}" NRestarts)
  jq -n \
    --arg link "$PRODUCTION_LINK" --arg binary "$binary" --arg binary_sha256 "$binary_sha" \
    --arg source "$source" --arg bundle "$bundle" \
    --arg spot_active "$spot_active" --arg spot_enabled "$spot_enabled" \
    --arg spot_pid "$spot_pid" --arg spot_invocation "$spot_invocation" \
    --arg spot_restarts "$spot_restarts" --arg usdm_active "$usdm_active" \
    --arg usdm_enabled "$usdm_enabled" --arg usdm_pid "$usdm_pid" \
    --arg usdm_invocation "$usdm_invocation" --arg usdm_restarts "$usdm_restarts" \
    '{production_link:$link,binary:$binary,binary_sha256:$binary_sha256,
      source_revision:$source,deployment_bundle_sha256:$bundle,
      spot:{unit:"binance-lob-archiver-production@spot.service",active:$spot_active,
        enabled:$spot_enabled,main_pid:$spot_pid,invocation_id:$spot_invocation,
        n_restarts:$spot_restarts},
      usdm:{unit:"binance-lob-archiver-production@usdm.service",active:$usdm_active,
        enabled:$usdm_enabled,main_pid:$usdm_pid,invocation_id:$usdm_invocation,
        n_restarts:$usdm_restarts}}' >"$output"
}

assert_production_baseline() {
  jq -e '
    .spot.active == "active" and .spot.enabled == "enabled"
    and (.spot.main_pid|test("^[1-9][0-9]*$"))
    and (.spot.invocation_id|length) > 0
    and (.spot.n_restarts|test("^[0-9]+$"))
    and .usdm.active == "active" and .usdm.enabled == "enabled"
    and (.usdm.main_pid|test("^[1-9][0-9]*$"))
    and (.usdm.invocation_id|length) > 0
    and (.usdm.n_restarts|test("^[0-9]+$"))' "$1" >/dev/null \
    || die 'production is not healthy enough to freeze a soak baseline'
}

evidence_dir="$EVIDENCE_ROOT/$CANDIDATE_SHA256/$evidence_run_id"
run_spool_path="$RUN_SPOOL_ROOT/$CANDIDATE_SHA256/$evidence_run_id"
direct_directory "/data/monday" || die '/data/monday is not a direct directory'
direct_directory "/data/monday/evidence" || die '/data/monday/evidence is not a direct directory'
assert_no_symlink_ancestors "$run_spool_path"
[[ ! -e $evidence_dir && ! -L $evidence_dir ]] || die 'soak evidence path already exists'
[[ ! -e $run_spool_path && ! -L $run_spool_path ]] || die 'run-scoped spool already exists'
run_resources_started=true
install -d -m 0750 "$EVIDENCE_ROOT" "$EVIDENCE_ROOT/$CANDIDATE_SHA256" "$evidence_dir"
direct_directory "$evidence_dir" || die 'soak evidence path is indirect'
install -d -m 0755 -o root -g root \
  "$RUN_SPOOL_ROOT" "$RUN_SPOOL_ROOT/$CANDIDATE_SHA256"
install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" \
  "$RUN_SPOOL_ROOT/$CANDIDATE_SHA256/$evidence_run_id" \
  "${spool_dir[spot]}" "${spool_dir[usdm]}"
for market in "${MARKETS[@]}"; do
  direct_directory "${spool_dir[$market]}" || die "$market run spool is indirect"
  [[ ! -e ${override_file[$market]} && ! -L ${override_file[$market]} ]] \
    || die "$market soak environment override already exists"
done
[[ ! -e "$evidence_dir/gate.json" && ! -e "$evidence_dir/PASSED.sha256" ]] \
  || die 'soak evidence path unexpectedly contains a formal gate marker'

baseline_fingerprint="$evidence_dir/production-baseline.json"
current_fingerprint="$tmp_dir/production-current.json"
install -d -m 0755 "$(dirname "$LOCK_FILE")"
exec 9>"$LOCK_FILE"
flock -n 9 || die 'another Rust collector release operation holds the lock'
capture_production_fingerprint "$baseline_fingerprint"
assert_production_baseline "$baseline_fingerprint"
assert_shadow_units_quiescent
for market in "${MARKETS[@]}"; do assert_spool_empty "$market"; done
capture_production_fingerprint "$current_fingerprint"
cmp -s "$baseline_fingerprint" "$current_fingerprint" \
  || die 'production changed while acquiring the release lock'
cp "$current_fingerprint" "$evidence_dir/production-prestart.json"
df -Pk /data >"$evidence_dir/data-df-prestart.txt"

run_created_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
jq -n --arg run_id "$evidence_run_id" --arg created_at "$run_created_at" \
  --arg host "$(hostname)" \
  --arg source "$SOURCE_REVISION" --arg candidate "$CANDIDATE_SHA256" \
  --arg bundle "$BUNDLE_SHA256" --argjson candidate_size_bytes "$CANDIDATE_SIZE_BYTES" \
  --arg candidate_build_id "$CANDIDATE_BUILD_ID" --argjson soak_seconds "$SOAK_SECONDS" \
  --arg run_mode "$RUN_MODE" --arg preflight_receipt "${PREFLIGHT_RECEIPT:-}" \
  --arg preflight_receipt_sha256 "${PREFLIGHT_RECEIPT_SHA256:-}" \
  --argjson sample_interval_seconds "$SAMPLE_INTERVAL_SECONDS" \
  '{schema:"monday.rust_lob_shadow_soak_run.v1",run_id:$run_id,created_at:$created_at,
    soak_only:true,formal_gate:false,cutover:false,live:false,
    host:$host,
    source_revision:$source,candidate_sha256:$candidate,
    candidate_size_bytes:$candidate_size_bytes,candidate_build_id:$candidate_build_id,
    deployment_bundle_sha256:$bundle,run_mode:$run_mode,soak_seconds:$soak_seconds,
    preflight_receipt:(if $preflight_receipt == "" then null else $preflight_receipt end),
    preflight_receipt_sha256:(if $preflight_receipt_sha256 == "" then null else $preflight_receipt_sha256 end),
    sample_interval_seconds:$sample_interval_seconds}' >"$evidence_dir/run.json"
chmod 0640 "$evidence_dir/run.json"
: >"$evidence_dir/watchdog-events.ndjson"
: >"$evidence_dir/producer-diagnostics.ndjson"
chmod 0640 "$evidence_dir/watchdog-events.ndjson" "$evidence_dir/producer-diagnostics.ndjson"

monotonic_seconds() { awk '{print int($1)}' /proc/uptime; }

assert_candidate_stable() {
  [[ -L $SHADOW_LINK ]] || die 'shadow candidate symlink disappeared'
  [[ $(readlink -f -- "$SHADOW_LINK") == "$candidate_binary" ]] \
    || die 'shadow candidate symlink changed during soak'
  printf '%s  %s\n' "$CANDIDATE_SHA256" "$SHADOW_LINK" \
    | sha256sum --check --strict >/dev/null
}

declare -A expected_shadow_pid expected_shadow_invocation
capture_shadow_start_identity() {
  local market=$1 service=${unit[$1]} pid invocation restarts
  [[ $(systemctl is-active "$service" 2>/dev/null || true) == active ]] \
    || die "$market shadow primary is not active"
  [[ $(systemctl show "$service" --property=SubState --value) == running ]] \
    || die "$market shadow primary is not running"
  pid=$(systemctl_prop "$service" MainPID)
  invocation=$(systemctl_prop "$service" InvocationID)
  restarts=$(systemctl_prop "$service" NRestarts)
  [[ $pid =~ ^[1-9][0-9]*$ && -n $invocation && $restarts == 0 ]] \
    || die "$market shadow primary has invalid startup identity"
  expected_shadow_pid[$market]=$pid
  expected_shadow_invocation[$market]=$invocation
}

health_passes() {
  local market=$1 health="${spool_dir[$1]}/health.json"
  [[ -f $health && ! -L $health ]] || return 1
  jq -e \
    --arg market "$market" --arg dataset "${dataset[$market]}" \
    --argjson minimum_symbols "${min_symbols[$market]}" \
    --argjson started_ns "$start_ns" \
    '.market == $market and .dataset == $dataset
      and .updated_at_ns >= $started_ns and .status == "synced"
      and .sequence_gaps == 0
      and (.symbol_count | type) == "number" and .symbol_count == (.symbol_count | floor)
      and .symbol_count >= $minimum_symbols
      and (.snapshot_ready_count | type) == "number"
      and .snapshot_ready_count == (.snapshot_ready_count | floor)
      and .snapshot_ready_count == .symbol_count
      and .bridged_count == .symbol_count
      and .stream_coverage_verified_count == .symbol_count
      and .snapshot_only_symbols == [] and .all_symbols_bridged == true
      and .all_stream_coverage_verified == true
      and ((.full_stream_coverage_verified == null) or (.full_stream_coverage_verified == true))
      and .queue_saturated == false and .disk_warning == false and .upload_warning == false
      and (.upload_failure_count | type) == "number"
      and .upload_failure_count >= 0 and .upload_failure_count == (.upload_failure_count | floor)
      and (.session_id | type) == "string" and (.session_id | length) > 0' "$health" >/dev/null
}

write_health_validation_failure() {
  local market=$1 health="${spool_dir[$1]}/health.json"
  local output="$evidence_dir/${market}-health-failure.json"
  if [[ ! -f $health || -L $health ]]; then
    jq -n --arg reason 'health.json missing or symbolic link' \
      '{health:null,validation:{pass:false,failed:["health_file"],reason:$reason}}' \
      >"$output"
    chmod 0640 "$output"
    return 0
  fi
  jq -c \
    --arg market "$market" --arg dataset "${dataset[$market]}" \
    --argjson minimum_symbols "${min_symbols[$market]}" \
    --argjson started_ns "$start_ns" \
    'def check($name; $ok; $actual; $expected):
       {name:$name,ok:$ok,actual:$actual,expected:$expected,
        reason:(if $ok then null else ($name + " predicate failed") end)};
     [
       check("market"; .market == $market; .market; $market),
       check("dataset"; .dataset == $dataset; .dataset; $dataset),
       check("updated_at_ns"; (.updated_at_ns|type) == "number" and .updated_at_ns >= $started_ns;
         .updated_at_ns; {type:"number",minimum:$started_ns}),
       check("status"; .status == "synced"; .status; "synced"),
       check("sequence_gaps"; .sequence_gaps == 0; .sequence_gaps; 0),
       check("symbol_count_type"; (.symbol_count|type) == "number"; .symbol_count; "number"),
       check("symbol_count_integer"; (.symbol_count|type) == "number" and .symbol_count == (.symbol_count|floor);
         .symbol_count; "integer"),
       check("minimum_symbols"; (.symbol_count|type) == "number" and .symbol_count >= $minimum_symbols;
         .symbol_count; $minimum_symbols),
       check("snapshot_ready_count_type"; (.snapshot_ready_count|type) == "number";
         .snapshot_ready_count; "number"),
       check("snapshot_ready_count_integer"; (.snapshot_ready_count|type) == "number" and .snapshot_ready_count == (.snapshot_ready_count|floor);
         .snapshot_ready_count; "integer"),
       check("snapshot_ready_full"; .snapshot_ready_count == .symbol_count;
         .snapshot_ready_count; .symbol_count),
       check("bridged_full"; .bridged_count == .symbol_count; .bridged_count; .symbol_count),
       check("stream_coverage_full"; .stream_coverage_verified_count == .symbol_count;
         .stream_coverage_verified_count; .symbol_count),
       check("snapshot_only_empty"; .snapshot_only_symbols == []; .snapshot_only_symbols; []),
       check("all_symbols_bridged"; .all_symbols_bridged == true; .all_symbols_bridged; true),
       check("all_stream_coverage_verified"; .all_stream_coverage_verified == true;
         .all_stream_coverage_verified; true),
       check("full_stream_coverage_verified";
         (.full_stream_coverage_verified == null or .full_stream_coverage_verified == true);
         .full_stream_coverage_verified; true),
       check("queue_saturated"; .queue_saturated == false; .queue_saturated; false),
       check("disk_warning"; .disk_warning == false; .disk_warning; false),
       check("upload_warning"; .upload_warning == false; .upload_warning; false),
       check("upload_failure_count_type"; (.upload_failure_count|type) == "number";
         .upload_failure_count; "number"),
       check("upload_failure_count_nonnegative";
         (.upload_failure_count|type) == "number" and .upload_failure_count >= 0;
         .upload_failure_count; ">=0"),
       check("upload_failure_count_integer";
         (.upload_failure_count|type) == "number" and .upload_failure_count == (.upload_failure_count|floor);
         .upload_failure_count; "integer"),
       check("session_id_type"; (.session_id|type) == "string"; .session_id; "string"),
       check("session_id_nonempty"; (.session_id|type) == "string" and (.session_id|length) > 0;
         .session_id; "nonempty")
     ] as $checks
     | {health:.,validation:{pass:all($checks[];.ok),failed:[$checks[]|select(.ok|not)|.name],checks:$checks}}' \
    "$health" >"$output" || \
    jq -n --arg reason 'health.json could not be parsed' \
      '{health:null,validation:{pass:false,failed:["health_json"],reason:$reason}}' >"$output"
  chmod 0640 "$output"
}

health_catalog_sha256() {
  jq -c '.symbols | keys | sort' "${spool_dir[$1]}/health.json" \
    | sha256sum | awk '{print $1}'
}

declare -A observed_session frozen_symbol_count frozen_catalog_sha256 readback_start_ns
declare -A initial_upload_failure_count last_health_updated_ns last_health_advance_mono
declare -A max_health_silence_seconds health_samples
declare -A recovery_active recovery_started_mono recovery_started_iso recovery_started_ns
declare -A recovery_last_transport_iso
watchdog_window_start_iso=
watchdog_failure=false
watchdog_failure_reason=
for market in "${MARKETS[@]}"; do
  recovery_active[$market]=false
  recovery_started_mono[$market]=-1
  recovery_started_iso[$market]=
  recovery_started_ns[$market]=0
  recovery_last_transport_iso[$market]=
done

sample_cgroup() {
  local market=$1 service=${unit[$1]} control_group cgroup_root cpu_stat memory_events
  local memory_current memory_max cpu_usage cpu_quota
  control_group=$(systemctl_prop "$service" ControlGroup)
  [[ $control_group == /* && $control_group != *..* ]] \
    || die "$market ControlGroup is invalid: $control_group"
  cgroup_root="/sys/fs/cgroup$control_group"
  [[ -d $cgroup_root && -r $cgroup_root/cpu.stat && -r $cgroup_root/memory.events ]] \
    || die "$market cgroup files are unavailable"
  cpu_stat=$(<"$cgroup_root/cpu.stat")
  memory_events=$(<"$cgroup_root/memory.events")
  memory_current=$(systemctl_prop "$service" MemoryCurrent)
  memory_max=$(systemctl_prop "$service" MemoryMax)
  cpu_usage=$(systemctl_prop "$service" CPUUsageNSec)
  cpu_quota=$(systemctl_prop "$service" CPUQuotaPerSecUSec)
  [[ $memory_current =~ ^[0-9]+$ && $memory_max =~ ^[0-9]+$ && $cpu_usage =~ ^[0-9]+$ ]] \
    || die "$market cgroup accounting is unavailable"
  (( memory_max > 0 && memory_current <= memory_max )) \
    || die "$market memory limit was exceeded"
  awk '$1 == "oom" && $2 != 0 {bad=1} $1 == "oom_kill" && $2 != 0 {bad=1}
    END {exit bad ? 1 : 0}' <<<"$memory_events" \
    || die "$market cgroup reports an OOM event"
  jq -cn --arg control_group "$control_group" --arg cpu_quota "$cpu_quota" \
    --argjson memory_current "$memory_current" --argjson memory_max "$memory_max" \
    --argjson cpu_usage_ns "$cpu_usage" --arg cpu_stat "$cpu_stat" \
    --arg memory_events "$memory_events" \
    '{control_group:$control_group,cpu_quota_per_sec_us:$cpu_quota,
      cpu_usage_ns:$cpu_usage_ns,memory_current:$memory_current,memory_max:$memory_max,
      cpu_stat:$cpu_stat,memory_events:$memory_events}'
}

record_watchdog_event() {
  local market=$1 phase=$2 kind=$3 now_iso=$4 line=$5
  jq -cn --arg sampled_at "$now_iso" --arg phase "$phase" \
    --arg market "$market" --arg producer "${unit[$market]}" \
    --arg kind "$kind" --arg line "$line" \
    --argjson recovering "${recovery_active[$market]:-false}" \
    '{sampled_at_utc:$sampled_at,phase:$phase,market:$market,producer:$producer,
      diagnostic_kind:$kind,recovery_active:$recovering,diagnostic:$line}' \
    >>"$evidence_dir/watchdog-events.ndjson"
}

begin_transport_recovery() {
  reanchor_recovery "$@"
}

assert_recovery_clear() {
  local market
  for market in "${MARKETS[@]}"; do
    [[ ${recovery_active[$market]:-false} != true ]] \
      || die "$market transport recovery is still pending"
  done
}

health_recovery_safety_passes() {
  local market=$1 health="${spool_dir[$1]}/health.json"
  [[ -f $health && ! -L $health ]] || return 1
  jq -e \
    --arg market "$market" --arg dataset "${dataset[$market]}" \
    --argjson minimum_symbols "${min_symbols[$market]}" \
    --argjson started_ns "$start_ns" \
    '.market == $market and .dataset == $dataset
      and .updated_at_ns >= $started_ns
      and (.status == "syncing" or .status == "reconnecting" or .status == "synced")
      and .sequence_gaps == 0
      and (.symbol_count | type) == "number" and .symbol_count == (.symbol_count | floor)
      and .symbol_count >= $minimum_symbols
      and (.snapshot_ready_count | type) == "number"
      and .snapshot_ready_count == (.snapshot_ready_count | floor)
      and .snapshot_ready_count >= 0 and .snapshot_ready_count <= .symbol_count
      and (.bridged_count | type) == "number"
      and .bridged_count >= 0 and .bridged_count <= .symbol_count
      and (.stream_coverage_verified_count | type) == "number"
      and .stream_coverage_verified_count >= 0
      and .stream_coverage_verified_count <= .symbol_count
      and .queue_saturated == false and .disk_warning == false and .upload_warning == false
      and (.upload_failure_count | type) == "number"
      and .upload_failure_count >= 0 and .upload_failure_count == (.upload_failure_count | floor)
      and (.session_id | type) == "string" and (.session_id | length) > 0' \
    "$health" >/dev/null
}

sample_health() {
  local market=$1 phase=$2 health="${spool_dir[$1]}/health.json"
  local health_state=missing health_obj='null' service_obj cgroup_obj spool_obj
  local now
  now=$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ)
  if [[ -f $health && ! -L $health ]]; then
    health_state=present
    health_obj=$(jq -ce \
      'if (.market|type) != "string" or (.dataset|type) != "string"
        or (.status|type) != "string" or (.updated_at_ns|type) != "number"
        or (.queue_remaining_capacity|type) != "number"
        or (.queue_saturated|type) != "boolean" or (.sequence_gaps|type) != "number"
        or (.session_id|type) != "string"
        then error("malformed health")
        else {market,dataset,status,queue_remaining_capacity,queue_saturated,sequence_gaps,
          symbol_count,snapshot_ready_count,stream_coverage_verified_count,
          all_stream_coverage_verified,full_stream_coverage_verified,disk_warning,
          upload_warning,upload_failure_count,session_id,updated_at_ns}
        end' "$health") || die "$market health is malformed"
  elif (( $(monotonic_seconds) >= ready_deadline )); then
    die "$market health is missing after the settle deadline"
  fi
  [[ $(systemctl is-active "${unit[$market]}" 2>/dev/null || true) == active ]] \
    || die "$market shadow primary is not active while sampling"
  service_obj=$(jq -cn \
    --arg active "$(systemctl is-active "${unit[$market]}" 2>/dev/null || true)" \
    --arg substate "$(systemctl_prop "${unit[$market]}" SubState)" \
    --arg pid "$(systemctl_prop "${unit[$market]}" MainPID)" \
    --arg invocation "$(systemctl_prop "${unit[$market]}" InvocationID)" \
    --arg restarts "$(systemctl_prop "${unit[$market]}" NRestarts)" \
    --arg cpu_usage "$(systemctl_prop "${unit[$market]}" CPUUsageNSec)" \
    '{active:$active,substate:$substate,main_pid:$pid,invocation_id:$invocation,
      n_restarts:$restarts,cpu_usage_ns:$cpu_usage}')
  cgroup_obj=$(sample_cgroup "$market")
  spool_obj=$(find "${spool_dir[$market]}" -maxdepth 1 -type f \
    -printf '%T@ %s %p\n' | sort -nr | sed -n '1,100p' \
    | jq -Rsc 'split("\n") | map(select(length > 0))')
  jq -cn --arg sampled_at "$now" --arg market "$market" --arg phase "$phase" \
    --arg health_state "$health_state" --argjson health "$health_obj" \
    --argjson service "$service_obj" --argjson cgroup "$cgroup_obj" \
    --argjson spool "$spool_obj" \
    '{sampled_at_utc:$sampled_at,market:$market,phase:$phase,health_state:$health_state,
      health:$health,systemd:$service,cgroup:$cgroup,spool_artifacts:$spool}' \
    >>"$evidence_dir/health-samples.ndjson"
}

capture_watchdog_diagnostics() {
  local phase=$1 now_iso now_mono now_ns market journal_lines health_obj service_obj line
  now_iso=$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ)
  now_mono=$(monotonic_seconds)
  now_ns=$(date +%s%N)
  [[ -n ${watchdog_window_start_iso:-} ]] || watchdog_window_start_iso=$start_iso
  for market in "${MARKETS[@]}"; do
    journal_lines=$(journalctl -u "${unit[$market]}" \
      --since "$watchdog_window_start_iso" --until "$now_iso" \
      --no-pager -o short-precise 2>&1 || true)
    while IFS= read -r line; do
      [[ -n $line ]] || continue
      if grep -Eiq "$FATAL_JOURNAL_REGEX" <<<"$line"; then
        record_watchdog_event "$market" "$phase" fatal "$now_iso" "$line"
        watchdog_failure=true
        watchdog_failure_reason="$market shadow journal contains a fatal watchdog/session diagnostic"
      elif grep -Eiq "$TRANSPORT_JOURNAL_REGEX" <<<"$line"; then
        begin_transport_recovery "$market" "$now_iso" "$now_mono" "$now_ns"
        record_watchdog_event "$market" "$phase" transport "$now_iso" "$line"
      fi
    done <<<"$journal_lines"
    if [[ -f ${spool_dir[$market]}/health.json && ! -L ${spool_dir[$market]}/health.json ]]; then
      health_obj=$(jq -c '{status,session_id,updated_at_ns,queue_remaining_capacity,
        queue_saturated,sequence_gaps,symbol_count,stream_coverage_verified_count,
        all_stream_coverage_verified}' "${spool_dir[$market]}/health.json" 2>/dev/null \
        || printf 'null')
    else
      health_obj=null
    fi
    service_obj=$(jq -cn \
      --arg active "$(systemctl is-active "${unit[$market]}" 2>/dev/null || true)" \
      --arg substate "$(systemctl_prop "${unit[$market]}" SubState)" \
      --arg pid "$(systemctl_prop "${unit[$market]}" MainPID)" \
      --arg invocation "$(systemctl_prop "${unit[$market]}" InvocationID)" \
      --arg restarts "$(systemctl_prop "${unit[$market]}" NRestarts)" \
      --argjson recovering "${recovery_active[$market]:-false}" \
      --arg recovery_started "${recovery_started_iso[$market]:-}" \
      --arg last_transport "${recovery_last_transport_iso[$market]:-}" \
      '{active:$active,substate:$substate,main_pid:$pid,invocation_id:$invocation,
        n_restarts:$restarts,recovery_active:$recovering,recovery_started_at:$recovery_started,
        recovery_last_transport_at:$last_transport}')
    jq -cn --arg sampled_at "$now_iso" --arg phase "$phase" \
      --arg market "$market" --arg producer "${unit[$market]}" \
      --argjson health "$health_obj" --argjson service "$service_obj" \
      --argjson failure "$watchdog_failure" \
      '{sampled_at_utc:$sampled_at,phase:$phase,market:$market,producer:$producer,
        watchdog_failure:$failure,health:$health,systemd:$service}' \
      >>"$evidence_dir/producer-diagnostics.ndjson"
  done
  watchdog_window_start_iso=$now_iso
}

validate_observation_sample() {
  local market=$1 health="${spool_dir[$1]}/health.json" session symbols catalog failures updated_ns
  local recovering=${recovery_active[$1]:-false}
  local current_mono next_updated_ns next_advance_mono next_max_gap sample_increment
  if [[ $recovering == true ]]; then
    if ! health_recovery_safety_passes "$market"; then
      write_health_validation_failure "$market"
      die "$market recovery health safety predicate failed"
    fi
  elif ! health_passes "$market"; then
    write_health_validation_failure "$market"
    die "$market health failed during observation"
  fi
  session=$(jq -er '.session_id' "$health")
  if [[ $recovering != true ]]; then
    [[ $session == "${observed_session[$market]}" ]] \
      || die "$market collector session changed without a transport diagnostic"
  fi
  symbols=$(jq -er '.symbol_count' "$health")
  [[ $symbols == "${frozen_symbol_count[$market]}" ]] \
    || die "$market catalog count changed during observation"
  catalog=$(health_catalog_sha256 "$market")
  [[ $catalog == "${frozen_catalog_sha256[$market]}" ]] \
    || die "$market catalog membership changed during observation"
  failures=$(jq -er '.upload_failure_count' "$health")
  [[ $failures == "${initial_upload_failure_count[$market]}" ]] \
    || die "$market recorded an OSS upload failure during observation"
  updated_ns=$(jq -er '.updated_at_ns' "$health")
  current_mono=$(monotonic_seconds)
  if [[ $recovering == true ]] \
    && (( current_mono - ${recovery_started_mono[$market]} > RECOVERY_SETTLE_SECONDS )); then
    die "$market transport recovery exceeded ${RECOVERY_SETTLE_SECONDS}s"
  fi
  if ! read -r next_updated_ns next_advance_mono next_max_gap sample_increment < <(
    monday_observe_health_freshness \
      "${last_health_updated_ns[$market]}" \
      "${last_health_advance_mono[$market]}" \
      "${max_health_silence_seconds[$market]}" \
      "$updated_ns" "$current_mono" "$MAX_HEALTH_SILENCE_SECONDS"
  ); then
    die "$market health timestamp regressed or stopped advancing"
  fi
  last_health_updated_ns[$market]=$next_updated_ns
  last_health_advance_mono[$market]=$next_advance_mono
  max_health_silence_seconds[$market]=$next_max_gap
  health_samples[$market]=$((health_samples[$market] + sample_increment))
  if [[ $recovering == true ]] \
    && (( current_mono > ${recovery_started_mono[$market]} )) \
    && (( updated_ns >= ${recovery_started_ns[$market]} )) \
    && health_passes "$market"; then
    observed_session[$market]=$session
    readback_start_ns[$market]=$updated_ns
    recovery_active[$market]=false
    recovery_started_mono[$market]=-1
    recovery_started_iso[$market]=
    recovery_started_ns[$market]=0
    recovery_last_transport_iso[$market]=
    jq -cn --arg sampled_at "$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ)" \
      --arg market "$market" --arg session "$session" --argjson readback_start "$updated_ns" \
      '{sampled_at_utc:$sampled_at,market:$market,diagnostic_kind:"transport_recovery_complete",
        session_id:$session,readback_start_ns:$readback_start}' \
      >>"$evidence_dir/watchdog-events.ndjson"
  fi
}

run_candidate_drain() {
  local market=$1
  runuser --user "$SERVICE_USER" -- env -i \
    HOME="$SERVICE_HOME" PATH="$SAFE_PATH" RUST_LOG=info \
    SPOOL_DIR="${spool_dir[$market]}" OSS_BUCKET="${oss_bucket[$market]}" \
    OSS_ENDPOINT="${oss_endpoint[$market]}" OSS_REGION="${oss_region[$market]}" \
    ALIYUN_PROFILE="${aliyun_profile[$market]}" \
    OSS_COPY_TIMEOUT_SECONDS="${oss_copy_timeout[$market]}" \
    "$candidate_binary" --upload-only
  assert_spool_empty "$market"
}

run_oss() {
  local market=$1
  shift
  runuser --user "$SERVICE_USER" -- env -i \
    HOME="$SERVICE_HOME" PATH="$SAFE_PATH" \
    aliyun ossutil "$@" --profile "${aliyun_profile[$market]}" \
    --endpoint "${oss_endpoint[$market]}" --region "${oss_region[$market]}"
}

manifest_uris() {
  local market=$1 listing=$2 prefix line token
  prefix="oss://${oss_bucket[$market]}/lake/raw/venue=binance/market=${market}/dataset=${dataset[$market]}/shard=${shard_id[$market]}/"
  run_oss "$market" ls "$prefix" --recursive --short-format \
    --max-age "$((SOAK_SECONDS + BOOTSTRAP_SETTLE_SECONDS + 900))s" >"$listing"
  while IFS= read -r line; do
    line=${line%$'\r'}
    if [[ $line =~ (oss://[^[:space:]]+\.manifest\.json) ]]; then
      printf '%s\n' "${BASH_REMATCH[1]}"
    else
      token=${line##*[$' \t']}
      token=${token#/}
      if [[ $token == *.manifest.json && $token == lake/* ]]; then
        printf 'oss://%s/%s\n' "${oss_bucket[$market]}" "$token"
      fi
    fi
  done <"$listing" | sort -u
}

strict_verifier_unit=
strict_verifier_counter=0
run_strict_verifier() {
  local status
  strict_verifier_counter=$((strict_verifier_counter + 1))
  strict_verifier_unit="monday-rust-soak-verifier-$$-$strict_verifier_counter.service"
  if systemd-run --quiet --wait --collect --unit="$strict_verifier_unit" \
    --uid="$SERVICE_USER" --gid="$SERVICE_USER" \
    --property=KillMode=control-group --property=MemoryHigh=5000M \
    --property=MemoryMax=6400M -- "$candidate_binary" "$@"; then
    status=0
  else
    status=$?
  fi
  systemctl stop "$strict_verifier_unit" >/dev/null 2>&1 || true
  strict_verifier_unit=
  return "$status"
}

verify_market_readback() {
  local market=$1 listing="$tmp_dir/$1-oss-list.txt" uri index=0 manifest start_ns end_ns
  local file digest manifest_digest zst_uri success_uri stale_count observed_stale data_bytes readback_start
  local candidates="$tmp_dir/$market-candidates.tsv" selected="$tmp_dir/$market-selected.tsv"
  local segment_dir zst_path manifest_path success_path
  local -a strict_segments=()
  local required_segments=$MIN_READBACK_SEGMENTS
  manifest_uris "$market" "$listing" >"$tmp_dir/$market-manifests.txt"
  : >"$candidates"
  while IFS= read -r uri; do
    [[ -n $uri ]] || continue
    index=$((index + 1))
    manifest="$tmp_dir/$market-manifest-$index.json"
    run_oss "$market" cp "$uri" "$manifest" --force --no-progress >/dev/null
    start_ns=$(jq -er '.start_received_at_ns' "$manifest")
    end_ns=$(jq -er '.end_received_at_ns' "$manifest")
    [[ $start_ns =~ ^[0-9]+$ && $end_ns =~ ^[0-9]+$ && $end_ns -ge $start_ns ]] \
      || die "$market manifest has invalid receive bounds: $uri"
    readback_start=${readback_start_ns[$market]:-0}
    [[ $readback_start =~ ^[0-9]+$ ]] || die "$market readback start is invalid"
    (( start_ns < readback_start )) && continue
    jq -e --arg market "$market" --arg dataset "${dataset[$market]}" \
      --arg shard "${shard_id[$market]}" --arg session "${observed_session[$market]}" \
      --argjson expected_streams "${expected_stream_types[$market]}" \
      '.schema == "binance.market_tape.v2" and .market == $market
       and .dataset == $dataset and .shard_id == $shard
       and (.file|type) == "string" and (.file|test("^[A-Za-z0-9._-]+\\.jsonl\\.zst$"))
       and (.sha256|type) == "string" and (.sha256|test("^[a-f0-9]{64}$"))
       and .has_replay_safe_checkpoint == true
       and .trade_summary_contract == "binance.aggregate_trade_summary.v1"
       and (.trade_summaries|type) == "object" and (.trade_summaries|length) > 0
       and .lob_continuity.contract == "binance.lob_continuity.v1"
       and .lob_continuity.capture_session_id == $session
       and .lob_continuity.sequence_gaps == 0
       and .lob_continuity.source_time_rollbacks == 0
       and .lob_continuity.declared_symbol_count == (.symbols|length)
       and .lob_continuity.covered_symbol_count == (.symbols|length)
       and .lob_continuity.missing_symbols == []
       and .stream_coverage_verified_count == (.symbols|length)
       and .all_stream_coverage_verified == true
       and (.stream_types|sort) == $expected_streams
       and (.event_types.agg_trade|type) == "number" and .event_types.agg_trade > 0
       and (.event_types.raw_trade|type) == "number" and .event_types.raw_trade > 0
       and (.event_types.book_ticker|type) == "number" and .event_types.book_ticker > 0' \
      "$manifest" >/dev/null \
      || die "$market manifest failed soak readback validation: $uri"
    file=$(jq -er '.file' "$manifest")
    digest=$(jq -er '.sha256' "$manifest")
    manifest_digest=$(sha256sum "$manifest" | awk '{print $1}')
    printf '%s\t%s\t%s\t%s\t%s\n' "$start_ns" "$end_ns" "$uri" "$file" \
      "$manifest_digest" >>"$candidates"
  done <"$tmp_dir/$market-manifests.txt"
  [[ $(wc -l <"$candidates" | tr -d ' ') -ge $required_segments ]] \
    || die "$market has fewer than $required_segments complete post-soak manifests"
  sort -nr -k1,1 "$candidates" | tail -n "$required_segments" | sort -n -k1,1 >"$selected"
  while IFS=$'\t' read -r start_ns end_ns uri file manifest_digest; do
    segment_dir="$tmp_dir/$market-segment-$start_ns"
    install -d -m 0750 -o "$SERVICE_USER" -g "$SERVICE_USER" "$segment_dir"
    manifest_path="$segment_dir/${file}.manifest.json"
    zst_path="$segment_dir/$file"
    success_path="$segment_dir/$file._SUCCESS"
    run_oss "$market" cp "$uri" "$manifest_path" --force --no-progress >/dev/null
    [[ $(sha256sum "$manifest_path" | awk '{print $1}') == "$manifest_digest" ]] \
      || die "$market manifest changed during readback: $uri"
    digest=$(jq -er '.sha256' "$manifest_path")
    zst_uri="${uri%/*}/$file"
    success_uri="${uri%/*}/$file._SUCCESS"
    run_oss "$market" cp "$zst_uri" "$zst_path" --force --no-progress >/dev/null
    printf '%s  %s\n' "$digest" "$zst_path" | sha256sum --check --strict >/dev/null
    data_bytes=$(stat -c '%s' "$zst_path")
    run_oss "$market" cp "$success_uri" "$success_path" --force --no-progress >/dev/null
    printf '%s\n' "$digest" | cmp -s - "$success_path" \
      || die "$market _SUCCESS does not match data SHA: $success_uri"
    stale_count=$(jq -er '(.event_types.stale_book_ticker // 0)
      | if (type == "number" and . == floor and . >= 0) then . else error("bad stale count") end' \
      "$manifest_path")
    observed_stale=$(zstd -q -d -c "$zst_path" \
      | jq -cn --argjson expected "$stale_count" \
        'reduce inputs as $row (0; . + (if $row.type == "stale_book_ticker" then 1 else 0 end))
         | if . == $expected then . else error("stale event count mismatch") end')
    jq -n --arg market "$market" --arg uri "$uri" --arg data_uri "$zst_uri" \
      --arg success_uri "$success_uri" --argjson stale_count "$observed_stale" \
      --argjson start_ns "$start_ns" --argjson end_ns "$end_ns" \
      --arg data_sha256 "$digest" --arg manifest_sha256 "$manifest_digest" \
      --argjson data_bytes "$data_bytes" \
      '{market:$market,manifest_uri:$uri,data_uri:$data_uri,success_uri:$success_uri,
        data_sha256:$data_sha256,manifest_sha256:$manifest_sha256,data_bytes:$data_bytes,
        start_received_at_ns:$start_ns,end_received_at_ns:$end_ns,
        stale_book_ticker_count:$stale_count,
        stale_semantics:(if $stale_count > 0 then "validated" else "not_observed" end)}' \
      >>"$evidence_dir/${market}-readback.jsonl"
    install -m 0640 "$manifest_path" "$evidence_dir/${market}-manifest-$start_ns.json"
    strict_segments+=("$zst_path" "$digest" "$manifest_digest")
  done <"$selected"
  local -a continuity_args=(--verify-segment "${strict_segments[0]}" \
    --segment-content-sha256 "${strict_segments[1]}" \
    --segment-manifest-sha256 "${strict_segments[2]}")
  if (( required_segments >= 2 )); then
    continuity_args+=(--verify-segment "${strict_segments[3]}" \
      --segment-content-sha256 "${strict_segments[4]}" \
      --segment-manifest-sha256 "${strict_segments[5]}")
  fi
  run_strict_verifier --require-lob-continuity "${continuity_args[@]}" \
    >"$evidence_dir/${market}-lob-continuity.txt" 2>&1 \
    || die "$market strict LOB continuity verifier failed"
  local -a aggregate_args=(--verify-aggregate-trade-continuity "${continuity_args[@]}")
  run_strict_verifier "${aggregate_args[@]}" \
    >"$evidence_dir/${market}-aggregate-continuity.txt" 2>&1 \
    || die "$market aggregate continuity verifier failed"
  local -a raw_args=(--verify-raw-trade-continuity "${continuity_args[@]}")
  run_strict_verifier "${raw_args[@]}" \
    >"$evidence_dir/${market}-raw-trade-continuity.txt" 2>&1 \
    || die "$market raw-trade continuity verifier failed"
}

soak_completed=false
drain_done=false
cleanup_failure=false
start_iso=$(date -u +%Y-%m-%dT%H:%M:%SZ)
start_ns=$(date +%s%N)
ready_deadline=0
observation_started_ns=0

stop_primaries_and_wait() {
  local deadline
  systemctl stop "${unit[spot]}" "${unit[usdm]}" >/dev/null 2>&1 || true
  deadline=$(( $(monotonic_seconds) + 120 ))
  while (( $(monotonic_seconds) < deadline )); do
    if [[ $(systemctl is-active "${unit[spot]}" 2>/dev/null || true) == inactive \
      && $(systemctl is-active "${unit[usdm]}" 2>/dev/null || true) == inactive ]]; then
      return 0
    fi
    sleep 1
  done
  return 1
}

capture_journal() {
  local end_iso=$1 output=${2:-"$evidence_dir/journal.txt"}
  journalctl -u "${unit[spot]}" -u "${unit[usdm]}" \
    --since "$start_iso" --until "$end_iso" --no-pager -o short-precise \
    >"$output" 2>&1 || cleanup_failure=true
}

capture_final_producer_diagnostics() {
  local suffix=${1:-final} market
  for market in "${MARKETS[@]}"; do
    {
      printf 'market=%s producer=%s\n' "$market" "${unit[$market]}"
      systemctl show "${unit[$market]}" -p ActiveState -p SubState -p MainPID \
        -p InvocationID -p NRestarts -p Result --no-pager
      if [[ -f ${spool_dir[$market]}/health.json && ! -L ${spool_dir[$market]}/health.json ]]; then
        jq -c '{status,session_id,updated_at_ns,queue_remaining_capacity,
          queue_saturated,sequence_gaps,symbol_count,stream_coverage_verified_count,
          all_stream_coverage_verified}' "${spool_dir[$market]}/health.json" || true
      fi
    } >"$evidence_dir/${market}-producer-$suffix.txt" 2>&1 || cleanup_failure=true
  done
}

cleanup() {
  local status=$? end_iso
  set +e
  end_iso=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  capture_journal "$end_iso" "$evidence_dir/journal-precleanup.txt"
  capture_final_producer_diagnostics precleanup
  [[ $watchdog_failure == true ]] && cleanup_failure=true
  for market in "${MARKETS[@]}"; do
    [[ ${recovery_active[$market]:-false} != true ]] || cleanup_failure=true
  done
  [[ -z $strict_verifier_unit ]] || systemctl stop "$strict_verifier_unit" >/dev/null 2>&1 || true
  if ! stop_primaries_and_wait; then cleanup_failure=true; fi
  for market in "${MARKETS[@]}"; do
    rm -f -- "${override_file[$market]}" || cleanup_failure=true
    [[ ! -e ${override_file[$market]} && ! -L ${override_file[$market]} ]] \
      || cleanup_failure=true
  done
  for upload_unit in binance-lob-archiver-rust-upload@spot.service \
    binance-lob-archiver-rust-upload@usdm.service; do
    systemctl stop "$upload_unit" >/dev/null 2>&1 || true
    [[ $(systemctl is-active "$upload_unit" 2>/dev/null || true) == inactive ]] \
      || cleanup_failure=true
    [[ $(systemctl is-enabled "$upload_unit" 2>/dev/null || true) == static ]] \
      || cleanup_failure=true
  done
  [[ $(systemctl is-enabled "${unit[spot]}" 2>/dev/null || true) == disabled ]] \
    || cleanup_failure=true
  [[ $(systemctl is-enabled "${unit[usdm]}" 2>/dev/null || true) == disabled ]] \
    || cleanup_failure=true
  if [[ $drain_done != true && $soak_completed == true && $status == 0 ]]; then
    for market in "${MARKETS[@]}"; do
      if ! (run_candidate_drain "$market"); then cleanup_failure=true; fi
    done
    drain_done=true
  fi
  if ! (capture_production_fingerprint "$current_fingerprint" \
    && cmp -s "$baseline_fingerprint" "$current_fingerprint"); then
    cleanup_failure=true
  fi
  install -m 0640 "$current_fingerprint" "$evidence_dir/production-final.json" 2>/dev/null \
    || cleanup_failure=true
  df -Pk /data >"$evidence_dir/data-df-final.txt" 2>/dev/null || cleanup_failure=true
  end_iso=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  capture_journal "$end_iso" "$evidence_dir/journal.txt"
  capture_final_producer_diagnostics
  if [[ -n ${LOCK_FILE:-} ]]; then
    flock -u 9 >/dev/null 2>&1 || true
    exec 9>&-
    exec 8>"$LOCK_FILE"
    flock -n 8 >/dev/null 2>&1 || cleanup_failure=true
    flock -u 8 >/dev/null 2>&1 || true
    exec 8>&-
  fi
  if [[ $cleanup_failure == true && $status == 0 ]]; then status=1; fi
  if [[ $status == 0 ]]; then
    jq -n --arg result passed --arg finished_at "$end_iso" --arg mode "$RUN_MODE" \
      --arg candidate "$CANDIDATE_SHA256" --arg source "$SOURCE_REVISION" \
      --arg build "$CANDIDATE_BUILD_ID" \
      --arg bundle "$BUNDLE_SHA256" \
      --arg preflight_receipt "${PREFLIGHT_RECEIPT:-}" \
      --arg preflight_receipt_sha256 "${PREFLIGHT_RECEIPT_SHA256:-}" \
      --arg run_spool "$RUN_SPOOL_ROOT/$CANDIDATE_SHA256/$evidence_run_id" \
      '{schema:"monday.rust_lob_shadow_soak_result.v1",result:$result,
        mode:$mode,candidate_sha256:$candidate,source_revision:$source,build_id:$build,
        deployment_bundle_sha256:$bundle,formal_gate:false,cutover:false,live:false,finished_at:$finished_at,
        preflight_receipt:(if $preflight_receipt == "" then null else $preflight_receipt end),
        preflight_receipt_sha256:(if $preflight_receipt_sha256 == "" then null else $preflight_receipt_sha256 end),
        run_spool:$run_spool}' >"$evidence_dir/receipt.json"
  else
    jq -n --arg result STOP --arg finished_at "$end_iso" --arg mode "$RUN_MODE" \
      --arg candidate "$CANDIDATE_SHA256" --arg source "$SOURCE_REVISION" \
      --arg build "$CANDIDATE_BUILD_ID" \
      --arg bundle "$BUNDLE_SHA256" \
      --arg preflight_receipt "${PREFLIGHT_RECEIPT:-}" \
      --arg preflight_receipt_sha256 "${PREFLIGHT_RECEIPT_SHA256:-}" \
      --arg run_spool "$RUN_SPOOL_ROOT/$CANDIDATE_SHA256/$evidence_run_id" \
      '{schema:"monday.rust_lob_shadow_soak_result.v1",result:$result,
        mode:$mode,candidate_sha256:$candidate,source_revision:$source,build_id:$build,
        deployment_bundle_sha256:$bundle,formal_gate:false,cutover:false,live:false,finished_at:$finished_at,
        preflight_receipt:(if $preflight_receipt == "" then null else $preflight_receipt end),
        preflight_receipt_sha256:(if $preflight_receipt_sha256 == "" then null else $preflight_receipt_sha256 end),
        run_spool:$run_spool}' >"$evidence_dir/receipt.json"
  fi
  rm -rf -- "$tmp_dir"
  exit "$status"
}
trap 'exit 143' HUP INT TERM
trap cleanup EXIT

systemctl reset-failed "${unit[spot]}" "${unit[usdm]}" >/dev/null 2>&1 || true
assert_candidate_stable
install -d -m 0755 "$OVERRIDE_ROOT"
for market in "${MARKETS[@]}"; do
  printf 'SPOOL_DIR=%s\n' "${spool_dir[$market]}" >"$tmp_dir/$market-soak.env"
  if [[ $RUN_MODE == correctness ]]; then
    printf 'SEGMENT_SECONDS=%s\n' "$CORRECTNESS_SEGMENT_SECONDS" \
      >>"$tmp_dir/$market-soak.env"
  fi
  install -m 0640 "$tmp_dir/$market-soak.env" "${override_file[$market]}"
done
feed_deadline=$(( $(monotonic_seconds) + TOTAL_FEED_SECONDS ))
systemctl start "${unit[spot]}" "${unit[usdm]}"
for market in "${MARKETS[@]}"; do capture_shadow_start_identity "$market"; done
watchdog_window_start_iso=$(date -u +%Y-%m-%dT%H:%M:%S.%3NZ)
capture_watchdog_diagnostics startup
[[ $watchdog_failure == false ]] \
  || die "$watchdog_failure_reason"

ready_deadline=$(( $(monotonic_seconds) + BOOTSTRAP_SETTLE_SECONDS ))
while ! health_passes spot || ! health_passes usdm; do
  for market in "${MARKETS[@]}"; do sample_health "$market" settle; done
  capture_watchdog_diagnostics settle
  [[ $watchdog_failure == false ]] \
    || die "$watchdog_failure_reason"
  for market in "${MARKETS[@]}"; do
    if [[ ${recovery_active[$market]:-false} == true ]] \
      && (( ${recovery_started_mono[$market]} + RECOVERY_SETTLE_SECONDS > ready_deadline )); then
      ready_deadline=$(( ${recovery_started_mono[$market]} + RECOVERY_SETTLE_SECONDS ))
    fi
  done
  (( $(monotonic_seconds) < ready_deadline && $(monotonic_seconds) < feed_deadline )) \
    || die 'shadow health did not reach synced full coverage before settle deadline'
  sleep "$SAMPLE_INTERVAL_SECONDS"
done

for market in "${MARKETS[@]}"; do
  health="${spool_dir[$market]}/health.json"
  observed_session[$market]=$(jq -er '.session_id' "$health")
  frozen_symbol_count[$market]=$(jq -er '.symbol_count' "$health")
  frozen_catalog_sha256[$market]=$(health_catalog_sha256 "$market")
  initial_upload_failure_count[$market]=$(jq -er '.upload_failure_count' "$health")
  last_health_updated_ns[$market]=$(jq -er '.updated_at_ns' "$health")
  last_health_advance_mono[$market]=$(monotonic_seconds)
  max_health_silence_seconds[$market]=0
  health_samples[$market]=1
done
observation_started_ns=$(date +%s%N)
for market in "${MARKETS[@]}"; do
  readback_start_ns[$market]=$observation_started_ns
done
soak_deadline=$(( $(monotonic_seconds) + SOAK_SECONDS ))
(( soak_deadline <= feed_deadline )) || die 'bootstrap exhausted the total feed deadline'
while (( $(monotonic_seconds) < soak_deadline )); do
  capture_watchdog_diagnostics observation
  [[ $watchdog_failure == false ]] \
    || die "$watchdog_failure_reason"
  for market in "${MARKETS[@]}"; do
    sample_health "$market" observation
    validate_observation_sample "$market"
    [[ $(systemctl show "${unit[$market]}" --property=MainPID --value) == "${expected_shadow_pid[$market]}" ]] \
      || die "$market shadow MainPID changed during soak"
    [[ $(systemctl show "${unit[$market]}" --property=InvocationID --value) == "${expected_shadow_invocation[$market]}" ]] \
      || die "$market shadow InvocationID changed during soak"
    [[ $(systemctl show "${unit[$market]}" --property=NRestarts --value) == 0 ]] \
      || die "$market shadow restarted during soak"
  done
  assert_candidate_stable
  remaining=$((soak_deadline - $(monotonic_seconds)))
  if (( remaining > SAMPLE_INTERVAL_SECONDS )); then
    sleep "$SAMPLE_INTERVAL_SECONDS"
  else
    sleep "$remaining"
  fi
done
soak_completed=true
capture_watchdog_diagnostics prestop
[[ $watchdog_failure == false ]] \
  || die "$watchdog_failure_reason"
assert_recovery_clear

prestop_iso=$(date -u +%Y-%m-%dT%H:%M:%SZ)
journalctl -u "${unit[spot]}" -u "${unit[usdm]}" \
  --since "$start_iso" --until "$prestop_iso" --no-pager -o short-precise \
  >"$evidence_dir/journal-prestop.txt" 2>&1 \
  || die 'failed to capture the pre-stop shadow journal'
tail_journal="$evidence_dir/journal-prestop-tail.txt"
journalctl -u "${unit[spot]}" -u "${unit[usdm]}" \
  --since "$watchdog_window_start_iso" --until "$prestop_iso" --no-pager -o short-precise \
  >"$tail_journal" 2>&1 \
  || die 'failed to capture the final pre-stop shadow journal window'
if grep -Eiq "$FATAL_JOURNAL_REGEX" "$evidence_dir/journal-prestop.txt"; then
  die 'shadow journal contains a fatal watchdog/session diagnostic'
fi
if grep -Eiq "$TRANSPORT_JOURNAL_REGEX" "$tail_journal"; then
  die 'shadow journal contains a transport reset after the final health sample'
fi

stop_primaries_and_wait || die 'shadow primaries did not stop synchronously'
for market in "${MARKETS[@]}"; do run_candidate_drain "$market"; done
drain_done=true
verify_market_readback spot
verify_market_readback usdm
assert_production_baseline "$baseline_fingerprint"
capture_production_fingerprint "$current_fingerprint"
cmp -s "$baseline_fingerprint" "$current_fingerprint" \
  || die 'production fingerprint changed during soak'
printf 'soak-only preparation body completed; evidence=%s\n' "$evidence_dir"
