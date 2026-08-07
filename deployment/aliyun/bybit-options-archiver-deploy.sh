#!/usr/bin/env bash
set -euo pipefail

# Governed Bybit Options archiver release staging lane.
#
# This script stages an immutable, digest-addressed release on the host:
#   - the collector binary
#   - the deployment bundle (unit templates, timer, policies, control-plane lib)
#   - release.json binding artifact_sha256, deployment_bundle_sha256, and
#     deployment_source_revision
#
# It points the shadow symlink at the candidate and verifies the staged bundle's
# rendered unit templates carry the fail-closed disk/spool env.  It does NOT touch systemd:
# a candidate must pass host-bybit-options-shadow-gate.sh and
# then be promoted by host-bybit-options-cutover.sh, which is the only writer of
# /etc/systemd/system/bybit-options-*.service.  This guarantees a running
# production unit can never be silently repointed at an ungated candidate.

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd -P)
readonly SCRIPT_DIR
readonly RELEASE_ROOT=/opt/monday/releases/bybit-options-archiver
readonly STATE_FILE=/opt/monday/state/bybit-options-archiver-deploy.json
readonly UNIT=bybit-options-archiver.service
readonly UPLOAD_UNIT=bybit-options-upload.service
readonly TIMER=bybit-options-upload.timer
readonly SHADOW_LINK=/opt/monday/bin/bybit-options-archiver-shadow
readonly REQUIRED_ENV_KEYS=(MIN_FREE_GB BYBIT_OPTIONS_SPOOL_MAX_BYTES)
readonly RELEASE_BUNDLE_PREFIX=/opt/monday/releases/bybit-options-archiver/
readonly BINARY_PATH_SUFFIX=/bybit-options-archiver
readonly DEPLOYMENT_ASSETS=(
  bybit-options-archiver.service
  bybit-options-upload.service
  bybit-options-upload.timer
  bybit-options-runtime-health-policy.jq
  bybit-options-shadow-gate-policy.jq
  bybit-options-control-plane-lib.sh
)

die() { printf 'Bybit Options deploy: %s\n' "$*" >&2; exit 1; }
usage() {
  printf '%s\n' \
    'Usage:' \
    '  bybit-options-archiver-deploy.sh install <artifact-dir> <source-revision>' \
    '  bybit-options-archiver-deploy.sh rollback <release-sha256>' \
    '  bybit-options-archiver-deploy.sh verify <release-sha256>'
  exit 2
}

atomic_symlink() {
  local target=$1 link=$2 temporary
  temporary="${link}.new.$$"
  rm -f "$temporary" || return 1
  ln -s "$target" "$temporary" || return 1
  mv -Tf "$temporary" "$link" || return 1
}

bundle_sha256() {
  local bundle=$1
  sha256sum "$bundle/DEPLOYMENT_BUNDLE.sha256" | awk '{print $1}'
}

# Render a unit template to a temp file and assert the governed configuration.
# The production ExecStart must remain the digest-addressed release path so the
# unit can never be hand-pointed at an arbitrary binary.
validate_rendered_unit() {
  local template=$1 expected_binary=$2 expect_upload_flag=$3 tmp
  tmp=$(mktemp)
  sed "s|${RELEASE_BUNDLE_PREFIX}@BYBIT_OPTIONS_ARCHIVER_SHA256@${BINARY_PATH_SUFFIX}|$expected_binary|g" \
    "$template" >"$tmp"
  local key
  for key in "${REQUIRED_ENV_KEYS[@]}"; do
    grep -Fq "Environment=$key=" "$tmp" \
      || die "$(basename "$template") is missing the governed $key environment"
  done
  if [[ $expect_upload_flag == true ]]; then
    grep -Fq -- '--upload-only' "$tmp" \
      || die "$(basename "$template") is not explicitly upload-only"
  else
    grep -Fq "ExecStart=$expected_binary" "$tmp" \
      || die "$(basename "$template") ExecStart is not the digest-addressed release binary"
  fi
  rm -f "$tmp"
}

verify_bundle() {
  local sha=$1
  local bundle="$RELEASE_ROOT/$sha/deployment"
  [[ -d $bundle && ! -L $bundle ]] || die "deployment bundle missing: $bundle"
  ( cd "$bundle" && sha256sum --check --strict DEPLOYMENT_BUNDLE.sha256 ) \
    || die 'deployment bundle failed its own digest check'
  local release_json="$RELEASE_ROOT/$sha/release.json"
  jq -e \
    --arg sha "$sha" \
    --arg bundle "$(bundle_sha256 "$bundle")" \
    '.artifact_sha256 == $sha and .deployment_bundle_sha256 == $bundle' \
    "$release_json" >/dev/null \
    || die 'release.json does not match the staged bundle identity'
  validate_rendered_unit "$bundle/$UNIT" "$RELEASE_ROOT/$sha/bybit-options-archiver" false
  validate_rendered_unit "$bundle/$UPLOAD_UNIT" "$RELEASE_ROOT/$sha/bybit-options-archiver" true
  grep -Fxq 'Unit=bybit-options-upload.service' "$bundle/$TIMER" \
    || die 'upload timer targets the wrong unit'
}

verify_release() {
  local sha=$1
  local source=$2
  local binary="$RELEASE_ROOT/$sha/bybit-options-archiver"
  [[ $sha =~ ^[a-f0-9]{64}$ && $source =~ ^[a-f0-9]{40}$ ]] || die 'invalid release identity'
  [[ -x $binary && ! -L $binary ]] || die "release binary missing: $binary"
  [[ $(sha256sum "$binary" | awk '{print $1}') == "$sha" ]] || die 'release binary digest drifted'
  [[ $(<"$RELEASE_ROOT/$sha/source-revision.txt") == "$source" ]] || die 'source revision drifted'
  jq -e --arg sha "$sha" --arg source "$source" \
    '.artifact_sha256 == $sha and .deployment_source_revision == $source' \
    "$RELEASE_ROOT/$sha/release.json" >/dev/null \
    || die 'release.json identity drifted'
  verify_bundle "$sha"
  "$binary" --version | grep -Fqx "bybit-options-archiver $source" \
    || die 'binary version does not bind source revision'
}

verify_staging() {
  local sha=$1
  local binary="$RELEASE_ROOT/$sha/bybit-options-archiver"
  [[ -L $SHADOW_LINK && $(readlink -f "$SHADOW_LINK") == "$binary" ]] \
    || die 'shadow symlink does not point at the staged release'
  printf '%s  %s\n' "$sha" "$SHADOW_LINK" | sha256sum --check --strict >/dev/null
}

install_release() {
  local artifact=$1 source=$2
  [[ -d $artifact && $source =~ ^[a-f0-9]{40}$ ]] || die 'invalid artifact or source revision'
  local candidate="$artifact/bybit-options-archiver"
  [[ -f $candidate && ! -L $candidate ]] || die 'artifact binary missing'
  "$candidate" --self-test >/dev/null || die 'artifact self-test failed'
  local sha; sha=$(sha256sum "$candidate" | awk '{print $1}')
  install -d -o root -g root -m 0755 "$RELEASE_ROOT"
  local staging; staging=$(mktemp -d "$RELEASE_ROOT/.${sha}.new.XXXXXX")
  trap 'rm -rf -- "$staging"' RETURN
  install -m 0555 "$candidate" "$staging/bybit-options-archiver"
  printf '%s  bybit-options-archiver\n' "$sha" >"$staging/bybit-options-archiver.sha256"
  printf '%s\n' "$source" >"$staging/source-revision.txt"
  chmod 0444 "$staging/bybit-options-archiver.sha256" "$staging/source-revision.txt"

  install -d -o root -g root -m 0555 "$staging/deployment"
  local asset
  for asset in "${DEPLOYMENT_ASSETS[@]}"; do
    [[ -f $SCRIPT_DIR/$asset ]] || die "missing deployment asset: $asset"
    install -m 0444 "$SCRIPT_DIR/$asset" "$staging/deployment/$asset"
  done
  ( cd "$staging/deployment" && sha256sum "${DEPLOYMENT_ASSETS[@]}" ) \
    >"$staging/deployment/DEPLOYMENT_BUNDLE.sha256"
  chmod 0444 "$staging/deployment/DEPLOYMENT_BUNDLE.sha256"
  local bundle_sha; bundle_sha=$(bundle_sha256 "$staging/deployment")
  jq -n \
    --arg schema monday.bybit_options_archiver_release.v1 \
    --arg artifact_sha256 "$sha" \
    --arg deployment_bundle_sha256 "$bundle_sha" \
    --arg deployment_source_revision "$source" \
    '{schema:$schema,artifact_sha256:$artifact_sha256,
      deployment_bundle_sha256:$deployment_bundle_sha256,
      deployment_source_revision:$deployment_source_revision}' \
    >"$staging/release.json"
  chmod 0444 "$staging/release.json"
  chmod 0555 "$staging"
  [[ ! -e "$RELEASE_ROOT/$sha" ]] || die 'release already exists'
  mv -- "$staging" "$RELEASE_ROOT/$sha"
  trap - RETURN
  verify_release "$sha" "$source"

  install -d -m 0755 /opt/monday/bin
  atomic_symlink "$RELEASE_ROOT/$sha/bybit-options-archiver" "$SHADOW_LINK"
  mkdir -p "${STATE_FILE%/*}"
  jq -n --arg sha "$sha" --arg source "$source" \
    '{schema:"monday.bybit_options_archiver_deploy.v1",staged:{sha256:$sha,source_revision:$source},production_active:false}' \
    >"$STATE_FILE"
  chmod 0444 "$STATE_FILE"
  verify_staging "$sha"
}

rollback_release() {
  local sha=$1 source
  source=$(<"$RELEASE_ROOT/$sha/source-revision.txt")
  verify_release "$sha" "$source"
  atomic_symlink "$RELEASE_ROOT/$sha/bybit-options-archiver" "$SHADOW_LINK"
  verify_staging "$sha"
}

command=${1:-}
case "$command" in
  install) [[ $# == 3 ]] || usage; install_release "$2" "$3" ;;
  rollback) [[ $# == 2 ]] || usage; rollback_release "$2" ;;
  verify) [[ $# == 2 ]] || usage; source=$(<"$RELEASE_ROOT/$2/source-revision.txt"); verify_release "$2" "$source"; verify_staging "$2" ;;
  *) usage ;;
esac
