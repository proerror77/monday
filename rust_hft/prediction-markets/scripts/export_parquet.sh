#!/bin/bash
set -e
EXPORT_DATE=${1:-$(date -d 'yesterday' +%Y-%m-%d)}
OUT_DIR=/opt/ploy/data/parquet
DB_URL="postgresql://postgres:postgres@localhost:5432/ploy"

# Derive date without dashes for partition table names
DATE_NODASH=$(echo "$EXPORT_DATE" | tr -d '-')

mkdir -p $OUT_DIR/{binance_price_ticks,clob_quote_ticks,binance_lob_ticks,binance_agg_trade_ticks,pm_market_metadata,pm_token_settlements}

# Determine the LOB partition table for this date
LOB_TABLE=$(PGPASSWORD=postgres psql -U postgres -d ploy -tAc \
  "SELECT tablename FROM pg_tables WHERE tablename IN (
     'binance_lob_ticks_new_${DATE_NODASH}',
     'binance_lob_ticks_${DATE_NODASH}'
   ) LIMIT 1" 2>/dev/null | tr -d '[:space:]')

if [ -z "$LOB_TABLE" ]; then
  echo "No LOB partition found for $EXPORT_DATE — skipping binance_lob_ticks"
  LOB_QUERY="SELECT NULL::text AS event_time WHERE false"
  SKIP_LOB=1
else
  echo "Using LOB partition: $LOB_TABLE"
  LOB_QUERY="SELECT * FROM pg.${LOB_TABLE}"
  SKIP_LOB=0
fi

duckdb -c "
INSTALL postgres_scanner; LOAD postgres_scanner;
ATTACH '$DB_URL' AS pg (TYPE POSTGRES, READ_ONLY);

COPY (SELECT * FROM pg.binance_price_ticks WHERE trade_time >= '$EXPORT_DATE'::date AND trade_time < '$EXPORT_DATE'::date + INTERVAL '1 day')
TO '$OUT_DIR/binance_price_ticks/$EXPORT_DATE.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);

COPY (SELECT * FROM pg.clob_quote_ticks WHERE received_at >= '$EXPORT_DATE'::date AND received_at < '$EXPORT_DATE'::date + INTERVAL '1 day')
TO '$OUT_DIR/clob_quote_ticks/$EXPORT_DATE.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);

COPY (SELECT * FROM pg.binance_agg_trade_ticks WHERE trade_time >= '$EXPORT_DATE'::date AND trade_time < '$EXPORT_DATE'::date + INTERVAL '1 day')
TO '$OUT_DIR/binance_agg_trade_ticks/$EXPORT_DATE.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);

COPY (SELECT * FROM pg.pm_market_metadata WHERE start_time >= '$EXPORT_DATE'::date AND start_time < '$EXPORT_DATE'::date + INTERVAL '1 day')
TO '$OUT_DIR/pm_market_metadata/$EXPORT_DATE.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);

COPY (SELECT * FROM pg.pm_token_settlements WHERE fetched_at >= '$EXPORT_DATE'::date AND fetched_at < '$EXPORT_DATE'::date + INTERVAL '1 day')
TO '$OUT_DIR/pm_token_settlements/$EXPORT_DATE.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);
"

if [ "$SKIP_LOB" -eq 0 ]; then
  duckdb -c "
INSTALL postgres_scanner; LOAD postgres_scanner;
ATTACH '$DB_URL' AS pg (TYPE POSTGRES, READ_ONLY);
COPY (SELECT * FROM pg.${LOB_TABLE})
TO '$OUT_DIR/binance_lob_ticks/$EXPORT_DATE.parquet' (FORMAT PARQUET, COMPRESSION ZSTD);
"
fi

echo "Export complete: $EXPORT_DATE"
