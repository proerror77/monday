#!/usr/bin/env python3
"""Binance aggTrade collector via WebSocket."""

import asyncio
import json
import os
import signal
import time
from datetime import datetime, timezone
from decimal import Decimal

import psycopg
import websockets

SYMBOLS = [
    s.strip().upper()
    for s in os.getenv(
        "BINANCE_AGGTRADE_SYMBOLS",
        "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,HYPEUSDT,BNBUSDT",
    ).split(",")
    if s.strip()
]
WS_URL = "wss://stream.binance.com:9443/stream"
DB_URL = (
    os.getenv("PLOY_DATABASE__URL")
    or os.getenv("DATABASE_URL")
    or "postgresql://postgres:postgres@localhost:5432/ploy"
)
COMMIT_BATCH_SIZE = max(1, int(os.getenv("BINANCE_AGGTRADE_COMMIT_BATCH_SIZE", "50")))
COMMIT_INTERVAL_SECS = max(
    0.1, float(os.getenv("BINANCE_AGGTRADE_COMMIT_INTERVAL_SECS", "1.0"))
)
WS_OPEN_TIMEOUT_SECS = max(
    1.0, float(os.getenv("BINANCE_AGGTRADE_WS_OPEN_TIMEOUT_SECS", "15.0"))
)
REPORT_INTERVAL_SECS = max(
    5.0, float(os.getenv("BINANCE_AGGTRADE_REPORT_INTERVAL_SECS", "60"))
)

RUNNING = True


def _on_signal(signum: int, _frame):
    global RUNNING
    RUNNING = False
    print(f"[binance-aggtrade] received signal={signum}, stopping...", flush=True)


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
            print("[binance-aggtrade] Database connection re-established", flush=True)
            return new_conn
        except Exception as exc:
            print(
                f"[binance-aggtrade] Database reconnect failed: {exc}, retrying in 5s...",
                flush=True,
            )
            await asyncio.sleep(5)

    return conn


async def collect_aggtrades():
    streams = [f"{symbol.lower()}@aggTrade" for symbol in SYMBOLS]
    subscribe_msg = {"method": "SUBSCRIBE", "params": streams, "id": 1}

    print(f"[binance-aggtrade] Starting collector for symbols: {SYMBOLS}", flush=True)
    print(
        f"[binance-aggtrade] Database: {DB_URL.split('@')[-1] if '@' in DB_URL else 'localhost'}",
        flush=True,
    )

    conn = await psycopg.AsyncConnection.connect(DB_URL)
    pending = 0
    insert_count = 0
    duplicate_count = 0
    last_commit_at = time.monotonic()
    last_report_at = time.monotonic()

    insert_sql = """
        INSERT INTO binance_agg_trade_ticks (
            symbol, agg_trade_id, first_trade_id, last_trade_id,
            price, quantity, trade_time, event_time, is_buyer_maker
        ) VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s)
        ON CONFLICT DO NOTHING
    """

    try:
        while RUNNING:
            try:
                async with websockets.connect(WS_URL, open_timeout=WS_OPEN_TIMEOUT_SECS) as ws:
                    await ws.send(json.dumps(subscribe_msg))
                    print(
                        f"[binance-aggtrade] WebSocket connected, subscribed to {len(streams)} streams",
                        flush=True,
                    )

                    async for message in ws:
                        if not RUNNING:
                            break

                        try:
                            payload = json.loads(message)
                            if "result" in payload:
                                continue
                            data = payload.get("data")
                            if not data:
                                continue

                            symbol = data["s"]
                            agg_trade_id = int(data["a"])
                            first_trade_id = int(data["f"])
                            last_trade_id = int(data["l"])
                            price = Decimal(data["p"])
                            quantity = Decimal(data["q"])
                            trade_time = datetime.fromtimestamp(
                                int(data["T"]) / 1000, tz=timezone.utc
                            )
                            event_time = datetime.fromtimestamp(
                                int(data["E"]) / 1000, tz=timezone.utc
                            )
                            is_buyer_maker = bool(data["m"])

                            cursor = await conn.execute(
                                insert_sql,
                                (
                                    symbol,
                                    agg_trade_id,
                                    first_trade_id,
                                    last_trade_id,
                                    price,
                                    quantity,
                                    trade_time,
                                    event_time,
                                    is_buyer_maker,
                                ),
                            )
                            pending += 1
                            rowcount = cursor.rowcount or 0
                            if rowcount > 0:
                                insert_count += rowcount
                            else:
                                duplicate_count += 1

                            now = time.monotonic()
                            if (
                                pending >= COMMIT_BATCH_SIZE
                                or now - last_commit_at >= COMMIT_INTERVAL_SECS
                            ):
                                await conn.commit()
                                pending = 0
                                last_commit_at = now

                            if now - last_report_at >= REPORT_INTERVAL_SECS:
                                print(
                                    f"[binance-aggtrade] Inserted {insert_count} agg trades in last interval (duplicates_ignored={duplicate_count})",
                                    flush=True,
                                )
                                insert_count = 0
                                duplicate_count = 0
                                last_report_at = now

                        except Exception as exc:
                            print(f"[binance-aggtrade] Database or parse error: {exc}", flush=True)
                            pending = 0
                            last_commit_at = time.monotonic()
                            conn = await _reconnect_db(conn)

            except (
                websockets.exceptions.WebSocketException,
                ConnectionError,
                TimeoutError,
                OSError,
            ) as exc:
                if RUNNING:
                    print(
                        f"[binance-aggtrade] WebSocket connection error: {exc}, reconnecting in 5s...",
                        flush=True,
                    )
                    await asyncio.sleep(5)
                else:
                    break
    finally:
        if pending > 0:
            await conn.commit()
        await conn.close()
        print("[binance-aggtrade] Collector stopped", flush=True)


def main():
    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    try:
        asyncio.run(collect_aggtrades())
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
