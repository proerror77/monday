#!/usr/bin/env bash
set -euo pipefail
script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT

# Invalid modes/paths must fail before any controller or writer dispatch.
for action in isolate drain resume retain; do
  code=0
  bash "$script_dir/collector-monitor-retained.sh" "$action" spot >"$fixture/output" 2>&1 || code=$?
  [[ $code == 2 ]]
done
code=0
sh "$script_dir/monday-collector-health.sh" --json --monitor-release >"$fixture/output" 2>&1 || code=$?
[[ $code == 2 ]] # An arbitrary checkout cannot masquerade as a monitor release.

# Exercise the manifest-to-file link using the actual monitor verification block.
sed -n '/^  expected_monitor_checks=/,/^  RETENTION_READER=/p' \
  "$script_dir/monday-collector-health.sh" >"$fixture/verify.sh"
[[ -s $fixture/verify.sh ]]
mkdir "$fixture/assets"
printf 'approved monitor bytes\n' >"$fixture/assets/collector-monitor-retained.sh"
sha=$(sha256sum "$fixture/assets/collector-monitor-retained.sh" | awk '{print $1}')
jq -cn --arg sha "$sha" '{assets:{"collector-monitor-retained.sh":$sha}}' >"$fixture/assets/release.json"
printf '%s  collector-monitor-retained.sh\n' "$sha" >"$fixture/assets/assets.sha256"
monitor_root="$fixture/assets" sh -e "$fixture/verify.sh"
printf 'tampered monitor bytes\n' >"$fixture/assets/collector-monitor-retained.sh"
# Updating the checksum file alone does not authorize different manifest bytes.
sha256sum "$fixture/assets/collector-monitor-retained.sh" | sed "s|$fixture/assets/||" >"$fixture/assets/assets.sha256"
if monitor_root="$fixture/assets" sh -e "$fixture/verify.sh" >"$fixture/output" 2>&1; then
  echo 'manifest mismatch was accepted' >&2; exit 1
fi
# A publisher's workstation UID must not become the owner of host assets.
mkdir "$fixture/extract"
printf 'monitor payload\n' >"$fixture/payload"
COPYFILE_DISABLE=1 tar --format=ustar -C "$fixture" -cf "$fixture/package.tar" payload
tar --no-same-owner -xf "$fixture/package.tar" -C "$fixture/extract"
cmp "$fixture/payload" "$fixture/extract/payload"
[[ $(stat -c %u "$fixture/extract/payload") == "$(id -u)" ]]
printf 'Monitor write-mode refusal, immutable-path refusal and manifest checksum binding passed\n'
