#!/bin/sh
#
# data-completeness-check.sh - read-only hourly OSS partition reconciliation
# for the Monday public-data lake (bucket monday-lob-apne1-1045353359, Aliyun
# Tokyo ap-northeast-1).
#
# Guards against a silent recurrence of the gaps found by the 2026-08-14
# seven-day OSS audit (#882): a ~36-hour USD-M hole, scattered single-hour
# Bybit options losses, and Polymarket data that stopped ~12 hours before the
# reported outage. The collectors already emit per-segment evidence
# (manifests, _SUCCESS markers, upload-status.json); what was missing is an
# EXPECTED vs ACTUAL comparison of hour partitions.
#
# Contract: for every governed dataset, for each of the last
# COMPLETENESS_WINDOW_DAYS days (UTC), every hour whose data should have
# landed must be present in OSS:
#   - an hour is EXPECTED once it has ended and the per-dataset grace lag has
#     passed (hour_start + 3600 + grace*3600 <= now). The current hour is
#     never expected (still collecting); with the default grace of 1 the
#     previous hour may still be in flight.
#   - a partition is PRESENT when its date=/hour= prefix holds at least one
#     object.
#   - TRIPLET datasets (binance spot/usdm, polymarket) require each data
#     object (*.jsonl.zst / *.ndjson.zst) to carry both <data>.manifest.json
#     and <data>._SUCCESS; bybit options carries no _SUCCESS by design and its
#     manifest drops the .zst suffix (<data minus .zst>.manifest.json, see
#     bybit-options-archiver.rs); binance-usdm reference is batch-partitioned
#     (~8.4k objects/day) and is checked for hour presence only.
# expected_hours/present_hours/missing_partitions count expected hours only;
# latest_landed_hour/lag_seconds also consider the in-flight current hour.
# Any missing expected partition, triplet violation in an expected hour, or
# OSS listing failure is a breach: the check fails closed and exits nonzero.
#
# The script is READ-ONLY toward OSS and the host: it only lists prefixes. It
# emits one JSON report (or a human ok:/breach: summary) and runs from the
# data-completeness-check.timer once an hour.
#
# Usage: data-completeness-check.sh [--json] [--output FILE]
#   --json         emit a single JSON object to stdout (nothing else on stdout)
#   --output FILE  also write the JSON report atomically to FILE
#
# Configuration environment:
#   OSS_BUCKET                    lake bucket (default monday-lob-apne1-1045353359)
#   OSS_ENDPOINT                  OSS endpoint (default the internal Tokyo one)
#   OSS_REGION                    OSS region (default ap-northeast-1)
#   ALIYUN_PROFILE                aliyun CLI profile (default ecs-role)
#   COMPLETENESS_WINDOW_DAYS      trailing days to reconcile (default 2)
#   COMPLETENESS_GRACE_HOURS      default grace lag in hours (default 1)
#   COMPLETENESS_GRACE_HOURS_{SPOT,USDM,BYBIT,POLYMARKET,REFERENCE}
#                                 per-dataset grace override
#   COMPLETENESS_START_EPOCH_USDM
#                                 optional UTC epoch when the LOB-only USD-M
#                                 dataset became authoritative; when omitted,
#                                 the first landed hour in the window is used
#                                 as a fail-closed activation boundary
#   MONDAY_COMPLETENESS_NOW_EPOCH overrides "now" (epoch seconds); reserved
#                                 for the offline contract test
set -u

TAG=data-completeness-check

OSS_BUCKET=${OSS_BUCKET:-monday-lob-apne1-1045353359}
OSS_ENDPOINT=${OSS_ENDPOINT:-oss-ap-northeast-1-internal.aliyuncs.com}
OSS_REGION=${OSS_REGION:-ap-northeast-1}
ALIYUN_PROFILE=${ALIYUN_PROFILE:-ecs-role}
WINDOW_DAYS=${COMPLETENESS_WINDOW_DAYS:-2}
DEFAULT_GRACE=${COMPLETENESS_GRACE_HOURS:-1}
GRACE_SPOT=${COMPLETENESS_GRACE_HOURS_SPOT:-$DEFAULT_GRACE}
GRACE_USDM=${COMPLETENESS_GRACE_HOURS_USDM:-$DEFAULT_GRACE}
GRACE_BYBIT=${COMPLETENESS_GRACE_HOURS_BYBIT:-$DEFAULT_GRACE}
GRACE_POLYMARKET=${COMPLETENESS_GRACE_HOURS_POLYMARKET:-$DEFAULT_GRACE}
GRACE_REFERENCE=${COMPLETENESS_GRACE_HOURS_REFERENCE:-$DEFAULT_GRACE}
START_EPOCH_USDM=${COMPLETENESS_START_EPOCH_USDM:-}

JSON_MODE=0
OUTPUT_FILE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --json) JSON_MODE=1 ;;
    --output)
      shift
      if [ $# -eq 0 ]; then
        printf 'usage: %s [--json] [--output FILE]\n' "$0" >&2
        exit 2
      fi
      OUTPUT_FILE=$1
      ;;
    *)
      printf 'usage: %s [--json] [--output FILE]\n' "$0" >&2
      exit 2
      ;;
  esac
  shift
done

if ! command -v jq >/dev/null 2>&1; then
  logger -t "$TAG" -p daemon.err -- 'jq is required but not installed' 2>/dev/null || true
  printf 'ok:false\nbreach: jq is required but not installed\n' >&2
  exit 1
fi

for value in "$WINDOW_DAYS" "$GRACE_SPOT" "$GRACE_USDM" "$GRACE_BYBIT" \
  "$GRACE_POLYMARKET" "$GRACE_REFERENCE"; do
  case "$value" in
    *[!0-9]* | '')
      printf 'ok:false\nbreach: non-numeric window/grace configuration: %s\n' "$value" >&2
      exit 2
      ;;
  esac
done
if [ "$WINDOW_DAYS" -lt 1 ]; then
  printf 'ok:false\nbreach: COMPLETENESS_WINDOW_DAYS must be >= 1\n' >&2
  exit 2
fi

NOW_SEC=${MONDAY_COMPLETENESS_NOW_EPOCH:-$(date +%s)}
case "$NOW_SEC" in
  *[!0-9]* | '')
    printf 'ok:false\nbreach: invalid MONDAY_COMPLETENESS_NOW_EPOCH\n' >&2
    exit 2
    ;;
esac
CURRENT_HOUR_START=$((NOW_SEC - NOW_SEC % 3600))
case "$START_EPOCH_USDM" in
  *[!0-9]*)
    printf 'ok:false\nbreach: invalid COMPLETENESS_START_EPOCH_USDM\n' >&2
    exit 2
    ;;
esac
if [ -n "$START_EPOCH_USDM" ] && [ "$START_EPOCH_USDM" -gt "$NOW_SEC" ]; then
  printf 'ok:false\nbreach: COMPLETENESS_START_EPOCH_USDM is in the future\n' >&2
  exit 2
fi

log() {
  logger -t "$TAG" -p "daemon.$1" -- "$2" 2>/dev/null || true
}

utc_fmt() {
  # Portable epoch -> UTC rendering: GNU date on the host, BSD date under the
  # macOS test stubs (same pattern as file_mtime in monday-collector-health.sh).
  date -u -d "@$1" "$2" 2>/dev/null || date -u -r "$1" "$2"
}

CHECKED_AT=$(utc_fmt "$NOW_SEC" +%Y-%m-%dT%H:%M:%SZ)

TMP_DIR=$(mktemp -d "${TMPDIR:-/tmp}/monday-data-completeness.XXXXXX") || exit 2
trap 'rm -rf "$TMP_DIR"' EXIT

breach_count=0
breaches=""
datasets_json='{}'

record_breach() {
  msg=$1
  breach_count=$((breach_count + 1))
  if [ -n "$breaches" ]; then
    breaches="$breaches
$msg"
  else
    breaches=$msg
  fi
  log err "$msg"
}

list_oss() {
  # $1 = prefix URI, $2... = extra ossutil ls flags. Prints the short-format
  # listing (one oss:// URI per line); nonzero exit on any OSS failure.
  uri=$1
  shift
  aliyun ossutil ls "$uri" "$@" --short-format \
    --profile "$ALIYUN_PROFILE" --endpoint "$OSS_ENDPOINT" --region "$OSS_REGION"
}

# Listing parser: one awk pass over the concatenated short-format listing of a
# dataset. Emits "H <date> <hour>" for every observed hour partition and, for
# triplet/manifest datasets, "V <date> <hour> <base> <reason>" for every data
# object whose companions are incomplete. The partition key (date=/hour=) is
# positional in the governed lake layout, so fields are matched by name, and
# both oss://bucket/key lines and bare key tokens are accepted (ossutil
# --short-format output varies by version; see manifest_uris in
# host-rust-lob-shadow-gate.sh).
parse_listing() {
  # $1 = listing file, $2 = mode (triplet|manifest|presence)
  awk -v mode="$2" '
    BEGIN { FS = "/" }
    {
      sub(/\r$/, "")
      start = 1
      if ($1 == "oss:") start = 4
      date = ""; hour = ""; tail = ""; in_hour = 0
      for (i = start; i <= NF; i++) {
        if (in_hour == 0) {
          if ($i ~ /^date=[0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]$/) date = substr($i, 6)
          else if ($i ~ /^hour=[0-9][0-9]$/) { hour = substr($i, 6); in_hour = 1 }
        } else if ($i != "") {
          tail = (tail == "") ? $i : tail "/" $i
        }
      }
      if (date == "" || hour == "") next
      print "H", date, hour
      if (mode == "presence" || tail == "") next
      base = tail; kind = ""
      if (tail ~ /\.manifest\.json$/) { kind = "manifest"; base = substr(tail, 1, length(tail) - 14) }
      else if (tail ~ /\._SUCCESS$/) { kind = "success"; base = substr(tail, 1, length(tail) - 9) }
      else if (tail ~ /\.zst$/) kind = "data"
      else next
      key = date SUBSEP hour SUBSEP base
      if (kind == "data") data[key] = 1
      else if (kind == "manifest") manifest[key] = 1
      else success[key] = 1
    }
    END {
      for (k in data) {
        split(k, p, SUBSEP)
        # The manifest is <data>.manifest.json for the triplet datasets and
        # <data minus .zst>.manifest.json for bybit options.
        alt = p[3]; sub(/\.zst$/, "", alt)
        alt_key = p[1] SUBSEP p[2] SUBSEP alt
        if (!((k) in manifest) && !((alt_key) in manifest))
          print "V", p[1], p[2], p[3], "missing manifest"
        if (mode == "triplet" && !((k) in success))
          print "V", p[1], p[2], p[3], "missing _SUCCESS"
      }
    }
  ' "$1"
}

# Window hours (UTC, descending) covered by the trailing WINDOW_DAYS*24 hours:
# "<hour_start_epoch> <date> <hour>" per line, shared by all datasets.
WINDOW_HOURS=$((WINDOW_DAYS * 24))
HOURS_FILE="$TMP_DIR/window-hours"
: >"$HOURS_FILE"
DATES=""
i=0
while [ "$i" -lt "$WINDOW_HOURS" ]; do
  hour_start=$((CURRENT_HOUR_START - i * 3600))
  day=$(utc_fmt "$hour_start" +%Y-%m-%d)
  hh=$(utc_fmt "$hour_start" +%H)
  printf '%s %s %s\n' "$hour_start" "$day" "$hh" >>"$HOURS_FILE"
  case " $DATES " in
    *" $day "*) ;;
    *) DATES="$DATES $day" ;;
  esac
  i=$((i + 1))
done

check_dataset() {
  # $1 = label, $2 = lake prefix (ends with /), $3 = mode, $4 = grace hours,
  # $5 = optional activation epoch (only used by the USD-M LOB dataset)
  label=$1
  prefix=$2
  mode=$3
  grace=$4
  start_epoch=${5:-}
  start_source=none
  listing_file="$TMP_DIR/$label.listing"
  parsed_file="$TMP_DIR/$label.parsed"
  present_file="$TMP_DIR/$label.present"
  violations_file="$TMP_DIR/$label.violations"
  : >"$listing_file"

  listing_failed=0
  for day in $DATES; do
    uri="oss://$OSS_BUCKET/${prefix}date=$day/"
    if [ "$mode" = presence ]; then
      # Batch-partitioned dataset: a directory-only listing of the date prefix
      # is enough to prove hour presence (~8.4k objects/day otherwise).
      if ! day_listing=$(list_oss "$uri" -d); then
        listing_failed=1
        record_breach "$label: OSS listing failed for date=$day"
        continue
      fi
    else
      if ! day_listing=$(list_oss "$uri" --recursive); then
        listing_failed=1
        record_breach "$label: OSS listing failed for date=$day"
        continue
      fi
    fi
    printf '%s\n' "$day_listing" >>"$listing_file"
  done

  parse_listing "$listing_file" "$mode" >"$parsed_file"
  grep '^H ' "$parsed_file" | sort -u >"$present_file" || true
  grep '^V ' "$parsed_file" >"$violations_file" || true

  # The USD-M dataset identity changed from the full tape to the Top-100
  # LOB-only prefix.  Do not call hours before that identity first landed
  # "missing"; if an operator knows the cutover epoch it can be pinned
  # explicitly, otherwise the earliest observed hour is the conservative
  # activation boundary.  A window with no landed hour is still a breach so a
  # total outage cannot silently become a green pre-launch window.
  if [ "$label" = binance-usdm ]; then
    if [ -n "$start_epoch" ]; then
      start_epoch=$((start_epoch - start_epoch % 3600))
      start_source=configured
    else
      first_landed_hour_start=""
      while read -r hour_start day hh; do
        if grep -Fqx "H $day $hh" "$present_file" \
          && { [ -z "$first_landed_hour_start" ] || [ "$hour_start" -lt "$first_landed_hour_start" ]; }; then
          first_landed_hour_start=$hour_start
        fi
      done <"$HOURS_FILE"
      if [ -n "$first_landed_hour_start" ]; then
        start_epoch=$first_landed_hour_start
        start_source=inferred_first_landed_hour
      else
        record_breach "$label: no landed hour establishes the dataset activation boundary"
        start_epoch=$((NOW_SEC + 3600))
        start_source=missing
      fi
    fi
  fi
  [ -n "$start_epoch" ] || start_epoch=0

  expected_count=0
  present_count=0
  missing=""
  latest_hour_start=""
  latest_hour_iso=""
  while read -r hour_start day hh; do
    if grep -Fqx "H $day $hh" "$present_file"; then
      if [ -z "$latest_hour_start" ]; then
        latest_hour_start=$hour_start
        latest_hour_iso=$(utc_fmt "$hour_start" +%Y-%m-%dT%H:00:00Z)
      fi
      if [ "$hour_start" -ge "$start_epoch" ] \
        && [ $((hour_start + 3600 + grace * 3600)) -le "$NOW_SEC" ]; then
        expected_count=$((expected_count + 1))
        present_count=$((present_count + 1))
      fi
    elif [ "$hour_start" -ge "$start_epoch" ] \
      && [ $((hour_start + 3600 + grace * 3600)) -le "$NOW_SEC" ]; then
      expected_count=$((expected_count + 1))
      missing="$missing date=$day/hour=$hh"
    fi
  done <"$HOURS_FILE"

  # Triplet publication is intentionally non-atomic across data, manifest and
  # _SUCCESS.  Ignore incomplete objects in current/in-grace hours, just as we
  # ignore missing partitions there; only settled expected hours may breach.
  eligible_violations_file="$TMP_DIR/$label.eligible-violations"
  awk -v now="$NOW_SEC" -v grace="$grace" -v start="$start_epoch" '
    NR == FNR { hour[$2 SUBSEP $3] = $1; next }
    {
      h = hour[$2 SUBSEP $3]
      if (h != "" && h >= start && h + 3600 + grace * 3600 <= now) print
    }
  ' "$HOURS_FILE" "$violations_file" >"$eligible_violations_file"
  mv "$eligible_violations_file" "$violations_file"
  violation_count=$(wc -l <"$violations_file" | tr -d ' ')

  if [ -n "$missing" ]; then
    # Word splitting of $missing is intended (whitespace-separated partitions).
    # shellcheck disable=SC2086
    missing_count=$(set -- $missing; printf '%s' "$#")
    record_breach "$label: $missing_count missing partition(s):$missing"
  fi
  if [ "$violation_count" -gt 0 ]; then
    record_breach "$label: $violation_count triplet violation(s)"
  fi

  if [ -n "$latest_hour_start" ]; then
    lag=$((NOW_SEC - latest_hour_start - 3600))
    [ "$lag" -lt 0 ] && lag=0
    latest_json=$(jq -n --arg h "$latest_hour_iso" --argjson l "$lag" \
      '{latest_landed_hour: $h, lag_seconds: $l}')
  else
    latest_json='{"latest_landed_hour": null, "lag_seconds": null}'
  fi

  # Word splitting of $missing is intended (whitespace-separated partitions).
  # shellcheck disable=SC2086
  missing_json=$(printf '%s\n' $missing | jq -Rsc 'split("\n") | map(select(length > 0))')
  violations_json=$(sed 's/^V \([^ ]*\) \([^ ]*\) /date=\1\/hour=\2 /' "$violations_file" \
    | jq -Rsc 'split("\n") | map(select(length > 0))')
  if [ -n "$start_epoch" ] && [ "$start_epoch" -le "$NOW_SEC" ]; then
    start_epoch_json=$start_epoch
  else
    start_epoch_json=null
  fi
  dobj=$(jq -n --arg prefix "$prefix" --arg mode "$mode" \
    --argjson grace "$grace" --argjson expected "$expected_count" \
    --argjson present "$present_count" --argjson missing "$missing_json" \
    --argjson violations "$violations_json" --argjson latest "$latest_json" \
    --argjson listing_failed "$listing_failed" --arg start_source "$start_source" \
    --argjson start_epoch "$start_epoch_json" \
    '{prefix: $prefix, mode: $mode, grace_hours: $grace,
      expected_hours: $expected, present_hours: $present,
      missing_partitions: $missing, triplet_violations: $violations,
      latest_landed_hour: $latest.latest_landed_hour,
      lag_seconds: $latest.lag_seconds,
      activation_start_epoch: $start_epoch,
      activation_start_source: $start_source,
      listing_failed: ($listing_failed == 1)}')
  datasets_json=$(jq -n --argjson base "$datasets_json" --arg k "$label" --argjson v "$dobj" \
    '$base + {($k): $v}')
}

check_dataset binance-spot \
  'lake/raw/venue=binance/market=spot/dataset=spot_all/shard=all/' \
  triplet "$GRACE_SPOT" ""
check_dataset binance-usdm \
  'lake/raw/venue=binance/market=usdm/dataset=usdm_perpetual_top100_lob/shard=all/' \
  triplet "$GRACE_USDM" "$START_EPOCH_USDM"
check_dataset bybit-options \
  'lake/raw/venue=bybit/market=option/dataset=options_quotes/' \
  manifest "$GRACE_BYBIT" ""
check_dataset polymarket-crypto-expiry \
  'lake/raw/venue=polymarket/dataset=crypto_expiry/' \
  triplet "$GRACE_POLYMARKET" ""
check_dataset binance-usdm-reference \
  'lake/raw/venue=binance_usdm/dataset=reference/' \
  presence "$GRACE_REFERENCE" ""

emit_report() {
  jq -n --argjson ok "$1" --arg checked "$CHECKED_AT" \
    --argjson now "$NOW_SEC" --argjson window_days "$WINDOW_DAYS" \
    --argjson datasets "$datasets_json" \
    '{ok: $ok, checked_at: $checked, now_epoch: $now, window_days: $window_days, datasets: $datasets}'
}

if [ "$breach_count" -gt 0 ]; then
  ok_str=false
else
  ok_str=true
fi

report_json=$(emit_report "$ok_str")

if [ -n "$OUTPUT_FILE" ]; then
  # A report-persistence failure means the timer run leaves stale health
  # evidence while the check itself may be green, so fail closed.
  out_tmp="$OUTPUT_FILE.$$"
  if printf '%s\n' "$report_json" >"$out_tmp" 2>/dev/null && mv "$out_tmp" "$OUTPUT_FILE" 2>/dev/null; then
    :
  else
    rm -f "$out_tmp" 2>/dev/null
    record_breach "state: cannot persist report file $OUTPUT_FILE"
    ok_str=false
    report_json=$(emit_report "$ok_str")
  fi
fi

if [ "$JSON_MODE" -eq 1 ]; then
  printf '%s\n' "$report_json"
else
  if [ "$breach_count" -gt 0 ]; then
    printf 'ok:false\n'
    printf '%s\n' "$breaches" | sed 's/^/breach: /'
  else
    printf 'ok:true\n'
  fi
fi

if [ "$breach_count" -gt 0 ]; then
  log err "$breach_count data-completeness breach(es) detected"
  exit 1
fi
log info "all data-completeness checks passed"
exit 0
