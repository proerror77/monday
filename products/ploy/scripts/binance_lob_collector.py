#!/usr/bin/env python3
"""Binance L2 orderbook collector via WebSocket.

Subscribes to Binance partial depth streams and persists normalized orderbook
snapshots into `binance_lob_ticks`.
"""

import asyncio
import json
import os
import signal
import time
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation
from typing import Iterable

import psycopg
import websockets
from psycopg.types.json import Jsonb

WS_URL = "wss://stream.binance.com:9443/stream"
DB_URL = (
    os.getenv("PLOY_DATABASE__URL")
    or os.getenv("DATABASE_URL")
    or "postgresql://postgres:postgres@localhost:5432/ploy"
)
SYMBOLS = [
    symbol.strip().upper()
    for symbol in os.getenv(
        "BINANCE_LOB_SYMBOLS",
        "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,HYPEUSDT,BNBUSDT",
    ).split(",")
    if symbol.strip()
]
DEPTH_LEVELS = max(10, int(os.getenv("BINANCE_LOB_LEVELS", "20")))
COMMIT_BATCH_SIZE = max(1, int(os.getenv("BINANCE_LOB_COMMIT_BATCH_SIZE", "25")))
COMMIT_INTERVAL_SECS = max(
    0.1, float(os.getenv("BINANCE_LOB_COMMIT_INTERVAL_SECS", "1.0"))
)
WS_OPEN_TIMEOUT_SECS = max(1.0, float(os.getenv("BINANCE_LOB_WS_OPEN_TIMEOUT_SECS", "15.0")))
REPORT_INTERVAL_SECS = max(5.0, float(os.getenv("BINANCE_LOB_REPORT_INTERVAL_SECS", "60")))
RUNNING = True


def _on_signal(signum: int, _frame):
    global RUNNING
    RUNNING = False
    print(f"[binance-lob] received signal={signum}, stopping...", flush=True)


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
            print("[binance-lob] Database connection re-established", flush=True)
            return new_conn
        except Exception as exc:
            print(f"[binance-lob] Database reconnect failed: {exc}, retrying in 5s...", flush=True)
            await asyncio.sleep(5)

    return conn


def _decimal(raw: str) -> Decimal:
    return Decimal(raw)


def _parse_levels(raw_levels: Iterable[Iterable[str]]) -> list[tuple[Decimal, Decimal]]:
    levels: list[tuple[Decimal, Decimal]] = []
    for raw_price, raw_size, *_ in raw_levels:
        try:
            price = _decimal(raw_price)
            size = _decimal(raw_size)
        except (InvalidOperation, ValueError):
            continue
        if price <= 0 or size <= 0:
            continue
        levels.append((price, size))
    return levels


def _sum_volume(levels: list[tuple[Decimal, Decimal]], depth: int) -> Decimal:
    total = Decimal("0")
    for _, size in levels[:depth]:
        total += size
    return total


def _obi(bid_volume: Decimal, ask_volume: Decimal) -> Decimal:
    denominator = bid_volume + ask_volume
    if denominator == 0:
        return Decimal("0")
    return (bid_volume - ask_volume) / denominator


def _levels_payload(levels: list[tuple[Decimal, Decimal]]) -> list[dict[str, str]]:
    return [
        {"price": format(price, "f"), "size": format(size, "f")}
        for price, size in levels
    ]


async def _flush(conn: psycopg.AsyncConnection, pending: int) -> int:
    if pending == 0:
        return 0
    await conn.commit()
    return 0


def _infer_symbol(stream_name: str | None, payload_symbol: str | None) -> str | None:
    if payload_symbol:
        return payload_symbol
    if not stream_name:
        return None
    raw_symbol = stream_name.split("@", 1)[0]
    return raw_symbol.upper() if raw_symbol else None


async def collect_lob():
    streams = [f"{symbol.lower()}@depth{DEPTH_LEVELS}@100ms" for symbol in SYMBOLS]
    subscribe_msg = {"method": "SUBSCRIBE", "params": streams, "id": 1}

    print(f"[binance-lob] Starting collector for symbols: {SYMBOLS}", flush=True)
    print(
        f"[binance-lob] Depth levels={DEPTH_LEVELS} batch={COMMIT_BATCH_SIZE} commit_interval={COMMIT_INTERVAL_SECS}s",
        flush=True,
    )
    print(
        f"[binance-lob] Database: {DB_URL.split('@')[-1] if '@' in DB_URL else 'localhost'}",
        flush=True,
    )

    conn = await psycopg.AsyncConnection.connect(DB_URL)
    pending = 0
    inserted = 0
    last_commit_at = time.monotonic()
    last_report_at = time.monotonic()

    insert_sql = """
        INSERT INTO binance_lob_ticks (
            symbol, update_id, best_bid, best_ask, mid_price, spread_bps,
            obi_5, obi_10, bid_volume_5, ask_volume_5, bids, asks, event_time, source
        ) VALUES (
            %s, %s, %s, %s, %s, %s,
            %s, %s, %s, %s, %s, %s, %s, %s
        )
    """

    try:
        while RUNNING:
            try:
                async with websockets.connect(
                    WS_URL,
                    ping_interval=20,
                    ping_timeout=20,
                    open_timeout=WS_OPEN_TIMEOUT_SECS,
                    max_queue=4096,
                ) as ws:
                    await ws.send(json.dumps(subscribe_msg))
                    print(
                        f"[binance-lob] WebSocket connected, subscribed to {len(streams)} streams",
                        flush=True,
                    )

                    async for message in ws:
                        if not RUNNING:
                            break

                        try:
                            payload = json.loads(message)
                        except json.JSONDecodeError as exc:
                            print(f"[binance-lob] Error parsing message: {exc}", flush=True)
                            continue

                        if "result" in payload:
                            continue

                        data = payload.get("data")
                        if not data:
                            continue

                        stream_name = payload.get("stream")
                        symbol = _infer_symbol(stream_name, data.get("s"))
                        event_time_ms = data.get("E")
                        update_id = data.get("u") or data.get("lastUpdateId")
                        raw_bids = data.get("b") or data.get("bids") or []
                        raw_asks = data.get("a") or data.get("asks") or []

                        if not symbol or event_time_ms is None or update_id is None:
                            if symbol and event_time_ms is None and raw_bids and raw_asks and update_id is not None:
                                event_time = datetime.now(timezone.utc)
                            else:
                                print(
                                    "[binance-lob] Skipping payload with missing symbol/event/update_id",
                                    flush=True,
                                )
                                continue
                        else:
                            event_time = datetime.fromtimestamp(
                                event_time_ms / 1000, tz=timezone.utc
                            )

                        bids = _parse_levels(raw_bids)
                        asks = _parse_levels(raw_asks)
                        if not bids or not asks:
                            continue

                        best_bid = bids[0][0]
                        best_ask = asks[0][0]
                        if best_bid <= 0 or best_ask <= 0 or best_ask < best_bid:
                            continue

                        mid_price = (best_bid + best_ask) / Decimal("2")
                        if mid_price <= 0:
                            continue

                        spread_bps = ((best_ask - best_bid) / mid_price) * Decimal("10000")
                        bid_volume_5 = _sum_volume(bids, 5)
                        ask_volume_5 = _sum_volume(asks, 5)
                        bid_volume_10 = _sum_volume(bids, 10)
                        ask_volume_10 = _sum_volume(asks, 10)
                        try:
                            await conn.execute(
                                insert_sql,
                                (
                                    symbol,
                                    int(update_id),
                                    best_bid,
                                    best_ask,
                                    mid_price,
                                    spread_bps,
                                    _obi(bid_volume_5, ask_volume_5),
                                    _obi(bid_volume_10, ask_volume_10),
                                    bid_volume_5,
                                    ask_volume_5,
                                    Jsonb(_levels_payload(bids[:DEPTH_LEVELS])),
                                    Jsonb(_levels_payload(asks[:DEPTH_LEVELS])),
                                    event_time,
                                    "binance_depth_ws",
                                ),
                            )
                            pending += 1
                            inserted += 1
                        except Exception as db_err:
                            print(f"[binance-lob] Database error: {db_err}", flush=True)
                            pending = 0
                            last_commit_at = time.monotonic()
                            conn = await _reconnect_db(conn)
                            continue

                        now = time.monotonic()
                        if (
                            pending >= COMMIT_BATCH_SIZE
                            or now - last_commit_at >= COMMIT_INTERVAL_SECS
                        ):
                            pending = await _flush(conn, pending)
                            last_commit_at = now

                        if now - last_report_at >= REPORT_INTERVAL_SECS:
                            print(
                                f"[binance-lob] Inserted {inserted} snapshots in last interval",
                                flush=True,
                            )
                            inserted = 0
                            last_report_at = now

            except (
                websockets.exceptions.WebSocketException,
                ConnectionError,
                TimeoutError,
                OSError,
            ) as exc:
                if RUNNING:
                    print(
                        f"[binance-lob] WebSocket connection error: {exc}, reconnecting in 5s...",
                        flush=True,
                    )
                    await asyncio.sleep(5)
                else:
                    break
    finally:
        if pending > 0:
            await _flush(conn, pending)
        await conn.close()
        print("[binance-lob] Collector stopped", flush=True)


def main():
    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)

    try:
        asyncio.run(collect_lob())
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
