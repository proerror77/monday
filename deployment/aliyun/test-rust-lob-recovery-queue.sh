#!/usr/bin/env bash
set -Eeuo pipefail
export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
QUEUE_SCRIPT="$SCRIPT_DIR/host-rust-lob-recovery-queue.sh"

for command in awk cmp grep install jq mktemp sed sha256sum sort; do
  command -v "$command" >/dev/null 2>&1 \
    || { printf 'missing test dependency: %s\n' "$command" >&2; exit 2; }
done

raw_tmp_dir=$(mktemp -d /tmp/monday-rust-lob-recovery-queue.XXXXXX 2>/dev/null || mktemp -d -t monday-rust-lob-recovery-queue)
tmp_dir=$(cd "$raw_tmp_dir" && pwd -P)
cleanup() {
  chmod -R u+w "$tmp_dir" 2>/dev/null || true
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

grep -Fq 'ExecStartPre=+/opt/monday/bin/monday-rust-lob-recovery-queue isolate %i' \
  "$SCRIPT_DIR/binance-lob-archiver-production@.service"
grep -Fq 'ExecStart=/opt/monday/bin/monday-rust-lob-recovery-queue drain %i' \
  "$SCRIPT_DIR/binance-lob-archiver-recovery@.service"
grep -Fq 'host-rust-lob-recovery-queue.sh' "$SCRIPT_DIR/deploy-rust-lob-release.sh"
grep -Fxq 'MemoryMax=2560M' "$SCRIPT_DIR/binance-lob-archiver-production@.service"
production_unit="$SCRIPT_DIR/binance-lob-archiver-production@.service"
grep -Fxq 'StartLimitIntervalSec=7200' "$production_unit"
grep -Fxq 'StartLimitBurst=5' "$SCRIPT_DIR/binance-lob-archiver-production@.service"
grep -Fxq 'TimeoutStartSec=120' "$SCRIPT_DIR/binance-lob-archiver-production@.service"
grep -Fxq 'CPUQuota=25%' "$SCRIPT_DIR/binance-lob-archiver-recovery@.service"
grep -Fxq 'MemoryMax=768M' "$SCRIPT_DIR/binance-lob-archiver-recovery@.service"

unit_seconds() {
  local key=$1
  sed -n "s/^${key}=//p" "$production_unit"
}
start_limit_seconds=$(unit_seconds StartLimitIntervalSec)
start_limit_burst=$(unit_seconds StartLimitBurst)
start_timeout_seconds=$(unit_seconds TimeoutStartSec)
stop_timeout_seconds=$(unit_seconds TimeoutStopSec)
restart_delay_seconds=$(unit_seconds RestartSec)
(( start_limit_seconds > start_limit_burst \
  * (start_timeout_seconds + stop_timeout_seconds + restart_delay_seconds) )) || {
  printf 'production restart limit can roll forward during repeated startup timeouts\n' >&2
  exit 1
}

# shellcheck disable=SC1090
. "$QUEUE_SCRIPT"

configure_paths /
[[ $ROOT_PREFIX == '' \
  && $OPT_ROOT == /opt/monday \
  && $DATA_ROOT == /data \
  && $LOCK_ROOT == /run/lock ]] || {
  printf 'root path configuration introduced a double-slash prefix\n' >&2
  exit 1
}

stat() {
  if [[ ${1:-} == -c ]]; then
    local format=$2
    shift 2
    [[ ${1:-} != -- ]] || shift
    case "$format" in
      %u)
        # Values are populated by configure_paths() from the sourced script.
        # shellcheck disable=SC2153
        case "$1" in
          "$CANONICAL_ROOT/spot"|"$CANONICAL_ROOT/usdm"|"$QUEUE_ROOT"/*/*.ready|"$QUEUE_ROOT"/*/*.running|"$QUEUE_ROOT"/*/*.failed|*/.binance-lob-archiver.lock)
            printf '4241\n'
            ;;
          *) printf '0\n' ;;
        esac
        ;;
      %g)
        # shellcheck disable=SC2153
        case "$1" in
          "$CANONICAL_ROOT/spot"|"$CANONICAL_ROOT/usdm"|*/.binance-lob-archiver.lock|"$QUEUE_ROOT"|"$QUEUE_ROOT"/*)
            printf '4242\n'
            ;;
          *) printf '0\n' ;;
        esac
        ;;
      %a)
        if [[ $(uname -s) == Darwin ]]; then
          /usr/bin/stat -f %Lp "$1"
        else
          /usr/bin/stat -c %a "$1"
        fi
        ;;
      %d)
        if [[ $(uname -s) == Darwin ]]; then
          /usr/bin/stat -f %d "$1"
        else
          /usr/bin/stat -c %d "$1"
        fi
        ;;
      *) return 2 ;;
    esac
  else
    /usr/bin/stat "$@"
  fi
}

mv() {
  local -a args=()
  local arg
  for arg in "$@"; do
    [[ $arg == -T || $arg == -Tf ]] || args+=("$arg")
  done
  command mv -f "${args[@]}"
}

sha256sum() {
  local -a args=()
  local arg check=0 checksum_file
  for arg in "$@"; do
    case "$arg" in
      --strict) ;;
      --check) check=1 ;;
      *) args+=("$arg") ;;
    esac
  done
  if (( check )); then
    if (( ${#args[@]} )); then
      command sha256sum -c "${args[@]}"
    else
      checksum_file=$(mktemp)
      command cat >"$checksum_file"
      command sha256sum -c "$checksum_file"
      command rm -f "$checksum_file"
    fi
  elif (( ${#args[@]} )); then
    command sha256sum "${args[@]}"
  else
    command sha256sum
  fi
}

install() {
  local -a args=()
  while (($#)); do
    case "$1" in
      -o|-g)
        shift 2
        ;;
      *)
        args+=("$1")
        shift
        ;;
    esac
  done
  command install "${args[@]}"
}

id() {
  if [[ ${1:-} == -u && ${2:-} == hftcollector ]]; then
    printf '4241\n'
  elif [[ ${1:-} == -g && ${2:-} == hftcollector ]]; then
    printf '4242\n'
  else
    command id "$@"
  fi
}

systemctl() {
  if [[ ${1:-} == start ]]; then
    if { : >&8; } 2>/dev/null; then
      printf 'fd8-open\n' >>"$MOCK_SEQUENCE_LOG"
    else
      printf 'fd8-closed\n' >>"$MOCK_SEQUENCE_LOG"
    fi
    if { : >&9; } 2>/dev/null; then
      printf 'fd9-open\n' >>"$MOCK_SEQUENCE_LOG"
    else
      printf 'fd9-closed\n' >>"$MOCK_SEQUENCE_LOG"
    fi
  fi
  printf 'systemctl %s\n' "$*" >>"$MOCK_SEQUENCE_LOG"
  printf '%s\n' "$*" >>"$MOCK_SYSTEMCTL_LOG"
  if [[ ${1:-} == start ]]; then
    return 0
  fi
  return 1
}

runuser() {
  printf 'runuser %s\n' "$*" >>"$MOCK_RUNUSER_LOG"
  while (($#)); do
    [[ $1 == -- ]] && { shift; break; }
    shift
  done
  "$@"
}

flock() {
  printf 'flock %s\n' "$*" >>"$MOCK_SEQUENCE_LOG"
  if [[ ${1:-} == -n && ${2:-} == 7 && ${MOCK_DRAIN_LOCK_BUSY:-0} == 1 ]]; then
    return 1
  fi
  return 0
}

sync() {
  local argument
  for argument in "$@"; do
    if [[ $argument == -f ]]; then
      printf 'recovery queue attempted filesystem-wide sync\n' >&2
      return 1
    fi
  done
  printf '%s\n' "$*" >>"$MOCK_SYNC_LOG"
  printf 'sync %s\n' "$*" >>"$MOCK_SEQUENCE_LOG"
  return 0
}

setup_fixture() {
  local fixture=$1
  local release_sha source_sha bundle_sha runtime_sha release_dir env_file release_env
  MOCK_SYSTEMCTL_LOG="$fixture/systemctl.log"
  MOCK_RUNUSER_LOG="$fixture/runuser.log"
  MOCK_CALLS_LOG="$fixture/binary.log"
  MOCK_SYNC_LOG="$fixture/sync.log"
  MOCK_SEQUENCE_LOG="$fixture/sequence.log"
  configure_paths "$fixture"
  mkdir -p \
    "$BIN_DIR" "$RELEASE_ROOT" "$CONFIG_ROOT" "$LOCK_ROOT" \
    "$CANONICAL_ROOT/spot" "$CANONICAL_ROOT/usdm" \
    "$QUEUE_ROOT/spot" "$QUEUE_ROOT/usdm" \
    "$EVIDENCE_ROOT"
  cat >"$fixture/fake-binary.sh" <<EOF
#!/usr/bin/env bash
set -Eeuo pipefail
printf '%s %s %s\n' "\${MARKET:-}" "\${1:-}" "\${SPOOL_DIR:-}" >>"$MOCK_CALLS_LOG"
case "\${1:-}" in
  --help)
    printf '%s\n' '--recover-parts-only'
    printf '%s\n' '--upload-only'
    ;;
  --recover-parts-only)
    if [[ -f $fixture/fail-recover ]]; then
      exit 1
    fi
    install -d -m 0750 "\$RECOVERY_BACKUP_DIR"
    jq -n \
      --arg artifact "\$RECOVERY_ARTIFACT_SHA256" \
      --arg bundle "\$RECOVERY_DEPLOYMENT_BUNDLE_SHA256" \
      --arg source "\$RECOVERY_DEPLOYMENT_SOURCE_REVISION" \
      '{artifact_sha256:\$artifact,deployment_bundle_sha256:\$bundle,
        deployment_source_revision:\$source}' >"\$RECOVERY_BACKUP_DIR/receipt.json"
    ;;
  --upload-only)
    if [[ -f $fixture/fail-upload ]]; then
      exit 1
    fi
    find "\$SPOOL_DIR" -type f \( \
      -name '*.jsonl.part' -o \
      -name '*.zst.tmp' -o \
      -name '*.part.corrupt' \
    \) -delete
    jq -n '{last_error:null,last_success_at:"2026-08-22T00:00:00Z"}' \
      >"\$SPOOL_DIR/upload-status.json"
    ;;
  *)
    exit 1
    ;;
esac
EOF
  chmod 0755 "$fixture/fake-binary.sh"
  release_sha=$(sha256sum "$fixture/fake-binary.sh" | awk '{print $1}')
  source_sha=$(printf 'c%.0s' {1..40})
  bundle_sha=$(printf 'b%.0s' {1..64})
  runtime_sha=$(printf 'd%.0s' {1..64})
  release_dir="$RELEASE_ROOT/$release_sha"
  mkdir -p "$release_dir/deployment"
  install -m 0755 "$fixture/fake-binary.sh" "$release_dir/binance-lob-archiver"
  jq -n \
    --arg artifact "$release_sha" \
    --arg bundle "$bundle_sha" \
    --arg source "$source_sha" \
    --arg runtime "$runtime_sha" \
    '{artifact_sha256:$artifact,deployment_bundle_sha256:$bundle,
      deployment_source_revision:$source,
      runtime_contract_sha256:$runtime}' >"$release_dir/release.json"
  ln -s "$release_dir/binance-lob-archiver" "$PRODUCTION_LINK"
  for market in spot usdm; do
    env_file="$CONFIG_ROOT/binance-lob-archiver-production-$market.env"
    release_env="$release_dir/deployment/binance-lob-archiver-production-$market.env"
    sed \
      -e "s|^SPOOL_DIR=.*|SPOOL_DIR=$CANONICAL_ROOT/$market|" \
      "$SCRIPT_DIR/binance-lob-archiver-production-$market.env" >"$env_file"
    install -m 0640 "$env_file" "$release_env"
    : >"$CANONICAL_ROOT/$market/.binance-lob-archiver.lock"
  done
}

activate_controller_fixture() {
  local fixture=$1 artifact_sha artifact_release staging controller_sha
  local controller_source controller_bundle runtime_contract market
  configure_paths "$fixture"
  artifact_release=$(dirname "$(readlink -f "$PRODUCTION_LINK")")
  artifact_sha=${artifact_release##*/}
  runtime_contract=$(jq -er '.runtime_contract_sha256' "$artifact_release/release.json")
  controller_source=$(printf 'e%.0s' {1..40})
  controller_bundle=$(printf 'f%.0s' {1..64})
  staging="$CONTROLLER_RELEASE_ROOT/staging"
  mkdir -p "$staging/deployment"
  install -m 0755 "$QUEUE_SCRIPT" \
    "$staging/deployment/host-rust-lob-recovery-queue.sh"
  install -m 0755 "$QUEUE_SCRIPT" "$INSTALLED_RECOVERY"
  for market in spot usdm; do
    install -m 0640 \
      "$artifact_release/deployment/binance-lob-archiver-production-$market.env" \
      "$staging/deployment/binance-lob-archiver-production-$market.env"
  done
  jq -n \
    --arg artifact "$artifact_sha" \
    --arg bundle "$controller_bundle" \
    --arg source "$controller_source" \
    --arg runtime "$runtime_contract" '
      {schema:"monday.rust_lob_controller_release.v1",
       artifact_sha256:$artifact,deployment_bundle_sha256:$bundle,
       deployment_source_revision:$source,runtime_contract_sha256:$runtime}' \
    >"$staging/release.json"
  controller_sha=$(sha256sum "$staging/release.json" | awk '{print $1}')
  (
    cd "$staging"
    sha256sum release.json >release.json.sha256
    for asset in deployment/*; do sha256sum "$asset"; done \
      | sort -k2 >deployment.sha256
  )
  mv "$staging" "$CONTROLLER_RELEASE_ROOT/$controller_sha"
  ln -s "$CONTROLLER_RELEASE_ROOT/$controller_sha" "$ACTIVE_CONTROLLER"
}

run_action() (
  local fixture=$1 action=$2 market=$3
  configure_paths "$fixture"
  # MARKET is consumed by functions from the sourced recovery script.
  # shellcheck disable=SC2034
  MARKET=$market
  canonical_paths_safe
  market_paths
  queue_lock
  case "$action" in
    isolate)
      secure_release_identity
      isolate_market
      ;;
    drain) drain_market ;;
    *) exit 2 ;;
  esac
)

stage_isolation_marker() {
  local fixture=$1 market=$2 ordinal=$3 timestamp=${4:-20260826T000001Z}
  local receipt_sha256
  configure_paths "$fixture"
  # MARKET and the release globals are consumed by the sourced queue script.
  # shellcheck disable=SC2034
  MARKET=$market
  market_paths
  secure_release_identity
  ensure_queue_directory "$QUEUE_ROOT" "$(id -g hftcollector)"
  ensure_queue_directory "$QUEUE_MARKET_ROOT" "$(id -g hftcollector)"
  STAGED_JOB_ID="$timestamp-$market-${RELEASE_SHA256:0:12}-$ordinal"
  STAGED_READY_DIR="$QUEUE_MARKET_ROOT/$STAGED_JOB_ID.ready"
  write_job_receipt "$STAGED_JOB_ID" "$RECOVERY_SERVICE@$market.service"
  receipt_sha256=$(sha256sum "$CANONICAL_SPOOL/job.json" | awk '{print $1}')
  write_isolation_marker "$STAGED_JOB_ID" "$receipt_sha256"
}

assert() {
  local label=$1
  shift
  "$@" || { printf 'assert failed: %s\n' "$label" >&2; exit 1; }
}

expect_failure() {
  local fixture=$1 action=$2 market=$3 status
  set +e
  (
    set -Eeuo pipefail
    run_action "$fixture" "$action" "$market"
  )
  status=$?
  set -e
  if (( status == 0 )); then
    printf 'expected failure: %s %s\n' "$action" "$market" >&2
    exit 1
  fi
}

test_no_part_noop() {
  local fixture=$tmp_dir/no-part
  setup_fixture "$fixture"
  run_action "$fixture" isolate spot
  assert no-ready-find test -z "$(find "$fixture/data/monday/spool/binance-lob-recovery/spot" -mindepth 1 -print -quit)"
  assert no-systemctl-log test ! -s "$fixture/systemctl.log"
}

test_isolate_creates_ready_job() {
  local fixture=$tmp_dir/isolate-ready ready_dir job_json env_sha
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  run_action "$fixture" isolate spot
  ready_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/spot" -mindepth 1 -maxdepth 1 -type d -name '*.ready' | head -n 1)
  [[ -n $ready_dir ]] || exit 1
  job_json="$ready_dir/job.json"
  jq -e '.market == "spot"
    and (.canonical_spool | endswith("/spot"))
    and .release_env == "recovery.env"' "$job_json" >/dev/null
  env_sha=$(sha256sum "$ready_dir/recovery.env" | awk '{print $1}')
  jq -e --arg env_sha "$env_sha" '.env_sha256 == $env_sha' "$job_json" >/dev/null
  assert canonical-clean test ! -e "$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  assert canonical-recreated test -d "$fixture/data/monday/spool/binance-lob/spot"
  assert systemctl-start grep -Fq 'start --no-block binance-lob-archiver-recovery@spot.service' "$fixture/systemctl.log"
  assert canonical-parent-synced grep -Fxq -- "$fixture/data/monday/spool/binance-lob" "$fixture/sync.log"
  assert queue-parent-synced grep -Fxq -- "$fixture/data/monday/spool/binance-lob-recovery/spot" "$fixture/sync.log"
  if grep -Eq '(^|[[:space:]])-f([[:space:]]|$)' "$fixture/sync.log"; then
    printf 'recovery queue used filesystem-wide sync instead of path-scoped fsync\n' >&2
    exit 1
  fi
}

test_many_incomplete_parts_isolate_without_sigpipe() {
  local fixture=$tmp_dir/many-parts ready_dir index
  setup_fixture "$fixture"
  for index in {0001..1200}; do
    : >"$fixture/data/monday/spool/binance-lob/spot/part-$index.jsonl.part"
  done
  run_action "$fixture" isolate spot
  ready_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/spot" \
    -mindepth 1 -maxdepth 1 -type d -name '*.ready' -print -quit)
  [[ -n $ready_dir ]]
}

test_isolate_preserves_upload_readback() {
  local fixture=$tmp_dir/preserve-upload ready_dir
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  jq -n '{last_success_at:"2026-08-22T00:00:00Z",last_error_at:null,
    last_error:null,failure_count:0}' \
    >"$fixture/data/monday/spool/binance-lob/spot/upload-status.json"
  run_action "$fixture" isolate spot
  ready_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/spot" \
    -mindepth 1 -maxdepth 1 -type d -name '*.ready' -print -quit)
  cmp -s "$ready_dir/upload-status.json" \
    "$fixture/data/monday/spool/binance-lob/spot/upload-status.json"
}

test_drain_runs_recover_then_upload() {
  local fixture=$tmp_dir/drain-success ready_dir evidence_dir
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/usdm/part-001.jsonl.part"
  run_action "$fixture" isolate usdm
  ready_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/usdm" -mindepth 1 -maxdepth 1 -type d -name '*.ready' | head -n 1)
  [[ -n $ready_dir ]] || exit 1
  run_action "$fixture" drain usdm
  evidence_dir=$(find "$fixture/data/monday/evidence/recoveries/lob-queue" -mindepth 1 -maxdepth 1 -type d | head -n 1)
  [[ -d $evidence_dir/spool.done ]] || exit 1
  jq -e '.result == "passed"' "$evidence_dir/result.json" >/dev/null
  grep -Eq '^usdm --recover-parts-only ' "$fixture/binary.log"
  grep -Eq '^usdm --upload-only ' "$fixture/binary.log"
  [[ $(sed -n '1p' "$fixture/binary.log") == usdm\ --recover-parts-only* ]]
  [[ $(sed -n '2p' "$fixture/binary.log") == usdm\ --upload-only* ]]
  assert backup-receipt test -f "$evidence_dir/recovery-input/receipt.json"
  assert evidence-synced grep -Fxq -- "$evidence_dir" "$fixture/sync.log"
  if grep -Eq '(^|[[:space:]])-f([[:space:]]|$)' "$fixture/sync.log"; then
    printf 'recovery drain used filesystem-wide sync instead of path-scoped fsync\n' >&2
    exit 1
  fi
}

test_drain_uses_queued_immutable_inputs() {
  local fixture=$tmp_dir/drain-no-current release_dir
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/usdm/part-001.jsonl.part"
  run_action "$fixture" isolate usdm
  release_dir=$(dirname "$(readlink -f "$fixture/opt/monday/bin/binance-lob-archiver")")
  rm -f "$fixture/opt/monday/bin/binance-lob-archiver"
  rm -f "$fixture/etc/monday/binance-lob-archiver-production-usdm.env"
  printf 'replaced bundle env\n' \
    >"$release_dir/deployment/binance-lob-archiver-production-usdm.env"
  jq '.deployment_bundle_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"' \
    "$release_dir/release.json" >"$release_dir/release.json.tmp"
  mv "$release_dir/release.json.tmp" "$release_dir/release.json"
  run_action "$fixture" drain usdm
  grep -Eq '^usdm --upload-only ' "$fixture/binary.log"
}

test_failed_job_is_not_retried() {
  local fixture=$tmp_dir/drain-fail failed_dir job_id evidence_dir before after
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/usdm/part-001.jsonl.part"
  run_action "$fixture" isolate usdm
  : >"$fixture/fail-upload"
  expect_failure "$fixture" drain usdm
  failed_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/usdm" -mindepth 1 -maxdepth 1 -type d -name '*.failed' | head -n 1)
  [[ -n $failed_dir ]] || exit 1
  job_id=$(jq -r '.job_id' "$failed_dir/job.json")
  evidence_dir="$fixture/data/monday/evidence/recoveries/lob-queue/$job_id"
  jq -e '.result == "failed"' "$evidence_dir/result.json" >/dev/null
  assert failed-input-preserved test -f "$failed_dir/job.json"
  before=$(wc -l <"$fixture/binary.log")
  rm -f "$fixture/fail-upload"
  run_action "$fixture" drain usdm
  after=$(wc -l <"$fixture/binary.log")
  [[ $before == "$after" ]]
}

test_recovery_failure_never_starts_upload() {
  local fixture=$tmp_dir/recover-fail failed_dir job_id evidence_dir
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/usdm/part-001.jsonl.part"
  run_action "$fixture" isolate usdm
  : >"$fixture/fail-recover"
  expect_failure "$fixture" drain usdm
  failed_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/usdm" -mindepth 1 -maxdepth 1 -type d -name '*.failed' | head -n 1)
  [[ -n $failed_dir ]] || exit 1
  job_id=$(jq -r '.job_id' "$failed_dir/job.json")
  evidence_dir="$fixture/data/monday/evidence/recoveries/lob-queue/$job_id"
  jq -e '.result == "failed"' "$evidence_dir/result.json" >/dev/null
  [[ $(grep -c '^usdm --recover-parts-only ' "$fixture/binary.log") == 1 ]]
  assert failed-recovery-no-upload test \
    "$(grep -c '^usdm --upload-only ' "$fixture/binary.log" || true)" -eq 0
}

test_other_market_recovery_defers_without_mutating_job() {
  local fixture=$tmp_dir/drain-deferred ready_dir
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  run_action "$fixture" isolate spot
  MOCK_DRAIN_LOCK_BUSY=1 run_action "$fixture" drain spot
  ready_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/spot" -mindepth 1 -maxdepth 1 -type d -name '*.ready' | head -n 1)
  [[ -n $ready_dir ]]
  assert deferred-no-binary test ! -s "$fixture/binary.log"
}

test_invalid_release_env_never_executes_recovery_binary() {
  local fixture=$tmp_dir/invalid-env ready_dir release_env env_sha
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/usdm/part-001.jsonl.part"
  run_action "$fixture" isolate usdm
  ready_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/usdm" -mindepth 1 -maxdepth 1 -type d -name '*.ready' | head -n 1)
  release_env="$ready_dir/$(jq -r '.release_env' "$ready_dir/job.json")"
  sed '/^OSS_COPY_TIMEOUT_SECONDS=/d' "$release_env" >"$release_env.tmp"
  mv "$release_env.tmp" "$release_env"
  env_sha=$(sha256sum "$release_env" | awk '{print $1}')
  jq --arg env_sha "$env_sha" '.env_sha256 = $env_sha' "$ready_dir/job.json" \
    >"$ready_dir/job.json.tmp"
  mv "$ready_dir/job.json.tmp" "$ready_dir/job.json"
  expect_failure "$fixture" drain usdm
  assert invalid-env-no-binary test ! -s "$fixture/binary.log"
}

test_unsafe_release_identity_fails_closed() {
  local fixture=$tmp_dir/unsafe-release release_dir release_json
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  release_dir=$(readlink -f "$fixture/opt/monday/bin/binance-lob-archiver")
  release_json="$(dirname "$release_dir")/release.json"
  jq '.artifact_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"' \
    "$release_json" >"$release_json.tmp"
  mv "$release_json.tmp" "$release_json"
  expect_failure "$fixture" isolate spot
}

test_active_controller_attributes_recovery_and_rejects_drift() {
  local fixture=$tmp_dir/controller-attribution ready_dir
  setup_fixture "$fixture"
  activate_controller_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  run_action "$fixture" isolate spot
  ready_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/spot" \
    -mindepth 1 -maxdepth 1 -type d -name '*.ready' -print -quit)
  jq -e \
    --arg bundle "$(printf 'f%.0s' {1..64})" \
    --arg source "$(printf 'e%.0s' {1..40})" '
      .deployment_bundle_sha256 == $bundle
      and .deployment_source_revision == $source' \
    "$ready_dir/job.json" >/dev/null
  printf '\n# drift\n' >>"$fixture/opt/monday/bin/monday-rust-lob-recovery-queue"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/usdm/part-001.jsonl.part"
  expect_failure "$fixture" isolate usdm
}

test_missing_canonical_lock_fails_closed() {
  local fixture=$tmp_dir/missing-lock
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  rm -f "$fixture/data/monday/spool/binance-lob/spot/.binance-lob-archiver.lock"
  expect_failure "$fixture" isolate spot
}

test_symlinked_queue_root_fails_closed() {
  local fixture=$tmp_dir/unsafe-path
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  rm -rf "$fixture/data/monday/spool/binance-lob-recovery"
  ln -s "$fixture/elsewhere" "$fixture/data/monday/spool/binance-lob-recovery"
  expect_failure "$fixture" isolate spot
}

test_isolation_marker_recovers_all_crash_boundaries() {
  local state fixture canonical marker ready
  for state in before-mv after-mv after-mkdir; do
    fixture="$tmp_dir/isolation-$state"
    setup_fixture "$fixture"
    canonical="$fixture/data/monday/spool/binance-lob/spot"
    printf 'part\n' >"$canonical/part-001.jsonl.part"
    stage_isolation_marker "$fixture" spot 7001
    marker=$ISOLATION_MARKER
    ready=$STAGED_READY_DIR
    case "$state" in
      before-mv) ;;
      after-mv)
        mv "$canonical" "$ready"
        ;;
      after-mkdir)
        mv "$canonical" "$ready"
        install -d -m 0750 -o 4241 -g 4242 "$canonical"
        ;;
    esac
    run_action "$fixture" isolate spot
    assert "$state-canonical" test -d "$canonical"
    assert "$state-ready" test -d "$ready"
    assert "$state-marker-committed" test ! -e "$marker"
    assert "$state-worker-restarted" \
      grep -Fq 'start --no-block binance-lob-archiver-recovery@spot.service' \
      "$fixture/systemctl.log"
  done
}

test_missing_canonical_with_multiple_backlog_uses_marker() {
  local fixture=$tmp_dir/missing-canonical-multiple canonical first_ready second_ready
  setup_fixture "$fixture"
  canonical="$fixture/data/monday/spool/binance-lob/spot"
  printf 'first\n' >"$canonical/part-001.jsonl.part"
  stage_isolation_marker "$fixture" spot 7101 20260826T000001Z
  first_ready=$STAGED_READY_DIR
  run_action "$fixture" isolate spot
  printf 'second\n' >"$canonical/part-002.jsonl.part"
  stage_isolation_marker "$fixture" spot 7102 20260826T000002Z
  second_ready=$STAGED_READY_DIR
  mv "$canonical" "$second_ready"
  run_action "$fixture" isolate spot
  assert canonical-recovered test -d "$canonical"
  assert first-backlog-preserved test -d "$first_ready"
  assert marked-transaction-preserved test -d "$second_ready"
  assert two-valid-ready-jobs test "$(find "$QUEUE_MARKET_ROOT" -mindepth 1 -maxdepth 1 -type d -name '*.ready' | wc -l | tr -d ' ')" -eq 2
  ( load_job "$first_ready" )
  ( load_job "$second_ready" )
  assert marker-committed test ! -e "$ISOLATION_MARKER"
  rm -rf "$canonical"
  : >"$fixture/systemctl.log"
  expect_failure "$fixture" isolate spot
  assert multiple-legacy-jobs-not-adopted test ! -e "$canonical"
  assert multiple-legacy-jobs-not-started test ! -s "$fixture/systemctl.log"
}

test_recovery_start_follows_durable_lock_handoff() {
  local fixture=$tmp_dir/lock-handoff sequence
  setup_fixture "$fixture"
  sequence="$fixture/sequence.log"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  run_action "$fixture" isolate spot
  awk -v canonical="$fixture/data/monday/spool/binance-lob/spot" \
    -v canonical_root="$fixture/data/monday/spool/binance-lob" \
    -v queue_root="$fixture/data/monday/spool/binance-lob-recovery/spot" \
    -v start='systemctl start --no-block binance-lob-archiver-recovery@spot.service' '
      $0 == "sync " canonical { canonical_sync = NR }
      $0 == "sync " canonical_root { canonical_root_sync = NR }
      $0 == "sync " queue_root { queue_sync = NR }
      $0 == "flock -u 8" { unlock8 = NR }
      $0 == "flock -u 9" { unlock9 = NR }
      $0 == "fd8-closed" { closed8 = NR }
      $0 == "fd9-closed" { closed9 = NR }
      $0 == start { started = NR }
      END {
        exit !(canonical_sync && canonical_root_sync && queue_sync && unlock8 && unlock9 &&
          closed8 && closed9 && started && canonical_sync < unlock8 &&
          canonical_root_sync < unlock8 && queue_sync < unlock8 && unlock9 == unlock8 + 1 &&
          closed8 == unlock9 + 1 && closed9 == closed8 + 1 && started == closed9 + 1)
      }
    ' "$sequence"

  printf 'second\n' >"$fixture/data/monday/spool/binance-lob/spot/part-002.jsonl.part"
  stage_isolation_marker "$fixture" spot 7202 20260826T000002Z
  mv "$fixture/data/monday/spool/binance-lob/spot" "$STAGED_READY_DIR"
  : >"$sequence"
  : >"$fixture/sync.log"
  run_action "$fixture" isolate spot
  awk -v canonical_root="$fixture/data/monday/spool/binance-lob" \
    -v start='systemctl start --no-block binance-lob-archiver-recovery@spot.service' '
      $0 == "sync " canonical_root { durable = NR }
      $0 == "flock -u 8" { unlock8 = NR }
      $0 == "flock -u 9" { unlock9 = NR }
      $0 == "fd8-closed" { closed8 = NR }
      $0 == "fd9-closed" { closed9 = NR }
      $0 == start { started = NR }
      END {
        exit !(durable && !unlock8 && unlock9 && closed8 && closed9 && started &&
          durable < unlock9 && closed8 == unlock9 + 1 && closed9 == closed8 + 1 &&
          started == closed9 + 1)
      }
    ' "$sequence"
}

test_missing_canonical_single_valid_legacy_job_recovers() {
  local fixture=$tmp_dir/missing-canonical-single-legacy ready_dir
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  run_action "$fixture" isolate spot
  ready_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/spot" -mindepth 1 -maxdepth 1 -type d -name '*.ready' | head -n 1)
  rm -rf "$fixture/data/monday/spool/binance-lob/spot"
  : >"$fixture/systemctl.log"
  run_action "$fixture" isolate spot
  assert legacy-canonical-recreated test -d "$fixture/data/monday/spool/binance-lob/spot"
  assert legacy-job-preserved test -d "$ready_dir"
  assert legacy-recovery-started \
    grep -Fq 'start --no-block binance-lob-archiver-recovery@spot.service' \
    "$fixture/systemctl.log"
}

test_missing_canonical_without_valid_legacy_job_fails_closed() {
  local fixture=$tmp_dir/missing-canonical-no-valid-legacy
  setup_fixture "$fixture"
  rm -rf "$fixture/data/monday/spool/binance-lob/spot"
  expect_failure "$fixture" isolate spot
  assert no-valid-legacy-canonical-not-invented test ! -e "$fixture/data/monday/spool/binance-lob/spot"
}

test_missing_canonical_mixed_legacy_queue_fails_closed() {
  local fixture=$tmp_dir/missing-canonical-mixed-legacy malformed
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  run_action "$fixture" isolate spot
  malformed="$fixture/data/monday/spool/binance-lob-recovery/spot/00000000T000000Z-spot-aaaaaaaaaaaa-1.ready"
  mkdir "$malformed"
  printf '{}\n' >"$malformed/job.json"
  chmod 0640 "$malformed/job.json"
  rm -rf "$fixture/data/monday/spool/binance-lob/spot"
  : >"$fixture/systemctl.log"
  expect_failure "$fixture" isolate spot
  assert mixed-legacy-canonical-not-invented test ! -e "$fixture/data/monday/spool/binance-lob/spot"
  assert mixed-legacy-recovery-not-started test ! -s "$fixture/systemctl.log"
}

test_isolation_transaction_drift_fails_closed() {
  local fixture=$tmp_dir/isolation-marker-drift canonical ready
  setup_fixture "$fixture"
  canonical="$fixture/data/monday/spool/binance-lob/spot"
  printf 'part\n' >"$canonical/part-001.jsonl.part"
  stage_isolation_marker "$fixture" spot 7301
  ready=$STAGED_READY_DIR
  mv "$canonical" "$ready"
  jq '.receipt_sha256 = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"' \
    "$ISOLATION_MARKER" >"$ISOLATION_MARKER.tmp"
  mv "$ISOLATION_MARKER.tmp" "$ISOLATION_MARKER"
  expect_failure "$fixture" isolate spot
  assert drift-ready-preserved test -d "$ready"
  assert drift-canonical-not-invented test ! -e "$canonical"
  assert drift-marker-preserved test -f "$ISOLATION_MARKER"

  fixture=$tmp_dir/isolation-canonical-drift
  setup_fixture "$fixture"
  canonical="$fixture/data/monday/spool/binance-lob/spot"
  printf 'old\n' >"$canonical/part-001.jsonl.part"
  stage_isolation_marker "$fixture" spot 7302
  ready=$STAGED_READY_DIR
  mv "$canonical" "$ready"
  install -d -m 0750 -o 4241 -g 4242 "$canonical"
  printf 'new\n' >"$canonical/part-unexpected.jsonl.part"
  expect_failure "$fixture" isolate spot
  assert canonical-drift-preserved test -f "$canonical/part-unexpected.jsonl.part"
  assert canonical-drift-marker-preserved test -f "$ISOLATION_MARKER"
}

test_malformed_oldest_ready_is_not_renamed() {
  local fixture=$tmp_dir/malformed-oldest malformed valid
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  run_action "$fixture" isolate spot
  valid=$(find "$fixture/data/monday/spool/binance-lob-recovery/spot" \
    -mindepth 1 -maxdepth 1 -type d -name '*.ready' -print -quit)
  malformed="$fixture/data/monday/spool/binance-lob-recovery/spot/00000000T000000Z-spot-aaaaaaaaaaaa-1.ready"
  mkdir "$malformed"
  printf '{}\n' >"$malformed/job.json"
  chmod 0640 "$malformed/job.json"
  expect_failure "$fixture" drain spot
  assert malformed-remains-ready test -d "$malformed"
  assert valid-remains-ready test -d "$valid"
  assert no-running-created test -z "$(find "$fixture/data/monday/spool/binance-lob-recovery/spot" \
    -mindepth 1 -maxdepth 1 -type d -name '*.running' -print -quit)"
}

test_passed_running_finalizes_without_retry() {
  local fixture=$tmp_dir/passed-running ready_dir running_dir job_id evidence_dir
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/usdm/part-001.jsonl.part"
  run_action "$fixture" isolate usdm
  ready_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/usdm" -mindepth 1 -maxdepth 1 -type d -name '*.ready' | head -n 1)
  running_dir="${ready_dir%.ready}.running"
  mv "$ready_dir" "$running_dir"
  # Globals below are produced/consumed by functions from the sourced script.
  # shellcheck disable=SC2034
  MARKET=usdm
  market_paths
  load_job "$running_dir"
  # shellcheck disable=SC2153
  job_id=$JOB_ID
  # shellcheck disable=SC2034
  JOB_STARTED_AT=2026-08-22T00:00:00Z
  evidence_dir="$fixture/data/monday/evidence/recoveries/lob-queue/$job_id"
  ensure_root_directory "$evidence_dir"
  write_result "$evidence_dir/result.json" passed upload-readback-ok complete
  run_action "$fixture" drain usdm
  [[ -d $evidence_dir/spool.done ]] || exit 1
  assert no-binary-call test ! -s "$fixture/binary.log"
}

test_running_job_ignores_untrusted_in_spool_pass_result() {
  local fixture=$tmp_dir/forged-result ready_dir running_dir
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/usdm/part-001.jsonl.part"
  run_action "$fixture" isolate usdm
  ready_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/usdm" -mindepth 1 -maxdepth 1 -type d -name '*.ready' | head -n 1)
  running_dir="${ready_dir%.ready}.running"
  mv "$ready_dir" "$running_dir"
  jq -n '{result:"passed"}' >"$running_dir/result.json"
  expect_failure "$fixture" drain usdm
  [[ -d $running_dir ]]
  assert forged-result-no-binary test ! -s "$fixture/binary.log"
}

test_no_part_noop
test_isolate_creates_ready_job
test_many_incomplete_parts_isolate_without_sigpipe
test_isolate_preserves_upload_readback
test_drain_runs_recover_then_upload
test_drain_uses_queued_immutable_inputs
test_recovery_failure_never_starts_upload
test_failed_job_is_not_retried
test_other_market_recovery_defers_without_mutating_job
test_invalid_release_env_never_executes_recovery_binary
test_unsafe_release_identity_fails_closed
test_active_controller_attributes_recovery_and_rejects_drift
test_missing_canonical_lock_fails_closed
test_symlinked_queue_root_fails_closed
test_isolation_marker_recovers_all_crash_boundaries
test_missing_canonical_with_multiple_backlog_uses_marker
test_recovery_start_follows_durable_lock_handoff
test_missing_canonical_single_valid_legacy_job_recovers
test_missing_canonical_without_valid_legacy_job_fails_closed
test_missing_canonical_mixed_legacy_queue_fails_closed
test_isolation_transaction_drift_fails_closed
test_malformed_oldest_ready_is_not_renamed
test_passed_running_finalizes_without_retry
test_running_job_ignores_untrusted_in_spool_pass_result

printf 'ok\n'
