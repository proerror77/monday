#!/usr/bin/env bash
set -Eeuo pipefail
umask 027
export LC_ALL=C

usage() {
  printf 'Usage: %s <artifact-file> <deployment-bundle.tar> <controller-release.json>\n' \
    "${0##*/}" >&2
}

configure_paths() {
  local root=${1%/}
  ARTIFACT_RELEASE_ROOT="$root/opt/monday/releases/binance-lob-archiver"
  CONTROLLER_RELEASE_ROOT="$root/opt/monday/releases/binance-lob-controller"
  PRODUCTION_BINARY="$root/opt/monday/bin/binance-lob-archiver"
  LOCK_ROOT="$root/run/lock"
}

die() {
  printf '%s\n' "$*" >&2
  exit 1
}

sha256_file() {
  sha256sum "$1" | awk '{print $1}'
}

direct_directory() {
  local path=$1 resolved
  [[ -d $path && ! -L $path ]] || return 1
  resolved=$(readlink -f -- "$path") || return 1
  [[ $resolved == "$path" ]]
}

regular_file() {
  [[ -f $1 && ! -L $1 ]]
}

validate_manifest() {
  jq -e '
    type == "object"
    and keys == [
      "artifact_sha256",
      "artifact_uri",
      "deployment_bundle_sha256",
      "deployment_bundle_uri",
      "deployment_source_revision",
      "runtime_contract_sha256",
      "schema"
    ]
    and .schema == "monday.rust_lob_controller_release.v1"
    and (.artifact_sha256 | type == "string" and test("^[a-f0-9]{64}$"))
    and (.artifact_uri
      | type == "string"
      and test("^oss://[A-Za-z0-9][A-Za-z0-9.-]*/[A-Za-z0-9._/@+=:-]+$"))
    and (.deployment_bundle_sha256
      | type == "string" and test("^[a-f0-9]{64}$"))
    and (.deployment_bundle_uri
      | type == "string"
      and test("^oss://[A-Za-z0-9][A-Za-z0-9.-]*/[A-Za-z0-9._/@+=:-]+$"))
    and (.deployment_source_revision
      | type == "string" and test("^[a-f0-9]{40,64}$"))
    and (.runtime_contract_sha256
      | type == "string" and test("^[a-f0-9]{64}$"))' \
    "$1" >/dev/null
}

extract_bundle() {
  local bundle=$1 destination=$2 listing=$3 entry duplicate
  tar -tf "$bundle" >"$listing" || die 'deployment bundle cannot be listed'
  [[ -s $listing ]] || die 'deployment bundle is empty'
  while IFS= read -r entry; do
    [[ $entry =~ ^[A-Za-z0-9][A-Za-z0-9._@+-]*$ ]] \
      || die "deployment bundle contains an unsafe asset name: $entry"
  done <"$listing"
  duplicate=$(sort "$listing" | uniq -d | head -n 1)
  [[ -z $duplicate ]] || die "deployment bundle repeats an asset: $duplicate"
  tar --no-same-owner --no-same-permissions -xf "$bundle" -C "$destination"
  while IFS= read -r entry; do
    regular_file "$destination/$entry" \
      || die "deployment bundle contains a non-regular asset: $entry"
  done <"$listing"
}

verify_published_release() {
  local release_dir=$1 extracted=$2 manifest=$3 listing=$4 source asset
  local expected_assets actual_assets top_level
  direct_directory "$release_dir" \
    || die "controller release path is indirect: $release_dir"
  direct_directory "$release_dir/deployment" \
    || die 'controller release deployment is indirect'
  regular_file "$release_dir/release.json" \
    || die 'controller release manifest is not a regular file'
  regular_file "$release_dir/release.json.sha256" \
    || die 'controller release manifest checksum is not a regular file'
  regular_file "$release_dir/deployment.sha256" \
    || die 'controller deployment checksum is not a regular file'
  cmp -s "$manifest" "$release_dir/release.json" \
    || die 'existing controller release manifest differs'
  (cd "$release_dir" \
    && sha256sum --check --strict release.json.sha256 >/dev/null \
    && sha256sum --check --strict deployment.sha256 >/dev/null) \
    || die 'existing controller release checksum verification failed'

  expected_assets=$(wc -l <"$listing" | tr -d ' ')
  actual_assets=$(find "$release_dir/deployment" -mindepth 1 -maxdepth 1 -print \
    | wc -l | tr -d ' ')
  [[ $actual_assets == "$expected_assets" ]] \
    || die 'existing controller release contains unexpected deployment assets'
  while IFS= read -r asset; do
    source="$extracted/$asset"
    if ! regular_file "$release_dir/deployment/$asset" \
      || ! cmp -s "$source" "$release_dir/deployment/$asset"; then
      die "existing controller release differs from bundle: $asset"
    fi
  done <"$listing"
  top_level=$(find "$release_dir" -mindepth 1 -maxdepth 1 -print \
    | wc -l | tr -d ' ')
  [[ $top_level == 4 ]] \
    || die 'existing controller release contains unexpected top-level assets'
}

publish_controller_release() (
  [[ $# -eq 4 ]] || return 2
  local root=$1 artifact_file=$2 bundle=$3 manifest=$4
  local artifact_sha artifact_uri bundle_sha manifest_sha runtime_contract source_revision
  local active_release active_binary active_metadata active_deployment
  local work_dir extracted listing release_dir staging asset mode

  configure_paths "$root"
  regular_file "$artifact_file" || die 'artifact input is not a regular file'
  regular_file "$bundle" || die 'deployment bundle input is not a regular file'
  regular_file "$manifest" || die 'controller release manifest is not a regular file'
  validate_manifest "$manifest" || die 'controller release manifest is invalid'

  artifact_sha=$(jq -er '.artifact_sha256' "$manifest")
  artifact_uri=$(jq -er '.artifact_uri' "$manifest")
  bundle_sha=$(jq -er '.deployment_bundle_sha256' "$manifest")
  manifest_sha=$(sha256_file "$manifest")
  runtime_contract=$(jq -er '.runtime_contract_sha256' "$manifest")
  source_revision=$(jq -er '.deployment_source_revision' "$manifest")
  [[ $(sha256_file "$artifact_file") == "$artifact_sha" ]] \
    || die 'downloaded artifact digest differs from controller release manifest'
  [[ $(sha256_file "$bundle") == "$bundle_sha" ]] \
    || die 'downloaded deployment bundle digest differs from controller release manifest'

  active_release="$ARTIFACT_RELEASE_ROOT/$artifact_sha"
  active_binary="$active_release/binance-lob-archiver"
  active_metadata="$active_release/release.json"
  active_deployment="$active_release/deployment"
  [[ -L $PRODUCTION_BINARY ]] \
    || die 'production binary is not a digest-addressed symlink'
  direct_directory "$active_release" || die 'active artifact release is indirect'
  direct_directory "$active_deployment" || die 'active artifact deployment is indirect'
  regular_file "$active_binary" || die 'active release binary is not a regular file'
  regular_file "$active_metadata" || die 'active release metadata is not a regular file'
  [[ $(readlink -f -- "$PRODUCTION_BINARY") == "$active_binary" ]] \
    || die 'production binary does not resolve to the requested artifact release'
  [[ $(sha256_file "$active_binary") == "$artifact_sha" ]] \
    || die 'active release binary digest mismatch'
  jq -e \
    --arg artifact_sha "$artifact_sha" \
    --arg artifact_uri "$artifact_uri" \
    --arg runtime_contract "$runtime_contract" '
      .artifact_sha256 == $artifact_sha
      and .artifact_uri == $artifact_uri
      and .runtime_contract_sha256 == $runtime_contract' \
    "$active_metadata" >/dev/null \
    || die 'active release metadata differs from controller release identity'

  work_dir=$(mktemp -d)
  extracted="$work_dir/deployment"
  listing="$work_dir/assets"
  mkdir "$extracted"
  trap 'rm -rf "$work_dir" "${staging:-}"' EXIT
  extract_bundle "$bundle" "$extracted" "$listing"
  regular_file "$extracted/rust-lob-control-plane-lib.sh" \
    || die 'deployment bundle is missing the runtime contract helper'
  regular_file "$active_deployment/rust-lob-control-plane-lib.sh" \
    || die 'active deployment is missing the runtime contract helper'
  # shellcheck disable=SC1090,SC1091
  . "$active_deployment/rust-lob-control-plane-lib.sh"
  [[ $(monday_rust_lob_runtime_contract_sha256 "$active_deployment") \
      == "$runtime_contract" ]] \
    || die 'active deployment runtime contract drifted from release metadata'
  [[ $(monday_rust_lob_runtime_contract_sha256 "$extracted") \
      == "$runtime_contract" ]] \
    || die 'controller bundle changes the gated runtime contract'

  if [[ -e $CONTROLLER_RELEASE_ROOT || -L $CONTROLLER_RELEASE_ROOT ]]; then
    direct_directory "$CONTROLLER_RELEASE_ROOT" \
      || die 'controller release root is indirect'
  else
    direct_directory "${CONTROLLER_RELEASE_ROOT%/*}" \
      || die 'release root is indirect'
    install -d -m 0755 "$CONTROLLER_RELEASE_ROOT"
  fi
  release_dir="$CONTROLLER_RELEASE_ROOT/$manifest_sha"
  if [[ -e $release_dir || -L $release_dir ]]; then
    verify_published_release "$release_dir" "$extracted" "$manifest" "$listing"
    printf 'controller release already published: %s\n' "$manifest_sha"
    return 0
  fi

  staging=$(mktemp -d "$CONTROLLER_RELEASE_ROOT/.${manifest_sha}.new.XXXXXX")
  mkdir -m 0755 "$staging/deployment"
  while IFS= read -r asset; do
    mode=0444
    [[ $asset == *.sh ]] && mode=0555
    install -m "$mode" "$extracted/$asset" "$staging/deployment/$asset"
  done <"$listing"
  install -m 0444 "$manifest" "$staging/release.json"
  (
    cd "$staging"
    sha256sum release.json >release.json.sha256
    for asset in deployment/*; do sha256sum "$asset"; done \
      | sort -k2 >deployment.sha256
    chmod 0444 release.json.sha256 deployment.sha256
  )
  chmod 0555 "$staging/deployment" "$staging"
  mv "$staging" "$release_dir"
  staging=
  verify_published_release "$release_dir" "$extracted" "$manifest" "$listing"
  printf 'published controller release %s (bundle %s) from %s; production unchanged\n' \
    "$manifest_sha" "$bundle_sha" "$source_revision"
)

main() {
  [[ $# -eq 3 ]] || { usage; exit 2; }
  (( EUID == 0 )) || die 'controller release publication must run as root'
  configure_paths ''
  install -d -m 0755 "$LOCK_ROOT"
  exec 9>"$LOCK_ROOT/monday-rust-lob-release.lock"
  flock -w 30 9 || die 'another Rust collector release operation holds the host lock'
  publish_controller_release '' "$1" "$2" "$3"
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
  main "$@"
fi
