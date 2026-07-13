#!/usr/bin/env python3
"""Emit a JSON snapshot of live market-data freshness.

The script is intended to run on the data/runtime host next to PostgreSQL. It
uses psql instead of a Python database dependency so it can be shipped with the
existing lightweight deployment bundle.
"""

import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from typing import Optional

DB_URL = (
    os.environ.get("PLOY_DATABASE__URL")
    or os.environ.get("DATABASE_URL")
    or "postgresql://postgres:postgres@localhost:5432/ploy"
)


def run_sql(query: str, timeout: int = 10) -> str:
    cmd = [
        "psql",
        DB_URL,
        "-t",
        "-A",
        "-v",
        "ON_ERROR_STOP=1",
        "-c",
        query,
    ]
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    return result.stdout.strip()


def sql_json(query: str, fallback):
    raw = run_sql(query)
    if not raw:
        return fallback
    return json.loads(raw)


TODAY_SUFFIX = datetime.now().strftime("%Y%m%d")


def current_partition(table: str) -> str:
    return f"{table}_new_{TODAY_SUFFIX}"


def source_snapshot(
    source_id: str,
    table: str,
    timestamp_column: str,
    stale_after_seconds: int,
    scan_table: Optional[str] = None,
    latest_method: str = "order",
    where_sql: str = "TRUE",
):
    scan_table = scan_table or table
    if latest_method == "max":
        latest_sql = f"SELECT max({timestamp_column}) AS latest_at FROM {scan_table} WHERE {where_sql}"
    else:
        latest_sql = f"""
  SELECT {timestamp_column} AS latest_at
  FROM {scan_table}
  WHERE {where_sql}
  ORDER BY {timestamp_column} DESC
  LIMIT 1
"""
    query = f"""
WITH table_ref AS (
  SELECT to_regclass('public.{table}') AS oid
),
latest AS (
  {latest_sql}
),
estimate AS (
  SELECT GREATEST(c.reltuples, 0)::bigint AS approx_rows
  FROM pg_class c
  JOIN table_ref r ON c.oid = r.oid
)
SELECT json_build_object(
  'source_id', '{source_id}',
  'table_name', '{table}',
  'latest_at', (SELECT latest_at FROM latest),
  'stale_after_seconds', {stale_after_seconds},
  'approx_rows', COALESCE((SELECT approx_rows FROM estimate), 0)
)::text
"""
    return sql_json(
        query,
        {
            "source_id": source_id,
            "table_name": table,
            "latest_at": None,
            "stale_after_seconds": stale_after_seconds,
            "approx_rows": 0,
        },
    )


def deribit_iv_samples():
    return sql_json(
        f"""
SELECT COALESCE(json_agg(row_to_json(row)), '[]'::json)::text
FROM (
  SELECT currency, instrument_name, mark_iv, bid_iv, ask_iv, underlying_price, fetched_at
  FROM {current_partition("deribit_iv_ticks")}
  ORDER BY id DESC
  LIMIT 8
) row
""",
        [],
    )


def deribit_greeks_samples():
    return sql_json(
        """
SELECT COALESCE(json_agg(row_to_json(row)), '[]'::json)::text
FROM (
  SELECT currency, instrument_name, mark_iv, delta, gamma, vega, theta,
         underlying_price, fetched_at
  FROM deribit_atm_greeks_ticks
  ORDER BY id DESC
  LIMIT 8
) row
""",
        [],
    )


def main() -> int:
    try:
        sources = [
            source_snapshot(
                "polymarket_quotes",
                "clob_quote_ticks",
                "received_at",
                30,
                latest_method="max",
            ),
            source_snapshot(
                "binance_lob",
                "binance_lob_ticks",
                "event_time",
                30,
                current_partition("binance_lob_ticks"),
            ),
            source_snapshot("binance_agg_trades", "binance_agg_trade_ticks", "trade_time", 30),
            source_snapshot(
                "deribit_iv",
                "deribit_iv_ticks",
                "fetched_at",
                300,
                current_partition("deribit_iv_ticks"),
                latest_method="max",
            ),
            source_snapshot(
                "deribit_atm_greeks",
                "deribit_atm_greeks_ticks",
                "fetched_at",
                300,
            ),
            source_snapshot(
                "binance_futures",
                "cex_public_market_ticks",
                "event_time",
                300,
                where_sql="source_key = 'binance/derivatives_snapshot'",
            ),
            source_snapshot(
                "binance_liquidations",
                "cex_public_market_ticks",
                "event_time",
                86400,
                where_sql="source_key = 'binance/liquidation'",
            ),
            source_snapshot(
                "okx_lob",
                "cex_public_market_ticks",
                "event_time",
                300,
                where_sql="source_key = 'okx/lob'",
            ),
            source_snapshot(
                "bybit_lob",
                "cex_public_market_ticks",
                "event_time",
                300,
                where_sql="source_key = 'bybit/lob'",
            ),
            source_snapshot(
                "coinbase_lob",
                "cex_public_market_ticks",
                "event_time",
                300,
                where_sql="source_key = 'coinbase/lob'",
            ),
            source_snapshot(
                "kraken_lob",
                "cex_public_market_ticks",
                "event_time",
                300,
                where_sql="source_key = 'kraken/lob'",
            ),
        ]
        payload = {
            "generated_at": datetime.now(timezone.utc).isoformat(),
            "sources": sources,
            "deribit_iv_samples": deribit_iv_samples(),
            "deribit_greeks_samples": deribit_greeks_samples(),
        }
        print(json.dumps(payload, separators=(",", ":"), default=str))
        return 0
    except Exception as exc:
        print(json.dumps({"error": str(exc)}), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
