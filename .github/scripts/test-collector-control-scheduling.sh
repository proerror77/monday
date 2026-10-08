#!/usr/bin/env bash
# Independent Linux fixtures verify scheduling, coverage and descendant cleanup.
set -Eeuo pipefail
source_root=$(cd -- "$(dirname -- "$0")/../.." && pwd)
runner="$source_root/.github/scripts/run-collector-control-contracts.sh"
temp=$(mktemp -d)
running=
cleanup() {
  if [[ -n $running ]]; then kill -TERM "$running" 2>/dev/null || true; wait "$running" 2>/dev/null || true; fi
  rm -rf -- "$temp"
}
trap cleanup EXIT
contracts=(polymarket-market-tape-upload-contract polymarket-reference-upload-contract
  monday-collector-health collector-health-unit-release rust-lob-control-plane
  rust-lob-controller-release rust-lob-recovery-queue rust-lob-restore
  polymarket-raw-ops-stage trading-ecs-host-contract binance-fee-release-contract
  binance-usdm-account-release-contract binance-fee-cutover bybit-options-release-contract
  bybit-options-shadow-gate upload-cpu-limits)
fixture() {
  root="$temp/$1"
  mkdir -p "$root/rust_hft" "$root/deployment/aliyun"
  : >"$root/events"
  cat >"$root/stub.sh" <<'STUB'
set -Eeuo pipefail
name=${1##*/}
root=$(cd .. && pwd)
event() { (flock 9; printf '%s %s\n' "$1" "$name" >&9) 9>>"$root/events"; }
event start
case "$name" in
  test-monday-collector-health.sh|test-rust-lob-control-plane.sh)
    if [[ ${HOLD:-} == 1 ]]; then
      bash -c 'trap "" TERM; exec sleep 60' &
      printf '%s\n' "$!" >"$root/$name.pid"
      wait "$!"
    fi
    sleep 0.2 ;;
  test-rust-lob-recovery-queue.sh) sleep 0.2 ;;
esac
event end
if [[ $name == "${FAIL:-}" ]]; then exit 37; fi
STUB
  for name in "${contracts[@]}"; do
    # shellcheck disable=SC2016 # Fixture expands its own script identity.
    printf 'exec bash ../stub.sh "$0"\n' >"$root/deployment/aliyun/test-$name.sh"
  done
}

fixture coverage
bash "$runner" --root "$root" >"$root/output"
printf '%s\n' "${contracts[@]/#/test-}" | sed 's/$/.sh/' | sort >"$root/expected"
awk '$1=="start" {print $2}' "$root/events" | sort >"$root/actual"
cmp "$root/expected" "$root/actual"
awk '
  $1=="start" {
    active++; if (active>peak) peak=active;
    slow=($2=="test-rust-lob-recovery-queue.sh" ||
          $2=="test-rust-lob-control-plane.sh" || $2=="test-monday-collector-health.sh");
    if (slow) {if (ended!=13) exit 1; parallel++}
    else if (active!=1 || parallel) exit 1
  }
  $1=="end" {active--; ended++}
  END {if (peak!=3 || parallel!=3 || active!=0 || ended!=16) exit 1}
' "$root/events"
[[ $(grep -c '^contract_result script=' "$root/output") == 16 ]]
grep -q elapsed_seconds= "$root/output"

fixture fast-failure
if FAIL=test-trading-ecs-host-contract.sh bash "$runner" --root "$root" >"$root/output"; then exit 1; fi
[[ $(grep -c '^start ' "$root/events") == 3 ]]
grep -q 'exit=37' "$root/output"

for failed_script in test-rust-lob-recovery-queue.sh test-rust-lob-control-plane.sh test-monday-collector-health.sh; do
  fixture "slow-failure-$failed_script"
  if FAIL=$failed_script bash "$runner" --root "$root" >"$root/output"; then exit 1; fi
  [[ $(grep -c '^end ' "$root/events") == 16 ]]
  grep -q "contract_result script=$failed_script exit=37 " "$root/output"
  [[ $(grep -c ' exit=0 ' "$root/output") == 15 ]]
done

fixture cancellation
HOLD=1 bash "$runner" --root "$root" >"$root/output" 2>&1 &
running=$!
deadline=$((SECONDS+10))
while [[ $(find "$root" -name '*.pid' | wc -l) -lt 2 && $SECONDS -lt $deadline ]]; do sleep 0.05; done
[[ $(find "$root" -name '*.pid' | wc -l) -eq 2 ]]
sleep 0.1
kill -TERM "$running"
status=0
wait "$running" || status=$?
running=
[[ $status == 143 ]]
for file in "$root"/*.pid; do
  pid=$(cat "$file")
  if [[ -e /proc/$pid/stat ]]; then
    [[ $(sed 's/.*) //' "/proc/$pid/stat" | cut -c1) == Z ]]
  fi
done

fixture fee-admission
mkdir -p "$root/.github/scripts" "$root/.github/workflows"
cp "$source_root/deployment/aliyun/test-binance-fee-cutover.sh" "$root/deployment/aliyun/"
cp "$source_root/.github/workflows/ci.yml" "$root/.github/workflows/"
cp "$runner" "$root/.github/scripts/"
check_fee() { bash "$root/deployment/aliyun/test-binance-fee-cutover.sh" --check-ci-registration; }
check_fee
cp "$root/.github/workflows/ci.yml" "$root/workflow-original"
sed '\|bash ../.github/scripts/run-collector-control-contracts.sh|d' "$root/workflow-original" >"$root/.github/workflows/ci.yml"
if check_fee 2>"$root/error"; then exit 1; fi
grep -q 'workflow must invoke' "$root/error"
cp "$root/workflow-original" "$root/.github/workflows/ci.yml"
sed '/^  test-binance-fee-cutover.sh$/d' "$runner" >"$root/.github/scripts/run-collector-control-contracts.sh"
if check_fee 2>"$root/error"; then exit 1; fi
grep -q 'register the fee cutover contract' "$root/error"
sed 's/^  test-binance-fee-cutover.sh$/  # test-binance-fee-cutover.sh/' "$runner" >"$root/.github/scripts/run-collector-control-contracts.sh"
if check_fee 2>"$root/error"; then exit 1; fi
grep -q 'register the fee cutover contract' "$root/error"
printf 'collector scheduling: coverage, parallel scheduling, failures, cancellation and fee admission passed\n'
