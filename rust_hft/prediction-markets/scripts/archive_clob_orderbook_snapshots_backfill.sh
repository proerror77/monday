#!/usr/bin/env bash
set -euo pipefail

ARCHIVE_ROOT="${PLOY_ORDERBOOK_ARCHIVE_ROOT:-/opt/ploy/data/lake}"
LOOKBACK_HOURS="${PLOY_CLOB_BOOK_ARCHIVE_LOOKBACK_HOURS:-72}"
MAX_HOURS_PER_RUN="${PLOY_CLOB_BOOK_ARCHIVE_MAX_HOURS_PER_RUN:-6}"
LAG_HOURS="${PLOY_CLOB_BOOK_ARCHIVE_LAG_HOURS:-1}"
EXPORT_SCRIPT="${PLOY_CLOB_BOOK_EXPORT_SCRIPT:-/opt/ploy/scripts/export_clob_orderbook_snapshots_parquet.sh}"

usage() {
  cat <<'USAGE'
Usage: archive_clob_orderbook_snapshots_backfill.sh [--lookback-hours N] [--max-hours N] [--lag-hours N] [--out-dir DIR]

Sequentially exports missing completed clob_orderbook_snapshots hours. Existing
hour directories with _SUCCESS are skipped. Defaults are intentionally bounded
for a live trading host.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --lookback-hours)
      LOOKBACK_HOURS="${2:?missing --lookback-hours value}"
      shift 2
      ;;
    --max-hours)
      MAX_HOURS_PER_RUN="${2:?missing --max-hours value}"
      shift 2
      ;;
    --lag-hours)
      LAG_HOURS="${2:?missing --lag-hours value}"
      shift 2
      ;;
    --out-dir)
      ARCHIVE_ROOT="${2:?missing --out-dir value}"
      shift 2
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      echo "unknown argument: $1" >&2
      usage >&2
      exit 2
      ;;
  esac
done

for value_name in LOOKBACK_HOURS MAX_HOURS_PER_RUN LAG_HOURS; do
  value="${!value_name}"
  if ! [[ "$value" =~ ^[0-9]+$ ]]; then
    echo "invalid ${value_name}: ${value}" >&2
    exit 2
  fi
done

if [[ ! -x "$EXPORT_SCRIPT" ]]; then
  echo "export script is not executable: $EXPORT_SCRIPT" >&2
  exit 2
fi

completed=0
checked=0

echo "archive_clob_orderbook_snapshots_backfill: archive_root=${ARCHIVE_ROOT} lookback_hours=${LOOKBACK_HOURS} max_hours_per_run=${MAX_HOURS_PER_RUN} lag_hours=${LAG_HOURS}"

for (( offset = LOOKBACK_HOURS; offset >= LAG_HOURS; offset-- )); do
  export_date="$(date -d "${offset} hours ago" +%Y-%m-%d)"
  export_hour="$(date -d "${offset} hours ago" +%H)"
  marker="${ARCHIVE_ROOT}/orderbook_snapshots/date=${export_date}/hour=${export_hour}/_SUCCESS"
  checked=$((checked + 1))

  if [[ -f "$marker" ]]; then
    continue
  fi

  if (( completed >= MAX_HOURS_PER_RUN )); then
    echo "archive_clob_orderbook_snapshots_backfill: reached max_hours_per_run=${MAX_HOURS_PER_RUN}"
    break
  fi

  echo "archive_clob_orderbook_snapshots_backfill: exporting missing date=${export_date} hour=${export_hour}"
  "$EXPORT_SCRIPT" --date "$export_date" --hour "$export_hour" --out-dir "$ARCHIVE_ROOT"
  completed=$((completed + 1))
done

echo "archive_clob_orderbook_snapshots_backfill: done checked=${checked} exported=${completed}"
