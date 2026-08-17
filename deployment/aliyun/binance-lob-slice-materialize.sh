#!/bin/sh
#
# binance-lob-slice-materialize.sh - batch driver that walks the production
# Binance LOB lake on OSS (bucket monday-lob-apne1-1045353359), downloads raw
# market-tape segments, slices oversized all-market segments into verified
# symbol-subset segments (binance-market-tape-slicer), and materializes them
# into content-addressed canonical replay parquet for apps/backtest
# (binance-replay-parquet-materializer).
#
# Why: production spot_all/usdm_all hour segments can decompress past the
# 2 GiB market-tape seal bound, so the materializer cannot consume them
# directly. The slicer rewrites one digest-verified segment into compliant
# slices without weakening any verification gate; this driver only orchestrates
# enumerate -> download -> slice -> materialize and records the evidence.
#
# Lake layout (governed):
#   lake/raw/venue=<venue>/market=<market>/dataset=<dataset>/shard=<shard>/date=<YYYY-MM-DD>/hour=<HH>/part-*.jsonl.zst
# with <data>.manifest.json and <data>._SUCCESS companions.
#
# Resume contract: all state lives under WORK_DIR/state/. A segment whose
# requested symbols all have <stem>.<SYMBOL>.ok records is skipped entirely; a
# segment with <stem>.sliced.ok reuses its recorded slice report instead of
# re-slicing. The materializer publishes immutable content-addressed
# artifacts, so re-running any step is safe. The run manifest
# ARTIFACT_DIR/slice-materialization-run.json is rebuilt from the ok records
# on every run. Exit status is nonzero if any segment or symbol failed.
#
# Usage:
#   binance-lob-slice-materialize.sh --market spot --start-date 2026-08-16 \
#     --end-date 2026-08-16 --symbols BTCUSDT,ETHUSDT \
#     --work-dir /data/lob-work --artifact-dir /data/lob-canonical
#     [--dataset spot_all] [--shard all] [--hours 00,01,...] \
#     [--max-slice-bytes 1500000000]
#
# Configuration environment:
#   OSS_BUCKET        lake bucket (default monday-lob-apne1-1045353359)
#   OSS_ENDPOINT      OSS endpoint (default the internal Tokyo one)
#   OSS_REGION        OSS region (default ap-northeast-1)
#   ALIYUN_PROFILE    aliyun CLI profile (default ecs-role)
#   SLICER_BIN        binance-market-tape-slicer path (default repo target/debug)
#   MATERIALIZER_BIN  binance-replay-parquet-materializer path (default repo target/debug)
#   CLEANUP_RAW       remove downloaded raw segments once fully materialized (default 0)
set -u

TAG=binance-lob-slice-materialize

OSS_BUCKET=${OSS_BUCKET:-monday-lob-apne1-1045353359}
OSS_ENDPOINT=${OSS_ENDPOINT:-oss-ap-northeast-1-internal.aliyuncs.com}
OSS_REGION=${OSS_REGION:-ap-northeast-1}
ALIYUN_PROFILE=${ALIYUN_PROFILE:-ecs-role}
SCRIPT_DIR=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
REPO_RUST=$(CDPATH='' cd -- "$SCRIPT_DIR/../../rust_hft" && pwd)
SLICER_BIN=${SLICER_BIN:-$REPO_RUST/target/debug/binance-market-tape-slicer}
MATERIALIZER_BIN=${MATERIALIZER_BIN:-$REPO_RUST/target/debug/binance-replay-parquet-materializer}
CLEANUP_RAW=${CLEANUP_RAW:-0}

MARKET=""
DATASET=""
SHARD=all
VENUE=binance
START_DATE=""
END_DATE=""
SYMBOLS=""
HOURS=""
WORK_DIR=""
ARTIFACT_DIR=""
MAX_SLICE_BYTES=""

usage() {
  printf 'usage: %s --market spot|usdm --start-date YYYY-MM-DD --end-date YYYY-MM-DD --symbols SYM[,SYM...] --work-dir DIR --artifact-dir DIR [--dataset NAME] [--shard NAME] [--hours HH[,HH...]] [--max-slice-bytes N]\n' "$0" >&2
  exit 2
}

while [ $# -gt 0 ]; do
  case "$1" in
    --market) shift; MARKET=${1:-} ;;
    --dataset) shift; DATASET=${1:-} ;;
    --shard) shift; SHARD=${1:-} ;;
    --start-date) shift; START_DATE=${1:-} ;;
    --end-date) shift; END_DATE=${1:-} ;;
    --symbols) shift; SYMBOLS=${1:-} ;;
    --hours) shift; HOURS=${1:-} ;;
    --work-dir) shift; WORK_DIR=${1:-} ;;
    --artifact-dir) shift; ARTIFACT_DIR=${1:-} ;;
    --max-slice-bytes) shift; MAX_SLICE_BYTES=${1:-} ;;
    *) usage ;;
  esac
  shift
done

if [ -z "$MARKET" ] || [ -z "$START_DATE" ] || [ -z "$END_DATE" ] || [ -z "$SYMBOLS" ] || \
  [ -z "$WORK_DIR" ] || [ -z "$ARTIFACT_DIR" ]; then
  usage
fi

case "$MARKET" in
  spot) DATASET=${DATASET:-spot_all} ;;
  usdm) DATASET=${DATASET:-usdm_all} ;;
  *) usage ;;
esac

case "$START_DATE$END_DATE" in
  *[!0-9-]*) printf 'breach: dates must be YYYY-MM-DD\n' >&2; exit 2 ;;
esac
SYMBOLS=$(printf '%s' "$SYMBOLS" | tr '[:lower:],' '[:upper:],')
case "$SYMBOLS" in
  *[!A-Z0-9,]* | '') printf 'breach: --symbols must be a comma-separated symbol list\n' >&2; exit 2 ;;
esac
case "$MAX_SLICE_BYTES" in
  *[!0-9]*) printf 'breach: --max-slice-bytes must be numeric\n' >&2; exit 2 ;;
esac

for tool in jq aliyun; do
  if ! command -v "$tool" >/dev/null 2>&1; then
    printf 'breach: %s is required but not installed\n' "$tool" >&2
    exit 1
  fi
done
for binary in "$SLICER_BIN" "$MATERIALIZER_BIN"; do
  if [ ! -x "$binary" ]; then
    printf 'breach: %s is not executable (build hft-collector first)\n' "$binary" >&2
    exit 1
  fi
done

log() {
  logger -t "$TAG" -p "daemon.$1" -- "$2" 2>/dev/null || true
  printf '%s\n' "$2" >&2
}

list_oss() {
  # $1 = prefix URI. Prints the short-format listing (one URI per line);
  # nonzero exit on any OSS failure.
  aliyun ossutil ls "$1" --short-format \
    --profile "$ALIYUN_PROFILE" --endpoint "$OSS_ENDPOINT" --region "$OSS_REGION"
}

oss_cp() {
  # $1 = object URI, $2 = local file; forced overwrite, nonzero on failure.
  aliyun ossutil cp "$1" "$2" -f \
    --profile "$ALIYUN_PROFILE" --endpoint "$OSS_ENDPOINT" --region "$OSS_REGION" >/dev/null 2>&1
}

date_epoch() {
  # $1 = YYYY-MM-DD; prints the UTC day-start epoch (GNU and BSD date).
  if date -d "$1" +%s >/dev/null 2>&1; then
    date -u -d "$1 00:00:00" +%s
  else
    TZ=UTC date -j -f "%Y-%m-%d %H:%M:%S" "$1 00:00:00" +%s
  fi
}

epoch_date() {
  # $1 = epoch seconds; prints YYYY-MM-DD (GNU and BSD date).
  if date -d "@$1" +%F >/dev/null 2>&1; then
    date -u -d "@$1" +%F
  else
    TZ=UTC date -j -f %s "$1" +%F
  fi
}

hour_selected() {
  # $1 = hour=HH token from the object key.
  [ -z "$HOURS" ] && return 0
  case ",$HOURS," in
    *",$1,"*) return 0 ;;
  esac
  return 1
}

START_EPOCH=$(date_epoch "$START_DATE") || exit 2
END_EPOCH=$(date_epoch "$END_DATE") || exit 2
if [ "$END_EPOCH" -lt "$START_EPOCH" ]; then
  printf 'breach: --end-date is before --start-date\n' >&2
  exit 2
fi

STATE_DIR=$WORK_DIR/state
RAW_DIR=$WORK_DIR/raw
SLICE_DIR=$WORK_DIR/slices
mkdir -p "$STATE_DIR" "$RAW_DIR" "$SLICE_DIR" "$ARTIFACT_DIR" || exit 1
RECORDS=$STATE_DIR/.run-records.$$.jsonl
FAILURES=$STATE_DIR/.run-failures.$$.jsonl
: >"$RECORDS"
: >"$FAILURES"
trap 'rm -f "$RECORDS" "$FAILURES"' EXIT

SLICER_ARGS=""
[ -n "$MAX_SLICE_BYTES" ] && SLICER_ARGS="--max-slice-bytes $MAX_SLICE_BYTES"

SEGMENTS_SEEN=0
SEGMENTS_SKIPPED=0
DAY=$START_EPOCH
while [ "$DAY" -le "$END_EPOCH" ]; do
  DATE=$(epoch_date "$DAY")
  DAY=$((DAY + 86400))
  PREFIX="oss://$OSS_BUCKET/lake/raw/venue=$VENUE/market=$MARKET/dataset=$DATASET/shard=$SHARD/date=$DATE/"
  LISTING=$STATE_DIR/.listing.$DATE.$$
  if ! list_oss "$PREFIX" >"$LISTING" 2>/dev/null; then
    log err "OSS listing failed for $PREFIX"
    printf '{"date":"%s","error":"oss_listing_failed"}\n' "$DATE" >>"$FAILURES"
    rm -f "$LISTING"
    continue
  fi
  # shellcheck disable=SC2013
  for URI in $(grep '\.jsonl\.zst$' "$LISTING" 2>/dev/null); do
    HOUR=$(printf '%s' "$URI" | sed -n 's|.*/hour=\([0-9][0-9]\)/.*|\1|p')
    if [ -z "$HOUR" ] || ! hour_selected "$HOUR"; then
      continue
    fi
    OBJECT=${URI##*/}
    PART=${OBJECT%.jsonl.zst}
    case "$PART" in
      '') continue ;;
    esac
    SEGMENTS_SEEN=$((SEGMENTS_SEEN + 1))

    # Resume: a segment is done once every requested symbol has an ok record.
    PENDING=""
    for SYMBOL in $(printf '%s' "$SYMBOLS" | tr ',' ' '); do
      [ -f "$STATE_DIR/$PART.$SYMBOL.ok" ] || PENDING="$PENDING $SYMBOL"
    done
    if [ -z "$PENDING" ]; then
      SEGMENTS_SKIPPED=$((SEGMENTS_SKIPPED + 1))
      for SYMBOL in $(printf '%s' "$SYMBOLS" | tr ',' ' '); do
        cat "$STATE_DIR/$PART.$SYMBOL.ok" >>"$RECORDS"
      done
      continue
    fi

    SEG_RAW=$RAW_DIR/$PART
    SEG_SLICES=$SLICE_DIR/$PART
    REPORT=$STATE_DIR/$PART.slices.json
    if [ ! -f "$STATE_DIR/$PART.sliced.ok" ]; then
      mkdir -p "$SEG_RAW" || continue
      DOWNLOAD_OK=1
      for SUFFIX in "" ".manifest.json" "._SUCCESS"; do
        if ! oss_cp "$URI$SUFFIX" "$SEG_RAW/$OBJECT$SUFFIX"; then
          log err "download failed for $URI$SUFFIX"
          DOWNLOAD_OK=0
          break
        fi
      done
      if [ "$DOWNLOAD_OK" -ne 1 ]; then
        printf '{"date":"%s","hour":"%s","segment":"%s","error":"download_failed"}\n' \
          "$DATE" "$HOUR" "$PART" >>"$FAILURES"
        continue
      fi
      mkdir -p "$SEG_SLICES" || continue
      # shellcheck disable=SC2086
      if ! "$SLICER_BIN" --segment "$SEG_RAW/$OBJECT" \
        --output-dir "$SEG_SLICES" --symbols "$SYMBOLS" $SLICER_ARGS \
        >"$REPORT.tmp" 2>"$STATE_DIR/$PART.slicer.log"; then
        log err "slicer failed for $PART (see $STATE_DIR/$PART.slicer.log)"
        printf '{"date":"%s","hour":"%s","segment":"%s","error":"slice_failed"}\n' \
          "$DATE" "$HOUR" "$PART" >>"$FAILURES"
        rm -f "$REPORT.tmp"
        continue
      fi
      mv "$REPORT.tmp" "$REPORT"
      : >"$STATE_DIR/$PART.sliced.ok"
    fi

    for SLICE_JSON in $(jq -c '.slices[]' "$REPORT" 2>/dev/null); do
      SLICE_FILE=$(printf '%s' "$SLICE_JSON" | jq -r '.file')
      SLICE_SHA=$(printf '%s' "$SLICE_JSON" | jq -r '.sha256')
      SLICE_MANIFEST_SHA=$(printf '%s' "$SLICE_JSON" | jq -r '.manifest_sha256')
      for SYMBOL in $(printf '%s' "$SLICE_JSON" | jq -r '.symbols[]'); do
        case ",$SYMBOLS," in
          *",$SYMBOL,"*) ;;
          *) continue ;;
        esac
        if [ -f "$STATE_DIR/$PART.$SYMBOL.ok" ]; then
          cat "$STATE_DIR/$PART.$SYMBOL.ok" >>"$RECORDS"
          continue
        fi
        MISSION="oss-$DATASET-$PART-$(printf '%s' "$SYMBOL" | tr '[:upper:]' '[:lower:]')"
        if MATERIALIZED=$("$MATERIALIZER_BIN" --mission-id "$MISSION" \
          --symbol "$SYMBOL" --market "$MARKET" \
          --segment "$SEG_SLICES/$SLICE_FILE" \
          --segment-content-sha256 "$SLICE_SHA" \
          --segment-manifest-sha256 "$SLICE_MANIFEST_SHA" \
          --artifact-dir "$ARTIFACT_DIR" 2>"$STATE_DIR/$PART.$SYMBOL.materializer.log"); then
          RECORD=$(printf '%s' "$MATERIALIZED" | jq -c \
            --arg date "$DATE" --arg hour "$HOUR" --arg part "$PART" --arg symbol "$SYMBOL" \
            --arg uri "$URI" --arg slice "$SLICE_FILE" --arg slice_sha "$SLICE_SHA" \
            '{date:$date,hour:$hour,segment:$part,symbol:$symbol,source_object:$uri,slice_file:$slice,slice_sha256:$slice_sha,mission_id:.manifest.mission_id,rows:.manifest.rows,artifact_sha256:.manifest.artifact_sha256,manifest_path:.manifest_path,manifest_sha256:.manifest_sha256}')
          printf '%s\n' "$RECORD" >"$STATE_DIR/$PART.$SYMBOL.ok"
          printf '%s\n' "$RECORD" >>"$RECORDS"
        else
          log err "materializer failed for $PART $SYMBOL (see $STATE_DIR/$PART.$SYMBOL.materializer.log)"
          printf '{"date":"%s","hour":"%s","segment":"%s","symbol":"%s","error":"materialize_failed"}\n' \
            "$DATE" "$HOUR" "$PART" "$SYMBOL" >>"$FAILURES"
          continue
        fi
      done
    done

    PENDING=""
    for SYMBOL in $(printf '%s' "$SYMBOLS" | tr ',' ' '); do
      [ -f "$STATE_DIR/$PART.$SYMBOL.ok" ] || PENDING="$PENDING $SYMBOL"
    done
    if [ -z "$PENDING" ] && [ "$CLEANUP_RAW" = "1" ]; then
      rm -rf "$SEG_RAW"
    fi
  done
  rm -f "$LISTING"
done

RECORD_COUNT=$(grep -c . "$RECORDS" 2>/dev/null || true)
FAILURE_COUNT=$(grep -c . "$FAILURES" 2>/dev/null || true)
case "$RECORD_COUNT" in '' | *[!0-9]*) RECORD_COUNT=0 ;; esac
case "$FAILURE_COUNT" in '' | *[!0-9]*) FAILURE_COUNT=0 ;; esac
RUN_MANIFEST=$ARTIFACT_DIR/slice-materialization-run.json
jq -n \
  --arg bucket "$OSS_BUCKET" --arg market "$MARKET" --arg dataset "$DATASET" \
  --arg shard "$SHARD" --arg start "$START_DATE" --arg end "$END_DATE" \
  --arg symbols "$SYMBOLS" --argjson seen "$SEGMENTS_SEEN" \
  --argjson skipped "$SEGMENTS_SKIPPED" \
  --slurpfile records "$RECORDS" --slurpfile failures "$FAILURES" \
  '{bucket:$bucket,market:$market,dataset:$dataset,shard:$shard,start_date:$start,end_date:$end,symbols:($symbols|split(",")),segments_seen:$seen,segments_skipped:$skipped,materialized:$records,failed:$failures}' \
  >"$RUN_MANIFEST.tmp" && mv "$RUN_MANIFEST.tmp" "$RUN_MANIFEST"

log info "segments=$SEGMENTS_SEEN skipped=$SEGMENTS_SKIPPED materialized=$RECORD_COUNT failures=$FAILURE_COUNT manifest=$RUN_MANIFEST"
[ "$FAILURE_COUNT" -eq 0 ]
