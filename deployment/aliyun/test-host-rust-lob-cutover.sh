#!/usr/bin/env bash
set -Eeuo pipefail
export LC_ALL=C

SCRIPT_DIR=$(cd -- "$(dirname -- "$0")" && pwd)
CUTOVER="$SCRIPT_DIR/host-rust-lob-cutover.sh"
fixture=$(readlink -f -- "$(mktemp -d)")
trap 'rm -rf -- "$fixture"' EXIT
fail() { printf 'cutover preflight test failed: %s\n' "$*" >&2; exit 1; }

# Reject every host command. These fixtures never reach a real service manager.
mkdir -p "$fixture/bin"
export FIXTURE_HOST_CALLS="$fixture/host.calls"
for command in systemctl ssh kubectl curl aliyun; do
  cat >"$fixture/bin/$command" <<'GUARD'
#!/usr/bin/env bash
printf '%s %s\n' "${0##*/}" "$*" >>"$FIXTURE_HOST_CALLS"
exit 97
GUARD
  chmod +x "$fixture/bin/$command"
done
export FIXTURE_NOW_NS=1700000000000000000
cat >"$fixture/bin/date" <<'CLOCK'
#!/usr/bin/env bash
[[ $# == 1 && $1 == +%s%N ]] || exit 98
printf '%s\n' "$FIXTURE_NOW_NS"
CLOCK
chmod +x "$fixture/bin/date"
real_find=$(command -v find)
export FIXTURE_REAL_FIND="$real_find"
cat >"$fixture/bin/find" <<'FIND'
#!/usr/bin/env bash
[[ ${FIXTURE_FAIL_FIND:-0} != 1 ]] || exit 73
exec "$FIXTURE_REAL_FIND" "$@"
FIND
chmod +x "$fixture/bin/find"
export PATH="$fixture/bin:$PATH"

# Source identity helpers only; do not source or run the cutover's mutation phase.
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"

target=$(printf 'a%.0s' {1..64})
gate_sha=$(printf 'b%.0s' {1..64})
cases=0
new_fixture() {
  ROOT="$fixture/root"
  rm -rf -- "$ROOT"
  mkdir -p "$ROOT/data/monday/spool/binance-lob/spot" \
    "$ROOT/data/monday/spool/binance-lob/usdm" \
    "$ROOT/opt/monday/releases/binance-lob-controller" "$ROOT/opt/monday/bin"
  ln -s before-controller "$ROOT/opt/monday/releases/binance-lob-controller/active"
  ln -s before-payload "$ROOT/opt/monday/bin/binance-lob-archiver"
  for market in spot usdm; do
    write_health "$market" "$((FIXTURE_NOW_NS - 30000000000))"
  done
  rm -f -- "$FIXTURE_HOST_CALLS"
}
write_health() {
  printf '{"updated_at_ns":%s}\n' "$2" \
    >"$ROOT/data/monday/spool/binance-lob/$1/health.json"
}
assert_untouched() {
  [[ ! -s $FIXTURE_HOST_CALLS ]] || fail 'a host command was attempted'
  [[ ! -s $ROOT/run/cutover-fixture.calls ]] || fail 'a fixture unit was touched'
  [[ $(readlink "$ROOT/opt/monday/releases/binance-lob-controller/active") == before-controller ]] \
    || fail 'active controller changed'
  [[ $(readlink "$ROOT/opt/monday/bin/binance-lob-archiver") == before-payload ]] \
    || fail 'production projection changed'
}
run_fixture() {
  local expected=$1 status log="$fixture/result.log"
  # No candidate release exists. A healthy preflight stops at target validation.
  # This executes the real entry point without reaching containment or cutover.
  if MONDAY_CONTROL_PLANE_TEST=1 MONDAY_CONTROL_PLANE_FIXTURE_SENTINEL=monday-v2-fixture \
    MONDAY_CUTOVER_FIXTURE_PREFLIGHT=1 MONDAY_CUTOVER_FIXTURE_SYSTEMD=1 \
    MONDAY_CUTOVER_FIXTURE_LEGACY_ACTIVE=1 MONDAY_ROOT="$ROOT" \
    bash "$CUTOVER" --from direct --to "$target" \
      --gate-receipt "$fixture/gate.json" --gate-sha256 "$gate_sha" >"$log" 2>&1; then
    fail 'fixture unexpectedly reached cutover completion'
  else
    status=$?
  fi
  [[ $status == 1 ]] || { cat "$log" >&2; fail "unexpected exit status $status"; }
  assert_untouched
  if [[ $expected == pass ]]; then
    grep -Fq 'target controller failed verification' "$log" \
      || { cat "$log" >&2; fail 'healthy preflight did not reach target validation'; }
    ! grep -Fq 'preflight refused' "$log" || fail 'healthy preflight was refused'
  else
    grep -Fq 'preflight refused; no units were stopped or masked' "$log" \
      || { cat "$log" >&2; fail 'fixture did not refuse in preflight'; }
    grep -Fq -- "$expected" "$log" || { cat "$log" >&2; fail "missing offending path: $expected"; }
  fi
  cases=$((cases + 1))
}

# The entry point must check before a rollback trap or containment can touch units.
preflight_line=$(grep -n -m1 '^  cutover_preflight ||' "$CUTOVER" | cut -d: -f1)
containment_line=$(grep -n -m1 '^  writer_containment_started=1$' "$CUTOVER" | cut -d: -f1)
cleanup_line=$(grep -n -m1 '^trap cleanup EXIT$' "$CUTOVER" | cut -d: -f1)
[[ $preflight_line -lt $containment_line && $preflight_line -lt $cleanup_line ]] \
  || fail 'preflight runs after containment or its rollback trap'

new_fixture
run_fixture pass
for market in spot usdm; do
  new_fixture
  write_health "$market" "$((FIXTURE_NOW_NS - 120000000000))"
  run_fixture pass

  for health_case in stale future missing malformed missing_timestamp string_timestamp fractional_timestamp; do
    new_fixture
    health="$ROOT/data/monday/spool/binance-lob/$market/health.json"
    case $health_case in
      stale) write_health "$market" "$((FIXTURE_NOW_NS - 121000000000))" ;;
      future) write_health "$market" "$((FIXTURE_NOW_NS + 1000000000))" ;;
      missing) rm -- "$health" ;;
      malformed) printf 'broken-json\n' >"$health" ;;
      missing_timestamp) printf '{}\n' >"$health" ;;
      string_timestamp) printf '{"updated_at_ns":"%s"}\n' "$FIXTURE_NOW_NS" >"$health" ;;
      fractional_timestamp) printf '{"updated_at_ns":1.5}\n' >"$health" ;;
    esac
    run_fixture "$health"
  done

  for suffix in jsonl.part jsonl.zst.tmp jsonl.part.corrupt; do
    new_fixture
    partition="$ROOT/data/monday/spool/binance-lob/$market/date=2026-10-04/hour=01"
    mkdir -p "$partition"
    artifact="$partition/part-1.$suffix"
    printf 'incomplete-segment\n' >"$artifact"
    artifact_sha=$(monday_sha256_file "$artifact")
    run_fixture "$artifact"
    [[ $(monday_sha256_file "$artifact") == "$artifact_sha" ]] || fail 'preflight changed an incomplete segment'
  done
done

new_fixture
for market in spot usdm; do
  partition="$ROOT/data/monday/spool/binance-lob/$market/date=2026-10-04/hour=01"
  mkdir -p "$partition"
  for suffix in jsonl.zst jsonl.zst.manifest.json jsonl.zst._SUCCESS; do
    printf 'sealed-fixture\n' >"$partition/part-1.$suffix"
  done
done
run_fixture pass

new_fixture
write_health usdm "$((FIXTURE_NOW_NS - 121000000000))"
for market in spot usdm; do
  artifact="$ROOT/data/monday/spool/binance-lob/$market/part-2.jsonl.part"
  printf 'incomplete-segment\n' >"$artifact"
done
run_fixture "$ROOT/data/monday/spool/binance-lob/usdm/health.json"
for market in spot usdm; do
  grep -Fq "$ROOT/data/monday/spool/binance-lob/$market/part-2.jsonl.part" "$fixture/result.log" \
    || fail 'preflight omitted an offending artifact'
done

new_fixture
rm -rf -- "$ROOT/data/monday/spool/binance-lob/usdm"
run_fixture "$ROOT/data/monday/spool/binance-lob/usdm"

new_fixture
mv -- "$ROOT/data/monday/spool/binance-lob/usdm" "$ROOT/indirect-spool"
ln -s "$ROOT/indirect-spool" "$ROOT/data/monday/spool/binance-lob/usdm"
run_fixture "$ROOT/data/monday/spool/binance-lob/usdm"

new_fixture
FIXTURE_FAIL_FIND=1 run_fixture "$ROOT/data/monday/spool/binance-lob/spot"
grep -Fq 'canonical spool scan failed' "$fixture/result.log" || fail 'failed scan did not refuse'

printf 'cutover preflight: %s local fixture cases passed; no host contacted\n' "$cases"
