#!/usr/bin/env bash
# shellcheck disable=SC2034,SC2317,SC2329,SC2030,SC2031
# Functions extracted with eval dynamically call these fixture helpers and consume their state.
# The render-failure subshell intentionally isolates its receipt directory.
set -Eeuo pipefail
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
SOURCE="$SCRIPT_DIR/host-bybit-options-cutover.sh"
export PATH="/opt/homebrew/opt/coreutils/libexec/gnubin:$PATH"
fixture=$(mktemp -d); fixture=$(cd "$fixture" && pwd -P)
trap 'rm -rf -- "$fixture"' EXIT
for fn in render_unit atomic_install atomic_symlink install_deployment rendered_unit_sha \
  mask_transition_units production_is_fail_closed bybit_path_sync bybit_immutable_json \
  bybit_file_fingerprint bybit_pid_writes_file bybit_no_writer preflight_active_segments \
  resume_units_inactive resume_failed_state_matches admit_resume_rollback orphan_locations_match_inventory \
  custody_orphans resume_rollback run_candidate_drain env_value_from_unit unit_active_json write_evidence \
  protected_root_directory resume_segment_set_matches segment_artifacts rollback_after_failure \
  copy_health_evidence clear_health_before_restart; do
  eval "$(sed -n "/^$fn() {/,/^}/p" "$SOURCE")"
done
# shellcheck disable=SC1091
. "$SCRIPT_DIR/bybit-options-control-plane-lib.sh"
fail() { printf '%s\n' "$*" >&2; exit 1; }
reject() { local label=$1; shift; if ("$@") >/dev/null 2>&1; then fail "accepted $label"; fi; }
secure_regular_file() { [[ -f $1 && ! -L $1 ]] || fail 'not regular'; }
path_is_direct_or_absent() { [[ ! -L $1 ]]; }
canonical_spool_paths_safe() { return 0; }
validate_deployment() { return 0; }
chown() { return 0; }
stat() { if [[ $1 == -c && $2 == %u ]]; then printf '0\n'; else command stat "$@"; fi; }
timeout() { [[ $1 == --signal=TERM && $2 == --kill-after=* ]] || return 1; shift 3; "$@"; }
if ! command -v flock >/dev/null; then flock() { return 0; }; fi
UNIT=bybit-options-archiver.service UPLOAD_UNIT=bybit-options-upload.service TIMER=bybit-options-upload.timer
PRODUCTION_UNITS=("$UNIT") UPLOAD_UNITS=("$UPLOAD_UNIT" "$TIMER") TRANSITION_MASK_UNITS=("$UNIT" "$UPLOAD_UNIT" "$TIMER")
DEPLOYMENT_ASSETS=("$UNIT" "$UPLOAD_UNIT" "$TIMER" bybit-options-runtime-health-policy.jq bybit-options-shadow-gate-policy.jq bybit-options-control-plane-lib.sh)
SYSTEMD_DIR="$fixture/etc" PROC_ROOT="$fixture/proc" CANONICAL_SPOOL="$fixture/spool" RELEASE_ROOT="$fixture/releases"
mkdir -p "$SYSTEMD_DIR" "$PROC_ROOT/123/fd" "$PROC_ROOT/123/fdinfo" "$PROC_ROOT/self/fd" "$CANONICAL_SPOOL" "$RELEASE_ROOT"
printf old >"$fixture/old"; printf candidate >"$fixture/candidate"
OLD_SHA256=$(sha256sum "$fixture/old" | awk '{print $1}')
CANDIDATE_SHA256=$(sha256sum "$fixture/candidate" | awk '{print $1}')
OLD_BINARY="$RELEASE_ROOT/$OLD_SHA256/bybit-options-archiver"
CANDIDATE_BINARY="$RELEASE_ROOT/$CANDIDATE_SHA256/bybit-options-archiver"
mkdir -p "${OLD_BINARY%/*}" "${CANDIDATE_BINARY%/*}"
cp "$fixture/old" "$OLD_BINARY"; cp "$fixture/candidate" "$CANDIDATE_BINARY"
PRODUCTION_LINK="$fixture/production"; ln -s "$OLD_BINARY" "$PRODUCTION_LINK"
ln -s "$OLD_BINARY" "$PROC_ROOT/123/exe"
CANDIDATE_DEPLOYMENT="$fixture/candidate-deployment"; mkdir "$CANDIDATE_DEPLOYMENT"
for asset in "${DEPLOYMENT_ASSETS[@]}"; do cp "$SCRIPT_DIR/$asset" "$CANDIDATE_DEPLOYMENT/$asset"; done
active=false enabled=false timer_active=false timer_enabled=false
systemctl() {
  case "$1" in
    show)
      case "$3" in
        --property=MainPID) [[ $2 != "$UNIT" || $active == false ]] && echo 0 || echo 123 ;;
        --property=InvocationID) printf 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\n' ;;
        --property=ActiveState) [[ ( $active == true && $2 == "$UNIT" ) || ( $timer_active == true && $2 == "$TIMER" ) ]] && echo active || echo inactive ;;
        --property=DropInPaths) printf '\n' ;;
        --property=LoadState) [[ -L $SYSTEMD_DIR/$2 && $(readlink "$SYSTEMD_DIR/$2") == /dev/null ]] && echo masked || echo loaded ;;
        *) return 1 ;;
      esac ;;
    is-active) [[ ( $active == true && $3 == "$UNIT" ) || ( $timer_active == true && $3 == "$TIMER" ) ]] ;;
    is-enabled)
      if [[ $2 == --quiet ]]; then [[ ( $3 == "$UNIT" && $enabled == true ) || ( $3 == "$TIMER" && $timer_enabled == true ) ]];
      else [[ -L $SYSTEMD_DIR/$2 ]] && echo masked || echo disabled; fi ;;
    disable) active=false; enabled=false; timer_active=false; timer_enabled=false ;;
    start)
      [[ -f $SYSTEMD_DIR/$2 && ! -L $SYSTEMD_DIR/$2 ]] || return 1
      if [[ $2 == "$UNIT" ]]; then active=true; else timer_active=true; fi ;;
    enable) if [[ $2 == "$UNIT" ]]; then enabled=true; else timer_enabled=true; fi ;;
    daemon-reload|unmask|reset-failed) return 0 ;;
    *) fail "unexpected service write: $*" ;;
  esac
}
# Current .active is admitted by the actual open writable inode, not age/name.
current="$CANONICAL_SPOOL/bybit-options.1790656000000.ndjson.active"
printf current >"$current"; chmod 0640 "$current"
ln -s "$current" "$PROC_ROOT/123/fd/4"; printf 'flags:\t0100001\n' >"$PROC_ROOT/123/fdinfo/4"
active=true
preflight_active_segments
printf orphan >"$CANONICAL_SPOOL/bybit-options.1786441956486.ndjson.active"
reject orphan-prestop preflight_active_segments
rm "$CANONICAL_SPOOL/bybit-options.1786441956486.ndjson.active" "$current" "$PROC_ROOT/123/fd/4"
active=false
# Five historical orphans: two empty, three nonempty. Contents are synthetic;
# only the original names/count/shape model the production incident.
for pair in '1786441956486:' '1786506847482:' '1786928606145:first' '1787000066143:second' '1787481346424:third'; do
  printf '%s' "${pair#*:}" >"$CANONICAL_SPOOL/bybit-options.${pair%%:*}.ndjson.active"
done
chmod 0640 "$CANONICAL_SPOOL"/*.active
: >"$CANONICAL_SPOOL/.bybit-options.lock"
failed_dir="$fixture/failed"; mkdir -p "$failed_dir/rollback-deployment"
OLD_DEPLOYMENT="$failed_dir/rollback-deployment"
for asset in "${DEPLOYMENT_ASSETS[@]}"; do cp "$CANDIDATE_DEPLOYMENT/$asset" "$OLD_DEPLOYMENT/$asset"; done
render_unit "$OLD_DEPLOYMENT/$UNIT" "$fixture/old-unit" "$OLD_BINARY"; mv "$fixture/old-unit" "$OLD_DEPLOYMENT/$UNIT"
render_unit "$OLD_DEPLOYMENT/$UPLOAD_UNIT" "$fixture/old-upload" "$OLD_BINARY"; mv "$fixture/old-upload" "$OLD_DEPLOYMENT/$UPLOAD_UNIT"
(cd "$OLD_DEPLOYMENT" && sha256sum "${DEPLOYMENT_ASSETS[@]}") >"$failed_dir/rollback-deployment.sha256"
RESUME_SNAPSHOT_SHA256=$(sha256sum "$failed_dir/rollback-deployment.sha256" | awk '{print $1}')
DEPLOYMENT_BUNDLE_SHA256=$(printf 'b%.0s' {1..64})
install_deployment "$CANDIDATE_DEPLOYMENT" "$CANDIDATE_BINARY"
jq -n '{failure_count:28,last_error:null,last_error_at:null,last_success_at:1790654342481}' >"$CANONICAL_SPOOL/upload-status.json"
FAILED_RECEIPT="$failed_dir/cutover.json"
# A fixed later timestamp models a failure after the fixture files existed.
jq -n --arg old "$OLD_SHA256" --arg candidate "$CANDIDATE_SHA256" --arg binary "$OLD_BINARY" \
  --arg bundle "$DEPLOYMENT_BUNDLE_SHA256" --arg snapshot "$RESUME_SNAPSHOT_SHA256" \
  '{schema:"monday.bybit_options_cutover.v1",result:"failed",host_mode:"upgrade",
    last_step:"drain-old-production-with-candidate",production_active:false,previous_sha256:$old,
    candidate_sha256:$candidate,current_binary:$binary,deployment_bundle_sha256:$bundle,
    rollback_deployment_manifest_sha256:$snapshot,started_at:"2099-01-01T00:00:00Z",upload_failure_baseline:28,
    bootstrap:{started_at_ms:0,upload_verified:false},rollback_health:{main_pid:"",invocation_id:""}}' >"$FAILED_RECEIPT"
FAILED_RECEIPT_SHA256=$(sha256sum "$FAILED_RECEIPT" | awk '{print $1}')
files='[]'; units='{}'; drops='{}'
for file in "$CANONICAL_SPOOL"/*.active; do
  fp=$(bybit_file_fingerprint "$file"); sha=$(sha256sum "$file" | awk '{print $1}')
  files=$(jq -cnS --argjson all "$files" --argjson fp "$fp" --arg sha "$sha" '$all+[$fp+{sha256:$sha,data_source_revision:null}]')
done
for unit in "${TRANSITION_MASK_UNITS[@]}"; do
  path="$SYSTEMD_DIR/$unit"
  units=$(jq -cnS --argjson all "$units" --arg unit "$unit" --arg path "$path" --arg sha "$(sha256sum "$path" | awk '{print $1}')" \
    --arg mode "$(stat -c %a "$path")" --argjson gid "$(stat -c %g "$path")" '$all+{($unit):{path:$path,sha256:$sha,mode:$mode,uid:0,gid:$gid}}')
  drops=$(jq -cnS --argjson all "$drops" --arg unit "$unit" '$all+{($unit):""}')
done
ORPHAN_INVENTORY="$fixture/inventory.json"
jq -cnS --arg receipt "$FAILED_RECEIPT" --arg sha "$FAILED_RECEIPT_SHA256" --arg spool "$CANONICAL_SPOOL" \
  --arg binary "$OLD_BINARY" --arg status "$(sha256sum "$CANONICAL_SPOOL/upload-status.json" | awk '{print $1}')" \
  --argjson files "$files" --argjson units "$units" --argjson drops "$drops" \
  '{schema:"monday.bybit_orphan_inventory.v1",failed_receipt:$receipt,failed_receipt_sha256:$sha,spool:$spool,
    files:$files,failed_state:{production_link:$binary,upload_status_sha256:$status,unit_fragments:$units,drop_in_paths:$drops,drop_ins:[]}}' >"$ORPHAN_INVENTORY"
ORPHAN_INVENTORY_SHA256=$(sha256sum "$ORPHAN_INVENTORY" | awk '{print $1}')
RECOVERY_REQUEST_ID=repair-1 RECOVERY_REQUEST_ROOT="$failed_dir/rollback-recoveries/repair-1"
CUSTODY_DIR="$RECOVERY_REQUEST_ROOT/custody" EVIDENCE_DIR="$RECOVERY_REQUEST_ROOT/runs/one"
mkdir -p "$EVIDENCE_DIR"
OPERATION=resume-rollback MASK_OWNED_UNITS=' ' MASK_SEQUENCE=0 OLD_MODE=upgrade
STARTED_AT=2026-09-29T05:00:00Z FAILURE_REASON='' RESULT=preflight STEP=preflight SUCCESS=0
ROLLBACK_MAIN_PID='' ROLLBACK_INVOCATION_ID='' ROLLBACK_UPLOAD_WARNING=null ROLLBACK_WARNING_INTERPRETED=false
BOOTSTRAP_STARTED_MS=0 BOOTSTRAP_UPLOAD_VERIFIED=false BOOTSTRAP_DEADLINE=0
ROLLBACK_RESULT=not-needed ROLLBACK_DEPLOYMENT_MANIFEST_SHA256='' DRAIN_ENV_KEYS=() SAFE_PATH=/usr/bin:/bin
require_empty_segment_spool() { [[ -z $(find "$CANONICAL_SPOOL" -name '*.ndjson.active' -print -quit) ]]; }
runuser() { [[ ! -e /dev/fd/7 ]] || fail 'custody FD leaked into uploader'; : >"$fixture/drain-ran"; }
capture_rollback_runtime_identity() {
  [[ ! -e /dev/fd/7 && $active == true && $(readlink -f "$PRODUCTION_LINK") == "$OLD_BINARY" ]] || return 1
  ROLLBACK_MAIN_PID=123 ROLLBACK_INVOCATION_ID=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
}
wait_for_rollback_health() { [[ $active == true ]]; }
runtime_matches_release() { [[ $1 == "$OLD_BINARY" && $active == true ]]; }
health_ready_for_rollback() { bybit_options_upload_status_ready "$CANONICAL_SPOOL/upload-status.json" 28 && [[ $active == true ]]; }
admit_resume_rollback
saved_sha=$FAILED_RECEIPT_SHA256
FAILED_RECEIPT_SHA256=$(printf '0%.0s' {1..64}); reject wrong-receipt admit_resume_rollback; FAILED_RECEIPT_SHA256=$saved_sha
saved_sha=$ORPHAN_INVENTORY_SHA256
ORPHAN_INVENTORY_SHA256=$(printf '0%.0s' {1..64}); reject wrong-inventory admit_resume_rollback; ORPHAN_INVENTORY_SHA256=$saved_sha
printf unknown >"$CANONICAL_SPOOL/unexpected.ndjson"
reject unknown-segment-before-custody admit_resume_rollback
rm "$CANONICAL_SPOOL/unexpected.ndjson"
# Failed render leaves its own .new temporary; it cannot block effective mask.
(
  EVIDENCE_DIR="$RECOVERY_REQUEST_ROOT/runs/render-failure"
  mkdir "$EVIDENCE_DIR"
  chown() { return 1; }
  reject render-failure render_unit "$CANDIDATE_DEPLOYMENT/$UNIT" "$SYSTEMD_DIR/$UNIT" "$CANDIDATE_BINARY"
  mask_transition_units
  [[ $(systemctl show "$UNIT" --property=LoadState --value) == masked ]]
)
rm "$SYSTEMD_DIR/$UNIT.new.$$"
install_deployment "$CANDIDATE_DEPLOYMENT" "$CANDIDATE_BINARY"
# Wrong content/stat fail before moving even the first empty file.
saved_inventory=$ORPHAN_INVENTORY
jq '.files[0].sha256=("0"*64)' "$saved_inventory" >"$fixture/bad-inventory.json"
ORPHAN_INVENTORY="$fixture/bad-inventory.json"; reject content-hash custody_orphans
jq '.files[0].inode += 1' "$saved_inventory" >"$fixture/bad-inventory.json"
reject inode-drift custody_orphans
ORPHAN_INVENTORY=$saved_inventory
# Simulate interruption after an authorized rename, before its move receipt.
mkdir -p "$CUSTODY_DIR"
first=$(jq -r '.files[0].name' "$ORPHAN_INVENTORY")
mv "$CANONICAL_SPOOL/$first" "$CUSTODY_DIR/$first"
# /run masking alone demonstrably leaves the /etc fragment loaded.
[[ $(systemctl show "$UNIT" --property=LoadState --value) == loaded ]]
(resume_rollback)
[[ -e $fixture/drain-ran && -f $SYSTEMD_DIR/$UNIT && ! -L $SYSTEMD_DIR/$UNIT ]]
[[ $(sha256sum "$SYSTEMD_DIR/$UNIT" | awk '{print $1}') == "$(rendered_unit_sha "$OLD_DEPLOYMENT" "$OLD_BINARY" "$UNIT")" ]]
[[ $(sha256sum "$FAILED_RECEIPT" | awk '{print $1}') == "$FAILED_RECEIPT_SHA256" ]]
jq -e '.operation=="resume-rollback" and .result=="restored" and .recovery.data_recovered==false' "$EVIDENCE_DIR/cutover.json" >/dev/null
while IFS= read -r entry; do
  name=$(jq -r .name <<<"$entry")
  [[ ! -e $CANONICAL_SPOOL/$name && -f $CUSTODY_DIR/$name ]]
  [[ $(sha256sum "$CUSTODY_DIR/$name" | awk '{print $1}') == "$(jq -r .sha256 <<<"$entry")" ]]
done < <(jq -c '.files[]' "$ORPHAN_INVENTORY")
completed_sha=$(sha256sum "$RECOVERY_REQUEST_ROOT/completed.json" | awk '{print $1}')
reject repeated-recovery resume_rollback
[[ $(sha256sum "$RECOVERY_REQUEST_ROOT/completed.json" | awk '{print $1}') == "$completed_sha" ]]
printf 'Native Bybit failed-cutover recovery tests passed\n'
