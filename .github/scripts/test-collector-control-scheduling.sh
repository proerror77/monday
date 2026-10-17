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
    if ($2=="test-rust-lob-recovery-queue.sh") {if (ended!=13 || active!=1) exit 1; recovery=1}
    else if (recovery) exit 1
  }
  $1=="end" {active--; ended++; if ($2=="test-rust-lob-recovery-queue.sh") recovery=0}
  END {if (peak!=2 || active!=0 || ended!=16) exit 1}
' "$root/events"
[[ $(grep -c '^contract_result script=' "$root/output") == 16 ]]
grep -q elapsed_seconds= "$root/output"

fixture fast-failure
if FAIL=test-trading-ecs-host-contract.sh bash "$runner" --root "$root" >"$root/output"; then exit 1; fi
[[ $(grep -c '^start ' "$root/events") == 3 ]]
grep -q 'exit=37' "$root/output"

fixture slow-failure
if FAIL=test-rust-lob-recovery-queue.sh bash "$runner" --root "$root" >"$root/output"; then exit 1; fi
[[ $(grep -c '^end ' "$root/events") == 16 ]]
grep -q 'exit=37' "$root/output"

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

# Preserve the full source-aware input union and retain a failing static gate.
lint_runner="$source_root/.github/scripts/check-collector-shells.sh"
fixture shell-lint
mkdir -p "$root/bin" "$root/.github/scripts"
lint_groups="$root/.github/scripts/collector-shellcheck-groups.json"
cp "$source_root/.github/scripts/collector-shellcheck-groups.json" "$lint_groups"
while IFS= read -r -d '' file; do
  printf '#!/usr/bin/env bash\n:\n' >"$root/rust_hft/$file"
done < <(jq -j '.[][] | . + "\u0000"' "$lint_groups" | sort -zu)
printf '#!/usr/bin/env bash\n:\n' >"$root/deployment/aliyun/space name.sh"
jq '.[-1] += ["../deployment/aliyun/space name.sh"]' "$lint_groups" >"$root/groups.new"
mv "$root/groups.new" "$lint_groups"
cp "$lint_groups" "$root/groups.saved"
cat >"$root/bin/shellcheck" <<'LINT'
#!/usr/bin/env bash
set -euo pipefail
[[ $1 == -- && $# -gt 1 ]]
shift
health=false companion=false
for file in "$@"; do
  [[ $file != ../deployment/aliyun/test-monday-collector-health.sh ]] || health=true
  [[ $file != ../deployment/aliyun/test-recovery-retention-fixture.sh ]] || companion=true
done
if [[ $health == true && $companion != true ]]; then
  echo 'missing retention source companion' >&2; exit 38
fi
exec 9>>"$LINT_ROOT/lint.lock"
flock 9
active=$(cat "$LINT_ROOT/lint.active")
active=$((active + 1))
printf '%s\n' "$active" >"$LINT_ROOT/lint.active"
peak=$(cat "$LINT_ROOT/lint.peak")
if (( active > peak )); then printf '%s\n' "$active" >"$LINT_ROOT/lint.peak"; fi
printf '%s\0' "$@" >>"$LINT_ROOT/lint.calls"
printf '%s\n' "$BASHPID" >>"$LINT_ROOT/lint.workers"
flock -u 9
sleep 0.1
flock 9
active=$(cat "$LINT_ROOT/lint.active")
printf '%s\n' "$((active - 1))" >"$LINT_ROOT/lint.active"
flock -u 9
for file in "$@"; do
  if [[ ${file##*/} == ${LINT_FAIL:-} ]]; then exit 37; fi
done
LINT
chmod +x "$root/bin/shellcheck"
reset_lint() {
  : >"$root/lint.calls"; : >"$root/lint.workers"
  printf '0\n' >"$root/lint.active"
  printf '0\n' >"$root/lint.peak"
}
run_lint() {
  LINT_ROOT="$root" LINT_FAIL="${1:-}" PATH="$root/bin:$PATH" \
    bash "$lint_runner" --root "$root"
}
check_lint_coverage() {
  (cd "$root/rust_hft" || exit; printf '%s\0' ../deployment/aliyun/*.sh) | sort -z >"$root/lint.expected"
  sort -zu "$root/lint.calls" >"$root/lint.actual"
  cmp "$root/lint.expected" "$root/lint.actual"
  [[ $(cat "$root/lint.active") == 0 ]]
  [[ $(wc -l <"$root/lint.workers") -eq $(jq length "$lint_groups") ]]
}
reset_lint
run_lint >"$root/lint.output"
check_lint_coverage
peak=$(cat "$root/lint.peak")
(( peak > 1 && peak <= 4 ))
reset_lint
if run_lint test-trading-ecs-host-contract.sh >"$root/lint.output" 2>&1; then
  echo 'parallel lint failure did not fail the static gate' >&2; exit 1
fi
check_lint_coverage
reset_lint
jq '.[0] -= ["../deployment/aliyun/test-recovery-retention-fixture.sh"]' "$lint_groups" >"$root/groups.new"
mv "$root/groups.new" "$lint_groups"
if run_lint >"$root/lint.output" 2>&1; then
  echo 'split source companions passed the static gate' >&2; exit 1
fi
grep -q 'missing retention source companion' "$root/lint.output"
cp "$root/groups.saved" "$lint_groups"
reset_lint
printf '#!/usr/bin/env bash\n:\n' >"$root/deployment/aliyun/unregistered.sh"
if run_lint >"$root/lint.output" 2>&1; then
  echo 'unregistered collector script passed the static gate' >&2; exit 1
fi
grep -q 'exact collector script set' "$root/lint.output"
[[ ! -s $root/lint.calls ]]
rm "$root/deployment/aliyun/unregistered.sh"
reset_lint
printf 'if\n' >"$root/deployment/aliyun/test-trading-ecs-host-contract.sh"
if run_lint >"$root/lint.output" 2>&1; then
  echo 'invalid shell syntax passed the static gate' >&2; exit 1
fi
[[ ! -s $root/lint.calls ]]
printf 'collector scheduling: coverage, isolation, failures, cancellation, fee admission and source-aware lint passed\n'
