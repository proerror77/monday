#!/usr/bin/env bash
set -Eeuo pipefail
export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(readlink -f "$(mktemp -d)")
fixture_root=$ROOT
trap 'chmod -R u+w "$ROOT" 2>/dev/null || true; rm -rf "$ROOT"' EXIT
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/host-rust-lob-controller-release.sh"
ROOT=$fixture_root

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
for asset in binance-lob-archiver-production@.service binance-lob-archiver-upload@.service; do
  cp "$ROOT/opt/monday/releases/binance-lob-controller/$(monday_sha256_file "$m0")/deployment/$asset" \
    "$ROOT/etc/systemd/system/$asset"
done
for asset in binance-lob-archiver-production-spot.env binance-lob-archiver-production-usdm.env; do
  cp "$ROOT/opt/monday/releases/binance-lob-controller/$(monday_sha256_file "$m0")/deployment/$asset" \
    "$ROOT/etc/monday/$asset"
done
# Bootstrap independently verifies all eight runtime unit/env bytes (the
# production and shadow lanes) before establishing stable projections.
for asset in binance-lob-archiver-rust@.service binance-lob-archiver-rust-upload@.service; do
  cp "$ROOT/opt/monday/releases/binance-lob-controller/$(monday_sha256_file "$m0")/deployment/$asset" \
    "$ROOT/etc/systemd/system/$asset"
done
for asset in binance-lob-archiver-rust-spot.env binance-lob-archiver-rust-usdm.env; do
  cp "$ROOT/opt/monday/releases/binance-lob-controller/$(monday_sha256_file "$m0")/deployment/$asset" \
    "$ROOT/etc/monday/$asset"
done
# Production upload-status is a sentinel: every shadow Gate/drain must leave
# the governed production spool untouched.
production_spool_root="$ROOT/data/monday/spool/binance-lob"
mkdir -p "$production_spool_root/spot" "$production_spool_root/usdm"
printf 'production-sentinel-spot\n' >"$production_spool_root/spot/upload-status.json"
printf 'production-sentinel-usdm\n' >"$production_spool_root/usdm/upload-status.json"
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
jq -cS -n --arg artifact_uri "$legacy_artifact_uri" --arg artifact_sha "$p0_sha" \
  --arg runtime "$(monday_manifest_field "$m0" runtime_contract_sha256)" \
  --arg source "$legacy_source" --arg bundle "$legacy_bundle_uri" --arg bundle_sha "$legacy_bundle_sha" \
  '{schema:("monday.rust_lob_controller_release." + "v1"),artifact_uri:$artifact_uri,
    artifact_sha256:$artifact_sha,runtime_contract_sha256:$runtime,
    deployment_source_revision:$source,deployment_bundle_uri:$bundle,
    deployment_bundle_sha256:$bundle_sha}' >"$legacy_work/release.json"
legacy_c0=$(monday_sha256_file "$legacy_work/release.json")
for asset in host-rust-lob-recovery-queue.sh monday-collector-health.sh; do
  cp -p -- "$source_dir/$asset" "$legacy_work/deployment/$asset"
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

# Bootstrap uses an explicit direct before topology and requires P1 == P0.
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p0_sha/binance-lob-archiver" \
  "$ROOT/opt/monday/bin/binance-lob-archiver"
gate_output=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-shadow-gate.sh" \
  --from-controller direct --candidate-controller "$c0" --root "$ROOT")
gate=$(printf '%s\n' "$gate_output" | sed -n 's/^V2 Gate receipt: //p')
gate_sha=$(printf '%s\n' "$gate_output" | sed -n 's/^SHA-256: //p')
[[ -f $gate && $gate_sha == "$(monday_sha256_file "$gate")" ]]
monday_validate_v2_gate "$gate" direct "$c0" "$gate_sha"
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

# A hard stop immediately after active=C1 must leave that one commit as the
# recovery source.  Restore then establishes every stable projection from C1;
# no receipt or guessed previous state is needed.  Rebuild the direct legacy
# topology afterwards so the normal bootstrap path remains covered below.
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CUTOVER_HARD_CRASH_AFTER_ACTIVE=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from direct --to "$c0" \
  --gate-receipt "$gate" --gate-sha256 "$gate_sha" --root "$ROOT" >/dev/null 2>&1; then
  printf 'hard-crash cutover unexpectedly survived SIGKILL\n' >&2
  exit 1
fi
[[ $(monday_active_controller_sha "$ROOT") == "$c0" ]]
for asset in binance-lob-archiver-rust-spot.env binance-lob-archiver-rust-usdm.env; do
  [[ -f "$(monday_runtime_asset_target "$ROOT" "$asset")" && ! -L "$(monday_runtime_asset_target "$ROOT" "$asset")" ]]
done
MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-restore.sh" --controller "$c0" --root "$ROOT" >/dev/null
rm -f -- "$ROOT/opt/monday/releases/binance-lob-controller/active"
ln -s "$legacy_root/$legacy_c0" "$ROOT/opt/monday/releases/binance-lob-controller/active"
rm -f -- "$ROOT/opt/monday/bin/binance-lob-archiver"
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p0_sha/binance-lob-archiver" \
  "$ROOT/opt/monday/bin/binance-lob-archiver"
while IFS= read -r asset; do
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
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p0_sha/binance-lob-archiver" \
  "$ROOT/opt/monday/bin/binance-lob-archiver"
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
ln -s "$ROOT/opt/monday/releases/binance-lob-archiver/$p0_sha/binance-lob-archiver" \
  "$ROOT/opt/monday/bin/binance-lob-archiver"

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

bootstrap_cutover_output=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from direct --to "$c0" \
  --gate-receipt "$gate" --gate-sha256 "$gate_sha" --root "$ROOT")
bootstrap_transition=$(printf '%s\n' "$bootstrap_cutover_output" | sed -n 's/^Transition receipt: //p')
bootstrap_transition_sha=$(printf '%s\n' "$bootstrap_cutover_output" | sed -n 's/^SHA-256: //p')
[[ $(monday_active_controller_sha "$ROOT") == "$c0" ]]
[[ $(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver") == \
  "$ROOT/opt/monday/releases/binance-lob-controller/active/binance-lob-archiver" ]]
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == \
  "$ROOT/opt/monday/releases/binance-lob-archiver/$p0_sha/binance-lob-archiver" ]]
[[ $(monday_sha256_file "$bootstrap_transition") == "$bootstrap_transition_sha" ]]
monday_validate_v2_transition "$bootstrap_transition" direct "$c0" "$gate" "$gate_sha"
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
for asset in "${!shadow_before_sha[@]}"; do
  if [[ $asset == *.service ]]; then target="$ROOT/etc/systemd/system/$asset"; else target="$ROOT/etc/monday/$asset"; fi
  [[ $(monday_sha256_file "$(readlink -f -- "$target")") == "${shadow_before_sha[$asset]}" ]] || {
    printf 'Gate did not restore shadow asset %s\n' "$asset" >&2
    exit 1
  }
done
[[ $(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver-shadow") == "$shadow_before_target" ]]

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
      '{market:"spot",dataset:"spot_all",status:"synced",sequence_gaps:0,symbol_count:1000,session_id:"cutover-fixture-spot",updated_at_ns:$observed}' \
      >"$production_spool_root/spot/health.json.tmp"
    mv -f -- "$production_spool_root/spot/health.json.tmp" "$production_spool_root/spot/health.json"
    if (( elapsed >= 3 )); then
      if [[ ${FIXTURE_SPOT_FLIP:-0} == 1 ]]; then
        : >"$ROOT/run/cutover-fixture-spot-flip"
      fi
      jq -cn --argjson observed "$observed_at_ns" \
        '{market:"usdm",dataset:"usdm_perpetual_top100_lob",status:"synced",sequence_gaps:0,symbol_count:100,session_id:"cutover-fixture-usdm",updated_at_ns:$observed}' \
        >"$production_spool_root/usdm/health.json.tmp"
      mv -f -- "$production_spool_root/usdm/health.json.tmp" "$production_spool_root/usdm/health.json"
    fi
    (( elapsed < 15 )) || break
    sleep 0.1
  done
}
rm -f -- "$ROOT/run/cutover-fixture-health.stop" "$ROOT/run/cutover-fixture-spot-flip" \
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
monday_validate_v2_transition "$transition" "$c0" "$c1" "$gate" "$gate_sha"
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
grep -Fqx 'Restart=no' "$stale_gate_dir/monday-rust-lob-gate-$(basename -- "$stale_gate_dir")-spot.service"
grep -Fqx 'RuntimeMaxSec=1800' "$stale_gate_dir/monday-rust-lob-gate-$(basename -- "$stale_gate_dir")-spot.service"
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
  '{market:"spot",dataset:"spot_all",shard_id:"all",file:"part-1.jsonl.zst",sha256:$sha,session_id:"fixture-session",catalog_sha256:"fixture-catalog"}' \
  >"$triplet_manifest"
triplet_manifest_sha=$(monday_sha256_file "$triplet_manifest")
triplet_success="$triplet_root/part-1.jsonl.zst._SUCCESS"
printf '%s\n' "$triplet_data_sha" >"$triplet_success"
triplet_success_sha=$(monday_sha256_file "$triplet_success")
triplet_status="$triplet_root/upload-status.json"
triplet_uri='oss://bucket/lake/raw/venue=binance/market=spot/dataset=spot_all/shard=all/part-1.jsonl.zst'
triplet_prefix='lake/raw/venue=binance/market=spot/dataset=spot_all/shard=all'
triplet_now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
jq -cn --arg now "$triplet_now" --arg uri "$triplet_uri" --arg prefix "$triplet_prefix" \
  --arg data "$triplet_data_sha" --arg manifest "$triplet_manifest_sha" --arg success "$triplet_success_sha" \
  '{last_success_at:$now,last_error:null,last_uploaded_triplet:{data_uri:$uri,object_prefix:$prefix,data_sha256:$data,manifest_sha256:$manifest,success_sha256:$success,uploaded_at:$now}}' \
  >"$triplet_status"
copy_triplet_fixture() {
  local uri=$1 target=$2 object
  object=${uri##*/}
  [[ ${TRIPLET_FIXTURE_MISSING:-0} != 1 ]] || return 1
  cp -p -- "$triplet_root/$object" "$target"
}
triplet_tmp="$ROOT/triplet-readback-tmp"
triplet_readback=$(monday_verify_upload_triplet_readback "$triplet_status" spot spot_all bucket "$triplet_prefix" \
  "$triplet_tmp" "$triplet_now" copy_triplet_fixture fixture-session)
[[ $(jq -r '.data_sha256' <<<"$triplet_readback") == "$triplet_data_sha" ]]
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
candidate=$(printf 'b%.0s' {1..64})
operator_json=$(MONDAY_CONTROL_PLANE_DRY_RUN=1 "$operator" gate \
  --instance i-fixture --from-controller "$c1" \
  --candidate-controller "$candidate")
jq -e --arg controller "$candidate" \
  '.operation == "gate" and .controller == $controller
   and (.command | contains(("/opt/monday/releases/binance-lob-controller/" + $controller)))
   and .production_changed == false' <<<"$operator_json" >/dev/null
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
