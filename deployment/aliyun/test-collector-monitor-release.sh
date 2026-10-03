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
# Adding five USD-M jobs to 23 Spot jobs must not exhaust Cloud Assistant's
# stdout buffer. Validate full custody first, then report every job/receipt
# without repeating the four other digests from its immutable receipt.
sed -n '/^retention_health_jobs()/,/^}/p' "$script_dir/monday-collector-health.sh" >"$fixture/projection.sh"
# shellcheck disable=SC1091 # Function extracted from the actual checked script.
. "$fixture/projection.sh"
jq -cn '{retained:[range(28)|{job_id:("20260925T000000Z-usdm-db78a31857cb-"+tostring),
  queue_state:(if .%2==0 then "failed" else "stale" end),old_payload_sha256:("a"*64),
  result_sha256:("b"*64),request_sha256:("c"*64),inventory_sha256:("d"*64),receipt_sha256:("e"*64),
  retained_bytes:1000000000,disposition:"retained_unrecovered",data_recovered:false,
  delivery_verified:false,replay_eligibility:"not_assessed"}]}' >"$fixture/full.json"
retention_health_jobs <"$fixture/full.json" >"$fixture/compact.json"
jq -e --slurpfile full "$fixture/full.json" 'length==28 and map(.job_id)==($full[0].retained|map(.job_id))
  and map(.receipt_sha256)==($full[0].retained|map(.receipt_sha256))
  and map(.queue_state)==($full[0].retained|map(.queue_state))
  and all(.[]; .data_recovered==false and .delivery_verified==false and .replay_eligibility=="not_assessed")' \
  "$fixture/compact.json" >/dev/null
[[ $(wc -c <"$fixture/compact.json") -lt 12288 ]]
printf 'Monitor write-mode refusal, immutable-path refusal and manifest checksum binding passed\n'
