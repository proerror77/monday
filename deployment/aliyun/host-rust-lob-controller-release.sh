#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf 'Usage: %s <artifact> <bundle.tar> <controller-release.json> [root]\n' "${0##*/}" >&2
}

die() { printf '%s\n' "$*" >&2; exit 1; }

root_join() {
  local root=${1:-/} suffix=${2#/}
  root=${root%/}
  [[ -n $root ]] || root=/
  if [[ $root == / ]]; then printf '/%s\n' "$suffix"; else printf '%s/%s\n' "$root" "$suffix"; fi
}

ROOT=${MONDAY_ROOT:-/}
sha256_file() { monday_sha256_file "$1"; }
regular_file() { monday_file_direct "$1"; }
direct_directory() { monday_path_direct "$1"; }

configure() {
  ROOT=${1:-$ROOT}; ROOT=${ROOT%/}
  [[ -n $ROOT ]] || ROOT=/
  ARTIFACT_ROOT=$(root_join "$ROOT" opt/monday/releases/binance-lob-archiver)
  CONTROLLER_ROOT=$(root_join "$ROOT" opt/monday/releases/binance-lob-controller)
}

validate_archive() {
  local archive=$1 list normalized entry invalid=false
  list=$(mktemp)
  tar -tf "$archive" >"$list" || { rm -f "$list"; return 1; }
  [[ -s $list ]] || { rm -f "$list"; return 1; }
  # bsdtar escapes a literal backslash in member names as `\\` while GNU tar
  # emits the byte unchanged.  Normalize only the one reviewed systemd slice
  # asset; every other member remains byte-for-byte subject to the allowlist.
  normalized=$(mktemp)
  while IFS= read -r entry; do
    if [[ $entry == 'system-binance\\x2dlob\\x2darchiver\\x2dproduction.slice' ]]; then
      entry='system-binance\x2dlob\x2darchiver\x2dproduction.slice'
    fi
    [[ $entry == 'system-binance\x2dlob\x2darchiver\x2dproduction.slice' \
      || $entry =~ ^[A-Za-z0-9][A-Za-z0-9._@+-]*$ ]] || {
      invalid=true
      break
    }
    printf '%s\n' "$entry" >>"$normalized"
  done <"$list"
  if [[ $invalid == true ]]; then
    rm -f "$list" "$normalized"
    return 1
  fi
  if [[ -n $(sort "$normalized" | uniq -d) ]]; then
    rm -f "$list" "$normalized"
    return 1
  fi
  mv -f "$normalized" "$list"
  printf '%s\n' "$list"
}

extract_archive() {
  local archive=$1 destination=$2 list=$3 entry
  mkdir "$destination"
  tar --no-same-owner --no-same-permissions -xf "$archive" -C "$destination"
  while IFS= read -r entry; do
    regular_file "$destination/$entry" || return 1
  done <"$list"
}

validate_payload() {
  local artifact=$1 artifact_sha=$2 release=$3 metadata
  regular_file "$artifact" || die 'payload artifact is not a regular file'
  [[ $(sha256_file "$artifact") == "$artifact_sha" ]] \
    || die 'payload artifact digest mismatch'
  metadata="$release/release.json"
  if [[ -e $metadata || -L $metadata ]]; then
    regular_file "$metadata" || die 'payload metadata is not a regular file'
    jq -e --arg sha "$artifact_sha" '.artifact_sha256 == $sha' "$metadata" >/dev/null \
      || die 'existing payload metadata binds a different artifact'
  fi
}

install_payload_release() {
  local artifact=$1 artifact_sha=$2 artifact_uri=$3 source_revision=$4 bundle_sha=$5 bundle_uri=$6 runtime=$7 extracted=$8 release staging asset
  release="$ARTIFACT_ROOT/$artifact_sha"
  if [[ -e $release || -L $release ]]; then
    direct_directory "$release" || die 'payload release path is indirect'
    validate_payload "$release/binance-lob-archiver" "$artifact_sha" "$release"
    return 0
  fi
  staging=$(mktemp -d "$ARTIFACT_ROOT/.${artifact_sha}.new.XXXXXX")
  trap 'rm -rf "${staging:-}"' RETURN
  install -m 0755 "$artifact" "$staging/binance-lob-archiver"
  mkdir -m 0755 "$staging/deployment"
  while IFS= read -r asset; do
    [[ -n $asset ]] || continue
    install -m 0444 "$extracted/$asset" "$staging/deployment/$asset"
  done < <(monday_runtime_assets)
  jq -cn --arg uri "$artifact_uri" --arg sha "$artifact_sha" \
    --arg runtime "$runtime" --arg source "$source_revision" \
    --arg bundle "$bundle_uri" --arg bundle_sha "$bundle_sha" \
    '{schema:"monday.rust_lob_payload_release.v1",artifact_uri:$uri,
      artifact_sha256:$sha,runtime_contract_sha256:$runtime,
      deployment_source_revision:$source,deployment_bundle_uri:$bundle,
      deployment_bundle_sha256:$bundle_sha}' >"$staging/release.json"
  chmod 0444 "$staging/release.json"
  chmod 0555 "$staging" "$staging/deployment"
  mv -f "$staging" "$release"
  staging=
}

publish_controller_release() (
  [[ $# -ge 3 && $# -le 4 ]] || { usage; return 2; }
  local artifact=$1 bundle=$2 manifest=$3 root=${4:-${MONDAY_ROOT:-/}}
  local artifact_sha artifact_uri bundle_sha bundle_uri source runtime manifest_sha release work extracted list asset mode expected_assets
  configure "$root"
  # shellcheck disable=SC1091
  . "$(dirname -- "$0")/rust-lob-control-plane-lib.sh"
  regular_file "$artifact" || die 'artifact input is missing'
  regular_file "$bundle" || die 'deployment bundle input is missing'
  regular_file "$manifest" || die 'controller manifest input is missing'
  monday_validate_v2_manifest "$manifest" || die 'controller manifest is not V2'
  artifact_sha=$(monday_manifest_field "$manifest" artifact_sha256)
  artifact_uri=$(monday_manifest_field "$manifest" artifact_uri)
  bundle_sha=$(monday_manifest_field "$manifest" deployment_bundle_sha256)
  bundle_uri=$(monday_manifest_field "$manifest" deployment_bundle_uri)
  source=$(monday_manifest_field "$manifest" deployment_source_revision)
  runtime=$(monday_manifest_field "$manifest" runtime_contract_sha256)
  [[ $(sha256_file "$bundle") == "$bundle_sha" ]] || die 'deployment bundle digest mismatch'
  if ! direct_directory "$ARTIFACT_ROOT"; then
    mkdir -p "$ARTIFACT_ROOT"
    direct_directory "$ARTIFACT_ROOT" || die 'payload release root is not a direct directory'
  fi
  if ! direct_directory "$CONTROLLER_ROOT"; then
    mkdir -p "$CONTROLLER_ROOT"
    direct_directory "$CONTROLLER_ROOT" || die 'controller release root is not a direct directory'
  fi
  manifest_sha=$(sha256_file "$manifest")
  release="$CONTROLLER_ROOT/$manifest_sha"
  work=$(mktemp -d); extracted="$work/deployment"
  list=$(validate_archive "$bundle") || die 'deployment bundle contains an unsafe member'
  trap 'rm -rf "${work:-}"' EXIT
  extract_archive "$bundle" "$extracted" "$list" || die 'deployment bundle contains a non-regular member'
  expected_assets=$(printf '%s\n' "$(monday_runtime_assets)" "$(monday_controller_assets)" | sort -u)
  cmp -s <(sort "$list") <(printf '%s\n' "$expected_assets") \
    || die 'deployment bundle contains an unexpected or missing asset'
  for asset in $(monday_runtime_assets) $(monday_controller_assets); do
    regular_file "$extracted/$asset" || die "deployment bundle is missing $asset"
  done
  [[ $(monday_rust_lob_runtime_contract_sha256 "$extracted") == "$runtime" ]] \
    || die 'runtime contract differs from the manifest'
  validate_payload "$artifact" "$artifact_sha" "$ARTIFACT_ROOT/$artifact_sha"
  install_payload_release "$artifact" "$artifact_sha" "$artifact_uri" "$source" \
    "$bundle_sha" "$bundle_uri" "$runtime" "$extracted"
  if [[ -e $release || -L $release ]]; then
    monday_verify_controller_release "$ROOT" "$manifest_sha" \
      || die 'existing controller release does not match the immutable manifest'
    printf 'controller release already published: %s\n' "$manifest_sha"
    return 0
  fi
  local staging
  staging=$(mktemp -d "$CONTROLLER_ROOT/.${manifest_sha}.new.XXXXXX")
  trap 'rm -rf "${staging:-}" "${work:-}"' EXIT
  mkdir -m 0755 "$staging/deployment"
  while IFS= read -r asset; do
    [[ -n $asset ]] || continue
    mode=0444
    [[ $asset == *.sh ]] && mode=0555
    install -m "$mode" "$extracted/$asset" "$staging/deployment/$asset"
  done < <(printf '%s\n' "$(monday_runtime_assets)" "$(monday_controller_assets)" | sort -u)
  ln -s "$ARTIFACT_ROOT/$artifact_sha/binance-lob-archiver" "$staging/binance-lob-archiver"
  install -m 0444 "$manifest" "$staging/release.json"
  (
    cd "$staging"
    monday_sha256_checksum_line release.json >release.json.sha256
    : >deployment.sha256
    for asset in deployment/*; do
      monday_sha256_checksum_line "$asset" >>deployment.sha256
    done
    sort -k2 -o deployment.sha256 deployment.sha256
  )
  chmod 0444 "$staging/release.json.sha256" "$staging/deployment.sha256"
  chmod 0555 "$staging" "$staging/deployment"
  mv -f "$staging" "$release"
  staging=
  monday_verify_controller_release "$ROOT" "$manifest_sha" \
    || die 'published controller release failed verification'
  printf 'published controller V2 %s (payload %s); production unchanged\n' \
    "$manifest_sha" "$artifact_sha"
)

main() {
  [[ ${EUID:-$(id -u)} -eq 0 || ${MONDAY_CONTROL_PLANE_TEST:-0} == 1 ]] \
    || die 'controller publication must run as root'
  publish_controller_release "$@"
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
