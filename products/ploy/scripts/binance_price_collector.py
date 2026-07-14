#!/usr/bin/env python3
"""Binance spot price collector via WebSocket.

Subscribes to Binance trade streams and persists price ticks into PostgreSQL.
"""

import asyncio
import json
import os
import signal
import sys
import time
from datetime import datetime, timezone
from decimal import Decimal
from typing import Optional

import psycopg
import websockets

# Configuration from environment
SYMBOLS = [s.strip().upper() for s in os.getenv("BINANCE_PRICE_SYMBOLS", "BTCUSDT,ETHUSDT,SOLUSDT").split(",") if s.strip()]
WS_URL = "wss://stream.binance.com:9443/stream"
DB_URL = os.getenv("PLOY_DATABASE__URL") or os.getenv("DATABASE_URL") or "postgresql://postgres:postgres@localhost:5432/ploy"
COMMIT_BATCH_SIZE = max(1, int(os.getenv("BINANCE_PRICE_COMMIT_BATCH_SIZE", "25")))
COMMIT_INTERVAL_SECS = max(0.1, float(os.getenv("BINANCE_PRICE_COMMIT_INTERVAL_SECS", "1.0")))
WS_OPEN_TIMEOUT_SECS = max(1.0, float(os.getenv("BINANCE_PRICE_WS_OPEN_TIMEOUT_SECS", "15.0")))

RUNNING = True


def _on_signal(signum: int, _frame):
    global RUNNING
    RUNNING = False
    print(f"[binance-price] received signal={signum}, stopping...", flush=True)


async def _reconnect_db(conn: psycopg.AsyncConnection) -> psycopg.AsyncConnection:
    try:
        await conn.rollback()
    except Exception:
        pass
    try:
        await conn.close()
    except Exception:
        pass

    while RUNNING:
        try:
            new_conn = await psycopg.AsyncConnection.connect(DB_URL)
            print("[binance-price] Database connection re-established", flush=True)
            return new_conn
        except Exception as exc:
            print(f"[binance-price] Database reconnect failed: {exc}, retrying in 5s...", flush=True)
            await asyncio.sleep(5)

    return conn


async def collect_prices():
    """Main collector loop."""
    # Build WebSocket subscription payload
    streams = [f"{symbol.lower()}@trade" for symbol in SYMBOLS]
    subscribe_msg = {
        "method": "SUBSCRIBE",
        "params": streams,
        "id": 1
    }

    print(f"[binance-price] Starting collector for symbols: {SYMBOLS}", flush=True)
    print(f"[binance-price] Database: {DB_URL.split('@')[-1] if '@' in DB_URL else 'localhost'}", flush=True)

    # Connect to database
    conn = await psycopg.AsyncConnection.connect(DB_URL)

    # Statistics
    insert_count = 0
    duplicate_count = 0
    last_report_time = datetime.now(timezone.utc)
    last_commit_at = time.monotonic()
    pending = 0

    try:
        while RUNNING:
            try:
                async with websockets.connect(WS_URL, open_timeout=WS_OPEN_TIMEOUT_SECS) as ws:
                    # Subscribe to trade streams
                    await ws.send(json.dumps(subscribe_msg))
                    print(f"[binance-price] WebSocket connected, subscribed to {len(streams)} streams", flush=True)

                    # Process messages
                    async for message in ws:
                        if not RUNNING:
                            break

                        try:
                            data = json.loads(message)

                            # Skip subscription confirmation
                            if "result" in data:
                                continue

                            # Extract trade data
                            if "data" not in data:
                                continue

                            trade = data["data"]
                            symbol = trade["s"]  # e.g., "BTCUSDT"
                            price = Decimal(trade["p"])
                            quantity = Decimal(trade["q"])
                            trade_time_ms = trade["T"]

                            # Convert to datetime
                            trade_time = datetime.fromtimestamp(trade_time_ms / 1000, tz=timezone.utc)

                            # Insert into database
                            try:
                                cursor = await conn.execute(
                                    """
                                    INSERT INTO binance_price_ticks (symbol, price, quantity, trade_time)
                                    VALUES (%s, %s, %s, %s)
                                    ON CONFLICT DO NOTHING
                                    """,
                                    (symbol, price, quantity, trade_time)
                                )
                                pending += 1
                                rowcount = cursor.rowcount or 0
                                if rowcount > 0:
                                    insert_count += rowcount
                                else:
                                    duplicate_count += 1

                                now_monotonic = time.monotonic()
                                if (
                                    pending >= COMMIT_BATCH_SIZE
                                    or now_monotonic - last_commit_at >= COMMIT_INTERVAL_SECS
                                ):
                                    await conn.commit()
                                    pending = 0
                                    last_commit_at = now_monotonic

                                # Report stats every 60 seconds
                                now = datetime.now(timezone.utc)
                                if (now - last_report_time).total_seconds() >= 60:
                                    print(
                                        f"[binance-price] Inserted {insert_count} ticks in last minute (duplicates_ignored={duplicate_count})",
                                        flush=True,
                                    )
                                    insert_count = 0
                                    duplicate_count = 0
                                    last_report_time = now

                            except Exception as db_err:
                                print(f"[binance-price] Database error: {db_err}", flush=True)
                                pending = 0
                                last_commit_at = time.monotonic()
                                conn = await _reconnect_db(conn)

                        except (json.JSONDecodeError, KeyError, ValueError) as e:
                            print(f"[binance-price] Error parsing message: {e}", flush=True)
                            continue

            except (
                websockets.exceptions.WebSocketException,
                ConnectionError,
                TimeoutError,
                OSError,
            ) as e:
                if RUNNING:
                    print(
                        f"[binance-price] WebSocket connection error: {e}, reconnecting in 5s...",
                        flush=True,
                    )
                    await asyncio.sleep(5)
                else:
                    break

    finally:
        if pending > 0:
            await conn.commit()
        await conn.close()
        print("[binance-price] Collector stopped", flush=True)


def main():
    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    try:
        asyncio.run(collect_prices())
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
