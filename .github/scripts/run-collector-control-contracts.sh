#!/usr/bin/env bash
# Linux CI: fast contracts first, isolated recovery, then two independent suites.
set -Eeuo pipefail

FAST=(
  test-polymarket-market-tape-upload-contract.sh
  test-polymarket-reference-upload-contract.sh
  test-trading-ecs-host-contract.sh
  test-polymarket-raw-ops-stage.sh
  test-collector-health-unit-release.sh
  test-rust-lob-controller-release.sh
  test-rust-lob-restore.sh
  test-binance-fee-release-contract.sh
  test-binance-usdm-account-release-contract.sh
  test-binance-fee-cutover.sh
  test-bybit-options-release-contract.sh
  test-bybit-options-shadow-gate.sh
)
ISOLATED=(test-rust-lob-recovery-queue.sh)
SLOW=(test-rust-lob-control-plane.sh test-monday-collector-health.sh)

root=$(cd -- "$(dirname -- "$0")/../.." && pwd)
if [[ ${1:-} == --root && $# == 2 ]]; then
  root=$(cd -- "$2" && pwd)
elif (( $# != 0 )); then
  printf 'usage: %s [--root directory]\n' "$0" >&2
  exit 2
fi
command -v setsid >/dev/null
logs=$(mktemp -d)
began=$SECONDS
active=()
failed=0

# shellcheck disable=SC2329 # Invoked by the EXIT trap, including signal exits.
cleanup() {
  local status=$? pid
  trap '' INT TERM
  for pid in "${active[@]}"; do kill -TERM -- "-$pid" 2>/dev/null || true; done
  if (( ${#active[@]} )); then
    sleep 2
    for pid in "${active[@]}"; do
      kill -KILL -- "-$pid" 2>/dev/null || true
      wait "$pid" 2>/dev/null || true
    done
  fi
  rm -rf -- "$logs"
  printf 'contract_suite elapsed_seconds=%s\n' "$((SECONDS - began))"
  exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

start() {
  local name=$1
  setsid bash "$root/deployment/aliyun/$name" >"$logs/$name" 2>&1 &
  started_pid=$!
  active+=("$started_pid")
  printf 'contract_start script=%s\n' "$name"
}
finish() {
  local pid=$1 name=$2 began_at=$3 status=0 item remaining=()
  wait "$pid" || status=$?
  for item in "${active[@]}"; do
    [[ $item == "$pid" ]] || remaining+=("$item")
  done
  active=("${remaining[@]}")
  printf '::group::%s\n' "$name"
  cat -- "$logs/$name"
  printf '::endgroup::\ncontract_result script=%s exit=%s elapsed_seconds=%s\n' \
    "$name" "$status" "$((SECONDS - began_at))"
  if (( status != 0 )); then failed=1; fi
}

cd -- "$root/rust_hft"
for name in "${FAST[@]}"; do
  began_at=$SECONDS
  start "$name"
  finish "$started_pid" "$name" "$began_at"
  if (( failed )); then exit 1; fi
done
printf 'contract_phase name=recovery_isolated\n'
for name in "${ISOLATED[@]}"; do
  began_at=$SECONDS
  start "$name"
  finish "$started_pid" "$name" "$began_at"
done
printf 'contract_phase name=parallel_health_control\n'
pids=() starts=()
for name in "${SLOW[@]}"; do
  starts+=("$SECONDS")
  start "$name"
  pids+=("$started_pid")
done
for i in "${!SLOW[@]}"; do finish "${pids[i]}" "${SLOW[i]}" "${starts[i]}"; done
exit "$failed"
