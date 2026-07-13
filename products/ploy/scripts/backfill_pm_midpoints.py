#!/usr/bin/env python3
"""
Backfill Polymarket midpoint prices into clob_quote_ticks.

For each event in pm_market_metadata within the given date range,
fetches the /midpoint price from Polymarket CLOB API at 30-second
intervals and inserts into clob_quote_ticks.

Usage:
  python3 scripts/backfill_pm_midpoints.py \
    --start 2026-04-01 \
    --end   2026-04-04 \
    --db    "postgresql://postgres:postgres@localhost:15432/ploy"

Note: Polymarket /midpoint only returns the CURRENT price, not historical.
This script is useful for backfilling recent data while the collector
was not running or was collecting bad data.
For truly historical data, use the /prices-history endpoint (OHLC only).
"""

import argparse
import asyncio
import time
from datetime import datetime, timezone
from decimal import Decimal

import asyncpg
import httpx

POLYMARKET_CLOB = "https://clob.polymarket.com"
HALF_SPREAD = Decimal("0.005")  # 0.5% synthetic spread around mid


async def get_active_tokens(conn, start: str, end: str) -> list[dict]:
    """Get all event tokens in the date range."""
    start_dt = datetime.fromisoformat(start).replace(tzinfo=timezone.utc)
    end_dt = datetime.fromisoformat(end).replace(hour=23, minute=59, second=59, tzinfo=timezone.utc)
    rows = await conn.fetch(
        """
        SELECT
            market_slug,
            symbol,
            start_time,
            end_time,
            trim(both '"' from (raw_market->'markets'->0->>'clobTokenIds')::jsonb->>0) AS up_token,
            trim(both '"' from (raw_market->'markets'->0->>'clobTokenIds')::jsonb->>1) AS down_token
        FROM pm_market_metadata
        WHERE end_time >= $1::timestamptz
          AND start_time <= $2::timestamptz
          AND raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
        ORDER BY start_time
        """,
        start_dt, end_dt
    )
    return [dict(r) for r in rows]


async def fetch_midpoint(client: httpx.AsyncClient, token_id: str) -> Decimal | None:
    """Fetch current midpoint price from Polymarket."""
    try:
        resp = await client.get(
            f"{POLYMARKET_CLOB}/midpoint",
            params={"token_id": token_id},
            timeout=5.0
        )
        if resp.status_code == 200:
            data = resp.json()
            mid_str = data.get("mid")
            if mid_str:
                return Decimal(mid_str)
    except Exception as e:
        print(f"  Error fetching midpoint for {token_id[:12]}...: {e}")
    return None


async def insert_quote(conn, token_id: str, side: str, bid: Decimal, ask: Decimal):
    """Insert a quote tick into clob_quote_ticks."""
    await conn.execute(
        """
        INSERT INTO clob_quote_ticks (
            token_id, side, best_bid, best_ask,
            received_at, source, domain
        ) VALUES ($1, $2, $3, $4, NOW(), 'midpoint_backfill', 'Crypto')
        ON CONFLICT DO NOTHING
        """,
        token_id, side, float(bid), float(ask)
    )


async def backfill(db_url: str, start: str, end: str, interval_secs: int = 30):
    conn = await asyncpg.connect(db_url)
    print(f"Connected to database")

    tokens = await get_active_tokens(conn, start, end)
    print(f"Found {len(tokens)} events ({len(tokens) * 2} tokens) in {start} → {end}")

    # Flatten to (token_id, side, slug) pairs
    token_list = []
    for event in tokens:
        if event["up_token"]:
            token_list.append((event["up_token"], "UP", event["symbol"]))
        if event["down_token"]:
            token_list.append((event["down_token"], "DOWN", event["symbol"]))

    print(f"Fetching midpoints for {len(token_list)} tokens...")
    print(f"Note: /midpoint returns CURRENT price only, not historical.")
    print(f"This backfill is useful for recent data gaps.\n")

    inserted = 0
    skipped = 0

    async with httpx.AsyncClient() as client:
        for i, (token_id, side, symbol) in enumerate(token_list):
            mid = await fetch_midpoint(client, token_id)

            if mid is None or mid <= 0:
                skipped += 1
                continue

            # Apply synthetic spread
            bid = max(mid - HALF_SPREAD, Decimal("0.01"))
            ask = min(mid + HALF_SPREAD, Decimal("0.99"))

            await insert_quote(conn, token_id, side, bid, ask)
            inserted += 1

            if (i + 1) % 50 == 0:
                print(f"  Progress: {i+1}/{len(token_list)} tokens, {inserted} inserted, {skipped} skipped")

            # Rate limit: ~10 req/sec
            await asyncio.sleep(0.1)

    print(f"\nDone: {inserted} quotes inserted, {skipped} skipped (no midpoint available)")
    await conn.close()


def main():
    parser = argparse.ArgumentParser(description="Backfill Polymarket midpoint prices")
    parser.add_argument("--start", required=True, help="Start date (YYYY-MM-DD)")
    parser.add_argument("--end", required=True, help="End date (YYYY-MM-DD)")
    parser.add_argument("--db", required=True, help="PostgreSQL connection URL")
    args = parser.parse_args()

    asyncio.run(backfill(args.db, args.start, args.end))


if __name__ == "__main__":
    main()
