#!/usr/bin/env bash
set -euo pipefail

umask 027
export LC_ALL=C

readonly REQUIRED_DURATION_SECONDS=3600
# The verifier subtracts a 600-second trade maturity lag and requires a
# non-empty event window, so the deployment tail must be strictly longer.
readonly PARITY_TAIL_SECONDS=601
readonly MINIMUM_GATE_SECONDS=$((REQUIRED_DURATION_SECONDS + PARITY_TAIL_SECONDS))
readonly MAX_ACCEPTED_CYCLE_SECONDS=180
readonly INITIAL_HEALTH_GRACE_SECONDS=60
readonly HEALTH_SETTLE_SECONDS=$((MAX_ACCEPTED_CYCLE_SECONDS + INITIAL_HEALTH_GRACE_SECONDS))
readonly MAX_HEALTH_SILENCE_SECONDS=240
readonly SAMPLE_SECONDS=30
readonly PARITY_CUTOFF_LAG_SECONDS=60
readonly SETTLEMENT_EVENT_LOOKBACK_SECONDS=900
readonly LEGACY_UNIT=polymarket-reference-collector.service
readonly LEGACY_EXEC='/usr/bin/python3 /opt/monday/bin/polymarket_reference_collector.py'
readonly RUST_PRODUCTION_EXEC='/opt/monday/bin/polymarket-raw-ops collect-reference'
readonly RUST_ACTIVE_BINARY=/opt/monday/bin/polymarket-raw-ops
readonly CONTROL_DIR=/opt/monday/control/polymarket-raw-ops
readonly LEGACY_FRAGMENT=/etc/systemd/system/polymarket-reference-collector.service
readonly SHADOW_FRAGMENT=/etc/systemd/system/polymarket-reference-collector-shadow@.service
readonly LEGACY_SPOOL=/data/monday/spool/polymarket-reference
readonly UPLOAD_ENV=/etc/monday/polymarket-market-tape-upload.env
readonly RELEASE_ROOT=/opt/monday/releases/polymarket-raw-ops
readonly SHADOW_ROOT=/data/monday/spool/polymarket-reference-rust-shadow
readonly EVIDENCE_ROOT=/data/monday/evidence/polymarket-shadow-gates
readonly LOCK_FILE=/run/monday/polymarket-raw-ops.lock
readonly RELEASE_MANIFEST_SCHEMA=monday.polymarket_raw_ops_release.v1
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
readonly SCRIPT_DIR
readonly RELEASE_MANIFEST="$SCRIPT_DIR/polymarket-raw-ops-release.json"
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
    'A production-eligible gate observes for 3600 seconds plus a 601-second current-hour parity tail.'
}

valid_parity_window() {
  local started_at=$1 ended_at=$2
  [[ $started_at =~ ^(0|[1-9][0-9]*)$ && $ended_at =~ ^(0|[1-9][0-9]*)$ ]] \
    && ((started_at < ended_at))
}

bounded_parity_window_start() {
  local gate_started_at=$1 common_cutoff=$2 allow_short=$3 lookback_seconds=$4
  local parity_started_at settlement_safe_started_at
  parity_started_at=$((common_cutoff - common_cutoff % 3600))
  settlement_safe_started_at=$((gate_started_at + lookback_seconds))
  ((parity_started_at >= settlement_safe_started_at)) \
    || parity_started_at=$settlement_safe_started_at
  if [[ $allow_short == true ]] && ((parity_started_at >= common_cutoff)); then
    ((common_cutoff > 0)) || return 1
    parity_started_at=$((common_cutoff - 1))
  fi
  printf '%s\n' "$parity_started_at"
}

bundle_sha256() {
  local directory=${1:-$SCRIPT_DIR}
  (
    cd "$directory"
    sha256sum "${BUNDLE_ASSETS[@]}" | sha256sum | awk '{print $1}'
  )
}

release_control_assets() {
  local control_dir=$1 gate=$1/polymarket-raw-ops-shadow-gate.sh
  [[ -f $gate && ! -L $gate ]] || return 1
  awk '
    $0 == "readonly -a BUNDLE_ASSETS=(" {
      if (found || inside) exit 2
      found = 1
      inside = 1
      next
    }
    inside && $0 == ")" {
      inside = 0
      closed = 1
      next
    }
    inside {
      if ($0 !~ /^  [A-Za-z0-9@._][A-Za-z0-9@._-]*$/) exit 2
      sub(/^  /, "")
      if ($0 == "." || $0 == ".." || seen[$0]++) exit 2
      if ($0 == "polymarket-raw-ops-shadow-gate.sh") has_gate = 1
      print
      count += 1
    }
    END {
      if (!found || !closed || inside || count == 0 || !has_gate) exit 2
    }
  ' "$gate"
}

direct_directory() {
  local path=$1
  [[ -d $path && ! -L $path && $(readlink -f -- "$path") == "$path" ]]
}

secure_root_directory() {
  local path=$1 owner mode
  direct_directory "$path" || return 1
  owner=$(stat -c %u -- "$path") || return 1
  mode=$(stat -c %a -- "$path") || return 1
  [[ $owner == 0 ]] && (( (8#$mode & 0022) == 0 ))
}

valid_absolute_path() {
  local path=$1
  [[ $path == /* && $path != *//* && $path != */./* && $path != */../* \
    && $path != */. && $path != */.. ]]
}

secure_root_chain() {
  local path=$1 remainder component current=
  valid_absolute_path "$path" || return 1
  if [[ $path == / ]]; then
    secure_root_directory /
    return
  fi
  remainder=${path#/}
  while [[ -n $remainder ]]; do
    component=${remainder%%/*}
    [[ -n $component ]] || return 1
    current="$current/$component"
    secure_root_directory "$current" || return 1
    [[ $remainder == "$component" ]] && break
    remainder=${remainder#*/}
  done
}

secure_root_chain_or_absent() {
  local path=$1 ancestor=$1 parent
  valid_absolute_path "$path" || return 1
  [[ ! -L $path ]] || return 1
  if [[ -e $path ]]; then
    secure_root_chain "$path"
    return
  fi
  while [[ ! -e $ancestor && ! -L $ancestor ]]; do
    parent=${ancestor%/*}
    [[ -n $parent ]] || parent=/
    [[ $parent != "$ancestor" ]] || return 1
    ancestor=$parent
  done
  [[ ! -L $ancestor ]] || return 1
  secure_root_chain "$ancestor"
}

secure_collector_directory() {
  local path=$1 owner group mode parent
  direct_directory "$path" || return 1
  owner=$(stat -c %U -- "$path") || return 1
  group=$(stat -c %G -- "$path") || return 1
  mode=$(stat -c %a -- "$path") || return 1
  [[ $owner == hftcollector && $group == hftcollector && $mode == 750 ]] || return 1
  parent=${path%/*}
  secure_root_chain "$parent"
}

secure_release_directory() {
  local path=$1 owner mode
  secure_root_chain "$path" || return 1
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

verify_release_manifest() {
  local manifest=$1
  secure_control_file "$manifest" || return 1
  jq -e -s --arg schema "$RELEASE_MANIFEST_SCHEMA" '
    length == 1 and (.[0] |
      .schema == $schema
      and (keys | sort) == (["candidate","control_archive","control_manifest",
        "schema","source_revision"] | sort)
      and (.source_revision | type == "string" and test("^[a-f0-9]{40,64}$"))
      and .candidate.file == "polymarket-raw-ops"
      and (.candidate | keys | sort) == ["file","sha256"]
      and (.candidate.sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and .control_manifest.file == "polymarket-raw-ops-control-assets.sha256"
      and (.control_manifest | keys | sort) == ["file","sha256"]
      and (.control_manifest.sha256 | type == "string" and test("^[a-f0-9]{64}$"))
      and .control_archive.file == "polymarket-raw-ops-control.tar.gz"
      and (.control_archive | keys | sort) == ["file","sha256"]
      and (.control_archive.sha256 | type == "string" and test("^[a-f0-9]{64}$"))
    )
  ' "$manifest" >/dev/null
}

verify_release_binding() {
  local manifest=$1 expected_manifest_sha=$2 expected_candidate_sha=$3
  local expected_source_revision=$4 expected_bundle_sha=$5 expected_archive_sha=$6
  local candidate=$7 control_dir=${8:-$SCRIPT_DIR}
  verify_release_manifest "$manifest" || return 1
  [[ $(sha256sum "$manifest" | awk '{print $1}') == "$expected_manifest_sha" ]] \
    || return 1
  [[ $(jq -er -s '.[0].candidate.sha256' "$manifest") \
    == "$expected_candidate_sha" ]] \
    || return 1
  [[ $(jq -er -s '.[0].source_revision' "$manifest") \
    == "$expected_source_revision" ]] \
    || return 1
  [[ $(jq -er -s '.[0].control_manifest.sha256' "$manifest") \
    == "$expected_bundle_sha" ]] || return 1
  [[ $(jq -er -s '.[0].control_archive.sha256' "$manifest") \
    == "$expected_archive_sha" ]] || return 1
  [[ $(bundle_sha256 "$control_dir") == "$expected_bundle_sha" ]] || return 1
  printf '%s  %s\n' "$expected_candidate_sha" "$candidate" \
    | sha256sum --check --strict >/dev/null
}

verify_control_release() {
  local control_dir=$1 expected_sha=$2 expected_binary=$3 manifest asset assets
  local actual_bundle_sha expected_bundle_sha
  local -a release_assets=()
  manifest="$control_dir/${RELEASE_MANIFEST##*/}"
  secure_control_file "$manifest" || return 1
  verify_release_manifest "$manifest" || return 1
  assets=$(release_control_assets "$control_dir") || return 1
  while IFS= read -r asset; do
    [[ $asset != "${RELEASE_MANIFEST##*/}" ]] || return 1
    release_assets+=("$asset")
    secure_control_file "$control_dir/$asset" || return 1
  done <<<"$assets"
  actual_bundle_sha=$(
    cd "$control_dir"
    sha256sum -- "${release_assets[@]}" | sha256sum | awk '{print $1}'
  ) || return 1
  expected_bundle_sha=$(jq -er '.control_manifest.sha256' "$manifest") || return 1
  [[ $actual_bundle_sha == "$expected_bundle_sha" ]] || return 1
  [[ $(jq -er '.candidate.sha256' "$manifest") == "$expected_sha" ]] || return 1
  printf '%s  %s\n' "$expected_sha" "$expected_binary" \
    | sha256sum --check --strict >/dev/null
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

journal_cursor() {
  local unit=$1 cursor
  journalctl --sync || return 1
  cursor=$(journalctl --unit "$unit" --lines=0 --show-cursor --no-pager \
    | sed -n 's/^-- cursor: //p') || return 1
  [[ -n $cursor ]] || return 1
  printf '%s\n' "$cursor"
}

verify_no_restart_after_cursor() {
  local unit=$1 cursor=$2 expected_invocation_id=$3
  journalctl --sync || return 1
  journalctl --unit "$unit" --after-cursor "$cursor" --output=json --no-pager \
    | jq -s -e --arg expected "$expected_invocation_id" '
      all(.[];
        ((.MESSAGE_ID // "") != "5eb03494b6584870a536b337290809b3")
        and ((.INVOCATION_ID // "") | length == 0 or . == $expected)
        and ((._SYSTEMD_INVOCATION_ID // "") | length == 0 or . == $expected)
      )
    ' >/dev/null || return 1
}

verify_runtime_identity() {
  local expected_exec=$1 expected_pid=$2 expected_restarts=$3 expected_invocation_id=$4
  local pid restarts invocation_id fragment drop_ins exec_argv cmdline
  systemctl is-active --quiet "$LEGACY_UNIT" || return 1
  fragment=$(systemctl show --property=FragmentPath --value "$LEGACY_UNIT") || return 1
  [[ $fragment == "$LEGACY_FRAGMENT" ]] || return 1
  drop_ins=$(systemctl show --property=DropInPaths --value "$LEGACY_UNIT") || return 1
  [[ -z $drop_ins ]] || return 1
  exec_argv=$(effective_exec_argv "$LEGACY_UNIT") || return 1
  [[ $exec_argv == "$expected_exec" ]] || return 1
  pid=$(systemctl show --property=MainPID --value "$LEGACY_UNIT") || return 1
  [[ $pid == "$expected_pid" ]] || return 1
  restarts=$(systemctl show --property=NRestarts --value "$LEGACY_UNIT") || return 1
  [[ $restarts == "$expected_restarts" ]] || return 1
  invocation_id=$(systemctl show --property=InvocationID --value "$LEGACY_UNIT") || return 1
  [[ $invocation_id == "$expected_invocation_id" ]] || return 1
  cmdline=$(proc_cmdline "$pid") || return 1
  [[ $cmdline == "$expected_exec " ]]
}

verify_legacy_identity() {
  verify_runtime_identity "$LEGACY_EXEC" "$@"
}

verify_baseline_identity() {
  if [[ $baseline_mode == legacy_python ]]; then
    verify_legacy_identity "$legacy_pid" "$legacy_restarts" "$legacy_invocation_id"
    return
  fi
  verify_runtime_identity "$RUST_PRODUCTION_EXEC" "$legacy_pid" \
    "$legacy_restarts" "$legacy_invocation_id" || return 1
  [[ $(readlink -f -- "$RUST_ACTIVE_BINARY") == "$baseline_release_path" ]] \
    || return 1
  secure_release_directory "${baseline_release_path%/*}" || return 1
  secure_control_file "$baseline_release_path"
  [[ -x $baseline_release_path ]] || return 1
  printf '%s  %s\n' "$baseline_release_sha" "$baseline_release_path" \
    | sha256sum --check --strict >/dev/null || return 1
  [[ $(readlink -f -- "/proc/$legacy_pid/exe") == "$baseline_release_path" ]]
}

verify_cutover_target_preflight() {
  local baseline_mode=$1 active_binary=$2 control_dir=$3 release_manifest_name=$4
  local file_verifier=$5 unit fragment expected_fragment drop_ins asset assets
  [[ $baseline_mode == legacy_python || $baseline_mode == rust_release ]] || return 1
  [[ $baseline_mode != legacy_python || ( ! -e $active_binary && ! -L $active_binary ) ]] \
    || return 1
  secure_root_chain_or_absent "$control_dir" || return 1
  for unit in polymarket-reference-collector.service \
    polymarket-reference-upload.service polymarket-reference-upload.timer \
    polymarket-market-tape-upload.service polymarket-market-tape-upload.timer; do
    expected_fragment="/etc/systemd/system/$unit"
    fragment=$(systemctl show --property=FragmentPath --value "$unit") || return 1
    [[ $fragment == "$expected_fragment" ]] || return 1
    "$file_verifier" "$expected_fragment" || return 1
    drop_ins=$(systemctl show --property=DropInPaths --value "$unit") || return 1
    [[ -z $drop_ins ]] || return 1
  done
  if [[ -e $control_dir || -L $control_dir ]]; then
    direct_directory "$control_dir" && secure_root_chain "$control_dir" || return 1
    assets=$(release_control_assets "$control_dir") || return 1
    while IFS= read -r asset; do
      [[ $asset != "$release_manifest_name" ]] || return 1
      "$file_verifier" "$control_dir/$asset" || return 1
    done <<<"$assets"
    "$file_verifier" "$control_dir/$release_manifest_name" || return 1
  fi
}

fresh_baseline_health_snapshot() {
  local health=$1 policy=${2:-$LEGACY_HEALTH_POLICY}
  local snapshot field timestamp epoch now
  [[ -f $health && ! -L $health ]] || return 1
  snapshot=$(jq -cS . "$health") || return 1
  jq -e -f "$policy" <<<"$snapshot" >/dev/null || return 1
  now=$(date -u +%s) || return 1
  for field in updated_at last_success_at; do
    timestamp=$(jq -er --arg field "$field" \
      '.[$field] | select(type == "string" and length > 0)' <<<"$snapshot") || return 1
    epoch=$(date -u -d "$timestamp" +%s) || return 1
    ((epoch <= now && now - epoch <= MAX_HEALTH_SILENCE_SECONDS)) || return 1
  done
  printf '%s\n' "$snapshot"
}

verify_fresh_baseline_health() {
  fresh_baseline_health_snapshot "$@" >/dev/null
}

baseline_health_requires_continuous_freshness() {
  [[ $1 == rust_release ]]
}

legacy_health_sample_state() {
  local health=$1 policy=$2 baseline_mode=$3
  if jq -e -f "$policy" "$health" >/dev/null; then
    printf '%s\n' clean
  elif [[ $baseline_mode == legacy_python ]] \
    && jq -e '.api_errors | type == "array" and length > 0' "$health" >/dev/null \
    && jq '.api_errors = []' "$health" | jq -e -f "$policy" >/dev/null; then
    printf '%s\n' transient_api_error
  else
    printf '%s\n' fatal
  fi
}

legacy_health_transition() {
  local sample_state=$1 error_started_at=$2 now_uptime=$3 budget_seconds=$4
  local decision=fatal
  case "$sample_state" in
    clean)
      if [[ -n $error_started_at ]] \
        && ((now_uptime - error_started_at > budget_seconds)); then
        decision=expired
      else
        error_started_at=
        decision=advance
      fi
      ;;
    transient_api_error)
      [[ -n $error_started_at ]] || error_started_at=$now_uptime
      if ((now_uptime - error_started_at <= budget_seconds)); then
        decision='wait'
      else
        decision=expired
      fi
      ;;
  esac
  printf '%s:%s\n' "$decision" "$error_started_at"
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
  local expected_pid=$1 expected_invocation_id=$2
  local pid restarts invocation_id fragment drop_ins exec_argv cmdline
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
  invocation_id=$(systemctl show --property=InvocationID --value "$shadow_unit") || return 1
  [[ $invocation_id == "$expected_invocation_id" ]] || return 1
  [[ $(readlink -f "/proc/$pid/exe") == "$release_binary" ]] || return 1
  cmdline=$(proc_cmdline "$pid") || return 1
  [[ $cmdline == "$release_binary collect-reference --spool-dir $shadow_spool " ]]
}

valid_shadow_control_group() {
  local control_group=$1
  valid_absolute_path "$control_group" \
    && [[ $control_group == /system.slice/* \
      && ${control_group##*/} == "$shadow_unit" ]]
}

shadow_memory_events_file() {
  local pid=$1 control_group cgroup_dir file proc_cgroup_file proc_binding owner mode
  control_group=$(systemctl show --property=ControlGroup --value "$shadow_unit") \
    || return 1
  valid_shadow_control_group "$control_group" || return 1

  proc_cgroup_file="/proc/$pid/cgroup"
  [[ -f $proc_cgroup_file && ! -L $proc_cgroup_file \
    && $(readlink -f -- "$proc_cgroup_file") == "$proc_cgroup_file" ]] || return 1
  proc_binding=$(<"$proc_cgroup_file") || return 1
  [[ $proc_binding == "0::$control_group" ]] || return 1

  cgroup_dir="/sys/fs/cgroup$control_group"
  direct_directory "$cgroup_dir" || return 1
  owner=$(stat -c %u -- "$cgroup_dir") || return 1
  mode=$(stat -c %a -- "$cgroup_dir") || return 1
  [[ $owner == 0 ]] || return 1
  (( (8#$mode & 022) == 0 )) || return 1

  file="$cgroup_dir/memory.events"
  [[ -f $file && ! -L $file \
    && $(readlink -f -- "$file") == "$file" ]] || return 1
  owner=$(stat -c %u -- "$file") || return 1
  mode=$(stat -c %a -- "$file") || return 1
  [[ $owner == 0 ]] || return 1
  (( (8#$mode & 022) == 0 )) || return 1
  printf '%s\n' "$file"
}

memory_events_snapshot() {
  local file=$1 key value high='' max='' oom='' oom_kill='' oom_group_kill=''
  while read -r key value; do
    [[ $value =~ ^[0-9]+$ ]] || return 1
    case "$key" in
      high) high=$value ;;
      max) max=$value ;;
      oom) oom=$value ;;
      oom_kill) oom_kill=$value ;;
      oom_group_kill) oom_group_kill=$value ;;
    esac
  done <"$file"
  [[ $high =~ ^[0-9]+$ && $max =~ ^[0-9]+$ && $oom =~ ^[0-9]+$ \
    && $oom_kill =~ ^[0-9]+$ && $oom_group_kill =~ ^[0-9]+$ ]] || return 1
  printf '%s %s %s %s %s\n' \
    "$high" "$max" "$oom" "$oom_kill" "$oom_group_kill"
}

stable_memory_events_snapshot() {
  local file=$1 baseline_high=$2 snapshot high max oom oom_kill oom_group_kill
  snapshot=$(memory_events_snapshot "$file") || return 1
  read -r high max oom oom_kill oom_group_kill <<<"$snapshot"
  [[ $high == "$baseline_high" && $max == 0 && $oom == 0 \
    && $oom_kill == 0 && $oom_group_kill == 0 ]] || return 1
  printf '%s\n' "$snapshot"
}

memory_events_json() {
  local snapshot=$1 high max oom oom_kill oom_group_kill
  read -r high max oom oom_kill oom_group_kill <<<"$snapshot"
  jq -cn \
    --argjson high "$high" \
    --argjson max "$max" \
    --argjson oom "$oom" \
    --argjson oom_kill "$oom_kill" \
    --argjson oom_group_kill "$oom_group_kill" \
    '{high:$high,max:$max,oom:$oom,oom_kill:$oom_kill,
      oom_group_kill:$oom_group_kill}'
}

[[ ${EUID} -eq 0 ]] || die 'must run as root'
[[ $# -eq 3 ]] || {
  usage >&2
  exit 2
}

for command in awk chown chmod date flock grep install journalctl jq mkdir mktemp mountpoint mv \
  readlink rm runuser sed sha256sum sleep stat sync systemctl tr wc; do
  command -v "$command" >/dev/null 2>&1 || die "missing required command: $command"
done

mountpoint -q /data || die '/data must be a mount point'
for path in "$SCRIPT_DIR" /opt/monday /opt/monday/bin /opt/monday/releases \
  "$RELEASE_ROOT" /etc/monday /etc/systemd/system /data /data/monday \
  /data/monday/spool "$SHADOW_ROOT" /data/monday/evidence "$EVIDENCE_ROOT" \
  /run/monday; do
  secure_root_chain_or_absent "$path" \
    || die "trusted path chain is not root-owned and non-writable: $path"
done
secure_collector_directory "$LEGACY_SPOOL" \
  || die 'legacy spool is not an exact hftcollector-owned 0750 directory'

candidate_source=$1
candidate_sha_cli=$(printf '%s' "$2" | tr '[:upper:]' '[:lower:]')
source_revision_cli=$(printf '%s' "$3" | tr '[:upper:]' '[:lower:]')
verify_release_manifest "$RELEASE_MANIFEST" || die 'installed release manifest is invalid'
candidate_sha=$(jq -er '.candidate.sha256' "$RELEASE_MANIFEST")
source_revision=$(jq -er '.source_revision' "$RELEASE_MANIFEST")
manifest_deployment_bundle_sha=$(jq -er '.control_manifest.sha256' "$RELEASE_MANIFEST")
control_archive_sha=$(jq -er '.control_archive.sha256' "$RELEASE_MANIFEST")
release_manifest_sha=$(sha256sum "$RELEASE_MANIFEST" | awk '{print $1}')
[[ $candidate_sha_cli == "$candidate_sha" ]] \
  || die 'candidate CLI digest differs from the verified release manifest'
[[ $source_revision_cli == "$source_revision" ]] \
  || die 'source CLI revision differs from the verified release manifest'
[[ $candidate_sha =~ ^[a-f0-9]{64}$ ]] || die 'candidate SHA-256 is invalid'
[[ $source_revision =~ ^[a-f0-9]{40,64}$ ]] || die 'source revision is invalid'
[[ $control_archive_sha =~ ^[a-f0-9]{64}$ ]] \
  || die 'control archive identity is invalid'
[[ -f $candidate_source && ! -L $candidate_source && -x $candidate_source ]] \
  || die 'candidate must be a direct executable regular file'
printf '%s  %s\n' "$candidate_sha" "$candidate_source" \
  | sha256sum --check --strict >/dev/null || die 'candidate checksum mismatch'

for asset in "${BUNDLE_ASSETS[@]}"; do
  secure_control_file "$SCRIPT_DIR/$asset"
done
secure_control_file "$UPLOAD_ENV"
deployment_bundle_sha=$(bundle_sha256)
[[ $deployment_bundle_sha == "$manifest_deployment_bundle_sha" ]] \
  || die 'installed control bundle differs from the verified release manifest'
verify_release_binding "$RELEASE_MANIFEST" "$release_manifest_sha" \
  "$candidate_sha" "$source_revision" "$deployment_bundle_sha" \
  "$control_archive_sha" "$candidate_source" \
  || die 'release manifest does not bind the candidate and installed control bundle'
load_oss_config_snapshot

install -d -m 0755 /run/monday
secure_root_chain /run/monday || die 'runtime control directory is not trusted'
exec 9>"$LOCK_FILE"
flock -n 9 || die 'another Polymarket release operation is running'

baseline_exec=$(effective_exec_argv "$LEGACY_UNIT") || \
  die 'active reference collector has no verifiable ExecStart'
baseline_release_path=
baseline_release_sha=
case "$baseline_exec" in
  "$LEGACY_EXEC")
    baseline_mode=legacy_python
    baseline_label=Python
    ;;
  "$RUST_PRODUCTION_EXEC")
    baseline_mode=rust_release
    baseline_label='Rust production'
    baseline_release_path=$(readlink -f -- "$RUST_ACTIVE_BINARY") || \
      die 'active Rust collector symlink cannot be resolved'
    [[ $baseline_release_path =~ ^$RELEASE_ROOT/([a-f0-9]{64})/polymarket-raw-ops$ ]] \
      || die 'active Rust collector does not resolve to an immutable release'
    baseline_release_sha=${BASH_REMATCH[1]}
    [[ $candidate_sha != "$baseline_release_sha" ]] || \
      die 'candidate digest matches the active Rust release'
    ;;
  *) die 'active reference collector ExecStart is not an approved baseline' ;;
esac
legacy_pid=$(systemctl show --property=MainPID --value "$LEGACY_UNIT")
[[ $legacy_pid =~ ^[1-9][0-9]*$ ]] || die 'active legacy collector has no verifiable MainPID'
legacy_restarts=$(systemctl show --property=NRestarts --value "$LEGACY_UNIT")
[[ $legacy_restarts =~ ^[0-9]+$ ]] \
  || die 'active legacy collector has no verifiable restart counter'
legacy_invocation_id=$(systemctl show --property=InvocationID --value "$LEGACY_UNIT")
[[ $legacy_invocation_id =~ ^[a-f0-9]{32}$ ]] \
  || die 'active legacy collector has no verifiable systemd invocation ID'
verify_baseline_identity \
  || die 'active reference collector identity or restart counter is not exact'
! baseline_health_requires_continuous_freshness "$baseline_mode" \
  || verify_fresh_baseline_health "$LEGACY_SPOOL/health.json" \
  || die 'active Rust collector health is not fresh and fail-closed clean'
verify_cutover_target_preflight "$baseline_mode" "$RUST_ACTIVE_BINARY" \
  "$CONTROL_DIR" "${RELEASE_MANIFEST##*/}" secure_control_file \
  || die 'production cutover target state would reject promotion'
[[ $baseline_mode != rust_release ]] \
  || verify_control_release "$CONTROL_DIR" "$baseline_release_sha" "$baseline_release_path" \
  || die 'global controls do not bind the active Rust baseline'

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
  if [[ -n ${shadow_unit:-} ]]; then
    systemctl thaw "$shadow_unit" >/dev/null 2>&1 || true
    systemctl stop "$shadow_unit" >/dev/null 2>&1 || true
  fi
  rm -rf "${staging:-}"
  rm -rf "${control_staging:-}"
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
  secure_root_chain "$RELEASE_ROOT" || die 'release root is not trusted after creation'
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
release_control_dir="$release_dir/control"
if [[ ! -e $release_control_dir && ! -L $release_control_dir ]]; then
  control_staging=$(mktemp -d "$release_dir/.control.new.XXXXXX")
  for asset in "${BUNDLE_ASSETS[@]}"; do
    mode=0644; [[ $asset == *.sh ]] && mode=0755
    install -m "$mode" "$SCRIPT_DIR/$asset" "$control_staging/$asset"
  done
  install -m 0444 "$RELEASE_MANIFEST" \
    "$control_staging/${RELEASE_MANIFEST##*/}"
  chmod 0755 "$control_staging"
  mv "$control_staging" "$release_control_dir"
  sync -f "$release_dir"
fi
secure_release_directory "$release_control_dir" \
  || die 'candidate release control directory is not root-owned mode 0755'
for asset in "${BUNDLE_ASSETS[@]}"; do
  secure_control_file "$release_control_dir/$asset"
done
pinned_release_manifest="$release_control_dir/${RELEASE_MANIFEST##*/}"
verify_release_binding "$pinned_release_manifest" "$release_manifest_sha" \
  "$candidate_sha" "$source_revision" "$deployment_bundle_sha" \
  "$control_archive_sha" "$release_binary" "$release_control_dir" \
  || die 'candidate release controls differ from the verified release bundle'
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
  secure_root_chain "$path" || die "created shadow path is not trusted: $path"
done
install -d -m 0750 -o hftcollector -g hftcollector "$shadow_spool"
install -d -m 0750 -o hftcollector -g hftcollector "$market_shadow_spool"
secure_collector_directory "$shadow_spool" \
  || die 'reference shadow spool identity or permissions are unsafe'
secure_collector_directory "$market_shadow_spool" \
  || die 'market shadow spool identity or permissions are unsafe'

install -d -m 0755 /run/monday
secure_root_chain /run/monday || die 'runtime environment directory is not trusted'
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

install -m 0644 "$release_control_dir/${SERVICE_TEMPLATE##*/}" \
  /etc/systemd/system/polymarket-reference-collector-shadow@.service
systemctl daemon-reload

baseline_health_snapshot=null
if ! baseline_health_requires_continuous_freshness "$baseline_mode"; then
  baseline_health_snapshot=$(fresh_baseline_health_snapshot \
    "$LEGACY_SPOOL/health.json" \
    "$release_control_dir/${LEGACY_HEALTH_POLICY##*/}") \
    || die 'active legacy collector health is not fresh and fail-closed clean'
fi
started_at_unix=$(date -u +%s)
started_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
start_uptime=$SECONDS
systemctl start "$shadow_unit"
shadow_invocation_id=$(systemctl show --property=InvocationID --value "$shadow_unit")
[[ $shadow_invocation_id =~ ^[a-f0-9]{32}$ ]] \
  || die 'Rust shadow has no verifiable systemd invocation ID'
shadow_pid=$(systemctl show --property=MainPID --value "$shadow_unit")
[[ $shadow_pid =~ ^[1-9][0-9]*$ ]] || die 'Rust shadow has no initial MainPID'
initial_shadow_pid=$shadow_pid
shadow_memory_events=$(shadow_memory_events_file "$initial_shadow_pid") \
  || die 'Rust shadow has no trusted cgroup memory.events file'
memory_events_start=$(memory_events_snapshot "$shadow_memory_events") \
  || die 'Rust shadow memory.events baseline is invalid'
read -r memory_events_start_high memory_events_start_max memory_events_start_oom \
  memory_events_start_oom_kill memory_events_start_oom_group_kill \
  <<<"$memory_events_start"
[[ $memory_events_start_high == 0 && $memory_events_start_max == 0 \
  && $memory_events_start_oom == 0 \
  && $memory_events_start_oom_kill == 0 \
  && $memory_events_start_oom_group_kill == 0 ]] \
  || die 'Rust shadow reached MemoryHigh, MemoryMax, or OOM before the gate baseline'
memory_events_end=$memory_events_start

last_health=
last_health_change=$start_uptime
last_legacy_health=
last_legacy_health_change=$start_uptime
legacy_api_error_started_at=
legacy_health_state=fatal
legacy_health_decision=fatal
common_cutoff=
parity_window_started_at=
while :; do
  now_uptime=$SECONDS
  elapsed=$((now_uptime - start_uptime))
  verify_baseline_identity \
    || die 'baseline collector PID, restart count, or effective unit identity changed during gate'
  if baseline_health_requires_continuous_freshness "$baseline_mode"; then
    legacy_health="$LEGACY_SPOOL/health.json"
    [[ -f $legacy_health && ! -L $legacy_health ]] \
      || die "$baseline_label health is missing"
    legacy_health_state=$(legacy_health_sample_state \
      "$legacy_health" "$release_control_dir/${LEGACY_HEALTH_POLICY##*/}" \
      "$baseline_mode")
    legacy_health_result=$(legacy_health_transition \
      "$legacy_health_state" "$legacy_api_error_started_at" \
      "$now_uptime" "$MAX_HEALTH_SILENCE_SECONDS")
    legacy_health_decision=${legacy_health_result%%:*}
    legacy_api_error_started_at=${legacy_health_result#*:}
    case "$legacy_health_decision" in
      advance|wait)
        ;;
      expired)
        die "$baseline_label API errors did not recover within the health budget"
        ;;
      *)
        die "$baseline_label health is not fail-closed clean during shadow"
        ;;
    esac
    current_legacy_health=$(jq -r '.updated_at' "$legacy_health")
    if [[ $current_legacy_health != "$last_legacy_health" ]]; then
      last_legacy_health=$current_legacy_health
      last_legacy_health_change=$now_uptime
    fi
    ((now_uptime - last_legacy_health_change <= MAX_HEALTH_SILENCE_SECONDS)) \
      || die "$baseline_label health stopped advancing during shadow"
  else
    legacy_health_decision=advance
  fi
  shadow_pid=$(systemctl show --property=MainPID --value "$shadow_unit")
  [[ $shadow_pid =~ ^[1-9][0-9]*$ ]] || die 'Rust shadow has no MainPID'
  [[ $shadow_pid == "$initial_shadow_pid" ]] || die 'Rust shadow MainPID changed during gate'
  verify_shadow_identity "$initial_shadow_pid" "$shadow_invocation_id" \
    || die 'Rust shadow systemd identity, PID, or command line changed during gate'
  memory_events_end=$(stable_memory_events_snapshot \
    "$shadow_memory_events" "$memory_events_start_high") \
    || die 'Rust shadow memory.events high grew or a MemoryMax/OOM event occurred'
  # A short gate may reduce the observation window, but it still starts with an
  # empty per-run spool. Keep the same bounded wait for the first health record
  # so test mode cannot fail before the collector has one full poll cycle.
  if ((elapsed >= HEALTH_SETTLE_SECONDS)); then
    health="$shadow_spool/health.json"
    [[ -f $health && ! -L $health ]] || die 'Rust shadow health is missing'
    jq -e -f "$release_control_dir/${RUST_HEALTH_POLICY##*/}" "$health" >/dev/null \
      || die 'Rust shadow health is not fail-closed clean'
    current_health=$(jq -r '.updated_at' "$health")
    if [[ $current_health != "$last_health" ]]; then
      last_health=$current_health
      last_health_change=$now_uptime
    fi
    ((now_uptime - last_health_change <= MAX_HEALTH_SILENCE_SECONDS)) \
      || die 'Rust shadow health stopped advancing'

    rust_success_at=$(jq -er '.last_success_at | select(type == "string" and length > 0)' \
      "$health") || die 'Rust health has no last_success_at'
    rust_success_epoch=$(date -u -d "$rust_success_at" +%s) \
      || die 'Rust last_success_at is invalid'
    now_epoch=$(date -u +%s)
    ((rust_success_epoch <= now_epoch && now_epoch - rust_success_epoch <= MAX_HEALTH_SILENCE_SECONDS)) \
      || die 'Rust last_success_at is stale or from the future'
    if [[ $legacy_health_decision == advance ]]; then
      common_cutoff=$rust_success_epoch
      if baseline_health_requires_continuous_freshness "$baseline_mode"; then
        legacy_success_at=$(jq -er \
          '.last_success_at | select(type == "string" and length > 0)' \
          "$legacy_health") || die "$baseline_label health has no last_success_at"
        legacy_success_epoch=$(date -u -d "$legacy_success_at" +%s) \
          || die "$baseline_label last_success_at is invalid"
        ((legacy_success_epoch <= now_epoch \
          && now_epoch - legacy_success_epoch <= MAX_HEALTH_SILENCE_SECONDS)) \
          || die "$baseline_label last_success_at is stale or from the future"
        ((legacy_success_epoch < common_cutoff)) && common_cutoff=$legacy_success_epoch
      fi
      if [[ $test_only == false ]]; then
        common_cutoff=$((common_cutoff - PARITY_CUTOFF_LAG_SECONDS))
      fi
      parity_window_started_at=$(bounded_parity_window_start \
        "$started_at_unix" "$common_cutoff" "$test_only" \
        "$SETTLEMENT_EVENT_LOOKBACK_SECONDS") \
        || die 'could not derive a bounded parity window start'
    fi
  fi

  if ((elapsed >= gate_seconds)) && [[ -n $common_cutoff ]] \
    && [[ $legacy_health_decision == advance ]]; then
    if valid_parity_window "$parity_window_started_at" "$common_cutoff"; then
      if [[ $test_only == true ]] \
        || ((common_cutoff - parity_window_started_at >= PARITY_TAIL_SECONDS)); then
        break
      fi
    elif [[ $test_only == true ]]; then
      die 'short test gate could not derive an ordered parity window'
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
valid_parity_window "$parity_window_started_at" "$common_cutoff" \
  || die 'settlement-safe parity start is not before the common cutoff'
if [[ $test_only == false ]]; then
  ((observed_duration_seconds >= MINIMUM_GATE_SECONDS)) \
    || die 'production shadow duration is shorter than required'
  ((common_cutoff - parity_window_started_at >= PARITY_TAIL_SECONDS)) \
    || die 'production parity window is too short for the mature trade interval'
fi

shadow_pid=$(systemctl show --property=MainPID --value "$shadow_unit")
[[ $shadow_pid =~ ^[1-9][0-9]*$ ]] || die 'Rust shadow has no final MainPID'
shadow_restarts=$(systemctl show --property=NRestarts --value "$shadow_unit")
[[ $shadow_restarts == 0 && $shadow_pid == "$initial_shadow_pid" ]] \
  || die 'Rust shadow did not remain a single continuous process'
verify_shadow_identity "$initial_shadow_pid" "$shadow_invocation_id" \
  || die 'final Rust shadow systemd identity differs from the gated candidate'
verify_baseline_identity \
  || die 'baseline collector identity changed before parity evidence was captured'
shadow_exec_argv=$(effective_exec_argv "$shadow_unit") \
  || die 'could not capture the effective Rust shadow ExecStart'
shadow_cmdline=$(proc_cmdline "$initial_shadow_pid") \
  || die 'could not capture the exact Rust shadow command line'
shadow_cmdline_argv=${shadow_cmdline% }
shadow_fragment_path=$(systemctl show --property=FragmentPath --value "$shadow_unit")
shadow_drop_ins=$(systemctl show --property=DropInPaths --value "$shadow_unit")
shadow_drop_ins_json=$(jq -cn --arg value "$shadow_drop_ins" \
  '$value | split(" ") | map(select(length > 0))')
shadow_stop_cursor=$(journal_cursor "$shadow_unit") \
  || die 'could not capture the Rust shadow journal cursor before stop'
verify_shadow_identity "$initial_shadow_pid" "$shadow_invocation_id" \
  || die 'Rust shadow identity changed immediately before stop'
systemctl freeze "$shadow_unit" \
  || die 'could not freeze the Rust shadow before its final memory snapshot'
shadow_freezer_state=$(systemctl show --property=FreezerState --value "$shadow_unit")
[[ $shadow_freezer_state == frozen ]] \
  || die 'Rust shadow did not enter the frozen state before its final memory snapshot'
verify_shadow_identity "$initial_shadow_pid" "$shadow_invocation_id" \
  || die 'Rust shadow identity changed while entering the frozen state'
memory_events_end=$(stable_memory_events_snapshot \
  "$shadow_memory_events" "$memory_events_start_high") \
  || die 'Rust shadow memory.events changed immediately before stop'
memory_events_start_json=$(memory_events_json "$memory_events_start") \
  || die 'could not serialize the Rust shadow memory.events baseline'
memory_events_end_json=$(memory_events_json "$memory_events_end") \
  || die 'could not serialize the final Rust shadow memory.events snapshot'
systemctl kill --kill-whom=main --signal=SIGTERM "$shadow_unit" \
  || die 'could not terminate the frozen Rust shadow main process'
systemctl thaw "$shadow_unit" \
  || die 'could not thaw the Rust shadow before its final stop'
shadow_thawed_state=$(systemctl show --property=FreezerState --value "$shadow_unit")
[[ $shadow_thawed_state == running ]] \
  || die 'Rust shadow did not leave the frozen state before its final stop'
systemctl stop "$shadow_unit"
verify_no_restart_after_cursor "$shadow_unit" "$shadow_stop_cursor" "$shadow_invocation_id" \
  || die 'Rust shadow journal recorded a restart during final stop'
stopped_shadow_restarts=$(systemctl show --property=NRestarts --value "$shadow_unit")
[[ $stopped_shadow_restarts == 0 ]] \
  || die 'Rust shadow restarted between final verification and stop'

evidence_parent="$EVIDENCE_ROOT/$candidate_sha"
install -d -m 0755 /data/monday/evidence "$EVIDENCE_ROOT" "$evidence_parent"
for path in /data/monday/evidence "$EVIDENCE_ROOT" "$evidence_parent"; do
  secure_root_chain "$path" || die "evidence path is not trusted: $path"
done
evidence_dir="$evidence_parent/$run_id"
mkdir -m 0750 "$evidence_dir" || die 'evidence run already exists'
secure_root_chain "$evidence_dir" || die 'evidence run directory is not trusted'
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
{
  jq -cn '{sequence:0,recorded_at:"2000-01-01T00:00:00Z",update:{
    kind:"event_discovered",event_id:"shadow-market-upload",symbol:"BTCUSDT",
    up_token:"shadow-up",down_token:"shadow-down",end_time:"2000-01-01T00:05:00Z",
    window_secs:300,price_to_beat:"100",resolved_up_won:null}}'
  jq -cn '{sequence:1,recorded_at:"2000-01-01T00:00:01Z",update:{
    kind:"quote",token_id:"shadow-up",bid:"0.49",ask:"0.51",
    bid_size:"10",ask_size:"11",bid_levels:[{price:"0.49",size:"10"}],
    ask_levels:[{price:"0.51",size:"11"}],request_status:"success",
    collection_result:"executable",ts:"2000-01-01T00:00:01Z"}}'
  jq -cn '{sequence:2,recorded_at:"2000-01-01T00:00:01Z",update:{
    kind:"quote",token_id:"shadow-down",bid:"0.49",ask:"0.51",
    bid_size:"10",ask_size:"11",bid_levels:[{price:"0.49",size:"10"}],
    ask_levels:[{price:"0.51",size:"11"}],request_status:"success",
    collection_result:"executable",ts:"2000-01-01T00:00:01Z"}}'
  jq -cn '{sequence:3,recorded_at:"2000-01-01T00:00:02Z",update:{
    kind:"reference_price",symbol:"BTCUSDT",source:"binance",asset_class:"crypto",
    price:"100",full_accuracy_value:null,is_carried_forward:false,
    ts:"2000-01-01T00:00:02Z"}}'
} >"$market_fixture"
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

if [[ $baseline_mode == legacy_python ]]; then
  verify_legacy_identity "$legacy_pid" "$legacy_restarts" "$legacy_invocation_id"
else
  verify_baseline_identity
fi || die 'baseline collector identity changed while parity or OSS readback was running'
baseline_proc_exe=''
[[ $baseline_mode != rust_release ]] || baseline_proc_exe=$(readlink -f -- "/proc/$legacy_pid/exe") || die 'could not capture the production Rust executable identity'
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
completed_at=$(date -u +%Y-%m-%dT%H:%M:%SZ)
production_eligible=true
[[ $test_only == false ]] || production_eligible=false
gate_tmp="$evidence_dir/.gate.json.tmp"
gate_json="$evidence_dir/gate.json"
jq \
  --arg candidate_sha256 "$candidate_sha" \
  --arg deployment_bundle_sha256 "$deployment_bundle_sha" \
  --arg deployment_source_revision "$source_revision" \
  --arg release_manifest_sha256 "$release_manifest_sha" \
  --arg control_archive_sha256 "$control_archive_sha" \
  --arg oss_config_sha256 "$oss_config_sha" \
  --arg started_at "$started_at" \
  --arg completed_at "$completed_at" \
  --arg baseline_mode "$baseline_mode" \
  --arg legacy_exec "$legacy_exec_argv" \
  --arg legacy_cmdline "$legacy_cmdline_argv" \
  --arg legacy_cmdline_sha256 "$legacy_cmdline_sha" \
  --arg legacy_invocation_id "$legacy_invocation_id" \
  --arg legacy_fragment_path "$legacy_fragment_path" \
  --argjson legacy_drop_in_paths "$legacy_drop_ins_json" \
  --argjson legacy_pid "$legacy_pid" \
  --argjson legacy_restarts "$legacy_restarts" \
  --arg baseline_release_path "$baseline_release_path" \
  --arg baseline_release_sha256 "$baseline_release_sha" --arg baseline_proc_exe "$baseline_proc_exe" \
  --arg shadow_exec "$shadow_exec_argv" \
  --arg shadow_cmdline "$shadow_cmdline_argv" \
  --arg shadow_invocation_id "$shadow_invocation_id" \
  --arg shadow_fragment_path "$shadow_fragment_path" \
  --argjson shadow_drop_in_paths "$shadow_drop_ins_json" \
  --argjson shadow_pid "$shadow_pid" \
  --argjson shadow_restarts "$shadow_restarts" \
  --argjson memory_events_start "$memory_events_start_json" \
  --argjson memory_events_end "$memory_events_end_json" \
  --arg shadow_run_id "$run_id" \
  --argjson duration_seconds "$observed_duration_seconds" \
  --argjson parity_window_started_at_unix "$parity_window_started_at" \
  --argjson parity_window_ended_at_unix "$common_cutoff" \
  --argjson production_eligible "$production_eligible" \
  --argjson baseline_health_snapshot "$baseline_health_snapshot" \
  --argjson uploaded_segments "$uploaded_segments" \
  --argjson canonical_uploaded_segments "$canonical_uploaded_segments" \
  --argjson market_uploaded_segments "$market_uploaded_segments" \
  --argjson market_canonical_uploaded_segments "$market_canonical_uploaded_segments" \
  '. + {
    schema:"monday.polymarket_shadow_gate.v1",
    baseline_mode:$baseline_mode,
    candidate_sha256:$candidate_sha256,
    deployment_bundle_sha256:$deployment_bundle_sha256,
    deployment_source_revision:$deployment_source_revision,
    release_manifest_sha256:$release_manifest_sha256,
    control_archive_sha256:$control_archive_sha256,
    oss_config_sha256:$oss_config_sha256,
    started_at:$started_at,
    completed_at:$completed_at,
    shadow_run_id:$shadow_run_id,
    duration_seconds:$duration_seconds,
    parity_window_started_at_unix:$parity_window_started_at_unix,
    parity_window_ended_at_unix:$parity_window_ended_at_unix,
    production_eligible:$production_eligible,
    baseline_health_snapshot:$baseline_health_snapshot,
    legacy_runtime:({exec_start:$legacy_exec,cmdline:$legacy_cmdline,
        cmdline_sha256:$legacy_cmdline_sha256,
        fragment_path:$legacy_fragment_path,drop_in_paths:$legacy_drop_in_paths,
        main_pid:$legacy_pid,restarts:$legacy_restarts,
        invocation_id:$legacy_invocation_id}
      + if $baseline_mode == "rust_release" then
          {release_path:$baseline_release_path,proc_exe:$baseline_proc_exe,
            release_sha256:$baseline_release_sha256} else {} end),
    shadow_runtime:{exec_start:$shadow_exec,cmdline:$shadow_cmdline,
      fragment_path:$shadow_fragment_path,drop_in_paths:$shadow_drop_in_paths,
      main_pid:$shadow_pid,restarts:$shadow_restarts,
      invocation_id:$shadow_invocation_id,
      memory_events:{start:$memory_events_start,end:$memory_events_end}},
    checks:(.checks + {
      health_freshness:true,
      candidate_identity:true,
      memory_events_stable:true,
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
  secure_root_chain "$evidence_dir" \
    || die 'evidence directory trust changed before marker publication'
  verify_release_binding "$pinned_release_manifest" "$release_manifest_sha" \
    "$candidate_sha" "$source_revision" "$deployment_bundle_sha" \
    "$control_archive_sha" "$release_binary" "$release_control_dir" \
    || die 'release manifest, candidate, or installed control bundle changed during gate'
  verify_baseline_identity \
    || die 'baseline collector identity changed before the gate marker was published'
  [[ $baseline_mode != rust_release ]] || verify_fresh_baseline_health "$LEGACY_SPOOL/health.json" "$release_control_dir/${LEGACY_HEALTH_POLICY##*/}" \
    || die 'active Rust collector health became stale before marker publication'
  verify_current_oss_config
  jq -e -f "$release_control_dir/${GATE_POLICY##*/}" "$gate_json" >/dev/null \
    || die 'combined gate evidence failed the production policy'
  marker="$evidence_dir/PASSED.sha256"
  (
    cd "$evidence_dir"
    sha256sum gate.json >".${marker##*/}.tmp"
    mv ".${marker##*/}.tmp" "${marker##*/}"
  )
  sync "$marker"
  sync -f "$evidence_dir"
fi

printf '%s\n' "$gate_json"
