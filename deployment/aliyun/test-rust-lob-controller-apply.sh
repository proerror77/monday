#!/usr/bin/env bash
set -Eeuo pipefail
export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
APPLY="$SCRIPT_DIR/host-rust-lob-controller-apply.sh"
LIB="$SCRIPT_DIR/rust-lob-control-plane-lib.sh"

for command in cmp jq mktemp readlink sha256sum sort; do
  command -v "$command" >/dev/null 2>&1 \
    || { printf 'missing test dependency: %s\n' "$command" >&2; exit 2; }
done

# shellcheck disable=SC1090
. "$APPLY"

tmp_dir=$(readlink -f "$(mktemp -d)")
cleanup() {
  chmod -R u+w "$tmp_dir" 2>/dev/null || true
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

stat() {
  if [[ ${1:-} == -c ]]; then
    local format=$2
    shift 2
    [[ ${1:-} != -- ]] || shift
    case "$format" in
      %u) printf '%s\n' "$(id -u)" ;;
      %a)
        if [[ $(uname -s) == Darwin ]]; then
          /usr/bin/stat -f %Lp "$1"
        else
          /usr/bin/stat -c %a "$1"
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

install() {
  local -a args=()
  while (($#)); do
    case "$1" in
      -o|-g) shift 2 ;;
      *) args+=("$1"); shift ;;
    esac
  done
  command install "${args[@]}"
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
  else
    command sha256sum "${args[@]}"
  fi
}

flock() { return 0; }

systemctl() {
  local action=${1:-} unit=${2:-} property=
  case "$action" in
    is-active)
      [[ $unit != --quiet ]] || unit=${3:-}
      case "$unit" in
        binance-lob-archiver-production@spot.service|\
        binance-lob-archiver-production@usdm.service) return 0 ;;
        binance-lob-archiver-recovery@spot.timer|\
        binance-lob-archiver-recovery@usdm.timer)
          [[ -f $MOCK_STATE/active/$unit ]]
          ;;
        *) return 1 ;;
      esac
      ;;
    is-enabled)
      [[ $unit != --quiet ]] || unit=${3:-}
      case "$unit" in
        binance-lob-archiver-production@spot.service|\
        binance-lob-archiver-production@usdm.service) return 0 ;;
        binance-lob-archiver-recovery@spot.timer|\
        binance-lob-archiver-recovery@usdm.timer) printf 'enabled\n'; return 0 ;;
        *) return 1 ;;
      esac
      ;;
    show)
      shift 2
      while (($#)); do
        case "$1" in
          --property=*) property=${1#*=} ;;
        esac
        shift
      done
      case "$unit:$property" in
        binance-lob-archiver-production@spot.service:MainPID) printf '101\n' ;;
        binance-lob-archiver-production@usdm.service:MainPID) printf '202\n' ;;
        *:NRestarts) printf '0\n' ;;
        *) return 1 ;;
      esac
      ;;
    stop)
      rm -f "$MOCK_STATE/active/$unit"
      ;;
    start)
      if [[ -f $MOCK_STATE/fail-start-once ]]; then
        rm -f "$MOCK_STATE/fail-start-once"
        return 1
      fi
      : >"$MOCK_STATE/active/$unit"
      ;;
    *) return 1 ;;
  esac
}

assets=(
  binance-lob-archiver-production@.service
  binance-lob-archiver-rust@.service
  binance-lob-archiver-upload@.service
  binance-lob-archiver-rust-upload@.service
  binance-lob-archiver-recovery@.service
  binance-lob-archiver-recovery@.timer
  binance-lob-archiver-production-spot.env
  binance-lob-archiver-production-usdm.env
  binance-lob-archiver-rust-spot.env
  binance-lob-archiver-rust-usdm.env
  host-rust-lob-recovery-queue.sh
  monday-collector-health.sh
  rust-lob-control-plane-lib.sh
)

setup_fixture() {
  local fixture=$1 artifact_release controller_staging controller_release bin_dir
  local asset mode artifact_bundle controller_bundle artifact_source controller_source
  configure_paths "$fixture"
  bin_dir=${INSTALLED_RECOVERY%/*}
  MOCK_STATE="$fixture/mock-systemctl"
  mkdir -p "$bin_dir" "$SYSTEMD_ROOT" "$CONFIG_ROOT" "$PROC_ROOT/101" \
    "$PROC_ROOT/202" "$LOCK_ROOT" "$EVIDENCE_ROOT" "$MOCK_STATE/active" \
    "$ARTIFACT_RELEASE_ROOT" "$CONTROLLER_RELEASE_ROOT"
  for unit in binance-lob-archiver-recovery@spot.timer \
    binance-lob-archiver-recovery@usdm.timer; do
    : >"$MOCK_STATE/active/$unit"
  done

  printf '#!/usr/bin/env bash\nexit 0\n' >"$fixture/artifact"
  chmod 0755 "$fixture/artifact"
  ARTIFACT_SHA256=$(sha256sum "$fixture/artifact" | awk '{print $1}')
  artifact_release="$ARTIFACT_RELEASE_ROOT/$ARTIFACT_SHA256"
  mkdir -p "$artifact_release/deployment"
  for asset in "${assets[@]}"; do
    mode=0644
    [[ $asset == *.sh ]] && mode=0755
    install -m "$mode" "$SCRIPT_DIR/$asset" "$artifact_release/deployment/$asset"
  done
  install -m 0755 "$fixture/artifact" "$artifact_release/binance-lob-archiver"
  RUNTIME_CONTRACT_SHA256=$(
    # shellcheck disable=SC1090
    . "$LIB"
    monday_rust_lob_runtime_contract_sha256 "$artifact_release/deployment"
  )
  artifact_bundle=$(printf 'a%.0s' {1..64})
  artifact_source=$(printf 'b%.0s' {1..40})
  jq -n --arg artifact "$ARTIFACT_SHA256" --arg runtime "$RUNTIME_CONTRACT_SHA256" \
    --arg bundle "$artifact_bundle" --arg source "$artifact_source" '
      {artifact_sha256:$artifact,runtime_contract_sha256:$runtime,
       deployment_bundle_sha256:$bundle,deployment_source_revision:$source}' \
    >"$artifact_release/release.json"
  ln -s "$artifact_release/binance-lob-archiver" "$PRODUCTION_BINARY"
  ln -s "$artifact_release/binance-lob-archiver" "$PROC_ROOT/101/exe"
  ln -s "$artifact_release/binance-lob-archiver" "$PROC_ROOT/202/exe"

  controller_staging="$CONTROLLER_RELEASE_ROOT/staging"
  mkdir -p "$controller_staging/deployment"
  for asset in "${assets[@]}"; do
    mode=0644
    [[ $asset == *.sh ]] && mode=0755
    install -m "$mode" "$SCRIPT_DIR/$asset" "$controller_staging/deployment/$asset"
  done
  printf '\n# controller update\n' \
    >>"$controller_staging/deployment/host-rust-lob-recovery-queue.sh"
  controller_bundle=$(printf 'c%.0s' {1..64})
  controller_source=$(printf 'd%.0s' {1..40})
  jq -n --arg artifact "$ARTIFACT_SHA256" --arg runtime "$RUNTIME_CONTRACT_SHA256" \
    --arg bundle "$controller_bundle" --arg source "$controller_source" '
      {schema:"monday.rust_lob_controller_release.v1",
       artifact_sha256:$artifact,runtime_contract_sha256:$runtime,
       deployment_bundle_sha256:$bundle,deployment_source_revision:$source}' \
    >"$controller_staging/release.json"
  CONTROLLER_SHA256=$(sha256sum "$controller_staging/release.json" | awk '{print $1}')
  (
    cd "$controller_staging"
    sha256sum release.json >release.json.sha256
    for asset in deployment/*; do sha256sum "$asset"; done \
      | sort -k2 >deployment.sha256
  )
  controller_release="$CONTROLLER_RELEASE_ROOT/$CONTROLLER_SHA256"
  mv "$controller_staging" "$controller_release"

  for asset in \
    binance-lob-archiver-production@.service \
    binance-lob-archiver-upload@.service \
    binance-lob-archiver-recovery@.service \
    binance-lob-archiver-recovery@.timer; do
    install -m 0644 "$controller_release/deployment/$asset" "$SYSTEMD_ROOT/$asset"
  done
  for asset in binance-lob-archiver-production-spot.env \
    binance-lob-archiver-production-usdm.env; do
    install -m 0644 "$controller_release/deployment/$asset" "$CONFIG_ROOT/$asset"
  done
  install -m 0755 "$controller_release/deployment/monday-collector-health.sh" \
    "$bin_dir/monday-collector-health.sh"
  install -m 0755 "$artifact_release/deployment/host-rust-lob-recovery-queue.sh" \
    "$INSTALLED_RECOVERY"
}

run_apply() {
  MONDAY_CONTROLLER_APPLY_ROOT_UID=$(id -u) \
    apply_controller_release "$1" "$CONTROLLER_SHA256" "$ARTIFACT_SHA256"
}

success_fixture="$tmp_dir/success"
setup_fixture "$success_fixture"
before_pid_state=$(capture_production_state "$ARTIFACT_SHA256")
run_apply "$success_fixture" >"$tmp_dir/success.out"
controller_release="$CONTROLLER_RELEASE_ROOT/$CONTROLLER_SHA256"
[[ $(readlink -f "$ACTIVE_CONTROLLER") == "$controller_release" ]]
cmp -s "$controller_release/deployment/host-rust-lob-recovery-queue.sh" \
  "$INSTALLED_RECOVERY"
[[ $(capture_production_state "$ARTIFACT_SHA256") == "$before_pid_state" ]]
passed_marker=$(find "$EVIDENCE_ROOT/$CONTROLLER_SHA256/runs" \
  -name APPLIED.sha256 -print -quit)
[[ -n $passed_marker ]]
(cd "${passed_marker%/*}" && sha256sum --check --strict APPLIED.sha256 >/dev/null)

rollback_fixture="$tmp_dir/rollback"
setup_fixture "$rollback_fixture"
before_recovery_sha=$(sha256sum "$INSTALLED_RECOVERY" | awk '{print $1}')
: >"$MOCK_STATE/fail-start-once"
if run_apply "$rollback_fixture" >"$tmp_dir/rollback.out" 2>&1; then
  printf 'controller apply unexpectedly passed after timer restart failure\n' >&2
  exit 1
fi
[[ ! -e $ACTIVE_CONTROLLER && ! -L $ACTIVE_CONTROLLER ]]
[[ $(sha256sum "$INSTALLED_RECOVERY" | awk '{print $1}') == "$before_recovery_sha" ]]
for unit in binance-lob-archiver-recovery@spot.timer \
  binance-lob-archiver-recovery@usdm.timer; do
  [[ -f $MOCK_STATE/active/$unit ]]
done
failed_marker=$(find "$EVIDENCE_ROOT/$CONTROLLER_SHA256/runs" \
  -name FAILED.sha256 -print -quit)
[[ -n $failed_marker ]]
jq -e '.result == "failed" and .rollback_result == "restored"' \
  "${failed_marker%/*}/apply.json" >/dev/null

printf 'Rust LOB controller apply tests passed\n'
