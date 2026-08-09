#!/usr/bin/env bash
#
# Contract tests for monday-collector-health.sh. Runs the health script against
# /tmp fixture trees with stubbed systemctl/df/journalctl/mountpoint/logger.
# The script is read-only toward units, so the stubs never mutate anything.
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
DF_AVAIL_WARN=39200000       # 20% free (<25% warn, >=10% crit)
DF_AVAIL_CRIT=9800000        # 5% free (<10% crit)

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
    STUB_JOURNAL_FAIL="${STUB_JOURNAL_FAIL:-0}" \
    MONDAY_COLLECTOR_SPOOL_ROOT="$spool_root" \
    MONDAY_COLLECTOR_STATE_DIR="${MONDAY_COLLECTOR_STATE_DIR:-$state_dir}" \
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
  STUB_JOURNAL_FAIL=0
  unset MONDAY_COLLECTOR_STATE_DIR
}

reset_state() {
  rm -rf "$state_dir"
  mkdir -p "$state_dir"
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
  # $1 path, $2 last_error_at JSON, $3 last_error JSON, $4 failure_count
  cat > "$1" <<EOF
{"last_success_at": "2026-08-07T00:00:00Z", "last_error_at": $2, "last_error": $3, "failure_count": $4}
EOF
}

healthy_fixtures() {
  make_spools
  write_health spot 45 0 false synced
  write_health usdm 45 0 false synced
  write_upload "$spool_root/binance-lob/spot/upload-status.json" null null 0
  write_upload "$spool_root/binance-lob/usdm/upload-status.json" null null 0
  write_upload "$spool_root/binance-usdm-reference/upload-status.json" null null 0
  write_upload "$spool_root/bybit-options/upload-status.json" null null 0
  write_upload "$spool_root/binance-fee/upload-status.json" null null 0
  write_upload "$spool_root/polymarket/upload-status.json" null null 0
  write_upload "$spool_root/polymarket-reference/upload-status.json" null null 0
}

healthy_scenario() {
  write_scenario <<'EOF'
binance-lob-archiver-production@spot.service	active	enabled	success	4
binance-lob-archiver-production@usdm.service	active	enabled	success	4
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
polymarket-raw-ops-gate@.service	-	disabled	-	-
EOF
}

# Stubs (installed on the fixture PATH ahead of the real binaries).
cat > "$stub_dir/systemctl" <<'EOF'
#!/bin/sh
SCENARIO="${STUB_SCENARIO:?STUB_SCENARIO not set}"
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
EOF

cat > "$stub_dir/mountpoint" <<'EOF'
#!/bin/sh
[ "${STUB_MOUNTED:-1}" = "1" ]
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
expect "healthy: state file written" "$([ -f "$state_dir/state.json" ]; echo $?)"
expect "healthy: state records nrestarts" "$(grep -q '^nrestarts|binance-lob-archiver-production@spot.service=4$' "$state_dir/state.json"; echo $?)"
expect "healthy: state records failure_count" "$(grep -q '^failure_count|polymarket-market-tape-upload=0$' "$state_dir/state.json"; echo $?)"

# ---------------------------------------------------------------------------
# 2. Disk critical
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_DF_AVAIL_KIB=$DF_AVAIL_CRIT
run_health
expect "disk critical: exit 1" "$(rc_is 1; echo $?)"
expect "disk critical: ok:false" "$(grep_out '^ok:false$'; echo $?)"
expect "disk critical: breach message" "$(grep_out 'below critical 10%'; echo $?)"

# ---------------------------------------------------------------------------
# 3. Disk warning
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_DF_AVAIL_KIB=$DF_AVAIL_WARN
run_health
expect "disk warning: exit 1" "$(rc_is 1; echo $?)"
expect "disk warning: breach message" "$(grep_out 'below warning 25%'; echo $?)"

# ---------------------------------------------------------------------------
# 4. Unit inactive
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^binance-lob-archiver-production@spot.service	active|binance-lob-archiver-production@spot.service	inactive|'
run_health
expect "unit inactive: exit 1" "$(rc_is 1; echo $?)"
expect "unit inactive: breach message" "$(grep_out 'not active'; echo $?)"

# ---------------------------------------------------------------------------
# 5. Timer not enabled
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^polymarket-market-tape-upload-watchdog.timer	active	enabled|polymarket-market-tape-upload-watchdog.timer	active	disabled|'
run_health
expect "timer not enabled: exit 1" "$(rc_is 1; echo $?)"
expect "timer not enabled: breach message" "$(grep_out 'timer not enabled'; echo $?)"

# ---------------------------------------------------------------------------
# 6. Unit result failure
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^binance-lob-archiver-production@usdm.service	active	enabled	success|binance-lob-archiver-production@usdm.service	active	enabled	exit-code|'
run_health
expect "unit result failure: exit 1" "$(rc_is 1; echo $?)"
expect "unit result failure: breach message" "$(grep_out "Result='exit-code'"; echo $?)"

# ---------------------------------------------------------------------------
# 7. Restart-rate delta (two runs)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
run_health
expect "restart delta: baseline healthy" "$(rc_is 0; echo $?)"
rewrite_scenario 's|^binance-lob-archiver-production@spot.service	active	enabled	success	4|binance-lob-archiver-production@spot.service	active	enabled	success	7|'
run_health
expect "restart delta: exit 1" "$(rc_is 1; echo $?)"
expect "restart delta: breach message" "$(grep_out 'restart rate high'; echo $?)"

# ---------------------------------------------------------------------------
# 8. Health stale
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
write_health spot 600 0 false synced
run_health
expect "health stale: exit 1" "$(rc_is 1; echo $?)"
expect "health stale: breach message" "$(grep_out 'health.json stale'; echo $?)"

# ---------------------------------------------------------------------------
# 9. Health sequence gap
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
write_health usdm 45 5 false synced
run_health
expect "health gap: exit 1" "$(rc_is 1; echo $?)"
expect "health gap: breach message" "$(grep_out 'sequence_gaps=5'; echo $?)"

# ---------------------------------------------------------------------------
# 10. Health missing
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
rm -f "$spool_root/binance-lob/spot/health.json"
run_health
expect "health missing: exit 1" "$(rc_is 1; echo $?)"
expect "health missing: breach message" "$(grep_out 'health.json missing'; echo $?)"

# ---------------------------------------------------------------------------
# 10b. Health.json is a symbolic link
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
rm -f "$spool_root/binance-lob/spot/health.json"
ln -s /dev/null "$spool_root/binance-lob/spot/health.json"
run_health
expect "health symlink: exit 1" "$(rc_is 1; echo $?)"
expect "health symlink: breach message" "$(grep_out 'health.json missing or a symbolic link'; echo $?)"

# ---------------------------------------------------------------------------
# 11. Upload error present
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
write_upload "$spool_root/binance-lob/spot/upload-status.json" '"2026-08-07T01:00:00Z"' '"oss upload readback mismatch"' 3
run_health
expect "upload error: exit 1" "$(rc_is 1; echo $?)"
expect "upload error: breach message" "$(grep_out 'upload last_error'; echo $?)"

# ---------------------------------------------------------------------------
# 12. Upload failure-count delta (two runs)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
write_upload "$spool_root/binance-lob/spot/upload-status.json" null null 3
run_health
expect "upload delta: baseline healthy" "$(rc_is 0; echo $?)"
write_upload "$spool_root/binance-lob/spot/upload-status.json" null null 5
run_health
expect "upload delta: exit 1" "$(rc_is 1; echo $?)"
expect "upload delta: breach message" "$(grep_out 'failure_count increased 3 -> 5'; echo $?)"

# ---------------------------------------------------------------------------
# 13. Delay-gate trips
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_JOURNAL_TRIPS=2
run_health
expect "delay gate: exit 1" "$(rc_is 1; echo $?)"
expect "delay gate: breach message" "$(grep_out 'delay-gate trip'; echo $?)"

# ---------------------------------------------------------------------------
# 13b. Delay-gate evidence uninspectable (journalctl failure)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_JOURNAL_FAIL=1
run_health
expect "delay gate journald fail: exit 1" "$(rc_is 1; echo $?)"
expect "delay gate journald fail: breach message" "$(grep_out 'journald query failed'; echo $?)"

# ---------------------------------------------------------------------------
# 13c. State persistence failure is a breach
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
# 14. /data unmounted
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_MOUNTED=0
run_health
expect "mount: exit 1" "$(rc_is 1; echo $?)"
expect "mount: breach message" "$(grep_out '/data is not mounted'; echo $?)"

# ---------------------------------------------------------------------------
# 15. bybit-options-archiver inactive (governed production lane must run)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^bybit-options-archiver.service	active	enabled|bybit-options-archiver.service	inactive	enabled|'
run_health
expect "bybit inactive: exit 1" "$(rc_is 1; echo $?)"
expect "bybit inactive: breach message" "$(grep_out 'bybit-options-archiver: not active'; echo $?)"

# ---------------------------------------------------------------------------
# 16. polymarket-raw-ops-gate indirect (an instance enabled)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^polymarket-raw-ops-gate@.service	-	disabled|polymarket-raw-ops-gate@.service	-	indirect|'
run_health
expect "poly gate indirect: exit 1" "$(rc_is 1; echo $?)"
expect "poly gate indirect: breach message" "$(grep_out 'polymarket-raw-ops-gate: expected disabled'; echo $?)"

# ---------------------------------------------------------------------------
# 17. Missing upload-status.json is not a breach
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
make_spools
write_health spot 45 0 false synced
write_health usdm 45 0 false synced
write_upload "$spool_root/binance-fee/upload-status.json" null null 0
run_health
expect "missing upload-status: exit 0" "$(rc_is 0; echo $?)"
expect "missing upload-status: ok:true" "$(grep_out '^ok:true$'; echo $?)"

# ---------------------------------------------------------------------------
# 18. JSON output shape (healthy)
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
  and (.checks.disk.free_percent | type) == "number"
  and .checks.mount.data_mounted == true
  and .checks.units["binance-lob-archiver-production@spot.service"].active == true
  and .checks.units["bybit-options-archiver.service"].active == true
  and .checks.units["bybit-options-archiver.service"].enabled == true
  and .checks.units["binance-fee-snapshot-spot.timer"].active == true
  and .checks.units["binance-fee-snapshot-usdm.timer"].active == true
  and .checks.units["binance-fee-upload.timer"].enabled == true
  and (.checks.health["binance-lob-archiver-production@spot"].age_seconds | type) == "number"
  and .checks.health["binance-lob-archiver-production@spot"].status == "synced"
  and (.checks.uploads["binance-lob-archiver-production@spot"].failure_count | type) == "number"
  and .checks.uploads["binance-lob-archiver-production@spot"].last_error_at == "null"
  and .checks.uploads["binance-fee-upload"].last_error == "null"
  and (.checks.delay_gate["binance-lob-archiver-production@spot.service"].trips_15m | type) == "number"
'; echo $?)"

# ---------------------------------------------------------------------------
# 19. JSON output shape (breaching)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_DF_AVAIL_KIB=$DF_AVAIL_CRIT
run_health --json
expect "json breach: exit 1" "$(rc_is 1; echo $?)"
expect "json breach: ok:false and breaches non-empty" "$(json_query '.ok == false and (.breaches | length) >= 1'; echo $?)"

# ---------------------------------------------------------------------------
# 20. --dry-run does not touch state
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
run_health --dry-run
expect "dry-run: exit 0" "$(rc_is 0; echo $?)"
expect "dry-run: state file not created" "$([ ! -e "$state_dir/state.json" ]; echo $?)"

# ---------------------------------------------------------------------------
# 21. Binance fee upload status is mandatory
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
rm -f "$spool_root/binance-fee/upload-status.json"
run_health
expect "fee status missing: exit 1" "$(rc_is 1; echo $?)"
expect "fee status missing: breach message" "$(grep_out 'binance-fee-upload: upload-status.json missing'; echo $?)"

# ---------------------------------------------------------------------------
printf '\n%d passed, %d failed\n' "$pass_count" "$fail_count"
if [ "$fail_count" -gt 0 ]; then
  printf 'details:\n'
  printf '%s\n' "$ERR" | sed 's/^/  stderr: /' | head -n 40
  exit 1
fi
exit 0
