#!/usr/bin/env bash
set -euo pipefail

: "${CLICKHOUSE_URL:?set CLICKHOUSE_URL}"
: "${CLICKHOUSE_USER:?set CLICKHOUSE_USER}"
: "${CLICKHOUSE_PASSWORD:?set CLICKHOUSE_PASSWORD}"
: "${CLICKHOUSE_DATABASE:?set CLICKHOUSE_DATABASE}"

run_query() {
  local query="$1"
  curl --fail --silent --show-error \
    --user "${CLICKHOUSE_USER}:${CLICKHOUSE_PASSWORD}" \
    --data-binary "${query}" \
    "${CLICKHOUSE_URL}/?database=${CLICKHOUSE_DATABASE}"
}

printf 'Collector table row counts for %s\n' "$CLICKHOUSE_DATABASE"
run_query "SELECT table, total_rows FROM system.tables WHERE database = currentDatabase() ORDER BY table FORMAT PrettyCompact"
