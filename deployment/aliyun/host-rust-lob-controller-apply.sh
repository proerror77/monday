#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf 'Usage: %s <controller-release-manifest-sha256> <artifact-sha256>\n' \
    "${0##*/}" >&2
}

configure_paths() {
  local root=${1%/}
  ARTIFACT_RELEASE_ROOT="$root/opt/monday/releases/binance-lob-archiver"
  CONTROLLER_RELEASE_ROOT="$root/opt/monday/releases/binance-lob-controller"
  ACTIVE_CONTROLLER="$CONTROLLER_RELEASE_ROOT/active"
  PRODUCTION_BINARY="$root/opt/monday/bin/binance-lob-archiver"
  INSTALLED_RECOVERY="$root/opt/monday/bin/monday-rust-lob-recovery-queue"
  SYSTEMD_ROOT="$root/etc/systemd/system"
  CONFIG_ROOT="$root/etc/monday"
  PROC_ROOT="$root/proc"
  LOCK_ROOT="$root/run/lock"
  EVIDENCE_ROOT="$root/data/monday/evidence/controller-applies"
  DATA_ROOT="$root/data"
  EXPECTED_ROOT_UID=${MONDAY_CONTROLLER_APPLY_ROOT_UID:-0}
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

secure_regular_file() {
  local path=$1 owner mode
  [[ -f $path && ! -L $path ]] || return 1
  owner=$(stat -c %u -- "$path") || return 1
  mode=$(stat -c %a -- "$path") || return 1
  [[ $owner == "$EXPECTED_ROOT_UID" ]] || return 1
  (( (8#$mode & 022) == 0 ))
}

atomic_install() {
  local mode=$1 source=$2 destination=$3 temporary
  temporary="${destination}.new.$$"
  install -m "$mode" "$source" "$temporary" \
    || { rm -f -- "$temporary"; return 1; }
  mv -Tf "$temporary" "$destination" \
    || { rm -f -- "$temporary"; return 1; }
  cmp -s -- "$source" "$destination"
}

atomic_symlink() {
  local target=$1 link=$2 temporary
  temporary="${link}.new.$$"
  rm -f -- "$temporary"
  ln -s "$target" "$temporary" || return 1
  mv -Tf "$temporary" "$link" \
    || { rm -f -- "$temporary"; return 1; }
  [[ -L $link && $(readlink -f -- "$link") == "$target" ]]
}

installed_asset_path() {
  case "$1" in
    binance-lob-archiver-production@.service|binance-lob-archiver-upload@.service|\
    binance-lob-archiver-recovery@.service|binance-lob-archiver-recovery@.timer)
      printf '%s/%s\n' "$SYSTEMD_ROOT" "$1"
      ;;
    binance-lob-archiver-production-spot.env|binance-lob-archiver-production-usdm.env)
      printf '%s/%s\n' "$CONFIG_ROOT" "$1"
      ;;
    monday-collector-health.sh)
      printf '%s/monday-collector-health.sh\n' "${INSTALLED_RECOVERY%/*}"
      ;;
    *) return 1 ;;
  esac
}

verify_unchanged_installed_assets() {
  local deployment=$1 asset installed
  local -a assets=(
    binance-lob-archiver-production@.service
    binance-lob-archiver-upload@.service
    binance-lob-archiver-production-spot.env
    binance-lob-archiver-production-usdm.env
    binance-lob-archiver-recovery@.service
    binance-lob-archiver-recovery@.timer
    monday-collector-health.sh
  )
  for asset in "${assets[@]}"; do
    installed=$(installed_asset_path "$asset") || return 1
    if ! secure_regular_file "$deployment/$asset" \
      || ! secure_regular_file "$installed" \
      || ! cmp -s -- "$deployment/$asset" "$installed"; then
      return 1
    fi
  done
}

capture_production_state() {
  local artifact_sha=$1 market unit pid restarts exe_sha
  for market in spot usdm; do
    unit="binance-lob-archiver-production@$market.service"
    systemctl is-active --quiet "$unit" || return 1
    systemctl is-enabled --quiet "$unit" || return 1
    pid=$(systemctl show "$unit" --property=MainPID --value) || return 1
    restarts=$(systemctl show "$unit" --property=NRestarts --value) || return 1
    [[ $pid =~ ^[1-9][0-9]*$ && $restarts =~ ^[0-9]+$ ]] || return 1
    [[ -e $PROC_ROOT/$pid/exe || -L $PROC_ROOT/$pid/exe ]] || return 1
    exe_sha=$(sha256sum "$PROC_ROOT/$pid/exe" | awk '{print $1}') || return 1
    [[ $exe_sha == "$artifact_sha" ]] || return 1
    printf '%s\t%s\t%s\t%s\n' "$unit" "$pid" "$restarts" "$exe_sha"
  done
}

timer_state() {
  local market=$1 unit active enabled
  unit="binance-lob-archiver-recovery@$market.timer"
  if systemctl is-active --quiet "$unit"; then active=active; else active=inactive; fi
  enabled=$(systemctl is-enabled "$unit" 2>/dev/null || true)
  [[ $enabled == enabled || $enabled == disabled ]] || return 1
  printf '%s\t%s\t%s\n' "$unit" "$active" "$enabled"
}

write_receipt() {
  local result=$1 rollback=$2 before_sha=$3 after_sha=$4 before_state=$5 after_state=$6
  local marker temporary="$EVIDENCE_DIR/apply.json.tmp.$$"
  case "$result" in
    passed) marker=APPLIED.sha256 ;;
    failed) marker=FAILED.sha256 ;;
    *) return 2 ;;
  esac
  jq -n \
    --arg result "$result" \
    --arg rollback "$rollback" \
    --arg controller_release_manifest_sha256 "$CONTROLLER_SHA256" \
    --arg artifact_sha256 "$ARTIFACT_SHA256" \
    --arg deployment_bundle_sha256 "$DEPLOYMENT_BUNDLE_SHA256" \
    --arg deployment_source_revision "$DEPLOYMENT_SOURCE_REVISION" \
    --arg before_sha256 "$before_sha" \
    --arg after_sha256 "$after_sha" \
    --rawfile production_before "$before_state" \
    --rawfile production_after "$after_state" '
      {schema:"monday.rust_lob_controller_apply.v1",result:$result,
       rollback_result:$rollback,
       controller_release_manifest_sha256:$controller_release_manifest_sha256,
       artifact_sha256:$artifact_sha256,
       deployment_bundle_sha256:$deployment_bundle_sha256,
       deployment_source_revision:$deployment_source_revision,
       applied_asset:"host-rust-lob-recovery-queue.sh",
       before_sha256:$before_sha256,after_sha256:$after_sha256,
       production_state_before:$production_before,
       production_state_after:$production_after}' >"$temporary" || return 1
  chmod 0440 "$temporary"
  mv -Tf "$temporary" "$EVIDENCE_DIR/apply.json" || return 1
  (cd "$EVIDENCE_DIR" && sha256sum apply.json >"$marker") || return 1
  chmod 0440 "$EVIDENCE_DIR/$marker"
}

apply_controller_release() (
  [[ $# -eq 3 ]] || return 2
  local root=$1
  CONTROLLER_SHA256=$2
  ARTIFACT_SHA256=$3
  local controller_release deployment manifest artifact_release artifact_metadata
  local artifact_binary runtime_contract active_runtime source before_sha after_sha
  local work_dir before_state after_state timer_before timer_after old_active_target=
  local recovery_backup mutation_started=0 success=0 rollback_result=not-needed

  # Invoked by the EXIT trap below.
  # shellcheck disable=SC2329
  rollback_on_exit() {
    local rc=$? unit active enabled
    trap - EXIT
    set +e
    if (( success == 0 )); then
      if (( mutation_started )); then
        if atomic_install 0755 "$recovery_backup" "$INSTALLED_RECOVERY"; then
          if [[ -n $old_active_target ]]; then
            atomic_symlink "$old_active_target" "$ACTIVE_CONTROLLER" \
              && rollback_result=restored || rollback_result=failed
          else
            rm -f -- "$ACTIVE_CONTROLLER" \
              && rollback_result=restored || rollback_result=failed
          fi
        else
          rollback_result=failed
        fi
      fi
      if [[ $rollback_result != failed && -f ${timer_before:-} ]]; then
        while IFS=$'\t' read -r unit active enabled; do
          if [[ $active == active ]] && ! systemctl start "$unit" >/dev/null; then
            rollback_result=failed
          fi
        done <"$timer_before"
      fi
      if [[ -n ${after_state:-} ]]; then
        capture_production_state "$ARTIFACT_SHA256" >"$after_state" 2>/dev/null || true
      fi
      if [[ -d ${EVIDENCE_DIR:-} && -f ${before_state:-} && -f ${after_state:-} ]]; then
        write_receipt failed "$rollback_result" "${before_sha:-}" "${after_sha:-}" \
          "$before_state" "$after_state" >/dev/null 2>&1 || true
      fi
    fi
    rm -rf "${work_dir:-}"
    exit "$rc"
  }

  [[ $CONTROLLER_SHA256 =~ ^[a-f0-9]{64}$ \
    && $ARTIFACT_SHA256 =~ ^[a-f0-9]{64}$ ]] || die 'invalid release identity'
  configure_paths "$root"
  controller_release="$CONTROLLER_RELEASE_ROOT/$CONTROLLER_SHA256"
  deployment="$controller_release/deployment"
  manifest="$controller_release/release.json"
  artifact_release="$ARTIFACT_RELEASE_ROOT/$ARTIFACT_SHA256"
  artifact_metadata="$artifact_release/release.json"
  artifact_binary="$artifact_release/binance-lob-archiver"
  for path in "$CONTROLLER_RELEASE_ROOT" "$controller_release" "$deployment" \
    "$ARTIFACT_RELEASE_ROOT" "$artifact_release" "$artifact_release/deployment"; do
    direct_directory "$path" || die "release path is missing or indirect: $path"
  done
  for source in "$manifest" "$controller_release/release.json.sha256" \
    "$controller_release/deployment.sha256" "$artifact_metadata" "$artifact_binary" \
    "$deployment/host-rust-lob-recovery-queue.sh" \
    "$deployment/rust-lob-control-plane-lib.sh"; do
    secure_regular_file "$source" || die "release file is missing or insecure: $source"
  done
  [[ $(sha256sum "$manifest" | awk '{print $1}') == "$CONTROLLER_SHA256" ]] \
    || die 'controller release manifest digest mismatch'
  (cd "$controller_release" \
    && sha256sum --check --strict release.json.sha256 >/dev/null \
    && sha256sum --check --strict deployment.sha256 >/dev/null) \
    || die 'controller release checksum verification failed'
  runtime_contract=$(jq -er '.runtime_contract_sha256' "$manifest")
  DEPLOYMENT_BUNDLE_SHA256=$(jq -er '.deployment_bundle_sha256' "$manifest")
  DEPLOYMENT_SOURCE_REVISION=$(jq -er '.deployment_source_revision' "$manifest")
  jq -e \
    --arg artifact "$ARTIFACT_SHA256" \
    --arg runtime "$runtime_contract" '
      .schema == "monday.rust_lob_controller_release.v1"
      and .artifact_sha256 == $artifact
      and .runtime_contract_sha256 == $runtime' "$manifest" >/dev/null \
    || die 'controller release does not bind the active artifact and runtime contract'
  jq -e --arg artifact "$ARTIFACT_SHA256" --arg runtime "$runtime_contract" '
      .artifact_sha256 == $artifact and .runtime_contract_sha256 == $runtime' \
    "$artifact_metadata" >/dev/null \
    || die 'artifact release metadata differs from the controller release'
  [[ -L $PRODUCTION_BINARY \
    && $(readlink -f -- "$PRODUCTION_BINARY") == "$artifact_binary" ]] \
    || die 'production binary does not resolve to the requested artifact release'
  [[ $(sha256sum "$artifact_binary" | awk '{print $1}') == "$ARTIFACT_SHA256" ]] \
    || die 'active binary digest mismatch'
  # shellcheck disable=SC1090,SC1091
  . "$deployment/rust-lob-control-plane-lib.sh"
  active_runtime=$(monday_rust_lob_runtime_contract_sha256 "$artifact_release/deployment")
  [[ $active_runtime == "$runtime_contract" \
    && $(monday_rust_lob_runtime_contract_sha256 "$deployment") == "$runtime_contract" ]] \
    || die 'controller release changes the active runtime contract'
  verify_unchanged_installed_assets "$deployment" \
    || die 'controller apply may change only the recovery queue script'
  secure_regular_file "$INSTALLED_RECOVERY" \
    || die 'installed recovery queue script is missing or insecure'

  install -d -m 0755 "$LOCK_ROOT"
  exec 9>"$LOCK_ROOT/monday-rust-lob-release.lock"
  flock -n 9 || die 'another Rust collector release operation holds the host lock'
  exec 8>"$LOCK_ROOT/monday-rust-lob-recovery-drain.lock"
  flock -n 8 || die 'another recovery drain operation holds the host lock'
  exec 7>"$LOCK_ROOT/monday-rust-lob-recovery-queue-spot.lock"
  flock -n 7 || die 'another Spot recovery operation holds the host lock'
  exec 6>"$LOCK_ROOT/monday-rust-lob-recovery-queue-usdm.lock"
  flock -n 6 || die 'another USD-M recovery operation holds the host lock'

  work_dir=$(mktemp -d)
  before_state="$work_dir/production-before.tsv"
  after_state="$work_dir/production-after.tsv"
  timer_before="$work_dir/timers-before.tsv"
  timer_after="$work_dir/timers-after.tsv"
  recovery_backup="$work_dir/monday-rust-lob-recovery-queue"
  trap rollback_on_exit EXIT
  capture_production_state "$ARTIFACT_SHA256" >"$before_state" \
    || die 'production units are not healthy before controller apply'
  for market in spot usdm; do timer_state "$market"; done >"$timer_before" \
    || die 'recovery timer state is unsupported'
  for market in spot usdm; do
    systemctl is-active --quiet "binance-lob-archiver-recovery@$market.service" \
      && die "recovery service is active: $market"
  done
  while IFS=$'\t' read -r unit active enabled; do
    [[ $active != active ]] || systemctl stop "$unit"
  done <"$timer_before"
  for market in spot usdm; do
    systemctl is-active --quiet "binance-lob-archiver-recovery@$market.service" \
      && die "recovery service started during controller apply: $market"
  done

  if [[ -e $ACTIVE_CONTROLLER || -L $ACTIVE_CONTROLLER ]]; then
    old_active_target=$(monday_rust_lob_active_controller_deployment \
      "$CONTROLLER_RELEASE_ROOT" "$ARTIFACT_SHA256" "$runtime_contract") \
      || die 'active controller identity is invalid'
    old_active_target=${old_active_target%/deployment}
  fi
  cp -p "$INSTALLED_RECOVERY" "$recovery_backup"
  before_sha=$(sha256sum "$recovery_backup" | awk '{print $1}')
  install -d -m 0750 "$EVIDENCE_ROOT/$CONTROLLER_SHA256/runs"
  EVIDENCE_DIR="$EVIDENCE_ROOT/$CONTROLLER_SHA256/runs/$(date -u +%Y%m%dT%H%M%SZ)-$$"
  mkdir -m 0750 "$EVIDENCE_DIR" || die 'controller apply evidence directory already exists'
  install -m 0440 "$recovery_backup" "$EVIDENCE_DIR/recovery-queue.before"

  mutation_started=1
  atomic_symlink "$controller_release" "$ACTIVE_CONTROLLER" \
    || die 'could not activate the controller release identity'
  atomic_install 0755 "$deployment/host-rust-lob-recovery-queue.sh" "$INSTALLED_RECOVERY" \
    || die 'could not install the recovery queue controller'
  after_sha=$(sha256sum "$INSTALLED_RECOVERY" | awk '{print $1}')
  cmp -s -- "$deployment/host-rust-lob-recovery-queue.sh" "$INSTALLED_RECOVERY" \
    || die 'installed recovery queue controller failed byte readback'
  [[ $(monday_rust_lob_active_controller_deployment \
      "$CONTROLLER_RELEASE_ROOT" "$ARTIFACT_SHA256" "$runtime_contract") == "$deployment" ]] \
    || die 'active controller identity failed readback'
  verify_unchanged_installed_assets "$deployment" \
    || die 'installed production assets changed during controller apply'
  while IFS=$'\t' read -r unit active enabled; do
    [[ $active != active ]] || systemctl start "$unit"
  done <"$timer_before"
  for market in spot usdm; do timer_state "$market"; done >"$timer_after"
  cmp -s "$timer_before" "$timer_after" || die 'recovery timer state changed'
  capture_production_state "$ARTIFACT_SHA256" >"$after_state" \
    || die 'production units are not healthy after controller apply'
  cmp -s "$before_state" "$after_state" \
    || die 'production PID, restart count, or binary identity changed'
  write_receipt passed not-needed "$before_sha" "$after_sha" "$before_state" "$after_state" \
    || die 'could not write controller apply receipt'
  success=1
  chmod 0550 "$EVIDENCE_DIR"
  trap - EXIT
  rm -rf "$work_dir"
  printf 'controller release applied without collector restart: %s\nEvidence: %s/apply.json\n' \
    "$CONTROLLER_SHA256" "$EVIDENCE_DIR"
)

main() {
  [[ $# -eq 2 ]] || { usage; exit 2; }
  (( EUID == 0 )) || die 'controller apply must run as root'
  configure_paths ''
  if [[ ! -d $DATA_ROOT || -L $DATA_ROOT ]] || ! mountpoint -q "$DATA_ROOT"; then
    die '/data must be a mounted filesystem'
  fi
  apply_controller_release '' \
    "$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]')" \
    "$(printf '%s' "$2" | tr '[:upper:]' '[:lower:]')"
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
