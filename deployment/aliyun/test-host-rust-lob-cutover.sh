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
export FIXTURE_CLOCK_FILE="$fixture/clock"
cat >"$fixture/bin/date" <<'CLOCK'
#!/usr/bin/env bash
[[ $# == 1 && $1 == +%s%N ]] || exit 98
if [[ -f $FIXTURE_CLOCK_FILE ]]; then
  cat "$FIXTURE_CLOCK_FILE"
else
  printf '%s\n' "$FIXTURE_NOW_NS"
fi
CLOCK
chmod +x "$fixture/bin/date"
real_find=$(command -v find)
export FIXTURE_REAL_FIND="$real_find"
cat >"$fixture/bin/find" <<'FIND'
#!/usr/bin/env bash
[[ ${FIXTURE_FAIL_FIND:-0} != 1 ]] || exit 73
if [[ -n ${FIXTURE_SCAN_NOW_NS:-} ]]; then
  printf '%s\n' "$FIXTURE_SCAN_NOW_NS" >"$FIXTURE_CLOCK_FILE"
fi
exec "$FIXTURE_REAL_FIND" "$@"
FIND
chmod +x "$fixture/bin/find"
export FIXTURE_REAL_JQ
FIXTURE_REAL_JQ=$(command -v jq)
cat >"$fixture/bin/jq" <<'JSON'
#!/usr/bin/env bash
if [[ -n ${FIXTURE_HEALTH_READ_NOW_NS:-} && ${!#} == */usdm/health.json ]]; then
  printf '%s\n' "$FIXTURE_HEALTH_READ_NOW_NS" >"$FIXTURE_CLOCK_FILE"
fi
exec "$FIXTURE_REAL_JQ" "$@"
JSON
chmod +x "$fixture/bin/jq"
export PATH="$fixture/bin:$PATH"

# Source identity helpers only; do not source or run the cutover's mutation phase.
# shellcheck disable=SC1091
. "$SCRIPT_DIR/rust-lob-control-plane-lib.sh"
# Load only the admission and containment functions for mutation-boundary tests.
# shellcheck disable=SC1090
. <(sed -n '/^cutover_health_preflight() {$/,/^}$/p; /^cutover_preflight() {$/,/^}$/p; /^cutover_contain_writers() {$/,/^}$/p' "$CUTOVER")

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
  rm -f -- "$FIXTURE_HOST_CALLS" "$FIXTURE_CLOCK_FILE"
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
containment_line=$(grep -n -m1 '^  cutover_contain_writers$' "$CUTOVER" | cut -d: -f1)
cleanup_line=$(grep -n -m1 '^trap cleanup EXIT$' "$CUTOVER" | cut -d: -f1)
[[ $preflight_line -lt $containment_line && $preflight_line -lt $cleanup_line ]] \
  || fail 'preflight runs after containment or its rollback trap'
projection_line=$(grep -n -m1 '^  projection_prepared=1$' "$CUTOVER" | cut -d: -f1)
[[ $projection_line -gt $containment_line ]] \
  || fail 'topology rollback can trigger containment after final preflight refusal'

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

  # Capture and compression keep these regular files open before shutdown.
  for suffix in jsonl.part jsonl.zst.tmp; do
    new_fixture
    partition="$ROOT/data/monday/spool/binance-lob/$market/date=2026-10-04/hour=01"
    mkdir -p "$partition"
    artifact="$partition/part-1.$suffix"
    exec 5>"$artifact"
    printf 'active-segment\n' >&5
    artifact_sha=$(monday_sha256_file "$artifact")
    run_fixture pass
    [[ $(monday_sha256_file "$artifact") == "$artifact_sha" ]] || fail 'preflight changed an active segment'
    exec 5>&-
  done

  new_fixture
  artifact="$ROOT/data/monday/spool/binance-lob/$market/part-1.jsonl.part.corrupt"
  printf 'corrupt-segment\n' >"$artifact"
  artifact_sha=$(monday_sha256_file "$artifact")
  run_fixture "$artifact"
  [[ $(monday_sha256_file "$artifact") == "$artifact_sha" ]] || fail 'preflight changed a corrupt segment'

  # Rust rejects these entry classes regardless of their names or suffixes.
  for suffix in unrelated jsonl.part jsonl.zst.tmp jsonl.part.corrupt; do
    for entry in directory_symlink file_symlink dangling_symlink fifo socket; do
      new_fixture
      partition="$ROOT/data/monday/spool/binance-lob/$market/date=2026-10-04/hour=01"
      mkdir -p "$partition" "$ROOT/outside"
      printf 'untouched\n' >"$ROOT/outside/file"
      artifact="$partition/entry.$suffix"
      case $entry in
        directory_symlink) ln -s "$ROOT/outside" "$artifact" ;;
        file_symlink) ln -s "$ROOT/outside/file" "$artifact" ;;
        dangling_symlink) ln -s "$ROOT/missing" "$artifact" ;;
        fifo) mkfifo "$artifact" ;;
        socket)
          perl - "$artifact" <<'SOCKET'
use strict;
use warnings;
use File::Basename qw(dirname basename);
use Socket qw(AF_UNIX SOCK_STREAM sockaddr_un);

chdir dirname($ARGV[0]) or die "chdir socket directory: $!\n";
socket(my $listener, AF_UNIX, SOCK_STREAM, 0) or die "create socket: $!\n";
bind($listener, sockaddr_un(basename($ARGV[0]))) or die "bind socket: $!\n";
close $listener or die "close socket: $!\n";
SOCKET
          ;;
      esac
      run_fixture "$artifact"
      [[ -e $artifact || -L $artifact ]] || fail 'preflight removed an unsafe entry'
      [[ $(cat "$ROOT/outside/file") == untouched ]] || fail 'preflight touched a symlink target'
    done
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
  artifact="$ROOT/data/monday/spool/binance-lob/$market/part-2.jsonl.part.corrupt"
  printf 'incomplete-segment\n' >"$artifact"
done
run_fixture "$ROOT/data/monday/spool/binance-lob/usdm/health.json"
for market in spot usdm; do
  grep -Fq "$ROOT/data/monday/spool/binance-lob/$market/part-2.jsonl.part.corrupt" "$fixture/result.log" \
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

# Time consumed by scanning must count toward the initial health age bound.
new_fixture
FIXTURE_SCAN_NOW_NS=$((FIXTURE_NOW_NS + 121000000000)) \
  run_fixture "$ROOT/data/monday/spool/binance-lob/spot/health.json"

# The dynamically sourced containment function consumes these variables and guards.
# shellcheck disable=SC2034,SC2329
run_containment_fixture() {
  local expected=$1 status log="$fixture/containment.log" flags="$fixture/containment.flags"
  if (
    TEST_ONLY=true
    MONDAY_CUTOVER_FIXTURE_PREFLIGHT=1
    writer_containment_started=0
    writer_containment_failed=0
    trap 'printf "%s\n" "$writer_containment_started" >"$flags"' EXIT
    # The dynamically sourced cutover_contain_writers calls this stub indirectly.
    # shellcheck disable=SC2317
    die() { printf 'pair cutover failed: %s\n' "$*" >&2; exit 1; }
    # A fresh sample reaches this guard. No fixture executes containment.
    # The dynamically sourced cutover_contain_writers calls this stub indirectly.
    # shellcheck disable=SC2317
    monday_rust_lob_contain_writers() { printf 'containment guard\n' >>"$FIXTURE_HOST_CALLS"; return 1; }
    cutover_contain_writers
  ) >"$log" 2>&1; then
    fail 'containment fixture unexpectedly succeeded'
  else
    status=$?
  fi
  [[ $status == 1 ]] || fail "unexpected containment exit status $status"
  if [[ $expected == pass ]]; then
    grep -Fqx 'containment guard' "$FIXTURE_HOST_CALLS" || fail 'fresh sample did not reach containment guard'
    [[ $(cat "$flags") == 1 ]] || fail 'containment guard ran before the flag was armed'
    rm -- "$FIXTURE_HOST_CALLS"
  else
    grep -Fq 'preflight refused; no units were stopped or masked' "$log" \
      || { cat "$log" >&2; fail 'final sample did not refuse containment'; }
    grep -Fq "$expected" "$log" || { cat "$log" >&2; fail 'final sample omitted the bad health path'; }
    [[ $(cat "$flags") == 0 ]] || fail 'failed final sample armed containment rollback'
  fi
  assert_untouched
  cases=$((cases + 1))
}

new_fixture
cutover_preflight || fail 'healthy preparation failed'
run_containment_fixture pass
for market in spot usdm; do
  for boundary_case in elapsed stopped_publishing future missing; do
    new_fixture
    cutover_preflight || fail 'healthy preparation failed'
    health="$ROOT/data/monday/spool/binance-lob/$market/health.json"
    case $boundary_case in
      elapsed) printf '%s\n' "$((FIXTURE_NOW_NS + 121000000000))" >"$FIXTURE_CLOCK_FILE" ;;
      stopped_publishing) write_health "$market" "$((FIXTURE_NOW_NS - 121000000000))" ;;
      future) write_health "$market" "$((FIXTURE_NOW_NS + 1000000000))" ;;
      missing) rm -- "$health" ;;
    esac
    run_containment_fixture "$health"
  done
done

# The final common clock must also include time consumed by the second health read.
new_fixture
cutover_preflight || fail 'healthy preparation failed'
FIXTURE_HEALTH_READ_NOW_NS=$((FIXTURE_NOW_NS + 121000000000)) \
  run_containment_fixture "$ROOT/data/monday/spool/binance-lob/spot/health.json"

printf 'cutover preflight: %s local fixture cases passed; no host contacted\n' "$cases"
