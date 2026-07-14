#!/usr/bin/env bash
set -euo pipefail

if [[ $# -ne 1 ]]; then
  echo "usage: $0 YYYY-MM-DD" >&2
  exit 64
fi

repair_date="$1"
if [[ ! "${repair_date}" =~ ^[0-9]{4}-[0-9]{2}-[0-9]{2}$ ]]; then
  echo "repair date must be YYYY-MM-DD: ${repair_date}" >&2
  exit 64
fi

db_url="${DB_URL:-postgresql://postgres:postgres@localhost:5432/ploy}"
start_ts="${repair_date}T00:00:00Z"
end_ts="$(date -u -d "${repair_date} + 1 day" +%Y-%m-%dT00:00:00Z)"

psql "${db_url}" \
  --set=ON_ERROR_STOP=1 \
  --set=start_ts="${start_ts}" \
  --set=end_ts="${end_ts}" <<'SQL'
\echo 'Repairing clob_quote_ticks bid_size/ask_size from clob_orderbook_snapshots'
\echo 'Window:' :start_ts 'to' :end_ts

-- The quote table is a compressed TimescaleDB hypertable on Tango. A one-day
-- repair can legitimately touch more than the default 100k decompressed tuple
-- DML guard. Keep the override scoped to this psql session.
SET timescaledb.max_tuples_decompressed_per_dml_transaction = 0;

\echo 'Before repair:'
SELECT
    count(*) AS quote_rows,
    count(*) FILTER (WHERE ask_size IS NOT NULL AND ask_size > 0) AS ask_size_rows,
    count(*) FILTER (WHERE bid_size IS NOT NULL AND bid_size > 0) AS bid_size_rows
FROM clob_quote_ticks
WHERE received_at >= :'start_ts'::timestamptz
  AND received_at < :'end_ts'::timestamptz
  AND source = 'polymarket_ws_collector';

\echo 'Materializing snapshot top-of-book levels into a temp table'
CREATE TEMP TABLE quote_size_repair AS
SELECT
    s.token_id,
    s.received_at,
    bid.price AS best_bid,
    bid.size AS bid_size,
    ask.price AS best_ask,
    ask.size AS ask_size
FROM clob_orderbook_snapshots s
LEFT JOIN LATERAL (
    SELECT
        (level->>'price')::numeric AS price,
        (level->>'size')::numeric AS size
    FROM jsonb_array_elements(s.bids) AS level
    WHERE (level->>'price')::numeric > 0.02
      AND (level->>'price')::numeric < 0.98
      AND (level->>'size')::numeric > 0
    ORDER BY (level->>'price')::numeric DESC
    LIMIT 1
) bid ON true
LEFT JOIN LATERAL (
    SELECT
        (level->>'price')::numeric AS price,
        (level->>'size')::numeric AS size
    FROM jsonb_array_elements(s.asks) AS level
    WHERE (level->>'price')::numeric > 0.02
      AND (level->>'price')::numeric < 0.98
      AND (level->>'size')::numeric > 0
    ORDER BY (level->>'price')::numeric ASC
    LIMIT 1
) ask ON true
WHERE s.received_at >= :'start_ts'::timestamptz
  AND s.received_at < :'end_ts'::timestamptz
  AND (bid.size IS NOT NULL OR ask.size IS NOT NULL);

CREATE INDEX quote_size_repair_token_time_idx
    ON quote_size_repair(token_id, received_at);
ANALYZE quote_size_repair;

\echo 'Temp repair rows:'
SELECT
    count(*) AS repair_rows,
    count(*) FILTER (WHERE ask_size IS NOT NULL AND ask_size > 0) AS ask_size_rows,
    count(*) FILTER (WHERE bid_size IS NOT NULL AND bid_size > 0) AS bid_size_rows
FROM quote_size_repair;

\echo 'Updating clob_quote_ticks from temp repair rows'
WITH updated AS (
    UPDATE clob_quote_ticks q
    SET
        bid_size = CASE
            WHEN q.bid_size IS NULL AND q.best_bid = r.best_bid THEN r.bid_size
            ELSE q.bid_size
        END,
        ask_size = CASE
            WHEN q.ask_size IS NULL AND q.best_ask = r.best_ask THEN r.ask_size
            ELSE q.ask_size
        END
    FROM quote_size_repair r
    WHERE q.token_id = r.token_id
      AND q.received_at = r.received_at
      AND q.source = 'polymarket_ws_collector'
      AND q.received_at >= :'start_ts'::timestamptz
      AND q.received_at < :'end_ts'::timestamptz
      AND (
        (q.bid_size IS NULL AND q.best_bid = r.best_bid AND r.bid_size IS NOT NULL)
        OR
        (q.ask_size IS NULL AND q.best_ask = r.best_ask AND r.ask_size IS NOT NULL)
      )
    RETURNING q.id
)
SELECT count(*) AS updated_rows FROM updated;

\echo 'After repair:'
SELECT
    count(*) AS quote_rows,
    count(*) FILTER (WHERE ask_size IS NOT NULL AND ask_size > 0) AS ask_size_rows,
    count(*) FILTER (WHERE bid_size IS NOT NULL AND bid_size > 0) AS bid_size_rows
FROM clob_quote_ticks
WHERE received_at >= :'start_ts'::timestamptz
  AND received_at < :'end_ts'::timestamptz
  AND source = 'polymarket_ws_collector';
SQL
