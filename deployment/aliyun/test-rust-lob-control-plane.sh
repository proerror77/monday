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
printf '\n# controller revision two fixture\n' >>"$source_dir/host-rust-lob-readback.sh"
p1="$ROOT/p1"; m1="$ROOT/m1.json"
p1_sha=$(publish_fixture "$p1" "$m1")
c0=$(monday_sha256_file "$m0")
c1=$(monday_sha256_file "$m1")

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

# A V2 before pair must be active and its payload may change.
ln -s "$ROOT/opt/monday/releases/binance-lob-controller/$c0" \
  "$ROOT/opt/monday/releases/binance-lob-controller/active"
mkdir -p "$ROOT/etc/systemd/system" "$ROOT/etc/monday"
for asset in \
  binance-lob-archiver-rust@.service binance-lob-archiver-rust-upload@.service; do
  cp "$ROOT/opt/monday/releases/binance-lob-controller/$c0/deployment/$asset" \
    "$ROOT/etc/systemd/system/$asset"
done
for asset in binance-lob-archiver-rust-spot.env binance-lob-archiver-rust-usdm.env; do
  cp "$ROOT/opt/monday/releases/binance-lob-controller/$c0/deployment/$asset" \
    "$ROOT/etc/monday/$asset"
done
declare -A shadow_before_sha
for asset in \
  binance-lob-archiver-rust@.service binance-lob-archiver-rust-upload@.service \
  binance-lob-archiver-rust-spot.env binance-lob-archiver-rust-usdm.env; do
  if [[ $asset == *.service ]]; then target="$ROOT/etc/systemd/system/$asset"; else target="$ROOT/etc/monday/$asset"; fi
  shadow_before_sha[$asset]=$(monday_sha256_file "$target")
done
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
  [[ $(monday_sha256_file "$target") == "${shadow_before_sha[$asset]}" ]] || {
    printf 'Gate did not restore shadow asset %s\n' "$asset" >&2
    exit 1
  }
done
[[ $(readlink -- "$ROOT/opt/monday/bin/binance-lob-archiver-shadow") == "$shadow_before_target" ]]

cutover_output=$(MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from "$c0" --to "$c1" \
  --gate-receipt "$gate" --gate-sha256 "$gate_sha" --root "$ROOT")
transition=$(printf '%s\n' "$cutover_output" | sed -n 's/^Transition receipt: //p')
transition_sha=$(printf '%s\n' "$cutover_output" | sed -n 's/^SHA-256: //p')
[[ $(monday_active_controller_sha "$ROOT") == "$c1" ]]
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == \
  "$ROOT/opt/monday/releases/binance-lob-archiver/$p1_sha/binance-lob-archiver" ]]
[[ $(monday_sha256_file "$transition") == "$transition_sha" ]]

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
  [[ $(monday_sha256_file "$target") == "${shadow_before_sha[$asset]}" ]] || {
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
if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" MONDAY_CUTOVER_FAIL_AFTER_ACTIVE=1 \
  "$SCRIPT_DIR/host-rust-lob-cutover.sh" --from "$c1" --to "$c2" \
  --gate-receipt "$gate2" --gate-sha256 "$gate2_sha" --root "$ROOT" \
  >/dev/null 2>&1; then
  printf 'fault-injected cutover unexpectedly succeeded\n' >&2
  exit 1
fi
[[ $(monday_active_controller_sha "$ROOT") == "$c1" ]]
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
[[ $(readlink -f -- "$ROOT/opt/monday/bin/binance-lob-archiver") == \
  "$ROOT/opt/monday/releases/binance-lob-archiver/$p2_sha/binance-lob-archiver" ]]
[[ $(monday_sha256_file "$transition2") == "$transition2_sha" ]]
MONDAY_CONTROL_PLANE_TEST=1 MONDAY_ROOT="$ROOT" \
  "$SCRIPT_DIR/host-rust-lob-readback.sh" --controller "$c2" \
  --transition-receipt "$transition2" --receipt-sha256 "$transition2_sha" \
  --root "$ROOT" >/dev/null

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

printf 'V2 Gate contract passed\n'
