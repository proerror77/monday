#!/usr/bin/env bash
set -euo pipefail

CH_URL="${CH_URL:-http://localhost:8123}"
DB="${CH_DB:-hft}"

echo "Checking ClickHouse at $CH_URL (DB=$DB)"

echo "1) Version:"
curl -sS "$CH_URL/?query=SELECT%20version()" && echo

echo "2) Ensure table exists:"
curl -sS "$CH_URL/?query=EXISTS%20TABLE%20$DB.lob_depth" && echo

echo "3) Row count (all):"
curl -sS "$CH_URL/?query=SELECT%20count()%20FROM%20$DB.lob_depth" && echo

echo "4) Rows per symbol (last 10 minutes):"
curl -sS "$CH_URL/?query=SELECT%20symbol,%20count()%20FROM%20$DB.lob_depth%20WHERE%20toDateTime(intDiv(timestamp,1000000))%20%3E%20now()-%20INTERVAL%2010%20MINUTE%20GROUP%20BY%20symbol%20ORDER%20BY%202%20DESC%20LIMIT%2020" | sed 's/\t/\t|\t/g' && echo

echo "Done."
