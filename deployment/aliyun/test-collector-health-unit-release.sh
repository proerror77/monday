#!/usr/bin/env bash
# Linux filesystem/rollback regression; no real services are changed.
set -Eeuo pipefail
script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
fixture=$(mktemp -d)
trap 'rm -rf -- "$fixture"' EXIT
# shellcheck source-path=SCRIPTDIR
# shellcheck source=host-collector-health-unit-release.sh
source "$script_dir/host-collector-health-unit-release.sh"
# Cross-layer integration: extending one timeout cannot leave an outer caller
# shorter than the full retained-index scan and its termination grace.
# shellcheck disable=SC2016 # Match literal shell source, not this test's variables.
reader_budget=$(sed -n 's/.*RETENTION_DEADLINE=$((SECONDS + \([0-9]*\))).*/\1/p' "$script_dir/host-rust-lob-recovery-queue.sh" | tail -1)
# shellcheck disable=SC2016
child_budget=$(sed -n 's/.*retained_output=$(timeout .*--kill-after=2s \([0-9]*\).*/\1/p' "$script_dir/monday-collector-health.sh")
unit_budget=$(awk -F= '$1=="TimeoutStartSec" {print $2}' "$script_dir/monday-collector-health.service")
workflow="$script_dir/../../.github/workflows/monitor-collector-host.yml"
cloud_budget=$(sed -n 's/.*--Timeout \([0-9]*\).*/\1/p' "$workflow")
polls=$(sed -n 's/.*seq 1 \([0-9]*\).*/\1/p' "$workflow")
[[ $reader_budget =~ ^[0-9]+$ && $child_budget =~ ^[0-9]+$ && $unit_budget =~ ^[0-9]+$
  && $cloud_budget =~ ^[0-9]+$ && $polls =~ ^[0-9]+$ ]]
(( child_budget > reader_budget + 2 && unit_budget > child_budget + 2
  && cloud_budget >= unit_budget && polls * 5 > cloud_budget ))
# Fixture UID boundary; production uses the real stat ownership checks.
stat() {
  if [[ ${1:-} == -c && ${2:-} == %u ]]; then printf '0\n'; else command stat "$@"; fi
}
install() {
  # Exercise the real install/rename/mode path as an unprivileged CI user.
  if [[ ${1:-} == -o && ${2:-} == root && ${3:-} == -g && ${4:-} == root ]]; then shift 4; fi
  command install "$@"
}
mkdir -p "$fixture/units" "$fixture/evidence"
target="$fixture/units/monday-collector-health.service"
candidate="$fixture/candidate.service"
cp "$script_dir/monday-collector-health.service" "$candidate"
printf '[Service]\nTimeoutStartSec=120\n' >"$target"
old_sha=$(health_unit_sha "$target"); new_sha=$(health_unit_sha "$candidate")
revision=$(printf '%040d' 1)
systemctl() {
  printf '%s\n' "$*" >>"$fixture/calls"
  case "$*" in
    'show monday-collector-health.service -p FragmentPath --value') printf '%s\n' "$target" ;;
    'show monday-collector-health.service -p DropInPaths --value') : ;;
    'show monday-collector-health.service -p TimeoutStartUSec --value')
      if [[ ${force_bad_readback:-false} == true ]]; then printf '2min\n'; else printf '8min\n'; fi ;;
    'show monday-collector-health.timer -p ActiveState --value') printf 'active\n' ;;
    'show monday-collector-health.service -p ActiveState --value') printf 'failed\n' ;;
    'stop monday-collector-health.timer monday-collector-health.service'|'daemon-reload'|'start monday-collector-health.timer') : ;;
    *) printf 'unexpected service operation: %s\n' "$*" >&2; return 1 ;;
  esac
}
(deploy_health_unit "$candidate" "$new_sha" "$old_sha" "$revision" "$target" "$fixture/evidence/success") >"$fixture/success.json"
[[ $(health_unit_sha "$target") == "$new_sha" ]]
jq -e '.result=="success" and .effective_timeout=="8min" and .timer_state=="active"' "$fixture/success.json" >/dev/null
[[ $(health_unit_sha "$fixture/evidence/success/before.service") == "$old_sha" ]]

# A failed effective-config readback restores the actual old bytes and timer.
cp "$fixture/evidence/success/before.service" "$target"
force_bad_readback=true
set +e
(set -e; deploy_health_unit "$candidate" "$new_sha" "$old_sha" "$revision" "$target" "$fixture/evidence/failure")
status=$?
set -e
[[ $status != 0 && $(health_unit_sha "$target") == "$old_sha" ]]
jq -e '.result=="failed" and .rollback=="passed"' "$fixture/evidence/failure/result.json" >/dev/null
force_bad_readback=false

# Wrong preimage refuses the write before any timer/service operation.
cp "$fixture/calls" "$fixture/calls.before"
if (deploy_health_unit "$candidate" "$new_sha" "$(printf '%064d' 1)" "$revision" "$target" "$fixture/evidence/wrong"); then exit 1; fi
cmp "$fixture/calls" "$fixture/calls.before"
[[ ! -e $fixture/evidence/wrong && $(health_unit_sha "$target") == "$old_sha" ]]
printf 'Pinned health-unit install, effective timeout, rollback and preimage refusal passed\n'
