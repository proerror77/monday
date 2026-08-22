#!/usr/bin/env bash
#
# Contract tests for monday-collector-health.sh. Runs the health script against
# /tmp fixture trees with stubbed systemctl/df/journalctl/mountpoint/logger.
# The script is read-only toward units, so the stubs never mutate anything.
#
# Contract under test: hard gates (breaches) —
#   1. upload-status.json exists and parses (binance-lob spot/usdm, binance-fee)
#   2. last_success_at present and fresh per upload lane; on the polymarket
#      lanes a backlog with a last success older than 30 minutes also breaches
#   3. pending upload backlog bounded (count + oldest pending age)
#   4. failure_count not growing, last_error empty (all lanes)
#   5. /data disk: free <= 25% warns, free <= 15% (used >= 85%) breaches
#   6. polymarket upload timers must be active while their collector is active
# plus the raw-ops Gate containment contract (static template with no active
# instance, running lock, or residual environment) and state-persistence
# failures. Everything else (units, timers, restarts,
# health.json, delay-gate journal, disk warning band, mount, fee snapshot
# journal) is a warning: reported, never blocking ok:true.
#
# Usage: ./test-monday-collector-health.sh
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
health_script="$script_dir/monday-collector-health.sh"
test_root=$(mktemp -d /tmp/monday-collector-health-test.XXXXXX)
trap 'rm -rf "$test_root"' EXIT

stub_dir="$test_root/bin"
spool_root="$test_root/spool"
state_dir="$test_root/state"
mkdir -p "$stub_dir" "$spool_root" "$state_dir"
scenario="$test_root/scenario.tsv"
out_file="$test_root/out"
err_file="$test_root/err"

DF_TOTAL=196000000   # KiB, ~187 GiB (matches the ~196G host disk)
DF_AVAIL_HEALTHY=117600000   # 60% free
DF_AVAIL_WARN=39200000       # 20% free (<25% warn, >=15% so not critical)
DF_AVAIL_CRIT=9800000        # 5% free (<15% crit)

pass_count=0
fail_count=0

expect() {
  # $1 description, $2 condition (0 = pass)
  if [ "$2" -eq 0 ]; then
    pass_count=$((pass_count + 1))
    printf 'PASS: %s\n' "$1"
  else
    fail_count=$((fail_count + 1))
    printf 'FAIL: %s\n' "$1"
  fi
}

rc_is() {
  [ "$RC" -eq "$1" ] && return 0 || return 1
}

grep_out() {
  printf '%s' "$OUT" | grep -q "$1" && return 0 || return 1
}

grep_not_out() {
  if printf '%s' "$OUT" | grep -q "$1"; then
    return 1
  fi
  return 0
}

json_query() {
  printf '%s' "$OUT" | jq -e "$1" >/dev/null 2>&1 && return 0 || return 1
}

run_health() {
  set +e
  env \
    STUB_SCENARIO="$scenario" \
    STUB_DF_TOTAL_KIB="${STUB_DF_TOTAL_KIB:-$DF_TOTAL}" \
    STUB_DF_AVAIL_KIB="${STUB_DF_AVAIL_KIB:-$DF_AVAIL_HEALTHY}" \
    STUB_MOUNTED="${STUB_MOUNTED:-1}" \
    STUB_JOURNAL_TRIPS="${STUB_JOURNAL_TRIPS:-0}" \
    STUB_JOURNAL_FEE_FAILURES="${STUB_JOURNAL_FEE_FAILURES:-0}" \
    STUB_JOURNAL_FAIL="${STUB_JOURNAL_FAIL:-0}" \
    STUB_FLOCK_HELD="${STUB_FLOCK_HELD:-0}" \
    STUB_FLOCK_ERROR="${STUB_FLOCK_ERROR:-0}" \
    STUB_LOCK_APPEAR="${STUB_LOCK_APPEAR:-0}" \
    STUB_LOCK_PATH="$test_root/run/monday/polymarket-raw-ops-gates/control.lock" \
    MONDAY_COLLECTOR_SPOOL_ROOT="$spool_root" \
    MONDAY_COLLECTOR_STATE_DIR="${MONDAY_COLLECTOR_STATE_DIR:-$state_dir}" \
    MONDAY_COLLECTOR_HEALTH_TEST_MODE=1 \
    MONDAY_COLLECTOR_HEALTH_TEST_ROOT="$test_root" \
    PATH="$stub_dir:$PATH" \
    "$health_script" "$@" >"$out_file" 2>"$err_file"
  rc=$?
  set -e
  RC=$rc
  OUT=$(cat "$out_file")
  ERR=$(cat "$err_file")
}

reset_env() {
  STUB_DF_TOTAL_KIB=$DF_TOTAL
  STUB_DF_AVAIL_KIB=$DF_AVAIL_HEALTHY
  STUB_MOUNTED=1
  STUB_JOURNAL_TRIPS=0
  STUB_JOURNAL_FEE_FAILURES=0
  STUB_JOURNAL_FAIL=0
  STUB_FLOCK_HELD=0
  STUB_FLOCK_ERROR=0
  STUB_LOCK_APPEAR=0
  unset MONDAY_COLLECTOR_STATE_DIR
}

reset_state() {
  rm -rf "$state_dir"
  mkdir -p "$state_dir"
  case "$test_root" in
    /tmp/monday-collector-health-test.*)
      rm -rf -- "$test_root/run"
      ;;
    *)
      printf 'refusing to reset an unexpected health test root: %s\n' "$test_root" >&2
      exit 2
      ;;
  esac
}

reset_spool() {
  rm -rf "$spool_root"
  make_spools
}

write_scenario() {
  cat > "$scenario"
}

rewrite_scenario() {
  # Apply a sed expression to the scenario fixture through an explicit temp file
  # and atomic rename. sed -i has no portable form (BSD requires a backup suffix,
  # GNU does not), so scenario rewrites never depend on a platform-specific -i.
  sed "$1" "$scenario" > "$scenario.$$" && mv "$scenario.$$" "$scenario"
}

make_spools() {
  mkdir -p "$spool_root/binance-lob/spot" \
           "$spool_root/binance-lob/usdm" \
           "$spool_root/binance-usdm-reference" \
           "$spool_root/bybit-options" \
           "$spool_root/binance-fee" \
           "$spool_root/polymarket" \
           "$spool_root/polymarket-reference"
}

touch_age() {
  touch -t "$(jq -rn --argjson age "$2" '
    now - $age | floor | localtime | strftime("%Y%m%d%H%M.%S")
  ')" "$1"
}

write_health() {
  # $1 market (spot|usdm), $2 age_seconds, $3 gaps, $4 disk_warning, $5 status
  market=$1
  age=$2
  gaps=$3
  dw=$4
  status=$5
  now_ns=$(( $(date +%s) * 1000000000 ))
  updated_ns=$(( now_ns - age * 1000000000 ))
  # Drop any symlink left by the health-symlink scenario first: cat > would
  # follow the link and write through it, leaving the symlink in place.
  rm -f "$spool_root/binance-lob/$market/health.json"
  cat > "$spool_root/binance-lob/$market/health.json" <<EOF
{"updated_at_ns": $updated_ns, "sequence_gaps": $gaps, "disk_warning": $dw, "status": "$status", "market": "$market"}
EOF
}

write_upload() {
  # $1 path, $2 last_error_at JSON, $3 last_error JSON, $4 failure_count,
  # $5 last_success age in seconds (default 60). Emits six fractional digits
  # and a Z suffix, exactly what polymarket_upload::utc_now (reused by the fee
  # and usdm-reference uploaders) produces, so the fixtures cannot hide a
  # parser gap behind jq's whole-second todate form. jq computes the time
  # portably (BSD date and GNU date disagree on epoch formatting flags).
  age=${5:-60}
  success_at=$(jq -rn --argjson age "$age" '
    (now - $age) as $t
    | ($t | floor | gmtime | strftime("%Y-%m-%dT%H:%M:%S"))
      + "."
      + (("000000" + ((($t - ($t | floor)) * 1000000 | floor) | tostring))[-6:])
      + "Z"')
  cat > "$1" <<EOF
{"last_success_at": "$success_at", "last_error_at": $2, "last_error": $3, "failure_count": $4}
EOF
}

write_upload_ms() {
  # bybit-options lane: epoch-millisecond timestamps.
  age=${5:-60}
  success_ms=$(jq -rn --argjson age "$age" '((now - $age) * 1000 | floor)')
  cat > "$1" <<EOF
{"last_success_at": $success_ms, "last_error_at": $2, "last_error": $3, "failure_count": $4}
EOF
}

healthy_fixtures() {
  reset_spool
  write_health spot 45 0 false synced
  write_health usdm 45 0 false synced
  write_upload "$spool_root/binance-lob/spot/upload-status.json" null null 0
  write_upload "$spool_root/binance-lob/usdm/upload-status.json" null null 0
  write_upload "$spool_root/binance-usdm-reference/upload-status.json" null null 0
  write_upload_ms "$spool_root/bybit-options/upload-status.json" null null 0
  write_upload "$spool_root/binance-fee/upload-status.json" null null 0
  write_upload "$spool_root/polymarket/upload-status.json" null null 0
  write_upload "$spool_root/polymarket-reference/upload-status.json" null null 0
  # Active (not yet rotated) tapes must not count as pending backlog.
  : > "$spool_root/polymarket/market-updates.ndjson"
  : > "$spool_root/polymarket-reference/market-updates.ndjson"
}

healthy_scenario() {
  write_scenario <<'EOF'
binance-lob-archiver-production@spot.service	active	enabled	success	4
binance-lob-archiver-production@usdm.service	active	enabled	success	4
binance-lob-archiver-recovery@spot.timer	active	enabled	-	-
binance-lob-archiver-recovery@usdm.timer	active	enabled	-	-
binance-usdm-reference-collector.service	active	enabled	success	12
bybit-options-archiver.service	active	enabled	success	1
polymarket-market-tape-upload.timer	active	enabled	-	-
polymarket-market-tape-upload.service	-	-	success	-
polymarket-reference-upload.timer	active	enabled	-	-
polymarket-reference-upload.service	-	-	success	-
polymarket-market-tape-upload-watchdog.timer	active	enabled	-	-
polymarket-market-tape-upload-watchdog.service	-	-	success	-
bybit-options-upload.timer	active	enabled	-	-
bybit-options-upload.service	-	-	success	-
binance-usdm-reference-upload.timer	active	enabled	-	-
binance-usdm-reference-upload.service	-	-	success	-
binance-fee-snapshot-spot.timer	active	enabled	-	-
binance-fee-snapshot-spot.service	-	-	success	-
binance-fee-snapshot-usdm.timer	active	enabled	-	-
binance-fee-snapshot-usdm.service	-	-	success	-
binance-fee-upload.timer	active	enabled	-	-
binance-fee-upload.service	-	-	success	-
polymarket-raw-ops-gate@.service	-	static	-	-
EOF
}

# Stubs (installed on the fixture PATH ahead of the real binaries).
cat > "$stub_dir/systemctl" <<'EOF'
#!/bin/sh
SCENARIO="${STUB_SCENARIO:?STUB_SCENARIO not set}"
case "${1:-}" in
  list-units)
    awk -F '\t' '
      $1 ~ /^polymarket-raw-ops-gate@.+[.]service$/ &&
      ($2 == "active" || $2 == "activating" || $2 == "deactivating" || $2 == "reloading") {
        printf "%s loaded %s running\n", $1, $2
      }
    ' "$SCENARIO"
    if [ "${STUB_LOCK_APPEAR:-0}" = "1" ]; then
      mkdir -p "$(dirname -- "${STUB_LOCK_PATH:?STUB_LOCK_PATH not set}")"
      : >"$STUB_LOCK_PATH"
    fi
    exit 0
    ;;
esac
unit=""
prop=""
prev=""
for a in "$@"; do
  if [ "$prev" = "--value" ]; then
    unit="$a"
  fi
  case "$a" in
    Result|NRestarts|ActiveState|SubState) prop="$a" ;;
  esac
  prev="$a"
done
if [ -z "$unit" ]; then
  for a in "$@"; do
    case "$a" in
      *.service|*.timer) unit="$a"; break ;;
    esac
  done
fi
[ -n "$unit" ] || exit 1
line=$(awk -F'\t' -v u="$unit" '$1 == u { print; exit }' "$SCENARIO" 2>/dev/null || true)
active="inactive"; enabled="disabled"; result="success"; nrestarts="0"
if [ -n "$line" ]; then
  active=$(printf '%s\n' "$line" | awk -F'\t' '{print $2}')
  enabled=$(printf '%s\n' "$line" | awk -F'\t' '{print $3}')
  result=$(printf '%s\n' "$line" | awk -F'\t' '{print $4}')
  nrestarts=$(printf '%s\n' "$line" | awk -F'\t' '{print $5}')
fi
[ "$active" != "-" ] || active="inactive"
[ "$enabled" != "-" ] || enabled="disabled"
[ "$result" != "-" ] || result="success"
[ "$nrestarts" != "-" ] || nrestarts="0"
case "$1" in
  is-active) printf '%s\n' "$active" ;;
  is-enabled) printf '%s\n' "$enabled" ;;
  show)
    case "$prop" in
      NRestarts) printf '%s\n' "$nrestarts" ;;
      *) printf '%s\n' "$result" ;;
    esac
    ;;
  *) exit 0 ;;
esac
EOF

cat > "$stub_dir/df" <<'EOF'
#!/bin/sh
total="${STUB_DF_TOTAL_KIB:-196000000}"
avail="${STUB_DF_AVAIL_KIB:-117600000}"
used=$((total - avail))
cap=$((used * 100 / total))
printf 'Filesystem 1024-blocks Used Available Capacity Mounted on\n'
printf '/dev/vda1 %s %s %s %s%% /data\n' "$total" "$used" "$avail" "$cap"
EOF

cat > "$stub_dir/journalctl" <<'EOF'
#!/bin/sh
if [ "${STUB_JOURNAL_FAIL:-0}" = "1" ]; then
  printf 'journalctl: cannot access the journal\n' >&2
  exit 1
fi
i=0
count="${STUB_JOURNAL_TRIPS:-0}"
while [ "$i" -lt "$count" ]; do
  printf 'err: binance: source-to-receive delay exceeds the governed limit\n'
  i=$((i + 1))
done
i=0
count="${STUB_JOURNAL_FEE_FAILURES:-0}"
while [ "$i" -lt "$count" ]; do
  printf 'systemd: binance-fee-snapshot-spot.service: Failed with result exit-code\n'
  i=$((i + 1))
done
EOF

cat > "$stub_dir/mountpoint" <<'EOF'
#!/bin/sh
[ "${STUB_MOUNTED:-1}" = "1" ]
EOF

cat > "$stub_dir/flock" <<'EOF'
#!/bin/sh
if [ "${STUB_FLOCK_ERROR:-0}" = "1" ]; then
  exit 2
fi
[ "${STUB_FLOCK_HELD:-0}" = "1" ] && exit 1
exit 0
EOF

cat > "$stub_dir/logger" <<'EOF'
#!/bin/sh
exit 0
EOF

chmod +x "$stub_dir"/* "$health_script"

# ---------------------------------------------------------------------------
# 1. Healthy baseline
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
run_health
expect "healthy: exit 0" "$(rc_is 0; echo $?)"
expect "healthy: ok:true" "$(grep_out '^ok:true$'; echo $?)"
expect "healthy: no breach lines" "$(grep_not_out '^breach:'; echo $?)"
expect "healthy: no warning lines" "$(grep_not_out '^warning:'; echo $?)"
expect "healthy: state file written" "$(if [ -f "$state_dir/state.json" ]; then echo 0; else echo 1; fi)"
expect "healthy: state records nrestarts" "$(grep -q '^nrestarts|binance-lob-archiver-production@spot.service=4$' "$state_dir/state.json"; echo $?)"
expect "healthy: state records failure_count" "$(grep -q '^failure_count|polymarket-market-tape-upload=0$' "$state_dir/state.json"; echo $?)"

# ---------------------------------------------------------------------------
# 2. Gate 1: missing upload-status.json on a mandated lane is a breach
#    (replaces the old 'LOB missing status is healthy' expectation)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
rm -f "$spool_root/binance-lob/spot/upload-status.json"
run_health
expect "gate1 lob missing: exit 1" "$(rc_is 1; echo $?)"
expect "gate1 lob missing: breach message" "$(grep_out 'binance-lob-archiver-production@spot: upload-status.json missing'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
rm -f "$spool_root/binance-fee/upload-status.json"
run_health
expect "gate1 fee missing: exit 1" "$(rc_is 1; echo $?)"
expect "gate1 fee missing: breach message" "$(grep_out 'binance-fee-upload: upload-status.json missing'; echo $?)"

# ---------------------------------------------------------------------------
# 3. Gate 1: malformed upload-status.json is a breach
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
printf '{not json\n' > "$spool_root/binance-fee/upload-status.json"
run_health
expect "gate1 fee malformed: exit 1" "$(rc_is 1; echo $?)"
expect "gate1 fee malformed: breach message" "$(grep_out 'binance-fee-upload: upload-status.json is malformed'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
printf '{"last_success_at":"2026-08-07T00:00:00Z","last_error_at":null,"last_error":null}\n' > "$spool_root/binance-fee/upload-status.json"
run_health
expect "gate1 fee missing count: exit 1" "$(rc_is 1; echo $?)"
expect "gate1 fee missing count: breach message" "$(grep_out 'binance-fee-upload: upload-status.json is malformed'; echo $?)"

# ---------------------------------------------------------------------------
# 4. Gate 1: missing upload-status.json on a non-mandated lane is only a warning
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
rm -f "$spool_root/polymarket/upload-status.json"
run_health
expect "gate1 polymarket missing: exit 0" "$(rc_is 0; echo $?)"
expect "gate1 polymarket missing: ok:true" "$(grep_out '^ok:true$'; echo $?)"
expect "gate1 polymarket missing: warning message" "$(grep_out '^warning: polymarket-market-tape-upload: upload-status.json missing'; echo $?)"
expect "gate1 polymarket missing: no breach lines" "$(grep_not_out '^breach:'; echo $?)"

# ---------------------------------------------------------------------------
# 5. Gate 2: stale last_success_at is a breach
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
write_upload "$spool_root/binance-lob/spot/upload-status.json" null null 0 7300
run_health
expect "gate2 lob stale: exit 1" "$(rc_is 1; echo $?)"
expect "gate2 lob stale: breach message" "$(grep_out 'binance-lob-archiver-production@spot: last upload success stale'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_upload "$spool_root/binance-fee/upload-status.json" null null 0 700
run_health
expect "gate2 fee stale: exit 1" "$(rc_is 1; echo $?)"
expect "gate2 fee stale: breach message" "$(grep_out 'binance-fee-upload: last upload success stale'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_upload_ms "$spool_root/bybit-options/upload-status.json" null null 0 5500
run_health
expect "gate2 bybit stale (epoch ms): exit 1" "$(rc_is 1; echo $?)"
expect "gate2 bybit stale (epoch ms): breach message" "$(grep_out 'bybit-options-upload: last upload success stale'; echo $?)"

# ---------------------------------------------------------------------------
# 6. Gate 2: missing last_success_at is a breach (fee lane before the parallel
#    uploader change deploys)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
printf '{"last_error_at":null,"last_error":null,"failure_count":0}\n' > "$spool_root/binance-fee/upload-status.json"
run_health
expect "gate2 fee missing success: exit 1" "$(rc_is 1; echo $?)"
expect "gate2 fee missing success: breach message" "$(grep_out 'binance-fee-upload: upload-status.json missing a parseable last_success_at'; echo $?)"

# ---------------------------------------------------------------------------
# 6b. Gate 2: real uploader timestamp formats parse (green)
#     - whole-second Z (legacy form)
#     - six fractional digits + Z (polymarket_upload::utc_now; fee, reference)
#     - +00:00 offset without/with fractional seconds (Chrono to_rfc3339, LOB)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
fresh_offset=$(jq -rn '(now - 60) | floor | gmtime | strftime("%Y-%m-%dT%H:%M:%S") + "+00:00"')
printf '{"last_success_at":"%s","last_error_at":null,"last_error":null,"failure_count":0}\n' "$fresh_offset" \
  > "$spool_root/binance-lob/spot/upload-status.json"
run_health
expect "gate2 offset format: exit 0" "$(rc_is 0; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
fresh_frac_offset=$(jq -rn '(now - 60) | floor | gmtime | strftime("%Y-%m-%dT%H:%M:%S") + ".123456+00:00"')
printf '{"last_success_at":"%s","last_error_at":null,"last_error":null,"failure_count":0}\n' "$fresh_frac_offset" \
  > "$spool_root/binance-lob/spot/upload-status.json"
run_health
expect "gate2 frac+offset format: exit 0" "$(rc_is 0; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
fresh_whole=$(jq -rn '(now - 60) | floor | gmtime | strftime("%Y-%m-%dT%H:%M:%S") + "Z"')
printf '{"last_success_at":"%s","last_error_at":null,"last_error":null,"failure_count":0}\n' "$fresh_whole" \
  > "$spool_root/binance-lob/spot/upload-status.json"
run_health
expect "gate2 whole-second format: exit 0" "$(rc_is 0; echo $?)"

# A non-UTC offset is refused (breach) rather than silently read as UTC.
reset_env
reset_state
healthy_scenario
healthy_fixtures
fresh_non_utc=$(jq -rn '(now - 60) | floor | gmtime | strftime("%Y-%m-%dT%H:%M:%S") + "+09:00"')
printf '{"last_success_at":"%s","last_error_at":null,"last_error":null,"failure_count":0}\n' "$fresh_non_utc" \
  > "$spool_root/binance-lob/spot/upload-status.json"
run_health
expect "gate2 non-UTC offset: exit 1" "$(rc_is 1; echo $?)"
expect "gate2 non-UTC offset: breach message" "$(grep_out 'binance-lob-archiver-production@spot: upload-status.json missing a parseable last_success_at'; echo $?)"

# ---------------------------------------------------------------------------
# 7. Gate 3: pending backlog count over the limit is a breach
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
for i in 1 2 3 4 5; do
  : > "$spool_root/binance-lob/spot/segment-$i.manifest.json"
done
run_health
expect "gate3 lob count: exit 1" "$(rc_is 1; echo $?)"
expect "gate3 lob count: breach message" "$(grep_out 'binance-lob-archiver-production@spot: pending upload backlog 5 over limit 4'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
i=0
while [ "$i" -lt 25 ]; do
  mkdir -p "$spool_root/binance-usdm-reference/lake/raw/venue=binance_usdm/dataset=reference/date=2099-01-01/hour=00/batch=$i"
  i=$((i + 1))
done
run_health
expect "gate3 reference count: exit 1" "$(rc_is 1; echo $?)"
expect "gate3 reference count: breach message" "$(grep_out 'binance-usdm-reference-collector: pending upload backlog 25 over limit 24'; echo $?)"

# ---------------------------------------------------------------------------
# 8. Gate 3: oldest pending backlog age over the bound is a breach
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
: > "$spool_root/binance-lob/spot/old.manifest.json"
touch -t 202001010000 "$spool_root/binance-lob/spot/old.manifest.json"
run_health
expect "gate3 lob age: exit 1" "$(rc_is 1; echo $?)"
expect "gate3 lob age: breach message" "$(grep_out 'binance-lob-archiver-production@spot: oldest pending upload backlog age'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
tape="$spool_root/polymarket/market-updates.20200101T000000.ndjson"
: > "$tape"
touch -t 202001010000 "$tape"
run_health
expect "gate3 polymarket tape age: exit 1" "$(rc_is 1; echo $?)"
expect "gate3 polymarket tape age: breach message" "$(grep_out 'polymarket-market-tape-upload: oldest pending upload backlog age'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
mkdir -p "$spool_root/binance-fee/lake/raw/venue=binance_usdm/dataset=fee/account=abc/date=2020-01-01/hour=00/batch=1"
touch -t 202001010000 "$spool_root/binance-fee/lake/raw/venue=binance_usdm/dataset=fee/account=abc/date=2020-01-01/hour=00/batch=1"
run_health
expect "gate3 fee batch age: exit 1" "$(rc_is 1; echo $?)"
expect "gate3 fee batch age: breach message" "$(grep_out 'binance-fee-upload: oldest pending upload backlog age'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
raw="$spool_root/bybit-options/quotes.ndjson"
: > "$raw"
: > "$raw.manifest.json"
: > "$raw._SUCCESS"
touch -t 202001010000 "$raw"
run_health
expect "gate3 bybit raw age: exit 1" "$(rc_is 1; echo $?)"
expect "gate3 bybit raw age: breach message" "$(grep_out 'bybit-options-upload: oldest pending upload backlog age'; echo $?)"

# An uploaded bybit segment (readback marker present) is not backlog.
reset_env
reset_state
healthy_scenario
healthy_fixtures
raw="$spool_root/bybit-options/quotes.ndjson"
: > "$raw"
: > "$raw.manifest.json"
: > "$raw._SUCCESS"
: > "$raw.uploaded.json"
touch -t 202001010000 "$raw"
run_health
expect "gate3 bybit uploaded: exit 0" "$(rc_is 0; echo $?)"

# Gate 3 still fails closed when a non-mandated lane has no status file: the
# backlog lives on disk independently of the uploader's status reporting.
reset_env
reset_state
healthy_scenario
healthy_fixtures
rm -f "$spool_root/polymarket/upload-status.json"
tape="$spool_root/polymarket/market-updates.20200101T000000.ndjson"
: > "$tape"
touch -t 202001010000 "$tape"
run_health
expect "gate3 no-status backlog: exit 1" "$(rc_is 1; echo $?)"
expect "gate3 no-status backlog: breach message" "$(grep_out 'polymarket-market-tape-upload: oldest pending upload backlog age'; echo $?)"
expect "gate3 no-status backlog: missing status still a warning" "$(grep_out '^warning: polymarket-market-tape-upload: upload-status.json missing'; echo $?)"

# A pending-backlog scan that cannot inspect the spool is a breach, never a
# silent zero backlog.
reset_env
reset_state
healthy_scenario
healthy_fixtures
chmod 000 "$spool_root/binance-fee"
run_health
chmod 755 "$spool_root/binance-fee"
expect "gate3 scan fail: exit 1" "$(rc_is 1; echo $?)"
expect "gate3 scan fail: breach message" "$(grep_out 'binance-fee-upload: pending upload backlog scan failed'; echo $?)"

# ---------------------------------------------------------------------------
# 9. Gate 4: last_error present is a breach
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
write_upload "$spool_root/binance-lob/spot/upload-status.json" '"2026-08-07T01:00:00Z"' '"oss upload readback mismatch"' 3
run_health
expect "gate4 upload error: exit 1" "$(rc_is 1; echo $?)"
expect "gate4 upload error: breach message" "$(grep_out 'upload last_error'; echo $?)"

# ---------------------------------------------------------------------------
# 10. Gate 4: failure_count growth is a breach (two runs)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
run_health
expect "gate4 delta: baseline healthy" "$(rc_is 0; echo $?)"
write_upload "$spool_root/binance-lob/spot/upload-status.json" null null 5
run_health
expect "gate4 delta: exit 1" "$(rc_is 1; echo $?)"
expect "gate4 delta: breach message" "$(grep_out 'failure_count increased 0 -> 5'; echo $?)"

# ---------------------------------------------------------------------------
# 11. Gate 4: a first observation with a nonzero failure_count is a breach,
#     uniformly across lanes
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
write_upload "$spool_root/polymarket/upload-status.json" null null 2
run_health
expect "gate4 initial count: exit 1" "$(rc_is 1; echo $?)"
expect "gate4 initial count: breach message" "$(grep_out 'polymarket-market-tape-upload: initial upload failure_count=2'; echo $?)"

# ---------------------------------------------------------------------------
# 12. Gate 5: disk critical (used >= 85%) is a breach; the warn band stays a
#     warning
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_DF_AVAIL_KIB=$DF_AVAIL_CRIT
run_health
expect "disk critical: exit 1" "$(rc_is 1; echo $?)"
expect "disk critical: breach message" "$(grep_out '^breach: disk: /data free .* at or below critical 15%'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_DF_AVAIL_KIB=$DF_AVAIL_WARN
run_health
expect "disk warn band: exit 0" "$(rc_is 0; echo $?)"
expect "disk warn band: ok:true" "$(grep_out '^ok:true$'; echo $?)"
expect "disk warn band: warning message" "$(grep_out '^warning: disk: /data free .* at or below warning 25%'; echo $?)"
expect "disk warn band: no breach lines" "$(grep_not_out '^breach:'; echo $?)"

# ---------------------------------------------------------------------------
# 13. Demoted: unit inactive / timer disabled / result failure are warnings
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^binance-lob-archiver-production@spot.service	active|binance-lob-archiver-production@spot.service	inactive|'
run_health
expect "unit inactive: exit 0" "$(rc_is 0; echo $?)"
expect "unit inactive: warning message" "$(grep_out '^warning: binance-lob-archiver-production@spot: not active'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^polymarket-market-tape-upload-watchdog.timer	active	enabled|polymarket-market-tape-upload-watchdog.timer	active	disabled|'
run_health
expect "timer not enabled: exit 0" "$(rc_is 0; echo $?)"
expect "timer not enabled: warning message" "$(grep_out '^warning: .*timer not enabled'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^binance-lob-archiver-recovery@spot.timer	active	enabled|binance-lob-archiver-recovery@spot.timer	inactive	disabled|'
run_health
expect "recovery timer down: exit 1" "$(rc_is 1; echo $?)"
expect "recovery timer down: active breach" "$(grep_out '^breach: binance-lob-archiver-recovery@spot.timer: timer not active'; echo $?)"
expect "recovery timer down: enabled breach" "$(grep_out '^breach: binance-lob-archiver-recovery@spot.timer: timer not enabled'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^binance-lob-archiver-production@spot.service	active|binance-lob-archiver-production@spot.service	inactive|; s|^binance-lob-archiver-recovery@spot.timer	active	enabled|binance-lob-archiver-recovery@spot.timer	inactive	disabled|'
run_health
expect "contained recovery timer down: exit 0" "$(rc_is 0; echo $?)"
expect "contained recovery timer down: no recovery alert" "$(grep_not_out 'binance-lob-archiver-recovery@spot.timer:'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^binance-lob-archiver-production@usdm.service	active	enabled	success|binance-lob-archiver-production@usdm.service	active	enabled	exit-code|'
run_health
expect "unit result failure: exit 0" "$(rc_is 0; echo $?)"
expect "unit result failure: warning message" "$(grep_out "^warning: .*Result='exit-code'"; echo $?)"

# ---------------------------------------------------------------------------
# 14. Demoted: restart-rate delta is a warning (two runs)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
run_health
expect "restart delta: baseline healthy" "$(rc_is 0; echo $?)"
rewrite_scenario 's|^binance-lob-archiver-production@spot.service	active	enabled	success	4|binance-lob-archiver-production@spot.service	active	enabled	success	7|'
run_health
expect "restart delta: exit 0" "$(rc_is 0; echo $?)"
expect "restart delta: warning message" "$(grep_out '^warning: .*restart rate high'; echo $?)"

# ---------------------------------------------------------------------------
# 15. Demoted: health.json stale/gap/missing/symlink are warnings
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
write_health spot 600 0 false synced
run_health
expect "health stale: exit 0" "$(rc_is 0; echo $?)"
expect "health stale: warning message" "$(grep_out '^warning: .*health.json stale'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_health usdm 45 5 false synced
run_health
expect "health gap: exit 0" "$(rc_is 0; echo $?)"
expect "health gap: warning message" "$(grep_out '^warning: .*sequence_gaps=5'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
rm -f "$spool_root/binance-lob/spot/health.json"
run_health
expect "health missing: exit 0" "$(rc_is 0; echo $?)"
expect "health missing: warning message" "$(grep_out '^warning: .*health.json missing'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
rm -f "$spool_root/binance-lob/spot/health.json"
ln -s /dev/null "$spool_root/binance-lob/spot/health.json"
run_health
expect "health symlink: exit 0" "$(rc_is 0; echo $?)"
expect "health symlink: warning message" "$(grep_out '^warning: .*health.json missing or a symbolic link'; echo $?)"

# ---------------------------------------------------------------------------
# 16. Demoted: delay-gate trips / journald failure / fee snapshot failures are
#     warnings
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_JOURNAL_TRIPS=2
run_health
expect "delay gate: exit 0" "$(rc_is 0; echo $?)"
expect "delay gate: warning message" "$(grep_out '^warning: .*delay-gate trip'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_JOURNAL_FAIL=1
run_health
expect "journald fail: exit 0" "$(rc_is 0; echo $?)"
expect "journald fail: warning message" "$(grep_out '^warning: .*journald query failed'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_JOURNAL_FEE_FAILURES=1
run_health
expect "fee snapshot failure: exit 0" "$(rc_is 0; echo $?)"
expect "fee snapshot failure: warning message" "$(grep_out '^warning: .*recent snapshot failure'; echo $?)"

# ---------------------------------------------------------------------------
# 17. Demoted: /data unmounted is a warning
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_MOUNTED=0
run_health
expect "mount: exit 0" "$(rc_is 0; echo $?)"
expect "mount: warning message" "$(grep_out '^warning: mount: /data is not mounted'; echo $?)"

# ---------------------------------------------------------------------------
# 18. State persistence failure stays a breach (gate 4 delta evidence)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
# Make the state directory uncreatable so write_state must record a breach.
: > "$test_root/blocked"
MONDAY_COLLECTOR_STATE_DIR="$test_root/blocked/state"
run_health
expect "state persist fail: exit 1" "$(rc_is 1; echo $?)"
expect "state persist fail: breach message" "$(grep_out 'state: state directory unavailable'; echo $?)"

# ---------------------------------------------------------------------------
# 19. polymarket-raw-ops-gate uses the static template and containment checks
# ---------------------------------------------------------------------------
if env MONDAY_COLLECTOR_HEALTH_TEST_MODE=1 \
  MONDAY_COLLECTOR_HEALTH_TEST_ROOT="/tmp/monday-collector-health-test../escape" \
  PATH="$stub_dir:$PATH" "$health_script" --json >/dev/null 2>&1; then
  printf 'health monitor accepted a traversal test root\n' >&2
  exit 1
fi
expect "poly gate test root traversal: rejected" "0"

reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^polymarket-raw-ops-gate@.service	-	static|polymarket-raw-ops-gate@.service	-	indirect|'
run_health
expect "poly gate indirect: exit 1" "$(rc_is 1; echo $?)"
expect "poly gate indirect: breach message" "$(grep_out 'polymarket-raw-ops-gate: unexpected is-enabled'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
run_health
expect "poly gate static clean: exit 0" "$(rc_is 0; echo $?)"
expect "poly gate static clean: no breach" "$(grep_not_out 'polymarket-raw-ops-gate:'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
mkdir -p "$test_root/run/monday/polymarket-raw-ops-gates"
: > "$test_root/run/monday/polymarket-raw-ops-gates/control.lock"
STUB_FLOCK_HELD=1
run_health
expect "poly gate held lock: exit 1" "$(rc_is 1; echo $?)"
expect "poly gate held lock: breach message" "$(grep_out 'polymarket-raw-ops-gate: running control lock is held or unavailable'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
mkdir -p "$test_root/run/monday/polymarket-raw-ops-gates"
: > "$test_root/run/monday/polymarket-raw-ops-gates/control.lock"
STUB_FLOCK_ERROR=1
run_health --json
expect "poly gate lock inspect error: exit 1" "$(rc_is 1; echo $?)"
expect "poly gate lock inspect error: separate JSON state" "$(json_query '
  .ok == false
  and .checks.units["polymarket-raw-ops-gate@.service"].lock_held == false
  and .checks.units["polymarket-raw-ops-gate@.service"].lock_check_failed == true
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
mkdir -p "$test_root/run/monday/polymarket-raw-ops-gates"
: > "$test_root/run/monday/polymarket-raw-ops-gates/control.lock"
chmod 000 "$test_root/run/monday/polymarket-raw-ops-gates/control.lock"
STUB_FLOCK_ERROR=1
run_health --json
chmod 600 "$test_root/run/monday/polymarket-raw-ops-gates/control.lock"
expect "poly gate unreadable regular lock: exit 1" "$(rc_is 1; echo $?)"
expect "poly gate unreadable regular lock: inspect failure" "$(json_query '
  .ok == false
  and .checks.units["polymarket-raw-ops-gate@.service"].lock_check_failed == true
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_LOCK_APPEAR=1
run_health --json
expect "poly gate lock appears during snapshot: exit 1" "$(rc_is 1; echo $?)"
expect "poly gate lock appears during snapshot: fail-closed readback" "$(json_query '
  .ok == false
  and .checks.units["polymarket-raw-ops-gate@.service"].lock_race_detected == true
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
mkdir -p "$test_root/run/monday/polymarket-raw-ops-gates"
: > "$test_root/run/monday/polymarket-raw-ops-gates/candidate.env"
run_health
expect "poly gate residual env: exit 1" "$(rc_is 1; echo $?)"
expect "poly gate residual env: breach message" "$(grep_out 'polymarket-raw-ops-gate: residual Gate environment file'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
mkdir -p "$test_root/run/monday"
: > "$test_root/run/monday/polymarket-raw-ops-gates"
run_health
expect "poly gate runtime root malformed: exit 1" "$(rc_is 1; echo $?)"
expect "poly gate runtime root malformed: breach message" "$(grep_out 'polymarket-raw-ops-gate: Gate runtime root is not an inspectable directory'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
printf '%s\n' 'polymarket-raw-ops-gate@candidate.service	active	enabled	success	0' >> "$scenario"
run_health
expect "poly gate active instance: exit 1" "$(rc_is 1; echo $?)"
expect "poly gate active instance: breach message" "$(grep_out 'polymarket-raw-ops-gate: active Gate instance'; echo $?)"

# ---------------------------------------------------------------------------
# 20. JSON output shape (healthy, includes the warnings array)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
run_health --json
expect "json healthy: exit 0" "$(rc_is 0; echo $?)"
expect "json healthy: parses and shape valid" "$(json_query '
  .ok == true
  and (.checked_at | type) == "string"
  and (.breaches | type) == "array"
  and (.breaches | length) == 0
  and (.warnings | type) == "array"
  and (.warnings | length) == 0
  and (.checks.disk.free_percent | type) == "number"
  and .checks.mount.data_mounted == true
  and .checks.units["binance-lob-archiver-production@spot.service"].active == true
  and .checks.units["bybit-options-archiver.service"].active == true
  and .checks.units["polymarket-raw-ops-gate@.service"].is_enabled == "static"
  and (.checks.units["polymarket-raw-ops-gate@.service"].active_instances | length) == 0
  and .checks.units["polymarket-raw-ops-gate@.service"].lock_held == false
  and .checks.units["polymarket-raw-ops-gate@.service"].lock_check_failed == false
  and (.checks.units["polymarket-raw-ops-gate@.service"].residual_env | length) == 0
  and .checks.units["polymarket-raw-ops-gate@.service"].residual_env_check_failed == false
  and (.checks.health["binance-lob-archiver-production@spot"].age_seconds | type) == "number"
  and .checks.health["binance-lob-archiver-production@spot"].status == "synced"
  and (.checks.uploads["binance-lob-archiver-production@spot"].failure_count | type) == "number"
  and .checks.uploads["binance-lob-archiver-production@spot"].last_error_at == "null"
  and (.checks.uploads["binance-lob-archiver-production@spot"].last_success_age_seconds | type) == "number"
  and (.checks.uploads["binance-lob-archiver-production@spot"].pending_count | type) == "number"
  and .checks.uploads["binance-fee-upload"].last_error == "null"
  and (.checks.uploads["bybit-options-upload"].last_success_age_seconds | type) == "number"
  and (.checks.delay_gate["binance-lob-archiver-production@spot.service"].trips_15m | type) == "number"
'; echo $?)"

# ---------------------------------------------------------------------------
# 21. JSON output shape (warnings do not block ok:true)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_DF_AVAIL_KIB=$DF_AVAIL_WARN
STUB_JOURNAL_TRIPS=1
run_health --json
expect "json warnings: exit 0" "$(rc_is 0; echo $?)"
expect "json warnings: ok:true with warnings" "$(json_query '
  .ok == true
  and (.breaches | length) == 0
  and (.warnings | length) >= 2
  and (.warnings | any(contains("at or below warning")))
  and (.warnings | any(contains("delay-gate trip")))
'; echo $?)"

# ---------------------------------------------------------------------------
# 22. JSON output shape (breaching)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
rm -f "$spool_root/binance-lob/usdm/upload-status.json"
run_health --json
expect "json breach: exit 1" "$(rc_is 1; echo $?)"
expect "json breach: ok:false and breaches non-empty" "$(json_query '.ok == false and (.breaches | length) >= 1'; echo $?)"

# ---------------------------------------------------------------------------
# 23. --dry-run does not touch state
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
run_health --dry-run
expect "dry-run: exit 0" "$(rc_is 0; echo $?)"
if [ ! -e "$state_dir/state.json" ]; then state_missing_rc=0; else state_missing_rc=1; fi
expect "dry-run: state file not created" "$state_missing_rc"

# ---------------------------------------------------------------------------
# 24. Gate 6: an inactive polymarket upload timer while its collector is
#     active is a breach; inactive collector + inactive timer is not
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
printf '%s\n' 'polymarket-market-tape.service	active	enabled	success	2' >> "$scenario"
rewrite_scenario 's|^polymarket-market-tape-upload.timer	active|polymarket-market-tape-upload.timer	inactive|'
run_health
expect "gate6 market timer down: exit 1" "$(rc_is 1; echo $?)"
expect "gate6 market timer down: breach message" "$(grep_out '^breach: polymarket-market-tape-upload.timer: polymarket-market-tape-upload.timer not active .* while collector polymarket-market-tape.service is active'; echo $?)"
expect "gate6 market timer down: timer warning still recorded" "$(grep_out '^warning: polymarket-market-tape-upload.timer: timer not active'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
printf '%s\n' 'polymarket-reference-collector.service	active	enabled	success	3' >> "$scenario"
rewrite_scenario 's|^polymarket-reference-upload.timer	active|polymarket-reference-upload.timer	inactive|'
run_health
expect "gate6 reference timer down: exit 1" "$(rc_is 1; echo $?)"
expect "gate6 reference timer down: breach message" "$(grep_out '^breach: polymarket-reference-upload.timer: .* while collector polymarket-reference-collector.service is active'; echo $?)"

# Both collectors absent from the scenario read as inactive, so inactive
# timers alone stay warnings (covered by the healthy baseline and section 13).

# ---------------------------------------------------------------------------
# 25. Gate 2 polymarket addendum: a pending rotated tape older than 30
#     minutes is a breach; a fresh pending tape with an hour-old last success
#     (normal post-rotation window) is not
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
write_upload "$spool_root/polymarket/upload-status.json" null null 0 1900
tape="$spool_root/polymarket/market-updates.20200101T000000.ndjson"
: > "$tape"
touch -t "$(jq -rn 'now - 1900 | floor | gmtime | strftime("%Y%m%d%H%M.%S")')" "$tape"
run_health
expect "gate2 poly pending stalled: exit 1" "$(rc_is 1; echo $?)"
expect "gate2 poly pending stalled: breach message" "$(grep_out '^breach: polymarket-market-tape-upload: 1 pending upload(s) stalled (oldest age .*s > 1800s)'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_upload "$spool_root/polymarket-reference/upload-status.json" null null 0 1900
tape="$spool_root/polymarket-reference/market-updates.20200101T000000.ndjson"
: > "$tape"
touch -t "$(jq -rn 'now - 1900 | floor | gmtime | strftime("%Y%m%d%H%M.%S")')" "$tape"
run_health
expect "gate2 poly-ref pending stalled: exit 1" "$(rc_is 1; echo $?)"
expect "gate2 poly-ref pending stalled: breach message" "$(grep_out '^breach: polymarket-reference-upload: 1 pending upload(s) stalled'; echo $?)"

# Healthy post-rotation window: last success ~55 minutes old and a just-
# rotated tape pending — the uploader picks it up on the next timer run, so
# this must NOT page.
reset_env
reset_state
healthy_scenario
healthy_fixtures
write_upload "$spool_root/polymarket/upload-status.json" null null 0 3300
: > "$spool_root/polymarket/market-updates.20200101T000000.ndjson"
run_health
expect "gate2 poly fresh pending post-rotation: exit 0" "$(rc_is 0; echo $?)"
expect "gate2 poly fresh pending post-rotation: ok:true" "$(grep_out '^ok:true$'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_upload "$spool_root/polymarket/upload-status.json" null null 0 1900
run_health
expect "gate2 poly stale without pending: exit 0" "$(rc_is 0; echo $?)"
expect "gate2 poly stale without pending: ok:true" "$(grep_out '^ok:true$'; echo $?)"

# The lane bound (two full rotations) still breaches without any backlog.
reset_env
reset_state
healthy_scenario
healthy_fixtures
write_upload "$spool_root/polymarket/upload-status.json" null null 0 7300
run_health
expect "gate2 poly stale lane bound: exit 1" "$(rc_is 1; echo $?)"
expect "gate2 poly stale lane bound: breach message" "$(grep_out '^breach: polymarket-market-tape-upload: last upload success stale'; echo $?)"

# ---------------------------------------------------------------------------
# 15. Recovery queue: empty queue is healthy, stale ready/running and any
#     failed queue entry breach fail-closed.
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
run_health --json
expect "recovery queue empty: exit 0" "$(rc_is 0; echo $?)"
expect "recovery queue empty: json zero counts" "$(json_query '
  .checks.recovery_queue.spot.ready_count == 0 and
  .checks.recovery_queue.spot.running_count == 0 and
  .checks.recovery_queue.spot.failed_count == 0 and
  .checks.recovery_queue.usdm.ready_count == 0 and
  .checks.recovery_queue.usdm.running_count == 0 and
  .checks.recovery_queue.usdm.failed_count == 0
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
mkdir -p "$spool_root/binance-lob-recovery/spot/job.ready"
touch_age "$spool_root/binance-lob-recovery/spot/job.ready" 60
run_health --json
expect "recovery queue ready fresh: exit 0" "$(rc_is 0; echo $?)"
expect "recovery queue ready fresh: json count" "$(json_query '
  .checks.recovery_queue.spot.ready_count == 1 and
  (.checks.recovery_queue.spot.ready_oldest_age_seconds >= 0) and
  .checks.recovery_queue.spot.failed_count == 0
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
mkdir -p "$spool_root/binance-lob-recovery/spot/job.ready"
touch_age "$spool_root/binance-lob-recovery/spot/job.ready" 1900
run_health
expect "recovery queue ready stale: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue ready stale: breach" "$(grep_out '^breach: binance-lob-recovery\[spot\]: oldest ready recovery job age '; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
mkdir -p "$spool_root/binance-lob-recovery/usdm/job.running"
touch_age "$spool_root/binance-lob-recovery/usdm/job.running" 7300
run_health
expect "recovery queue running stale: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue running stale: breach" "$(grep_out '^breach: binance-lob-recovery\[usdm\]: oldest running recovery job age '; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
mkdir -p "$spool_root/binance-lob-recovery/usdm/job.failed"
touch_age "$spool_root/binance-lob-recovery/usdm/job.failed" 30
run_health
expect "recovery queue failed present: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue failed present: breach" "$(grep_out '^breach: binance-lob-recovery\[usdm\]: failed recovery job(s) present'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
mkdir -p "$spool_root/binance-lob-recovery"
ln -s "$spool_root/binance-lob/spot" "$spool_root/binance-lob-recovery/spot"
run_health
expect "recovery queue scan failure: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue scan failure: breach" "$(grep_out '^breach: binance-lob-recovery\[spot\]: recovery queue root is not an inspectable directory'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
printf 'not-a-directory\n' >"$spool_root/binance-lob-recovery"
run_health
expect "recovery queue malformed parent: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue malformed parent: breach" "$(grep_out '^breach: binance-lob-recovery: recovery queue root is not an inspectable directory'; echo $?)"

# ---------------------------------------------------------------------------
printf '\n%d passed, %d failed\n' "$pass_count" "$fail_count"
if [ "$fail_count" -gt 0 ]; then
  printf 'details:\n'
  printf '%s\n' "$ERR" | sed 's/^/  stderr: /' | head -n 40
  exit 1
fi
exit 0
