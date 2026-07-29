#!/usr/bin/env bash
# shellcheck disable=SC1090
set -euo pipefail

export LC_ALL=C
SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
readonly SCRIPT_DIR
readonly CUTOVER="$SCRIPT_DIR/polymarket-raw-ops-cutover.sh"

if command -v gsha256sum >/dev/null 2>&1; then
  sha256sum() { command gsha256sum "$@"; }
fi
mv_command=$(command -v gmv || command -v mv)
mv() {
  if [[ ${stage_test_race_destination:-} && ${1:-} == -T && ${2:-} == -n ]]; then
    mkdir "$stage_test_race_destination"
    stage_test_race_destination=
  fi
  command "$mv_command" "$@"
}

for command in awk basename cat chmod cmp cp find grep id jq ln mkdir mktemp mv rm \
  sed sha256sum shellcheck sort stat tar wc; do
  command -v "$command" >/dev/null 2>&1 || {
    printf 'missing raw-ops stage test dependency: %s\n' "$command" >&2
    exit 2
  }
done

shellcheck "$CUTOVER" "$0"
tmp_dir=$(mktemp -d)
trap 'rm -rf -- "$tmp_dir"' EXIT

contract="$tmp_dir/stage-contract.sh"
sed -n \
  -e '/^readonly RELEASE_MANIFEST_SCHEMA=/p' \
  -e '/^readonly -a BUNDLE_ASSETS=(/,/^)/p' \
  -e '/^readonly -a STAGE_ARTIFACT_ASSETS=(/,/^)/p' \
  -e '/^bundle_sha256() {$/,/^}/p' \
  -e '/^verify_release_manifest() {$/,/^}/p' \
  -e '/^verify_release_binding() {$/,/^}/p' \
  -e '/^stage_release() ($/,/^)/p' \
  "$CUTOVER" >"$contract"

test_uid=$(id -u)
stat_uid() { stat -c %u "$1" 2>/dev/null || stat -f %u "$1"; }
stat_mode() { stat -c %a "$1" 2>/dev/null || stat -f %Lp "$1"; }
secure_regular_file() {
  local path=$1 mode
  [[ -f $path && ! -L $path && $(stat_uid "$path") == "$test_uid" ]] || return 1
  mode=$(stat_mode "$path") || return 1
  (( (8#$mode & 022) == 0 ))
}
secure_root_chain() {
  local path=$1 mode
  [[ -d $path && ! -L $path && $(stat_uid "$path") == "$test_uid" ]] || return 1
  mode=$(stat_mode "$path") || return 1
  (( (8#$mode & 022) == 0 ))
}
die() { printf 'stage rejected: %s\n' "$*" >&2; exit 1; }
# shellcheck source=/dev/null
source "$contract"
declare -F stage_release >/dev/null || {
  printf 'cutover command has no atomic raw-ops stage implementation\n' >&2
  exit 1
}

source_revision=1111111111111111111111111111111111111111
artifact="$tmp_dir/artifact"
controls="$tmp_dir/controls"
mkdir -p "$artifact" "$controls"
for asset in "${BUNDLE_ASSETS[@]}"; do
  cp "$SCRIPT_DIR/$asset" "$controls/$asset"
done
cat >"$artifact/polymarket-raw-ops" <<EOF
#!/usr/bin/env bash
printf '%s\n' 'polymarket-raw-ops $source_revision'
EOF
chmod 0755 "$artifact/polymarket-raw-ops"
candidate_sha=$(sha256sum "$artifact/polymarket-raw-ops" | awk '{print $1}')
printf '%s  %s\n' "$candidate_sha" polymarket-raw-ops \
  >"$artifact/polymarket-raw-ops.sha256"
printf '%s\n' "$source_revision" >"$artifact/source-revision.txt"
(
  cd "$controls"
  sha256sum "${BUNDLE_ASSETS[@]}" \
    >"$artifact/polymarket-raw-ops-control-assets.sha256"
  tar -czf "$artifact/polymarket-raw-ops-control.tar.gz" "${BUNDLE_ASSETS[@]}"
)
bundle_sha=$(sha256sum "$artifact/polymarket-raw-ops-control-assets.sha256" \
  | awk '{print $1}')
archive_sha=$(sha256sum "$artifact/polymarket-raw-ops-control.tar.gz" | awk '{print $1}')
printf '%s\n' "$bundle_sha" >"$artifact/deployment-bundle.sha256"
printf '%s  %s\n' "$archive_sha" polymarket-raw-ops-control.tar.gz \
  >"$artifact/polymarket-raw-ops-control.tar.gz.sha256"
jq -S -n --arg source "$source_revision" --arg candidate "$candidate_sha" \
  --arg bundle "$bundle_sha" --arg archive "$archive_sha" \
  '{schema:"monday.polymarket_raw_ops_release.v1",source_revision:$source,
    candidate:{file:"polymarket-raw-ops",sha256:$candidate},
    control_manifest:{file:"polymarket-raw-ops-control-assets.sha256",sha256:$bundle},
    control_archive:{file:"polymarket-raw-ops-control.tar.gz",sha256:$archive}}' \
  >"$artifact/polymarket-raw-ops-release.json"
(
  cd "$artifact"
  sha256sum polymarket-raw-ops-release.json \
    >polymarket-raw-ops-release.json.sha256
)

expect_stage_rejected() {
  local label=$1 rejected_artifact=$2
  if stage_release "$rejected_artifact" "$candidate_root" "$source_revision" \
    >/dev/null 2>&1; then
    printf 'stage accepted %s\n' "$label" >&2
    exit 1
  fi
}

candidate_root="$tmp_dir/candidates"
mkdir "$candidate_root"
staged=$(stage_release "$artifact" "$candidate_root" "$source_revision")
manifest_sha=$(sha256sum "$artifact/polymarket-raw-ops-release.json" | awk '{print $1}')
[[ $staged == "$candidate_root/$manifest_sha" && -d $staged && ! -L $staged ]] || {
  printf 'stage did not publish the manifest-addressed candidate directory\n' >&2
  exit 1
}
expected="$tmp_dir/expected-files"
actual="$tmp_dir/actual-files"
printf '%s\n' "${STAGE_ARTIFACT_ASSETS[@]}" "${BUNDLE_ASSETS[@]}" | sort >"$expected"
find "$staged" -mindepth 1 -maxdepth 1 -exec basename {} \; | sort >"$actual"
cmp -s "$expected" "$actual" || {
  printf 'staged candidate does not contain the exact release payload\n' >&2
  exit 1
}
verify_release_binding "$staged/polymarket-raw-ops-release.json" "$manifest_sha" \
  "$candidate_sha" "$source_revision" "$bundle_sha" "$archive_sha" \
  "$staged/polymarket-raw-ops" "$staged" || {
  printf 'existing Gate verifier rejected the staged release\n' >&2
  exit 1
}

expect_stage_rejected 'an existing immutable destination' "$artifact"

race_root="$tmp_dir/race-candidates"
mkdir "$race_root"
stage_test_race_destination="$race_root/$manifest_sha"
if stage_release "$artifact" "$race_root" "$source_revision" >/dev/null 2>&1; then
  printf 'stage accepted a destination created during publication\n' >&2
  exit 1
fi
stage_test_race_destination=
[[ -d $race_root/$manifest_sha \
  && -z $(find "$race_root/$manifest_sha" -mindepth 1 -maxdepth 1 -print -quit) ]] || {
  printf 'publication race left a usable or nested candidate\n' >&2
  exit 1
}

missing_manifest="$tmp_dir/missing-manifest"
cp -R "$artifact" "$missing_manifest"
rm "$missing_manifest/polymarket-raw-ops-release.json"
expect_stage_rejected 'an artifact without a release manifest' "$missing_manifest"

wrong_candidate="$tmp_dir/wrong-candidate"
cp -R "$artifact" "$wrong_candidate"
printf 'tampered\n' >>"$wrong_candidate/polymarket-raw-ops"
expect_stage_rejected 'a candidate with the wrong SHA-256' "$wrong_candidate"

symbolic="$tmp_dir/symbolic"
cp -R "$artifact" "$symbolic"
rm "$symbolic/source-revision.txt"
ln -s "$artifact/source-revision.txt" "$symbolic/source-revision.txt"
expect_stage_rejected 'a symbolic artifact member' "$symbolic"

insecure_mode="$tmp_dir/insecure-mode"
cp -R "$artifact" "$insecure_mode"
chmod 0666 "$insecure_mode/source-revision.txt"
expect_stage_rejected 'a group/world-writable artifact member' "$insecure_mode"

bad_source="$tmp_dir/bad-source"
cp -R "$artifact" "$bad_source"
wrong_source=2222222222222222222222222222222222222222
printf '%s\n' "$wrong_source" >"$bad_source/source-revision.txt"
jq --arg source "$wrong_source" '.source_revision=$source' \
  "$bad_source/polymarket-raw-ops-release.json" >"$bad_source/release.tmp"
mv "$bad_source/release.tmp" "$bad_source/polymarket-raw-ops-release.json"
(
  cd "$bad_source"
  sha256sum polymarket-raw-ops-release.json >polymarket-raw-ops-release.json.sha256
)
expect_stage_rejected 'a manifest that differs from the trusted source' "$bad_source"

mixed_controls="$tmp_dir/mixed-controls"
mixed_control_dir="$tmp_dir/mixed-control-dir"
cp -R "$artifact" "$mixed_controls"
cp -R "$controls" "$mixed_control_dir"
printf '\n# mixed source control\n' \
  >>"$mixed_control_dir/polymarket-legacy-health-policy.jq"
(
  cd "$mixed_control_dir"
  sha256sum "${BUNDLE_ASSETS[@]}" \
    >"$mixed_controls/polymarket-raw-ops-control-assets.sha256"
  tar -czf "$mixed_controls/polymarket-raw-ops-control.tar.gz" \
    "${BUNDLE_ASSETS[@]}"
)
mixed_bundle_sha=$(sha256sum "$mixed_controls/polymarket-raw-ops-control-assets.sha256" \
  | awk '{print $1}')
mixed_archive_sha=$(sha256sum "$mixed_controls/polymarket-raw-ops-control.tar.gz" \
  | awk '{print $1}')
printf '%s\n' "$mixed_bundle_sha" >"$mixed_controls/deployment-bundle.sha256"
printf '%s  %s\n' "$mixed_archive_sha" polymarket-raw-ops-control.tar.gz \
  >"$mixed_controls/polymarket-raw-ops-control.tar.gz.sha256"
jq --arg bundle "$mixed_bundle_sha" --arg archive "$mixed_archive_sha" \
  '.control_manifest.sha256=$bundle | .control_archive.sha256=$archive' \
  "$mixed_controls/polymarket-raw-ops-release.json" \
  >"$mixed_controls/release.tmp"
mv "$mixed_controls/release.tmp" "$mixed_controls/polymarket-raw-ops-release.json"
(
  cd "$mixed_controls"
  sha256sum polymarket-raw-ops-release.json \
    >polymarket-raw-ops-release.json.sha256
)
expect_stage_rejected 'controls mixed from another release' "$mixed_controls"

unexpected="$tmp_dir/unexpected"
cp -R "$artifact" "$unexpected"
printf 'unexpected\n' >"$controls/unexpected-control"
(
  cd "$controls"
  tar -czf "$unexpected/polymarket-raw-ops-control.tar.gz" \
    "${BUNDLE_ASSETS[@]}" unexpected-control
)
unexpected_archive_sha=$(sha256sum "$unexpected/polymarket-raw-ops-control.tar.gz" \
  | awk '{print $1}')
printf '%s  %s\n' "$unexpected_archive_sha" polymarket-raw-ops-control.tar.gz \
  >"$unexpected/polymarket-raw-ops-control.tar.gz.sha256"
jq --arg sha "$unexpected_archive_sha" '.control_archive.sha256=$sha' \
  "$unexpected/polymarket-raw-ops-release.json" >"$unexpected/release.tmp"
mv "$unexpected/release.tmp" "$unexpected/polymarket-raw-ops-release.json"
(
  cd "$unexpected"
  sha256sum polymarket-raw-ops-release.json >polymarket-raw-ops-release.json.sha256
)
expect_stage_rejected 'an unexpected control archive entry' "$unexpected"
if find "$candidate_root" -mindepth 1 -maxdepth 1 -name '.*.new.*' | grep -q .; then
  printf 'failed staging left a partial candidate directory\n' >&2
  exit 1
fi

printf 'Polymarket raw-ops atomic stage tests passed\n'
