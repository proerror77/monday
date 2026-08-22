#!/usr/bin/env bash
set -Eeuo pipefail
export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
QUEUE_SCRIPT="$SCRIPT_DIR/host-rust-lob-recovery-queue.sh"

for command in awk grep install jq mktemp python3 sed sha256sum; do
  command -v "$command" >/dev/null 2>&1 \
    || { printf 'missing test dependency: %s\n' "$command" >&2; exit 2; }
done

tmp_dir=$(python3 -c 'import os, tempfile; print(os.path.realpath(tempfile.mkdtemp()))')
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

# shellcheck disable=SC1090
. "$QUEUE_SCRIPT"

stat() {
  if [[ ${1:-} == -c ]]; then
    local format=$2
    shift 2
    [[ ${1:-} != -- ]] || shift
    case "$format" in
      %u|%g) printf '0\n' ;;
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
    printf '0\n'
  elif [[ ${1:-} == -g && ${2:-} == hftcollector ]]; then
    printf '0\n'
  else
    command id "$@"
  fi
}

systemctl() {
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
  return 0
}

sync() {
  return 0
}

setup_fixture() {
  local fixture=$1
  local release_sha source_sha bundle_sha release_dir canonical_root queue_root env_file release_env
  FIXTURE_ROOT=$fixture
  MOCK_SYSTEMCTL_LOG="$fixture/systemctl.log"
  MOCK_RUNUSER_LOG="$fixture/runuser.log"
  MOCK_CALLS_LOG="$fixture/binary.log"
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
  release_dir="$RELEASE_ROOT/$release_sha"
  mkdir -p "$release_dir/deployment"
  install -m 0755 "$fixture/fake-binary.sh" "$release_dir/binance-lob-archiver"
  jq -n \
    --arg artifact "$release_sha" \
    --arg bundle "$bundle_sha" \
    --arg source "$source_sha" \
    '{artifact_sha256:$artifact,deployment_bundle_sha256:$bundle,
      deployment_source_revision:$source}' >"$release_dir/release.json"
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

run_action() (
  local fixture=$1 action=$2 market=$3
  configure_paths "$fixture"
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

assert() {
  local label=$1
  shift
  "$@" || { printf 'assert failed: %s\n' "$label" >&2; exit 1; }
}

expect_failure() {
  local fixture=$1 action=$2 market=$3
  if run_action "$fixture" "$action" "$market"; then
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
  local fixture=$tmp_dir/isolate-ready ready_dir job_json
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  run_action "$fixture" isolate spot
  ready_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/spot" -mindepth 1 -maxdepth 1 -type d -name '*.ready' | head -n 1)
  [[ -n $ready_dir ]] || exit 1
  job_json="$ready_dir/job.json"
  jq -e '.market == "spot" and (.canonical_spool | endswith("/spot"))' "$job_json" >/dev/null
  assert canonical-clean test ! -e "$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  assert canonical-recreated test -d "$fixture/data/monday/spool/binance-lob/spot"
  assert systemctl-start grep -Fq 'start --no-block binance-lob-archiver-recovery@spot.service' "$fixture/systemctl.log"
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
}

test_drain_does_not_depend_on_current_production_symlink() {
  local fixture=$tmp_dir/drain-no-current
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/usdm/part-001.jsonl.part"
  run_action "$fixture" isolate usdm
  rm -f "$fixture/opt/monday/bin/binance-lob-archiver"
  rm -f "$fixture/etc/monday/binance-lob-archiver-production-usdm.env"
  run_action "$fixture" drain usdm
  grep -Eq '^usdm --upload-only ' "$fixture/binary.log"
}

test_failed_job_is_not_retried() {
  local fixture=$tmp_dir/drain-fail failed_dir before after
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/usdm/part-001.jsonl.part"
  run_action "$fixture" isolate usdm
  : >"$fixture/fail-upload"
  expect_failure "$fixture" drain usdm
  failed_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/usdm" -mindepth 1 -maxdepth 1 -type d -name '*.failed' | head -n 1)
  [[ -n $failed_dir ]] || exit 1
  jq -e '.result == "failed"' "$failed_dir/result.json" >/dev/null
  assert failed-input-preserved test -f "$failed_dir/job.json"
  before=$(wc -l <"$fixture/binary.log")
  rm -f "$fixture/fail-upload"
  run_action "$fixture" drain usdm
  after=$(wc -l <"$fixture/binary.log")
  [[ $before == "$after" ]]
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

test_missing_canonical_recovers_only_with_valid_queue_job() {
  local fixture=$tmp_dir/missing-canonical-valid
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  run_action "$fixture" isolate spot
  rm -rf "$fixture/data/monday/spool/binance-lob/spot"
  run_action "$fixture" isolate spot
  [[ -d $fixture/data/monday/spool/binance-lob/spot ]]
  assert worker-restarted grep -Fq 'start --no-block binance-lob-archiver-recovery@spot.service' "$fixture/systemctl.log"
}

test_missing_canonical_without_valid_queue_job_fails_closed() {
  local fixture=$tmp_dir/missing-canonical-invalid ready_dir
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/spot/part-001.jsonl.part"
  run_action "$fixture" isolate spot
  ready_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/spot" -mindepth 1 -maxdepth 1 -type d -name '*.ready' | head -n 1)
  jq '.canonical_spool = "/wrong"' "$ready_dir/job.json" >"$ready_dir/job.json.tmp"
  mv "$ready_dir/job.json.tmp" "$ready_dir/job.json"
  rm -rf "$fixture/data/monday/spool/binance-lob/spot"
  expect_failure "$fixture" isolate spot
}

test_passed_running_finalizes_without_retry() {
  local fixture=$tmp_dir/passed-running ready_dir running_dir job_id evidence_dir
  setup_fixture "$fixture"
  printf 'part\n' >"$fixture/data/monday/spool/binance-lob/usdm/part-001.jsonl.part"
  run_action "$fixture" isolate usdm
  ready_dir=$(find "$fixture/data/monday/spool/binance-lob-recovery/usdm" -mindepth 1 -maxdepth 1 -type d -name '*.ready' | head -n 1)
  running_dir="${ready_dir%.ready}.running"
  mv "$ready_dir" "$running_dir"
  job_id=$(jq -r '.job_id' "$running_dir/job.json")
  jq -n '{result:"passed"}' >"$running_dir/result.json"
  run_action "$fixture" drain usdm
  evidence_dir="$fixture/data/monday/evidence/recoveries/lob-queue/$job_id"
  [[ -d $evidence_dir/spool.done ]] || exit 1
  assert no-binary-call test ! -s "$fixture/binary.log"
}

test_no_part_noop
test_isolate_creates_ready_job
test_drain_runs_recover_then_upload
test_drain_does_not_depend_on_current_production_symlink
test_failed_job_is_not_retried
test_unsafe_release_identity_fails_closed
test_missing_canonical_lock_fails_closed
test_symlinked_queue_root_fails_closed
test_missing_canonical_recovers_only_with_valid_queue_job
test_missing_canonical_without_valid_queue_job_fails_closed
test_passed_running_finalizes_without_retry

printf 'ok\n'
