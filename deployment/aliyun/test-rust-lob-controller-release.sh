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

source_dir="$ROOT/source"
mkdir -p "$source_dir"
assets=()
while IFS= read -r asset; do
  assets+=("$asset")
  cp "$SCRIPT_DIR/$asset" "$source_dir/$asset"
done < <({ monday_runtime_assets; monday_controller_assets; } | sort -u)

payload="$ROOT/payload"
printf '#!/usr/bin/env bash\nexit 0\n' >"$payload"
chmod 0755 "$payload"
payload_sha=$(monday_sha256_file "$payload")
runtime_sha=$(monday_rust_lob_runtime_contract_sha256 "$source_dir")
bundle="$ROOT/deployment.tar"
COPYFILE_DISABLE=1 tar -C "$source_dir" -cf "$bundle" "${assets[@]}"
bundle_sha=$(monday_sha256_file "$bundle")
manifest="$ROOT/release.json"
jq -cS -n --arg uri oss://bucket/payload --arg sha "$payload_sha" \
  --arg runtime "$runtime_sha" --arg source "$(printf 'a%.0s' {1..40})" \
  --arg bundle oss://bucket/controller --arg bundle_sha "$bundle_sha" \
  '{schema:"monday.rust_lob_controller_release.v2",control_plane_version:2,
    topology:"stable",artifact_uri:$uri,artifact_sha256:$sha,
    runtime_contract_sha256:$runtime,deployment_source_revision:$source,
    deployment_bundle_uri:$bundle,deployment_bundle_sha256:$bundle_sha}' >"$manifest"

publish_controller_release "$payload" "$bundle" "$manifest" "$ROOT" \
  >/dev/null
controller_sha=$(monday_sha256_file "$manifest")
controller="$ROOT/opt/monday/releases/binance-lob-controller/$controller_sha"
[[ -d $controller && -L $controller/binance-lob-archiver ]]
monday_verify_controller_release "$ROOT" "$controller_sha"
publish_controller_release "$payload" "$bundle" "$manifest" "$ROOT" \
  | grep -Fq 'already published'
[[ ! -e "$ROOT/opt/monday/releases/binance-lob-controller/active" ]]

if jq '.control_plane_version = 1' "$manifest" >"$ROOT/v1.json"; then
  if publish_controller_release "$payload" "$bundle" "$ROOT/v1.json" "$ROOT" \
    >/dev/null 2>&1; then
    printf 'accepted a V1 controller manifest\n' >&2
    exit 1
  fi
fi

duplicate_bundle="$ROOT/duplicate.tar"
cp "$bundle" "$duplicate_bundle"
COPYFILE_DISABLE=1 tar -C "$source_dir" -rf "$duplicate_bundle" "${assets[0]}"
duplicate_sha=$(monday_sha256_file "$duplicate_bundle")
jq --arg sha "$duplicate_sha" '.deployment_bundle_sha256 = $sha' "$manifest" >"$ROOT/duplicate.json"
if publish_controller_release "$payload" "$duplicate_bundle" "$ROOT/duplicate.json" "$ROOT" \
  >/dev/null 2>&1; then
  printf 'accepted a duplicate-member deployment archive\n' >&2
  exit 1
fi

printf 'unexpected controller payload\n' >"$source_dir/unexpected.txt"
extra_bundle="$ROOT/extra.tar"
cp "$bundle" "$extra_bundle"
COPYFILE_DISABLE=1 tar -C "$source_dir" -rf "$extra_bundle" unexpected.txt
extra_sha=$(monday_sha256_file "$extra_bundle")
jq --arg sha "$extra_sha" '.deployment_bundle_sha256 = $sha' "$manifest" >"$ROOT/extra.json"
if publish_controller_release "$payload" "$extra_bundle" "$ROOT/extra.json" "$ROOT" \
  >/dev/null 2>&1; then
  printf 'accepted an unexpected deployment archive member\n' >&2
  exit 1
fi

printf 'controller V2 release contract passed\n'
