#!/usr/bin/env bash
set -Eeuo pipefail
export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
PUBLISHER="$SCRIPT_DIR/host-rust-lob-controller-release.sh"
LIB="$SCRIPT_DIR/rust-lob-control-plane-lib.sh"

for command in cmp find jq mktemp readlink sha256sum sort tar; do
  command -v "$command" >/dev/null 2>&1 \
    || { printf 'missing test dependency: %s\n' "$command" >&2; exit 2; }
done

# shellcheck disable=SC1090
. "$PUBLISHER"
# shellcheck disable=SC1090
. "$LIB"

tmp_dir=$(readlink -f "$(mktemp -d)")
cleanup() {
  chmod -R u+w "$tmp_dir" 2>/dev/null || true
  rm -rf "$tmp_dir"
}
trap cleanup EXIT

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
  host-rust-lob-shadow-preflight.sh
  host-rust-lob-shadow-soak.sh
  host-rust-lob-shadow-gate.sh
  host-rust-lob-cutover.sh
  host-rust-lob-restore.sh
  host-rust-lob-recovery-queue.sh
  host-rust-lob-controller-release.sh
  monday-collector-health.sh
  rust-lob-control-plane-lib.sh
  rust-lob-runtime-health-policy.jq
  rust-lob-shadow-gate-policy.jq
)

make_bundle() {
  local directory=$1 output=$2
  COPYFILE_DISABLE=1 tar -C "$directory" -cf "$output" "${assets[@]}"
}

write_manifest() {
  local output=$1 artifact_sha=$2 bundle_sha=$3 runtime_contract=$4
  jq -n \
    --arg artifact_sha "$artifact_sha" \
    --arg bundle_sha "$bundle_sha" \
    --arg runtime_contract "$runtime_contract" \
    '{schema:"monday.rust_lob_controller_release.v1",
      artifact_uri:("oss://bucket/releases/" + $artifact_sha + "/binance-lob-archiver"),
      artifact_sha256:$artifact_sha,
      runtime_contract_sha256:$runtime_contract,
      deployment_source_revision:("c" * 40),
      deployment_bundle_uri:("oss://bucket/controllers/deployment-" + $bundle_sha + ".tar"),
      deployment_bundle_sha256:$bundle_sha}' >"$output"
}

fixture="$tmp_dir/fixture"
bundle_source="$tmp_dir/bundle-source"
mkdir -p "$fixture/opt/monday/bin" "$bundle_source"
for asset in "${assets[@]}"; do
  cp "$SCRIPT_DIR/$asset" "$bundle_source/$asset"
done
printf '\n# controller-only fixture change\n' \
  >>"$bundle_source/host-rust-lob-recovery-queue.sh"

artifact="$tmp_dir/binance-lob-archiver"
printf '#!/usr/bin/env bash\nexit 0\n' >"$artifact"
chmod 0755 "$artifact"
artifact_sha=$(sha256sum "$artifact" | awk '{print $1}')
artifact_release="$fixture/opt/monday/releases/binance-lob-archiver/$artifact_sha"
mkdir -p "$artifact_release/deployment"
for asset in "${assets[@]}"; do
  cp "$SCRIPT_DIR/$asset" "$artifact_release/deployment/$asset"
done
cp "$artifact" "$artifact_release/binance-lob-archiver"
artifact_uri="oss://bucket/releases/$artifact_sha/binance-lob-archiver"
runtime_contract=$(monday_rust_lob_runtime_contract_sha256 "$artifact_release/deployment")
jq -n \
  --arg artifact_uri "$artifact_uri" \
  --arg artifact_sha "$artifact_sha" \
  --arg runtime_contract "$runtime_contract" \
  '{artifact_uri:$artifact_uri,artifact_sha256:$artifact_sha,
    runtime_contract_sha256:$runtime_contract}' >"$artifact_release/release.json"
ln -s "$artifact_release/binance-lob-archiver" \
  "$fixture/opt/monday/bin/binance-lob-archiver"

bundle="$tmp_dir/deployment.tar"
manifest="$tmp_dir/controller-release.json"
make_bundle "$bundle_source" "$bundle"
bundle_sha=$(sha256sum "$bundle" | awk '{print $1}')
write_manifest "$manifest" "$artifact_sha" "$bundle_sha" "$runtime_contract"
active_metadata_before=$(sha256sum "$artifact_release/release.json" | awk '{print $1}')
active_target_before=$(readlink "$fixture/opt/monday/bin/binance-lob-archiver")

publish_controller_release "$fixture" "$artifact" "$bundle" "$manifest" \
  >"$tmp_dir/first.out"
controller_release="$fixture/opt/monday/releases/binance-lob-controller/$bundle_sha"
[[ -d $controller_release && ! -L $controller_release ]]
cmp -s "$manifest" "$controller_release/release.json"
(cd "$controller_release" \
  && sha256sum --check --strict release.json.sha256 >/dev/null \
  && sha256sum --check --strict deployment.sha256 >/dev/null)
[[ $(sha256sum "$artifact_release/release.json" | awk '{print $1}') \
  == "$active_metadata_before" ]]
[[ $(readlink "$fixture/opt/monday/bin/binance-lob-archiver") \
  == "$active_target_before" ]]
publish_controller_release "$fixture" "$artifact" "$bundle" "$manifest" \
  >"$tmp_dir/second.out"
grep -Fq 'already published' "$tmp_dir/second.out"

runtime_source="$tmp_dir/runtime-source"
cp -R "$bundle_source" "$runtime_source"
printf '\nRUNTIME_CONTRACT_FIXTURE=changed\n' \
  >>"$runtime_source/binance-lob-archiver-production-spot.env"
runtime_bundle="$tmp_dir/runtime.tar"
runtime_manifest="$tmp_dir/runtime.json"
make_bundle "$runtime_source" "$runtime_bundle"
runtime_bundle_sha=$(sha256sum "$runtime_bundle" | awk '{print $1}')
runtime_changed=$(monday_rust_lob_runtime_contract_sha256 "$runtime_source")
write_manifest "$runtime_manifest" "$artifact_sha" "$runtime_bundle_sha" "$runtime_changed"
if (publish_controller_release "$fixture" "$artifact" "$runtime_bundle" \
  "$runtime_manifest") >"$tmp_dir/runtime.out" 2>&1; then
  printf 'controller release accepted a changed runtime contract\n' >&2
  exit 1
fi
grep -Fq 'active release metadata differs from controller release identity' \
  "$tmp_dir/runtime.out"

bad_artifact="$tmp_dir/bad-artifact"
printf 'different artifact\n' >"$bad_artifact"
if (publish_controller_release "$fixture" "$bad_artifact" "$bundle" "$manifest") \
  >"$tmp_dir/artifact.out" 2>&1; then
  printf 'controller release accepted an artifact digest mismatch\n' >&2
  exit 1
fi
grep -Fq 'downloaded artifact digest differs' "$tmp_dir/artifact.out"

jq '.deployment_bundle_sha256 = ("0" * 64)' "$manifest" \
  >"$tmp_dir/wrong-manifest.json"
if (publish_controller_release "$fixture" "$artifact" "$bundle" \
  "$tmp_dir/wrong-manifest.json") >"$tmp_dir/manifest.out" 2>&1; then
  printf 'controller release accepted a bundle digest mismatch\n' >&2
  exit 1
fi
grep -Fq 'downloaded deployment bundle digest differs' "$tmp_dir/manifest.out"

symlink_source="$tmp_dir/symlink-source"
cp -R "$bundle_source" "$symlink_source"
rm "$symlink_source/host-rust-lob-restore.sh"
ln -s host-rust-lob-cutover.sh "$symlink_source/host-rust-lob-restore.sh"
symlink_bundle="$tmp_dir/symlink.tar"
symlink_manifest="$tmp_dir/symlink.json"
make_bundle "$symlink_source" "$symlink_bundle"
symlink_sha=$(sha256sum "$symlink_bundle" | awk '{print $1}')
write_manifest "$symlink_manifest" "$artifact_sha" "$symlink_sha" "$runtime_contract"
if (publish_controller_release "$fixture" "$artifact" "$symlink_bundle" \
  "$symlink_manifest") >"$tmp_dir/symlink.out" 2>&1; then
  printf 'controller release accepted a symlink asset\n' >&2
  exit 1
fi
grep -Fq 'deployment bundle contains a non-regular asset' "$tmp_dir/symlink.out"

printf 'Rust LOB controller release tests passed\n'
