#!/usr/bin/env bash
set -Eeuo pipefail
export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
export MONDAY_CONTROL_PLANE_FIXTURE_SENTINEL=monday-v2-fixture
ROOT=$(readlink -f "$(mktemp -d)")
fixture_root=$ROOT
trap 'chmod -R u+w "$ROOT" 2>/dev/null || true; rm -rf "$ROOT"' EXIT
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/host-rust-lob-controller-release.sh"
ROOT=$fixture_root

# Resource Envelope V2 is a single immutable runtime contract: the production
# template and its aggregate slice carry the pair cap, while each sequential
# shadow phase has its own smaller envelope.  Keep these assertions before any
# fixture publication so an old eight-asset/v1 contract fails immediately.
production_slice_asset='system-binance\x2dlob\x2darchiver\x2dproduction.slice'
[[ -f "$SCRIPT_DIR/$production_slice_asset" ]] || {
  printf 'Resource Envelope V2 slice asset is missing\n' >&2
  exit 1
}
[[ $(monday_sha256_file "$SCRIPT_DIR/$production_slice_asset") =~ ^[a-f0-9]{64}$ ]] || {
  printf 'Resource Envelope V2 slice digest is not a canonical SHA-256\n' >&2
  exit 1
}
[[ $(monday_runtime_assets | wc -l | tr -d '[:space:]') == 9 ]] || {
  printf 'Resource Envelope V2 must publish exactly nine runtime assets\n' >&2
  exit 1
}
if grep -Fqx 'Slice=system-binance\x2dlob\x2darchiver\x2dproduction.slice' \
  "$SCRIPT_DIR/binance-lob-archiver-production@.service"; then
  printf 'production template must rely on the automatic aggregate slice\n' >&2
  exit 1
fi
grep -Fqx 'MemoryHigh=3072M' "$SCRIPT_DIR/$production_slice_asset" || {
  printf 'production slice MemoryHigh is not 3072M\n' >&2
  exit 1
}
grep -Fqx 'MemoryMax=3584M' "$SCRIPT_DIR/$production_slice_asset" || {
  printf 'production slice MemoryMax is not 3584M\n' >&2
  exit 1
}
gate_cleanup_trap_line=$(grep -nF 'trap cleanup EXIT;' "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" | head -n1 | cut -d: -f1)
gate_slice_create_line=$(grep -nF "printf '[Slice]\\nMemoryHigh=1280M" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" | head -n1 | cut -d: -f1)
[[ $gate_cleanup_trap_line =~ ^[0-9]+$ && $gate_slice_create_line =~ ^[0-9]+$ \
  && $gate_cleanup_trap_line -lt $gate_slice_create_line ]] || {
  printf 'Gate cleanup trap must precede run-scoped slice creation\n' >&2
  exit 1
}
gate_protect_line=$(grep -nF "chmod 0440 \"\$gate_json\" \"\$run_json\" \"\$passed_marker_tmp\"" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" | head -n1 | cut -d: -f1)
gate_marker_line=$(grep -nF "mv -f -- \"\$passed_marker_tmp\" \"\$passed_marker\"" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" | head -n1 | cut -d: -f1)
gate_close_dir_line=$(grep -nF "chmod 0550 \"\$evidence_dir\"" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" | head -n1 | cut -d: -f1)
gate_publish_dir_line=$(grep -nF "mv -- \"\$evidence_dir\" \"\$final_evidence_dir\"" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" | head -n1 | cut -d: -f1)
[[ $gate_protect_line =~ ^[0-9]+$ && $gate_marker_line =~ ^[0-9]+$ \
  && $gate_close_dir_line =~ ^[0-9]+$ && $gate_publish_dir_line =~ ^[0-9]+$ \
  && $gate_protect_line -lt $gate_marker_line \
  && $gate_marker_line -lt $gate_close_dir_line \
  && $gate_close_dir_line -lt $gate_publish_dir_line ]] || {
  printf 'Gate evidence is not fully protected before atomic directory publication\n' >&2
  exit 1
}

# Recovery authority must cross a durability barrier before active=C1, and
# Gate-bearing success evidence must be durable before that authority is
# removed.  Readback alone proves bytes, not power-loss ordering.
cutover_script="$SCRIPT_DIR/host-rust-lob-cutover.sh"
restore_script="$SCRIPT_DIR/host-rust-lob-restore.sh"
cutover_intent_flush_line=$(grep -nF "could not durably flush cutover recovery intent" "$cutover_script" | cut -d: -f1)
cutover_intent_commit_line=$(grep -nF "could not durably commit cutover recovery intent" "$cutover_script" | cut -d: -f1)
cutover_active_line=$(grep -nF "monday_atomic_symlink \"\$target_release\" \"\$active_link\"" "$cutover_script" | cut -d: -f1)
cutover_active_sync_line=$(grep -nF "could not durably commit active controller switch" "$cutover_script" | cut -d: -f1)
cutover_active_crash_line=$(grep -nF 'MONDAY_CUTOVER_HARD_CRASH_AFTER_ACTIVE:-' "$cutover_script" | cut -d: -f1)
cutover_transition_flush_line=$(grep -nF "could not durably flush transition receipt" "$cutover_script" | cut -d: -f1)
cutover_transition_commit_line=$(grep -nF "could not durably commit transition digest" "$cutover_script" | cut -d: -f1)
cutover_transition_crash_line=$(grep -nF 'MONDAY_CUTOVER_HARD_CRASH_AFTER_TRANSITION_RECEIPT' "$cutover_script" | cut -d: -f1)
cutover_intent_clear_line=$(grep -nF "could not clear committed cutover recovery intent" "$cutover_script" | tail -n1 | cut -d: -f1)
restore_receipt_flush_line=$(grep -nF "could not durably flush restore receipt'" "$restore_script" | cut -d: -f1)
restore_receipt_commit_line=$(grep -nF "could not durably commit restore receipt digest" "$restore_script" | cut -d: -f1)
restore_receipt_crash_line=$(grep -nF 'MONDAY_RESTORE_HARD_CRASH_AFTER_RECEIPT' "$restore_script" | cut -d: -f1)
restore_intent_clear_line=$(grep -nF "could not clear committed restore recovery intent" "$restore_script" | tail -n1 | cut -d: -f1)
[[ $cutover_intent_flush_line =~ ^[0-9]+$ && $cutover_intent_commit_line =~ ^[0-9]+$ \
  && $cutover_active_line =~ ^[0-9]+$ && $cutover_active_sync_line =~ ^[0-9]+$ && $cutover_active_crash_line =~ ^[0-9]+$ \
  && $cutover_transition_flush_line =~ ^[0-9]+$ && $cutover_transition_commit_line =~ ^[0-9]+$ && $cutover_transition_crash_line =~ ^[0-9]+$ \
  && $cutover_intent_clear_line =~ ^[0-9]+$ && $restore_receipt_flush_line =~ ^[0-9]+$ \
  && $restore_receipt_commit_line =~ ^[0-9]+$ && $restore_receipt_crash_line =~ ^[0-9]+$ && $restore_intent_clear_line =~ ^[0-9]+$ \
  && $cutover_intent_flush_line -lt $cutover_intent_commit_line \
  && $cutover_intent_commit_line -lt $cutover_active_line \
  && $cutover_active_line -lt $cutover_active_sync_line \
  && $cutover_active_sync_line -lt $cutover_active_crash_line \
  && $cutover_transition_flush_line -lt $cutover_transition_commit_line \
  && $cutover_transition_commit_line -lt $cutover_transition_crash_line \
  && $cutover_transition_crash_line -lt $cutover_intent_clear_line \
  && $restore_receipt_flush_line -lt $restore_receipt_commit_line \
  && $restore_receipt_commit_line -lt $restore_receipt_crash_line \
  && $restore_receipt_crash_line -lt $restore_intent_clear_line ]] || {
  printf 'cutover/restore evidence can cross a power-loss boundary before its authority is durable\n' >&2
  exit 1
}
cutover_cleanup_source=$(sed -n '/^cleanup() {$/,/^}$/p' "$cutover_script")
if grep -Fq "rm -f -- \"\$recovery_intent\"" <<<"$cutover_cleanup_source"; then
  printf 'failed cutover cleanup can clear durable recovery authority\n' >&2
  exit 1
fi
grep -Fqx 'MemoryHigh=1792M' "$SCRIPT_DIR/binance-lob-archiver-rust@.service" || {
  printf 'shadow MemoryHigh is not 1792M\n' >&2
  exit 1
}
grep -Fqx 'MemoryMax=2048M' "$SCRIPT_DIR/binance-lob-archiver-rust@.service" || {
  printf 'shadow MemoryMax is not 2048M\n' >&2
  exit 1
}
evidence_timeout_seconds=$(sed -n 's/^readonly EVIDENCE_TIMEOUT_SECONDS=//p' \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh")
gate_segment_seconds=$(sed -n 's/^readonly GATE_SEGMENT_SECONDS=//p' \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh")
health_settle_seconds=$(sed -n 's/^readonly HEALTH_SETTLE_SECONDS=//p' \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh")
shadow_runtime_max_seconds=$(sed -n 's/^readonly SHADOW_RUNTIME_MAX_SECONDS=//p' \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh")
shadow_stop_timeout_seconds=$(sed -n 's/^readonly SHADOW_STOP_TIMEOUT_SECONDS=//p' \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh")
upload_drain_timeout_seconds=$(sed -n 's/^readonly UPLOAD_DRAIN_TIMEOUT_SECONDS=//p' \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh")
transient_work_timeout_seconds=$(sed -n 's/^readonly TRANSIENT_WORK_TIMEOUT_SECONDS=//p' \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh")
spot_segment_seconds=$(sed -n 's/^SEGMENT_SECONDS=//p' \
  "$SCRIPT_DIR/binance-lob-archiver-rust-spot.env")
usdm_segment_seconds=$(sed -n 's/^SEGMENT_SECONDS=//p' \
  "$SCRIPT_DIR/binance-lob-archiver-rust-usdm.env")
production_spot_segment_seconds=$(sed -n 's/^SEGMENT_SECONDS=//p' \
  "$SCRIPT_DIR/binance-lob-archiver-production-spot.env")
production_usdm_segment_seconds=$(sed -n 's/^SEGMENT_SECONDS=//p' \
  "$SCRIPT_DIR/binance-lob-archiver-production-usdm.env")
[[ $evidence_timeout_seconds == 600 \
  && $gate_segment_seconds == 120 && $health_settle_seconds == 240 \
  && $shadow_runtime_max_seconds == 900 && $shadow_stop_timeout_seconds == 60 \
  && $upload_drain_timeout_seconds == 300 && $transient_work_timeout_seconds == 300 \
  && $spot_segment_seconds =~ ^[1-9][0-9]*$ \
  && $spot_segment_seconds == "$usdm_segment_seconds" \
  && $production_spot_segment_seconds == 300 \
  && $production_spot_segment_seconds == "$production_usdm_segment_seconds" ]] || {
  printf 'Fast Gate bounds or the 300-second production cadence drifted\n' >&2
  exit 1
}
gate_observation_source=$(sed -n '/^run_market_gate_phase()/,/^}/p' \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh")
grep -Fq "observation_started_ns=\$(monotonic_nanoseconds)" <<<"$gate_observation_source"
grep -Fq "observation_finished_ns=\$(monotonic_nanoseconds)" <<<"$gate_observation_source"

# Resource Envelope V2 reserves the production slice's unallocated aggregate
# cap from parent memory.stat anon.  File cache and memory.current remain audit
# fields and must not reduce the required budget a second time.
admission_available_bytes=6442450944
admission_reserve_bytes=1073741824
admission_phase_max_bytes=1610612736
admission_parent_anon_bytes=317067264
admission_slice_max_bytes=3758096384
admission_required_bytes=$((admission_reserve_bytes + admission_phase_max_bytes + admission_slice_max_bytes - admission_parent_anon_bytes))
[[ $(monday_shadow_memory_admission \
  "$admission_available_bytes" "$admission_reserve_bytes" "$admission_phase_max_bytes" \
  "$admission_parent_anon_bytes" "$admission_slice_max_bytes") \
  == "$admission_required_bytes" ]] || {
  printf 'sequential memory admission rejected the available host headroom\n' >&2
  exit 1
}
[[ $(monday_shadow_memory_admission \
  "$admission_required_bytes" "$admission_reserve_bytes" "$admission_phase_max_bytes" \
  "$admission_parent_anon_bytes" "$admission_slice_max_bytes") \
  == "$admission_required_bytes" ]] || {
  printf 'sequential memory admission rejected exact phase plus reserve\n' >&2
  exit 1
}
if monday_shadow_memory_admission "$((admission_required_bytes - 1))" \
  "$admission_reserve_bytes" "$admission_phase_max_bytes" \
  "$admission_parent_anon_bytes" "$admission_slice_max_bytes" >/dev/null 2>&1; then
  printf 'sequential memory admission accepted one byte below phase plus reserve\n' >&2
  exit 1
fi
if monday_shadow_memory_admission "$admission_available_bytes" \
  "$admission_reserve_bytes" "$admission_phase_max_bytes" \
  "$((admission_slice_max_bytes + 1))" "$admission_slice_max_bytes" >/dev/null 2>&1; then
  printf 'sequential memory admission accepted anon greater than slice limit\n' >&2
  exit 1
fi
if monday_shadow_memory_admission "$admission_available_bytes" \
  "$admission_reserve_bytes" "$admission_phase_max_bytes" \
  "$admission_parent_anon_bytes" >/dev/null 2>&1; then
  printf 'sequential memory admission accepted legacy three-argument shape\n' >&2
  exit 1
fi
if monday_shadow_memory_admission "$admission_available_bytes" \
  "$admission_reserve_bytes" "$admission_phase_max_bytes" \
  "$admission_parent_anon_bytes" "$admission_slice_max_bytes" 1 >/dev/null 2>&1; then
  printf 'sequential memory admission accepted legacy six-argument shape\n' >&2
  exit 1
fi
if monday_shadow_memory_admission 0 1 9223372036854775807 0 9223372036854775807 >/dev/null 2>&1; then
  printf 'sequential memory admission accepted an overflowing reserve plus phase\n' >&2
  exit 1
fi

# Production cgroup snapshots are validated independently of a host systemd
# daemon.  The same fixture exercises the exact slice/parent/child topology,
# active-child set, limits, and process identities used by the Gate.
production_snapshot="$ROOT/production-snapshot.json"
production_exe_sha=$(printf 'a%.0s' {1..64})
jq -n --arg exe "$production_exe_sha" '
  {slice:"system-binance\\x2dlob\\x2darchiver\\x2dproduction.slice",
   parent_control_group:"/system.slice/system-binance\\x2dlob\\x2darchiver\\x2dproduction.slice",parent_cgroup_procs:[],
   active_child_control_groups:["/system.slice/system-binance\\x2dlob\\x2darchiver\\x2dproduction.slice/binance-lob-archiver-production@spot.service",
     "/system.slice/system-binance\\x2dlob\\x2darchiver\\x2dproduction.slice/binance-lob-archiver-production@usdm.service"],
   children:{spot:{market:"spot",slice:"system-binance\\x2dlob\\x2darchiver\\x2dproduction.slice",
       control_group:"/system.slice/system-binance\\x2dlob\\x2darchiver\\x2dproduction.slice/binance-lob-archiver-production@spot.service",
       main_pid:101,process_exe_sha256:$exe,n_restarts:8,active:true,
       systemd_memory_max_bytes:2684354560,memory_max_bytes:2684354560},
     usdm:{market:"usdm",slice:"system-binance\\x2dlob\\x2darchiver\\x2dproduction.slice",
       control_group:"/system.slice/system-binance\\x2dlob\\x2darchiver\\x2dproduction.slice/binance-lob-archiver-production@usdm.service",
       main_pid:102,process_exe_sha256:$exe,n_restarts:8,active:true,
       systemd_memory_max_bytes:2684354560,memory_max_bytes:2684354560}},
   production_envelope_state:"signed",
   production_slice_memory_high_bytes:3221225472,production_slice_memory_max_bytes:3758096384,
   systemd_production_slice_memory_high_bytes:3221225472,systemd_production_slice_memory_max_bytes:3758096384,
   target_production_slice_memory_high_bytes:3221225472,target_production_slice_memory_max_bytes:3758096384,
   parent_memory_current_bytes:1101067264,parent_memory_peak_bytes:5100000000,
   parent_memory_anon_bytes:317067264,parent_memory_file_bytes:784000000,
   parent_memory_stat:{anon:317067264,file:784000000},
   child_memory_max_sum_bytes:5368709120,parent_memory_events:{high:0,oom:0,oom_kill:0}}
' >"$production_snapshot"
monday_validate_lob_production_snapshot "$production_snapshot"
production_identity=$(monday_lob_production_snapshot_identity "$production_snapshot")
direct_production_snapshot="$ROOT/direct-production-snapshot.json"
jq '.production_envelope_state = "legacy-unlimited"
  | .production_slice_memory_high_bytes = null
  | .production_slice_memory_max_bytes = null
  | .systemd_production_slice_memory_high_bytes = null
  | .systemd_production_slice_memory_max_bytes = null' \
  "$production_snapshot" >"$direct_production_snapshot"
monday_validate_lob_production_snapshot "$direct_production_snapshot" direct
if monday_validate_lob_production_snapshot "$direct_production_snapshot" stable; then
  printf 'stable production snapshot validation accepted a legacy unlimited envelope\n' >&2
  exit 1
fi

# Runtime-boundary verification reads unordered KEY=VALUE output from
# systemctl and requires the permanent aggregate slice plus both direct child
# units.  This small stub keeps the helper covered without mutating a host.
# Invoked indirectly by the sourced helper.
# shellcheck disable=SC2317,SC2329
systemctl() {
  local action=${1:-} unit=${2:-}
  [[ $action == show ]] || return 1
  if [[ $unit == 'system-binance\x2dlob\x2darchiver\x2dproduction.slice' ]]; then
    printf '%s\n' \
      'ControlGroup=/system.slice/system-binance\x2dlob\x2darchiver\x2dproduction.slice' \
      'MemoryMax=3758096384' 'MemoryHigh=3221225472'
  elif [[ $unit == 'binance-lob-archiver-production@spot.service' ]]; then
    printf '%s\n' \
      'MemoryMax=2684354560' \
      'ControlGroup=/system.slice/system-binance\x2dlob\x2darchiver\x2dproduction.slice/binance-lob-archiver-production@spot.service' \
      'Slice=system-binance\x2dlob\x2darchiver\x2dproduction.slice'
  elif [[ $unit == 'binance-lob-archiver-production@usdm.service' ]]; then
    printf '%s\n' \
      'Slice=system-binance\x2dlob\x2darchiver\x2dproduction.slice' \
      'ControlGroup=/system.slice/system-binance\x2dlob\x2darchiver\x2dproduction.slice/binance-lob-archiver-production@usdm.service' \
      'MemoryMax=2684354560'
  else
    return 1
  fi
}
monday_rust_lob_verify_systemd_production_slice "$ROOT"
unset -f systemctl
mutated_snapshot="$ROOT/production-snapshot-mutated.json"
for mutation in extra-child non-direct wrong-limit identity; do
  case "$mutation" in
    extra-child)
      jq '.active_child_control_groups += ["/system.slice/foreign.service"]' "$production_snapshot" >"$mutated_snapshot" ;;
    non-direct)
      jq '.children.spot.control_group = "/other.slice/binance-lob-archiver-production@spot.service"' \
        "$production_snapshot" >"$mutated_snapshot" ;;
    wrong-limit)
      jq '.children.usdm.memory_max_bytes = 2147483648 | .child_memory_max_sum_bytes = 4831838208' \
        "$production_snapshot" >"$mutated_snapshot" ;;
    identity)
      jq '.children.spot.main_pid = 99999 | .children.spot.n_restarts = 99' \
        "$production_snapshot" >"$mutated_snapshot" ;;
  esac
  if [[ $mutation == identity ]]; then
    monday_validate_lob_production_snapshot "$mutated_snapshot"
    mutated_identity=$(monday_lob_production_snapshot_identity "$mutated_snapshot")
    [[ $mutated_identity == "$production_identity" ]] || {
      printf 'production snapshot identity changed after PID/restart-only drift\n' >&2
      exit 1
    }
  elif monday_validate_lob_production_snapshot "$mutated_snapshot"; then
    printf 'production snapshot validator accepted %s mutation\n' "$mutation" >&2
    exit 1
  fi
done

# PID and restart counters are volatile audit fields, not stable identity.
# Executable and cgroup changes remain identity drift even when the snapshot
# is otherwise structurally valid.
identity_exe_snapshot="$ROOT/production-snapshot-identity-exe.json"
identity_exe_sha=$(printf 'b%.0s' {1..64})
jq --arg exe "$identity_exe_sha" \
  '.children.spot.process_exe_sha256 = $exe' "$production_snapshot" \
  >"$identity_exe_snapshot"
[[ $(monday_lob_production_snapshot_identity "$identity_exe_snapshot") != "$production_identity" ]] || {
  printf 'production snapshot identity ignored executable drift\n' >&2
  exit 1
}
identity_cgroup_snapshot="$ROOT/production-snapshot-identity-cgroup.json"
jq '.children.spot.control_group += "-changed"' "$production_snapshot" \
  >"$identity_cgroup_snapshot"
[[ $(monday_lob_production_snapshot_identity "$identity_cgroup_snapshot") != "$production_identity" ]] || {
  printf 'production snapshot identity ignored cgroup drift\n' >&2
  exit 1
}

jq '.parent_cgroup_procs=[4242]' "$production_snapshot" >"$mutated_snapshot"
if monday_validate_lob_production_snapshot "$mutated_snapshot"; then
  printf 'production snapshot validator accepted a non-empty parent cgroup\n' >&2
  exit 1
fi

assets=()
source_dir="$ROOT/source"
mkdir -p "$source_dir"
while IFS= read -r asset; do
  assets+=("$asset")
  cp "$SCRIPT_DIR/$asset" "$source_dir/$asset"
done < <({ monday_runtime_assets; monday_controller_assets; } | sort -u)

publish_fixture() {
  local payload=$1 manifest=$2
  local payload_sha runtime_sha bundle bundle_sha
  printf '#!/usr/bin/env bash\n# %s\nexit 0\n' "$payload" >"$payload"
  chmod 0755 "$payload"
  payload_sha=$(monday_sha256_file "$payload")
  runtime_sha=$(monday_rust_lob_runtime_contract_sha256 "$source_dir")
  bundle="$payload.tar"
  COPYFILE_DISABLE=1 tar -C "$source_dir" -cf "$bundle" "${assets[@]}"
  bundle_sha=$(monday_sha256_file "$bundle")
  jq -cS -n --arg uri oss://bucket/payload --arg sha "$payload_sha" \
    --arg runtime "$runtime_sha" --arg source "$(printf 'a%.0s' {1..40})" \
    --arg bundle oss://bucket/controller --arg bundle_sha "$bundle_sha" \
    '{schema:"monday.rust_lob_controller_release.v2",control_plane_version:2,
      topology:"stable",artifact_uri:$uri,artifact_sha256:$sha,
      runtime_contract_sha256:$runtime,deployment_source_revision:$source,
      deployment_bundle_uri:$bundle,deployment_bundle_sha256:$bundle_sha}' >"$manifest"
  publish_controller_release "$payload" "$bundle" "$manifest" "$ROOT" >/dev/null
  rm -f "$bundle"
  printf '%s\n' "$payload_sha"
}

mkdir -p "$ROOT/opt/monday/bin"
p0="$ROOT/p0"; m0="$ROOT/m0.json"
p0_sha=$(publish_fixture "$p0" "$m0")
mkdir -p "$ROOT/etc/systemd/system" "$ROOT/etc/monday"
for asset in "$production_slice_asset" binance-lob-archiver-production@.service binance-lob-archiver-upload@.service; do
  cp "$ROOT/opt/monday/releases/binance-lob-controller/$(monday_sha256_file "$m0")/deployment/$asset" \
    "$ROOT/etc/systemd/system/$asset"
done
for asset in binance-lob-archiver-production-spot.env binance-lob-archiver-production-usdm.env; do
  cp "$ROOT/opt/monday/releases/binance-lob-controller/$(monday_sha256_file "$m0")/deployment/$asset" \
    "$ROOT/etc/monday/$asset"
done
# Bootstrap independently verifies all nine runtime unit/env bytes (the
# production and shadow lanes) before establishing stable projections.
for asset in binance-lob-archiver-rust@.service binance-lob-archiver-rust-upload@.service; do
  cp "$ROOT/opt/monday/releases/binance-lob-controller/$(monday_sha256_file "$m0")/deployment/$asset" \
    "$ROOT/etc/systemd/system/$asset"
done
for asset in binance-lob-archiver-rust-spot.env binance-lob-archiver-rust-usdm.env; do
  cp "$ROOT/opt/monday/releases/binance-lob-controller/$(monday_sha256_file "$m0")/deployment/$asset" \
    "$ROOT/etc/monday/$asset"
done
# The fixture starts from the historical eight-asset runtime.  The candidate
# release carries the ninth signed aggregate-slice asset, but the legacy live
# topology deliberately does not; this is the only direct R0 -> R2 delta.
rm -f -- "$ROOT/etc/systemd/system/$production_slice_asset"
# Production upload-status is a sentinel: every shadow Gate/drain must leave
# the governed production spool untouched.
production_spool_root="$ROOT/data/monday/spool/binance-lob"
mkdir -p "$production_spool_root/spot" "$production_spool_root/usdm"
printf 'production-sentinel-spot\n' >"$production_spool_root/spot/upload-status.json"
printf 'production-sentinel-usdm\n' >"$production_spool_root/usdm/upload-status.json"
mkdir -p "$ROOT/run/lock"
: >"$ROOT/run/lock/monday-rust-lob-control-plane.lock"
production_spot_status_sha=$(monday_sha256_file "$production_spool_root/spot/upload-status.json")
production_usdm_status_sha=$(monday_sha256_file "$production_spool_root/usdm/upload-status.json")
printf '\n# controller revision two fixture\n' >>"$source_dir/host-rust-lob-readback.sh"
p1="$ROOT/p1"; m1="$ROOT/m1.json"
p1_sha=$(publish_fixture "$p1" "$m1")
c0=$(monday_sha256_file "$m0")
c1=$(monday_sha256_file "$m1")

# The real bootstrap starts from the immutable controller identity created by
# the pre-V2 apply path.  It is read-only evidence: no v1 control byte is
# sourced or executed during the V2 Gate/cutover.
legacy_root="$ROOT/opt/monday/releases/binance-lob-controller"
legacy_work="$ROOT/legacy-controller"
mkdir -p "$legacy_work/deployment"
legacy_artifact_uri=oss://bucket/payload
legacy_bundle_uri=oss://bucket/legacy-controller
legacy_source=$(printf '9%.0s' {1..40})
legacy_bundle_sha=$(monday_sha256_file "$p0")
legacy_runtime_sha=$(monday_rust_lob_runtime_contract_sha256_v1 "$source_dir")
candidate_runtime_sha=$(monday_manifest_field "$m0" runtime_contract_sha256)
[[ $legacy_runtime_sha != "$candidate_runtime_sha" ]] || {
  printf 'typed runtime migration fixture did not produce distinct R0/R2 identities\n' >&2
  exit 1
}
jq -cS -n --arg artifact_uri "$legacy_artifact_uri" --arg artifact_sha "$p0_sha" \
  --arg runtime "$legacy_runtime_sha" \
  --arg source "$legacy_source" --arg bundle "$legacy_bundle_uri" --arg bundle_sha "$legacy_bundle_sha" \
  '{schema:("monday.rust_lob_controller_release." + "v1"),artifact_uri:$artifact_uri,
    artifact_sha256:$artifact_sha,runtime_contract_sha256:$runtime,
    deployment_source_revision:$source,deployment_bundle_uri:$bundle,
    deployment_bundle_sha256:$bundle_sha}' >"$legacy_work/release.json"
legacy_c0=$(monday_sha256_file "$legacy_work/release.json")
for asset in host-rust-lob-recovery-queue.sh monday-collector-health.sh; do
  cp -p -- "$source_dir/$asset" "$legacy_work/deployment/$asset"
  # Deliberately make the legacy C0 helper bytes differ from the V2 C1
  # projection.  A crash after active=C1 must therefore replace these regular
  # legacy files from the verified active controller, rather than silently
  # accepting whichever bytes happened to be left on disk.
  printf '\n# legacy C0 helper projection fixture\n' >>"$legacy_work/deployment/$asset"
done
mkdir -p "$legacy_root/$legacy_c0/deployment"
cp -p -- "$legacy_work/release.json" "$legacy_root/$legacy_c0/release.json"
cp -p -- "$legacy_work/deployment/"* "$legacy_root/$legacy_c0/deployment/"
(cd "$legacy_root/$legacy_c0" && sha256sum release.json >release.json.sha256 && sha256sum deployment/* >deployment.sha256)
ln -s "$legacy_root/$legacy_c0" "$legacy_root/active"
cp -p -- "$legacy_work/deployment/host-rust-lob-recovery-queue.sh" \
  "$ROOT/opt/monday/bin/monday-rust-lob-recovery-queue"
cp -p -- "$legacy_work/deployment/monday-collector-health.sh" \
  "$ROOT/opt/monday/bin/monday-collector-health.sh"

# Bootstrap uses an explicit direct before topology.  The live legacy P0/R0
# stays frozen until Cutover while the candidate may carry a new P1/R1.
legacy_production_projection="../releases/binance-lob-archiver/$p0_sha/binance-lob-archiver"
ln -s "$legacy_production_projection" "$ROOT/opt/monday/bin/binance-lob-archiver"
# Live runtime digesting is read-only: even with a nonexistent TMPDIR, the
# canonical v1 asset order and digest must match the fixture's recorded R0.
runtime_tmpdir_guard="$ROOT/nonexistent-runtime-tmp"
live_runtime_without_tmp=$(TMPDIR="$runtime_tmpdir_guard" \
  monday_rust_lob_live_runtime_contract_sha256_v1 "$ROOT")
[[ $live_runtime_without_tmp == "$legacy_runtime_sha" && ! -e $runtime_tmpdir_guard ]] || {
  printf 'live runtime digest helper changed identity or created TMPDIR\n' >&2
  exit 1
}
legacy_production_service="$ROOT/etc/systemd/system/binance-lob-archiver-production@.service"
cp -p -- "$legacy_production_service" "$legacy_production_service.before-delta"
chmod u+w "$legacy_production_service"
printf '\n# illegal legacy runtime delta fixture\n' >>"$legacy_production_service"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" \
  --from-controller direct --candidate-controller "$c0" --root "$ROOT" >/dev/null 2>&1; then
  printf 'Gate accepted an illegal legacy runtime delta\n' >&2
  exit 1
fi
mv -f -- "$legacy_production_service.before-delta" "$legacy_production_service"
# A read-only host preflight must validate the same C/from/P/R and installed
# production bytes as the formal Gate, then emit advisory JSON without creating
# or truncating the existing lock, run spool, evidence, worker slice, lease,
# shadow, or systemd unit.
preflight_residue_before=$(find "$ROOT/data/monday/spool/binance-lob-rust-shadow" \
  "$ROOT/data/monday/evidence/shadow-gates" "$ROOT/run/monday/rust-lob-gate" \
  "$ROOT/run/systemd/system" -mindepth 1 -print 2>/dev/null | LC_ALL=C sort || true)
preflight_lock_path="$ROOT/run/lock/monday-rust-lob-control-plane.lock"
preflight_lock_sha=$(monday_sha256_file "$preflight_lock_path")
preflight_tmpdir_guard="$ROOT/nonexistent-preflight-tmp"
rm -rf -- "$preflight_tmpdir_guard"
rm -f -- "$ROOT/run/gate-fixture.calls"
subminute_env="$source_dir/binance-lob-archiver-rust-spot.env"
cp -p -- "$subminute_env" "$subminute_env.before-test"
sed 's/^SEGMENT_SECONDS=.*/SEGMENT_SECONDS=59/' "$subminute_env.before-test" >"$subminute_env"
subminute_manifest="$ROOT/subminute-manifest.json"
publish_fixture "$p0" "$subminute_manifest" >/dev/null
subminute_controller=$(monday_sha256_file "$subminute_manifest")
mv -f -- "$subminute_env.before-test" "$subminute_env"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$subminute_controller" --preflight-only --root "$ROOT" \
  >"$ROOT/run/preflight-subminute-segment.out" 2>&1; then
  printf 'read-only preflight accepted a sub-minute segment cadence\n' >&2
  exit 1
fi
grep -Fq 'spot shadow SEGMENT_SECONDS is below the collector minimum' \
  "$ROOT/run/preflight-subminute-segment.out" || {
  cat "$ROOT/run/preflight-subminute-segment.out" >&2
  exit 1
}
preflight_output=$(TMPDIR="$preflight_tmpdir_guard" MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --preflight-only --root "$ROOT")
jq -e --arg runtime "$candidate_runtime_sha" \
  '.schema == "monday.rust_lob_shadow_gate_preflight.v1"
   and .operation == "gate" and .preflight_only == true
   and .authoritative == false and .production_changed == false
   and .authorizes_gate == false and .authorizes_cutover == false
   and .candidate_runtime_contract_sha256 == $runtime
   and .production_cgroup_snapshot.production_envelope_state == "legacy-unlimited"
   and .production_cgroup_snapshot.production_slice_memory_high_bytes == null
   and .production_cgroup_snapshot.production_slice_memory_max_bytes == null
   and .production_cgroup_snapshot.target_production_slice_memory_high_bytes == 3221225472
   and .production_cgroup_snapshot.target_production_slice_memory_max_bytes == 3758096384
   and (.io_full_psi_windows | length == 1)
   and .io_full_psi_windows[0].phase == "preflight"
   and .io_full_psi_windows[0].veto == false
   and (.checks.controller and .checks.from_controller and .checks.payload
     and .checks.runtime_contract and .checks.installed_bytes
     and .checks.production_cgroup and .checks.psi_sampler)' \
  <<<"$preflight_output" >/dev/null
# A direct v1 -> V2 migration must be able to Gate a new payload while the
# immutable legacy payload keeps running until Cutover.
[[ $p0_sha != "$p1_sha" ]]
if ! payload_delta_preflight=$(TMPDIR="$preflight_tmpdir_guard" \
  MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c1" --preflight-only --root "$ROOT" 2>&1); then
  printf '%s\n' "$payload_delta_preflight" >&2
  printf 'read-only preflight rejected a direct payload transition\n' >&2
  exit 1
fi
jq -e --arg from "$legacy_c0" --arg before "$p0_sha" --arg candidate "$p1_sha" \
  '.from_controller_sha256 == $from
   and .candidate_payload_sha256 == $candidate
   and all(.production_cgroup_snapshot.children[];
     .process_exe_sha256 == $before)
   and .production_changed == false' <<<"$payload_delta_preflight" >/dev/null
[[ ! -e "$preflight_tmpdir_guard" ]] || {
  printf 'read-only preflight created a temporary directory\n' >&2
  exit 1
}
preflight_residue_after=$(find "$ROOT/data/monday/spool/binance-lob-rust-shadow" \
  "$ROOT/data/monday/evidence/shadow-gates" "$ROOT/run/monday/rust-lob-gate" \
  "$ROOT/run/systemd/system" -mindepth 1 -print 2>/dev/null | LC_ALL=C sort || true)
[[ $preflight_residue_after == "$preflight_residue_before" ]] || {
  printf 'read-only preflight left run-scoped residue\n' >&2
  exit 1
}
[[ $(monday_sha256_file "$preflight_lock_path") == "$preflight_lock_sha" ]] || {
  printf 'read-only preflight changed the existing lock file\n' >&2
  exit 1
}
[[ ! -s "$ROOT/run/gate-fixture.calls" ]] || {
  printf 'read-only preflight invoked a mutating systemd action\n' >&2
  exit 1
}
# The read-only preflight must reject the same invalid live production cgroup
# topology as the formal Gate.
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_EXTRA_CHILD=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --preflight-only --root "$ROOT" \
  >"$ROOT/run/preflight-extra-cgroup-child.err" 2>&1; then
  printf 'read-only preflight accepted an extra active production cgroup child\n' >&2
  exit 1
fi
grep -Fq 'production cgroup snapshot is invalid' \
  "$ROOT/run/preflight-extra-cgroup-child.err" || {
  cat "$ROOT/run/preflight-extra-cgroup-child.err" >&2
  exit 1
}
rm -rf -- "$ROOT/sys/fs/cgroup/system.slice/$production_slice_asset/foreign.service"
# A direct v1 -> V2 Gate must not require C1's runtime bytes to be installed
# before Cutover. Bind the live eight-asset topology to a distinct immutable
# C0 runtime, then prove the 300-second C1 still passes read-only preflight.
runtime_delta_envs=(
  "$ROOT/etc/monday/binance-lob-archiver-production-spot.env"
  "$ROOT/etc/monday/binance-lob-archiver-production-usdm.env"
  "$ROOT/etc/monday/binance-lob-archiver-rust-spot.env"
  "$ROOT/etc/monday/binance-lob-archiver-rust-usdm.env"
)
write_legacy_runtime_delta() {
  local source=$1 target=$2
  case ${target##*/} in
    binance-lob-archiver-production-spot.env)
      sed -e 's/^SEGMENT_SECONDS=.*/SEGMENT_SECONDS=3600/' \
        -e 's|^SPOOL_DIR=.*|SPOOL_DIR=/data/monday/spool/binance-lob-v1/spot|' "$source" >"$target" ;;
    binance-lob-archiver-production-usdm.env)
      sed -e 's/^SEGMENT_SECONDS=.*/SEGMENT_SECONDS=3600/' \
        -e 's|^SPOOL_DIR=.*|SPOOL_DIR=/data/monday/spool/binance-lob-v1/usdm|' "$source" >"$target" ;;
    *) sed 's/^SEGMENT_SECONDS=.*/SEGMENT_SECONDS=3600/' "$source" >"$target" ;;
  esac
}
for runtime_env in "${runtime_delta_envs[@]}"; do
  cp -p -- "$runtime_env" "$runtime_env.before-direct-delta"
  chmod u+w "$runtime_env"
  write_legacy_runtime_delta "$runtime_env.before-direct-delta" "$runtime_env"
done
legacy_delta_runtime=$(monday_rust_lob_live_runtime_contract_sha256_v1 "$ROOT")
[[ $legacy_delta_runtime != "$candidate_runtime_sha" ]]
legacy_delta_manifest="$ROOT/legacy-runtime-delta.json"
jq --arg runtime "$legacy_delta_runtime" --arg source "$(printf '7%.0s' {1..40})" \
  '.runtime_contract_sha256 = $runtime | .deployment_source_revision = $source' \
  "$legacy_work/release.json" >"$legacy_delta_manifest"
legacy_delta_c0=$(monday_sha256_file "$legacy_delta_manifest")
mkdir -p "$legacy_root/$legacy_delta_c0/deployment"
cp -p -- "$legacy_delta_manifest" "$legacy_root/$legacy_delta_c0/release.json"
(cd "$legacy_root/$legacy_delta_c0" && sha256sum release.json >release.json.sha256)
rm -f -- "$legacy_root/active"
ln -s "$legacy_root/$legacy_delta_c0" "$legacy_root/active"
runtime_delta_preflight=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --preflight-only --root "$ROOT")
jq -e --arg from "$legacy_delta_c0" --arg candidate "$c0" \
  --arg runtime "$candidate_runtime_sha" \
  '.from_controller_sha256 == $from
   and .candidate_controller_sha256 == $candidate
   and .candidate_runtime_contract_sha256 == $runtime
   and .checks.installed_bytes == true
   and .production_changed == false' <<<"$runtime_delta_preflight" >/dev/null
rm -f -- "$legacy_root/active"
ln -s "$legacy_root/$legacy_c0" "$legacy_root/active"
for runtime_env in "${runtime_delta_envs[@]}"; do
  mv -f -- "$runtime_env.before-direct-delta" "$runtime_env"
done
[[ $(monday_rust_lob_live_runtime_contract_sha256_v1 "$ROOT") == "$legacy_runtime_sha" ]]
# An unresolved lease left by the retired controller still blocks this Gate.
legacy_lease="$ROOT/run/monday/rust-lob-gate/bootstrap-slice-lease-20260829T000000Z-1.json"
mkdir -p "${legacy_lease%/*}"
printf '{}\n' >"$legacy_lease"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --preflight-only --root "$ROOT" \
  >"$ROOT/run/preflight-legacy-lease.err" 2>&1; then
  printf 'read-only preflight accepted unresolved legacy lease state\n' >&2
  exit 1
fi
grep -Fq 'unresolved legacy production-envelope lease blocks Gate' \
  "$ROOT/run/preflight-legacy-lease.err" || {
  cat "$ROOT/run/preflight-legacy-lease.err" >&2
  exit 1
}
# A terminal marker alone is not audit evidence.  The complete run-scoped
# record must validate before it can stop blocking a new Gate.
jq -cn '{schema:"monday.rust_lob_bootstrap_slice_lease.v1",applied:true,restored:true}' \
  >"$legacy_lease"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --preflight-only --root "$ROOT" \
  >"$ROOT/run/preflight-incomplete-terminal-lease.err" 2>&1; then
  printf 'read-only preflight accepted an incomplete terminal legacy lease\n' >&2
  exit 1
fi
grep -Fq 'unresolved legacy production-envelope lease blocks Gate' \
  "$ROOT/run/preflight-incomplete-terminal-lease.err" || {
  cat "$ROOT/run/preflight-incomplete-terminal-lease.err" >&2
  exit 1
}
legacy_run=20260829T000000Z-1
legacy_gate_script="$ROOT/opt/monday/releases/binance-lob-controller/$c0/deployment/host-rust-lob-shadow-gate.sh"
legacy_recovery_service="monday-rust-lob-gate-${legacy_run}-lease-recovery.service"
legacy_recovery_timer="monday-rust-lob-gate-${legacy_run}-lease-recovery.timer"
jq -cn --arg run "$legacy_run" --arg slice "$production_slice_asset" \
  --arg controller "$c0" --arg gate_script "$legacy_gate_script" \
  --arg gate_script_sha "$(monday_sha256_file "$legacy_gate_script")" \
  --arg recovery_service "$legacy_recovery_service" \
  --arg recovery_timer "$legacy_recovery_timer" \
  '{schema:"monday.rust_lob_bootstrap_slice_lease.v1",run_id:$run,slice:$slice,
    mode:"temporary-bootstrap",before_memory_high:"infinity",before_memory_max:"infinity",
    before_parent_control_group:("/system.slice/" + $slice),
    before_parent_memory_current_bytes:0,before_parent_memory_anon_bytes:0,
    requested_memory_high:"3072M",requested_memory_max:"3584M",
    candidate_controller_sha256:$controller,gate_script:$gate_script,
    gate_script_sha256:$gate_script_sha,gate_pid:1,gate_starttime:1,
    recovery_service:$recovery_service,recovery_timer:$recovery_timer,
    applied:true,restored:true}' >"$legacy_lease"
if ! MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --preflight-only --root "$ROOT" \
  >"$ROOT/run/preflight-terminal-v1-lease.json" 2>&1; then
  cat "$ROOT/run/preflight-terminal-v1-lease.json" >&2
  exit 1
fi
jq -c --arg timer monday-collector-health.timer --arg service monday-collector-health.service '
  .schema = "monday.rust_lob_bootstrap_slice_lease.v2"
  | .bootstrap_monitor_containment = {
      required:true,timer:$timer,service:$service,
      before_timer:{unit:$timer,load_state:"loaded",active_state:"active",sub_state:"waiting",unit_file_state:"enabled"},
      before_service:{unit:$service,load_state:"loaded",active_state:"inactive",sub_state:"dead",unit_file_state:"static"},
      pause_applied:true,timer_restored:true,service_was_noninactive:false,service_quiesced:true
    }
' "$legacy_lease" >"${legacy_lease}.tmp"
mv -f -- "${legacy_lease}.tmp" "$legacy_lease"
if ! MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --preflight-only --root "$ROOT" \
  >"$ROOT/run/preflight-terminal-v2-lease.json" 2>&1; then
  cat "$ROOT/run/preflight-terminal-v2-lease.json" >&2
  exit 1
fi
rm -f -- "$legacy_lease"
# An absent lock is a fail-closed preflight error and must not be recreated.
mv -f -- "$preflight_lock_path" "$preflight_lock_path.missing"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --preflight-only --root "$ROOT" \
  >/dev/null 2>"$ROOT/run/preflight-lock-missing.err"; then
  printf 'read-only preflight recreated a missing lock file\n' >&2
  exit 1
fi
[[ ! -e "$preflight_lock_path" && -e "$preflight_lock_path.missing" ]] || {
  printf 'missing-lock preflight changed lock path state\n' >&2
  exit 1
}
mv -f -- "$preflight_lock_path.missing" "$preflight_lock_path"
preflight_residue_after=$(find "$ROOT/data/monday/spool/binance-lob-rust-shadow" \
  "$ROOT/data/monday/evidence/shadow-gates" "$ROOT/run/monday/rust-lob-gate" \
  "$ROOT/run/systemd/system" -mindepth 1 -print 2>/dev/null | LC_ALL=C sort || true)
[[ $preflight_residue_after == "$preflight_residue_before" ]] || {
  printf 'missing-lock preflight left run-scoped residue\n' >&2
  exit 1
}
# Fixture lock contention is deterministic because macOS has no flock binary;
# production uses the real flock command on the same read-only descriptor.
rm -f -- "$ROOT/run/gate-fixture.calls"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_LOCK_BUSY=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --preflight-only --root "$ROOT" \
  >/dev/null 2>"$ROOT/run/preflight-lock-busy.err"; then
  printf 'read-only preflight ignored lock contention\n' >&2
  exit 1
fi
[[ $(monday_sha256_file "$preflight_lock_path") == "$preflight_lock_sha" ]] || {
  printf 'lock-contention preflight changed the existing lock file\n' >&2
  exit 1
}
[[ ! -s "$ROOT/run/gate-fixture.calls" ]] || {
  printf 'lock-contention preflight invoked a mutating systemd action\n' >&2
  exit 1
}
# A failed identity check must fail before any write as well.  Point production
# at bytes that differ from immutable legacy C0, then restore the exact link.
preflight_residue_before=$preflight_residue_after
direct_production="$ROOT/opt/monday/bin/binance-lob-archiver"
direct_production_before=$(readlink -- "$direct_production")
rm -f -- "$direct_production"
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p1_sha/binance-lob-archiver" \
  "$direct_production"
identity_preflight_succeeded=false
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c1" --preflight-only --root "$ROOT" \
  >"$ROOT/run/preflight-failure.err" 2>&1; then
  identity_preflight_succeeded=true
fi
rm -f -- "$direct_production"
ln -s "$direct_production_before" "$direct_production"
if [[ $identity_preflight_succeeded == true ]]; then
  printf 'read-only preflight accepted production bytes outside legacy C0\n' >&2
  exit 1
fi
grep -Fq 'direct bootstrap requires an immutable v1 active controller' \
  "$ROOT/run/preflight-failure.err"
preflight_residue_after=$(find "$ROOT/data/monday/spool/binance-lob-rust-shadow" \
  "$ROOT/data/monday/evidence/shadow-gates" "$ROOT/run/monday/rust-lob-gate" \
  "$ROOT/run/systemd/system" -mindepth 1 -print 2>/dev/null | LC_ALL=C sort || true)
[[ $preflight_residue_after == "$preflight_residue_before" ]] || {
  printf 'failed read-only preflight left run-scoped residue\n' >&2
  exit 1
}

# A different candidate payload and runtime must remain recoverable through the
# direct Cutover boundary.  Reinstall the distinct legacy cadence and bind it to
# its immutable C0 before producing the Gate used by this crash transition.
for runtime_env in "${runtime_delta_envs[@]}"; do
  cp -p -- "$runtime_env" "$runtime_env.before-payload-delta"
  chmod u+w "$runtime_env"
  write_legacy_runtime_delta "$runtime_env.before-payload-delta" "$runtime_env"
done
rm -f -- "$legacy_root/active"
ln -s "$legacy_root/$legacy_delta_c0" "$legacy_root/active"
[[ $(monday_rust_lob_live_runtime_contract_sha256_v1 "$ROOT") == "$legacy_delta_runtime" ]]
# SIGKILL after the recovery intent but before active=C1 must leave C0/P0/R0
# frozen and permit one identical retry.  The retry is then killed immediately
# after active=C1; Restore must converge that authorized intermediate state to
# C1/P1/R1 without guessing another Gate, payload, or runtime.
if ! payload_delta_gate_output=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  bash -c '
    root=$1; shift
    mkdir -p "$root/proc/$$"
    printf "1 fixture S 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 4242\\n" >"$root/proc/$$/stat"
    printf "%s\\n" "$$" >"$root/run/payload-delta-gate.pid"
    exec "$@"
  ' _ "$ROOT" "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" \
  --from-controller direct --candidate-controller "$c1" --root "$ROOT" 2>&1); then
  printf '%s\n' "$payload_delta_gate_output" >&2
  printf 'formal Gate rejected a direct payload transition\n' >&2
  exit 1
fi
payload_delta_gate_pid=$(cat "$ROOT/run/payload-delta-gate.pid")
rm -rf -- "$ROOT/proc/$payload_delta_gate_pid" "$ROOT/run/payload-delta-gate.pid"
payload_delta_gate=$(printf '%s\n' "$payload_delta_gate_output" | sed -n 's/^V2 Gate receipt: //p')
payload_delta_gate_sha=$(printf '%s\n' "$payload_delta_gate_output" | sed -n 's/^SHA-256: //p')
monday_validate_v2_gate "$payload_delta_gate" direct "$c1" "$payload_delta_gate_sha"
rm -f -- "$ROOT/run/cutover-fixture.calls"
payload_delta_recovery="$ROOT/data/monday/evidence/cutovers/$c1/recovery.json"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CUTOVER_FIXTURE_SYSTEMD=1 \
  MONDAY_CUTOVER_FIXTURE_LEGACY_ACTIVE=1 MONDAY_CUTOVER_HARD_CRASH_AFTER_RECOVERY_INTENT=1 \
  MONDAY_ROOT="$ROOT" "$SCRIPT_DIR/host-rust-lob-cutover.sh" \
  --from direct --to "$c1" --gate-receipt "$payload_delta_gate" \
  --gate-sha256 "$payload_delta_gate_sha" --root "$ROOT" \
  >"$ROOT/run/payload-delta-precommit.err" 2>&1; then
  printf 'pre-commit direct payload transition unexpectedly survived SIGKILL\n' >&2
  exit 1
fi
[[ $(monday_active_controller_sha "$ROOT") == "$legacy_delta_c0" ]]
[[ $(readlink -- "$direct_production") == "$direct_production_before" ]]
[[ $(monday_rust_lob_live_runtime_contract_sha256_v1 "$ROOT") == "$legacy_delta_runtime" ]]
if [[ ! -f $payload_delta_recovery || -L $payload_delta_recovery ]]; then
  printf 'pre-commit hard crash did not preserve its recovery intent\n' >&2
  exit 1
fi
payload_delta_recovery_sha=$(monday_sha256_file "$payload_delta_recovery")
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CUTOVER_FIXTURE_SYSTEMD=1 \
  MONDAY_CUTOVER_FIXTURE_LEGACY_ACTIVE=1 MONDAY_CUTOVER_HARD_CRASH_AFTER_ACTIVE=1 \
  MONDAY_ROOT="$ROOT" "$SCRIPT_DIR/host-rust-lob-cutover.sh" \
  --from direct --to "$c1" --gate-receipt "$payload_delta_gate" \
  --gate-sha256 "$payload_delta_gate_sha" --root "$ROOT" \
  >"$ROOT/run/payload-delta-cutover.err" 2>&1; then
  printf 'hard-crash direct payload transition unexpectedly survived SIGKILL\n' >&2
  exit 1
fi
[[ $(monday_active_controller_sha "$ROOT") == "$c1" ]]
[[ $(readlink -- "$direct_production") == "$direct_production_before" ]]
[[ $(monday_rust_lob_live_runtime_contract_sha256_v1 "$ROOT") == "$legacy_delta_runtime" ]]
if [[ ! -f $payload_delta_recovery || -L $payload_delta_recovery ]]; then
  cat "$ROOT/run/payload-delta-cutover.err" >&2
  printf 'hard-crash cutover did not preserve its recovery intent\n' >&2
  exit 1
fi
[[ $(monday_sha256_file "$payload_delta_recovery") == "$payload_delta_recovery_sha" ]]
payload_delta_restore_receipt="$ROOT/data/monday/evidence/restores/$c1/restore.json"
payload_delta_restore_marker="$payload_delta_restore_receipt.sha256"
payload_delta_restore_stop="$ROOT/run/payload-delta-restore-health.stop"
write_payload_delta_restore_health() {
  local session_prefix=$1 observed market symbols dataset
  while [[ ! -e "$ROOT/run/restore-fixture-start-spot" ]]; do
    [[ -e $payload_delta_restore_stop ]] && return 0
    sleep 0.05
  done
  while [[ ! -e $payload_delta_restore_stop ]]; do
    observed=$(date +%s%N)
    for market in spot usdm; do
      symbols=1000; dataset=spot_all
      [[ $market == usdm ]] && symbols=100 && dataset=usdm_perpetual_top100_lob
      jq -cn --arg market "$market" --arg dataset "$dataset" \
        --arg session "$session_prefix-$market" --argjson symbols "$symbols" \
        --argjson observed "$observed" '
          {market:$market,dataset:$dataset,status:"synced",sequence_gaps:0,
           symbol_count:$symbols,snapshot_ready_count:$symbols,bridged_count:$symbols,
           stream_coverage_verified_count:$symbols,snapshot_only_symbols:[],
           all_symbols_bridged:true,all_stream_coverage_verified:true,
           full_stream_coverage_verified:true,pending_upload_segments:0,
           queue_saturated:false,disk_warning:false,upload_warning:false,
           session_id:$session,updated_at_ns:$observed}
        ' >"$production_spool_root/$market/health.json.tmp"
      mv -f -- "$production_spool_root/$market/health.json.tmp" \
        "$production_spool_root/$market/health.json"
    done
    sleep 0.05
  done
}
payload_delta_restore_pid_before=5151
mkdir -p "$ROOT/proc/$payload_delta_restore_pid_before"
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p1_sha/binance-lob-archiver" \
  "$ROOT/proc/$payload_delta_restore_pid_before/exe"
rm -f -- "$payload_delta_restore_stop" "$ROOT/run/restore-fixture-start-spot" \
  "$ROOT/run/restore-fixture-start-usdm" "$production_spool_root/spot/health.json" \
  "$production_spool_root/usdm/health.json"
(write_payload_delta_restore_health restore-reboot-before) &
payload_delta_restore_writer=$!
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_RESTORE_FIXTURE_SYSTEMD=1 \
  MONDAY_RESTORE_FIXTURE_PID="$payload_delta_restore_pid_before" \
  MONDAY_RESTORE_HEALTH_TIMEOUT_SECONDS=3 MONDAY_RESTORE_FAIL_AFTER_RECEIPT=1 \
  MONDAY_RESTORE_HARD_CRASH_AFTER_DIGEST_CLEANUP=1 \
  MONDAY_ROOT="$ROOT" "$SCRIPT_DIR/host-rust-lob-restore.sh" \
  --controller "$c1" --root "$ROOT" >"$ROOT/run/payload-delta-restore.err" 2>&1; then
  printf 'post-receipt cleanup unexpectedly survived SIGKILL\n' >&2
  exit 1
fi
: >"$payload_delta_restore_stop"
wait "$payload_delta_restore_writer"
[[ -f $payload_delta_recovery && ! -L $payload_delta_recovery ]]
[[ -f $payload_delta_restore_receipt && ! -L $payload_delta_restore_receipt ]]
[[ ! -e $payload_delta_restore_marker && ! -L $payload_delta_restore_marker ]]
jq -e --arg gate "$payload_delta_gate" --arg gate_sha "$payload_delta_gate_sha" '
  .transition_receipt == null
  and .gate_receipt == $gate and .gate_sha256 == $gate_sha
' "$payload_delta_restore_receipt" >/dev/null
payload_delta_restore_receipt_before=$(monday_sha256_file "$payload_delta_restore_receipt")
payload_delta_restore_pid_after=5152
mkdir -p "$ROOT/proc/$payload_delta_restore_pid_after"
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p1_sha/binance-lob-archiver" \
  "$ROOT/proc/$payload_delta_restore_pid_after/exe"
rm -f -- "$payload_delta_restore_stop" "$ROOT/run/restore-fixture-start-spot" \
  "$ROOT/run/restore-fixture-start-usdm"
(write_payload_delta_restore_health restore-reboot-after) &
payload_delta_restore_writer=$!
if ! payload_delta_restore_output=$(MONDAY_CONTROL_PLANE_TEST=1 \
  MONDAY_RESTORE_FIXTURE_SYSTEMD=1 MONDAY_RESTORE_FIXTURE_PID="$payload_delta_restore_pid_after" \
  MONDAY_RESTORE_HEALTH_TIMEOUT_SECONDS=3 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-restore.sh" --controller "$c1" --root "$ROOT" 2>&1); then
  : >"$payload_delta_restore_stop"
  wait "$payload_delta_restore_writer"
  printf '%s\n' "$payload_delta_restore_output" >&2
  printf 'Restore rejected post-receipt recovery after a host reboot\n' >&2
  exit 1
fi
: >"$payload_delta_restore_stop"
wait "$payload_delta_restore_writer"
grep -Fq 'Pair restore recovery complete' <<<"$payload_delta_restore_output"
[[ $(monday_sha256_file "$payload_delta_restore_receipt") == "$payload_delta_restore_receipt_before" ]]
[[ -f $payload_delta_restore_marker && ! -L $payload_delta_restore_marker ]]
payload_delta_restore_sha=$(awk '$2 == "restore.json" { count++; value=$1 } END { if (count != 1) exit 1; print value }' \
  "$payload_delta_restore_marker")
[[ $(monday_sha256_file "$payload_delta_restore_receipt") == "$payload_delta_restore_sha" ]]
jq -e --argjson old_pid "$payload_delta_restore_pid_before" \
  '.process_identity.spot.main_pid == $old_pid and .process_identity.usdm.main_pid == $old_pid' \
  "$payload_delta_restore_receipt" >/dev/null
jq -e '.session_id == "restore-reboot-after-spot"' \
  "$production_spool_root/spot/health.json" >/dev/null
[[ $(readlink -- "$direct_production") == \
  "$ROOT/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver" ]]
[[ $(readlink -f -- "$direct_production") == \
  "$ROOT/opt/monday/releases/binance-lob-archiver/$p1_sha/binance-lob-archiver" ]]
for asset in $(monday_runtime_assets); do
  target=$(monday_runtime_asset_target "$ROOT" "$asset")
  [[ -L $target && $(readlink -- "$target") == \
    "$ROOT/opt/monday/releases/binance-lob-controller/active/deployment/$asset" ]]
done
[[ $(monday_rust_lob_live_runtime_contract_sha256 "$ROOT") == "$candidate_runtime_sha" ]]
[[ ! -e $payload_delta_recovery ]]
jq -e --arg gate "$payload_delta_gate" --arg gate_sha "$payload_delta_gate_sha" '
  .transition_receipt == null
  and .gate_receipt == $gate and .gate_sha256 == $gate_sha
' "$payload_delta_restore_receipt" >/dev/null

# Reset the same immutable C0/P0/R0, then fail only after transition.json and
# its digest have both passed readback.  Cleanup must preserve that committed
# authority and active C1 instead of rolling back after recovery deletion starts.
rm -rf -- "$ROOT/data/monday/evidence/restores/$c1"
rm -f -- "$ROOT/opt/monday/releases/binance-lob-controller/active"
ln -s "$legacy_root/$legacy_delta_c0" "$ROOT/opt/monday/releases/binance-lob-controller/active"
rm -f -- "$direct_production"
ln -s "$direct_production_before" "$direct_production"
rm -f -- "$(monday_runtime_asset_target "$ROOT" "$production_slice_asset")"
while IFS= read -r asset; do
  [[ $asset == "$production_slice_asset" ]] && continue
  target=$(monday_runtime_asset_target "$ROOT" "$asset")
  rm -f -- "$target"
  cp -p -- "$ROOT/opt/monday/releases/binance-lob-controller/$c0/deployment/$asset" "$target"
done < <(monday_runtime_assets)
for runtime_env in "${runtime_delta_envs[@]}"; do
  chmod u+w "$runtime_env"
  write_legacy_runtime_delta "$runtime_env.before-payload-delta" "$runtime_env"
done
while IFS= read -r asset; do
  target=$(monday_controller_projection_target "$ROOT" "$asset")
  rm -f -- "$target"
  cp -p -- "$legacy_root/$legacy_c0/deployment/$asset" "$target"
done < <(monday_controller_projection_assets)
[[ $(monday_rust_lob_live_runtime_contract_sha256_v1 "$ROOT") == "$legacy_delta_runtime" ]]

if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CUTOVER_FIXTURE_SYSTEMD=1 \
  MONDAY_CUTOVER_FIXTURE_LEGACY_ACTIVE=1 MONDAY_CUTOVER_FAIL_AFTER_TRANSITION_COMMIT=1 \
  MONDAY_ROOT="$ROOT" "$SCRIPT_DIR/host-rust-lob-cutover.sh" \
  --from direct --to "$c1" --gate-receipt "$payload_delta_gate" \
  --gate-sha256 "$payload_delta_gate_sha" --root "$ROOT" \
  >"$ROOT/run/payload-delta-transition.err" 2>&1; then
  printf 'post-commit direct payload transition unexpectedly succeeded\n' >&2
  exit 1
fi
payload_delta_transition="$ROOT/data/monday/evidence/cutovers/$c1/transition.json"
payload_delta_transition_marker="$payload_delta_transition.sha256"
[[ -f $payload_delta_transition && ! -L $payload_delta_transition ]]
[[ -f $payload_delta_transition_marker && ! -L $payload_delta_transition_marker ]]
[[ -f $payload_delta_recovery && ! -L $payload_delta_recovery ]]
payload_delta_transition_sha=$(awk '$2 == "transition.json" { count++; value=$1 } END { if (count != 1) exit 1; print value }' \
  "$payload_delta_transition_marker")
[[ $(monday_sha256_file "$payload_delta_transition") == "$payload_delta_transition_sha" ]]
monday_validate_v2_transition "$ROOT" "$payload_delta_transition" direct "$c1" \
  "$payload_delta_gate" "$payload_delta_gate_sha"
[[ $(monday_active_controller_sha "$ROOT") == "$c1" ]]
[[ $(monday_rust_lob_live_runtime_contract_sha256 "$ROOT") == "$candidate_runtime_sha" ]]
# Model the narrower receipt-before-digest power-loss point.  The same durable
# intent must let Restore reconstruct the exact digest before clearing itself.
rm -f -- "$payload_delta_transition_marker"
MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-restore.sh" --controller "$c1" --root "$ROOT" >/dev/null
[[ ! -e $payload_delta_recovery ]]
[[ -f $payload_delta_transition_marker && ! -L $payload_delta_transition_marker ]]
[[ $(awk '$2 == "transition.json" { count++; value=$1 } END { if (count != 1) exit 1; print value }' \
  "$payload_delta_transition_marker") == "$payload_delta_transition_sha" ]]
payload_delta_restore_receipt="$ROOT/data/monday/evidence/restores/$c1/restore.json"
jq -e --arg transition "$payload_delta_transition" \
  --arg gate "$payload_delta_gate" --arg gate_sha "$payload_delta_gate_sha" '
    .transition_receipt == $transition
    and .gate_receipt == $gate and .gate_sha256 == $gate_sha
  ' "$payload_delta_restore_receipt" >/dev/null

# Return to the immutable legacy pair so the remaining direct-bootstrap
# rejection and ordinary success cases keep their original starting state.
rm -rf -- "$ROOT/data/monday/evidence/restores/$c1"
rm -rf -- "$ROOT/data/monday/evidence/cutovers/$c1"
rm -f -- "$ROOT/opt/monday/releases/binance-lob-controller/active"
ln -s "$legacy_root/$legacy_c0" "$ROOT/opt/monday/releases/binance-lob-controller/active"
rm -f -- "$direct_production"
ln -s "$direct_production_before" "$direct_production"
rm -f -- "$(monday_runtime_asset_target "$ROOT" "$production_slice_asset")"
while IFS= read -r asset; do
  [[ $asset == "$production_slice_asset" ]] && continue
  target=$(monday_runtime_asset_target "$ROOT" "$asset")
  rm -f -- "$target"
  cp -p -- "$ROOT/opt/monday/releases/binance-lob-controller/$c0/deployment/$asset" "$target"
done < <(monday_runtime_assets)
while IFS= read -r asset; do
  target=$(monday_controller_projection_target "$ROOT" "$asset")
  rm -f -- "$target"
  cp -p -- "$legacy_root/$legacy_c0/deployment/$asset" "$target"
done < <(monday_controller_projection_assets)
for runtime_env in "${runtime_delta_envs[@]}"; do
  rm -f -- "$runtime_env.before-payload-delta"
done
rm -rf -- "$(dirname -- "$payload_delta_gate")"
rm -f -- "$ROOT/run/cutover-fixture.calls"

# (d) A resource monitor breach must return to its caller so EXIT cleanup can
# remove every run-scoped writer path without changing production.
resource_breach_active_before=$(monday_active_controller_sha "$ROOT")
resource_breach_payload_before=$(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver")
rm -f -- "$ROOT/run/gate-fixture.calls"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_RESOURCE_BREACH=1 \
  MONDAY_GATE_FIXTURE_RECORD_CALLS=1 MONDAY_ROOT="$ROOT" \
  bash -c '
    root=$1; shift
    mkdir -p "$root/proc/$$"
    printf "1 fixture S 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 4242\\n" >"$root/proc/$$/stat"
    printf "%s\\n" "$$" >"$root/run/resource-breach-gate.pid"
    exec "$@"
  ' _ "$ROOT" "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --root "$ROOT" \
  >"$ROOT/run/resource-breach.err" 2>&1; then
  printf 'Gate accepted an injected direct-bootstrap resource monitor breach\n' >&2
  exit 1
fi
resource_breach_gate_pid=$(cat "$ROOT/run/resource-breach-gate.pid")
rm -rf -- "$ROOT/proc/$resource_breach_gate_pid" "$ROOT/run/resource-breach-gate.pid"
grep -Fqx 'resource monitor breached during preflight: fixture-resource-breach' "$ROOT/run/resource-breach.err"
if grep -Fq 'run-scoped Gate cleanup was incomplete' "$ROOT/run/resource-breach.err"; then
  printf 'primary resource breach was misreported as cleanup incomplete\n' >&2
  exit 1
fi
resource_breach_run=$(find "$ROOT/data/monday/evidence/shadow-gates/$c0/$candidate_runtime_sha/runs" \
  -mindepth 1 -maxdepth 1 -type d -print 2>/dev/null | LC_ALL=C sort | tail -n1)
resource_breach_run=${resource_breach_run##*/}
resource_breach_slice="mondayrustlobgate${resource_breach_run//[^0-9]/}.slice"
resource_breach_diagnostic="$ROOT/data/monday/evidence/shadow-gates/$c0/$candidate_runtime_sha/runs/$resource_breach_run/resource-monitor-failure.json"
jq -e '.schema == "monday.rust_lob_shadow_gate_resource_breach.v1"
  and .authoritative == false and .phase == "preflight"
  and .cause == "fixture-resource-breach"
  and (.elapsed_us | type == "number") and (.delta_us | type == "number")
  and (.ratio | type == "number") and (.consecutive_hits | type == "number")
  and .cleanup_failed == false' "$resource_breach_diagnostic" >/dev/null
[[ ! -e "$ROOT/run/monday/rust-lob-gate/$resource_breach_run" ]]
[[ ! -e "$ROOT/data/monday/spool/binance-lob-rust-shadow/gate/$resource_breach_run" ]]
[[ ! -e "$ROOT/run/systemd/system/$resource_breach_slice" ]]
if find "$ROOT/data/monday/evidence/shadow-gates/$c0" -type f \
  \( -name gate.json -o -name PASSED.sha256 \) -print -quit 2>/dev/null | grep -q .; then
  printf 'resource monitor breach left an authoritative Gate receipt\n' >&2
  exit 1
fi
[[ $(monday_active_controller_sha "$ROOT") == "$resource_breach_active_before" ]]
[[ $(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver") == "$resource_breach_payload_before" ]]
[[ $(monday_sha256_file "$production_spool_root/spot/upload-status.json") == "$production_spot_status_sha" ]]
[[ $(monday_sha256_file "$production_spool_root/usdm/upload-status.json") == "$production_usdm_status_sha" ]]

# (e) A teardown error remains distinct from a primary resource breach and is
# reported as cleanup incomplete.  This fixture only fails the monitor stop
# operation.
rm -f -- "$ROOT/run/gate-fixture.calls"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_RESOURCE_STOP_FAILURE=1 \
  MONDAY_GATE_FIXTURE_RECORD_CALLS=1 MONDAY_ROOT="$ROOT" \
  bash -c '
    root=$1; shift
    mkdir -p "$root/proc/$$"
    printf "1 fixture S 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 4242\\n" >"$root/proc/$$/stat"
    exec "$@"
  ' _ "$ROOT" "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --root "$ROOT" >"$ROOT/run/resource-stop-failure.err" 2>&1; then
  printf 'Gate accepted an injected resource monitor teardown failure\n' >&2
  exit 1
fi
grep -Fq 'run-scoped Gate cleanup was incomplete' "$ROOT/run/resource-stop-failure.err"
if grep -Fq 'resource monitor breached during' "$ROOT/run/resource-stop-failure.err"; then
  printf 'teardown failure was misreported as a primary resource breach\n' >&2
  exit 1
fi

gate_output=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  bash -c '
    root=$1; shift
    mkdir -p "$root/proc/$$"
    printf "1 fixture S 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 4242\\n" >"$root/proc/$$/stat"
    printf "%s\\n" "$$" >"$root/run/direct-gate.pid"
    exec "$@"
  ' _ "$ROOT" "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" \
  --from-controller direct --candidate-controller "$c0" --root "$ROOT")
direct_gate_pid=$(cat "$ROOT/run/direct-gate.pid")
rm -rf -- "$ROOT/proc/$direct_gate_pid" "$ROOT/run/direct-gate.pid"
gate=$(printf '%s\n' "$gate_output" | sed -n 's/^V2 Gate receipt: //p')
gate_sha=$(printf '%s\n' "$gate_output" | sed -n 's/^SHA-256: //p')
[[ -f $gate && $gate_sha == "$(monday_sha256_file "$gate")" ]]
monday_validate_v2_gate "$gate" direct "$c0" "$gate_sha"
jq -e -f "$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq" "$gate" >/dev/null
jq -e '
  .schema == "monday.rust_lob_shadow_gate.v7"
  and .source_mode == "direct"
  and .production_memory.production_envelope_state == "legacy-unlimited"
  and .production_memory.production_slice_memory_high_bytes == null
  and .production_memory.production_slice_memory_max_bytes == null
  and .production_memory.systemd_production_slice_memory_high_bytes == null
  and .production_memory.systemd_production_slice_memory_max_bytes == null
  and .production_memory.target_production_slice_memory_high_bytes == 3221225472
  and .production_memory.target_production_slice_memory_max_bytes == 3758096384
  and all(.resource_admission[];
    .production_slice_memory_max_bytes == null
    and .target_production_slice_memory_max_bytes == 3758096384
    and .production_memory_growth_bytes
      == (3758096384 - .production_parent_memory_anon_bytes))' "$gate" >/dev/null
jq -e --argjson evidence_timeout "$evidence_timeout_seconds" \
  --argjson segment "$gate_segment_seconds" \
  '.evidence_timeout_seconds == $evidence_timeout and .segment_seconds == $segment
   and (.io_full_psi_windows | length == 9)
   and all(.io_full_psi_windows[]; .veto == false)
   and all(.markets[]; . as $market
     | .observed_runtime_seconds >= 0 and .observed_runtime_seconds <= $evidence_timeout
     and .observation_started_at_ns <= .observed_at_ns
     and .segment_count == 2 and .oss_triplet_count == 2
     and all(.segments[]; .end_received_at_ns > $market.observation_started_at_ns)
     and all(.triplets[]; .end_received_at_ns > $market.observation_started_at_ns))' \
  "$gate" >/dev/null
run_json="$(dirname -- "$gate")/run.json"
jq -e --argjson segment "$gate_segment_seconds" \
  --argjson evidence_timeout "$evidence_timeout_seconds" \
  '.segment_seconds == $segment
   and .test_only == true
   and .evidence_timeout_seconds == $evidence_timeout
   and .formal_evidence_timeout_seconds == $evidence_timeout
   and all(.markets[];
     .observation_started_at_ns >= 0
     and .observation_finished_monotonic_ns >= .observation_started_monotonic_ns
     and (.observation_finished_monotonic_ns - .observation_started_monotonic_ns)
       >= (.observed_runtime_seconds * 1000000000)
     and (.observation_finished_monotonic_ns - .observation_started_monotonic_ns)
       < ((.observed_runtime_seconds + 1) * 1000000000))' \
  "$run_json" >/dev/null
jq -e --slurpfile gate "$gate" '
  .markets as $run_markets
  | all(["spot", "usdm"][]; . as $market
    | $run_markets[$market].observation_started_at_ns
      == $gate[0].markets[$market].observation_started_at_ns)' \
  "$run_json" >/dev/null
# A reconnect-boundary segment is skipped, but two later adjacent clean
# segments can still satisfy the evidence-driven Gate.
reconnect_gate_output=$(MONDAY_CONTROL_PLANE_TEST=1 \
  MONDAY_GATE_FIXTURE_RECONNECT_BOUNDARY=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --root "$ROOT")
reconnect_gate=$(printf '%s\n' "$reconnect_gate_output" | sed -n 's/^V2 Gate receipt: //p')
reconnect_gate_sha=$(printf '%s\n' "$reconnect_gate_output" | sed -n 's/^SHA-256: //p')
monday_validate_v2_gate "$reconnect_gate" direct "$c0" "$reconnect_gate_sha"
jq -e 'all(.markets[];
  .segment_count == 2 and .oss_triplet_count == 2
  and (.segments | length == 2) and (.triplets | length == 2))' \
  "$reconnect_gate" >/dev/null

# Clean files sealed before observation starts are audit input only.  The local
# selector and OSS readback must both choose the later adjacent pair.
preobservation_gate_output=$(MONDAY_CONTROL_PLANE_TEST=1 \
  MONDAY_GATE_FIXTURE_PREOBSERVATION_SEGMENTS=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --root "$ROOT")
preobservation_gate=$(printf '%s\n' "$preobservation_gate_output" | sed -n 's/^V2 Gate receipt: //p')
preobservation_gate_sha=$(printf '%s\n' "$preobservation_gate_output" | sed -n 's/^SHA-256: //p')
monday_validate_v2_gate "$preobservation_gate" direct "$c0" "$preobservation_gate_sha"
jq -e 'all(.markets[]; . as $market
  | all(.segments[]; .end_received_at_ns > $market.observation_started_at_ns)
  and all(.triplets[]; .end_received_at_ns > $market.observation_started_at_ns))' \
  "$preobservation_gate" >/dev/null

# Time alone never passes the Gate: one clean segment is insufficient.
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_SINGLE_CLEAN_SEGMENT=1 \
  MONDAY_ROOT="$ROOT" "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" \
  --from-controller direct --candidate-controller "$c0" --root "$ROOT" \
  >"$ROOT/run/single-clean-segment.err" 2>&1; then
  printf 'Gate accepted a single clean segment\n' >&2
  exit 1
fi
grep -Fq 'fixture did not produce two adjacent clean segments' \
  "$ROOT/run/single-clean-segment.err"
# Candidate and production cgroup memory.events are part of the same Gate
# evidence.  An OOM counter increment in the run-scoped worker slice must
# fail the Gate before any receipt or marker can be authorized.
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_GATE_OOM=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --root "$ROOT" \
  >"$ROOT/run/gate-oom.err" 2>&1; then
  printf 'Gate accepted a run-scoped worker OOM counter increment\n' >&2
  exit 1
fi
grep -Fq 'run-scoped Gate worker slice memory.events recorded an OOM during Gate' \
  "$ROOT/run/gate-oom.err" || {
  cat "$ROOT/run/gate-oom.err" >&2
  exit 1
}
# A fixture receipt must not become production evidence by editing its mode.
# Keep the adjacent run.json's provenance unchanged while moving its canonical
# run paths and provide the marker a real cutover would require; the
# authoritative wrapper still rejects the fixture provenance.
tampered_run_id=20240101T000000Z-1
tampered_dir="$(dirname -- "$(dirname -- "$gate")")/$tampered_run_id"
tampered_spool="$ROOT/data/monday/spool/binance-lob-rust-shadow/gate/$tampered_run_id"
tampered_unit_root="$ROOT/run/monday/rust-lob-gate/$tampered_run_id"
mkdir -p "$tampered_dir"
jq --arg run "$tampered_run_id" --arg spool "$tampered_spool" \
  '.run_id = $run | .run_spool = $spool' "$run_json" >"$tampered_dir/run.json"
jq --arg run "$tampered_run_id" --arg spool "$tampered_spool" \
  --arg unit_root "$tampered_unit_root" \
  '.run_id = $run
   | .run_spool = $spool
   | .shadow_staging.spool_root = $spool
   | .shadow_staging.run_unit_root = $unit_root
   | .markets.spot.spool_dir = ($spool + "/spot")
   | .markets.usdm.spool_dir = ($spool + "/usdm")
   | .test_only = false | .production_eligible = true' \
  "$gate" >"$tampered_dir/gate.json"
tampered_gate="$tampered_dir/gate.json"
tampered_sha=$(monday_sha256_file "$tampered_gate")
tampered_run_sha=$(monday_sha256_file "$tampered_dir/run.json")
printf '%s  gate.json\n%s  run.json\n' "$tampered_sha" "$tampered_run_sha" \
  >"$tampered_dir/PASSED.sha256"
chmod 0440 "$tampered_gate" "$tampered_dir/run.json" "$tampered_dir/PASSED.sha256"
chmod 0550 "$tampered_dir"
(cd "$tampered_dir" && sha256sum --check --strict PASSED.sha256 >/dev/null)
monday_validate_v2_gate "$tampered_gate" direct "$c0" "$tampered_sha"
if monday_validate_v2_gate_authoritative "$ROOT" "$tampered_gate" direct "$c0" "$tampered_sha"; then
  printf 'authoritative Gate validation accepted a promoted fixture receipt\n' >&2
  exit 1
fi
# A terminal-looking receipt without its adjacent run.json/marker is audit
# residue only.  It must not permanently block a fresh Gate for the candidate.
incomplete_prior_dir="$ROOT/data/monday/evidence/shadow-gates/$c0/$candidate_runtime_sha/runs/20240101T000000Z-2"
mkdir -p "$incomplete_prior_dir"
jq '.test_only = false | .production_eligible = true' \
  "$gate" >"$incomplete_prior_dir/gate.json"
incomplete_prior_gate="$incomplete_prior_dir/gate.json"
incomplete_prior_sha=$(monday_sha256_file "$incomplete_prior_gate")
if monday_validate_v2_gate_authoritative "$ROOT" "$incomplete_prior_gate" direct "$c0" \
  "$incomplete_prior_sha"; then
  printf 'authoritative Gate validation accepted an incomplete terminalization\n' >&2
  exit 1
fi
next_gate_output=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --root "$ROOT")
next_gate=$(printf '%s\n' "$next_gate_output" | sed -n 's/^V2 Gate receipt: //p')
next_gate_sha=$(printf '%s\n' "$next_gate_output" | sed -n 's/^SHA-256: //p')
[[ -f $next_gate && $next_gate_sha == "$(monday_sha256_file "$next_gate")" ]] || {
  printf 'incomplete prior Gate receipt blocked a fresh candidate run\n' >&2
  exit 1
}
jq -e '.shadow_staging.aggregate_slice.cgroup
  == ("/" + .shadow_staging.aggregate_slice.name)' "$gate" >/dev/null
oss_source=$(sed -n '/^run_oss()/,/^verify_oss_roundtrips()/p' \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh")
grep -Fq 'systemd-run --quiet --pipe --wait --collect' <<<"$oss_source"
grep -Fq -- "--slice=\"\$GATE_WORKER_SLICE\"" <<<"$oss_source"
grep -Fq -- '--property=MemoryMax=1536M' <<<"$oss_source"
jq -e 'all(.resource_admission[];
  .required_bytes == (.phase_memory_max_bytes + .host_memory_reserve_bytes + .production_memory_growth_bytes)
  and .production_memory_growth_bytes == (.target_production_slice_memory_max_bytes - .production_parent_memory_anon_bytes)
  and .host_memory_reserve_bytes == 1073741824
  and .host_memory_available_bytes >= .required_bytes
  and (has("production_memory_growth_headroom_bytes") | not))' "$gate" >/dev/null
low_end_available="$ROOT/low-end-available.json"
jq '(.resource_admission[0].current_memory_available_bytes = (.resource_admission[0].required_bytes - 1))' \
  "$gate" >"$low_end_available"
low_end_available_sha=$(monday_sha256_file "$low_end_available")
monday_validate_v2_gate "$low_end_available" direct "$c0" "$low_end_available_sha"
jq -e -f "$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq" "$low_end_available" >/dev/null
tampered="$ROOT/tampered-memory-admission.json"
jq '(.resource_admission[0].required_bytes) -= 1' "$gate" >"$tampered"
tampered_sha=$(monday_sha256_file "$tampered")
if monday_validate_v2_gate "$tampered" direct "$c0" "$tampered_sha"; then
  printf 'Gate validator accepted a phase requirement below phase max plus reserve\n' >&2
  exit 1
fi
if jq -e -f "$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq" "$tampered" >/dev/null 2>&1; then
  printf 'Gate policy accepted a phase requirement below phase max plus reserve\n' >&2
  exit 1
fi
tampered="$ROOT/tampered-segment-identity.json"
tampered_data_sha=$(printf 'f%.0s' {1..64})
jq --arg sha "$tampered_data_sha" '
    .markets.spot.triplets[1].data_sha256 = $sha
    | .markets.spot.triplets[1].success_content = ($sha + "\n")' \
  "$gate" >"$tampered"
tampered_sha=$(monday_sha256_file "$tampered")
if monday_validate_v2_gate "$tampered" direct "$c0" "$tampered_sha"; then
  printf 'Gate validator accepted mismatched local and OSS segment identity\n' >&2
  exit 1
fi
if jq -e -f "$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq" "$tampered" >/dev/null 2>&1; then
  printf 'Gate policy accepted mismatched local and OSS segment identity\n' >&2
  exit 1
fi
tampered="$ROOT/tampered-observation-cutoff.json"
jq '.markets.spot.observation_started_at_ns = .markets.spot.segments[0].end_received_at_ns' \
  "$gate" >"$tampered"
tampered_sha=$(monday_sha256_file "$tampered")
if monday_validate_v2_gate "$tampered" direct "$c0" "$tampered_sha"; then
  printf 'Gate validator accepted pre-observation segment evidence\n' >&2
  exit 1
fi
if jq -e -f "$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq" "$tampered" >/dev/null 2>&1; then
  printf 'Gate policy accepted pre-observation segment evidence\n' >&2
  exit 1
fi
tampered="$ROOT/tampered-production-memory.json"
jq '.production_memory.children.spot.memory_max_bytes = 2147483648
    | .production_memory.children.spot.systemd_memory_max_bytes = 2147483648
    | .production_memory.child_memory_max_sum_bytes = 4831838208' "$gate" >"$tampered"
tampered_sha=$(monday_sha256_file "$tampered")
if monday_validate_v2_gate "$tampered" direct "$c0" "$tampered_sha"; then
  printf 'Gate validator accepted tampered production cgroup memory limits\n' >&2
  exit 1
fi
if jq -e -f "$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq" "$tampered" >/dev/null 2>&1; then
  printf 'Gate policy accepted tampered production cgroup memory limits\n' >&2
  exit 1
fi
# Production-shaped path construction reaches spool preparation without
# opening a market socket.  The fixture still uses an isolated root, while
# exercising the unconditional run-scoped path branch.
path_only_output=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_PATH_ONLY=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c0" --root "$ROOT")
grep -Fq "V2 Gate spool preparation: $ROOT/data/monday/spool/binance-lob-rust-shadow/gate/" \
  <<<"$path_only_output"
jq -e --arg from "$legacy_c0" \
  '.source_mode == "direct" and .from_controller_sha256 == $from
   and .transition.before == $from and .transition.topology == "direct-bootstrap"' \
  "$gate" >/dev/null

# Gate's production lane is static evidence, not a process start.  The shared
# verifier must reject a candidate unit or market environment that would alter
# the governed production identity even though the Gate itself runs shadow.
production_verify_dir="$ROOT/production-runtime-verify"
mkdir -p "$production_verify_dir"
for asset in \
  binance-lob-archiver-production@.service binance-lob-archiver-upload@.service \
  binance-lob-archiver-production-spot.env binance-lob-archiver-production-usdm.env; do
  cp -p -- "$ROOT/opt/monday/releases/binance-lob-controller/$c0/deployment/$asset" \
    "$production_verify_dir/$asset"
done
sed -i.bak 's/^User=hftcollector$/User=untrusted/' \
  "$production_verify_dir/binance-lob-archiver-production@.service"
rm -f -- "$production_verify_dir/binance-lob-archiver-production@.service.bak"
if monday_verify_production_runtime_assets "$ROOT" "$production_verify_dir" "$p0_sha"; then
  printf 'production runtime verifier accepted an untrusted unit user\n' >&2
  exit 1
fi
chmod u+w "$production_verify_dir/binance-lob-archiver-production@.service"
rm -f -- "$production_verify_dir/binance-lob-archiver-production@.service"
cp -p -- "$ROOT/opt/monday/releases/binance-lob-controller/$c0/deployment/binance-lob-archiver-production@.service" \
  "$production_verify_dir/binance-lob-archiver-production@.service"
chmod u+w "$production_verify_dir/binance-lob-archiver-production-spot.env"
sed -i.bak 's/^OSS_ENDPOINT=.*/OSS_ENDPOINT=foreign.endpoint.example/' \
  "$production_verify_dir/binance-lob-archiver-production-spot.env"
rm -f -- "$production_verify_dir/binance-lob-archiver-production-spot.env.bak"
if monday_verify_production_runtime_assets "$ROOT" "$production_verify_dir" "$p0_sha"; then
  printf 'production runtime verifier accepted a foreign market endpoint\n' >&2
  exit 1
fi
chmod u+w "$production_verify_dir/binance-lob-archiver-production-spot.env"
cp -p -- "$ROOT/opt/monday/releases/binance-lob-controller/$c0/deployment/binance-lob-archiver-production-spot.env" \
  "$production_verify_dir/binance-lob-archiver-production-spot.env"
chmod u+w "$production_verify_dir/binance-lob-archiver-production@.service"
printf '\nExecStartPost=/bin/true\n' >>"$production_verify_dir/binance-lob-archiver-production@.service"
if monday_verify_production_runtime_assets "$ROOT" "$production_verify_dir" "$p0_sha"; then
  printf 'production runtime verifier accepted an unallowlisted ExecStartPost\n' >&2
  exit 1
fi
cp -p -- "$ROOT/opt/monday/releases/binance-lob-controller/$c0/deployment/binance-lob-archiver-production@.service" \
  "$production_verify_dir/binance-lob-archiver-production@.service"
chmod u+w "$production_verify_dir/binance-lob-archiver-production@.service"
printf 'ExecStart=/opt/monday/bin/binance-lob-archiver\n' >>"$production_verify_dir/binance-lob-archiver-production@.service"
if monday_verify_production_runtime_assets "$ROOT" "$production_verify_dir" "$p0_sha"; then
  printf 'production runtime verifier accepted a duplicate ExecStart\n' >&2
  exit 1
fi
cp -p -- "$ROOT/opt/monday/releases/binance-lob-controller/$c0/deployment/binance-lob-archiver-production@.service" \
  "$production_verify_dir/binance-lob-archiver-production@.service"
chmod u+w "$production_verify_dir/binance-lob-archiver-production-spot.env"
printf 'UNTRUSTED_RUNTIME_FLAG=1\n' >>"$production_verify_dir/binance-lob-archiver-production-spot.env"
if monday_verify_production_runtime_assets "$ROOT" "$production_verify_dir" "$p0_sha"; then
  printf 'production runtime verifier accepted an unknown environment key\n' >&2
  exit 1
fi

# Failed bootstrap cleanup must durably restore the raw C0/P0/R0 topology and
# remove partial evidence before active rolls back.  A hard stop immediately
# after that last rollback therefore leaves a complete C0 that accepts the one
# identical retry, not a mixed topology that requires guessed recovery.
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CUTOVER_FAIL_AFTER_ACTIVE=1 \
  MONDAY_CUTOVER_HARD_CRASH_AFTER_ACTIVE_ROLLBACK=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from direct --to "$c0" \
  --gate-receipt "$gate" --gate-sha256 "$gate_sha" --root "$ROOT" >/dev/null 2>&1; then
  printf 'post-rollback hard-crash cutover unexpectedly survived SIGKILL\n' >&2
  exit 1
fi
[[ $(monday_active_controller_sha "$ROOT") == "$legacy_c0" ]]
legacy_hard_crash_recovery="$ROOT/data/monday/evidence/cutovers/$c0/recovery.json"
if [[ ! -f $legacy_hard_crash_recovery || -L $legacy_hard_crash_recovery ]]; then
  printf 'same-payload hard crash did not preserve its recovery intent\n' >&2
  exit 1
fi
if [[ ! -L $ROOT/opt/monday/bin/binance-lob-archiver ]]; then
  printf 'same-payload hard crash removed the direct production projection\n' >&2
  exit 1
fi
for asset in binance-lob-archiver-rust-spot.env binance-lob-archiver-rust-usdm.env; do
  [[ -f "$(monday_runtime_asset_target "$ROOT" "$asset")" && ! -L "$(monday_runtime_asset_target "$ROOT" "$asset")" ]]
done
for asset in host-rust-lob-recovery-queue.sh monday-collector-health.sh; do
  target=$(monday_controller_projection_target "$ROOT" "$asset")
  active_asset="$ROOT/opt/monday/releases/binance-lob-controller/$c0/deployment/$asset"
  [[ -f $target && ! -L $target ]]
  [[ $(monday_sha256_file "$target") != "$(monday_sha256_file "$active_asset")" ]]
done
hard_crash_retry_output=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from direct --to "$c0" \
  --gate-receipt "$gate" --gate-sha256 "$gate_sha" --root "$ROOT")
grep -Fq 'Pair cutover complete' <<<"$hard_crash_retry_output"
[[ $(monday_active_controller_sha "$ROOT") == "$c0" ]]
[[ ! -e $legacy_hard_crash_recovery && ! -L $legacy_hard_crash_recovery ]]
rm -rf -- "$ROOT/data/monday/evidence/cutovers/$c0"
rm -f -- "$ROOT/opt/monday/releases/binance-lob-controller/active"
ln -s "$legacy_root/$legacy_c0" "$ROOT/opt/monday/releases/binance-lob-controller/active"
rm -f -- "$ROOT/opt/monday/bin/binance-lob-archiver"
ln -s "$direct_production_before" "$ROOT/opt/monday/bin/binance-lob-archiver"
rm -f -- "$(monday_runtime_asset_target "$ROOT" "$production_slice_asset")"
while IFS= read -r asset; do
  [[ $asset == "$production_slice_asset" ]] && continue
  target=$(monday_runtime_asset_target "$ROOT" "$asset")
  rm -f -- "$target"
  cp -p -- "$ROOT/opt/monday/releases/binance-lob-controller/$c0/deployment/$asset" "$target"
done < <(monday_runtime_assets)
while IFS= read -r asset; do
  target=$(monday_controller_projection_target "$ROOT" "$asset")
  rm -f -- "$target"
  cp -p -- "$legacy_root/$legacy_c0/deployment/$asset" "$target"
done < <(monday_controller_projection_assets)

# A distinct legacy controller carrying the same P/R is still not the Gate's
# authorized before identity.  Cutover must resolve the active legacy C0 and
# reject the receipt, then the original direct topology is restored.
legacy_alt_work="$ROOT/legacy-controller-alt"
mkdir -p "$legacy_alt_work/deployment"
jq --arg source "$(printf '8%.0s' {1..40})" --arg uri oss://bucket/legacy-controller-alt \
  '.deployment_source_revision = $source | .deployment_bundle_uri = $uri' \
  "$legacy_work/release.json" >"$legacy_alt_work/release.json"
legacy_alt_c0=$(monday_sha256_file "$legacy_alt_work/release.json")
for asset in host-rust-lob-recovery-queue.sh monday-collector-health.sh; do
  cp -p -- "$legacy_work/deployment/$asset" "$legacy_alt_work/deployment/$asset"
done
mkdir -p "$legacy_root/$legacy_alt_c0/deployment"
cp -p -- "$legacy_alt_work/release.json" "$legacy_root/$legacy_alt_c0/release.json"
cp -p -- "$legacy_alt_work/deployment/"* "$legacy_root/$legacy_alt_c0/deployment/"
(cd "$legacy_root/$legacy_alt_c0" && sha256sum release.json >release.json.sha256 && sha256sum deployment/* >deployment.sha256)
rm -f -- "$legacy_root/active"
ln -s "$legacy_root/$legacy_alt_c0" "$legacy_root/active"
rm -f -- "$ROOT/opt/monday/bin/binance-lob-archiver"
ln -s "$direct_production_before" "$ROOT/opt/monday/bin/binance-lob-archiver"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from direct --to "$c0" \
  --gate-receipt "$gate" --gate-sha256 "$gate_sha" --root "$ROOT" \
  >/dev/null 2>&1; then
  printf 'cutover accepted a different legacy C0 with the same P/R\n' >&2
  exit 1
fi
rm -f -- "$legacy_root/active"
ln -s "$legacy_root/$legacy_c0" "$legacy_root/active"
rm -f -- "$ROOT/opt/monday/bin/binance-lob-archiver"
ln -s "$direct_production_before" "$ROOT/opt/monday/bin/binance-lob-archiver"

# Bootstrap must independently bind the live R0 bytes.  A missing or drifted
# shadow runtime asset is rejected before any active/controller projection is
# changed, even when the Gate receipt itself was produced earlier.
bootstrap_shadow="$ROOT/etc/monday/binance-lob-archiver-rust-spot.env"
cp -p -- "$bootstrap_shadow" "$bootstrap_shadow.saved"
rm -f -- "$bootstrap_shadow"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from direct --to "$c0" \
  --gate-receipt "$gate" --gate-sha256 "$gate_sha" --root "$ROOT" >/dev/null 2>&1; then
  printf 'bootstrap accepted a missing shadow runtime asset\n' >&2
  exit 1
fi
mv -f -- "$bootstrap_shadow.saved" "$bootstrap_shadow"
chmod u+w "$bootstrap_shadow"
printf 'bootstrap-r0-drift\n' >"$bootstrap_shadow"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from direct --to "$c0" \
  --gate-receipt "$gate" --gate-sha256 "$gate_sha" --root "$ROOT" >/dev/null 2>&1; then
  printf 'bootstrap accepted drifted runtime bytes\n' >&2
  exit 1
fi
cp -p -- "$ROOT/opt/monday/releases/binance-lob-controller/$c0/deployment/binance-lob-archiver-rust-spot.env" \
  "$bootstrap_shadow"

# A normal direct-bootstrap failure must restore the exact pre-bootstrap writer
# states from the snapshot, including active legacy units; it must not leave
# the migration partially contained.
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CUTOVER_FIXTURE_SYSTEMD=1 \
  MONDAY_CUTOVER_FIXTURE_LEGACY_ACTIVE=1 MONDAY_CUTOVER_FAIL_AFTER_TRANSITION_EVIDENCE_COMMIT=1 \
  MONDAY_ROOT="$ROOT" "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from direct --to "$c0" \
  --gate-receipt "$gate" --gate-sha256 "$gate_sha" --root "$ROOT" >/dev/null 2>&1; then
  printf 'fault-injected direct bootstrap unexpectedly succeeded\n' >&2
  exit 1
fi
if [[ $(readlink -- "$direct_production") != "$direct_production_before" ]]; then
  printf 'failed direct bootstrap did not preserve the raw production projection\n' >&2
  exit 1
fi
direct_failure_recovery="$ROOT/data/monday/evidence/cutovers/$c0/recovery.json"
direct_failure_transition="$ROOT/data/monday/evidence/cutovers/$c0/transition.json"
if [[ ! -f $direct_failure_recovery || -L $direct_failure_recovery ]]; then
  printf 'failed direct bootstrap cleared its recovery authority\n' >&2
  exit 1
fi
[[ ! -e $direct_failure_transition && ! -L $direct_failure_transition \
  && ! -e $direct_failure_transition.sha256 && ! -L $direct_failure_transition.sha256 ]]
direct_failure_calls="$ROOT/run/cutover-fixture.calls"
for unit in \
  binance-lob-archiver@spot.service binance-lob-archiver@usdm.service \
  binance-lob-archiver-upload@spot.service binance-lob-archiver-upload@usdm.service; do
  grep -Fq "mask $unit" "$direct_failure_calls"
  grep -Fq "start $unit" "$direct_failure_calls"
done
rm -f -- "$ROOT/run/cutover-fixture.calls"

bootstrap_cutover_output=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CUTOVER_FIXTURE_SYSTEMD=1 \
  MONDAY_CUTOVER_FIXTURE_LEGACY_ACTIVE=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from direct --to "$c0" \
  --gate-receipt "$gate" --gate-sha256 "$gate_sha" --root "$ROOT")
[[ ! -e $direct_failure_recovery && ! -L $direct_failure_recovery ]]
bootstrap_transition=$(printf '%s\n' "$bootstrap_cutover_output" | sed -n 's/^Transition receipt: //p')
bootstrap_transition_sha=$(printf '%s\n' "$bootstrap_cutover_output" | sed -n 's/^SHA-256: //p')
[[ $(monday_active_controller_sha "$ROOT") == "$c0" ]]
[[ $(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver") == \
  "$ROOT/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver" ]]
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == \
  "$ROOT/opt/monday/releases/binance-lob-archiver/$p0_sha/binance-lob-archiver" ]]
[[ $(monday_sha256_file "$bootstrap_transition") == "$bootstrap_transition_sha" ]]
monday_validate_v2_transition "$ROOT" "$bootstrap_transition" direct "$c0" "$gate" "$gate_sha"
bootstrap_calls="$ROOT/run/cutover-fixture.calls"
for unit in \
  binance-lob-archiver@spot.service binance-lob-archiver@usdm.service \
  binance-lob-archiver-upload@spot.service binance-lob-archiver-upload@usdm.service; do
  grep -Fqx "stop $unit" "$bootstrap_calls"
  grep -Fqx "disable $unit" "$bootstrap_calls"
  grep -Fqx "mask $unit" "$bootstrap_calls"
  if grep -Fq "start $unit" "$bootstrap_calls"; then
    printf 'direct bootstrap resumed a legacy writer: %s\n' "$unit" >&2
    exit 1
  fi
done
mkdir -p "$ROOT/fixture-upload-status-empty"
MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  MONDAY_UPLOAD_STATUS_ROOT="$ROOT/fixture-upload-status-empty" \
  "$SCRIPT_DIR/host-rust-lob-readback.sh" --controller "$c0" \
  --transition-receipt "$bootstrap_transition" --receipt-sha256 "$bootstrap_transition_sha" \
  --root "$ROOT" >/dev/null
while IFS= read -r asset; do
  target=$(monday_runtime_asset_target "$ROOT" "$asset")
  [[ -L $target && $(readlink -- "$target") == \
    "$ROOT/opt/monday/releases/binance-lob-controller/active/deployment/$asset" ]]
done < <(monday_runtime_assets)

# Fresh resource admission must be checked immediately before a phase.  If
# MemAvailable falls before the Shadow admission, the Gate must reject before
# systemctl start and leave no candidate writer running.
rm -f -- "$ROOT/run/gate-fixture.calls"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_FRESH_ADMISSION_FAIL=1 \
  MONDAY_GATE_FIXTURE_RECORD_CALLS=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c0" \
  --candidate-controller "$c1" --root "$ROOT" >/dev/null 2>&1; then
  printf 'Gate accepted a phase after fresh memory admission failed\n' >&2
  exit 1
fi
if [[ -f $ROOT/run/gate-fixture.calls ]] && grep -Eq \
  '^start monday-rust-lob-gate-.*-(spot|usdm)\\.service$' "$ROOT/run/gate-fixture.calls"; then
  printf 'Shadow writer started after fresh memory admission failed\n' >&2
  exit 1
fi

# A production MainPID that is not present in its reported child cgroup is a
# mixed identity and must fail before any candidate writer starts.
rm -f -- "$ROOT/run/gate-fixture.calls"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_PID_MISMATCH=1 \
  MONDAY_GATE_FIXTURE_RECORD_CALLS=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c0" \
  --candidate-controller "$c1" --root "$ROOT" >/dev/null 2>&1; then
  printf 'Gate accepted a MainPID outside its production child cgroup\n' >&2
  exit 1
fi
if [[ -f $ROOT/run/gate-fixture.calls ]] && grep -Eq \
  '^start monday-rust-lob-gate-.*-(spot|usdm)\\.service$' "$ROOT/run/gate-fixture.calls"; then
  printf 'Shadow writer started after production MainPID membership failed\n' >&2
  exit 1
fi

# The asynchronous monitor is a synchronous guard in TEST_ONLY.  A changed
# production executable identity writes the breach marker and blocks the first
# phase, so no candidate writer or receipt can be produced.
rm -f -- "$ROOT/run/gate-fixture.calls"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_IDENTITY_DRIFT=1 \
  MONDAY_GATE_FIXTURE_RECORD_CALLS=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c0" \
  --candidate-controller "$c1" --root "$ROOT" >/dev/null 2>&1; then
  printf 'Gate accepted production identity drift in its monitor guard\n' >&2
  exit 1
fi
if [[ -f $ROOT/run/gate-fixture.calls ]] && grep -Eq \
  '^start monday-rust-lob-gate-.*-(spot|usdm)\\.service$' "$ROOT/run/gate-fixture.calls"; then
  printf 'Shadow writer started after production identity drift\n' >&2
  exit 1
fi

# A V2 active controller paired with a direct production binary is a mixed
# topology, not a second bootstrap mode.  Gate must reject it before reading
# any candidate control bytes, then the stable projection is restored.
mixed_production_target=$(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver")
rm -f -- "$ROOT/opt/monday/bin/binance-lob-archiver"
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p0_sha/binance-lob-archiver" \
  "$ROOT/opt/monday/bin/binance-lob-archiver"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c1" --root "$ROOT" >/dev/null 2>&1; then
  printf 'Gate accepted a mixed V2-active/direct-production topology\n' >&2
  exit 1
fi
rm -f -- "$ROOT/opt/monday/bin/binance-lob-archiver"
ln -s "$mixed_production_target" "$ROOT/opt/monday/bin/binance-lob-archiver"

# Simulate the production unit's ExecStartPre identity boundary.  The real
# helper is not invoked in an isolated fixture, but the exact stable
# projection, executable bit, resolved bytes, and active-controller target
# are checked through the same fixed path systemd calls.
simulate_recovery_execstartpre() {
  local asset target expected
  while IFS= read -r asset; do
    target=$(monday_controller_projection_target "$ROOT" "$asset")
    expected="$ROOT/opt/monday/releases/binance-lob-controller/active/deployment/$asset"
    bash -c 'set -eu
      entry=$1
      expected=$2
      test -L "$entry"
      test -x "$entry"
      test "$(readlink -- "$entry")" = "$expected"
      resolved=$(readlink -f -- "$entry")
      test -f "$resolved" && test ! -L "$resolved"
      cmp -s "$resolved" "$expected"' _ "$target" "$expected" \
      || return 1
  done < <(monday_controller_projection_assets)
}
simulate_recovery_execstartpre
recovery_projection=$(monday_controller_projection_target "$ROOT" host-rust-lob-recovery-queue.sh)
recovery_projection_target=$(readlink -- "$recovery_projection")
rm -f -- "$recovery_projection"
printf 'tampered-recovery-helper\n' >"$recovery_projection"
if simulate_recovery_execstartpre; then
  printf 'ExecStartPre simulation accepted a non-projected recovery helper\n' >&2
  exit 1
fi
rm -f -- "$recovery_projection"
ln -s "$recovery_projection_target" "$recovery_projection"
simulate_recovery_execstartpre

if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" \
  --from-controller direct --candidate-controller "$c0" --root "$ROOT" \
  >/dev/null 2>&1; then
  printf 'Gate accepted a second receipt for the same controller\n' >&2
  exit 1
fi

# Identity tampering is rejected by the receipt hash and transition identity.
tampered="$ROOT/tampered.json"
jq '.transition.after = ("f" * 64)' "$gate" >"$tampered"
tampered_sha=$(monday_sha256_file "$tampered")
if monday_validate_v2_gate "$tampered" direct "$c0" "$tampered_sha"; then
  printf 'Gate validator accepted a tampered transition\n' >&2
  exit 1
fi
tampered="$ROOT/tampered-policy.json"
jq '.checks.oss_triplets = false' "$gate" >"$tampered"
if jq -e -f "$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq" "$tampered" >/dev/null 2>&1; then
  printf 'Gate policy accepted a failed OSS check\n' >&2
  exit 1
fi

# A hand-written v5-shaped summary is not authoritative: the validator and
# policy must require the per-market triplet evidence rather than trusting the
# advertised counts.
fake="$ROOT/fake-v5.json"
jq '.markets.spot.triplets = [] | .markets.usdm.triplets = []' "$gate" >"$fake"
fake_sha=$(monday_sha256_file "$fake")
if monday_validate_v2_gate "$fake" direct "$c0" "$fake_sha"; then
  printf 'Gate validator accepted a summary-only v5 receipt\n' >&2
  exit 1
fi
if jq -e -f "$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq" "$fake" >/dev/null 2>&1; then
  printf 'Gate policy accepted a summary-only v5 receipt\n' >&2
  exit 1
fi

# Control-byte evidence is keyed to the exact controller asset set; a
# same-sized map with an unexpected asset must not authorize a transition.
fake="$ROOT/fake-control-assets.json"
jq '.candidate_control_bytes.assets = {unexpected: ("0" * 64)}' "$gate" >"$fake"
fake_sha=$(monday_sha256_file "$fake")
if monday_validate_v2_gate "$fake" direct "$c0" "$fake_sha"; then
  printf 'Gate validator accepted an unexpected control asset map\n' >&2
  exit 1
fi
if jq -e -f "$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq" "$fake" >/dev/null 2>&1; then
  printf 'Gate policy accepted an unexpected control asset map\n' >&2
  exit 1
fi

# A V2 before pair must be active and its payload may change.  Bootstrap has
# already established the permanent stable projections for every runtime
# asset; the test must not write through those links into immutable C0.
declare -A shadow_before_sha
for asset in \
  binance-lob-archiver-rust@.service binance-lob-archiver-rust-upload@.service \
  binance-lob-archiver-rust-spot.env binance-lob-archiver-rust-usdm.env; do
  if [[ $asset == *.service ]]; then target="$ROOT/etc/systemd/system/$asset"; else target="$ROOT/etc/monday/$asset"; fi
  [[ -L $target && $(readlink -- "$target") == "$ROOT/opt/monday/releases/binance-lob-controller/active/deployment/$asset" ]]
  shadow_before_sha[$asset]=$(monday_sha256_file "$(readlink -f -- "$target")")
done
# The before runtime must contain all four shadow assets and each one must
# resolve to the active controller projection.  Missing or drifted bytes are
# rejected before a Gate can stage a candidate.
missing_shadow="$ROOT/etc/monday/binance-lob-archiver-rust-spot.env"
rm -f -- "$missing_shadow"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c0" \
  --candidate-controller "$c1" --root "$ROOT" >/dev/null 2>&1; then
  printf 'Gate accepted a missing shadow runtime asset\n' >&2
  exit 1
fi
ln -s "$ROOT/opt/monday/releases/binance-lob-controller/active/deployment/binance-lob-archiver-rust-spot.env" \
  "$missing_shadow"
drift_shadow="$ROOT/etc/monday/binance-lob-archiver-rust-usdm.env"
rm -f -- "$drift_shadow"
printf 'drifted-runtime-byte\n' >"$drift_shadow"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c0" \
  --candidate-controller "$c1" --root "$ROOT" >/dev/null 2>&1; then
  printf 'Gate accepted drifted shadow runtime bytes\n' >&2
  exit 1
fi
rm -f -- "$drift_shadow"
ln -s "$ROOT/opt/monday/releases/binance-lob-controller/active/deployment/binance-lob-archiver-rust-usdm.env" \
  "$drift_shadow"
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p0_sha/binance-lob-archiver" \
  "$ROOT/opt/monday/bin/binance-lob-archiver-shadow"
shadow_before_target=$(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver-shadow")
gate_output=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" \
  --from-controller "$c0" --candidate-controller "$c1" --root "$ROOT")
gate=$(printf '%s\n' "$gate_output" | sed -n 's/^V2 Gate receipt: //p')
gate_sha=$(printf '%s\n' "$gate_output" | sed -n 's/^SHA-256: //p')
monday_validate_v2_gate "$gate" "$c0" "$c1" "$gate_sha"
jq -e '.source_mode == "stable"' "$gate" >/dev/null
# URI identity is part of the v5 policy, not just the downloader.  Moving a
# complete same-session triplet below an extra path component must fail both
# validators before it can become transition evidence.
fake="$ROOT/fake-extra-triplet.json"
jq '.markets.spot.triplets[0]
    |= (.data_uri |= sub("/shard=all/"; "/shard=all/extra/")
      | .manifest_uri |= sub("/shard=all/"; "/shard=all/extra/")
      | .success_uri |= sub("/shard=all/"; "/shard=all/extra/"))' \
  "$gate" >"$fake"
fake_sha=$(monday_sha256_file "$fake")
if monday_validate_v2_gate "$fake" "$c0" "$c1" "$fake_sha"; then
  printf 'Gate validator accepted an extra nested triplet path\n' >&2
  exit 1
fi
if jq -e -f "$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq" "$fake" >/dev/null 2>&1; then
  printf 'Gate policy accepted an extra nested triplet path\n' >&2
  exit 1
fi
# The standalone v5 policy and the shell validator must agree on Gregorian
# calendar validity, including leap years, rather than accepting date-shaped
# but impossible partitions.
rewrite_triplet_partition_date() {
  local source=$1 target=$2 date_value=$3
  jq --arg date "$date_value" '
    .markets.spot.triplets[0]
    |= (.object_prefix |= sub("/date=[0-9]{4}-[0-9]{2}-[0-9]{2}/hour="; ("/date=" + $date + "/hour="))
      | .data_uri |= sub("/date=[0-9]{4}-[0-9]{2}-[0-9]{2}/hour="; ("/date=" + $date + "/hour="))
      | .manifest_uri |= sub("/date=[0-9]{4}-[0-9]{2}-[0-9]{2}/hour="; ("/date=" + $date + "/hour="))
      | .success_uri |= sub("/date=[0-9]{4}-[0-9]{2}-[0-9]{2}/hour="; ("/date=" + $date + "/hour=")))
  ' "$source" >"$target"
}
fake="$ROOT/fake-impossible-date.json"
rewrite_triplet_partition_date "$gate" "$fake" 2026-02-29
fake_sha=$(monday_sha256_file "$fake")
if monday_validate_v2_gate "$fake" "$c0" "$c1" "$fake_sha"; then
  printf 'Gate validator accepted an impossible Gregorian date\n' >&2
  exit 1
fi
if jq -e -f "$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq" "$fake" >/dev/null 2>&1; then
  printf 'Gate policy accepted an impossible Gregorian date\n' >&2
  exit 1
fi
fake="$ROOT/fake-leap-date.json"
rewrite_triplet_partition_date "$gate" "$fake" 2024-02-29
fake_sha=$(monday_sha256_file "$fake")
monday_validate_v2_gate "$fake" "$c0" "$c1" "$fake_sha"
jq -e -f "$SCRIPT_DIR/rust-lob-shadow-gate-policy.jq" "$fake" >/dev/null
for asset in "${!shadow_before_sha[@]}"; do
  if [[ $asset == *.service ]]; then target="$ROOT/etc/systemd/system/$asset"; else target="$ROOT/etc/monday/$asset"; fi
  [[ $(monday_sha256_file "$(readlink -f -- "$target")") == "${shadow_before_sha[$asset]}" ]] || {
    printf 'Gate did not restore shadow asset %s\n' "$asset" >&2
    exit 1
  }
done
[[ $(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver-shadow") == "$shadow_before_target" ]]

# Cutover validates only the signed aggregate-slice configuration before any
# lane is unmasked.  A configured-limit failure must therefore leave the
# production start boundary untouched.
rm -f -- "$ROOT/run/cutover-fixture.calls"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CUTOVER_FIXTURE_SYSTEMD=1 \
  MONDAY_CUTOVER_FIXTURE_BAD_CONFIG=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from "$c0" --to "$c1" \
  --gate-receipt "$gate" --gate-sha256 "$gate_sha" --root "$ROOT" >/dev/null 2>&1; then
  printf 'cutover accepted an invalid signed production slice configuration\n' >&2
  exit 1
fi
config_line=$(grep -n -m1 '^verify-config ' "$ROOT/run/cutover-fixture.calls" | cut -d: -f1 || true)
unmask_line=$(grep -n -m1 '^unmask ' "$ROOT/run/cutover-fixture.calls" | cut -d: -f1 || true)
if [[ -z $config_line || ( -n $unmask_line && $unmask_line -lt $config_line ) ]]; then
  printf 'cutover unmasked production lanes before configured slice validation\n' >&2
  exit 1
fi

# Once configured limits pass, the full two-child verifier runs after starts;
# a child membership mismatch must be observed only at that post-start point.
rm -f -- "$ROOT/run/cutover-fixture.calls"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CUTOVER_FIXTURE_SYSTEMD=1 \
  MONDAY_CUTOVER_FIXTURE_BAD_MEMBERSHIP=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from "$c0" --to "$c1" \
  --gate-receipt "$gate" --gate-sha256 "$gate_sha" --root "$ROOT" >/dev/null 2>&1; then
  printf 'cutover accepted a live production child membership mismatch\n' >&2
  exit 1
fi
if [[ ! -f $ROOT/run/cutover-fixture.calls ]] || \
  [[ $(grep -Ec '^start binance-lob-archiver-production@(spot|usdm)\.service$' \
    "$ROOT/run/cutover-fixture.calls") -ne 2 ]]; then
  printf 'cutover did not reach the post-start membership verifier\n' >&2
  exit 1
fi

fixture_process_pid=4242
mkdir -p "$ROOT/proc/$fixture_process_pid"
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p1_sha/binance-lob-archiver" \
  "$ROOT/proc/$fixture_process_pid/exe"
write_cutover_fixture_health() {
  local started elapsed observed_at_ns
  started=$(date +%s)
  mkdir -p "$production_spool_root/spot" "$production_spool_root/usdm"
  while [[ ! -e "$ROOT/run/cutover-fixture-health.stop" ]]; do
    elapsed=$(( $(date +%s) - started ))
    (( elapsed >= 1 )) || { sleep 0.1; continue; }
    observed_at_ns=$(date +%s%N)
    jq -cn --argjson observed "$observed_at_ns" \
      '{market:"spot",dataset:"spot_all",status:"synced",sequence_gaps:0,symbol_count:1000,snapshot_ready_count:1000,bridged_count:1000,stream_coverage_verified_count:1000,snapshot_only_symbols:[],all_symbols_bridged:true,all_stream_coverage_verified:true,full_stream_coverage_verified:true,pending_upload_segments:0,queue_saturated:false,disk_warning:false,upload_warning:false,session_id:"cutover-fixture-spot",updated_at_ns:$observed}' \
      >"$production_spool_root/spot/health.json.tmp"
    mv -f -- "$production_spool_root/spot/health.json.tmp" "$production_spool_root/spot/health.json"
    if [[ ${FIXTURE_SPOT_FLIP:-0} == 1 ]]; then
      if [[ ! -e "$ROOT/run/cutover-fixture-spot-observed" ]]; then
        sleep 0.1
        continue
      fi
      : >"$ROOT/run/cutover-fixture-spot-flip"
    elif (( elapsed < 3 )); then
      sleep 0.1
      continue
    fi
    jq -cn --argjson observed "$observed_at_ns" \
      '{market:"usdm",dataset:"usdm_perpetual_top100_lob",status:"synced",sequence_gaps:0,symbol_count:100,snapshot_ready_count:100,bridged_count:100,stream_coverage_verified_count:100,snapshot_only_symbols:[],all_symbols_bridged:true,all_stream_coverage_verified:true,full_stream_coverage_verified:true,pending_upload_segments:0,queue_saturated:false,disk_warning:false,upload_warning:false,session_id:"cutover-fixture-usdm",updated_at_ns:$observed}' \
      >"$production_spool_root/usdm/health.json.tmp"
    mv -f -- "$production_spool_root/usdm/health.json.tmp" "$production_spool_root/usdm/health.json"
    (( elapsed < 15 )) || break
    sleep 0.1
  done
}
rm -f -- "$ROOT/run/cutover-fixture-health.stop" "$ROOT/run/cutover-fixture-spot-flip" \
  "$ROOT/run/cutover-fixture-spot-observed" \
  "$production_spool_root/spot/health.json" "$production_spool_root/usdm/health.json"
(
  write_cutover_fixture_health
) &
fixture_health_writer=$!
cutover_output=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CUTOVER_FIXTURE_SYSTEMD=1 \
  MONDAY_CUTOVER_FIXTURE_VERIFY_PROCESS=1 MONDAY_CUTOVER_FIXTURE_PID="$fixture_process_pid" \
  MONDAY_CUTOVER_HEALTH_TIMEOUT_SECONDS=5 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from "$c0" --to "$c1" \
  --gate-receipt "$gate" --gate-sha256 "$gate_sha" --root "$ROOT")
 : >"$ROOT/run/cutover-fixture-health.stop"
wait "$fixture_health_writer"
transition=$(printf '%s\n' "$cutover_output" | sed -n 's/^Transition receipt: //p')
transition_sha=$(printf '%s\n' "$cutover_output" | sed -n 's/^SHA-256: //p')
[[ $(monday_active_controller_sha "$ROOT") == "$c1" ]]
[[ $(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver") == \
  "$ROOT/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver" ]]
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == \
  "$ROOT/opt/monday/releases/binance-lob-archiver/$p1_sha/binance-lob-archiver" ]]
[[ $(monday_sha256_file "$transition") == "$transition_sha" ]]
monday_validate_v2_transition "$ROOT" "$transition" "$c0" "$c1" "$gate" "$gate_sha"
jq -e --argjson pid "$fixture_process_pid" \
  '.production_process | .spot.main_pid == $pid and .usdm.main_pid == $pid
   and .spot.process_exe_sha256 == .usdm.process_exe_sha256
   and .spot.n_restarts == 0 and .usdm.n_restarts == 0
   and (.spot.session_id | length) > 0 and (.usdm.session_id | length) > 0' \
  "$transition" >/dev/null

# A fault after the active-pair rename restores both identities under the lock.
printf '\n# controller revision three fixture\n' >>"$source_dir/host-rust-lob-readback.sh"
p2="$ROOT/p2"; m2="$ROOT/m2.json"
p2_sha=$(publish_fixture "$p2" "$m2")
c2=$(monday_sha256_file "$m2")
active_before_failure=$(monday_active_controller_sha "$ROOT")
production_before_failure=$(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver")
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_FAIL_RESTART=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c1" \
  --candidate-controller "$c2" --root "$ROOT" >/dev/null 2>&1; then
  printf 'fault-injected Gate unexpectedly succeeded\n' >&2
  exit 1
fi
[[ $(monday_active_controller_sha "$ROOT") == "$active_before_failure" ]]
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == "$production_before_failure" ]]
for asset in "${!shadow_before_sha[@]}"; do
  if [[ $asset == *.service ]]; then target="$ROOT/etc/systemd/system/$asset"; else target="$ROOT/etc/monday/$asset"; fi
  [[ $(monday_sha256_file "$(readlink -f -- "$target")") == "${shadow_before_sha[$asset]}" ]] || {
    printf 'failed Gate did not restore shadow asset %s\n' "$asset" >&2
    exit 1
  }
done
if find "$ROOT/data/monday/evidence/shadow-gates/$c2" -name PASSED.sha256 -print -quit | grep -q .; then
  printf 'failed Gate left a PASSED marker\n' >&2
  exit 1
fi
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_TAMPER_OSS=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c1" \
  --candidate-controller "$c2" --root "$ROOT" >/dev/null 2>&1; then
  printf 'OSS-tampered Gate unexpectedly succeeded\n' >&2
  exit 1
fi
[[ $(monday_active_controller_sha "$ROOT") == "$active_before_failure" ]]
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == "$production_before_failure" ]]
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_TRAILING_UNSAFE=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c1" \
  --candidate-controller "$c2" --root "$ROOT" >"$ROOT/run/trailing-unsafe.err" 2>&1; then
  printf 'Gate accepted a trailing replay-unsafe OSS manifest\n' >&2
  exit 1
fi
grep -Fq 'replay-safe manifest ordering failed' "$ROOT/run/trailing-unsafe.err"
[[ $(monday_active_controller_sha "$ROOT") == "$active_before_failure" ]]
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == "$production_before_failure" ]]
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_EXTRA_NESTED=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c1" \
  --candidate-controller "$c2" --root "$ROOT" >/dev/null 2>&1; then
  printf 'Gate accepted a nested extra-date OSS object\n' >&2
  exit 1
fi
[[ $(monday_active_controller_sha "$ROOT") == "$active_before_failure" ]]
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == "$production_before_failure" ]]
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --resource-preflight "$c2" >/dev/null 2>&1; then
  printf 'Gate exposed a public preflight action\n' >&2
  exit 1
fi
gate_output=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c1" \
  --candidate-controller "$c2" --root "$ROOT")
gate2=$(printf '%s\n' "$gate_output" | sed -n 's/^V2 Gate receipt: //p')
gate2_sha=$(printf '%s\n' "$gate_output" | sed -n 's/^SHA-256: //p')

# Spot is ready before USD-M, but its process identity changes while the
# second lane is catching up.  The final paired re-read must reject this
# partial-ready transition and leave the previous pair active.
flip_fixture_pid=4244; flip_fixture_pid_after=4245
mkdir -p "$ROOT/proc/$flip_fixture_pid" "$ROOT/proc/$flip_fixture_pid_after"
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p2_sha/binance-lob-archiver" \
  "$ROOT/proc/$flip_fixture_pid/exe"
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p2_sha/binance-lob-archiver" \
  "$ROOT/proc/$flip_fixture_pid_after/exe"
rm -f -- "$ROOT/run/cutover-fixture-health.stop" "$ROOT/run/cutover-fixture-spot-flip" \
  "$ROOT/run/cutover-fixture-spot-observed" \
  "$production_spool_root/spot/health.json" "$production_spool_root/usdm/health.json"
(
  FIXTURE_SPOT_FLIP=1 write_cutover_fixture_health
) &
flip_health_writer=$!
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CUTOVER_FIXTURE_SYSTEMD=1 \
  MONDAY_CUTOVER_FIXTURE_VERIFY_PROCESS=1 MONDAY_CUTOVER_FIXTURE_PID="$flip_fixture_pid" \
  MONDAY_CUTOVER_FIXTURE_SPOT_FLIP_PID="$flip_fixture_pid_after" \
  MONDAY_CUTOVER_HEALTH_TIMEOUT_SECONDS=5 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from "$c1" --to "$c2" \
  --gate-receipt "$gate2" --gate-sha256 "$gate2_sha" --root "$ROOT" >/dev/null 2>&1; then
  printf 'cutover accepted a Spot identity change while USD-M was becoming ready\n' >&2
  exit 1
fi
: >"$ROOT/run/cutover-fixture-health.stop"
wait "$flip_health_writer"
[[ $(monday_active_controller_sha "$ROOT") == "$c1" ]]
[[ ! -e "$ROOT/data/monday/evidence/cutovers/$c2/transition.json" ]]

# Cutover must reject a candidate whose process restart counter changes while
# waiting for the first fresh health publication.  This exercises the same
# post-start identity check as production, but remains a bounded fixture.
restart_fixture_pid=4243
mkdir -p "$ROOT/proc/$restart_fixture_pid"
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p2_sha/binance-lob-archiver" \
  "$ROOT/proc/$restart_fixture_pid/exe"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CUTOVER_FIXTURE_SYSTEMD=1 \
  MONDAY_CUTOVER_FIXTURE_VERIFY_PROCESS=1 MONDAY_CUTOVER_FIXTURE_PID="$restart_fixture_pid" \
  MONDAY_CUTOVER_FIXTURE_RESTARTS=1 MONDAY_CUTOVER_HEALTH_TIMEOUT_SECONDS=2 \
  MONDAY_ROOT="$ROOT" "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from "$c1" --to "$c2" \
  --gate-receipt "$gate2" --gate-sha256 "$gate2_sha" --root "$ROOT" \
  >/dev/null 2>&1; then
  printf 'cutover accepted a changed process restart counter\n' >&2
  exit 1
fi
[[ $(monday_active_controller_sha "$ROOT") == "$c1" ]]
[[ ! -e "$ROOT/data/monday/evidence/cutovers/$c2/transition.json" ]]

# A candidate Spot lane may start before the USD-M lane fails.  The rollback
# must restore the complete before pair (including both stable projections),
# then restart both old lanes; no candidate process or transition receipt may
# remain.  The once-only fixture lets the old USD-M lane recover successfully.
partial_receipt="$ROOT/data/monday/evidence/cutovers/$c2/transition.json"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CUTOVER_FIXTURE_SYSTEMD=1 \
  MONDAY_CUTOVER_FIXTURE_FAIL_USDM_ONCE=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from "$c1" --to "$c2" \
  --gate-receipt "$gate2" --gate-sha256 "$gate2_sha" --root "$ROOT" \
  >/dev/null 2>&1; then
  printf 'partial-start cutover unexpectedly succeeded\n' >&2
  exit 1
fi
[[ $(monday_active_controller_sha "$ROOT") == "$c1" ]]
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == \
  "$ROOT/opt/monday/releases/binance-lob-archiver/$p1_sha/binance-lob-archiver" ]]
[[ ! -e $partial_receipt && ! -L $partial_receipt ]]
partial_calls="$ROOT/run/cutover-fixture.calls"
for unit in binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service; do
  grep -Fq "mask $unit" "$partial_calls"
done
partial_process_root="$ROOT/run/cutover-fixture.processes"
for process in "$partial_process_root"/*; do
  [[ -e $process ]] || continue
  [[ $(cat "$process") == "$c1" ]] || {
    printf 'partial-start rollback left a candidate process: %s\n' "$process" >&2
    exit 1
  }
done

# If the old USD-M lane also fails during rollback, both lanes must remain
# stopped/masked rather than being reported as recovered with a partial pair.
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CUTOVER_FIXTURE_SYSTEMD=1 \
  MONDAY_CUTOVER_FIXTURE_FAIL_USDM=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from "$c1" --to "$c2" \
  --gate-receipt "$gate2" --gate-sha256 "$gate2_sha" --root "$ROOT" \
  >/dev/null 2>&1; then
  printf 'partial-start rollback-failure fixture unexpectedly succeeded\n' >&2
  exit 1
fi
[[ $(monday_active_controller_sha "$ROOT") == "$c1" ]]
for unit in binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service; do
  grep -Fq "mask $unit" "$partial_calls"
done
for unit in \
  binance-lob-archiver@spot.service binance-lob-archiver@usdm.service \
  binance-lob-archiver-upload@spot.service binance-lob-archiver-upload@usdm.service \
  binance-lob-archiver-rust@spot.service binance-lob-archiver-rust@usdm.service \
  binance-lob-archiver-rust-upload@spot.service binance-lob-archiver-rust-upload@usdm.service; do
  grep -Fq "mask $unit" "$partial_calls"
done
if compgen -G "$partial_process_root/*" >/dev/null; then
  printf 'contained partial-start rollback left a process marker\n' >&2
  exit 1
fi
[[ ! -e $partial_receipt && ! -L $partial_receipt ]]

active_before_stage=$(monday_active_controller_sha "$ROOT")
production_before_stage=$(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver")
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" MONDAY_CUTOVER_FAIL_AFTER_ASSET_STAGE=1 \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from "$c1" --to "$c2" \
  --gate-receipt "$gate2" --gate-sha256 "$gate2_sha" --root "$ROOT" \
  >/dev/null 2>&1; then
  printf 'fault-injected asset-stage cutover unexpectedly succeeded\n' >&2
  exit 1
fi
[[ $(monday_active_controller_sha "$ROOT") == "$active_before_stage" ]]
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == "$production_before_stage" ]]
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" MONDAY_CUTOVER_FAIL_AFTER_ACTIVE=1 \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from "$c1" --to "$c2" \
  --gate-receipt "$gate2" --gate-sha256 "$gate2_sha" --root "$ROOT" \
  >/dev/null 2>&1; then
  printf 'fault-injected cutover unexpectedly succeeded\n' >&2
  exit 1
fi
[[ $(monday_active_controller_sha "$ROOT") == "$c1" ]]
[[ $(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver") == \
  "$ROOT/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver" ]]
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == \
  "$ROOT/opt/monday/releases/binance-lob-archiver/$p1_sha/binance-lob-archiver" ]]

cutover_output=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from "$c1" --to "$c2" \
  --gate-receipt "$gate2" --gate-sha256 "$gate2_sha" --root "$ROOT")
transition2=$(printf '%s\n' "$cutover_output" | sed -n 's/^Transition receipt: //p')
transition2_sha=$(printf '%s\n' "$cutover_output" | sed -n 's/^SHA-256: //p')
transition2_marker="$transition2.sha256"
mv -- "$transition2_marker" "$transition2_marker.held"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-restore.sh" --controller "$c2" --root "$ROOT" \
  >/dev/null 2>&1; then
  printf 'restore accepted transition evidence without its durable digest\n' >&2
  exit 1
fi
mv -- "$transition2_marker.held" "$transition2_marker"
rm "$ROOT/opt/monday/bin/binance-lob-archiver"
MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-restore.sh" --controller "$c2" --root "$ROOT" >/dev/null
[[ $(monday_active_controller_sha "$ROOT") == "$c2" ]]
[[ $(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver") == \
  "$ROOT/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver" ]]
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == \
  "$ROOT/opt/monday/releases/binance-lob-archiver/$p2_sha/binance-lob-archiver" ]]
[[ $(monday_sha256_file "$transition2") == "$transition2_sha" ]]
MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  MONDAY_UPLOAD_STATUS_ROOT="$ROOT/fixture-upload-status-empty" \
  "$SCRIPT_DIR/host-rust-lob-readback.sh" --controller "$c2" \
  --transition-receipt "$transition2" --receipt-sha256 "$transition2_sha" \
  --root "$ROOT" >/dev/null

# Restore's health and process checks are exercised with one bounded fixture
# writer.  The writer waits for the fixture systemd start marker, so every
# accepted sample is fresh relative to restore_started_ns; invalid policy
# states are rejected without waiting for the production timeout.
restore_fixture_pid=5252
mkdir -p "$ROOT/proc/$restore_fixture_pid"
rm -f -- "$ROOT/proc/$restore_fixture_pid/exe"
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p2_sha/binance-lob-archiver" \
  "$ROOT/proc/$restore_fixture_pid/exe"
write_restore_fixture_health() {
  local mode=$1 observed status gaps ready symbols market dataset
  while [[ ! -e "$ROOT/run/restore-fixture-start-spot" ]]; do
    [[ -e "$ROOT/run/restore-fixture-health.stop" ]] && return 0
    sleep 0.05
  done
  while [[ ! -e "$ROOT/run/restore-fixture-health.stop" ]]; do
    observed=$(date +%s%N)
    for market in spot usdm; do
      symbols=1000; dataset=spot_all
      [[ $market == usdm ]] && symbols=100 && dataset=usdm_perpetual_top100_lob
      status=synced; gaps=0; ready=$symbols
      case $mode in
        unsynced) status=starting ;;
        gaps) gaps=1 ;;
        nonready) ready=0 ;;
        success) : ;;
        *) return 2 ;;
      esac
      jq -cn --arg market "$market" --arg dataset "$dataset" --arg status "$status" \
        --arg session "restore-fixture-${mode}-${market}" --argjson symbols "$symbols" \
        --argjson ready "$ready" --argjson gaps "$gaps" --argjson observed "$observed" \
        '{market:$market,dataset:$dataset,status:$status,sequence_gaps:$gaps,symbol_count:$symbols,
          snapshot_ready_count:$ready,bridged_count:$symbols,stream_coverage_verified_count:$symbols,
          snapshot_only_symbols:[],all_symbols_bridged:true,all_stream_coverage_verified:true,
          full_stream_coverage_verified:true,pending_upload_segments:0,queue_saturated:false,
          disk_warning:false,upload_warning:false,session_id:$session,updated_at_ns:$observed}' \
        >"$production_spool_root/$market/health.json.tmp"
      mv -f -- "$production_spool_root/$market/health.json.tmp" \
        "$production_spool_root/$market/health.json"
    done
    sleep 0.05
  done
}
run_restore_health_fixture() {
  local mode=$1 receipt="$ROOT/data/monday/evidence/restores/$c2/restore.json"
  rm -f -- "$receipt" "$receipt.sha256" \
    "$production_spool_root/spot/health.json" "$production_spool_root/usdm/health.json" \
    "$ROOT/run/restore-fixture-health.stop" "$ROOT/run/restore-fixture-start-spot" \
    "$ROOT/run/restore-fixture-start-usdm"
  (write_restore_fixture_health "$mode") &
  local writer=$!
  if [[ $mode == success ]]; then
    MONDAY_CONTROL_PLANE_TEST=1 MONDAY_RESTORE_FIXTURE_SYSTEMD=1 \
      MONDAY_RESTORE_FIXTURE_PID="$restore_fixture_pid" \
      MONDAY_RESTORE_HEALTH_TIMEOUT_SECONDS=2 MONDAY_ROOT="$ROOT" \
      "$SCRIPT_DIR/host-rust-lob-restore.sh" --controller "$c2" --root "$ROOT" >/dev/null
  else
    if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_RESTORE_FIXTURE_SYSTEMD=1 \
      MONDAY_RESTORE_FIXTURE_PID="$restore_fixture_pid" \
      MONDAY_RESTORE_HEALTH_TIMEOUT_SECONDS=2 MONDAY_ROOT="$ROOT" \
      "$SCRIPT_DIR/host-rust-lob-restore.sh" --controller "$c2" --root "$ROOT" \
      >/dev/null 2>&1; then
      printf 'restore accepted invalid health state: %s\n' "$mode" >&2
      exit 1
    fi
  fi
  : >"$ROOT/run/restore-fixture-health.stop"
  wait "$writer"
  [[ $mode != success || -f $receipt ]] || {
    printf 'restore success fixture did not emit a receipt\n' >&2
    exit 1
  }
}
run_restore_health_fixture unsynced
run_restore_health_fixture gaps
run_restore_health_fixture nonready

# A production ExecStartPre drift is a preflight failure.  It must not be
# overwritten from an unrelated source, and the failure cleanup still masks
# every writer.  Restore the exact active-C projection afterwards.
restore_service_projection="$ROOT/etc/systemd/system/binance-lob-archiver-production@.service"
restore_service_source=$(readlink -f -- "$restore_service_projection")
rm -f -- "$restore_service_projection"
cp -p -- "$restore_service_source" "$restore_service_projection"
chmod u+w "$restore_service_projection"
printf 'ExecStartPre=/opt/monday/bin/untrusted-helper\n' >>"$restore_service_projection"
rm -f -- "$ROOT/data/monday/evidence/restores/$c2/restore.json" \
  "$ROOT/data/monday/evidence/restores/$c2/restore.json.sha256" \
  "$ROOT/run/restore-fixture.calls"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_RESTORE_FIXTURE_SYSTEMD=1 \
  MONDAY_RESTORE_FIXTURE_PID="$restore_fixture_pid" MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-restore.sh" --controller "$c2" --root "$ROOT" \
  >/dev/null 2>&1; then
  printf 'restore accepted drifted ExecStartPre projection\n' >&2
  exit 1
fi
restore_calls="$ROOT/run/restore-fixture.calls"
for unit in \
  binance-lob-archiver@spot.service binance-lob-archiver@usdm.service \
  binance-lob-archiver-upload@spot.service binance-lob-archiver-upload@usdm.service \
  binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service; do
  grep -Fq "mask $unit" "$restore_calls"
done
rm -f -- "$restore_service_projection"
ln -s "$ROOT/opt/monday/releases/binance-lob-controller/active/deployment/binance-lob-archiver-production@.service" \
  "$restore_service_projection"

# Restore uses the same two-stage systemd proof as Cutover: signed aggregate
# configuration before unmask, then exact child membership after both starts.
for restore_failure in config membership; do
  rm -f -- "$ROOT/data/monday/evidence/restores/$c2/restore.json" \
    "$ROOT/data/monday/evidence/restores/$c2/restore.json.sha256" \
    "$ROOT/run/restore-fixture.calls"
  restore_env=(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_RESTORE_FIXTURE_SYSTEMD=1 \
    MONDAY_RESTORE_FIXTURE_PID="$restore_fixture_pid" MONDAY_ROOT="$ROOT")
  if [[ $restore_failure == config ]]; then
    restore_env+=(MONDAY_RESTORE_FIXTURE_BAD_CONFIG=1)
  else
    restore_env+=(MONDAY_RESTORE_FIXTURE_BAD_MEMBERSHIP=1)
  fi
  if env "${restore_env[@]}" "$SCRIPT_DIR/host-rust-lob-restore.sh" \
    --controller "$c2" --root "$ROOT" >/dev/null 2>&1; then
    printf 'restore accepted an invalid production slice %s state\n' "$restore_failure" >&2
    exit 1
  fi
  restore_config_line=$(grep -n -m1 '^verify-config ' "$ROOT/run/restore-fixture.calls" | cut -d: -f1 || true)
  restore_unmask_line=$(grep -n -m1 '^unmask ' "$ROOT/run/restore-fixture.calls" | cut -d: -f1 || true)
  [[ -n $restore_config_line && ( -z $restore_unmask_line || $restore_unmask_line -gt $restore_config_line ) ]] || {
    printf 'restore crossed the production start boundary before slice configuration validation\n' >&2
    exit 1
  }
  if [[ $restore_failure == membership ]]; then
    [[ $(grep -Ec '^start binance-lob-archiver-production@(spot|usdm)\.service$' \
      "$ROOT/run/restore-fixture.calls") -eq 2 ]] || {
      printf 'restore did not reach post-start membership validation\n' >&2
      exit 1
    }
  fi
done
run_restore_health_fixture success
grep -Fq 'enable binance-lob-archiver-production@spot.service' "$restore_calls"
grep -Fq 'enable binance-lob-archiver-production@usdm.service' "$restore_calls"

# Merely finding restore evidence must not suppress containment before the
# active immutable pair is proven.  Break active with a valid receipt present,
# then require fail-closed cleanup to mask every canonical writer.
restore_active="$ROOT/opt/monday/releases/binance-lob-controller/active"
restore_active_target=$(readlink -- "$restore_active")
rm -f -- "$restore_calls" "$restore_active"
ln -s "$ROOT/opt/monday/releases/binance-lob-controller/missing" "$restore_active"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_RESTORE_FIXTURE_SYSTEMD=1 \
  MONDAY_RESTORE_FIXTURE_PID="$restore_fixture_pid" MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-restore.sh" --controller "$c2" --root "$ROOT" \
  >/dev/null 2>&1; then
  printf 'restore evidence suppressed containment for an invalid active pair\n' >&2
  exit 1
fi
for unit in \
  binance-lob-archiver@spot.service binance-lob-archiver@usdm.service \
  binance-lob-archiver-upload@spot.service binance-lob-archiver-upload@usdm.service \
  binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service; do
  grep -Fq "mask $unit" "$restore_calls"
done
rm -f -- "$restore_active"
ln -s "$restore_active_target" "$restore_active"

# A repeated successful restore is a read-only idempotency check.  It must
# verify the live pair/timers/health contract without issuing any systemd
# containment or projection mutation; a drifted projection fails closed and
# likewise leaves the call log untouched.
restore_calls_sha=$(monday_sha256_file "$restore_calls")
MONDAY_CONTROL_PLANE_TEST=1 MONDAY_RESTORE_FIXTURE_SYSTEMD=1 \
  MONDAY_RESTORE_FIXTURE_PID="$restore_fixture_pid" MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-restore.sh" --controller "$c2" --root "$ROOT" >/dev/null
[[ $(monday_sha256_file "$restore_calls") == "$restore_calls_sha" ]]
restore_projection_target=$(readlink -- "$ROOT/opt/monday/bin/monday-rust-lob-recovery-queue")
rm -f -- "$ROOT/opt/monday/bin/monday-rust-lob-recovery-queue"
printf 'idempotency-drift\n' >"$ROOT/opt/monday/bin/monday-rust-lob-recovery-queue"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_RESTORE_FIXTURE_SYSTEMD=1 \
  MONDAY_RESTORE_FIXTURE_PID="$restore_fixture_pid" MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-restore.sh" --controller "$c2" --root "$ROOT" \
  >/dev/null 2>&1; then
  printf 'restore idempotency accepted a drifted controller projection\n' >&2
  exit 1
fi
[[ $(monday_sha256_file "$restore_calls") == "$restore_calls_sha" ]]
rm -f -- "$ROOT/opt/monday/bin/monday-rust-lob-recovery-queue"
ln -s "$restore_projection_target" "$ROOT/opt/monday/bin/monday-rust-lob-recovery-queue"

# Readback reuses the active-C health policy before and after the independent
# OSS phase.  A fixture systemd view supplies process/unit identity while the
# restored health files exercise the positive path and three fail-closed
# policy negatives, including a disabled production unit.
readback_fixture_pid=5353
mkdir -p "$ROOT/proc/$readback_fixture_pid"
rm -f -- "$ROOT/proc/$readback_fixture_pid/exe"
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p2_sha/binance-lob-archiver" \
  "$ROOT/proc/$readback_fixture_pid/exe"
readback_out="$ROOT/data/monday/evidence/readbacks/$c2"
run_readback_fixture() {
  local mode=$1
  rm -rf -- "$readback_out" "$readback_out.sha256"
  case $mode in
    success) : ;;
    unsynced)
      jq '.status = "starting"' "$production_spool_root/spot/health.json" \
        >"$production_spool_root/spot/health.json.tmp"
      mv -f -- "$production_spool_root/spot/health.json.tmp" "$production_spool_root/spot/health.json"
      jq '.status = "starting"' "$production_spool_root/usdm/health.json" \
        >"$production_spool_root/usdm/health.json.tmp"
      mv -f -- "$production_spool_root/usdm/health.json.tmp" "$production_spool_root/usdm/health.json" ;;
    gaps)
      jq '.sequence_gaps = 1' "$production_spool_root/spot/health.json" \
        >"$production_spool_root/spot/health.json.tmp"
      mv -f -- "$production_spool_root/spot/health.json.tmp" "$production_spool_root/spot/health.json"
      jq '.sequence_gaps = 1' "$production_spool_root/usdm/health.json" \
        >"$production_spool_root/usdm/health.json.tmp"
      mv -f -- "$production_spool_root/usdm/health.json.tmp" "$production_spool_root/usdm/health.json" ;;
    nonready)
      jq '.snapshot_ready_count = 0' "$production_spool_root/spot/health.json" \
        >"$production_spool_root/spot/health.json.tmp"
      mv -f -- "$production_spool_root/spot/health.json.tmp" "$production_spool_root/spot/health.json"
      jq '.snapshot_ready_count = 0' "$production_spool_root/usdm/health.json" \
        >"$production_spool_root/usdm/health.json.tmp"
      mv -f -- "$production_spool_root/usdm/health.json.tmp" "$production_spool_root/usdm/health.json" ;;
    disabled) : ;;
    *) return 2 ;;
  esac
  if [[ $mode == disabled ]]; then
    if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_READBACK_FIXTURE_SYSTEMD=1 \
      MONDAY_READBACK_FIXTURE_UNIT_FILE_STATE=disabled MONDAY_READBACK_FIXTURE_PID="$readback_fixture_pid" \
      MONDAY_UPLOAD_STATUS_ROOT="$ROOT/fixture-upload-status-empty" MONDAY_ROOT="$ROOT" \
      "$SCRIPT_DIR/host-rust-lob-readback.sh" --controller "$c2" \
      --transition-receipt "$transition2" --receipt-sha256 "$transition2_sha" --root "$ROOT" \
      >/dev/null 2>&1; then
      printf 'readback accepted a disabled production unit\n' >&2
      exit 1
    fi
  elif [[ $mode == success ]]; then
    MONDAY_CONTROL_PLANE_TEST=1 MONDAY_READBACK_FIXTURE_SYSTEMD=1 \
      MONDAY_READBACK_FIXTURE_PID="$readback_fixture_pid" \
      MONDAY_UPLOAD_STATUS_ROOT="$ROOT/fixture-upload-status-empty" MONDAY_ROOT="$ROOT" \
      "$SCRIPT_DIR/host-rust-lob-readback.sh" --controller "$c2" \
      --transition-receipt "$transition2" --receipt-sha256 "$transition2_sha" --root "$ROOT" \
      >/dev/null
    jq -e '.result == "success" and .unit_file_state_verified == true
      and .health_policy_verified == true
      and .process_identity.spot.unit_file_state == "enabled"
      and .process_identity.usdm.unit_file_state == "enabled"' "$readback_out" >/dev/null
  else
    if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_READBACK_FIXTURE_SYSTEMD=1 \
      MONDAY_READBACK_FIXTURE_PID="$readback_fixture_pid" \
      MONDAY_UPLOAD_STATUS_ROOT="$ROOT/fixture-upload-status-empty" MONDAY_ROOT="$ROOT" \
      "$SCRIPT_DIR/host-rust-lob-readback.sh" --controller "$c2" \
      --transition-receipt "$transition2" --receipt-sha256 "$transition2_sha" --root "$ROOT" \
      >/dev/null 2>&1; then
      printf 'readback accepted invalid health state: %s\n' "$mode" >&2
      exit 1
    fi
  fi
}
run_readback_fixture success
run_readback_fixture unsynced
run_readback_fixture gaps
run_readback_fixture nonready
run_readback_fixture disabled

# Health liveness is independent from stable process identity: a newer
# observed_at is accepted while a backwards/stalled sample is rejected.
health_freshness=$(monday_observe_health_freshness 100 10 0 200 11 120)
read -r health_updated health_mono health_gap health_increment <<<"$health_freshness"
[[ $health_updated == 200 && $health_mono == 11 && $health_gap == 1 && $health_increment == 1 ]]
if monday_observe_health_freshness 200 11 0 199 12 120 >/dev/null 2>&1; then
  printf 'health readback accepted a regressed observed_at\n' >&2
  exit 1
fi

# A SIGKILL after a run-scoped Gate start cannot run the EXIT trap.  The
# bounded unit/spool must remain isolated, with all governed /etc and global
# shadow projections byte-for-byte unchanged; the next serialized Gate owns
# stale-run cleanup and can recover without touching production.
printf '\n# controller revision four fixture\n' >>"$source_dir/host-rust-lob-readback.sh"
p3="$ROOT/p3"; m3="$ROOT/m3.json"
publish_fixture "$p3" "$m3" >/dev/null
c3=$(monday_sha256_file "$m3")
shadow_link_before_sigkill=$(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver-shadow")
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_GATE_FIXTURE_SIGKILL=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c2" \
  --candidate-controller "$c3" --root "$ROOT" >/dev/null 2>&1; then
  printf 'SIGKILL Gate fixture unexpectedly survived\n' >&2
  exit 1
fi
for asset in "${!shadow_before_sha[@]}"; do
  if [[ $asset == *.service ]]; then target="$ROOT/etc/systemd/system/$asset"; else target="$ROOT/etc/monday/$asset"; fi
  [[ $(monday_sha256_file "$(readlink -f -- "$target")") == "${shadow_before_sha[$asset]}" ]] || {
    printf 'SIGKILL Gate changed governed shadow asset %s\n' "$asset" >&2
    exit 1
  }
done
[[ $(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver-shadow") == "$shadow_link_before_sigkill" ]]
stale_gate_dir=$(find "$ROOT/run/monday/rust-lob-gate" -mindepth 1 -maxdepth 1 -type d -print -quit)
[[ -n $stale_gate_dir && -f "$stale_gate_dir/monday-rust-lob-gate-$(basename -- "$stale_gate_dir")-spot.service" ]]
stale_upload_unit="$stale_gate_dir/monday-rust-lob-gate-$(basename -- "$stale_gate_dir")-spot-upload.service"
[[ -f $stale_upload_unit ]]
grep -Fqx 'Restart=no' "$stale_gate_dir/monday-rust-lob-gate-$(basename -- "$stale_gate_dir")-spot.service"
grep -Fqx 'RuntimeMaxSec=900' "$stale_gate_dir/monday-rust-lob-gate-$(basename -- "$stale_gate_dir")-spot.service"
grep -Fqx 'TimeoutStopSec=60' "$stale_gate_dir/monday-rust-lob-gate-$(basename -- "$stale_gate_dir")-spot.service"
grep -Fqx 'Restart=no' "$stale_upload_unit"
grep -Fqx 'RuntimeMaxSec=300' "$stale_upload_unit"
grep -Fqx 'TimeoutStartSec=300' "$stale_upload_unit"
stale_run=$(basename -- "$stale_gate_dir")
[[ -d "$ROOT/data/monday/spool/binance-lob-rust-shadow/gate/$stale_run" ]]
stale_search_unit="$ROOT/run/systemd/system/monday-rust-lob-gate-${stale_run}-spot.service"
[[ -f "$stale_search_unit" && ! -L "$stale_search_unit" ]]
gate_output=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c2" \
  --candidate-controller "$c3" --root "$ROOT")
gate3=$(printf '%s\n' "$gate_output" | sed -n 's/^V2 Gate receipt: //p')
gate3_sha=$(printf '%s\n' "$gate_output" | sed -n 's/^SHA-256: //p')
monday_validate_v2_gate "$gate3" "$c2" "$c3" "$gate3_sha"
[[ ! -e "$stale_gate_dir" && ! -e "$ROOT/data/monday/spool/binance-lob-rust-shadow/gate/$stale_run" ]]

# A Type=simple shadow may briefly report systemd's executor PID before the
# collector process has exec'd.  Startup identity verification waits for two
# consecutive candidate-PID/executable observations instead of failing on
# that transient; a continuously foreign executable remains fail-closed.
printf '\n# controller revision five fixture\n' >>"$source_dir/host-rust-lob-readback.sh"
p4="$ROOT/p4"; m4="$ROOT/m4.json"
publish_fixture "$p4" "$m4" >/dev/null
c4=$(monday_sha256_file "$m4")
identity_active_before=$(monday_active_controller_sha "$ROOT")
identity_payload_before=$(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver")
identity_gate_output=$(MONDAY_CONTROL_PLANE_TEST=1 \
  MONDAY_GATE_FIXTURE_SHADOW_IDENTITY_SEQUENCE=wrong,correct \
  MONDAY_ALLOW_SHORT_GATE_FOR_TESTS=1 MONDAY_GATE_TEST_SECONDS=1 \
  MONDAY_TEST_HEALTH_SETTLE_SECONDS=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c2" \
  --candidate-controller "$c4" --root "$ROOT")
identity_gate=$(printf '%s\n' "$identity_gate_output" | sed -n 's/^V2 Gate receipt: //p')
identity_gate_sha=$(printf '%s\n' "$identity_gate_output" | sed -n 's/^SHA-256: //p')
[[ -f $identity_gate && $identity_gate_sha == "$(monday_sha256_file "$identity_gate")" ]]
monday_validate_v2_gate "$identity_gate" "$c2" "$c4" "$identity_gate_sha"
[[ $(monday_active_controller_sha "$ROOT") == "$identity_active_before" ]]
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == "$identity_payload_before" ]]

printf '\n# controller revision six fixture\n' >>"$source_dir/host-rust-lob-readback.sh"
p5="$ROOT/p5"; m5="$ROOT/m5.json"
publish_fixture "$p5" "$m5" >/dev/null
c5=$(monday_sha256_file "$m5")
identity_failure_output="$ROOT/startup-identity-failure.txt"
if MONDAY_CONTROL_PLANE_TEST=1 \
  MONDAY_GATE_FIXTURE_SHADOW_IDENTITY_SEQUENCE=wrong \
  MONDAY_ALLOW_SHORT_GATE_FOR_TESTS=1 MONDAY_GATE_TEST_SECONDS=1 \
  MONDAY_TEST_HEALTH_SETTLE_SECONDS=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c2" \
  --candidate-controller "$c5" --root "$ROOT" >"$identity_failure_output" 2>&1; then
  printf 'Gate accepted a continuously foreign startup executable\n' >&2
  exit 1
fi
grep -Fq 'startup identity timed out' "$identity_failure_output" || {
  printf 'startup identity timeout was not reported\n' >&2
  exit 1
}
[[ $(monday_active_controller_sha "$ROOT") == "$identity_active_before" ]]
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == "$identity_payload_before" ]]
if find "$ROOT/data/monday/evidence/shadow-gates/$c5" -type f \
  \( -name gate.json -o -name PASSED.sha256 \) -print -quit 2>/dev/null | grep -q .; then
  printf 'failed startup identity Gate left an authoritative receipt\n' >&2
  exit 1
fi

# The governed shadow source unit has one optional soak EnvironmentFile and no
# other extension points.  Unknown commands or additional EnvironmentFiles
# must be rejected before the candidate can render a run-scoped unit.
shadow_unit_source="$source_dir/binance-lob-archiver-rust@.service"
shadow_unit_saved="$ROOT/binance-lob-archiver-rust@.service.saved"
cp -p -- "$shadow_unit_source" "$shadow_unit_saved"
for mutation in execstartpost environmentfile; do
  cp -p -- "$shadow_unit_saved" "$shadow_unit_source"
  chmod u+w "$shadow_unit_source"
  case "$mutation" in
    execstartpost) printf 'ExecStartPost=/bin/true\n' >>"$shadow_unit_source" ;;
    environmentfile) printf 'EnvironmentFile=/run/monday/foreign.env\n' >>"$shadow_unit_source" ;;
  esac
  bad_payload="$ROOT/p-bad-shadow-$mutation"; bad_manifest="$ROOT/m-bad-shadow-$mutation.json"
  publish_fixture "$bad_payload" "$bad_manifest" >/dev/null
  bad_controller=$(monday_sha256_file "$bad_manifest")
  if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
    "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c3" \
    --candidate-controller "$bad_controller" --root "$ROOT" >/dev/null 2>&1; then
    printf 'Gate accepted an unallowlisted shadow %s directive\n' "$mutation" >&2
    exit 1
  fi
done
cp -p -- "$shadow_unit_saved" "$shadow_unit_source"

# The upload drain is a separately rendered transient unit, so it must reject
# the same unallowlisted control directives before any candidate writer starts.
shadow_upload_source="$source_dir/binance-lob-archiver-rust-upload@.service"
shadow_upload_saved="$ROOT/binance-lob-archiver-rust-upload@.service.saved"
cp -p -- "$shadow_upload_source" "$shadow_upload_saved"
printf 'ExecStartPost=/bin/true\n' >>"$shadow_upload_source"
bad_payload="$ROOT/p-bad-shadow-upload"; bad_manifest="$ROOT/m-bad-shadow-upload.json"
publish_fixture "$bad_payload" "$bad_manifest" >/dev/null
bad_controller=$(monday_sha256_file "$bad_manifest")
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c2" \
  --candidate-controller "$bad_controller" --root "$ROOT" >/dev/null 2>&1; then
  printf 'Gate accepted an unallowlisted shadow upload directive\n' >&2
  exit 1
fi
cp -p -- "$shadow_upload_saved" "$shadow_upload_source"

# Candidate shadow identity is fixed by the Gate contract.  Each foreign
# spool/endpoint/profile/shard mutation must be rejected before any writer is
# started or any global shadow projection is touched.
shadow_env_source="$source_dir/binance-lob-archiver-rust-spot.env"
shadow_env_saved="$ROOT/binance-lob-archiver-rust-spot.env.saved"
cp -p -- "$shadow_env_source" "$shadow_env_saved"
for mutation in spool endpoint profile shard; do
  rm -f -- "$shadow_env_source"
  cp -p -- "$shadow_env_saved" "$shadow_env_source"
  case "$mutation" in
    spool) sed -i.bak 's|^SPOOL_DIR=.*$|SPOOL_DIR=/data/monday/spool/binance-lob/spot|' "$shadow_env_source" ;;
    endpoint) sed -i.bak 's|^OSS_ENDPOINT=.*$|OSS_ENDPOINT=foreign.endpoint.example|' "$shadow_env_source" ;;
    profile) sed -i.bak 's|^ALIYUN_PROFILE=.*$|ALIYUN_PROFILE=foreign-profile|' "$shadow_env_source" ;;
    shard) sed -i.bak 's|^SHARD_ID=.*$|SHARD_ID=foreign-shard|' "$shadow_env_source" ;;
  esac
  rm -f -- "$shadow_env_source.bak"
  bad_payload="$ROOT/p-bad-$mutation"; bad_manifest="$ROOT/m-bad-$mutation.json"
  publish_fixture "$bad_payload" "$bad_manifest" >/dev/null
  bad_controller=$(monday_sha256_file "$bad_manifest")
  if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
    "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller "$c2" \
    --candidate-controller "$bad_controller" --root "$ROOT" >/dev/null 2>&1; then
    printf 'Gate accepted foreign shadow %s identity\n' "$mutation" >&2
    exit 1
  fi
done
rm -f -- "$shadow_env_source"
cp -p -- "$shadow_env_saved" "$shadow_env_source"

# If the second production lane fails during restore, both lanes must be
# contained and the failed attempt must not emit a success receipt.
restore_evidence="$ROOT/data/monday/evidence/restores/$c2"
rm -f -- "$restore_evidence/restore.json" "$restore_evidence/restore.json.sha256"
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_RESTORE_FIXTURE_SYSTEMD=1 \
  MONDAY_RESTORE_FIXTURE_FAIL_USDM=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-restore.sh" --controller "$c2" --root "$ROOT" >/dev/null 2>&1; then
  printf 'restore second-lane fault unexpectedly succeeded\n' >&2
  exit 1
fi
restore_calls="$ROOT/run/restore-fixture.calls"
for unit in binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service; do
  grep -Fq "stop $unit" "$restore_calls"
  grep -Fq "disable $unit" "$restore_calls"
  grep -Fq "mask $unit" "$restore_calls"
done
for unit in \
  binance-lob-archiver@spot.service binance-lob-archiver@usdm.service \
  binance-lob-archiver-upload@spot.service binance-lob-archiver-upload@usdm.service; do
  grep -Fq "stop $unit" "$restore_calls"
  grep -Fq "disable $unit" "$restore_calls"
  grep -Fq "mask $unit" "$restore_calls"
done
[[ ! -e $restore_evidence/restore.json && ! -e $restore_evidence/restore.json.sha256 ]]
[[ $(monday_active_controller_sha "$ROOT") == "$c2" ]]

# Shared OSS triplet readback rejects stale status, foreign prefixes, missing
# objects, and a marker whose digest is internally consistent but whose bytes
# are not the canonical data SHA plus one newline.
triplet_root="$ROOT/triplet-fixture"; mkdir -p "$triplet_root"
triplet_data="$triplet_root/part-1.jsonl.zst"
printf 'fixture-data\n' >"$triplet_data"
triplet_data_sha=$(monday_sha256_file "$triplet_data")
triplet_manifest="$triplet_root/part-1.jsonl.zst.manifest.json"
jq -cn --arg sha "$triplet_data_sha" \
  '{schema:"binance.market_tape.v2",market:"spot",dataset:"spot_all",shard_id:"all",file:"part-1.jsonl.zst",sha256:$sha,session_id:"fixture-session",catalog_sha256:"fixture-catalog",received_at:"2026-08-27T00:00:00Z"}' \
  >"$triplet_manifest"
triplet_success="$triplet_root/part-1.jsonl.zst._SUCCESS"
printf '%s\n' "$triplet_data_sha" >"$triplet_success"
triplet_success_sha=$(monday_sha256_file "$triplet_success")
triplet_status="$triplet_root/upload-status.json"
triplet_prefix='lake/raw/venue=binance/market=spot/dataset=spot_all/shard=all'
triplet_object_prefix="$triplet_prefix/date=2026-08-28/hour=05"
triplet_uri="oss://bucket/$triplet_object_prefix/part-1.jsonl.zst"
triplet_now_ns=$(( $(date +%s%N) - 120000000000 ))
triplet_now=$(monday_epoch_ns_rfc3339 "$triplet_now_ns")
triplet_start_ns=$((triplet_now_ns - 3600000000000))
triplet_end_ns=$((triplet_start_ns + 300000000000))
jq --argjson start "$triplet_start_ns" --argjson end "$triplet_end_ns" \
  '.start_received_at_ns=$start | .end_received_at_ns=$end' \
  "$triplet_manifest" >"$triplet_manifest.tmp"
mv -f -- "$triplet_manifest.tmp" "$triplet_manifest"
triplet_manifest_sha=$(monday_sha256_file "$triplet_manifest")
jq -cn --arg now "$triplet_now" --arg uri "$triplet_uri" --arg prefix "$triplet_object_prefix" \
  --arg data "$triplet_data_sha" --arg manifest "$triplet_manifest_sha" --arg success "$triplet_success_sha" \
  --argjson start "$triplet_start_ns" --argjson end "$triplet_end_ns" \
  '{last_success_at:$now,last_error:null,last_error_at:null,failure_count:0,discovery_failed:false,pending_batches:0,failed_batches:[],last_uploaded_triplet:{data_uri:$uri,object_prefix:$prefix,data_sha256:$data,manifest_sha256:$manifest,success_sha256:$success,uploaded_at:$now,start_received_at_ns:$start,end_received_at_ns:$end}}' \
  >"$triplet_status"
copy_triplet_fixture() {
  local uri=$1 target=$2 object
  object=${uri##*/}
  [[ ${TRIPLET_FIXTURE_MISSING:-0} != 1 ]] || return 1
  cp -p -- "$triplet_root/$object" "$target"
}
triplet_tmp="$ROOT/triplet-readback-tmp"
triplet_readback=$(monday_verify_upload_triplet_readback "$triplet_status" spot spot_all bucket "$triplet_prefix" \
  "$triplet_tmp" "$triplet_now" copy_triplet_fixture fixture-session 0)
[[ $(jq -r '.data_sha256' <<<"$triplet_readback") == "$triplet_data_sha" ]]
production_readback=$(monday_verify_upload_triplet_readback "$triplet_status" spot spot_all bucket "$triplet_prefix" \
  "$triplet_tmp" "$triplet_now" copy_triplet_fixture fixture-session "$triplet_start_ns")
[[ $(jq -r '.end_received_at_ns - .start_received_at_ns' <<<"$production_readback") == 300000000000 ]]
cp -p -- "$triplet_manifest" "$triplet_manifest.full-cadence"
partial_end_ns=$((triplet_start_ns + 1000000000))
jq --argjson end "$partial_end_ns" '.end_received_at_ns=$end' \
  "$triplet_manifest.full-cadence" >"$triplet_manifest"
partial_manifest_sha=$(monday_sha256_file "$triplet_manifest")
jq --arg sha "$partial_manifest_sha" --argjson end "$partial_end_ns" \
  '.last_uploaded_triplet.manifest_sha256=$sha | .last_uploaded_triplet.end_received_at_ns=$end' \
  "$triplet_status" >"$triplet_status.partial-cadence"
if monday_verify_upload_triplet_readback "$triplet_status.partial-cadence" spot spot_all bucket "$triplet_prefix" \
  "$triplet_tmp" "$triplet_now" copy_triplet_fixture fixture-session "$triplet_start_ns" >/dev/null 2>&1; then
  printf 'production triplet readback accepted a partial 300-second segment\n' >&2
  exit 1
fi
mv -f -- "$triplet_manifest.full-cadence" "$triplet_manifest"
# A historical capture may predate the recovery job; only the upload commit
# must be newer than its cutoff.
historical_readback=$(monday_verify_upload_triplet_readback "$triplet_status" spot spot_all bucket "$triplet_prefix" \
  "$triplet_tmp" "$triplet_now" copy_triplet_fixture fixture-session 0)
[[ $(jq -r '.session_id' <<<"$historical_readback") == fixture-session ]]
# failure_count is a cumulative audit counter.  A healthy retry can carry a
# non-zero historical count, while current error or pending fields still fail.
retry_status="$triplet_root/upload-status.retry.json"
jq '.failure_count = 7' "$triplet_status" >"$retry_status"
retry_readback=$(monday_verify_upload_triplet_readback "$retry_status" spot spot_all bucket "$triplet_prefix" \
  "$triplet_tmp" "$triplet_now" copy_triplet_fixture fixture-session 0)
[[ $(jq -r '.failure_count' <<<"$retry_readback") == 7 ]]
if jq '.failure_count = -1' "$retry_status" >"$retry_status.negative" \
  && monday_verify_upload_triplet_readback "$retry_status.negative" spot spot_all bucket "$triplet_prefix" \
      "$triplet_tmp" "$triplet_now" copy_triplet_fixture fixture-session 0 >/dev/null 2>&1; then
  printf 'triplet readback accepted a negative cumulative failure_count\n' >&2
  exit 1
fi
for retry_mutation in last_error pending_batches; do
  case "$retry_mutation" in
    last_error) jq '.last_error = "retry failed"' "$retry_status" >"$retry_status.$retry_mutation" ;;
    pending_batches) jq '.pending_batches = 1' "$retry_status" >"$retry_status.$retry_mutation" ;;
  esac
  if monday_verify_upload_triplet_readback "$retry_status.$retry_mutation" spot spot_all bucket "$triplet_prefix" \
      "$triplet_tmp" "$triplet_now" copy_triplet_fixture fixture-session 0 >/dev/null 2>&1; then
    printf 'triplet readback accepted retry status with current %s\n' "$retry_mutation" >&2
    exit 1
  fi
done
if monday_verify_upload_triplet_readback "$triplet_status" spot spot_all bucket "$triplet_prefix" \
    "$triplet_tmp" "$triplet_now" copy_triplet_fixture fixture-session "$triplet_now_ns" >/dev/null 2>&1; then
  printf 'triplet readback ignored the minimum capture cutoff\n' >&2
  exit 1
fi

bad_prefixes=(
  "$triplet_prefix/date=2026-02-29/hour=05"
  "$triplet_prefix/date=2026-08-28/hour=24"
  "$triplet_prefix/date=2026-08-28/hour=05/extra"
  "$triplet_prefix/date=2026-08-28//hour=05"
  "$triplet_prefix/date=2026-08-28/hour=05%2Fforeign"
  "$triplet_prefix/date=2026-08-28/hour=05/../foreign"
  "$triplet_prefix/date=2026-08-28/hour=05/./foreign"
)
bad_prefixes+=("$(printf '%s\\evil' "$triplet_object_prefix")")
for bad_prefix in "${bad_prefixes[@]}"; do
  if jq --arg prefix "$bad_prefix" '.last_uploaded_triplet.object_prefix=$prefix' "$triplet_status" \
      >"$triplet_status.bad-prefix" \
    && monday_verify_upload_triplet_readback "$triplet_status.bad-prefix" spot spot_all bucket "$triplet_prefix" \
      "$triplet_tmp" "$triplet_now" copy_triplet_fixture "" 0 >/dev/null 2>&1; then
    printf 'triplet readback accepted malformed object prefix: %s\n' "$bad_prefix" >&2
    exit 1
  fi
done
bad_data_uri="oss://bucket/$triplet_object_prefix/../part-1.jsonl.zst"
if jq --arg uri "$bad_data_uri" '.last_uploaded_triplet.data_uri=$uri' "$triplet_status" \
    >"$triplet_status.bad-uri" \
  && monday_verify_upload_triplet_readback "$triplet_status.bad-uri" spot spot_all bucket "$triplet_prefix" \
    "$triplet_tmp" "$triplet_now" copy_triplet_fixture "" 0 >/dev/null 2>&1; then
  printf 'triplet readback accepted a traversing data URI\n' >&2
  exit 1
fi
if monday_verify_upload_triplet_readback "$triplet_status" spot spot_all bucket "$triplet_prefix" \
    "$triplet_tmp" "$triplet_now" copy_triplet_fixture changed-session >/dev/null 2>&1; then
  printf 'triplet readback accepted a mismatched current health session\n' >&2
  exit 1
fi
if jq '.last_success_at = "2000-01-01T00:00:00Z"' "$triplet_status" >"$triplet_status.stale" \
  && monday_verify_upload_triplet_readback "$triplet_status.stale" spot spot_all bucket "$triplet_prefix" \
    "$triplet_tmp" "$triplet_now" copy_triplet_fixture >/dev/null 2>&1; then
  printf 'triplet readback accepted stale last_success_at\n' >&2
  exit 1
fi
if jq '.last_success_at = "2999-01-01T00:00:00Z"' "$triplet_status" >"$triplet_status.future" \
  && monday_verify_upload_triplet_readback "$triplet_status.future" spot spot_all bucket "$triplet_prefix" \
    "$triplet_tmp" "$triplet_now" copy_triplet_fixture >/dev/null 2>&1; then
  printf 'triplet readback accepted future last_success_at\n' >&2
  exit 1
fi
if jq '.last_uploaded_triplet.uploaded_at = "2999-01-01T00:00:00Z"' "$triplet_status" >"$triplet_status.future-triplet" \
  && monday_verify_upload_triplet_readback "$triplet_status.future-triplet" spot spot_all bucket "$triplet_prefix" \
    "$triplet_tmp" "$triplet_now" copy_triplet_fixture >/dev/null 2>&1; then
  printf 'triplet readback accepted future triplet timestamp\n' >&2
  exit 1
fi
cp -p -- "$triplet_manifest" "$triplet_manifest.valid"
cp -p -- "$triplet_status" "$triplet_status.valid"
future_manifest_end_ns=$(( $(date +%s%N) + 3600000000000 ))
jq --argjson end "$future_manifest_end_ns" \
  '.end_received_at_ns=$end' "$triplet_manifest.valid" >"$triplet_manifest"
future_manifest_sha=$(monday_sha256_file "$triplet_manifest")
jq --arg sha "$future_manifest_sha" --argjson end "$future_manifest_end_ns" \
  '.last_uploaded_triplet.manifest_sha256=$sha | .last_uploaded_triplet.end_received_at_ns=$end' \
  "$triplet_status.valid" >"$triplet_status.future-manifest"
if monday_verify_upload_triplet_readback "$triplet_status.future-manifest" spot spot_all bucket "$triplet_prefix" \
    "$triplet_tmp" "$triplet_now" copy_triplet_fixture >/dev/null 2>&1; then
  printf 'triplet readback accepted a future manifest boundary\n' >&2
  exit 1
fi
cp -p -- "$triplet_manifest.valid" "$triplet_manifest"
cp -p -- "$triplet_status.valid" "$triplet_status"
if jq '.last_uploaded_triplet.data_uri |= sub("oss://bucket"; "oss://foreign")' "$triplet_status" \
  >"$triplet_status.foreign" \
  && monday_verify_upload_triplet_readback "$triplet_status.foreign" spot spot_all bucket "$triplet_prefix" \
    "$triplet_tmp" "$triplet_now" copy_triplet_fixture >/dev/null 2>&1; then
  printf 'triplet readback accepted a foreign bucket\n' >&2
  exit 1
fi
if jq '.last_uploaded_triplet.object_prefix = "foreign/prefix"' "$triplet_status" \
  >"$triplet_status.foreign-prefix" \
  && monday_verify_upload_triplet_readback "$triplet_status.foreign-prefix" spot spot_all bucket "$triplet_prefix" \
    "$triplet_tmp" "$triplet_now" copy_triplet_fixture >/dev/null 2>&1; then
  printf 'triplet readback accepted a foreign object prefix\n' >&2
  exit 1
fi
if jq '.last_error = "status drift"' "$triplet_status" >"$triplet_status.drift" \
  && monday_verify_upload_triplet_readback "$triplet_status.drift" spot spot_all bucket "$triplet_prefix" \
    "$triplet_tmp" "$triplet_now" copy_triplet_fixture >/dev/null 2>&1; then
  printf 'triplet readback accepted upload-status drift\n' >&2
  exit 1
fi
printf 'not-the-data-sha\n' >"$triplet_success"
wrong_success_sha=$(monday_sha256_file "$triplet_success")
jq --arg sha "$wrong_success_sha" \
  '.last_uploaded_triplet.success_sha256 = $sha' "$triplet_status" >"$triplet_status.bad-success"
if monday_verify_upload_triplet_readback "$triplet_status.bad-success" spot spot_all bucket "$triplet_prefix" \
  "$triplet_tmp" "$triplet_now" copy_triplet_fixture >/dev/null 2>&1; then
  printf 'triplet readback accepted non-canonical success bytes\n' >&2
  exit 1
fi
printf '%s\n' "$triplet_data_sha" >"$triplet_success"
TRIPLET_FIXTURE_MISSING=1
if monday_verify_upload_triplet_readback "$triplet_status" spot spot_all bucket "$triplet_prefix" \
    "$triplet_tmp" "$triplet_now" copy_triplet_fixture >/dev/null 2>&1; then
  printf 'triplet readback accepted a missing OSS object\n' >&2
  exit 1
fi

# The local operator is the only Cloud Assistant entry point. Its dry run must
# carry an exact controller path and never expose a second routing mode.
operator="$SCRIPT_DIR/rust-lob-control-plane.sh"
grep -Fq 'timeout_seconds=300' "$operator" || {
  printf 'LOB Gate preflight operator timeout is not capped at 300 seconds\n' >&2
  exit 1
}
candidate=$(printf 'b%.0s' {1..64})
operator_json=$(MONDAY_CONTROL_PLANE_DRY_RUN=1 "$operator" gate \
  --instance i-fixture --from-controller "$c1" \
  --candidate-controller "$candidate")
jq -e --arg controller "$candidate" \
  '.operation == "gate" and .controller == $controller
   and (.command | contains(("/opt/monday/releases/binance-lob-controller/" + $controller)))
   and (.command | contains("timeout --signal=TERM --kill-after=240s 2100s"))
   and (.command | contains("--preflight-only") | not)
   and .preflight_only == false and .timeout_seconds == 2400
   and .host_timeout_seconds == 2100 and .host_kill_after_seconds == 240
   and (.host_timeout_seconds + .host_kill_after_seconds < .timeout_seconds)
   and .production_changed == false' <<<"$operator_json" >/dev/null
operator_preflight_json=$(MONDAY_CONTROL_PLANE_DRY_RUN=1 "$operator" gate \
  --instance i-fixture --from-controller "$c1" \
  --candidate-controller "$candidate" --preflight-only)
jq -e --arg controller "$candidate" \
  '.operation == "gate" and .controller == $controller
   and .preflight_only == true and .timeout_seconds == 300
   and .host_timeout_seconds == 0 and .host_kill_after_seconds == 0
   and .production_changed == false
   and (.command | contains(("/opt/monday/releases/binance-lob-controller/" + $controller)))
   and (.command | contains("--preflight-only"))' \
  <<<"$operator_preflight_json" >/dev/null
if MONDAY_CONTROL_PLANE_DRY_RUN=1 "$operator" cutover \
  --instance i-fixture --from direct --to "$candidate" \
  --gate-receipt /data/gate.json --gate-sha256 "$candidate" --preflight-only \
  >/dev/null 2>&1; then
  printf 'operator accepted --preflight-only for a non-Gate operation\n' >&2
  exit 1
fi
if MONDAY_CONTROL_PLANE_DRY_RUN=1 "$operator" unknown >/dev/null 2>&1; then
  printf 'operator accepted an unknown operation\n' >&2
  exit 1
fi
for path in \
  "$operator" "$SCRIPT_DIR/publish-rust-lob-pair-release.sh" \
  "$SCRIPT_DIR/host-rust-lob-controller-release.sh" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" "$SCRIPT_DIR/host-rust-lob-cutover.sh" \
  "$SCRIPT_DIR/host-rust-lob-restore.sh" "$SCRIPT_DIR/host-rust-lob-readback.sh" \
  "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"; do
  for forbidden in \
    "$(printf '%s%s' controller- apply)" \
    "$(printf '%s%s' adopt- production)" \
    "$(printf '%s%s' invoke-rust-lob- operation)" \
    "$(printf '%s%s' deploy-rust-lob- release)" \
    "$(printf '%s%s' controller_release. v1)" \
    "$(printf '%s%s' shadow_gate. v4)"; do
    if grep -Fq "$forbidden" "$path"; then
      printf 'obsolete control-plane routing remains in %s\n' "$path" >&2
      exit 1
    fi
  done
done

# Production-root dry coverage: the real root must join to /opt and /data
# exactly (never //opt or //data), while every host action rejects test mode
# against /.  This exercises each direct path before any filesystem read.
[[ $(monday_root_join / opt/monday) == /opt/monday ]]
[[ $(monday_root_join / data/monday) == /data/monday ]]
[[ $(monday_sha256_file "$production_spool_root/spot/upload-status.json") == "$production_spot_status_sha" ]]
[[ $(monday_sha256_file "$production_spool_root/usdm/upload-status.json") == "$production_usdm_status_sha" ]]
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT=/ \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" --from-controller direct \
  --candidate-controller "$c1" --root / >/dev/null 2>&1; then
  printf 'Gate accepted an unsafe production root in test mode\n' >&2
  exit 1
fi
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT=/ \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from direct --to "$c1" \
  --gate-receipt /data/gate.json --gate-sha256 "$(printf '%064d' 0)" --root / >/dev/null 2>&1; then
  printf 'Cutover accepted an unsafe production root in test mode\n' >&2
  exit 1
fi
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT=/ \
  "$SCRIPT_DIR/host-rust-lob-restore.sh" --controller "$c1" --root / >/dev/null 2>&1; then
  printf 'Restore accepted an unsafe production root in test mode\n' >&2
  exit 1
fi
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT=/ \
  "$SCRIPT_DIR/host-rust-lob-readback.sh" --controller "$c1" \
  --transition-receipt /data/transition.json --receipt-sha256 "$(printf '%064d' 0)" --root / >/dev/null 2>&1; then
  printf 'Readback accepted an unsafe production root in test mode\n' >&2
  exit 1
fi

printf 'V2 Gate contract passed\n'
