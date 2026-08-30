#!/usr/bin/env bash
#
# Contract tests for data-completeness-check.sh. Runs the check against a /tmp
# fixture lake with a stubbed `aliyun ossutil ls` backed by a plain directory
# tree, so the suite runs fully offline (same stub pattern as
# test-polymarket-raw-ops-control-plane.sh).
#
# Contract under test (#882): for each governed dataset, every hour partition
# that should have landed (hour ended + per-dataset grace lag) must be present
# in OSS; triplet datasets must carry data + .manifest.json + ._SUCCESS (bybit
# options: data + manifest only, no _SUCCESS by design; binance-usdm reference
# is batch-partitioned and checked for hour presence only). The current hour
# and the in-grace previous hour may be absent. Any missing partition, triplet
# violation, or OSS listing failure fails closed with a nonzero exit.
#
# Usage: ./test-data-completeness-check.sh
set -euo pipefail

script_dir=$(cd -- "$(dirname -- "$0")" && pwd)
check_script="$script_dir/data-completeness-check.sh"
test_root=$(mktemp -d /tmp/monday-data-completeness-test.XXXXXX)
trap 'rm -rf "$test_root"' EXIT

stub_dir="$test_root/bin"
fake_oss="$test_root/oss"
out_file="$test_root/out"
err_file="$test_root/err"
call_log="$test_root/aliyun-calls.log"
mkdir -p "$stub_dir" "$fake_oss"

# Fixed "now": 2026-08-15T10:30:00Z. The 2-day window then covers the 48 hour
# partitions 2026-08-13T10:00Z .. 2026-08-15T09:00Z... (window start is the
# current hour minus 47h) and every hour up to 2026-08-15T08:00Z is expected
# with the default 1-hour grace: 46 expected hours per dataset.
NOW_ISO=2026-08-15T10:30:00Z
NOW_EPOCH=$(date -u -d "$NOW_ISO" +%s 2>/dev/null \
  || date -u -j -f "%Y-%m-%dT%H:%M:%SZ" "$NOW_ISO" +%s)
EXPECTED_FULL_WINDOW=46
EXPECTED_ONE_DAY=22

BUCKET=monday-lob-apne1-1045353359
FAKE_SHA=0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef

P_SPOT='lake/raw/venue=binance/market=spot/dataset=spot_all/shard=all'
P_USDM='lake/raw/venue=binance/market=usdm/dataset=usdm_perpetual_top100_lob/shard=all'
P_BYBIT='lake/raw/venue=bybit/market=option/dataset=options_quotes'
P_POLY='lake/raw/venue=polymarket/dataset=crypto_expiry'
P_REF='lake/raw/venue=binance_usdm/dataset=reference'

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

rc_is() { [ "$RC" -eq "$1" ]; }
grep_out() { printf '%s' "$OUT" | grep -q "$1"; }
grep_not_out() { ! printf '%s' "$OUT" | grep -q "$1"; }
json_query() { printf '%s' "$OUT" | jq -e "$1" >/dev/null 2>&1; }

# Stubbed aliyun CLI: only `aliyun ossutil ls <uri> [flags]` is supported,
# backed by the fixture tree under FAKE_OSS_ROOT. --recursive lists every
# object below the prefix; -d lists immediate subdirectories (hour= partitions)
# with a trailing slash, mirroring ossutil --short-format output.
cat >"$stub_dir/aliyun" <<'EOF'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$STUB_CALL_LOG"
[[ $1 == ossutil ]] || exit 2
[[ $2 == ls ]] || exit 2
uri=$3
shift 3
recursive=0
for arg in "$@"; do
  [[ $arg == --recursive ]] && recursive=1
done
[[ $uri == oss://"$STUB_OSS_BUCKET"/* ]] || exit 2
if [[ -n ${STUB_ALIYUN_FAIL_MATCH:-} && $uri == *"$STUB_ALIYUN_FAIL_MATCH"* ]]; then
  exit 3
fi
key=${uri#oss://"$STUB_OSS_BUCKET"/}
if [[ ! -d $FAKE_OSS_ROOT/$key ]]; then
  exit 0
fi
if [[ $recursive == 1 ]]; then
  (cd "$FAKE_OSS_ROOT" && find "$key" -type f | sort | sed "s|^|oss://$STUB_OSS_BUCKET/|")
else
  (cd "$FAKE_OSS_ROOT" && find "$key" -mindepth 1 -maxdepth 1 -type d | sort \
    | sed "s|^|oss://$STUB_OSS_BUCKET/|;s|\$|/|")
fi
EOF
chmod +x "$stub_dir/aliyun"

run_check() {
  set +e
  env \
    STUB_OSS_BUCKET="$BUCKET" \
    STUB_CALL_LOG="$call_log" \
    STUB_ALIYUN_FAIL_MATCH="${STUB_ALIYUN_FAIL_MATCH:-}" \
    FAKE_OSS_ROOT="$fake_oss" \
    OSS_BUCKET="$BUCKET" \
    MONDAY_COMPLETENESS_NOW_EPOCH="$NOW_EPOCH" \
    COMPLETENESS_WINDOW_DAYS="${COMPLETENESS_WINDOW_DAYS:-2}" \
    PATH="$stub_dir:$PATH" \
    "$check_script" "$@" >"$out_file" 2>"$err_file"
  rc=$?
  set -e
  RC=$rc
  OUT=$(cat "$out_file")
  if [ "$rc" -ne 0 ] && [ -s "$err_file" ]; then
    printf 'stderr: %s\n' "$(cat "$err_file")" >&2
  fi
}

reset_env() {
  STUB_ALIYUN_FAIL_MATCH=""
  unset COMPLETENESS_WINDOW_DAYS
  unset COMPLETENESS_START_EPOCH_USDM
  : >"$call_log"
}

reset_lake() {
  case "$test_root" in
    /tmp/monday-data-completeness-test.*) rm -rf "$fake_oss" ;;
    *)
      printf 'refusing to reset an unexpected completeness test root: %s\n' "$test_root" >&2
      exit 2
      ;;
  esac
  mkdir -p "$fake_oss"
}

hours_of_date() {
  # $1 = date, $2 = first hour, $3 = last hour; prints zero-padded hours
  h=$2
  while [ "$h" -le "$3" ]; do
    printf '%s %02d\n' "$1" "$h"
    h=$((h + 1))
  done
}

window_hours() {
  # All partitions a complete fixture needs: 08-13/08-14 full, 08-15 00..09
  # (09 is inside the grace lag; the current hour 10 is still collecting).
  hours_of_date 2026-08-13 0 23
  hours_of_date 2026-08-14 0 23
  hours_of_date 2026-08-15 0 9
}

make_triplet() {
  # $1 = lake prefix, $2 = date, $3 = hour, $4 = data filename (may carry a
  # sha256=<hex>/ subdirectory, as in the polymarket layout)
  dir="$fake_oss/$1/date=$2/hour=$3"
  mkdir -p "$dir/$(dirname "$4")"
  : >"$dir/$4"
  : >"$dir/$4.manifest.json"
  : >"$dir/$4._SUCCESS"
}

make_manifest_pair() {
  # bybit options: data + manifest, no _SUCCESS by design; the manifest drops
  # the .zst suffix (mirrors bybit-options-archiver.rs)
  dir="$fake_oss/$1/date=$2/hour=$3/sha256=$FAKE_SHA"
  mkdir -p "$dir"
  : >"$dir/$4"
  : >"$dir/${4%.zst}.manifest.json"
}

make_reference_hour() {
  dir="$fake_oss/$1/date=$2/hour=$3/batch=00000"
  mkdir -p "$dir"
  : >"$dir/part-0000.ndjson.zst"
}

make_complete_lake() {
  while read -r day hh; do
    make_triplet "$P_SPOT" "$day" "$hh" "segment-$day-$hh.jsonl.zst"
    make_triplet "$P_USDM" "$day" "$hh" "segment-$day-$hh.jsonl.zst"
    make_manifest_pair "$P_BYBIT" "$day" "$hh" "quotes-$day-$hh.ndjson.zst"
    make_triplet "$P_POLY" "$day" "$hh" "sha256=$FAKE_SHA/market-updates.$day-$hh.ndjson.zst"
    make_reference_hour "$P_REF" "$day" "$hh"
  done < <(window_hours)
}

remove_partition() {
  # $1 = lake prefix, $2 = date, $3 = hour
  rm -rf "$fake_oss/$1/date=$2/hour=$3"
}

# --- scenario: complete lake ------------------------------------------------
reset_env
reset_lake
make_complete_lake
run_check --json
expect 'complete lake: exit 0' "$(rc_is 0 && echo 0 || echo 1)"
expect 'complete lake: ok:true' "$(json_query '.ok == true' && echo 0 || echo 1)"
expect 'complete lake: spot expected hours' \
  "$(json_query ".datasets[\"binance-spot\"].expected_hours == $EXPECTED_FULL_WINDOW" && echo 0 || echo 1)"
expect 'complete lake: spot present hours' \
  "$(json_query ".datasets[\"binance-spot\"].present_hours == $EXPECTED_FULL_WINDOW" && echo 0 || echo 1)"
expect 'complete lake: spot no missing partitions' \
  "$(json_query '.datasets["binance-spot"].missing_partitions == []' && echo 0 || echo 1)"
expect 'complete lake: spot no triplet violations' \
  "$(json_query '.datasets["binance-spot"].triplet_violations == []' && echo 0 || echo 1)"
expect 'complete lake: latest landed hour is the grace hour' \
  "$(json_query '.datasets["binance-spot"].latest_landed_hour == "2026-08-15T09:00:00Z"' && echo 0 || echo 1)"
expect 'complete lake: lag is 1800s' \
  "$(json_query '.datasets["binance-spot"].lag_seconds == 1800' && echo 0 || echo 1)"
expect 'complete lake: bybit manifest-only accepted (no _SUCCESS by design)' \
  "$(json_query '.datasets["bybit-options"].triplet_violations == [] and .datasets["bybit-options"].present_hours == 46' && echo 0 || echo 1)"
expect 'complete lake: reference presence-only accepted' \
  "$(json_query '.datasets["binance-usdm-reference"].present_hours == 46' && echo 0 || echo 1)"
expect 'complete lake: reference listed with -d (batch-partitioned)' \
  "$(grep -q 'dataset=reference.* -d ' "$call_log" && echo 0 || echo 1)"
expect 'complete lake: reference not listed recursively' \
  "$(! grep -q 'dataset=reference.*--recursive' "$call_log" && echo 0 || echo 1)"

# --- scenario: in-flight triplets stay inside the hour grace ----------------
reset_env
reset_lake
make_complete_lake
make_triplet "$P_SPOT" 2026-08-15 10 "segment-current.jsonl.zst"
rm "$fake_oss/$P_SPOT/date=2026-08-15/hour=10/segment-current.jsonl.zst._SUCCESS"
rm "$fake_oss/$P_SPOT/date=2026-08-15/hour=09/segment-2026-08-15-09.jsonl.zst.manifest.json"
run_check --json
expect 'in-flight triplets: exit 0' "$(rc_is 0 && echo 0 || echo 1)"
expect 'in-flight triplets: current and in-grace violations excluded' \
  "$(json_query '.datasets["binance-spot"].triplet_violations == []' && echo 0 || echo 1)"

# --- scenario: missing hour partition ---------------------------------------
reset_env
reset_lake
make_complete_lake
remove_partition "$P_SPOT" 2026-08-14 07
run_check --json
expect 'missing hour: exit 1' "$(rc_is 1 && echo 0 || echo 1)"
expect 'missing hour: ok:false' "$(json_query '.ok == false' && echo 0 || echo 1)"
expect 'missing hour: partition reported' \
  "$(json_query '.datasets["binance-spot"].missing_partitions == ["date=2026-08-14/hour=07"]' && echo 0 || echo 1)"
expect 'missing hour: present count drops' \
  "$(json_query '.datasets["binance-spot"].present_hours == 45' && echo 0 || echo 1)"
expect 'missing hour: other datasets unaffected' \
  "$(json_query '.datasets["binance-usdm"].missing_partitions == []' && echo 0 || echo 1)"

# --- scenario: triplet violations -------------------------------------------
reset_env
reset_lake
make_complete_lake
rm "$fake_oss/$P_USDM/date=2026-08-14/hour=03/segment-2026-08-14-03.jsonl.zst._SUCCESS"
rm "$fake_oss/$P_POLY/date=2026-08-13/hour=22/sha256=$FAKE_SHA/market-updates.2026-08-13-22.ndjson.zst.manifest.json"
run_check --json
expect 'triplet violation: exit 1' "$(rc_is 1 && echo 0 || echo 1)"
expect 'triplet violation: usdm missing _SUCCESS reported' \
  "$(json_query '.datasets["binance-usdm"].triplet_violations | length == 1' && echo 0 || echo 1)"
expect 'triplet violation: usdm violation names the segment' \
  "$(json_query '.datasets["binance-usdm"].triplet_violations[0] | contains("date=2026-08-14/hour=03") and contains("missing _SUCCESS")' && echo 0 || echo 1)"
expect 'triplet violation: polymarket missing manifest reported' \
  "$(json_query '.datasets["polymarket-crypto-expiry"].triplet_violations[0] | contains("date=2026-08-13/hour=22") and contains("missing manifest")' && echo 0 || echo 1)"
expect 'triplet violation: partition still counts as present' \
  "$(json_query '.datasets["binance-usdm"].missing_partitions == []' && echo 0 || echo 1)"

# --- scenario: bybit manifest violation (no _SUCCESS required) ---------------
reset_env
reset_lake
make_complete_lake
rm "$fake_oss/$P_BYBIT/date=2026-08-14/hour=11/sha256=$FAKE_SHA/quotes-2026-08-14-11.ndjson.manifest.json"
run_check --json
expect 'bybit violation: exit 1' "$(rc_is 1 && echo 0 || echo 1)"
expect 'bybit violation: missing manifest reported' \
  "$(json_query '.datasets["bybit-options"].triplet_violations[0] | contains("missing manifest")' && echo 0 || echo 1)"
expect 'bybit violation: no _SUCCESS demand' \
  "$(json_query '[.datasets["bybit-options"].triplet_violations[] | contains("missing _SUCCESS")] | any | not' && echo 0 || echo 1)"

# --- scenario: grace lag ----------------------------------------------------
reset_env
reset_lake
make_complete_lake
# Current hour (10) and previous hour (09, inside grace) may be absent.
remove_partition "$P_SPOT" 2026-08-15 09
run_check --json
expect 'grace: absent in-grace hour tolerated' \
  "$(rc_is 0 && json_query '.ok == true' && echo 0 || echo 1)"
# The hour before the grace boundary is expected and its absence breaches.
remove_partition "$P_SPOT" 2026-08-15 08
run_check --json
expect 'grace: absent post-grace hour breaches' \
  "$(rc_is 1 && json_query '.datasets["binance-spot"].missing_partitions == ["date=2026-08-15/hour=08"]' && echo 0 || echo 1)"

# --- USD-M activation boundary ---------------------------------------------
reset_env
reset_lake
make_complete_lake
rm -rf "$fake_oss/$P_USDM/date=2026-08-13" "$fake_oss/$P_USDM/date=2026-08-14"
run_check --json
expect 'usdm pre-launch hours are excluded from the inferred activation boundary' \
  "$(rc_is 0 && json_query '.datasets["binance-usdm"].expected_hours == 9 and .datasets["binance-usdm"].present_hours == 9 and .datasets["binance-usdm"].activation_start_source == "inferred_first_landed_hour"' && echo 0 || echo 1)"

# --- scenario: reference hour presence --------------------------------------
reset_env
reset_lake
make_complete_lake
remove_partition "$P_REF" 2026-08-14 03
run_check --json
expect 'reference: missing hour breaches' \
  "$(rc_is 1 && json_query '.datasets["binance-usdm-reference"].missing_partitions == ["date=2026-08-14/hour=03"]' && echo 0 || echo 1)"

# --- scenario: OSS listing failure fails closed ------------------------------
reset_env
reset_lake
make_complete_lake
STUB_ALIYUN_FAIL_MATCH='dataset=spot_all'
run_check --json
expect 'listing failure: exit 1' "$(rc_is 1 && echo 0 || echo 1)"
expect 'listing failure: listing_failed flag' \
  "$(json_query '.datasets["binance-spot"].listing_failed == true' && echo 0 || echo 1)"
expect 'listing failure: healthy datasets stay green' \
  "$(json_query '.datasets["binance-usdm"].listing_failed == false and .datasets["binance-usdm"].missing_partitions == []' && echo 0 || echo 1)"

# --- scenario: empty lake ----------------------------------------------------
reset_env
reset_lake
run_check --json
expect 'empty lake: exit 1' "$(rc_is 1 && echo 0 || echo 1)"
expect 'empty lake: all hours missing' \
  "$(json_query ".datasets[\"binance-spot\"].missing_partitions | length == $EXPECTED_FULL_WINDOW" && echo 0 || echo 1)"
expect 'empty lake: latest landed hour null' \
  "$(json_query '.datasets["binance-spot"].latest_landed_hour == null and .datasets["binance-spot"].lag_seconds == null' && echo 0 || echo 1)"

# --- scenario: configurable window -------------------------------------------
reset_env
reset_lake
make_complete_lake
COMPLETENESS_WINDOW_DAYS=1
run_check --json
expect 'window=1: exit 0' "$(rc_is 0 && echo 0 || echo 1)"
expect 'window=1: expected hours shrink' \
  "$(json_query ".datasets[\"binance-spot\"].expected_hours == $EXPECTED_ONE_DAY" && echo 0 || echo 1)"

# --- scenario: --output report file ------------------------------------------
reset_env
reset_lake
make_complete_lake
report_file="$test_root/report.json"
run_check --json --output "$report_file"
expect 'output file: exit 0' "$(rc_is 0 && echo 0 || echo 1)"
expect 'output file: report persisted' \
  "$(jq -e '.ok == true and .datasets["binance-spot"].expected_hours == 46' "$report_file" >/dev/null 2>&1 && echo 0 || echo 1)"

# --- scenario: text mode ------------------------------------------------------
reset_env
reset_lake
make_complete_lake
remove_partition "$P_BYBIT" 2026-08-13 15
run_check
expect 'text mode: ok:false line' "$(grep_out '^ok:false$' && echo 0 || echo 1)"
expect 'text mode: breach line names the dataset' \
  "$(grep_out '^breach: bybit-options: 1 missing partition(s): date=2026-08-13/hour=15$' && echo 0 || echo 1)"
reset_env
reset_lake
make_complete_lake
run_check
expect 'text mode: ok:true line' "$(rc_is 0 && grep_out '^ok:true$' && echo 0 || echo 1)"

# --- scenario: usage ----------------------------------------------------------
run_check --bogus
expect 'usage: unknown flag exits 2' "$(rc_is 2 && echo 0 || echo 1)"
run_check --output
expect 'usage: --output without value exits 2' "$(rc_is 2 && echo 0 || echo 1)"

printf '\n%d passed, %d failed\n' "$pass_count" "$fail_count"
[ "$fail_count" -eq 0 ]
