#!/usr/bin/env python3
"""
Continuous Polymarket CLOB quote collection for active crypto up/down markets.

This collector reads active PM 5m/15m windows from `pm_market_metadata`,
normalizes Gamma hex `clobTokenIds` into canonical decimal CLOB asset IDs, and
subscribes to the market WebSocket using the current protocol:

    {
      "type": "market",
      "operation": "subscribe",
      "markets": [],
      "assets_ids": ["<decimal token id>", ...],
      "initial_dump": true
    }

Incoming messages use `event_type` rather than `type`, and the initial dump may
arrive as an array of book snapshots, so this collector handles both shapes.
"""

from __future__ import annotations

import argparse
import asyncio
import json
import logging
import signal
from datetime import datetime, timedelta, timezone
from typing import Any

import asyncpg
import websockets
from websockets.exceptions import ConnectionClosed

logging.basicConfig(
    level=logging.INFO,
    format="%(asctime)s [%(levelname)s] %(message)s",
    datefmt="%Y-%m-%d %H:%M:%S",
)
logger = logging.getLogger(__name__)


def normalize_token_id(raw_token: str) -> str:
    token = str(raw_token).strip().strip('"')
    if token.startswith(("0x", "0X")):
        return str(int(token, 16))
    return token


def best_bid_from_levels(levels: list[dict[str, Any]]) -> tuple[float | None, float | None]:
    best_price: float | None = None
    best_size: float | None = None
    for level in levels:
        try:
            price = float(level["price"])
        except (KeyError, TypeError, ValueError):
            continue
        if best_price is None or price > best_price:
            best_price = price
            try:
                best_size = float(level["size"])
            except (KeyError, TypeError, ValueError):
                best_size = None
    return best_price, best_size


def best_ask_from_levels(levels: list[dict[str, Any]]) -> tuple[float | None, float | None]:
    best_price: float | None = None
    best_size: float | None = None
    for level in levels:
        try:
            price = float(level["price"])
        except (KeyError, TypeError, ValueError):
            continue
        if best_price is None or price < best_price:
            best_price = price
            try:
                best_size = float(level["size"])
            except (KeyError, TypeError, ValueError):
                best_size = None
    return best_price, best_size


class PolymarketQuoteCollector:
    """Collect and persist best bid/ask ticks from the Polymarket CLOB WS."""

    def __init__(
        self,
        db_url: str,
        symbols: list[str],
        timeframe: str = "5m",
        ws_url: str = "wss://ws-subscriptions-clob.polymarket.com/ws/market",
        lookahead_hours: int = 2,
        refresh_seconds: int = 300,
    ) -> None:
        self.db_url = db_url
        self.symbols = symbols
        self.timeframe = timeframe
        self.ws_url = ws_url
        self.lookahead_hours = lookahead_hours
        self.refresh_seconds = refresh_seconds

        self.db_pool: asyncpg.Pool | None = None
        self.ws: websockets.ClientConnection | None = None
        self.loop: asyncio.AbstractEventLoop | None = None
        self.has_domain_column = False
        self.running = True

        self.subscribed_tokens: set[str] = set()
        self.token_metadata: dict[str, dict[str, Any]] = {}
        self.first_quote_tokens: set[str] = set()
        self.last_refresh: datetime | None = None

        self.quotes_received = 0
        self.quotes_inserted = 0

    async def connect_db(self) -> None:
        logger.info("Connecting to database...")
        self.db_pool = await asyncpg.create_pool(
            self.db_url,
            min_size=1,
            max_size=10,
            command_timeout=60,
        )

        async with self.db_pool.acquire() as conn:
            self.has_domain_column = bool(
                await conn.fetchval(
                    """
                    SELECT EXISTS (
                        SELECT 1
                        FROM information_schema.columns
                        WHERE table_schema = 'public'
                          AND table_name = 'clob_quote_ticks'
                          AND column_name = 'domain'
                    )
                    """
                )
            )

        logger.info(
            "Database connected (clob_quote_ticks.domain=%s)",
            "present" if self.has_domain_column else "absent",
        )

    async def close_db(self) -> None:
        if self.db_pool is not None:
            await self.db_pool.close()
            logger.info("Database connection closed")

    async def get_active_markets(self) -> list[dict[str, Any]]:
        assert self.db_pool is not None

        query = """
        SELECT
            market_slug,
            symbol,
            start_time,
            end_time,
            ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->0)::text AS up_token_raw,
            ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->1)::text AS down_token_raw
        FROM pm_market_metadata
        WHERE symbol = ANY($1)
          AND market_slug LIKE $2
          AND end_time > NOW()
          AND start_time < NOW() + ($3 * INTERVAL '1 hour')
          AND raw_market->'markets'->0->'clobTokenIds' IS NOT NULL
        ORDER BY start_time;
        """

        pattern = f"%-updown-{self.timeframe}-%"
        async with self.db_pool.acquire() as conn:
            rows = await conn.fetch(query, self.symbols, pattern, self.lookahead_hours)

        markets: list[dict[str, Any]] = []
        for row in rows:
            markets.append(
                {
                    "slug": row["market_slug"],
                    "symbol": row["symbol"],
                    "start_time": row["start_time"],
                    "end_time": row["end_time"],
                    "up_token": normalize_token_id(row["up_token_raw"]),
                    "down_token": normalize_token_id(row["down_token_raw"]),
                }
            )
        return markets

    def build_subscription_message(self, operation: str, token_ids: set[str]) -> dict[str, Any]:
        payload: dict[str, Any] = {
            "type": "market",
            "operation": operation,
            "markets": [],
            "assets_ids": sorted(token_ids),
        }
        if operation == "subscribe":
            payload["initial_dump"] = True
            payload["custom_feature_enabled"] = True
        return payload

    async def send_subscription_update(self, operation: str, token_ids: set[str]) -> None:
        if self.ws is None or not token_ids:
            return

        payload = self.build_subscription_message(operation, token_ids)
        await self.ws.send(json.dumps(payload))
        logger.info("%s %d token(s)", operation, len(token_ids))

    async def refresh_subscriptions(self) -> None:
        logger.info("Refreshing market subscriptions...")
        markets = await self.get_active_markets()
        logger.info("Found %d active markets", len(markets))

        new_tokens: set[str] = set()
        new_metadata: dict[str, dict[str, Any]] = {}
        for market in markets:
            for side, token_id in (("UP", market["up_token"]), ("DOWN", market["down_token"])):
                new_tokens.add(token_id)
                new_metadata[token_id] = {
                    "slug": market["slug"],
                    "symbol": market["symbol"],
                    "side": side,
                    "end_time": market["end_time"],
                }

        added = new_tokens - self.subscribed_tokens
        removed = self.subscribed_tokens - new_tokens

        if removed:
            await self.send_subscription_update("unsubscribe", removed)
        if added:
            await self.send_subscription_update("subscribe", added)

        self.subscribed_tokens = new_tokens
        self.token_metadata = new_metadata
        self.last_refresh = datetime.now(timezone.utc)
        logger.info("Active subscriptions: %d tokens", len(self.subscribed_tokens))

    async def insert_quote(
        self,
        token_id: str,
        best_bid: float | None,
        best_ask: float | None,
        bid_size: float | None = None,
        ask_size: float | None = None,
    ) -> None:
        if best_bid is None or best_ask is None:
            return

        metadata = self.token_metadata.get(token_id)
        if metadata is None or self.db_pool is None:
            return

        if self.has_domain_column:
            query = """
            INSERT INTO clob_quote_ticks (
                token_id, side, best_bid, best_ask, bid_size, ask_size,
                received_at, source, domain
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
            """
            args = (
                token_id,
                metadata["side"],
                best_bid,
                best_ask,
                bid_size,
                ask_size,
                datetime.now(timezone.utc),
                "polymarket_ws_collector",
                "Crypto",
            )
        else:
            query = """
            INSERT INTO clob_quote_ticks (
                token_id, side, best_bid, best_ask, bid_size, ask_size,
                received_at, source
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            """
            args = (
                token_id,
                metadata["side"],
                best_bid,
                best_ask,
                bid_size,
                ask_size,
                datetime.now(timezone.utc),
                "polymarket_ws_collector",
            )

        async with self.db_pool.acquire() as conn:
            await conn.execute(query, *args)

        self.quotes_inserted += 1
        if token_id not in self.first_quote_tokens:
            self.first_quote_tokens.add(token_id)
            logger.info(
                "First quote %s %s %s bid=%.4f ask=%.4f",
                metadata["symbol"],
                metadata["side"],
                token_id,
                best_bid,
                best_ask,
            )

    async def handle_book_update(self, message: dict[str, Any]) -> None:
        token_id = str(message.get("asset_id") or "")
        if token_id not in self.subscribed_tokens:
            return

        bids = message.get("bids") or []
        asks = message.get("asks") or []
        best_bid, bid_size = best_bid_from_levels(bids)
        best_ask, ask_size = best_ask_from_levels(asks)

        if best_bid is None or best_ask is None:
            return

        self.quotes_received += 1
        await self.insert_quote(token_id, best_bid, best_ask, bid_size, ask_size)

    async def handle_best_bid_ask(self, message: dict[str, Any]) -> None:
        token_id = str(message.get("asset_id") or "")
        if token_id not in self.subscribed_tokens:
            return

        try:
            best_bid = float(message["best_bid"])
            best_ask = float(message["best_ask"])
        except (KeyError, TypeError, ValueError):
            return

        self.quotes_received += 1
        await self.insert_quote(token_id, best_bid, best_ask)

    async def handle_message(self, payload: Any) -> None:
        if isinstance(payload, list):
            for item in payload:
                await self.handle_message(item)
            return

        if not isinstance(payload, dict):
            return

        event_type = payload.get("event_type")
        if event_type == "book":
            await self.handle_book_update(payload)
        elif event_type == "best_bid_ask":
            await self.handle_best_bid_ask(payload)
        elif event_type == "price_change":
            for entry in payload.get("price_changes") or []:
                token_id = str(entry.get("asset_id") or "")
                if token_id not in self.subscribed_tokens:
                    continue
                best_bid_raw = entry.get("best_bid")
                best_ask_raw = entry.get("best_ask")
                if best_bid_raw is None or best_ask_raw is None:
                    continue
                try:
                    best_bid = float(best_bid_raw)
                    best_ask = float(best_ask_raw)
                except (TypeError, ValueError):
                    continue
                self.quotes_received += 1
                await self.insert_quote(token_id, best_bid, best_ask)
        elif event_type:
            logger.debug("Ignoring event_type=%s", event_type)

        if self.quotes_received and self.quotes_received % 100 == 0:
            logger.info(
                "Stats: received=%d inserted=%d active_tokens=%d",
                self.quotes_received,
                self.quotes_inserted,
                len(self.subscribed_tokens),
            )

    async def websocket_loop(self) -> None:
        while self.running:
            try:
                logger.info("Connecting to %s...", self.ws_url)
                async with websockets.connect(
                    self.ws_url,
                    ping_interval=20,
                    ping_timeout=10,
                ) as ws:
                    self.ws = ws
                    logger.info("WebSocket connected")

                    await self.refresh_subscriptions()

                    async for raw_message in ws:
                        try:
                            payload = json.loads(raw_message)
                        except json.JSONDecodeError:
                            logger.warning("Non-JSON WS message: %s", raw_message[:200])
                            continue

                        await self.handle_message(payload)

                        if (
                            self.last_refresh is not None
                            and datetime.now(timezone.utc) - self.last_refresh
                            > timedelta(seconds=self.refresh_seconds)
                        ):
                            await self.refresh_subscriptions()
            except ConnectionClosed:
                logger.warning("WebSocket closed; reconnecting in 5s")
                await asyncio.sleep(5)
            except Exception as exc:
                logger.error("WebSocket loop error: %s; reconnecting in 10s", exc)
                await asyncio.sleep(10)
            finally:
                self.ws = None

    async def run(self) -> None:
        self.loop = asyncio.get_running_loop()
        await self.connect_db()
        try:
            await self.websocket_loop()
        finally:
            await self.close_db()

    def stop(self) -> None:
        logger.info("Stopping collector...")
        self.running = False
        if self.loop is not None and self.ws is not None:
            self.loop.call_soon_threadsafe(
                lambda: asyncio.create_task(self.ws.close())
            )


async def main() -> int:
    parser = argparse.ArgumentParser(description="Polymarket Quote Collector")
    parser.add_argument(
        "--symbols",
        default="BTCUSDT,ETHUSDT,SOLUSDT",
        help="Comma-separated symbols (default: BTCUSDT,ETHUSDT,SOLUSDT)",
    )
    parser.add_argument(
        "--timeframe",
        default="5m",
        choices=["5m", "15m"],
        help="Market timeframe (default: 5m)",
    )
    parser.add_argument(
        "--db-url",
        default="postgresql://postgres:postgres@localhost:5432/ploy",
        help="PostgreSQL connection URL",
    )
    parser.add_argument(
        "--lookahead-hours",
        type=int,
        default=2,
        help="How far ahead to subscribe to upcoming windows (default: 2)",
    )
    parser.add_argument(
        "--refresh-seconds",
        type=int,
        default=300,
        help="How often to refresh active-market subscriptions (default: 300)",
    )
    args = parser.parse_args()

    collector = PolymarketQuoteCollector(
        db_url=args.db_url,
        symbols=[item.strip() for item in args.symbols.split(",") if item.strip()],
        timeframe=args.timeframe,
        lookahead_hours=args.lookahead_hours,
        refresh_seconds=args.refresh_seconds,
    )

    def signal_handler(sig: int, _frame: Any) -> None:
        logger.info("Received signal %s", sig)
        collector.stop()

    signal.signal(signal.SIGINT, signal_handler)
    signal.signal(signal.SIGTERM, signal_handler)

    logger.info("=" * 60)
    logger.info("Polymarket Quote Collector Starting")
    logger.info("Symbols: %s", ",".join(collector.symbols))
    logger.info("Timeframe: %s", args.timeframe)
    logger.info("Lookahead hours: %s", args.lookahead_hours)
    logger.info("=" * 60)

    await collector.run()
    logger.info("Collector stopped")
    return 0


if __name__ == "__main__":
    raise SystemExit(asyncio.run(main()))
