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
  host-rust-lob-shadow-gate.sh
  host-rust-lob-cutover.sh
  host-rust-lob-restore.sh
  host-rust-lob-recovery-queue.sh
  host-rust-lob-controller-release.sh
  host-rust-lob-controller-apply.sh
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

make_artifact_release() {
  local artifact_file=$1 artifact_sha=$2 artifact_uri=$3 deployment_source=$4
  local release="$fixture/opt/monday/releases/binance-lob-archiver/$artifact_sha"
  local runtime_contract
  mkdir -p "$release/deployment"
  for asset in "${assets[@]}"; do
    cp "$deployment_source/$asset" "$release/deployment/$asset"
  done
  cp "$artifact_file" "$release/binance-lob-archiver"
  runtime_contract=$(monday_rust_lob_runtime_contract_sha256 "$release/deployment")
  jq -n \
    --arg artifact_uri "$artifact_uri" \
    --arg artifact_sha "$artifact_sha" \
    --arg runtime_contract "$runtime_contract" \
    '{artifact_uri:$artifact_uri,artifact_sha256:$artifact_sha,
      runtime_contract_sha256:$runtime_contract}' >"$release/release.json"
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
artifact_uri="oss://bucket/releases/$artifact_sha/binance-lob-archiver"
make_artifact_release "$artifact" "$artifact_sha" "$artifact_uri" "$SCRIPT_DIR"
runtime_contract=$(jq -er '.runtime_contract_sha256' "$artifact_release/release.json")

production_artifact="$tmp_dir/binance-lob-archiver-production"
printf '#!/usr/bin/env bash\nprintf production\\nexit 0\n' >"$production_artifact"
chmod 0755 "$production_artifact"
production_artifact_sha=$(sha256sum "$production_artifact" | awk '{print $1}')
production_artifact_release="$fixture/opt/monday/releases/binance-lob-archiver/$production_artifact_sha"
production_artifact_uri="oss://bucket/releases/$production_artifact_sha/binance-lob-archiver"
make_artifact_release \
  "$production_artifact" "$production_artifact_sha" "$production_artifact_uri" "$SCRIPT_DIR"
ln -s "$production_artifact_release/binance-lob-archiver" \
  "$fixture/opt/monday/bin/binance-lob-archiver"

bundle="$tmp_dir/deployment.tar"
manifest="$tmp_dir/controller-release.json"
make_bundle "$bundle_source" "$bundle"
bundle_members=$(tar -tf "$bundle")
grep -Fxq 'host-rust-lob-shadow-gate.sh' <<<"$bundle_members"
for removed_asset in \
  host-rust-lob-shadow-preflight.sh \
  host-rust-lob-shadow-soak.sh; do
  if grep -Fxq "$removed_asset" <<<"$bundle_members"; then
    printf 'controller release bundle retained removed asset: %s\n' "$removed_asset" >&2
    exit 1
  fi
done
bundle_sha=$(sha256sum "$bundle" | awk '{print $1}')
write_manifest "$manifest" "$artifact_sha" "$bundle_sha" "$runtime_contract"
manifest_sha=$(sha256sum "$manifest" | awk '{print $1}')
staged_metadata_before=$(sha256sum "$artifact_release/release.json" | awk '{print $1}')
production_target_before=$(readlink "$fixture/opt/monday/bin/binance-lob-archiver")
controller_root="$fixture/opt/monday/releases/binance-lob-controller"
[[ ! -e "$controller_root/active" && ! -L "$controller_root/active" ]]

publish_controller_release "$fixture" "$artifact" "$bundle" "$manifest" \
  >"$tmp_dir/first.out"
controller_release="$fixture/opt/monday/releases/binance-lob-controller/$manifest_sha"
[[ -d $controller_release && ! -L $controller_release ]]
cmp -s "$manifest" "$controller_release/release.json"
(cd "$controller_release" \
  && sha256sum --check --strict release.json.sha256 >/dev/null \
  && sha256sum --check --strict deployment.sha256 >/dev/null)
[[ $(sha256sum "$artifact_release/release.json" | awk '{print $1}') \
  == "$staged_metadata_before" ]]
[[ $(readlink "$fixture/opt/monday/bin/binance-lob-archiver") \
  == "$production_target_before" ]]
publish_controller_release "$fixture" "$artifact" "$bundle" "$manifest" \
  >"$tmp_dir/second.out"
grep -Fq 'already published' "$tmp_dir/second.out"

ln -s "$controller_release" "$controller_root/active"
active_controller_before=$(readlink "$controller_root/active")
p0_manifest="$tmp_dir/production-controller-release.json"
write_manifest "$p0_manifest" "$production_artifact_sha" "$bundle_sha" "$runtime_contract"
p0_manifest_sha=$(sha256sum "$p0_manifest" | awk '{print $1}')
publish_controller_release "$fixture" "$production_artifact" "$bundle" \
  "$p0_manifest" >"$tmp_dir/production.out"
production_controller_release="$controller_root/$p0_manifest_sha"
[[ -d $production_controller_release && ! -L $production_controller_release ]]
[[ $p0_manifest_sha != "$manifest_sha" ]]
[[ $(readlink "$controller_root/active") == "$active_controller_before" ]]
[[ $(readlink "$fixture/opt/monday/bin/binance-lob-archiver") == "$production_target_before" ]]

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
grep -Fq 'staged artifact metadata differs from controller release identity' \
  "$tmp_dir/runtime.out"

runtime_artifact="$tmp_dir/binance-lob-archiver-runtime"
printf '#!/usr/bin/env bash\nprintf runtime\\nexit 0\n' >"$runtime_artifact"
chmod 0755 "$runtime_artifact"
runtime_artifact_sha=$(sha256sum "$runtime_artifact" | awk '{print $1}')
runtime_artifact_uri="oss://bucket/releases/$runtime_artifact_sha/binance-lob-archiver"
make_artifact_release \
  "$runtime_artifact" "$runtime_artifact_sha" "$runtime_artifact_uri" "$runtime_source"
runtime_candidate_manifest="$tmp_dir/runtime-candidate.json"
write_manifest "$runtime_candidate_manifest" "$runtime_artifact_sha" \
  "$runtime_bundle_sha" "$runtime_changed"
runtime_candidate_sha=$(sha256sum "$runtime_candidate_manifest" | awk '{print $1}')
publish_controller_release "$fixture" "$runtime_artifact" "$runtime_bundle" \
  "$runtime_candidate_manifest" >"$tmp_dir/runtime-candidate.out"
[[ -d "$controller_root/$runtime_candidate_sha" \
  && ! -L "$controller_root/$runtime_candidate_sha" ]]
[[ $(readlink "$fixture/opt/monday/bin/binance-lob-archiver") \
  == "$production_target_before" ]]
[[ $(readlink "$controller_root/active") == "$active_controller_before" ]]

staged_tampered_source="$tmp_dir/staged-tampered-source"
cp -R "$SCRIPT_DIR" "$staged_tampered_source"
printf '\nSTAGED_RUNTIME_CONTRACT_FIXTURE=changed\n' \
  >>"$staged_tampered_source/binance-lob-archiver-production-spot.env"
staged_tampered_sentinel="$tmp_dir/staged-helper-executed"
printf '\nmonday_rust_lob_runtime_contract_sha256() { printf "%%s\\n" "%s"; : > "%s"; }\n' \
  "$runtime_contract" "$staged_tampered_sentinel" \
  >>"$staged_tampered_source/rust-lob-control-plane-lib.sh"
staged_tampered_artifact="$tmp_dir/binance-lob-archiver-tampered"
printf '#!/usr/bin/env bash\nprintf tampered\\nexit 0\n' >"$staged_tampered_artifact"
chmod 0755 "$staged_tampered_artifact"
staged_tampered_sha=$(sha256sum "$staged_tampered_artifact" | awk '{print $1}')
staged_tampered_uri="oss://bucket/releases/$staged_tampered_sha/binance-lob-archiver"
make_artifact_release \
  "$staged_tampered_artifact" "$staged_tampered_sha" \
  "$staged_tampered_uri" "$staged_tampered_source"
staged_tampered_release="$fixture/opt/monday/releases/binance-lob-archiver/$staged_tampered_sha"
jq --arg runtime_contract "$runtime_contract" \
  '.runtime_contract_sha256 = $runtime_contract' \
  "$staged_tampered_release/release.json" \
  >"$tmp_dir/staged-tampered-release.json"
mv "$tmp_dir/staged-tampered-release.json" "$staged_tampered_release/release.json"
staged_tampered_manifest="$tmp_dir/staged-tampered-controller.json"
write_manifest "$staged_tampered_manifest" "$staged_tampered_sha" \
  "$bundle_sha" "$runtime_contract"
staged_tampered_manifest_sha=$(sha256sum "$staged_tampered_manifest" | awk '{print $1}')
if (publish_controller_release "$fixture" "$staged_tampered_artifact" "$bundle" \
  "$staged_tampered_manifest") >"$tmp_dir/staged-tampered.out" 2>&1; then
  printf 'controller release trusted a tampered staged runtime helper\n' >&2
  exit 1
fi
grep -Fq 'staged artifact runtime contract drifted from release metadata' \
  "$tmp_dir/staged-tampered.out"
[[ ! -e "$controller_root/$staged_tampered_manifest_sha" \
  && ! -L "$controller_root/$staged_tampered_manifest_sha" ]]
[[ ! -e "$staged_tampered_sentinel" && ! -L "$staged_tampered_sentinel" ]]
[[ $(readlink "$fixture/opt/monday/bin/binance-lob-archiver") \
  == "$production_target_before" ]]
[[ $(readlink "$controller_root/active") == "$active_controller_before" ]]

tampered_candidate_helper_source="$tmp_dir/tampered-candidate-helper-source"
cp -R "$bundle_source" "$tampered_candidate_helper_source"
printf '\nCANDIDATE_HELPER_TAMPER=changed\n' \
  >>"$tampered_candidate_helper_source/rust-lob-control-plane-lib.sh"
tampered_candidate_helper_bundle="$tmp_dir/tampered-candidate-helper.tar"
make_bundle "$tampered_candidate_helper_source" "$tampered_candidate_helper_bundle"
if (publish_controller_release "$fixture" "$artifact" \
  "$tampered_candidate_helper_bundle" "$manifest") \
  >"$tmp_dir/tampered-candidate-helper.out" 2>&1; then
  printf 'controller release accepted a tampered candidate helper\n' >&2
  exit 1
fi
grep -Fq 'downloaded deployment bundle digest differs' \
  "$tmp_dir/tampered-candidate-helper.out"

tampered_candidate_runtime_source="$tmp_dir/tampered-candidate-runtime-source"
cp -R "$bundle_source" "$tampered_candidate_runtime_source"
printf '\nCANDIDATE_RUNTIME_TAMPER=changed\n' \
  >>"$tampered_candidate_runtime_source/binance-lob-archiver-production-spot.env"
tampered_candidate_runtime_bundle="$tmp_dir/tampered-candidate-runtime.tar"
make_bundle "$tampered_candidate_runtime_source" "$tampered_candidate_runtime_bundle"
if (publish_controller_release "$fixture" "$artifact" \
  "$tampered_candidate_runtime_bundle" "$manifest") \
  >"$tmp_dir/tampered-candidate-runtime.out" 2>&1; then
  printf 'controller release accepted a tampered candidate runtime asset\n' >&2
  exit 1
fi
grep -Fq 'downloaded deployment bundle digest differs' \
  "$tmp_dir/tampered-candidate-runtime.out"

controller_manifest_tampered="$tmp_dir/controller-manifest-tampered.json"
jq '.deployment_source_revision = ("d" * 40)' "$manifest" \
  >"$controller_manifest_tampered"
controller_manifest_tampered_sha=$(sha256sum "$controller_manifest_tampered" | awk '{print $1}')
controller_manifest_tampered_real="$tmp_dir/controller-release-real"
cp -R "$controller_release" "$controller_manifest_tampered_real"
ln -s "$controller_manifest_tampered_real" \
  "$controller_root/$controller_manifest_tampered_sha"
if (publish_controller_release "$fixture" "$artifact" "$bundle" \
  "$controller_manifest_tampered") >"$tmp_dir/controller-path.out" 2>&1; then
  printf 'controller release accepted an indirect existing release path\n' >&2
  exit 1
fi
grep -Fq 'controller release path is indirect' "$tmp_dir/controller-path.out"

controller_root_real="$tmp_dir/controller-root-real"
mv "$controller_root" "$controller_root_real"
ln -s "$controller_root_real" "$controller_root"
if (publish_controller_release "$fixture" "$artifact" "$bundle" "$manifest") \
  >"$tmp_dir/controller-root.out" 2>&1; then
  printf 'controller release accepted an indirect controller release root\n' >&2
  exit 1
fi
grep -Fq 'controller release root is indirect' "$tmp_dir/controller-root.out"
rm "$controller_root"
mv "$controller_root_real" "$controller_root"

release_manifest_backup="$tmp_dir/release.json.backup"
release_checksum_backup="$tmp_dir/release.json.sha256.backup"
cp "$controller_release/release.json" "$release_manifest_backup"
cp "$controller_release/release.json.sha256" "$release_checksum_backup"
chmod u+w "$controller_release"
chmod u+w "$controller_release/release.json" "$controller_release/release.json.sha256"
printf '{"tampered":true}\n' >"$controller_release/release.json"
sha256sum "$controller_release/release.json" >"$controller_release/release.json.sha256"
chmod 0444 "$controller_release/release.json" "$controller_release/release.json.sha256"
if (publish_controller_release "$fixture" "$artifact" "$bundle" "$manifest") \
  >"$tmp_dir/controller-manifest.out" 2>&1; then
  printf 'controller release accepted a manifest digest mismatch\n' >&2
  exit 1
fi
grep -Fq 'existing controller release manifest differs' "$tmp_dir/controller-manifest.out"
chmod u+w "$controller_release"
chmod u+w "$controller_release/release.json" "$controller_release/release.json.sha256"
cp "$release_manifest_backup" "$controller_release/release.json"
cp "$release_checksum_backup" "$controller_release/release.json.sha256"
chmod 0444 "$controller_release/release.json" "$controller_release/release.json.sha256"
chmod 0555 "$controller_release"

release_checksum_backup="$tmp_dir/deployment.sha256.backup"
cp "$controller_release/deployment.sha256" "$release_checksum_backup"
chmod u+w "$controller_release"
chmod u+w "$controller_release/deployment.sha256"
printf 'tampered\n' >"$controller_release/deployment.sha256"
chmod 0444 "$controller_release/deployment.sha256"
if (publish_controller_release "$fixture" "$artifact" "$bundle" "$manifest") \
  >"$tmp_dir/controller-checksum.out" 2>&1; then
  printf 'controller release accepted a checksum mismatch\n' >&2
  exit 1
fi
grep -Fq 'existing controller release checksum verification failed' \
  "$tmp_dir/controller-checksum.out"
chmod u+w "$controller_release"
chmod u+w "$controller_release/deployment.sha256"
cp "$release_checksum_backup" "$controller_release/deployment.sha256"
chmod 0444 "$controller_release/deployment.sha256"
chmod 0555 "$controller_release"

indirect_artifact="$tmp_dir/binance-lob-archiver-indirect"
printf '#!/usr/bin/env bash\nprintf indirect\\nexit 0\n' >"$indirect_artifact"
chmod 0755 "$indirect_artifact"
indirect_artifact_sha=$(sha256sum "$indirect_artifact" | awk '{print $1}')
indirect_artifact_uri="oss://bucket/releases/$indirect_artifact_sha/binance-lob-archiver"
indirect_artifact_release="$fixture/opt/monday/releases/binance-lob-archiver/$indirect_artifact_sha"
make_artifact_release \
  "$indirect_artifact" "$indirect_artifact_sha" "$indirect_artifact_uri" "$SCRIPT_DIR"
indirect_artifact_real="$tmp_dir/indirect-artifact-real"
mv "$indirect_artifact_release" "$indirect_artifact_real"
ln -s "$indirect_artifact_real" "$indirect_artifact_release"
indirect_manifest="$tmp_dir/indirect-artifact.json"
write_manifest "$indirect_manifest" "$indirect_artifact_sha" "$bundle_sha" \
  "$runtime_contract"
if (publish_controller_release "$fixture" "$indirect_artifact" "$bundle" \
  "$indirect_manifest") >"$tmp_dir/indirect-artifact.out" 2>&1; then
  printf 'controller release accepted an indirect staged artifact release\n' >&2
  exit 1
fi
grep -Fq 'staged artifact release is missing or indirect' \
  "$tmp_dir/indirect-artifact.out"

tampered_source="$tmp_dir/tampered-source"
cp -R "$bundle_source" "$tampered_source"
printf '\nTAMPERED_RUNTIME_FIXTURE=changed\n' \
  >>"$tampered_source/binance-lob-archiver-production-spot.env"
printf '\nmonday_rust_lob_runtime_contract_sha256() { printf "%%s\\n" "%s"; }\n' \
  "$runtime_contract" >>"$tampered_source/rust-lob-control-plane-lib.sh"
tampered_bundle="$tmp_dir/tampered.tar"
tampered_manifest="$tmp_dir/tampered.json"
make_bundle "$tampered_source" "$tampered_bundle"
tampered_bundle_sha=$(sha256sum "$tampered_bundle" | awk '{print $1}')
write_manifest "$tampered_manifest" "$artifact_sha" "$tampered_bundle_sha" \
  "$runtime_contract"
if (publish_controller_release "$fixture" "$artifact" "$tampered_bundle" \
  "$tampered_manifest") >"$tmp_dir/tampered.out" 2>&1; then
  printf 'controller release trusted a tampered candidate runtime helper\n' >&2
  exit 1
fi
grep -Fq 'controller bundle changes the gated runtime contract' \
  "$tmp_dir/tampered.out"

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
