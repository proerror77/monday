#!/usr/bin/env bash
set -euo pipefail

DB_URL="${DATABASE_URL:-postgresql://postgres:postgres@localhost:5432/ploy}"
OUT_DIR="${PLOY_ORDERBOOK_ARCHIVE_ROOT:-/opt/ploy/data/lake}"
LAG_HOURS="${PLOY_CLOB_BOOK_ARCHIVE_LAG_HOURS:-1}"

EXPORT_DATE=""
EXPORT_HOUR=""

usage() {
  cat <<'USAGE'
Usage: export_clob_orderbook_snapshots_parquet.sh [--date YYYY-MM-DD --hour HH] [--lag-hours N] [--out-dir DIR]

Exports one completed hour of clob_orderbook_snapshots to Parquet/ZSTD without
sampling or dropping fields. Defaults to the most recently completed hour.
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --date)
      EXPORT_DATE="${2:?missing --date value}"
      shift 2
      ;;
    --hour)
      EXPORT_HOUR="${2:?missing --hour value}"
      shift 2
      ;;
    --lag-hours)
      LAG_HOURS="${2:?missing --lag-hours value}"
      shift 2
      ;;
    --out-dir)
      OUT_DIR="${2:?missing --out-dir value}"
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

if [[ -z "$EXPORT_DATE" || -z "$EXPORT_HOUR" ]]; then
  if ! [[ "$LAG_HOURS" =~ ^[0-9]+$ ]]; then
    echo "invalid --lag-hours value: $LAG_HOURS" >&2
    exit 2
  fi
  EXPORT_DATE="$(date -d "${LAG_HOURS} hours ago" +%Y-%m-%d)"
  EXPORT_HOUR="$(date -d "${LAG_HOURS} hours ago" +%H)"
fi

if ! [[ "$EXPORT_DATE" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
  echo "invalid --date value: $EXPORT_DATE" >&2
  exit 2
fi
if ! [[ "$EXPORT_HOUR" =~ ^[0-9]{2}$ ]] || (( 10#$EXPORT_HOUR > 23 )); then
  echo "invalid --hour value: $EXPORT_HOUR" >&2
  exit 2
fi

START_TS="${EXPORT_DATE} ${EXPORT_HOUR}:00:00+08"
END_TS="$(date -d "${START_TS} +1 hour" '+%Y-%m-%d %H:%M:%S%:z')"
BASE_DIR="${OUT_DIR}/orderbook_snapshots/date=${EXPORT_DATE}"
HOUR_DIR="${BASE_DIR}/hour=${EXPORT_HOUR}"
TMP_DIR="${HOUR_DIR}.tmp.$$"
PARQUET_FILE="${TMP_DIR}/snapshots.parquet"
MANIFEST_FILE="${TMP_DIR}/manifest.json"

mkdir -p "$TMP_DIR"
cleanup() {
  rm -rf "$TMP_DIR"
}
trap cleanup EXIT

echo "export_clob_orderbook_snapshots: date=${EXPORT_DATE} hour=${EXPORT_HOUR} start=${START_TS} end=${END_TS} out=${HOUR_DIR}"

duckdb -c "
SET home_directory='/opt/ploy';
SET threads=1;
INSTALL postgres_scanner; LOAD postgres_scanner;
SET pg_connection_limit=1;
SET pg_use_ctid_scan=false;
ATTACH '${DB_URL}' AS pg (TYPE POSTGRES, READ_ONLY);

COPY (
  SELECT
    id,
    domain,
    token_id,
    market,
    bids,
    asks,
    book_timestamp,
    hash,
    source,
    context,
    received_at
  FROM pg.clob_orderbook_snapshots
  WHERE received_at >= TIMESTAMPTZ '${START_TS}'
    AND received_at < TIMESTAMPTZ '${END_TS}'
) TO '${PARQUET_FILE}' (FORMAT PARQUET, COMPRESSION ZSTD);
"

row_count="$(duckdb -noheader -csv -c "SELECT count(*) FROM read_parquet('${PARQUET_FILE}');")"
file_bytes="$(wc -c < "$PARQUET_FILE" | tr -d '[:space:]')"
sha256="$(sha256sum "$PARQUET_FILE" | awk '{print $1}')"
created_at="$(date -u '+%Y-%m-%dT%H:%M:%SZ')"

cat > "$MANIFEST_FILE" <<JSON
{
  "table": "clob_orderbook_snapshots",
  "format": "parquet",
  "compression": "zstd",
  "date": "${EXPORT_DATE}",
  "hour": "${EXPORT_HOUR}",
  "start_ts": "${START_TS}",
  "end_ts": "${END_TS}",
  "row_count": ${row_count},
  "file": "snapshots.parquet",
  "file_bytes": ${file_bytes},
  "sha256": "${sha256}",
  "created_at": "${created_at}",
  "full_fidelity": true,
  "columns": ["id", "domain", "token_id", "market", "bids", "asks", "book_timestamp", "hash", "source", "context", "received_at"]
}
JSON
touch "${TMP_DIR}/_SUCCESS"

rm -rf "$HOUR_DIR"
mv "$TMP_DIR" "$HOUR_DIR"
trap - EXIT

complete_hours="$(
  find "$BASE_DIR" -mindepth 2 -maxdepth 2 -type f -name _SUCCESS 2>/dev/null \
    | sed -E 's#.*/hour=([0-9]{2})/_SUCCESS#\1#' \
    | sort -u \
    | wc -l \
    | tr -d '[:space:]'
)"
if [[ "$complete_hours" == "24" ]]; then
  touch "${BASE_DIR}/_SUCCESS"
fi

echo "export_clob_orderbook_snapshots: complete rows=${row_count} file_bytes=${file_bytes} sha256=${sha256}"
