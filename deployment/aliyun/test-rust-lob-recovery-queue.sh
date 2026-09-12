#!/usr/bin/env bash
# shellcheck disable=SC2016,SC2034 # Contract strings and globals are consumed by sourced recovery functions.
set -Eeuo pipefail
export LC_ALL=C
trap 'printf "Recovery test failed: status=%s line=%s\n" "$?" "$LINENO" >&2' ERR
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
RECOVERY="$SCRIPT_DIR/host-rust-lob-recovery-queue.sh"
[[ -x $RECOVERY ]]
bash -n "$RECOVERY"
if "$RECOVERY" obsolete spot >/dev/null 2>&1; then
  printf 'recovery queue retained an unknown action\n' >&2
  exit 1
fi
grep -Fq "active V2 controller is required" "$RECOVERY"
grep -Fq 'monday.rust_lob_controller_release.v2' "$RECOVERY"
grep -Fq 'monday.rust_lob_controller_release.v2' "$RECOVERY"
grep -Fq 'needs_recovery_isolation' "$RECOVERY"
grep -Fq 'has_undrained_complete_segments' "$RECOVERY"
if grep -Fq 'if ! has_incomplete_parts "$CANONICAL_SPOOL"' "$RECOVERY"; then
  printf 'recovery isolate still skips complete undrained segments\n' >&2
  exit 1
fi
projection_contract='"$ACTIVE_CONTROLLER/deployment/binance-lob-archiver-production-$MARKET.env"'
resolved_contract='secure_regular_file "$installed_env" 0'
obsolete_contract='secure_regular_file "$ENV_FILE" 0'
grep -Fq 'installed env is not the active controller projection' "$RECOVERY"
grep -Fq "$projection_contract" "$RECOVERY"
grep -Fq "$resolved_contract" "$RECOVERY"
if grep -Fq "$obsolete_contract" "$RECOVERY"; then
  printf 'recovery queue still rejects the governed environment projection symlink\n' >&2
  exit 1
fi
printf 'V2 recovery queue contract passed\n'

# Exercise the real result writer.  The failed-readback path must still commit
# a valid terminal receipt when no successful triplet was produced.
fixture=$(readlink -f -- "$(mktemp -d)")
trap 'rm -rf -- "$fixture"' EXIT
# shellcheck disable=SC1090,SC1091
. "$RECOVERY"
JOB_ID='fixture-spot'
MARKET=spot
JOB_RELEASE_SHA256=$(printf '%064d' 1)
JOB_BUNDLE_SHA256=$(printf '%064d' 2)
JOB_SOURCE_REVISION=$(printf '%040d' 3)
JOB_ENV_SHA256=$(printf '%064d' 4)
JOB_STARTED_AT=2026-09-02T14:16:00Z
UPLOAD_TRIPLET_READBACK='{}'
write_result "$fixture/failed.json" failed upload-readback 'readback failed'
jq -e '.result == "failed" and .upload_triplet_readback == {}' "$fixture/failed.json" >/dev/null
UPLOAD_TRIPLET_READBACK='{"data_sha256":"verified-fixture"}'
write_result "$fixture/passed.json" passed upload-readback-ok 'readback passed'
jq -e '.result == "passed" and .upload_triplet_readback.data_sha256 == "verified-fixture"' \
  "$fixture/passed.json" >/dev/null
unset UPLOAD_TRIPLET_READBACK
write_result "$fixture/unset.json" failed interrupted 'interrupted before readback'
jq -e '.upload_triplet_readback == {}' "$fixture/unset.json" >/dev/null

# The privileged caller owns the private readback directory.  Its unprivileged
# OSS child returns bytes on stdout and never receives a local destination.
RELEASE_ENV_FILE="$fixture/recovery.env"
printf '%s\n' 'ALIYUN_PROFILE=ecs-role' 'OSS_ENDPOINT=oss-ap-northeast-1-internal.aliyuncs.com' \
  'OSS_REGION=ap-northeast-1' >"$RELEASE_ENV_FILE"
SAFE_PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
# shellcheck disable=SC2317,SC2329 # Called by the real sourced copy_active_oss function.
runuser() {
  [[ "$1 $2 $3 $4 $5" == '--user hftcollector -- env -i' ]] || return 1
  shift 5
  [[ "$1" == HOME=/var/lib/hft-collector && "$2" == "PATH=$SAFE_PATH" \
    && "$3" == ALIYUN_PROFILE=ecs-role ]] || return 1
  shift 3
  [[ "$1 $2 $3 $4" == '/usr/local/bin/aliyun ossutil cat oss://fixture/data' ]] || return 1
  [[ "$*" != *"$fixture"* ]] || return 1
  printf 'binary\000fixture\377\n'
}
mkdir -m 0700 "$fixture/private"
# shellcheck disable=SC2218 # Imported above; a later scenario replaces this function.
copy_active_oss oss://fixture/data "$fixture/private/data"
printf 'binary\000fixture\377\n' >"$fixture/expected"
cmp "$fixture/expected" "$fixture/private/data"
[[ $(stat -c %a "$fixture/private") == 700 ]]
# shellcheck disable=SC2317,SC2329 # Injected failure consumed by copy_active_oss.
runuser() { return 1; }
if copy_active_oss oss://fixture/data "$fixture/private/failed"; then
  printf 'recovery accepted a failed OSS download\n' >&2
  exit 1
fi
printf 'Recovery result and private OSS readback behavior passed\n'

# A serialization failure in an if/|| context must not publish an empty result,
# and a later failure must never replace a committed success.
UPLOAD_TRIPLET_READBACK=invalid-json
if (write_result "$fixture/invalid.json" failed test invalid) >/dev/null 2>&1; then
  printf 'result writer accepted malformed triplet JSON\n' >&2; exit 1
fi
[[ ! -e $fixture/invalid.json ]]
original_result_sha=$(sha256sum "$fixture/passed.json" | awk '{print $1}')
UPLOAD_TRIPLET_READBACK='{}'
if (write_result "$fixture/passed.json" failed test overwrite) >/dev/null 2>&1; then
  printf 'result writer replaced a committed success\n' >&2; exit 1
fi
[[ $(sha256sum "$fixture/passed.json" | awk '{print $1}') == "$original_result_sha" ]]

# Keep the production identity/transition validators as the seam: these tests
# build real immutable controller directories and use the real release verifier.
# The already-tested authoritative Gate validator is replaced by a strict
# fixture assertion, so this suite performs no host transition or network call.
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
eval "$(declare -f monday_validate_v2_transition | sed '1s/monday_validate_v2_transition/fixture_real_transition_validator/')"
configure_paths "$fixture/host"
MARKET=spot
market_paths
mkdir -p "$RELEASE_ROOT" "$CONTROLLER_RELEASE_ROOT" "$QUEUE_MARKET_ROOT" \
  "$EVIDENCE_ROOT" "$LOCK_ROOT" "$CANONICAL_SPOOL" "$ROOT_PREFIX/tmp"
fixture_uid=$(id -u); fixture_gid=$(id -g)
eval "$(declare -f secure_regular_file | sed '1s/secure_regular_file/fixture_real_regular_file/')"
eval "$(declare -f secure_directory | sed '1s/secure_directory/fixture_real_directory/')"
secure_regular_file() { fixture_real_regular_file "$1" "$fixture_uid"; }
secure_directory() { fixture_real_directory "$1" "$fixture_uid" "$fixture_gid"; }
ensure_root_directory() {
  if [[ -e $1 || -L $1 ]]; then secure_directory "$1" "$fixture_uid" "$fixture_gid";
  else mkdir -m 0750 "$1"; fi
}
id() {
  if [[ ${2:-} == hftcollector ]]; then
    case "$1" in -u) printf '%s\n' "$fixture_uid" ;; -g) printf '%s\n' "$fixture_gid" ;; *) return 1 ;; esac
  else command id "$@"; fi
}
systemctl() {
  [[ "$*" == 'start --no-block binance-lob-archiver-recovery@spot.service' ]] || return 1
  printf '%s\n' "$*" >>"$fixture/service.calls"
}
if ! command -v flock >/dev/null 2>&1; then
  # macOS has no flock executable.  Fixture lifecycle operations are serial;
  # Linux runs the real lock commands while this platform tests the same state
  # transitions without claiming a kernel-lock contention check.
  flock() { return 0; }
fi
runuser() {
  [[ "$1 $2 $3" == '--user hftcollector --' ]] || return 1
  shift 3
  "$@"
}

printf '#!/usr/bin/env bash\nfixture_root=%q\n' "$fixture" >"$fixture/payload"
cat >>"$fixture/payload" <<'PAYLOAD'
set -Eeuo pipefail
printf '%s %s\n' "$1" "$SPOOL_DIR" >>"$fixture_root/payload.calls"
case $1 in
  --recover-parts-only)
    [[ ! -e $SPOOL_DIR/.fixture-fail-recover ]] || exit 71
    [[ ! -e $RECOVERY_BACKUP_DIR ]] || exit 72
    mkdir "$RECOVERY_BACKUP_DIR"
    find "$SPOOL_DIR" -type f \( -name '*.jsonl.part' -o -name '*.zst.tmp' \) \
      -exec cp '{}' "$RECOVERY_BACKUP_DIR/" \;
    printf '{"fixture":"preserved-input"}\n' >"$RECOVERY_BACKUP_DIR/receipt.json"
    find "$SPOOL_DIR" -type f \( -name '*.jsonl.part' -o -name '*.zst.tmp' \) -delete
    printf 'recovered\n' >"$SPOOL_DIR/part-900.jsonl.zst"
    printf '{}\n' >"$SPOOL_DIR/part-900.jsonl.zst.manifest.json"
    printf 'success\n' >"$SPOOL_DIR/part-900.jsonl.zst._SUCCESS"
    ;;
  --upload-only)
    [[ ! -e $SPOOL_DIR/.fixture-fail-upload ]] || exit 73
    find "$SPOOL_DIR" -type f -name 'part-*' -print >>"$fixture_root/uploaded-files"
    find "$SPOOL_DIR" -type f -name 'part-*' -delete
    cp "$fixture_root/status.after.json" "$SPOOL_DIR/upload-status.json"
    ;;
  *) exit 2 ;;
esac
PAYLOAD
chmod 0755 "$fixture/payload"
fixture_payload=$(sha256sum "$fixture/payload" | awk '{print $1}')
mkdir "$RELEASE_ROOT/$fixture_payload"
cp "$fixture/payload" "$RELEASE_ROOT/$fixture_payload/binance-lob-archiver"
fixture_source_old=$(printf '%040d' 11)
fixture_source_new=$(printf '%040d' 12)
fixture_bundle_old=$(printf '%064d' 13)
fixture_bundle_new=$(printf '%064d' 14)

fixture_controller() {
  local name=$1 source=$2 bundle=$3 root="$fixture/controller-$1" asset runtime controller
  mkdir -p "$root/deployment"
  while IFS= read -r asset; do
    cp "$SCRIPT_DIR/$asset" "$root/deployment/$asset"
  done < <(printf '%s\n' "$(monday_runtime_assets)" "$(monday_controller_assets)" | sort -u)
  cat >"$root/deployment/binance-lob-archiver-production-spot.env" <<EOF
MARKET=spot
DATASET=spot_all
SHARD_ID=all
SPOOL_DIR=$CANONICAL_SPOOL
SNAPSHOT_LIMIT=5000
ZSTD_TIMEOUT_SECONDS=60
OSS_BUCKET=monday-lob-apne1-1045353359
OSS_ENDPOINT=oss-ap-northeast-1-internal.aliyuncs.com
OSS_REGION=ap-northeast-1
ALIYUN_PROFILE=ecs-role
OSS_COPY_TIMEOUT_SECONDS=60
EOF
  runtime=$(monday_rust_lob_runtime_contract_sha256 "$root/deployment")
  jq -cSn --arg payload "$fixture_payload" --arg runtime "$runtime" --arg source "$source" \
    --arg bundle "$bundle" '{schema:"monday.rust_lob_controller_release.v2",control_plane_version:2,topology:"stable",
      artifact_sha256:$payload,artifact_uri:("oss://bucket/payload/"+$payload),
      runtime_contract_sha256:$runtime,deployment_source_revision:$source,
      deployment_bundle_sha256:$bundle,deployment_bundle_uri:("oss://bucket/bundle/"+$bundle)}' \
    >"$root/release.json"
  controller=$(sha256sum "$root/release.json" | awk '{print $1}')
  ln -s "$RELEASE_ROOT/$fixture_payload/binance-lob-archiver" "$root/binance-lob-archiver"
  (cd "$root"
    sha256sum release.json >release.json.sha256
    for asset in deployment/*; do monday_sha256_checksum_line "$asset"; done | sort -k2 >deployment.sha256)
  chmod -R go-w "$root"
  mv "$root" "$CONTROLLER_RELEASE_ROOT/$controller"
  monday_verify_controller_release "$ROOT_PREFIX" "$controller" || fail 'immutable fixture controller failed validation'
  printf '%s\n' "$controller"
}
fixture_old_c=$(fixture_controller old "$fixture_source_old" "$fixture_bundle_old")
fixture_new_c=$(fixture_controller new "$fixture_source_new" "$fixture_bundle_new")
fixture_active_c=$fixture_new_c
fixture_runtime=$(jq -r '.runtime_contract_sha256' "$CONTROLLER_RELEASE_ROOT/$fixture_new_c/release.json")
fixture_env_sha=$(sha256sum "$CONTROLLER_RELEASE_ROOT/$fixture_old_c/deployment/binance-lob-archiver-production-spot.env" | awk '{print $1}')

secure_release_identity() {
  monday_verify_controller_release "$ROOT_PREFIX" "$fixture_active_c" || fail 'fixture active controller is invalid'
  ACTIVE_CONTROLLER_SHA256=$fixture_active_c
  ACTIVE_RUNTIME_CONTRACT_SHA256=$fixture_runtime
  RELEASE_SHA256=$fixture_payload
  RELEASE_ENV_FILE="$CONTROLLER_RELEASE_ROOT/$fixture_active_c/deployment/binance-lob-archiver-production-spot.env"
  ENV_SHA256=$(sha256sum "$RELEASE_ENV_FILE" | awk '{print $1}')
  RELEASE_BUNDLE_SHA256=$(jq -r '.deployment_bundle_sha256' "$CONTROLLER_RELEASE_ROOT/$fixture_active_c/release.json")
  RELEASE_SOURCE_REVISION=$(jq -r '.deployment_source_revision' "$CONTROLLER_RELEASE_ROOT/$fixture_active_c/release.json")
}
fixture_transition="$DATA_ROOT/monday/evidence/cutovers/$fixture_new_c/transition.json"
fixture_resume_gate="$DATA_ROOT/monday/evidence/shadow-gates/$fixture_new_c/$fixture_runtime/runs/20260908T000000Z-1/gate.json"
mkdir -p "${fixture_transition%/*}" "${fixture_resume_gate%/*}"
jq -cn '{test_only:false,production_eligible:true}' >"$fixture_resume_gate"
fixture_resume_gate_sha=$(sha256sum "$fixture_resume_gate" | awk '{print $1}')
jq -cn --arg from "$fixture_old_c" --arg to "$fixture_new_c" \
  --arg gate "$fixture_resume_gate" --arg gate_sha "$fixture_resume_gate_sha" \
  '{from_controller_sha256:$from,controller_sha256:$to,from_source_mode:"stable",
    production_eligible:true,test_only:false,result:"success",gate_receipt:$gate,
    gate_sha256:$gate_sha}' >"$fixture_transition"
fixture_transition_sha=$(sha256sum "$fixture_transition" | awk '{print $1}')
monday_validate_v2_transition() {
  [[ "$1" == "$ROOT_PREFIX" && "$2" == "$fixture_transition" && "$3" == "$fixture_old_c" \
    && "$4" == "$fixture_new_c" && "$5" == "$fixture_resume_gate" && "$6" == "$fixture_resume_gate_sha" ]] \
    && jq -e --arg controller "$fixture_new_c" '.controller_sha256 == $controller' "$2" >/dev/null
}

# Exercise the real shared validator in the same if/|| context used by resume.
# The Gate itself is assumed independently verified here; all transition shape,
# identity and embedded-evidence checks still execute their production code.
(
  # shellcheck disable=SC2317,SC2329 # Called by the real transition validator below.
  monday_validate_v2_gate_authoritative() {
    [[ "$1" == "$ROOT_PREFIX" && "$2" == "$fixture/validator-gate.json" \
      && "$3" == "$fixture_old_c" && "$4" == "$fixture_new_c" \
      && "$5" == "$fixture_transition_sha" ]]
  }
  phases='["oss-readback-spot","oss-readback-usdm","preflight","shadow-spot","shadow-usdm","strict-verifier-spot","strict-verifier-usdm","upload-drain-spot","upload-drain-usdm"]'
  runtime_keys=$(monday_runtime_assets | jq -Rsc 'split("\n") | map(select(length>0))')
  controller_keys=$(monday_controller_projection_assets | jq -Rsc 'split("\n") | map(select(length>0))')
  jq -cn --arg payload "$fixture_payload" --arg runtime "$fixture_runtime" --arg from "$fixture_old_c" \
    --argjson phases "$phases" \
    '{candidate_payload_sha256:$payload,candidate_runtime_contract_sha256:$runtime,
      from_controller_sha256:$from,production_runtime:{},candidate_control_bytes:{},
      resource_admission:($phases|map({phase:.})),io_full_psi_windows:($phases|map({phase:.})),
      shadow_staging:{},checks:{},markets:{spot:{},usdm:{}}}' >"$fixture/validator-gate.json"
  gate_evidence=$(jq -c '{candidate_control_bytes,resource_admission,io_full_psi_windows,shadow_staging,checks,markets}' "$fixture/validator-gate.json")
  jq -cn --arg from "$fixture_old_c" --arg to "$fixture_new_c" --arg payload "$fixture_payload" \
    --arg runtime "$fixture_runtime" --arg gate "$fixture/validator-gate.json" --arg gate_sha "$fixture_transition_sha" \
    --argjson evidence "$gate_evidence" --argjson assets "$runtime_keys" --argjson controllers "$controller_keys" \
    '{schema:"monday.rust_lob_pair_transition.v2",control_plane_version:2,operation:"cutover",
      from_source_mode:"stable",source_mode:"stable",from_controller_sha256:$from,controller_sha256:$to,
      payload_sha256:$payload,runtime_contract_sha256:$runtime,gate_receipt:$gate,gate_sha256:$gate_sha,
      test_only:true,production_eligible:false,production_runtime:{},production_process:{},
      recovery_schedulers:{spot:{active:true,enabled:true,unit:"binance-lob-archiver-recovery@spot.timer"},
        usdm:{active:true,enabled:true,unit:"binance-lob-archiver-recovery@usdm.timer"}},
      gate_evidence:$evidence,active_pair_committed:true,result:"success",
      completed_at:"2020-01-01T00:00:00Z",completed_at_ns:1577836800000000000,
      stable_production_projection:"/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver",
      before:{controller:$from,payload_sha256:$payload,runtime_contract_sha256:$runtime,
        production_projection:"/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver",
        assets:($assets|map({key:.,value:{state:"absent",sha256:null}})|from_entries)},
      installed_assets:($assets|map({key:.,value:$payload})|from_entries),
      installed_projections:($assets|map({key:.,value:"fixture"})|from_entries),
      installed_controller_projections:($controllers|map({key:.,value:{sha256:$payload,
        target:("/opt/monday/releases/binance-lob-controller/active/deployment/"+.)}})|from_entries)}' \
      >"$fixture/validator-transition.json"
  fixture_real_transition_validator "$ROOT_PREFIX" "$fixture/validator-transition.json" \
    "$fixture_old_c" "$fixture_new_c" "$fixture/validator-gate.json" "$fixture_transition_sha" \
    || fail 'valid conditional-context transition fixture was rejected'
  for mutation in '.controller_sha256="wrong"' '.active_pair_committed=false' \
    '.gate_evidence.markets.spot={wrong:true}' '.production_runtime={wrong:true}'; do
    jq "$mutation" "$fixture/validator-transition.json" >"$fixture/validator-invalid.json"
    if fixture_real_transition_validator "$ROOT_PREFIX" "$fixture/validator-invalid.json" \
      "$fixture_old_c" "$fixture_new_c" "$fixture/validator-gate.json" "$fixture_transition_sha"; then
      fail "transition validator ignored a failure in conditional context: $mutation"
    fi
  done
)

mkdir "$fixture/oss"
printf 'real fixture object bytes\n' >"$fixture/oss/part-1.jsonl.zst"
fixture_data_sha=$(sha256sum "$fixture/oss/part-1.jsonl.zst" | awk '{print $1}')
jq -cn --arg sha "$fixture_data_sha" \
  '{schema:"binance.market_tape.v2",market:"spot",dataset:"spot_all",shard_id:"all",
    file:"part-1.jsonl.zst",sha256:$sha,session_id:"recovery-fixture",catalog_sha256:"fixture",
    start_received_at_ns:1577836800000000000,end_received_at_ns:1577836801000000000}' \
  >"$fixture/oss/part-1.jsonl.zst.manifest.json"
printf '%s\n' "$fixture_data_sha" >"$fixture/oss/part-1.jsonl.zst._SUCCESS"
fixture_manifest_sha=$(sha256sum "$fixture/oss/part-1.jsonl.zst.manifest.json" | awk '{print $1}')
fixture_prefix=lake/raw/venue=binance/market=spot/dataset=spot_all/shard=all/date=2020-01-01/hour=00
jq -cn --arg data "$fixture_data_sha" --arg manifest "$fixture_manifest_sha" \
  --arg prefix "$fixture_prefix" \
  '{last_error:null,last_error_at:null,last_success_at:"2020-01-01T00:01:00Z",pending_batches:0,
    last_uploaded_object:("oss://monday-lob-apne1-1045353359/"+$prefix+"/part-1.jsonl.zst"),
    last_uploaded_triplet:{data_sha256:$data,manifest_sha256:$manifest,success_sha256:$data,
      object_prefix:$prefix,uploaded_at:"2020-01-01T00:01:00Z"}}' >"$fixture/status.before.json"
jq --arg now "$(date -u +%Y-%m-%dT%H:%M:%SZ)" \
  '.last_success_at=$now | .last_uploaded_triplet.uploaded_at=$now' \
  "$fixture/status.before.json" >"$fixture/status.after.json"
copy_active_oss() {
  [[ ${FIXTURE_OSS_FAIL:-0} != 1 ]] || return 1
  cp "$fixture/oss/${1##*/}" "$2"
}

fixture_job() {
  local serial=$1 state=$2 dir
  RESUME_JOB_ID="20260902T141601Z-spot-${fixture_payload:0:12}-$serial"
  dir="$QUEUE_MARKET_ROOT/$RESUME_JOB_ID.$state"
  mkdir -m 0750 "$dir"
  : >"$dir/.binance-lob-archiver.lock"
  cp "$CONTROLLER_RELEASE_ROOT/$fixture_old_c/deployment/binance-lob-archiver-production-spot.env" "$dir/recovery.env"
  cp "$fixture/status.before.json" "$dir/upload-status.json"
  jq -cn --arg id "$RESUME_JOB_ID" --arg canonical "$CANONICAL_SPOOL" \
    --arg payload "$fixture_payload" --arg controller "$fixture_old_c" --arg runtime "$fixture_runtime" \
    --arg env "$fixture_env_sha" --arg bundle "$fixture_bundle_old" --arg source "$fixture_source_old" \
    '{schema:"monday.rust_lob_recovery_queue.v1",job_id:$id,queued_at:"2020-01-01T00:00:00Z",
      market:"spot",canonical_spool:$canonical,release_sha256:$payload,payload_sha256:$payload,
      controller_sha256:$controller,runtime_contract_sha256:$runtime,env_sha256:$env,
      deployment_bundle_sha256:$bundle,deployment_source_revision:$source,
      release_env:"recovery.env",recovery_unit:"binance-lob-archiver-recovery@spot.service"}' >"$dir/job.json"
  RESUME_JOB_SHA256=$(sha256sum "$dir/job.json" | awk '{print $1}')
  RESUME_FROM_CONTROLLER=$fixture_old_c
  RESUME_CONTROLLER=$fixture_new_c
  RESUME_TRANSITION_RECEIPT=$fixture_transition
  RESUME_TRANSITION_SHA256=$fixture_transition_sha
  RESUME_REQUEST_ID=initial
  fixture_job_dir=$dir
}
fixture_resume() { (queue_lock; resume_market); }
fixture_drain() { (queue_lock; drain_market); }
fixture_attempt() {
  local job_root="$EVIDENCE_ROOT/$RESUME_JOB_ID" request_sha
  request_sha=$(jq -r '.request_sha256' "$job_root/resume.json")
  printf '%s/attempts/%s\n' "$job_root" "$request_sha"
}
expect_rejected() {
  local label=$1; shift
  if ("$@") >"$fixture/rejected.log" 2>&1; then
    printf 'unexpected success: %s\n' "$label" >&2
    tail -n 30 "$fixture/rejected.log" >&2
    find "$QUEUE_MARKET_ROOT" -maxdepth 2 -print >&2
    [[ ! -e $fixture/payload.calls ]] || cat "$fixture/payload.calls" >&2
    exit 1
  fi
}

# Identity mismatches remain fail-closed without an explicit adoption.
fixture_job 101 ready
fixture_drain >/dev/null
[[ -d $QUEUE_MARKET_ROOT/$RESUME_JOB_ID.stale ]]
[[ $(jq -r .result "$EVIDENCE_ROOT/$RESUME_JOB_ID/result.json") == stale ]]
[[ ! -e $fixture/payload.calls ]]
fixture_old_result_sha=$(sha256sum "$EVIDENCE_ROOT/$RESUME_JOB_ID/result.json" | awk '{print $1}')

# Negative authority checks must not create an adoption or alter the job.
fixture_job 102 ready
good_job_sha=$RESUME_JOB_SHA256
RESUME_JOB_SHA256=$(printf '%064d' 999)
expect_rejected wrong-job-hash fixture_resume
RESUME_JOB_SHA256=$good_job_sha
RESUME_TRANSITION_SHA256=$(printf '%064d' 998)
expect_rejected wrong-transition-hash fixture_resume
RESUME_TRANSITION_SHA256=$fixture_transition_sha
cp "$fixture_resume_gate" "$fixture/formal-resume-gate.json"
cp "$fixture_transition" "$fixture/formal-resume-transition.json"
formal_resume_gate_sha=$fixture_resume_gate_sha
formal_resume_transition_sha=$fixture_transition_sha
# Only the Gate's mode changes; update both referenced hashes so rejection is
# specifically the production/fixture boundary, not a stale digest mismatch.
jq '.test_only=true | .production_eligible=false' "$fixture/formal-resume-gate.json" >"$fixture_resume_gate"
fixture_resume_gate_sha=$(sha256sum "$fixture_resume_gate" | awk '{print $1}')
jq --arg gate_sha "$fixture_resume_gate_sha" '.gate_sha256=$gate_sha' \
  "$fixture/formal-resume-transition.json" >"$fixture_transition"
fixture_transition_sha=$(sha256sum "$fixture_transition" | awk '{print $1}')
RESUME_TRANSITION_SHA256=$fixture_transition_sha
expect_rejected fixture-gate-in-production-transition fixture_resume
grep -Fq 'resume requires a production-eligible Gate' "$fixture/rejected.log"
cp "$fixture/formal-resume-gate.json" "$fixture_resume_gate"
cp "$fixture/formal-resume-transition.json" "$fixture_transition"
fixture_resume_gate_sha=$formal_resume_gate_sha
fixture_transition_sha=$formal_resume_transition_sha
RESUME_TRANSITION_SHA256=$fixture_transition_sha
printf '\n' >>"$fixture_job_dir/recovery.env"
expect_rejected changed-original-env fixture_resume
cp "$CONTROLLER_RELEASE_ROOT/$fixture_old_c/deployment/binance-lob-archiver-production-spot.env" "$fixture_job_dir/recovery.env"
fixture_active_c=$fixture_old_c
expect_rejected wrong-active-controller fixture_resume
fixture_active_c=$fixture_new_c
saved_runtime=$fixture_runtime
fixture_runtime=$(printf '%064d' 997)
expect_rejected changed-runtime-contract fixture_resume
fixture_runtime=$saved_runtime
origin_script="$CONTROLLER_RELEASE_ROOT/$fixture_old_c/deployment/host-rust-lob-recovery-queue.sh"
cp "$origin_script" "$fixture/original-controller-script"
printf '\n# altered immutable source\n' >>"$origin_script"
expect_rejected mutated-origin-controller fixture_resume
cp "$fixture/original-controller-script" "$origin_script"
[[ ! -e $EVIDENCE_ROOT/$RESUME_JOB_ID/resume.json ]]
rm -rf "$fixture_job_dir"

# The production incident: immutable old backup and an empty legacy result
# temporary remain intact; an already-uploaded spool needs no binary invocation.
fixture_job 103 running
mkdir -p "$EVIDENCE_ROOT/$RESUME_JOB_ID/recovery-input"
printf '{"original":"receipt"}\n' >"$EVIDENCE_ROOT/$RESUME_JOB_ID/recovery-input/receipt.json"
: >"$EVIDENCE_ROOT/$RESUME_JOB_ID/result.json.tmp"
old_backup_sha=$(sha256sum "$EVIDENCE_ROOT/$RESUME_JOB_ID/recovery-input/receipt.json" | awk '{print $1}')
fixture_resume >/dev/null
attempt=$(fixture_attempt)
adoption_sha=$(sha256sum "$attempt/adoption.json" | awk '{print $1}')
fixture_resume >/dev/null
[[ $(sha256sum "$attempt/adoption.json" | awk '{print $1}') == "$adoption_sha" ]]
fixture_drain >/dev/null
[[ -d $attempt/spool.done && ! -e $attempt/recovery-input && ! -e $fixture/payload.calls ]]
jq -e --arg old "$fixture_old_c" --arg active "$fixture_new_c" \
  '.result == "passed" and .controller_sha256 == $old and .executing_controller_sha256 == $active
    and .minimum_upload_success_at == "2020-01-01T00:00:00Z"
    and .upload_triplet_readback.data_sha256 != null' "$attempt/result.json" >/dev/null
[[ $(sha256sum "$attempt/spool.done/job.json" | awk '{print $1}') == "$RESUME_JOB_SHA256" ]]
[[ $(sha256sum "$EVIDENCE_ROOT/$RESUME_JOB_ID/recovery-input/receipt.json" | awk '{print $1}') == "$old_backup_sha" ]]
[[ -f $EVIDENCE_ROOT/$RESUME_JOB_ID/result.json.tmp && ! -s $EVIDENCE_ROOT/$RESUME_JOB_ID/result.json.tmp ]]
fixture_resume >/dev/null
complete_pointer_sha=$(sha256sum "$EVIDENCE_ROOT/$RESUME_JOB_ID/resume.json" | awk '{print $1}')
RESUME_REQUEST_ID=must-not-replace-success
expect_rejected new-request-after-success fixture_resume
[[ $(sha256sum "$EVIDENCE_ROOT/$RESUME_JOB_ID/resume.json" | awk '{print $1}') == "$complete_pointer_sha" ]]

# A mixed spool resumes through the same payload: existing sealed segments are
# uploaded alongside newly recovered parts, with a fresh attempt-owned backup.
fixture_job 104 ready
printf 'raw input\n' >"$fixture_job_dir/part-2.jsonl.part"
printf 'interrupted derived output\n' >"$fixture_job_dir/part-2.jsonl.zst.tmp"
printf 'already sealed\n' >"$fixture_job_dir/part-3.jsonl.zst"
printf '{}\n' >"$fixture_job_dir/part-3.jsonl.zst.manifest.json"
fixture_resume >/dev/null
attempt=$(fixture_attempt)
fixture_drain >/dev/null
[[ -f $attempt/recovery-input/part-2.jsonl.part && -f $attempt/recovery-input/part-2.jsonl.zst.tmp ]]
[[ -d $attempt/spool.done && $(jq -r .result "$attempt/result.json") == passed ]]
grep -Fq -- '--recover-parts-only' "$fixture/payload.calls"
grep -Fq 'part-3.jsonl.zst' "$fixture/uploaded-files"
grep -Fq 'part-900.jsonl.zst' "$fixture/uploaded-files"

# Failure evidence is immutable.  Replaying a failed request reports the same
# failure; only a new explicit request creates a separate attempt after repair.
fixture_job 105 ready
printf 'raw input\n' >"$fixture_job_dir/part-4.jsonl.part"
: >"$fixture_job_dir/.fixture-fail-upload"
fixture_resume >/dev/null
failed_attempt=$(fixture_attempt)
expect_rejected upload-failure fixture_drain
[[ $(jq -r .result "$failed_attempt/result.json") == failed ]]
failed_sha=$(sha256sum "$failed_attempt/result.json" | awk '{print $1}')
expect_rejected failed-request-replay fixture_resume
[[ $(sha256sum "$failed_attempt/result.json" | awk '{print $1}') == "$failed_sha" ]]
rm "$QUEUE_MARKET_ROOT/$RESUME_JOB_ID.failed/.fixture-fail-upload"
RESUME_REQUEST_ID=after-upload-repair
fixture_resume >/dev/null
attempt=$(fixture_attempt)
[[ $attempt != "$failed_attempt" ]]
fixture_drain >/dev/null
[[ $(jq -r .result "$attempt/result.json") == passed && -d $attempt/spool.done ]]
[[ $(sha256sum "$failed_attempt/result.json" | awk '{print $1}') == "$failed_sha" ]]

# Commit-before-rename interruption is replayable.  No result or job receipt is
# fabricated, and the same request retains its adoption digest.
fixture_job 106 running
fixture_resume >/dev/null
attempt=$(fixture_attempt)
adoption_sha=$(sha256sum "$attempt/adoption.json" | awk '{print $1}')
mv "$QUEUE_MARKET_ROOT/$RESUME_JOB_ID.ready" "$QUEUE_MARKET_ROOT/$RESUME_JOB_ID.running"
fixture_resume >/dev/null
[[ -d $QUEUE_MARKET_ROOT/$RESUME_JOB_ID.ready ]]
[[ $(sha256sum "$attempt/adoption.json" | awk '{print $1}') == "$adoption_sha" ]]
fixture_drain >/dev/null

# A committed result survives archival interruption and failure/signal handling.
mv "$attempt/spool.done" "$QUEUE_MARKET_ROOT/$RESUME_JOB_ID.running"
passed_sha=$(sha256sum "$attempt/result.json" | awk '{print $1}')
expect_rejected failure-after-success mark_failed "$QUEUE_MARKET_ROOT/$RESUME_JOB_ID.running" signal interrupted
[[ $(sha256sum "$attempt/result.json" | awk '{print $1}') == "$passed_sha" ]]
fixture_drain >/dev/null
[[ -d $attempt/spool.done && $(sha256sum "$attempt/result.json" | awk '{print $1}') == "$passed_sha" ]]

# B's adoption can be durable while the pointer still names predecessor A.
# Replaying B completes this exact commit; a truly superseded A stays rejected.
fixture_job 107 ready
fixture_resume >/dev/null
first_attempt=$(fixture_attempt)
cp "$EVIDENCE_ROOT/$RESUME_JOB_ID/resume.json" "$fixture/first-resume.json"
RESUME_REQUEST_ID=second-request
fixture_resume >/dev/null
attempt=$(fixture_attempt)
adoption_sha=$(sha256sum "$attempt/adoption.json" | awk '{print $1}')
cp "$fixture/first-resume.json" "$EVIDENCE_ROOT/$RESUME_JOB_ID/resume.json.tmp"
mv -f "$EVIDENCE_ROOT/$RESUME_JOB_ID/resume.json.tmp" "$EVIDENCE_ROOT/$RESUME_JOB_ID/resume.json"
fixture_resume >/dev/null
[[ $(fixture_attempt) == "$attempt" && $(sha256sum "$attempt/adoption.json" | awk '{print $1}') == "$adoption_sha" ]]
RESUME_REQUEST_ID=initial
expect_rejected superseded-request fixture_resume
[[ $(fixture_attempt) == "$attempt" && $first_attempt != "$attempt" ]]
RESUME_REQUEST_ID=second-request
fixture_drain >/dev/null

# The main-entrypoint identity check resolves the installed CLI symlink and
# rejects a byte-identical script that is outside the active projection.
saved_installed_recovery=$INSTALLED_RECOVERY
mkdir -p "$BIN_DIR"
ln -s "$RECOVERY" "$BIN_DIR/fixture-recovery-projection"
INSTALLED_RECOVERY="$BIN_DIR/fixture-recovery-projection"
active_recovery_program_matches "$INSTALLED_RECOVERY"
cp "$RECOVERY" "$fixture/copied-recovery.sh"
expect_rejected non-projected-controller active_recovery_program_matches "$fixture/copied-recovery.sh"
INSTALLED_RECOVERY=$saved_installed_recovery

# An adopted stale job preserves its prior terminal evidence.  A forged or
# missing remote object never becomes a recovery success.
RESUME_JOB_ID="20260902T141601Z-spot-${fixture_payload:0:12}-101"
RESUME_JOB_SHA256=$(sha256sum "$QUEUE_MARKET_ROOT/$RESUME_JOB_ID.stale/job.json" | awk '{print $1}')
RESUME_REQUEST_ID=adopt-stale
fixture_resume >/dev/null
attempt=$(fixture_attempt)
FIXTURE_OSS_FAIL=1
expect_rejected missing-remote-object fixture_drain
unset FIXTURE_OSS_FAIL
[[ $(jq -r .result "$attempt/result.json") == failed ]]
[[ $(sha256sum "$EVIDENCE_ROOT/$RESUME_JOB_ID/result.json" | awk '{print $1}') == "$fixture_old_result_sha" ]]

# Isolate/drain must cover rotated complete undrained segments, not only
# incomplete parts.  Drain of a complete-only spool uses --upload-only.
empty_spool="$fixture/empty-isolate-spool"
complete_spool="$fixture/complete-isolate-spool"
part_spool="$fixture/part-isolate-spool"
mkdir -p "$empty_spool" "$complete_spool" "$part_spool"
printf '{}\n' >"$empty_spool/upload-status.json"
printf '{}\n' >"$empty_spool/health.json"
: >"$empty_spool/.binance-lob-archiver.lock"
if has_incomplete_parts "$empty_spool" || has_undrained_complete_segments "$empty_spool" \
  || needs_recovery_isolation "$empty_spool"; then
  printf 'empty spool was treated as recovery work\n' >&2
  exit 1
fi
printf 'sealed\n' >"$complete_spool/part-1.jsonl.zst"
printf '{}\n' >"$complete_spool/part-1.jsonl.zst.manifest.json"
printf 'ok\n' >"$complete_spool/part-1.jsonl.zst._SUCCESS"
has_undrained_complete_segments "$complete_spool"
needs_recovery_isolation "$complete_spool"
if has_incomplete_parts "$complete_spool"; then
  printf 'complete segment was classified as an incomplete part\n' >&2
  exit 1
fi
printf 'raw\n' >"$part_spool/part-2.jsonl.part"
has_incomplete_parts "$part_spool"
needs_recovery_isolation "$part_spool"
if has_undrained_complete_segments "$part_spool"; then
  printf 'incomplete part was classified as a complete segment\n' >&2
  exit 1
fi

rm -f "$fixture/payload.calls" "$fixture/uploaded-files"
fixture_job 108 ready
printf 'already sealed\n' >"$fixture_job_dir/part-complete.jsonl.zst"
printf '{}\n' >"$fixture_job_dir/part-complete.jsonl.zst.manifest.json"
printf 'success\n' >"$fixture_job_dir/part-complete.jsonl.zst._SUCCESS"
fixture_resume >/dev/null
attempt=$(fixture_attempt)
fixture_drain >/dev/null
[[ -d $attempt/spool.done && $(jq -r .result "$attempt/result.json") == passed ]]
grep -Fq -- '--upload-only' "$fixture/payload.calls"
if grep -Fq -- '--recover-parts-only' "$fixture/payload.calls"; then
  printf 'complete-only drain invoked recover-parts\n' >&2
  exit 1
fi
grep -Fq 'part-complete.jsonl.zst' "$fixture/uploaded-files"

saved_canonical=$CANONICAL_SPOOL
isolate_spool="$fixture/host/data/monday/spool/binance-lob/spot"
rm -rf -- "$isolate_spool"
mkdir -p "$isolate_spool"
printf '{}\n' >"$isolate_spool/upload-status.json"
: >"$isolate_spool/.binance-lob-archiver.lock"
chmod 0640 "$isolate_spool/.binance-lob-archiver.lock" "$isolate_spool/upload-status.json"
ready_before=$(find "$QUEUE_MARKET_ROOT" -mindepth 1 -maxdepth 1 -type d -name '*.ready' -print | wc -l)
fixture_isolate() {
  (
    CURRENT_ACTION=isolate
    install() {
      local mode=0750 dest src
      while (($#)); do
        case "$1" in
          -d) shift ;;
          -m) mode=$2; shift 2 ;;
          -o|-g) shift 2 ;;
          --) shift; break ;;
          *) break ;;
        esac
      done
      dest=${*: -1}
      if (($# <= 1)); then
        mkdir -p "$dest"
        chmod "$mode" "$dest" 2>/dev/null || true
      else
        src=${*: -2:1}
        cp -- "$src" "$dest"
        chmod "$mode" "$dest" 2>/dev/null || true
      fi
    }
    queue_lock
    run_isolate
  )
}
if ! fixture_isolate >/dev/null; then
  printf 'empty canonical spool isolate failed\n' >&2
  exit 1
fi
ready_after=$(find "$QUEUE_MARKET_ROOT" -mindepth 1 -maxdepth 1 -type d -name '*.ready' -print | wc -l)
[[ $ready_before == "$ready_after" ]] \
  || { printf 'empty spool isolate queued a job\n' >&2; exit 1; }

printf 'sealed\n' >"$isolate_spool/part-pending.jsonl.zst"
printf '{}\n' >"$isolate_spool/part-pending.jsonl.zst.manifest.json"
printf 'ok\n' >"$isolate_spool/part-pending.jsonl.zst._SUCCESS"
fixture_isolate >/dev/null
ready_dir=$(find "$QUEUE_MARKET_ROOT" -mindepth 1 -maxdepth 1 -type d -name '*.ready' -print \
  | while IFS= read -r dir; do
      [[ -f $dir/part-pending.jsonl.zst ]] && printf '%s\n' "$dir"
    done | sed -n '1p')
[[ -n $ready_dir ]] || { printf 'complete undrained isolate did not queue a job\n' >&2; exit 1; }
[[ -f $ready_dir/part-pending.jsonl.zst && -f $ready_dir/part-pending.jsonl.zst.manifest.json ]]
[[ -f $isolate_spool/upload-status.json && ! -e $isolate_spool/part-pending.jsonl.zst ]]
CANONICAL_SPOOL=$saved_canonical
printf 'Explicit recovery adoption, historical readback, mixed drain and complete-segment isolation passed\n'
