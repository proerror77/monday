#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf '%s\n' \
    "Usage: ${0##*/} --from-controller <sha|direct> --candidate-controller <sha> [--root <path>]" \
    '       The command emits one immutable V2 Gate receipt and never changes the pair.' >&2
}
die() { printf '%s\n' "$*" >&2; exit 1; }

ROOT=${MONDAY_ROOT:-/}
FROM=; CANDIDATE=; EVIDENCE_ROOT=
while (($#)); do
  case $1 in
    --from-controller) FROM=${2:-}; shift 2 ;;
    --candidate-controller) CANDIDATE=${2:-}; shift 2 ;;
    --root) ROOT=${2:-}; shift 2 ;;
    --evidence-root) EVIDENCE_ROOT=${2:-}; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) usage; exit 2 ;;
  esac
done
ROOT=${ROOT%/}; [[ -n $ROOT ]] || ROOT=/
[[ $FROM == direct || $FROM =~ ^[a-f0-9]{64}$ ]] || die 'from controller is invalid'
[[ $CANDIDATE =~ ^[a-f0-9]{64}$ ]] || die 'candidate controller is invalid'
[[ $FROM != "$CANDIDATE" ]] || die 'Gate requires distinct before and candidate controllers'
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"

controller_root="$ROOT/opt/monday/releases/binance-lob-controller"
candidate_release="$controller_root/$CANDIDATE"
monday_verify_controller_release "$ROOT" "$CANDIDATE" \
  || die 'candidate controller release failed verification'
candidate_manifest="$candidate_release/release.json"
candidate_payload=$(monday_manifest_field "$candidate_manifest" artifact_sha256)
candidate_runtime=$(monday_manifest_field "$candidate_manifest" runtime_contract_sha256)
candidate_bundle=$(monday_manifest_field "$candidate_manifest" deployment_bundle_sha256)
candidate_source=$(monday_manifest_field "$candidate_manifest" deployment_source_revision)

active=none
if [[ -L $controller_root/active ]]; then
  active=$(monday_active_controller_sha "$ROOT") \
    || die 'active controller link is invalid'
fi
if [[ $FROM == direct ]]; then
  [[ $active == none ]] || die 'direct bootstrap requires no active V2 controller'
  production="$ROOT/opt/monday/bin/binance-lob-archiver"
  [[ -e $production || -L $production ]] || die 'bootstrap production payload is missing'
  production_target=$(readlink -f -- "$production") || die 'bootstrap production payload is unresolved'
  monday_file_direct "$production_target" || die 'bootstrap production payload is not a regular file'
  before_payload=$(monday_sha256_file "$production_target")
  [[ $before_payload == "$candidate_payload" ]] \
    || die 'bootstrap requires candidate payload to equal the direct payload'
  before_runtime=direct
else
  [[ $active == "$FROM" ]] || die 'active controller is not the requested before pair'
  monday_verify_controller_release "$ROOT" "$FROM" \
    || die 'before controller release failed verification'
  before_manifest="$controller_root/$FROM/release.json"
  before_payload=$(monday_manifest_field "$before_manifest" artifact_sha256)
  before_runtime=$(monday_manifest_field "$before_manifest" runtime_contract_sha256)
  production="$ROOT/opt/monday/bin/binance-lob-archiver"
  production_target="$ROOT/opt/monday/releases/binance-lob-archiver/$before_payload/binance-lob-archiver"
  [[ -L $production && $(readlink -f -- "$production") == "$production_target" ]] \
    || die 'production payload does not match the active before controller'
  [[ $before_runtime =~ ^[a-f0-9]{64}$ ]] || die 'before runtime contract is invalid'
fi

if [[ ${MONDAY_CONTROL_PLANE_TEST:-0} != 1 ]]; then
  [[ $(readlink -f -- "$0") == "$candidate_release/deployment/host-rust-lob-shadow-gate.sh" ]] \
    || die 'Gate must execute from the candidate controller release'
  for unit in binance-lob-archiver-production@spot.service binance-lob-archiver-production@usdm.service; do
    systemctl is-active --quiet "$unit" || die "production unit is inactive: $unit"
  done
  [[ -x "$candidate_release/binance-lob-archiver" ]] || die 'candidate payload is not executable'
  "$candidate_release/binance-lob-archiver" --self-test >/dev/null \
    || die 'candidate payload self-test failed'
fi

EVIDENCE_ROOT=${EVIDENCE_ROOT:-$ROOT/data/monday/evidence/shadow-gates}
mkdir -p "$EVIDENCE_ROOT/$CANDIDATE/runs"
existing=$(find "$EVIDENCE_ROOT/$CANDIDATE" -type f -name gate.json -print -quit)
[[ -z $existing ]] || die 'candidate controller already has a Gate receipt'
run_id=$(date -u +%Y%m%dT%H%M%SZ)-$$
run_dir="$EVIDENCE_ROOT/$CANDIDATE/runs/$run_id"
mkdir "$run_dir"
gate="$run_dir/gate.json"
gate_tmp="$gate.tmp"
checks=$(jq -cn --arg from "$FROM" --arg active "$active" \
  --arg before_payload "$before_payload" --arg candidate "$CANDIDATE" \
  --arg candidate_payload "$candidate_payload" \
  '{before_controller:$from,observed_active_controller:$active,
    before_payload:$before_payload,candidate_controller:$candidate,
    candidate_payload:$candidate_payload,control_bytes_source:"candidate-controller",
    pair_identity:true,production_unchanged:true}')
jq -cS -n \
  --arg run "$run_id" --arg from "$FROM" --arg candidate "$CANDIDATE" \
  --arg payload "$candidate_payload" --arg runtime "$candidate_runtime" \
  --arg bundle "$candidate_bundle" --arg source "$candidate_source" \
  --arg before_payload "$before_payload" --arg before_runtime "$before_runtime" \
  --argjson checks "$checks" \
  '{schema:"monday.rust_lob_shadow_gate.v5",control_plane_version:2,
    run_id:$run,passed:true,production_eligible:true,
    transition:{before:$from,after:$candidate,topology:(if $from == "direct" then "bootstrap" else "stable" end)},
    before:{controller_sha256:$from,payload_sha256:$before_payload,runtime_contract_sha256:$before_runtime},
    candidate_controller_sha256:$candidate,candidate_payload_sha256:$payload,
    candidate_runtime_contract_sha256:$runtime,deployment_bundle_sha256:$bundle,
    deployment_source_revision:$source,checks:$checks}' >"$gate_tmp"
mv -f "$gate_tmp" "$gate"
gate_sha=$(monday_sha256_file "$gate")
printf '%s  gate.json\n' "$gate_sha" >"$gate.sha256"
chmod 0440 "$gate" "$gate.sha256"
printf 'V2 Gate receipt: %s\nSHA-256: %s\n' "$gate" "$gate_sha"
