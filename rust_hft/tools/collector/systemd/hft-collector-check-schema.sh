#!/usr/bin/env bash
set -euo pipefail

# Wrapper to run the repo's schema checker if present on the host.
# Install this to /usr/local/bin/hft-collector-check-schema and make it executable.

CH_URL=${CH_URL:-"https://kcveg5xfsi.ap-northeast-1.aws.clickhouse.cloud:8443"}
CH_USER=${CH_USER:-"default"}
CH_PASSWORD=${CH_PASSWORD:-""}
CH_DATABASE=${CH_DATABASE:-"hft_db"}
EXCHANGES_CSV=${1:-"binance,bitget,okx,bybit,hyperliquid"}

SCRIPT_DIR="/opt/hft-collector/scripts"
ALT_DIR="/usr/local/share/hft-collector/scripts"

if [ -x "$SCRIPT_DIR/check_schema.sh" ]; then
  export CH_URL CH_USER CH_PASSWORD CH_DATABASE
  exec "$SCRIPT_DIR/check_schema.sh" "$EXCHANGES_CSV"
elif [ -x "$ALT_DIR/check_schema.sh" ]; then
  export CH_URL CH_USER CH_PASSWORD CH_DATABASE
  exec "$ALT_DIR/check_schema.sh" "$EXCHANGES_CSV"
else
  echo "check_schema.sh not found; skipping schema check" >&2
  exit 0
fi

