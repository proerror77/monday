#!/usr/bin/env bash
set -euo pipefail

umask 027
export LC_ALL=C

readonly REQUIRED_DURATION_SECONDS=3600
readonly OBSERVATION_GRACE_SECONDS=90
readonly MAX_ARTIFACT_GAP_NS=90000000000
readonly RELEASE_SCHEMA=monday.binance_usdm_reference_release.v1
readonly GATE_SCHEMA=monday.binance_usdm_reference_shadow_gate.v1
readonly HEALTH_SCHEMA=binance.usdm_reference_health.v1
readonly VERIFICATION_SCHEMA=monday.binance_usdm_reference_artifact_verification.v1
readonly SERVICE_TEMPLATE=binance-usdm-reference-collector-shadow@.service
readonly GATE_POLICY=binance-usdm-reference-shadow-gate-policy.jq
readonly RUNNER=binance-usdm-reference-shadow-gate.sh

die() {
  printf 'Binance USD-M reference shadow gate failed: %s\n' "$*" >&2
  exit 1
}

usage() {
  printf '%s\n' \
    'Usage: binance-usdm-reference-shadow-gate.sh <candidate-sha256>' \
    '' \
    'The gate only observes an already-running isolated shadow service.' \
    'A production-eligible gate requires at least 3600 seconds of artifacts.'
}

[[ $# -eq 1 ]] || {
  usage >&2
  exit 2
}

test_mode=false
root_prefix=
if [[ ${MONDAY_ALLOW_REFERENCE_GATE_TEST_MODE:-0} == 1 ]]; then
  [[ -n ${MONDAY_REFERENCE_GATE_TEST_ROOT:-} ]] \
    || die 'test mode requires MONDAY_REFERENCE_GATE_TEST_ROOT'
  root_prefix=$(cd -- "$MONDAY_REFERENCE_GATE_TEST_ROOT" && pwd -P)
  [[ $root_prefix == /* && $root_prefix != / ]] || die 'invalid test root'
  test_mode=true
else
  [[ -z ${MONDAY_REFERENCE_GATE_TEST_ROOT+x} \
    && -z ${MONDAY_REFERENCE_GATE_TEST_SECONDS+x} \
    && -z ${MONDAY_REFERENCE_GATE_TEST_GRACE_SECONDS+x} \
    && -z ${MONDAY_REFERENCE_GATE_TEST_NOW_NS+x} ]] \
    || die 'test overrides require explicit test mode'
  [[ ${EUID} -eq 0 ]] || die 'must run as root'
fi

for command in awk cmp date dirname find jq mkdir mktemp readlink rm sha256sum \
  sleep sort stat systemctl tr; do
  command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done

prefix_path() {
  printf '%s%s\n' "$root_prefix" "$1"
}

candidate_sha=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')
[[ $candidate_sha =~ ^[a-f0-9]{64}$ ]] \
  || die 'candidate SHA-256 must be 64 hexadecimal characters'
release_root=$(prefix_path /opt/monday/releases/binance-usdm-reference-collector)
release="$release_root/$candidate_sha"
deployment="$release/deployment"
collector="$release/binance-usdm-reference-collector"
verifier="$release/binance-usdm-reference-artifact-verifier"
release_json="$release/release.json"
control_manifest="$release/binance-usdm-reference-control-assets.sha256"
control_archive="$release/binance-usdm-reference-control.tar.gz"
spool=$(prefix_path "/data/monday/spool/binance-usdm-reference-shadow/$candidate_sha")
evidence_root=$(prefix_path /data/monday/evidence/binance-usdm-reference-shadow-gates)
installed_service=$(prefix_path "/etc/systemd/system/$SERVICE_TEMPLATE")
proc_root=$(prefix_path /proc)
lock_file=$(prefix_path /run/monday/binance-usdm-reference-release.lock)
unit="binance-usdm-reference-collector-shadow@$candidate_sha.service"
script_dir=$(cd -- "$(dirname -- "$0")" && pwd -P)
[[ $script_dir == "$deployment" ]] || die 'gate runner is outside the candidate deployment bundle'

direct_directory() {
  local path=$1
  [[ -d $path && ! -L $path && $(cd -- "$path" && pwd -P) == "$path" ]]
}

secure_root_directory() {
  local path=$1 mode owner
  direct_directory "$path" || return 1
  if [[ $test_mode == false ]]; then
    owner=$(stat -c %u -- "$path")
    mode=$(stat -c %a -- "$path")
    [[ $owner == 0 ]] || return 1
    (( (8#$mode & 022) == 0 )) || return 1
  fi
}

secure_spool_directory() {
  local path=$1 mode owner
  direct_directory "$path" || return 1
  if [[ $test_mode == false ]]; then
    owner=$(stat -c %U -- "$path")
    mode=$(stat -c %a -- "$path")
    [[ $owner == hftcollector ]] || return 1
    (( (8#$mode & 022) == 0 )) || return 1
  fi
}

direct_file() {
  local path=$1
  [[ -f $path && ! -L $path && $(readlink -f -- "$path") == "$path" ]]
}

secure_control_file() {
  local path=$1 mode owner
  direct_file "$path" || die "missing direct control file: $path"
  if [[ $test_mode == false ]]; then
    owner=$(stat -c %u -- "$path")
    mode=$(stat -c %a -- "$path")
    [[ $owner == 0 ]] || die "control file is not root-owned: $path"
    (( (8#$mode & 022) == 0 )) || die "control file is group/world writable: $path"
  fi
}

secure_data_file() {
  local path=$1 mode owner
  direct_file "$path" || die "missing direct collector file: $path"
  if [[ $test_mode == false ]]; then
    owner=$(stat -c %U -- "$path")
    mode=$(stat -c %a -- "$path")
    [[ $owner == hftcollector ]] || die "collector file has the wrong owner: $path"
    (( (8#$mode & 022) == 0 )) \
      || die "collector file is group/world writable: $path"
  fi
}

check_sha() {
  local expected=$1 path=$2
  if [[ $test_mode == true ]]; then
    [[ $(sha256sum "$path" | awk '{print $1}') == "$expected" ]]
  else
    printf '%s  %s\n' "$expected" "$path" | sha256sum --check --strict >/dev/null
  fi
}

for directory in "$release_root" "$release" "$deployment"; do
  secure_root_directory "$directory" \
    || die "release directory is indirect or insecure: $directory"
done
secure_spool_directory "$spool" || die 'shadow spool directory is indirect or insecure'
for file in "$collector" "$verifier" "$release_json" "$control_manifest" \
  "$control_archive" "$deployment/$RUNNER" "$deployment/$GATE_POLICY" \
  "$deployment/$SERVICE_TEMPLATE" "$installed_service"; do
  secure_control_file "$file"
done
[[ -x $collector && -x $verifier && -x "$deployment/$RUNNER" ]] \
  || die 'candidate, verifier, and gate runner must be executable'
cmp -s "$deployment/$SERVICE_TEMPLATE" "$installed_service" \
  || die 'installed shadow service differs from the release bundle'

manifest_sha=$(sha256sum "$control_manifest" | awk '{print $1}')
archive_sha=$(sha256sum "$control_archive" | awk '{print $1}')
verifier_sha=$(sha256sum "$verifier" | awk '{print $1}')
jq -e --arg candidate "$candidate_sha" --arg verifier "$verifier_sha" \
  --arg manifest "$manifest_sha" --arg archive "$archive_sha" \
  --arg schema "$RELEASE_SCHEMA" '
  .schema == $schema
  and (keys | sort) == (["candidate","control_archive","control_manifest",
    "schema","source_revision","verifier"] | sort)
  and (.source_revision | type == "string" and test("^[a-f0-9]{40,64}$"))
  and .candidate == {file:"binance-usdm-reference-collector",sha256:$candidate}
  and .verifier == {file:"binance-usdm-reference-artifact-verifier",sha256:$verifier}
  and .control_manifest == {
    file:"binance-usdm-reference-control-assets.sha256",sha256:$manifest}
  and .control_archive == {
    file:"binance-usdm-reference-control.tar.gz",sha256:$archive}
' "$release_json" >/dev/null || die 'release identity or asset binding is invalid'
source_revision=$(jq -er .source_revision "$release_json")
check_sha "$candidate_sha" "$collector" || die 'candidate SHA-256 drifted'

expected_assets=$(printf '%s\n' "$GATE_POLICY" "$RUNNER" "$SERVICE_TEMPLATE" | sort)
actual_assets=$(awk 'NF == 2 && $1 ~ /^[a-f0-9]{64}$/ {print $2}' "$control_manifest" | sort)
[[ $actual_assets == "$expected_assets" \
  && $(awk 'NF == 2 && $1 ~ /^[a-f0-9]{64}$/ {count++} END {print count+0}' \
    "$control_manifest") == 3 ]] || die 'control manifest has an unexpected asset set'
(
  cd "$deployment"
  if [[ $test_mode == true ]]; then
    sha256sum --check "$control_manifest" >/dev/null
  else
    sha256sum --check --strict "$control_manifest" >/dev/null
  fi
) || die 'control bundle asset digest mismatch'

mkdir -p -- "$(dirname -- "$lock_file")"
secure_root_directory "$(dirname -- "$lock_file")" \
  || die 'release lock directory is indirect or insecure'
if [[ $test_mode == true ]]; then
  mkdir -- "$lock_file.test-lock" || die 'another test USD-M reference gate is running'
else
  command -v flock >/dev/null 2>&1 || die 'missing required command: flock'
  command -v mountpoint >/dev/null 2>&1 || die 'missing required command: mountpoint'
  mountpoint -q /data || die '/data must be a mount point'
  [[ ! -e $lock_file || -f $lock_file && ! -L $lock_file ]] \
    || die 'release lock is not a direct regular file'
  exec 9>>"$lock_file"
  secure_control_file "$lock_file"
  flock -n 9 || die 'another USD-M reference gate is running'
fi

gate_seconds=${MONDAY_REFERENCE_GATE_TEST_SECONDS:-$REQUIRED_DURATION_SECONDS}
grace_seconds=${MONDAY_REFERENCE_GATE_TEST_GRACE_SECONDS:-$OBSERVATION_GRACE_SECONDS}
[[ $gate_seconds =~ ^[1-9][0-9]*$ && $grace_seconds =~ ^[0-9]+$ ]] \
  || die 'gate and grace durations must be integers'
if [[ $test_mode == false ]]; then
  ((gate_seconds >= REQUIRED_DURATION_SECONDS)) \
    || die 'production gate duration cannot be shorter than 3600 seconds'
  [[ $gate_seconds == "$REQUIRED_DURATION_SECONDS" \
    && $grace_seconds == "$OBSERVATION_GRACE_SECONDS" ]] \
    || die 'production duration overrides are not allowed'
fi

systemctl_value() {
  systemctl show "$unit" --property="$1" --value
}

start_pid=''
start_restarts=''
start_invocation=''
end_pid=''
end_restarts=''
end_invocation=''

verify_process() {
  local pid=$1 expected actual
  [[ $pid =~ ^[1-9][0-9]*$ ]] || return 1
  [[ -L $proc_root/$pid/exe && $(readlink -f -- "$proc_root/$pid/exe") == "$collector" ]] \
    || return 1
  [[ -f $proc_root/$pid/cmdline && ! -L $proc_root/$pid/cmdline ]] || return 1
  expected=$(printf '%s\n' "$collector" --output-root "$spool" --interval-seconds 30 \
    --request-timeout-seconds 10 --oi-concurrency 2 --max-staleness-ms 30000)
  actual=$(tr '\0' '\n' <"$proc_root/$pid/cmdline")
  [[ $actual == "$expected" ]]
}

capture_identity() {
  local prefix=$1 fragment drop_ins pid restarts invocation
  systemctl is-active --quiet "$unit" || die 'shadow service is not active'
  fragment=$(systemctl_value FragmentPath)
  drop_ins=$(systemctl_value DropInPaths)
  pid=$(systemctl_value MainPID)
  restarts=$(systemctl_value NRestarts)
  invocation=$(systemctl_value InvocationID)
  [[ $fragment == "$installed_service" && -z $drop_ins ]] \
    || die 'shadow service fragment or drop-ins do not match the release'
  [[ $restarts == 0 && $invocation =~ ^[a-f0-9]{32}$ ]] \
    || die 'shadow service restarted or has no stable invocation identity'
  verify_process "$pid" || die 'shadow process identity or argv is invalid'
  printf -v "${prefix}_pid" '%s' "$pid"
  printf -v "${prefix}_restarts" '%s' "$restarts"
  printf -v "${prefix}_invocation" '%s' "$invocation"
}

capture_identity start
if [[ $test_mode == true ]]; then
  start_ns=${MONDAY_REFERENCE_GATE_TEST_NOW_NS:-}
  [[ $start_ns =~ ^[1-9][0-9]*$ ]] || die 'test mode requires a positive start time'
  end_ns=$((start_ns + (gate_seconds + grace_seconds) * 1000000000))
else
  [[ -r /proc/uptime ]] || die '/proc/uptime is required for monotonic timing'
  start_uptime_seconds=$(awk '{print int($1)}' /proc/uptime)
  start_ns=$(date -u +%s%N)
  [[ $start_ns =~ ^[1-9][0-9]{18}$ ]] || die 'could not read nanosecond wall clock'
  sleep "$((gate_seconds + grace_seconds))"
  end_ns=$(date -u +%s%N)
  end_uptime_seconds=$(awk '{print int($1)}' /proc/uptime)
  [[ $end_ns =~ ^[1-9][0-9]{18}$ && $end_ns -ge $start_ns ]] \
    || die 'wall clock regressed during the gate'
  ((end_uptime_seconds - start_uptime_seconds >= gate_seconds + grace_seconds)) \
    || die 'monotonic observation duration is too short'
fi

temp_dir=$(mktemp -d)
cleanup() {
  rm -rf -- "$temp_dir"
}
trap cleanup EXIT

symlinked=$(find "$spool" -type l \( -name reference.ndjson \
  -o -name reference.ndjson.manifest.json -o -name reference.ndjson._SUCCESS \) \
  -print -quit)
[[ -z $symlinked ]] || die "reference artifact contains a symlink: $symlinked"

artifact_count=0
while IFS= read -r manifest; do
  secure_data_file "$manifest"
  observed=$(jq -er '.observed_at_ns | select(type == "number" and . == floor)' "$manifest")
  [[ $observed =~ ^[1-9][0-9]*$ ]] || die "invalid artifact observation time: $manifest"
  ((observed >= start_ns && observed <= end_ns)) || continue
  data=${manifest%.manifest.json}
  success="$data._SUCCESS"
  secure_data_file "$data"
  secure_data_file "$success"
  data_sha=$(jq -er '.sha256 | select(type == "string" and test("^[a-f0-9]{64}$"))' \
    "$manifest")
  current_manifest_sha=$(sha256sum "$manifest" | awk '{print $1}')
  verification=$("$verifier" --data-path "$data" --data-sha256 "$data_sha" \
    --manifest-sha256 "$current_manifest_sha") \
    || die "canonical verifier rejected artifact: $manifest"
  jq -e --arg schema "$VERIFICATION_SCHEMA" --arg path "$data" \
    --arg data "$data_sha" --arg manifest "$current_manifest_sha" \
    --slurpfile source "$manifest" '
    .schema == $schema and .data_path == $path and .data_sha256 == $data
    and .manifest_sha256 == $manifest and .content_rows_verified == true
    and .metadata_observations == $source[0].coverage.metadata_observations
    and .mark_index_funding_observations ==
      $source[0].coverage.mark_index_funding_observations
    and .open_interest_observations == $source[0].coverage.open_interest_observations
  ' <<<"$verification" >/dev/null || die "verifier output is not bound to artifact: $manifest"
  [[ ! -e $temp_dir/$observed.json ]] || die 'duplicate artifact observation time'
  jq -c --arg manifest_sha "$current_manifest_sha" '
    {canonical_readback:true,dataset:.dataset,venue:.venue,
      manifest_schema:.schema,data_schema:.data_schema,source_origin:.source_origin,
      source_endpoints:.source_endpoints,max_staleness_ms:.max_staleness_ms,
      data_sha256:.sha256,manifest_sha256:$manifest_sha,success_sha256:.sha256,
      content_rows_verified:true,observed_at_ns:.observed_at_ns,
      time_bounds:.time_bounds,coverage:.coverage}
  ' "$manifest" >"$temp_dir/$observed.json"
  check_sha "$data_sha" "$data" || die "artifact changed after readback: $data"
  check_sha "$current_manifest_sha" "$manifest" \
    || die "manifest changed after readback: $manifest"
  artifact_count=$((artifact_count + 1))
done < <(find "$spool" -type f -name reference.ndjson.manifest.json | sort)
((artifact_count >= 3)) || die 'fewer than three new canonical artifacts were observed'
artifacts=$(jq -s 'sort_by(.observed_at_ns)' "$temp_dir"/*.json)
span_ns=$(jq -er '.[-1].observed_at_ns - .[0].observed_at_ns' <<<"$artifacts")
max_gap_ns=$(jq -er '[range(1; length) as $i |
  .[$i].observed_at_ns - .[$i - 1].observed_at_ns] as $gaps
  | select(all($gaps[]; . > 0 and . <= 90000000000)) | ($gaps | max)' \
  <<<"$artifacts") || die 'artifact observations are discontinuous'
((span_ns >= gate_seconds * 1000000000)) \
  || die 'artifact observation span is shorter than the gate duration'
((max_gap_ns <= MAX_ARTIFACT_GAP_NS)) || die 'artifact gap exceeds 90 seconds'

health="$spool/health.json"
secure_data_file "$health"
latest_data_sha=$(jq -er '.[-1].data_sha256' <<<"$artifacts")
latest_manifest_sha=$(jq -er '.[-1].manifest_sha256' <<<"$artifacts")
latest_observed=$(jq -er '.[-1].observed_at_ns' <<<"$artifacts")
latest_data=$(find "$spool" -type f -name reference.ndjson.manifest.json \
  -exec jq -er --argjson observed "$latest_observed" \
    'select(.observed_at_ns == $observed) | input_filename' {} \; | head -n 1)
latest_data=${latest_data%.manifest.json}
jq -e --arg schema "$HEALTH_SCHEMA" --arg data "$latest_data_sha" \
  --arg manifest "$latest_manifest_sha" --arg path "$latest_data" \
  --argjson observed "$latest_observed" '
  .schema == $schema and .status == "healthy"
  and .source_origin == "https://fapi.binance.com"
  and .api_error_count == 0 and .total_api_errors == 0
  and .artifact_error_count == 0 and .total_artifact_errors == 0
  and .last_error == null and .data_path == $path
  and .data_sha256 == $data and .manifest_sha256 == $manifest
  and (.last_success_at_ns | type == "number" and . == floor)
  and .last_success_at_ns >= $observed
  and .last_success_at_ns - $observed <= 90000000000
' "$health" >/dev/null || die 'health is stale, erroneous, or not bound to the latest artifact'
health_evidence=$(jq '{schema,status,source_origin,api_error_count,total_api_errors,
  artifact_error_count,total_artifact_errors,last_success_at_ns,data_sha256,
  manifest_sha256}' "$health")

capture_identity end
[[ $end_pid == "$start_pid" && $end_restarts == "$start_restarts" \
  && $end_invocation == "$start_invocation" ]] \
  || die 'shadow service identity changed during the gate'
check_sha "$candidate_sha" "$collector" || die 'candidate changed during the gate'
check_sha "$verifier_sha" "$verifier" || die 'verifier changed during the gate'

for directory in "$(prefix_path /data/monday)" "$(prefix_path /data/monday/evidence)" \
  "$evidence_root"; do
  mkdir -p -- "$directory"
  secure_root_directory "$directory" || die "evidence directory is insecure: $directory"
done
bundle_dir="$evidence_root/$candidate_sha/$manifest_sha/runs"
mkdir -p -- "$bundle_dir"
secure_root_directory "$bundle_dir" || die 'gate runs directory is indirect or insecure'
run_id="$(date -u +%Y%m%dT%H%M%SZ)-${start_ns}-$$"
evidence_dir="$bundle_dir/$run_id"
mkdir -- "$evidence_dir" || die 'gate evidence run already exists'
gate_json="$evidence_dir/gate.json"
production_eligible=true
[[ $test_mode == false ]] || production_eligible=false
# The artifacts array exceeds MAX_ARG_STRLEN (128 KiB) for a full 3600-second
# observation (~110 artifacts), so it must reach jq through a file, not an
# inline --argjson argument.
artifacts_file="$temp_dir/.artifacts.json"
printf '%s\n' "$artifacts" >"$artifacts_file"
jq -S -n --arg schema "$GATE_SCHEMA" --arg candidate "$candidate_sha" \
  --arg bundle "$manifest_sha" --arg source "$source_revision" \
  --arg unit "$unit" --arg invocation "$start_invocation" \
  --argjson duration "$gate_seconds" --argjson eligible "$production_eligible" \
  --argjson health "$health_evidence" --slurpfile artifacts "$artifacts_file" \
  --argjson max_gap "$max_gap_ns" '
  {schema:$schema,candidate_sha256:$candidate,
    deployment_bundle_sha256:$bundle,deployment_source_revision:$source,
    passed:$eligible,production_eligible:$eligible,duration_seconds:$duration,
    service:{unit:$unit,active:true,restart_count:0,binary_sha256:$candidate,
      invocation_id_start:$invocation,invocation_id_end:$invocation},
    health:$health,artifact_count:($artifacts[0]|length),max_artifact_gap_ns:$max_gap,
    artifacts:$artifacts[0]}
' >"$gate_json"
chmod 0640 "$gate_json"

if [[ $test_mode == false ]]; then
  jq -e --arg candidate_sha256 "$candidate_sha" \
    --arg deployment_bundle_sha256 "$manifest_sha" \
    --arg deployment_source_revision "$source_revision" \
    -f "$deployment/$GATE_POLICY" "$gate_json" >/dev/null \
    || die 'compiled production evidence failed the release gate policy'
  gate_sha=$(sha256sum "$gate_json" | awk '{print $1}')
  (set -C; printf '%s  gate.json\n' "$gate_sha" >"$evidence_dir/PASSED.sha256") \
    || die 'could not publish immutable PASSED marker'
  chmod 0640 "$evidence_dir/PASSED.sha256"
fi

printf '%s\n' "$gate_json"
