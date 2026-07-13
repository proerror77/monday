#!/usr/bin/env bash
set -euo pipefail

# Create or refresh the research_valid_windows materialized view on a remote host.
#
# Usage:
#   ./scripts/refresh_research_valid_windows.sh
#   PLOY_RESEARCH_HOST=tango-1-1 ./scripts/refresh_research_valid_windows.sh
#
# Behavior:
# - Ensures migration 036 SQL is applied (idempotent CREATE MATERIALIZED VIEW IF NOT EXISTS)
# - Refreshes the matview if it already exists
#
# Notes:
# - Runs entirely on the remote host against localhost PostgreSQL
# - Does not build Rust on the host

HOST="${PLOY_RESEARCH_HOST:-tango-1-1}"
MIGRATION_PATH="${PLOY_RESEARCH_MIGRATION_PATH:-/opt/ploy/migrations/036_research_valid_windows_matview.sql}"

ssh "${HOST}" 'bash -s' <<'REMOTE'
set -euo pipefail

if [[ -f /opt/ploy/.env ]]; then
  # shellcheck disable=SC1091
  source /opt/ploy/.env >/dev/null 2>&1 || true
fi

DB_URL="${PLOY_RESEARCH_DB_URL:-${DATABASE_URL:-postgresql://postgres:postgres@localhost:5432/ploy}}"
MIGRATION_PATH="${PLOY_RESEARCH_MIGRATION_PATH:-/opt/ploy/migrations/036_research_valid_windows_matview.sql}"

if [[ ! -f "${MIGRATION_PATH}" ]]; then
  echo "missing migration: ${MIGRATION_PATH}" >&2
  exit 2
fi

echo "applying ${MIGRATION_PATH}"
psql "${DB_URL}" -v ON_ERROR_STOP=1 -f "${MIGRATION_PATH}"

echo "refreshing research_valid_windows"
psql "${DB_URL}" -v ON_ERROR_STOP=1 <<'SQL'
REFRESH MATERIALIZED VIEW CONCURRENTLY research_valid_windows;
SQL

echo "done"
REMOTE
