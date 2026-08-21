#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf 'Usage: %s <current-binary-sha256> <candidate-binary-sha256>\n' "${0##*/}" >&2
}

configure_paths() {
  local root=${1%/}
  RELEASE_ROOT="$root/opt/monday/releases/binance-lob-archiver"
  PRODUCTION_BINARY="$root/opt/monday/bin/binance-lob-archiver"
  SYSTEMD_ROOT="$root/etc/systemd/system"
  CONFIG_ROOT="$root/etc/monday"
  DATA_ROOT="$root/data"
  GATE_ROOT="$root/data/monday/evidence/shadow-gates"
  EVIDENCE_ROOT="$root/data/monday/evidence/release-adoptions"
  PROC_ROOT="$root/proc"
  LOCK_ROOT="$root/run/lock"
  PRODUCTION_SERVICE="$SYSTEMD_ROOT/binance-lob-archiver-production@.service"
  UPLOAD_SERVICE="$SYSTEMD_ROOT/binance-lob-archiver-upload@.service"
  SPOT_ENV="$CONFIG_ROOT/binance-lob-archiver-production-spot.env"
  USDM_ENV="$CONFIG_ROOT/binance-lob-archiver-production-usdm.env"
}

die() {
  printf '%s\n' "$*" >&2
  exit 1
}

direct_directory() {
  local path=$1 resolved
  [[ -d $path && ! -L $path ]] || return 1
  resolved=$(readlink -f -- "$path") || return 1
  [[ $resolved == "$path" ]]
}

direct_directory_or_absent() {
  local path=$1
  [[ -e $path || -L $path ]] || return 0
  direct_directory "$path"
}

secure_direct_directory() {
  local path=$1 owner mode
  direct_directory "$path" || return 1
  owner=$(stat -c %u -- "$path") || return 1
  mode=$(stat -c %a -- "$path") || return 1
  [[ $owner == "$EXPECTED_ROOT_UID" ]] || return 1
  (( (8#$mode & 022) == 0 ))
}

secure_regular_file() {
  local path=$1 owner mode
  [[ -f $path && ! -L $path ]] || return 1
  owner=$(stat -c %u -- "$path") || return 1
  mode=$(stat -c %a -- "$path") || return 1
  [[ $owner == "$EXPECTED_ROOT_UID" ]] || return 1
  (( (8#$mode & 022) == 0 ))
}

require_secure_file() {
  secure_regular_file "$1" || die "required file is missing, indirect, writable, or not root-owned: $1"
}

require_line() {
  grep -Fxq -- "$2" "$1" || die "$1 is missing required setting: $2"
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
    || die "$file must contain exactly one $key setting"
  [[ $actual == "$expected" ]] \
    || die "$file has unsafe $key=$actual (expected $expected)"
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

validate_production_assets() {
  local file market dataset spool symbols
  require_secure_file "$PRODUCTION_SERVICE"
  require_line "$PRODUCTION_SERVICE" 'AssertPathIsMountPoint=/data'
  require_line "$PRODUCTION_SERVICE" \
    'EnvironmentFile=/etc/monday/binance-lob-archiver-production-%i.env'
  require_line "$PRODUCTION_SERVICE" \
    'ExecStart=/opt/monday/bin/binance-lob-archiver'
  for market in spot usdm; do
    if [[ $market == spot ]]; then
      file=$SPOT_ENV
      dataset=spot_all
    else
      file=$USDM_ENV
      dataset=$(env_value "$file" DATASET) \
        || die "$file has no DATASET"
      [[ $dataset == usdm_perpetual_all || $dataset == usdm_perpetual_top100_lob ]] \
        || die "$file has an unsupported USD-M DATASET=$dataset"
    fi
    spool="/data/monday/spool/binance-lob/$market"
    require_secure_file "$file"
    require_env_value "$file" MARKET "$market"
    require_env_value "$file" DATASET "$dataset"
    require_env_value "$file" SHARD_ID all
    symbols=$(env_value "$file" SYMBOLS) \
      || die "$file must contain exactly one SYMBOLS setting"
    if [[ $market == spot ]]; then
      [[ $symbols == ALL ]] || die "$file must set SYMBOLS=ALL"
    elif [[ $symbols != ALL ]]; then
      is_usdm_top100 "$symbols" \
        || die "$file must set SYMBOLS=ALL or 100 unique explicit symbols"
    fi
    require_env_value "$file" DEPTH_MODE diff
    require_env_value "$file" SEGMENT_SECONDS 3600
    require_env_value "$file" SPOOL_DIR "$spool"
    require_env_value "$file" OSS_BUCKET monday-lob-apne1-1045353359
    require_env_value "$file" OSS_ENDPOINT oss-ap-northeast-1-internal.aliyuncs.com
    require_env_value "$file" OSS_REGION ap-northeast-1
    require_env_value "$file" ALIYUN_PROFILE ecs-role
  done
}

unit_not_enabled() {
  local state
  state=$(systemctl is-enabled "$1" 2>/dev/null || true)
  case "$state" in
    disabled|static|indirect|not-found) return 0 ;;
    *) return 1 ;;
  esac
}

verify_upload_quiescent() {
  local unit
  for unit in \
    binance-lob-archiver-upload@spot.service \
    binance-lob-archiver-upload@usdm.service; do
    ! systemctl is-active --quiet "$unit" \
      || die "synthetic rollback upload unit became active: $unit"
    unit_not_enabled "$unit" \
      || die "synthetic rollback upload unit became enabled: $unit"
  done
}

capture_unit_state() {
  local unit=$1 pid restarts exe_sha
  systemctl is-active --quiet "$unit" || return 1
  systemctl is-enabled --quiet "$unit" || return 1
  pid=$(systemctl show "$unit" --property=MainPID --value) || return 1
  restarts=$(systemctl show "$unit" --property=NRestarts --value) || return 1
  [[ $pid =~ ^[1-9][0-9]*$ && $restarts =~ ^[0-9]+$ ]] || return 1
  [[ -e $PROC_ROOT/$pid/exe || -L $PROC_ROOT/$pid/exe ]] || return 1
  exe_sha=$(sha256sum "$PROC_ROOT/$pid/exe" | awk '{print $1}') || return 1
  [[ $exe_sha == "$CURRENT_SHA256" ]] || return 1
  jq -cn \
    --arg unit "$unit" \
    --argjson main_pid "$pid" \
    --argjson n_restarts "$restarts" \
    --arg exe_sha256 "$exe_sha" \
    '{unit:$unit,active:true,enabled:true,main_pid:$main_pid,
      n_restarts:$n_restarts,exe_sha256:$exe_sha256}'
}

capture_unit_states() {
  local spot usdm
  spot=$(capture_unit_state binance-lob-archiver-production@spot.service) \
    || return 1
  usdm=$(capture_unit_state binance-lob-archiver-production@usdm.service) \
    || return 1
  jq -cn --argjson spot "$spot" --argjson usdm "$usdm" \
    '{spot:$spot,usdm:$usdm}'
}

capture_health_state() {
  local market=$1 expected_dataset minimum_symbols file state updated_at_ns now_ns
  case "$market" in
    spot) expected_dataset=spot_all; minimum_symbols=1000 ;;
    usdm)
      expected_dataset=$(env_value "$USDM_ENV" DATASET) || return 1
      [[ $expected_dataset == usdm_perpetual_all \
        || $expected_dataset == usdm_perpetual_top100_lob ]] || return 1
      if [[ $(env_value "$USDM_ENV" SYMBOLS) == ALL ]]; then
        minimum_symbols=400
      else
        minimum_symbols=100
      fi
      ;;
    *) return 1 ;;
  esac
  file="$DATA_ROOT/monday/spool/binance-lob/$market/health.json"
  [[ -f $file && ! -L $file ]] || return 1
  state=$(jq -ce \
    --arg market "$market" \
    --arg dataset "$expected_dataset" \
    --argjson minimum_symbols "$minimum_symbols" '
      select(.market == $market and .dataset == $dataset)
      | select(.status == "synced")
      | select(.updated_at_ns | type == "number" and floor == . and . > 0)
      | select(.session_id | type == "string" and length > 0)
      | select(.symbol_count | type == "number" and floor == .
          and (if $market == "usdm" and $minimum_symbols == 100
            then . == 100 else . >= $minimum_symbols end))
      | select(.snapshot_ready_count == .symbol_count)
      | select(.sequence_gaps | type == "number" and floor == . and . >= 0)
      | select(.pending_upload_segments == 0)
      | select(.queue_saturated == false)
      | select(.disk_warning == false)
      | select(.upload_warning == false)
      | {market,dataset,status,session_id,updated_at_ns,sequence_gaps,
         symbol_count,snapshot_ready_count,pending_upload_segments,
         queue_saturated,disk_warning,upload_warning}' "$file") || return 1
  updated_at_ns=$(jq -er '.updated_at_ns' <<<"$state") || return 1
  now_ns=$(($(date +%s) * 1000000000))
  (( updated_at_ns <= now_ns + 5000000000 )) || return 1
  (( now_ns - updated_at_ns <= 90000000000 )) || return 1
  printf '%s\n' "$state"
}

capture_health_states() {
  local spot usdm
  spot=$(capture_health_state spot) || return 1
  usdm=$(capture_health_state usdm) || return 1
  jq -cn --argjson spot "$spot" --argjson usdm "$usdm" \
    '{spot:$spot,usdm:$usdm}'
}

verify_health_continuity() {
  local before=$1 after=$2 market before_static after_static before_ns after_ns
  for market in spot usdm; do
    before_static=$(jq -cS --arg market "$market" '.[$market] | del(.updated_at_ns)' \
      <<<"$before") || return 1
    after_static=$(jq -cS --arg market "$market" '.[$market] | del(.updated_at_ns)' \
      <<<"$after") || return 1
    [[ $before_static == "$after_static" ]] || return 1
    before_ns=$(jq -er --arg market "$market" '.[$market].updated_at_ns' <<<"$before") \
      || return 1
    after_ns=$(jq -er --arg market "$market" '.[$market].updated_at_ns' <<<"$after") \
      || return 1
    (( after_ns >= before_ns )) || return 1
  done
}

atomic_install() {
  local mode=$1 source=$2 destination=$3 temporary
  temporary="${destination}.new.$$"
  install -m "$mode" "$source" "$temporary" \
    || { rm -f -- "$temporary"; return 1; }
  mv -Tf "$temporary" "$destination" \
    || { rm -f -- "$temporary"; return 1; }
}

verify_candidate_release() {
  local markers marker marker_entry gate_dir gate_json gate_policy candidate_usdm_env
  CANDIDATE_RELEASE="$RELEASE_ROOT/$CANDIDATE_SHA256"
  CANDIDATE_BINARY="$CANDIDATE_RELEASE/binance-lob-archiver"
  CANDIDATE_DEPLOYMENT="$CANDIDATE_RELEASE/deployment"
  CANDIDATE_METADATA="$CANDIDATE_RELEASE/release.json"
  CANDIDATE_UPLOAD="$CANDIDATE_DEPLOYMENT/binance-lob-archiver-upload@.service"
  candidate_usdm_env="$CANDIDATE_DEPLOYMENT/binance-lob-archiver-production-usdm.env"
  gate_policy="$CANDIDATE_DEPLOYMENT/rust-lob-shadow-gate-policy.jq"
  for path in "$CANDIDATE_RELEASE" "$CANDIDATE_DEPLOYMENT"; do
    secure_direct_directory "$path" \
      || die "candidate release path is indirect, writable, or not root-owned: $path"
  done
  require_secure_file "$CANDIDATE_BINARY"
  [[ -x $CANDIDATE_BINARY ]] || die 'candidate binary is not executable'
  printf '%s  %s\n' "$CANDIDATE_SHA256" "$CANDIDATE_BINARY" \
    | sha256sum --check --strict >/dev/null \
    || die 'candidate binary digest does not match its release path'
  require_secure_file "$CANDIDATE_METADATA"
  require_secure_file "$CANDIDATE_UPLOAD"
  require_secure_file "$candidate_usdm_env"
  require_secure_file "$gate_policy"
  require_env_value "$candidate_usdm_env" DATASET usdm_perpetual_top100_lob
  require_line "$CANDIDATE_UPLOAD" 'AssertPathIsMountPoint=/data'
  require_line "$CANDIDATE_UPLOAD" \
    'ExecStart=/opt/monday/bin/binance-lob-archiver --upload-only'
  if grep -Eq '^(WantedBy|RequiredBy)=' "$CANDIDATE_UPLOAD"; then
    die 'candidate upload unit must not be installable/enabled'
  fi
  DEPLOYMENT_BUNDLE_SHA256=$(jq -er '.deployment_bundle_sha256' "$CANDIDATE_METADATA") \
    || die 'candidate release is missing its deployment bundle digest'
  DEPLOYMENT_SOURCE_REVISION=$(jq -er '.deployment_source_revision' "$CANDIDATE_METADATA") \
    || die 'candidate release is missing its deployment source revision'
  [[ $DEPLOYMENT_BUNDLE_SHA256 =~ ^[a-f0-9]{64}$ ]] \
    || die 'candidate deployment bundle digest is invalid'
  [[ $DEPLOYMENT_SOURCE_REVISION =~ ^[a-f0-9]{40,64}$ ]] \
    || die 'candidate deployment source revision is invalid'
  jq -e --arg sha "$CANDIDATE_SHA256" --arg bundle "$DEPLOYMENT_BUNDLE_SHA256" \
    '.artifact_sha256 == $sha and .deployment_bundle_sha256 == $bundle' \
    "$CANDIDATE_METADATA" >/dev/null \
    || die 'candidate release identity does not match its metadata'

  GATE_BUNDLE_DIR="$GATE_ROOT/$CANDIDATE_SHA256/$DEPLOYMENT_BUNDLE_SHA256"
  for path in "$GATE_ROOT" "$GATE_ROOT/$CANDIDATE_SHA256" "$GATE_BUNDLE_DIR" \
    "$GATE_BUNDLE_DIR/runs"; do
    secure_direct_directory "$path" \
      || die "candidate gate path is indirect, writable, or not root-owned: $path"
  done
  shopt -s nullglob
  markers=("$GATE_BUNDLE_DIR"/runs/*/PASSED.sha256)
  shopt -u nullglob
  (( ${#markers[@]} == 1 )) \
    || die "expected exactly one verified candidate gate, found ${#markers[@]}"
  marker=${markers[0]}
  gate_dir=${marker%/*}
  gate_json="$gate_dir/gate.json"
  secure_direct_directory "$gate_dir" \
    || die "candidate gate run path is indirect, writable, or not root-owned: $gate_dir"
  require_secure_file "$marker"
  require_secure_file "$gate_json"
  [[ $(wc -l <"$marker") -eq 1 ]] \
    || die 'candidate gate marker must contain exactly one entry'
  marker_entry=$(<"$marker")
  [[ $marker_entry =~ ^[A-Fa-f0-9]{64}[[:space:]]+gate\.json$ ]] \
    || die 'candidate gate marker must bind only gate.json'
  (cd "$gate_dir" && sha256sum --check --strict PASSED.sha256 >/dev/null) \
    || die 'candidate gate marker does not verify'
  jq -e \
    --arg candidate_sha256 "$CANDIDATE_SHA256" \
    --arg deployment_bundle_sha256 "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg deployment_source_revision "$DEPLOYMENT_SOURCE_REVISION" \
    -f "$gate_policy" "$gate_json" >/dev/null \
    || die 'candidate gate is not production eligible'
  [[ $(jq -er '.markets.usdm.symbols_config' "$gate_json") \
    == "$(env_value "$candidate_usdm_env" SYMBOLS)" ]] \
    || die 'candidate gate USD-M symbols differ from the candidate production scope'
}

validate_existing_adoption() {
  local target adopted_marker_entry
  [[ -L $PRODUCTION_BINARY ]] || die 'existing adoption is missing the production symlink'
  target=$(readlink -f -- "$PRODUCTION_BINARY") || die 'production symlink is dangling'
  [[ $target == "$RELEASE_BINARY" ]] || die "production symlink drifted to $target"
  require_secure_file "$RELEASE_BINARY"
  [[ -x $RELEASE_BINARY ]] || die 'adopted release binary is not executable'
  printf '%s  %s\n' "$CURRENT_SHA256" "$RELEASE_BINARY" \
    | sha256sum --check --strict >/dev/null || die 'adopted release binary digest drifted'
  secure_direct_directory "$RELEASE_DIR" \
    || die 'adopted release directory is indirect, writable, or not root-owned'
  secure_direct_directory "$RELEASE_DEPLOYMENT" \
    || die 'adopted deployment directory is indirect, writable, or not root-owned'
  require_secure_file "$RELEASE_DIR/adopted-release.json"
  require_secure_file "$RELEASE_DIR/deployment.sha256"
  (cd "$RELEASE_DIR" && sha256sum --check --strict deployment.sha256 >/dev/null) \
    || die 'adopted release deployment manifest does not verify'
  jq -e \
    --arg current "$CURRENT_SHA256" \
    --arg candidate "$CANDIDATE_SHA256" \
    --arg bundle "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg source "$DEPLOYMENT_SOURCE_REVISION" \
    '.schema == "monday.rust_lob_adopted_release.v1"
      and .artifact_sha256 == $current
      and .compatibility_candidate_binary_sha256 == $candidate
      and .compatibility_deployment_bundle_sha256 == $bundle
      and .compatibility_deployment_source_revision == $source
      and .legacy_binary_supports_upload_only == false' \
    "$RELEASE_DIR/adopted-release.json" >/dev/null \
    || die 'adopted release metadata identity drifted'
  for asset in \
    binance-lob-archiver-production@.service \
    binance-lob-archiver-upload@.service \
    binance-lob-archiver-production-spot.env \
    binance-lob-archiver-production-usdm.env; do
    require_secure_file "$RELEASE_DEPLOYMENT/$asset"
  done
  cmp -s "$RELEASE_DEPLOYMENT/binance-lob-archiver-production@.service" \
    "$PRODUCTION_SERVICE" || die 'installed production unit drifted from adopted release'
  cmp -s "$RELEASE_DEPLOYMENT/binance-lob-archiver-upload@.service" \
    "$UPLOAD_SERVICE" || die 'installed upload unit drifted from adopted release'
  cmp -s "$RELEASE_DEPLOYMENT/binance-lob-archiver-production-spot.env" \
    "$SPOT_ENV" || die 'installed Spot env drifted from adopted release'
  cmp -s "$RELEASE_DEPLOYMENT/binance-lob-archiver-production-usdm.env" \
    "$USDM_ENV" || die 'installed USD-M env drifted from adopted release'
  secure_direct_directory "$EVIDENCE_DIR" \
    || die 'adoption evidence directory is indirect, writable, or not root-owned'
  require_secure_file "$EVIDENCE_DIR/MANIFEST.sha256"
  require_secure_file "$EVIDENCE_DIR/ADOPTED.sha256"
  require_secure_file "$EVIDENCE_DIR/adoption.json"
  [[ $(wc -l <"$EVIDENCE_DIR/ADOPTED.sha256") -eq 1 ]] \
    || die 'adoption marker must contain exactly one entry'
  adopted_marker_entry=$(<"$EVIDENCE_DIR/ADOPTED.sha256")
  [[ $adopted_marker_entry =~ ^[A-Fa-f0-9]{64}[[:space:]]+MANIFEST\.sha256$ ]] \
    || die 'adoption marker must bind only MANIFEST.sha256'
  (cd "$EVIDENCE_DIR" \
    && sha256sum --check --strict ADOPTED.sha256 >/dev/null \
    && sha256sum --check --strict MANIFEST.sha256 >/dev/null) \
    || die 'immutable adoption evidence does not verify'
  jq -e \
    --arg current "$CURRENT_SHA256" \
    --arg candidate "$CANDIDATE_SHA256" \
    --arg upload_source "$CANDIDATE_UPLOAD" \
    '.schema == "monday.rust_lob_release_adoption.v1"
      and .result == "passed"
      and .current_binary_sha256 == $current
      and .candidate_binary_sha256 == $candidate
      and .synthetic_upload_unit.source == $upload_source
      and .synthetic_upload_unit.originally_present == false
      and .synthetic_upload_unit.enabled_or_started == false
      and .legacy_binary_supports_upload_only == false' \
    "$EVIDENCE_DIR/adoption.json" >/dev/null \
    || die 'adoption evidence identity or safety claims do not match'
  verify_upload_quiescent
  capture_unit_states >/dev/null || die 'production unit identity drifted after adoption'
  capture_health_states >/dev/null || die 'production health is stale after adoption'
}

write_adoption_evidence() {
  local adoption_json upload_sha
  upload_sha=$(sha256sum "$CANDIDATE_UPLOAD" | awk '{print $1}') || return 1
  adoption_json="$EVIDENCE_STAGING/adoption.json"
  jq -n \
    --arg started_at "$STARTED_AT" \
    --arg completed_at "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
    --arg current_sha "$CURRENT_SHA256" \
    --arg candidate_sha "$CANDIDATE_SHA256" \
    --arg release_binary "$RELEASE_BINARY" \
    --arg upload_source "$CANDIDATE_UPLOAD" \
    --arg upload_sha "$upload_sha" \
    --arg bundle_sha "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg source_revision "$DEPLOYMENT_SOURCE_REVISION" \
    --argjson before_units "$BEFORE_UNITS" \
    --argjson after_units "$AFTER_UNITS" \
    --argjson before_health "$BEFORE_HEALTH" \
    --argjson after_health "$AFTER_HEALTH" \
    '{schema:"monday.rust_lob_release_adoption.v1",result:"passed",
      started_at:$started_at,completed_at:$completed_at,
      current_binary_sha256:$current_sha,candidate_binary_sha256:$candidate_sha,
      adopted_release_binary:$release_binary,
      candidate_deployment_bundle_sha256:$bundle_sha,
      candidate_deployment_source_revision:$source_revision,
      legacy_binary_supports_upload_only:false,
      synthetic_upload_unit:{source:$upload_source,sha256:$upload_sha,
        purpose:"rollback-compatibility-only",originally_present:false,
        enabled_or_started:false},
      production_units:{before:$before_units,after:$after_units},
      production_health:{before:$before_health,after:$after_health}}' \
    >"$adoption_json" || return 1
  chmod 0440 "$adoption_json" || return 1
  printf '%s\n' "$BEFORE_UNITS" >"$EVIDENCE_STAGING/before-units.json" || return 1
  printf '%s\n' "$AFTER_UNITS" >"$EVIDENCE_STAGING/after-units.json" || return 1
  printf '%s\n' "$BEFORE_HEALTH" >"$EVIDENCE_STAGING/before-health.json" || return 1
  printf '%s\n' "$AFTER_HEALTH" >"$EVIDENCE_STAGING/after-health.json" || return 1
  chmod 0440 "$EVIDENCE_STAGING"/{before,after}-{units,health}.json || return 1
  (
    cd "$EVIDENCE_STAGING"
    sha256sum \
      adoption.json \
      before-units.json after-units.json \
      before-health.json after-health.json \
      deployment/binance-lob-archiver-production@.service \
      deployment/binance-lob-archiver-upload@.service \
      deployment/binance-lob-archiver-production-spot.env \
      deployment/binance-lob-archiver-production-usdm.env \
      candidate-release.json adopted-release.json \
      release-deployment.sha256 >MANIFEST.sha256
    printf '%s  MANIFEST.sha256\n' "$(sha256sum MANIFEST.sha256 | awk '{print $1}')" \
      >ADOPTED.sha256
  ) || return 1
  chmod 0440 "$EVIDENCE_STAGING/MANIFEST.sha256" "$EVIDENCE_STAGING/ADOPTED.sha256" \
    || return 1
}

rollback_on_exit() {
  local rc=$? rollback_failed=0 temporary
  trap - EXIT
  set +e
  if (( SUCCESS == 0 )); then
    if (( LINK_REPLACED )); then
      temporary="${PRODUCTION_BINARY}.restore.$$"
      install -m "$ORIGINAL_MODE" "$RELEASE_BINARY" "$temporary" \
        && mv -Tf "$temporary" "$PRODUCTION_BINARY" \
        || rollback_failed=1
      rm -f -- "$temporary"
      sync -f "${PRODUCTION_BINARY%/*}" || rollback_failed=1
    fi
    if (( UPLOAD_INSTALLED )); then
      rm -f -- "$UPLOAD_SERVICE" || rollback_failed=1
      systemctl daemon-reload >/dev/null 2>&1 || rollback_failed=1
      sync -f "$SYSTEMD_ROOT" || rollback_failed=1
    fi
    (( RELEASE_CREATED == 0 )) || rm -rf -- "$RELEASE_DIR" || rollback_failed=1
    [[ -z ${RELEASE_STAGING:-} ]] || rm -rf -- "$RELEASE_STAGING"
    [[ -z ${EVIDENCE_STAGING:-} ]] || rm -rf -- "$EVIDENCE_STAGING"
    [[ -z ${LINK_TMP:-} ]] || rm -f -- "$LINK_TMP"
    (( EVIDENCE_CREATED == 0 )) || rm -rf -- "$EVIDENCE_DIR"
    if (( LINK_REPLACED )); then
      [[ -f $PRODUCTION_BINARY && ! -L $PRODUCTION_BINARY ]] || rollback_failed=1
      printf '%s  %s\n' "$CURRENT_SHA256" "$PRODUCTION_BINARY" \
        | sha256sum --check --strict >/dev/null || rollback_failed=1
    fi
    if (( rollback_failed )); then
      printf 'release adoption rollback could not fully restore the host\n' >&2
      rc=1
    fi
  fi
  exit "$rc"
}

adopt_release() (
  set -Eeuo pipefail
  CURRENT_SHA256=$1
  CANDIDATE_SHA256=$2
  STARTED_AT=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  RELEASE_DIR="$RELEASE_ROOT/$CURRENT_SHA256"
  RELEASE_BINARY="$RELEASE_DIR/binance-lob-archiver"
  RELEASE_DEPLOYMENT="$RELEASE_DIR/deployment"
  EVIDENCE_DIR="$EVIDENCE_ROOT/$CURRENT_SHA256"
  RELEASE_STAGING=
  EVIDENCE_STAGING=
  SUCCESS=0
  LINK_REPLACED=0
  UPLOAD_INSTALLED=0
  RELEASE_CREATED=0
  EVIDENCE_CREATED=0
  LINK_TMP=
  trap rollback_on_exit EXIT

  for path in \
    "${RELEASE_ROOT%/binance-lob-archiver}" \
    "$RELEASE_ROOT" \
    "${PRODUCTION_BINARY%/*}" \
    "$SYSTEMD_ROOT" \
    "$CONFIG_ROOT" \
    "$DATA_ROOT" \
    "$DATA_ROOT/monday" \
    "$DATA_ROOT/monday/spool" \
    "$DATA_ROOT/monday/spool/binance-lob" \
    "$DATA_ROOT/monday/spool/binance-lob/spot" \
    "$DATA_ROOT/monday/spool/binance-lob/usdm" \
    "$DATA_ROOT/monday/evidence" \
    "$EVIDENCE_ROOT"; do
    direct_directory_or_absent "$path" || die "managed path is indirect or a symlink: $path"
  done
  install -d -m 0750 "$DATA_ROOT/monday/evidence" "$EVIDENCE_ROOT"
  for path in \
    "${RELEASE_ROOT%/binance-lob-archiver}" \
    "$RELEASE_ROOT" \
    "${PRODUCTION_BINARY%/*}" \
    "$SYSTEMD_ROOT" \
    "$CONFIG_ROOT" \
    "$DATA_ROOT" \
    "$DATA_ROOT/monday" \
    "$DATA_ROOT/monday/evidence" \
    "$EVIDENCE_ROOT"; do
    secure_direct_directory "$path" \
      || die "managed path is writable or not root-owned: $path"
  done
  verify_candidate_release
  validate_production_assets

  if [[ -L $PRODUCTION_BINARY ]]; then
    validate_existing_adoption
    SUCCESS=1
    printf 'production binary already adopted as release %s\n' "$CURRENT_SHA256"
    exit 0
  fi
  require_secure_file "$PRODUCTION_BINARY"
  [[ -x $PRODUCTION_BINARY ]] || die 'current production binary is not executable'
  printf '%s  %s\n' "$CURRENT_SHA256" "$PRODUCTION_BINARY" \
    | sha256sum --check --strict >/dev/null \
    || die 'current production binary digest does not match the pinned digest'
  ORIGINAL_MODE=$(stat -c %a -- "$PRODUCTION_BINARY")
  [[ $ORIGINAL_MODE == 755 ]] || die 'current production binary mode must be 0755'
  legacy_help=$("$PRODUCTION_BINARY" --help 2>&1) \
    || die 'could not verify legacy binary upload-only capability'
  if grep -Fq -- '--upload-only' <<<"$legacy_help"; then
    die 'current production binary already supports --upload-only; adoption is not applicable'
  fi
  [[ ! -e $RELEASE_DIR && ! -L $RELEASE_DIR ]] \
    || die 'refusing partial pre-existing adopted release'
  [[ ! -e $EVIDENCE_DIR && ! -L $EVIDENCE_DIR ]] \
    || die 'refusing partial pre-existing adoption evidence'
  [[ ! -e $UPLOAD_SERVICE && ! -L $UPLOAD_SERVICE ]] \
    || die 'rollback upload unit was expected to be absent before adoption'
  verify_upload_quiescent

  BEFORE_UNITS=$(capture_unit_states) || die 'production units are not active, enabled, or identity-stable'
  BEFORE_HEALTH=$(capture_health_states) || die 'production health is stale or invalid before adoption'

  RELEASE_STAGING=$(mktemp -d "$RELEASE_ROOT/.${CURRENT_SHA256}.adopt.XXXXXX")
  install -d -m 0755 "$RELEASE_STAGING/deployment"
  install -m 0755 "$PRODUCTION_BINARY" "$RELEASE_STAGING/binance-lob-archiver"
  install -m 0644 "$PRODUCTION_SERVICE" \
    "$RELEASE_STAGING/deployment/binance-lob-archiver-production@.service"
  install -m 0644 "$CANDIDATE_UPLOAD" \
    "$RELEASE_STAGING/deployment/binance-lob-archiver-upload@.service"
  install -m 0640 "$SPOT_ENV" \
    "$RELEASE_STAGING/deployment/binance-lob-archiver-production-spot.env"
  install -m 0640 "$USDM_ENV" \
    "$RELEASE_STAGING/deployment/binance-lob-archiver-production-usdm.env"
  jq -n \
    --arg sha "$CURRENT_SHA256" \
    --arg candidate "$CANDIDATE_SHA256" \
    --arg bundle "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg source "$DEPLOYMENT_SOURCE_REVISION" \
    --arg upload "$CANDIDATE_UPLOAD" \
    '{schema:"monday.rust_lob_adopted_release.v1",artifact_sha256:$sha,
      origin:"running-host-regular-binary",
      compatibility_candidate_binary_sha256:$candidate,
      compatibility_deployment_bundle_sha256:$bundle,
      compatibility_deployment_source_revision:$source,
      synthetic_upload_unit_source:$upload,legacy_binary_supports_upload_only:false}' \
    >"$RELEASE_STAGING/adopted-release.json"
  chmod 0444 "$RELEASE_STAGING/adopted-release.json"
  (
    cd "$RELEASE_STAGING"
    sha256sum \
      deployment/binance-lob-archiver-production@.service \
      deployment/binance-lob-archiver-upload@.service \
      deployment/binance-lob-archiver-production-spot.env \
      deployment/binance-lob-archiver-production-usdm.env >deployment.sha256
  )
  chmod 0444 "$RELEASE_STAGING/deployment.sha256"
  chmod 0755 "$RELEASE_STAGING"
  printf '%s  %s\n' "$CURRENT_SHA256" "$RELEASE_STAGING/binance-lob-archiver" \
    | sha256sum --check --strict >/dev/null \
    || die 'staged adopted binary digest mismatch'

  EVIDENCE_STAGING=$(mktemp -d "$EVIDENCE_ROOT/.${CURRENT_SHA256}.adopt.XXXXXX")
  install -d -m 0750 "$EVIDENCE_STAGING/deployment"
  install -m 0440 "$PRODUCTION_SERVICE" \
    "$EVIDENCE_STAGING/deployment/binance-lob-archiver-production@.service"
  install -m 0440 "$CANDIDATE_UPLOAD" \
    "$EVIDENCE_STAGING/deployment/binance-lob-archiver-upload@.service"
  install -m 0440 "$SPOT_ENV" \
    "$EVIDENCE_STAGING/deployment/binance-lob-archiver-production-spot.env"
  install -m 0440 "$USDM_ENV" \
    "$EVIDENCE_STAGING/deployment/binance-lob-archiver-production-usdm.env"
  install -m 0440 "$CANDIDATE_METADATA" "$EVIDENCE_STAGING/candidate-release.json"
  install -m 0440 "$RELEASE_STAGING/adopted-release.json" \
    "$EVIDENCE_STAGING/adopted-release.json"
  install -m 0440 "$RELEASE_STAGING/deployment.sha256" \
    "$EVIDENCE_STAGING/release-deployment.sha256"

  [[ $(capture_unit_states) == "$BEFORE_UNITS" ]] \
    || die 'production unit identity changed while staging adoption'
  current_health=$(capture_health_states) \
    || die 'production health became stale while staging adoption'
  verify_health_continuity "$BEFORE_HEALTH" "$current_health" \
    || die 'production health changed unexpectedly while staging adoption'
  BEFORE_HEALTH=$current_health
  printf '%s  %s\n' "$CURRENT_SHA256" "$PRODUCTION_BINARY" \
    | sha256sum --check --strict >/dev/null \
    || die 'production binary changed while staging adoption'

  sync -f "$RELEASE_STAGING"
  mv -T "$RELEASE_STAGING" "$RELEASE_DIR"
  RELEASE_STAGING=
  RELEASE_CREATED=1
  sync -f "$RELEASE_ROOT"
  atomic_install 0644 "$CANDIDATE_UPLOAD" "$UPLOAD_SERVICE" \
    || die 'could not install the synthetic rollback upload unit'
  UPLOAD_INSTALLED=1
  systemctl daemon-reload || die 'systemd daemon-reload failed after upload-unit install'
  verify_upload_quiescent

  LINK_TMP="${PRODUCTION_BINARY}.adopt.$$"
  ln -s "$RELEASE_BINARY" "$LINK_TMP"
  mv -Tf "$LINK_TMP" "$PRODUCTION_BINARY"
  LINK_TMP=
  LINK_REPLACED=1
  sync -f "${PRODUCTION_BINARY%/*}"
  [[ $(readlink -f -- "$PRODUCTION_BINARY") == "$RELEASE_BINARY" ]] \
    || die 'production symlink does not resolve to the adopted release'
  printf '%s  %s\n' "$CURRENT_SHA256" "$PRODUCTION_BINARY" \
    | sha256sum --check --strict >/dev/null \
    || die 'production symlink does not preserve the running binary digest'

  AFTER_UNITS=$(capture_unit_states) || die 'production unit identity changed after adoption'
  [[ $AFTER_UNITS == "$BEFORE_UNITS" ]] \
    || die 'production PID, restart count, or executable digest changed during adoption'
  AFTER_HEALTH=$(capture_health_states) || die 'production health is stale after adoption'
  verify_health_continuity "$BEFORE_HEALTH" "$AFTER_HEALTH" \
    || die 'production health identity or freshness changed during adoption'
  verify_upload_quiescent
  write_adoption_evidence || die 'could not write immutable adoption evidence'
  chmod 0550 "$EVIDENCE_STAGING/deployment" "$EVIDENCE_STAGING"
  sync -f "$EVIDENCE_STAGING"
  mv -T "$EVIDENCE_STAGING" "$EVIDENCE_DIR"
  EVIDENCE_STAGING=
  EVIDENCE_CREATED=1
  sync -f "$EVIDENCE_ROOT"
  SUCCESS=1
  printf 'adopted running production binary as digest release %s without restarting it\n' \
    "$CURRENT_SHA256"
  printf 'evidence: %s/adoption.json\n' "$EVIDENCE_DIR"
)

main() {
  [[ $(id -u) == 0 ]] || { printf 'must run as root\n' >&2; exit 2; }
  if [[ $# -ne 2 || ! $1 =~ ^[A-Fa-f0-9]{64}$ || ! $2 =~ ^[A-Fa-f0-9]{64}$ ]]; then
    usage
    exit 2
  fi
  for command in awk chmod cmp date find flock grep id install jq ln mkdir mktemp mountpoint \
    mv readlink rm sha256sum sort stat sync systemctl tr wc; do
    command -v "$command" >/dev/null 2>&1 \
      || { printf 'missing required command: %s\n' "$command" >&2; exit 2; }
  done
  configure_paths ''
  EXPECTED_ROOT_UID=0
  install -d -m 0755 "$LOCK_ROOT"
  exec 9>"$LOCK_ROOT/monday-rust-lob-release.lock"
  flock -n 9 \
    || { printf 'another Rust collector release operation holds the host lock\n' >&2; exit 1; }
  if [[ ! -d $DATA_ROOT || -L $DATA_ROOT ]] || ! mountpoint -q "$DATA_ROOT"; then
    printf '/data must be a mounted filesystem\n' >&2
    exit 1
  fi
  adopt_release \
    "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')" \
    "$(printf '%s' "$2" | tr '[:upper:]' '[:lower:]')"
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
