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
#   7. /data must be mounted
# plus the raw-ops Gate containment contract (static template with no active
# instance, running lock, or residual environment) and state-persistence
# failures. Everything else (units, timers, restarts,
# health.json sequence counters and disk warning band are warnings: reported,
# never blocking ok:true. journalctl must not be called by this monitor.
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
journal_calls_file="$test_root/journal.calls"
flock_calls_file="$test_root/flock.calls"

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
  : > "$journal_calls_file"
  : > "$flock_calls_file"
  set +e
  env \
    STUB_SCENARIO="$scenario" \
    STUB_DF_TOTAL_KIB="${STUB_DF_TOTAL_KIB:-$DF_TOTAL}" \
    STUB_DF_AVAIL_KIB="${STUB_DF_AVAIL_KIB:-$DF_AVAIL_HEALTHY}" \
    STUB_MOUNTED="${STUB_MOUNTED:-1}" \
    STUB_JOURNAL_TRIPS="${STUB_JOURNAL_TRIPS:-0}" \
    STUB_JOURNAL_FEE_FAILURES="${STUB_JOURNAL_FEE_FAILURES:-0}" \
    STUB_JOURNAL_FAIL="${STUB_JOURNAL_FAIL:-0}" \
    STUB_JOURNAL_CALLS_FILE="$journal_calls_file" \
    STUB_FLOCK_CALLS_FILE="$flock_calls_file" \
    STUB_FLOCK_HELD="${STUB_FLOCK_HELD:-0}" \
    STUB_FLOCK_ERROR="${STUB_FLOCK_ERROR:-0}" \
    STUB_LOCK_APPEAR="${STUB_LOCK_APPEAR:-0}" \
    STUB_LOCK_PATH="$test_root/run/monday/polymarket-raw-ops-gates/control.lock" \
    MONDAY_COLLECTOR_SPOOL_ROOT="$spool_root" \
    MONDAY_COLLECTOR_STATE_DIR="${MONDAY_COLLECTOR_STATE_DIR:-$state_dir}" \
    MONDAY_COLLECTOR_HEALTH_TEST_MODE=1 \
    MONDAY_COLLECTOR_HEALTH_TEST_ROOT="$test_root" \
    MONDAY_COLLECTOR_HEALTH_TEST_HFT_GID="${STUB_HFT_GID:-$(id -g)}" \
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
  STUB_HFT_GID=$(id -g)
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
# $1 market (spot|usdm), $2 age_seconds, $3 current sequence_gaps, $4 disk_warning,
  # $5 status, $6 session_id (optional), $7 sequence_gap_total (optional).
  market=$1
  age=$2
  gaps=$3
  dw=$4
  status=$5
  session=${6:-"fixture-$market"}
  total=${7:-$gaps}
  case "$total" in
    '' | *[!0-9]*) total_json=$(jq -Rn --arg value "$total" '$value') ;;
    *) total_json=$total ;;
  esac
  session_json=$(jq -Rn --arg value "$session" '$value')
  now_ns=$(( $(date +%s) * 1000000000 ))
  updated_ns=$(( now_ns - age * 1000000000 ))
  # Drop any symlink left by the health-symlink scenario first: cat > would
  # follow the link and write through it, leaving the symlink in place.
  rm -f "$spool_root/binance-lob/$market/health.json"
  cat > "$spool_root/binance-lob/$market/health.json" <<EOF
{"updated_at_ns": $updated_ns, "sequence_gaps": $gaps, "sequence_gap_total": $total_json, "session_id": $session_json, "disk_warning": $dw, "status": "$status", "market": "$market"}
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

make_recovery_queue_root() {
  mkdir -p "$spool_root/binance-lob-recovery"
  chgrp "$(id -g)" "$spool_root/binance-lob-recovery"
  chmod 0750 "$spool_root/binance-lob-recovery"
}

make_recovery_queue() {
  make_recovery_queue_root
  mkdir -p "$spool_root/binance-lob-recovery/$1"
  chgrp "$(id -g)" "$spool_root/binance-lob-recovery/$1"
  chmod 0750 "$spool_root/binance-lob-recovery/$1"
}

write_recovery_job() {
  # $1 market, $2 job id, $3 state (ready|running|failed)
  market=$1
  job_id=$2
  state=$3
  job_dir="$spool_root/binance-lob-recovery/$market/$job_id.$state"
  hash64=$(printf '%064d' 0)
  source_revision=$(printf '%040d' 0)
  make_recovery_queue "$market"
  mkdir -p "$job_dir"
  jq -n \
    --arg schema monday.rust_lob_recovery_queue.v1 \
    --arg market "$market" \
    --arg job_id "$job_id" \
    --arg queued_at 2026-08-26T00:00:00Z \
    --arg canonical_spool "$spool_root/binance-lob/$market" \
    --arg recovery_unit "binance-lob-archiver-recovery@$market.service" \
    --arg release_sha256 "$hash64" \
    --arg deployment_bundle_sha256 "$hash64" \
    --arg deployment_source_revision "$source_revision" \
    --arg env_sha256 "$hash64" \
    '{schema:$schema,market:$market,job_id:$job_id,queued_at:$queued_at,
      canonical_spool:$canonical_spool,recovery_unit:$recovery_unit,
      release_sha256:$release_sha256,
      deployment_bundle_sha256:$deployment_bundle_sha256,
      deployment_source_revision:$deployment_source_revision,
      env_sha256:$env_sha256,release_env:"recovery.env"}' \
    >"$job_dir/job.json"
}

write_isolation_marker() {
  # $1 market, $2 job id; the matching ready receipt must already exist.
  market=$1
  job_id=$2
  queue_dir="$spool_root/binance-lob-recovery/$market"
  ready_dir="$queue_dir/$job_id.ready"
  receipt_sha256=$(sha256sum "$ready_dir/job.json" | awk '{print $1}')
  jq -n \
    --arg schema monday.rust_lob_recovery_isolation.v1 \
    --arg job_id "$job_id" \
    --arg market "$market" \
    --arg canonical_spool "$spool_root/binance-lob/$market" \
    --arg ready_dir "$ready_dir" \
    --arg receipt_sha256 "$receipt_sha256" \
    '{schema:$schema,job_id:$job_id,market:$market,
      canonical_spool:$canonical_spool,ready_dir:$ready_dir,
      receipt_sha256:$receipt_sha256}' >"$queue_dir/isolation.json"
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
    Result|NRestarts|ActiveState|SubState|NextElapseUSecMonotonic) prop="$a" ;;
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
active="inactive"; enabled="disabled"; result="success"; nrestarts="0"; substate=""; next_elapse=""
if [ -n "$line" ]; then
  active=$(printf '%s\n' "$line" | awk -F'\t' '{print $2}')
  enabled=$(printf '%s\n' "$line" | awk -F'\t' '{print $3}')
  result=$(printf '%s\n' "$line" | awk -F'\t' '{print $4}')
  nrestarts=$(printf '%s\n' "$line" | awk -F'\t' '{print $5}')
  substate=$(printf '%s\n' "$line" | awk -F'\t' '{print $6}')
  next_elapse=$(printf '%s\n' "$line" | awk -F'\t' '{print $7}')
fi
[ "$active" != "-" ] || active="inactive"
[ "$enabled" != "-" ] || enabled="disabled"
[ "$result" != "-" ] || result="success"
[ "$nrestarts" != "-" ] || nrestarts="0"
if [ "${unit##*.}" = "timer" ]; then
  [ -n "$substate" ] || substate="waiting"
  [ -n "$next_elapse" ] || next_elapse="123456789"
else
  [ -n "$substate" ] || substate="dead"
fi
[ "$substate" != "-" ] || substate=""
[ "$next_elapse" != "-" ] || next_elapse=""
case "$1" in
  is-active) printf '%s\n' "$active" ;;
  is-enabled) printf '%s\n' "$enabled" ;;
  show)
    case "$prop" in
      NRestarts) printf '%s\n' "$nrestarts" ;;
      SubState) printf '%s\n' "$substate" ;;
      NextElapseUSecMonotonic) printf '%s\n' "$next_elapse" ;;
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
if [ -n "${STUB_JOURNAL_CALLS_FILE:-}" ]; then
  printf '%s\n' "$*" >> "$STUB_JOURNAL_CALLS_FILE"
fi
printf 'journalctl must not be called by collector-health\n' >&2
exit 99
EOF

cat > "$stub_dir/mountpoint" <<'EOF'
#!/bin/sh
[ "${STUB_MOUNTED:-1}" = "1" ]
EOF

cat > "$stub_dir/flock" <<'EOF'
#!/bin/sh
if [ -n "${STUB_FLOCK_CALLS_FILE:-}" ]; then
  printf '%s\n' "$*" >> "$STUB_FLOCK_CALLS_FILE"
fi
case "$*" in
  '-s -n 9' | '-n 9') ;;
  *) exit 2 ;;
esac
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
expect "healthy: state records sequence session" "$(grep -q '^sequence_gap_session|binance-lob-archiver-production@spot=fixture-spot$' "$state_dir/state.json"; echo $?)"
expect "healthy: state records sequence total" "$(grep -q '^sequence_gap_total|binance-lob-archiver-production@spot=0$' "$state_dir/state.json"; echo $?)"

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
write_upload "$spool_root/binance-lob/spot/upload-status.json" null null 0 700
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
for i in 1 2 3; do
  : > "$spool_root/binance-lob/spot/segment-$i.manifest.json"
done
run_health
expect "gate3 lob count: exit 1" "$(rc_is 1; echo $?)"
expect "gate3 lob count: breach message" "$(grep_out 'binance-lob-archiver-production@spot: pending upload backlog 3 over limit 2'; echo $?)"

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
touch_age "$spool_root/binance-lob/spot/old.manifest.json" 901
run_health
expect "gate3 lob age: exit 1" "$(rc_is 1; echo $?)"
expect "gate3 lob age: breach message" "$(grep_out 'binance-lob-archiver-production@spot: oldest pending upload backlog age .* over 900s'; echo $?)"

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
rewrite_scenario 's|^polymarket-market-tape-upload-watchdog.timer\tactive\tenabled\t-\t-$|polymarket-market-tape-upload-watchdog.timer\tactive\tenabled\t-\t-\telapsed\t123456789|'
run_health
expect "timer elapsed: exit 1" "$(rc_is 1; echo $?)"
expect "timer elapsed: breach message" "$(grep_out "^breach: polymarket-market-tape-upload-watchdog.timer: timer not waiting or running (SubState='elapsed')"; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^polymarket-market-tape-upload-watchdog.timer\tactive\tenabled\t-\t-$|polymarket-market-tape-upload-watchdog.timer\tactive\tenabled\t-\t-\twaiting\t-|'
run_health
expect "timer missing next elapse: exit 1" "$(rc_is 1; echo $?)"
expect "timer missing next elapse: breach message" "$(grep_out '^breach: polymarket-market-tape-upload-watchdog.timer: waiting timer has no finite next elapse'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^polymarket-market-tape-upload-watchdog.timer\tactive\tenabled\t-\t-$|polymarket-market-tape-upload-watchdog.timer\tactive\tenabled\t-\t-\trunning\tinfinity|'
run_health
expect "timer running: exit 0" "$(rc_is 0; echo $?)"
expect "timer running: no watchdog breach" "$(grep_not_out '^breach: polymarket-market-tape-upload-watchdog.timer:'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^polymarket-market-tape-upload.timer\tactive\tenabled\t-\t-$|polymarket-market-tape-upload.timer\tactive\tenabled\t-\t-\telapsed\tinfinity|'
run_health
expect "market upload timer elapsed: exit 1" "$(rc_is 1; echo $?)"
expect "market upload timer elapsed: breach message" "$(grep_out "^breach: polymarket-market-tape-upload.timer: timer not waiting or running (SubState='elapsed')"; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
rewrite_scenario 's|^polymarket-reference-upload.timer\tactive\tenabled\t-\t-$|polymarket-reference-upload.timer\tactive\tenabled\t-\t-\twaiting\tinfinity|'
run_health
expect "reference upload timer infinite next: exit 1" "$(rc_is 1; echo $?)"
expect "reference upload timer infinite next: breach message" "$(grep_out '^breach: polymarket-reference-upload.timer: waiting timer has no finite next elapse'; echo $?)"

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
# 15. Demoted: health freshness and typed sequence counters are warnings.
#     The counter is cumulative within one session and compared against the
#     prior poll from the existing state file.
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
write_health usdm 45 5 false synced session-usdm 5
run_health
expect "health gap: exit 0" "$(rc_is 0; echo $?)"
expect "health gap current counter: warning message" "$(grep_out '^warning: .*sequence_gaps=5'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_health usdm 45 2 false synced mismatch-session 5
run_health --json
expect "health gap fields differ: exit 0" "$(rc_is 0; echo $?)"
expect "health gap fields differ: preserve current sequence_gaps" "$(json_query '.checks.health["binance-lob-archiver-production@usdm"].sequence_gaps == 2 and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_total == 5 and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_baseline == "baseline"'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_health usdm 45 0 false synced session-usdm 2
run_health
write_health usdm 45 0 false synced session-usdm 5
run_health --json
expect "health gap increase: exit 0" "$(rc_is 0; echo $?)"
expect "health gap increase: delta warning" "$(grep_out 'sequence_gap_total increased 2 -> 5 (delta=3)'; echo $?)"
expect "health gap increase: typed delta" "$(json_query '.checks.health["binance-lob-archiver-production@usdm"].sequence_gap_delta == 3 and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_baseline == "increased" and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_observed == true'; echo $?)"

write_health usdm 45 0 false synced session-usdm 5
run_health --json
expect "health gap stable: exit 0" "$(rc_is 0; echo $?)"
expect "health gap stable: no repeated warning" "$(grep_not_out 'sequence_gap_total increased'; echo $?)"
expect "health gap stable: zero delta" "$(json_query '.checks.health["binance-lob-archiver-production@usdm"].sequence_gap_delta == 0 and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_baseline == "stable"'; echo $?)"

write_health usdm 45 0 false synced session-usdm-next 1
run_health --json
expect "health session change: exit 0" "$(rc_is 0; echo $?)"
expect "health session change: warning and baseline reset" "$(grep_out 'sequence_gap session changed (session-usdm -> session-usdm-next); baseline reset at total=1'; echo $?)"
expect "health session change: typed status" "$(json_query '.checks.health["binance-lob-archiver-production@usdm"].sequence_gap_baseline == "session_changed" and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_observed == true and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_delta == null'; echo $?)"

write_health usdm 45 0 false synced session-usdm-next 0
run_health --json
expect "health gap regression: exit 0" "$(rc_is 0; echo $?)"
expect "health gap regression: warning" "$(grep_out 'sequence_gap_total regressed 1 -> 0'; echo $?)"
expect "health gap regression: typed status" "$(json_query '.checks.health["binance-lob-archiver-production@usdm"].sequence_gap_baseline == "regressed" and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_observed == true'; echo $?)"
write_health usdm 45 0 false synced session-usdm-next 1
run_health --json
expect "health gap post-regression: exit 0" "$(rc_is 0; echo $?)"
expect "health gap post-regression: delta warning" "$(json_query '.warnings | any(contains("sequence_gap_total increased 0 -> 1 (delta=1)"))'; echo $?)"
expect "health gap post-regression: rebaseline applied" "$(json_query '.checks.health["binance-lob-archiver-production@usdm"].sequence_gap_baseline == "increased" and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_delta == 1'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_health usdm 45 0 false synced session-preserved 4
run_health
write_health usdm 45 0 false synced session-preserved oops
run_health --json
expect "health malformed total: exit 0" "$(rc_is 0; echo $?)"
expect "health malformed total: warning" "$(grep_out 'sequence counter malformed'; echo $?)"
expect "health malformed total: typed null and prior retained" "$(json_query '.checks.health["binance-lob-archiver-production@usdm"].sequence_gap_total == null and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_previous_total == 4 and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_baseline == "malformed" and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_observed == false'; echo $?)"
expect "health malformed total: state retained" "$(grep -q '^sequence_gap_total|binance-lob-archiver-production@usdm=4$' "$state_dir/state.json"; echo $?)"

write_health usdm 45 0 false synced session-preserved 7
jq 'del(.sequence_gap_total)' "$spool_root/binance-lob/usdm/health.json" > "$spool_root/binance-lob/usdm/health.json.tmp" \
  && mv "$spool_root/binance-lob/usdm/health.json.tmp" "$spool_root/binance-lob/usdm/health.json"
run_health --json
expect "health missing total: typed null and prior retained" "$(json_query '.checks.health["binance-lob-archiver-production@usdm"].sequence_gap_total == null and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_previous_total == 4 and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_baseline == "malformed" and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_observed == false'; echo $?)"

write_health usdm 45 0 false synced malformed-session 6
jq '.session_id = 12345' "$spool_root/binance-lob/usdm/health.json" > "$spool_root/binance-lob/usdm/health.json.tmp" \
  && mv "$spool_root/binance-lob/usdm/health.json.tmp" "$spool_root/binance-lob/usdm/health.json"
run_health --json
expect "health malformed session: exit 0" "$(rc_is 0; echo $?)"
expect "health malformed session: typed total and prior retained" "$(json_query '.checks.health["binance-lob-archiver-production@usdm"].sequence_gap_total == 6 and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_previous_total == 4 and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_baseline == "malformed" and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_observed == false'; echo $?)"

write_health usdm 45 0 false synced session-preserved 9
run_health --dry-run --json
expect "health dry-run: exit 0" "$(rc_is 0; echo $?)"
expect "health dry-run: no counter comparison" "$(grep_not_out 'sequence_gap_total increased'; echo $?)"
expect "health dry-run: typed status" "$(json_query '.checks.health["binance-lob-archiver-production@usdm"].sequence_gap_baseline == "dry_run" and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_observed == true and .checks.health["binance-lob-archiver-production@usdm"].sequence_gap_delta == null'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_health spot 45 0 false synced preserved-session 4
run_health
rm -f "$spool_root/binance-lob/spot/health.json"
run_health --json
expect "health missing: exit 0" "$(rc_is 0; echo $?)"
expect "health missing: warning message" "$(json_query '.warnings | any(contains("health.json missing"))'; echo $?)"
expect "health missing: prior retained" "$(json_query '.checks.health["binance-lob-archiver-production@spot"].sequence_gap_observed == false and .checks.health["binance-lob-archiver-production@spot"].sequence_gap_baseline == "missing" and .checks.health["binance-lob-archiver-production@spot"].sequence_gap_previous_total == 4'; echo $?)"
expect "health missing: state retained" "$(grep -q '^sequence_gap_total|binance-lob-archiver-production@spot=4$' "$state_dir/state.json"; echo $?)"

write_health spot 45 0 false synced preserved-session 5
jq '.updated_at_ns = {invalid: true}' "$spool_root/binance-lob/spot/health.json" > "$spool_root/binance-lob/spot/health.json.tmp" \
  && mv "$spool_root/binance-lob/spot/health.json.tmp" "$spool_root/binance-lob/spot/health.json"
run_health --json
expect "health invalid timestamp: exit 0" "$(rc_is 0; echo $?)"
expect "health invalid timestamp: prior retained" "$(json_query '.checks.health["binance-lob-archiver-production@spot"].sequence_gap_observed == false and .checks.health["binance-lob-archiver-production@spot"].sequence_gap_baseline == "malformed" and .checks.health["binance-lob-archiver-production@spot"].sequence_gap_previous_total == 4'; echo $?)"

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
# 16. Journal scans are removed. The compatibility delay_gate projection is
#     explicit, and fee health remains covered by oneshot Result + upload status.
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
run_health --json
expect "typed health replacement: exit 0" "$(rc_is 0; echo $?)"
expect "typed health replacement: delay gate compatibility" "$(json_query '
  .checks.delay_gate["binance-lob-archiver-production@spot.service"].trips_15m == null
  and .checks.delay_gate["binance-lob-archiver-production@spot.service"].observed == false
  and .checks.delay_gate["binance-lob-archiver-production@spot.service"].skipped_reason == "replaced_by_health_sequence_counters"
  and .checks.delay_gate["binance-lob-archiver-production@spot.service"].replacement == "checks.health"
'; echo $?)"
expect "typed health replacement: journalctl zero calls" "$(if [ ! -s "$journal_calls_file" ]; then echo 0; else echo 1; fi)"
expect "fee oneshot and upload evidence: clean" "$(json_query '.checks.units["binance-fee-snapshot-spot.service"].result == "success" and .checks.units["binance-fee-upload.service"].result == "success" and .checks.uploads["binance-fee-upload"].failure_count == 0'; echo $?)"
awk -F '\t' 'BEGIN { OFS = "\t" } $1 == "binance-fee-snapshot-spot.service" { $4 = "exit-code" } { print }' \
  "$scenario" > "$scenario.$$" && mv "$scenario.$$" "$scenario"
run_health --json
expect "fee oneshot failure remains a warning" "$(rc_is 0; echo $?)"
expect "fee oneshot failure is observed without journal" "$(json_query '.warnings | any(contains("binance-fee-snapshot-spot.service: last systemd Result"))'; echo $?)"
expect "fee oneshot failure: journalctl zero calls" "$(if [ ! -s "$journal_calls_file" ]; then echo 0; else echo 1; fi)"

# ---------------------------------------------------------------------------
# 17. /data unmounted is a breach
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_MOUNTED=0
run_health
expect "mount: exit 1" "$(rc_is 1; echo $?)"
expect "mount: breach message" "$(grep_out '^breach: mount: /data is not mounted'; echo $?)"

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
  and .checks.delay_gate["binance-lob-archiver-production@spot.service"].trips_15m == null
  and .checks.delay_gate["binance-lob-archiver-production@spot.service"].observed == false
  and .checks.delay_gate["binance-lob-archiver-production@spot.service"].skipped_reason == "replaced_by_health_sequence_counters"
  and .checks.delay_gate["binance-lob-archiver-production@spot.service"].replacement == "checks.health"
  and .checks.health["binance-lob-archiver-production@spot"].sequence_gap_total == 0
  and .checks.health["binance-lob-archiver-production@spot"].sequence_gap_baseline == "baseline"
  and .checks.health["binance-lob-archiver-production@spot"].sequence_gap_observed == true
  and .checks.health["binance-lob-archiver-production@spot"].session_id == "fixture-spot"
'; echo $?)"
expect "json healthy: journalctl zero calls" "$(if [ ! -s "$journal_calls_file" ]; then echo 0; else echo 1; fi)"

# ---------------------------------------------------------------------------
# 21. JSON output shape (warnings do not block ok:true)
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_DF_AVAIL_KIB=$DF_AVAIL_WARN
write_health usdm 45 1 false synced warning-session 1
run_health --json
expect "json warnings: exit 0" "$(rc_is 0; echo $?)"
expect "json warnings: ok:true with warnings" "$(json_query '
  .ok == true
  and (.breaches | length) == 0
  and (.warnings | length) >= 2
  and (.warnings | any(contains("at or below warning")))
  and (.warnings | any(contains("sequence_gaps=1")))
'; echo $?)"

# ---------------------------------------------------------------------------
# 21b. Journal command is a hard-fail stub and must never be reached.
# ---------------------------------------------------------------------------
reset_env
reset_state
healthy_scenario
healthy_fixtures
run_health --json
journal_call_count=$(wc -l < "$journal_calls_file" | tr -d ' ')
expect "journal replacement: exit 0" "$(rc_is 0; echo $?)"
expect "journal replacement: zero journal calls" "$(if [ "$journal_call_count" = 0 ]; then echo 0; else echo 1; fi)"
expect "journal replacement: no journald coordination schema" "$(json_query 'has("checks") and (.checks | has("journald_coordination") | not)'; echo $?)"

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
  .checks.recovery_queue.spot.malformed_count == 0 and
  .checks.recovery_queue.spot.legacy_unreceipted_count == 0 and
  .checks.recovery_queue.usdm.ready_count == 0 and
  .checks.recovery_queue.usdm.running_count == 0 and
  .checks.recovery_queue.usdm.failed_count == 0 and
  .checks.recovery_queue.usdm.malformed_count == 0 and
  .checks.recovery_queue.usdm.legacy_unreceipted_count == 0
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_recovery_job spot job ready
touch_age "$spool_root/binance-lob-recovery/spot/job.ready" 60
run_health --json
expect "recovery queue ready fresh: exit 0" "$(rc_is 0; echo $?)"
expect "recovery queue ready fresh: json count" "$(json_query '
  .checks.recovery_queue.spot.ready_count == 1 and
  (.checks.recovery_queue.spot.ready_oldest_age_seconds >= 0) and
  .checks.recovery_queue.spot.failed_count == 0 and
  .checks.recovery_queue.spot.malformed_count == 0
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_recovery_job spot job ready
touch_age "$spool_root/binance-lob-recovery/spot/job.ready" 1900
run_health
expect "recovery queue ready stale: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue ready stale: breach" "$(grep_out '^breach: binance-lob-recovery\[spot\]: oldest ready recovery job age '; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_recovery_job usdm job running
touch_age "$spool_root/binance-lob-recovery/usdm/job.running" 7300
run_health
expect "recovery queue running stale: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue running stale: breach" "$(grep_out '^breach: binance-lob-recovery\[usdm\]: oldest running recovery job age '; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_recovery_job usdm job failed
touch_age "$spool_root/binance-lob-recovery/usdm/job.failed" 30
run_health
expect "recovery queue failed present: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue failed present: breach" "$(grep_out '^breach: binance-lob-recovery\[usdm\]: failed recovery job(s) present'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
make_recovery_queue spot
mkdir -p "$spool_root/binance-lob-recovery/spot/missing.ready"
run_health --json
expect "recovery queue missing receipt: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue missing receipt: malformed count" "$(json_query '
  .checks.recovery_queue.spot.ready_count == 1 and
  .checks.recovery_queue.spot.malformed_count == 1
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_recovery_job usdm mismatch ready
jq '.canonical_spool = "/wrong"' \
  "$spool_root/binance-lob-recovery/usdm/mismatch.ready/job.json" \
  >"$spool_root/binance-lob-recovery/usdm/mismatch.ready/job.json.tmp"
mv "$spool_root/binance-lob-recovery/usdm/mismatch.ready/job.json.tmp" \
  "$spool_root/binance-lob-recovery/usdm/mismatch.ready/job.json"
run_health --json
expect "recovery queue mismatched receipt: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue mismatched receipt: malformed count" "$(json_query '
  .checks.recovery_queue.usdm.ready_count == 1 and
  .checks.recovery_queue.usdm.malformed_count == 1
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_recovery_job spot missing-hash ready
jq 'del(.release_sha256)' \
  "$spool_root/binance-lob-recovery/spot/missing-hash.ready/job.json" \
  >"$spool_root/binance-lob-recovery/spot/missing-hash.ready/job.json.tmp"
mv "$spool_root/binance-lob-recovery/spot/missing-hash.ready/job.json.tmp" \
  "$spool_root/binance-lob-recovery/spot/missing-hash.ready/job.json"
run_health --json
expect "recovery queue missing hash receipt: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue missing hash receipt: malformed count" "$(json_query '
  .checks.recovery_queue.spot.ready_count == 1 and
  .checks.recovery_queue.spot.malformed_count == 1
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
isolation_job=20260826T000001Z-spot-000000000000-1
write_recovery_job spot "$isolation_job" ready
write_isolation_marker spot "$isolation_job"
run_health --json
expect "recovery queue active isolation: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue active isolation: bound marker" "$(json_query '
  .checks.recovery_queue.spot.isolation_active == true and
  .checks.recovery_queue.spot.isolation_valid == true and
  (.checks.recovery_queue.spot.isolation_age_seconds | type) == "number"
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
isolation_job=20260826T000001Z-usdm-000000000000-2
write_recovery_job usdm "$isolation_job" ready
write_isolation_marker usdm "$isolation_job"
jq '.receipt_sha256 = ("f" * 64)' \
  "$spool_root/binance-lob-recovery/usdm/isolation.json" \
  >"$spool_root/binance-lob-recovery/usdm/isolation.json.tmp"
mv "$spool_root/binance-lob-recovery/usdm/isolation.json.tmp" \
  "$spool_root/binance-lob-recovery/usdm/isolation.json"
run_health --json
expect "recovery queue drifted isolation: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue drifted isolation: malformed marker" "$(json_query '
  .checks.recovery_queue.usdm.isolation_active == true and
  .checks.recovery_queue.usdm.isolation_valid == false
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
make_recovery_queue spot
mkdir -p "$spool_root/binance-lob-recovery/spot/legacy-unreceipted/legacy-job"
run_health --json
expect "recovery queue legacy containment: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue legacy containment: count" "$(json_query '
  .checks.recovery_queue.spot.legacy_unreceipted_count == 1
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
make_recovery_queue spot
mkdir -p "$spool_root/binance-lob-recovery/spot/legacy-unreceipted"
chmod 0770 "$spool_root/binance-lob-recovery/spot/legacy-unreceipted"
run_health
expect "recovery queue writable legacy containment: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue writable legacy containment: breach" "$(grep_out '^breach: binance-lob-recovery\[spot\]: legacy-unreceipted is not an inspectable root-owned directory'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
make_recovery_queue_root
chmod 0770 "$spool_root/binance-lob-recovery"
run_health
expect "recovery queue writable root: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue writable root: breach" "$(grep_out '^breach: binance-lob-recovery: recovery queue root is not an inspectable directory'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
make_recovery_queue spot
chmod 0770 "$spool_root/binance-lob-recovery/spot"
run_health
expect "recovery queue writable market: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue writable market: breach" "$(grep_out '^breach: binance-lob-recovery\[spot\]: recovery queue root is not an inspectable directory'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
make_recovery_queue spot
chmod 0740 "$spool_root/binance-lob-recovery/spot"
run_health
expect "recovery queue untraversable market: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue untraversable market: breach" "$(grep_out '^breach: binance-lob-recovery\[spot\]: recovery queue root is not an inspectable directory'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
STUB_HFT_GID=99999
make_recovery_queue_root
run_health
expect "recovery queue wrong collector group: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue wrong collector group: breach" "$(grep_out '^breach: binance-lob-recovery: recovery queue root is not an inspectable directory'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_recovery_job spot writable-dir ready
chmod 0770 "$spool_root/binance-lob-recovery/spot/writable-dir.ready"
run_health --json
expect "recovery queue writable job dir: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue writable job dir: malformed" "$(json_query '
  .checks.recovery_queue.spot.malformed_count == 1
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
write_recovery_job spot writable-receipt ready
chmod 0660 "$spool_root/binance-lob-recovery/spot/writable-receipt.ready/job.json"
run_health --json
expect "recovery queue writable receipt: exit 1" "$(rc_is 1; echo $?)"
expect "recovery queue writable receipt: malformed" "$(json_query '
  .checks.recovery_queue.spot.malformed_count == 1
'; echo $?)"

reset_env
reset_state
healthy_scenario
healthy_fixtures
make_recovery_queue_root
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
