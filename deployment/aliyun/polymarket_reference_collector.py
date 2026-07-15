#!/usr/bin/env python3
"""Collect Polymarket market metadata, public trades, and settlements to NDJSON."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import os
import re
import shutil
import time
import urllib.parse
import urllib.request
from datetime import datetime, timedelta, timezone
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any


GAMMA_MARKETS_URL = "https://gamma-api.polymarket.com/markets/keyset"
GAMMA_MARKET_URL = "https://gamma-api.polymarket.com/markets/{market_id}"
DATA_TRADES_URL = "https://data-api.polymarket.com/trades"
ACTIVE_TAPE = "market-updates.ndjson"
USER_AGENT = "monday-polymarket-reference-collector/1.0"
SETTLEMENT_PRICE = Decimal("0.999")
TRADE_ID_VERSION = "v2"

SYMBOL_ALIASES = (
    ("BTCUSDT", ("BITCOIN", "BTC")),
    ("ETHUSDT", ("ETHEREUM", "ETH")),
    ("SOLUSDT", ("SOLANA", "SOL ")),
    ("XRPUSDT", ("XRP",)),
    ("DOGEUSDT", ("DOGECOIN", "DOGE")),
    ("HYPEUSDT", ("HYPERLIQUID", "HYPE")),
    ("BNBUSDT", ("BINANCE COIN", "BNB")),
)


class DataCompletenessError(RuntimeError):
    """Raised when a persisted health failure requires a systemd restart."""


def utc_now() -> datetime:
    return datetime.now(timezone.utc)


def iso_z(value: datetime) -> str:
    return value.astimezone(timezone.utc).isoformat(timespec="microseconds").replace("+00:00", "Z")


def parse_datetime(value: Any) -> datetime | None:
    if not isinstance(value, str) or not value:
        return None
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def parse_json_array(value: Any) -> list[Any]:
    if isinstance(value, list):
        return value
    if isinstance(value, str):
        try:
            parsed = json.loads(value)
        except json.JSONDecodeError:
            return []
        return parsed if isinstance(parsed, list) else []
    return []


def infer_symbol(question: Any) -> str | None:
    if not isinstance(question, str):
        return None
    upper = question.upper()
    for symbol, aliases in SYMBOL_ALIASES:
        if any(alias in upper for alias in aliases):
            return symbol
    return None


def market_start_time(market: dict[str, Any]) -> datetime | None:
    candidates = [market.get("eventStartTime"), market.get("startDate")]
    events = market.get("events")
    if isinstance(events, list) and events and isinstance(events[0], dict):
        candidates.extend((events[0].get("startTime"), events[0].get("startDate")))
    return next((parsed for value in candidates if (parsed := parse_datetime(value)) is not None), None)


def infer_window_seconds(market: dict[str, Any]) -> int | None:
    end = parse_datetime(market.get("endDate"))
    start = market_start_time(market)
    if start is not None and end is not None:
        duration = round((end - start).total_seconds())
        if duration in (300, 900):
            return duration

    text = f"{market.get('slug', '')} {market.get('question', '')}".lower()
    if re.search(r"(?:^|[-_ ])15m(?:[-_ ]|$)|15 minutes?", text):
        return 900
    if re.search(r"(?:^|[-_ ])5m(?:[-_ ]|$)|5 minutes?", text):
        return 300
    return None


def is_target_market(market: dict[str, Any], symbols: set[str]) -> tuple[str, int] | None:
    symbol = infer_symbol(market.get("question"))
    window_seconds = infer_window_seconds(market)
    condition_id = market.get("conditionId")
    token_ids = parse_json_array(market.get("clobTokenIds"))
    if (
        symbol not in symbols
        or window_seconds not in (300, 900)
        or not isinstance(condition_id, str)
        or not condition_id
        or len(token_ids) != 2
    ):
        return None
    return symbol, window_seconds


def stable_trade_id(trade: dict[str, Any]) -> str:
    parts = (
        str(trade.get("transactionHash", "")),
        str(trade.get("conditionId", "")),
        str(trade.get("asset", "")),
        str(trade.get("side", "")),
        str(trade.get("timestamp", "")),
        str(trade.get("proxyWallet", "")),
        canonical_decimal(trade.get("size")),
        canonical_decimal(trade.get("price")),
        str(trade.get("outcomeIndex", "")),
    )
    return hashlib.sha256("|".join(parts).encode()).hexdigest()


def canonical_decimal(value: Any) -> str:
    try:
        normalized = Decimal(str(value)).normalize()
    except (InvalidOperation, ValueError):
        return str(value)
    return format(normalized, "f")


def stable_payload_hash(payload: dict[str, Any]) -> str:
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode()
    return hashlib.sha256(encoded).hexdigest()


def advance_trade_finalization(
    tracked: dict[str, Any],
    now: datetime,
    retrieved_at: str,
    new_trade_count: int,
    truncated: bool,
    was_settled: bool,
    lag_seconds: int,
    stable_polls_required: int,
) -> bool:
    """Require both an indexing delay and stable polls before ending trade overlap."""
    tracked.setdefault("settlement_seen_at", retrieved_at)
    if new_trade_count:
        tracked["last_trade_change_at"] = retrieved_at
        tracked["trade_finalization_stable_polls"] = 0
    anchors = [
        value
        for value in (
            parse_datetime(tracked.get("settlement_seen_at")),
            parse_datetime(tracked.get("last_trade_change_at")),
        )
        if value is not None
    ]
    lag_elapsed = bool(anchors) and (now - max(anchors)).total_seconds() >= lag_seconds
    if not lag_elapsed or truncated or new_trade_count or not was_settled:
        tracked["trade_finalization_stable_polls"] = 0
        return False
    tracked["trade_finalization_stable_polls"] = int(
        tracked.get("trade_finalization_stable_polls", 0)
    ) + 1
    return int(tracked["trade_finalization_stable_polls"]) >= stable_polls_required


def settlement_from_market(
    market: dict[str, Any], symbol: str, window_seconds: int, retrieved_at: str
) -> dict[str, Any] | None:
    if market.get("closed") is not True:
        return None
    outcomes = [str(value) for value in parse_json_array(market.get("outcomes"))]
    token_ids = [str(value) for value in parse_json_array(market.get("clobTokenIds"))]
    raw_prices = parse_json_array(market.get("outcomePrices"))
    if len(outcomes) != 2 or len(token_ids) != 2 or len(raw_prices) != 2:
        return None
    try:
        prices = [Decimal(str(value)) for value in raw_prices]
    except (InvalidOperation, ValueError):
        return None
    winners = [index for index, price in enumerate(prices) if price >= SETTLEMENT_PRICE]
    if len(winners) != 1:
        return None
    winner = winners[0]
    winning_outcome = outcomes[winner]
    lowered = winning_outcome.lower()
    resolved_up_won = lowered in {"up", "yes"}
    if lowered not in {"up", "down", "yes", "no"}:
        resolved_up_won = None
    return {
        "kind": "market_settlement",
        "market_id": str(market.get("id", "")),
        "condition_id": str(market.get("conditionId", "")),
        "symbol": symbol,
        "market_window_secs": window_seconds,
        "winning_token_id": token_ids[winner],
        "winning_outcome": winning_outcome,
        "resolved_up_won": resolved_up_won,
        "resolution_source": "gamma_api_closed_market",
        "retrieved_at": retrieved_at,
        "market": market,
    }


def atomic_json(path: Path, payload: dict[str, Any]) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("w", encoding="utf-8") as handle:
        json.dump(payload, handle, sort_keys=True, separators=(",", ":"))
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)


def load_json(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
        return value if isinstance(value, dict) else {}
    except (FileNotFoundError, json.JSONDecodeError):
        return {}


class TapeWriter:
    def __init__(self, spool_dir: Path) -> None:
        self.spool_dir = spool_dir
        self.spool_dir.mkdir(parents=True, exist_ok=True)
        self.active = spool_dir / ACTIVE_TAPE
        self.hour: str | None = None
        self.sequence = 0
        self.handle = None
        self._recover_active()

    def _recover_active(self) -> None:
        if not self.active.exists() or self.active.stat().st_size == 0:
            self.handle = self.active.open("ab", buffering=0)
            return
        expected = 0
        last_complete_offset = 0
        first_recorded: datetime | None = None
        with self.active.open("rb") as handle:
            for raw_line in handle:
                if not raw_line.endswith(b"\n"):
                    break
                row = json.loads(raw_line)
                if row.get("sequence") != expected:
                    raise ValueError(f"active tape sequence gap expected={expected}")
                if first_recorded is None:
                    first_recorded = parse_datetime(row.get("recorded_at"))
                expected += 1
                last_complete_offset = handle.tell()
        if last_complete_offset != self.active.stat().st_size:
            with self.active.open("r+b") as handle:
                handle.truncate(last_complete_offset)
                handle.flush()
                os.fsync(handle.fileno())
        self.sequence = expected
        self.hour = first_recorded.strftime("%Y%m%dT%H") if first_recorded else None
        self.handle = self.active.open("ab", buffering=0)

    def _rotate(self, now: datetime) -> None:
        if self.handle is not None:
            self.handle.flush()
            os.fsync(self.handle.fileno())
            self.handle.close()
        if self.active.exists() and self.active.stat().st_size > 0:
            stamp = now.strftime("%Y%m%dT%H%M%S%f")
            os.replace(self.active, self.spool_dir / f"market-updates.{stamp}.ndjson")
        self.sequence = 0
        self.hour = None
        self.handle = self.active.open("ab", buffering=0)

    def write_updates(self, updates: list[dict[str, Any]], now: datetime) -> None:
        if not updates:
            return
        target_hour = now.strftime("%Y%m%dT%H")
        if self.hour is not None and self.hour != target_hour:
            self._rotate(now)
        self.hour = target_hour
        recorded_at = iso_z(now)
        start_offset = os.fstat(self.handle.fileno()).st_size
        start_sequence = self.sequence
        try:
            for update in updates:
                row = {"sequence": self.sequence, "recorded_at": recorded_at, "update": update}
                encoded = (
                    json.dumps(row, sort_keys=True, separators=(",", ":")).encode() + b"\n"
                )
                self._write_all(self.handle, encoded)
                self.sequence += 1
            self.handle.flush()
            os.fsync(self.handle.fileno())
        except BaseException:
            self.sequence = start_sequence
            os.ftruncate(self.handle.fileno(), start_offset)
            os.fsync(self.handle.fileno())
            raise

    @staticmethod
    def _write_all(handle: Any, encoded: bytes) -> None:
        view = memoryview(encoded)
        written = 0
        while written < len(view):
            count = handle.write(view[written:])
            if not isinstance(count, int) or count <= 0:
                raise OSError("tape write made no progress")
            written += count

    def close(self) -> None:
        if self.handle is not None:
            self.handle.flush()
            os.fsync(self.handle.fileno())
            self.handle.close()
            self.handle = None


class ReferenceCollector:
    def __init__(self, args: argparse.Namespace) -> None:
        self.args = args
        self.symbols = set(args.symbols)
        self.spool_dir = args.spool_dir
        self.state_path = self.spool_dir / "collector-state.json"
        self.health_path = self.spool_dir / "health.json"
        self.state = load_json(self.state_path)
        self.state.setdefault("markets", {})
        self.state.setdefault("trade_seen", {})
        if self.state.get("trade_id_version") != TRADE_ID_VERSION:
            active = self.spool_dir / ACTIVE_TAPE
            if active.exists() and active.stat().st_size:
                quarantine = self.spool_dir / f"superseded-v1-{ACTIVE_TAPE}.{time.time_ns()}"
                os.replace(active, quarantine)
            self.state["trade_seen"] = {}
            for tracked in self.state["markets"].values():
                if isinstance(tracked, dict):
                    tracked["trade_complete"] = False
            self.state["trade_id_version"] = TRADE_ID_VERSION
        self.writer = TapeWriter(self.spool_dir)
        self.recover_state_from_active_tape()
        self.last_success_monotonic = time.monotonic()

    def recover_state_from_active_tape(self) -> None:
        """Merge durable tape records missing from a pre-crash state checkpoint."""
        if not self.writer.active.exists():
            return
        with self.writer.active.open("rb") as handle:
            for raw_line in handle:
                if not raw_line.endswith(b"\n"):
                    continue
                row = json.loads(raw_line)
                update = row.get("update", {})
                kind = update.get("kind")
                market_id = update.get("market_id")
                condition_id = update.get("condition_id")
                if kind == "polymarket_trade":
                    record_id = update.get("record_id")
                    timestamp = update.get("trade_ts_unix")
                    if (
                        update.get("record_id_version") == TRADE_ID_VERSION
                        and isinstance(condition_id, str)
                        and isinstance(record_id, str)
                        and isinstance(timestamp, int)
                    ):
                        self.state["trade_seen"].setdefault(condition_id, {})[
                            record_id
                        ] = timestamp
                elif kind in {"market_metadata", "market_settlement"} and isinstance(
                    market_id, str
                ):
                    tracked = self.state["markets"].setdefault(market_id, {})
                    for source, target in (
                        ("condition_id", "condition_id"),
                        ("symbol", "symbol"),
                        ("market_window_secs", "market_window_secs"),
                    ):
                        if update.get(source) is not None:
                            tracked[target] = update[source]
                    market = update.get("market")
                    if isinstance(market, dict):
                        tracked["end_time"] = market.get("endDate")
                        tracked["last_metadata_hash"] = stable_payload_hash(market)
                    if kind == "market_settlement":
                        tracked["settled"] = True

    def get_json(self, url: str, params: dict[str, Any] | None = None) -> Any:
        if params:
            url = f"{url}?{urllib.parse.urlencode(params)}"
        request = urllib.request.Request(
            url, headers={"User-Agent": USER_AGENT, "Accept": "application/json"}
        )
        with urllib.request.urlopen(request, timeout=self.args.http_timeout) as response:
            return json.load(response)

    def discover_markets(self, now: datetime) -> list[dict[str, Any]]:
        params: dict[str, Any] = {
            "end_date_min": iso_z(now - timedelta(seconds=self.args.market_lookback_secs)),
            "end_date_max": iso_z(now + timedelta(minutes=30)),
            "closed": "false",
            "limit": 100,
        }
        cursor = None
        markets: list[dict[str, Any]] = []
        while len(markets) < self.args.max_markets:
            call_params = dict(params)
            if cursor:
                call_params["after_cursor"] = cursor
            payload = self.get_json(GAMMA_MARKETS_URL, call_params)
            if not isinstance(payload, dict) or not isinstance(payload.get("markets"), list):
                raise ValueError("Gamma keyset response is missing markets")
            page = [item for item in payload["markets"] if isinstance(item, dict)]
            markets.extend(page)
            cursor = payload.get("next_cursor")
            if len(page) < 100 or not isinstance(cursor, str) or not cursor:
                break
        return markets[: self.args.max_markets]

    def fetch_trades(self, condition_id: str) -> tuple[list[dict[str, Any]], bool]:
        trades: list[dict[str, Any]] = []
        truncated = False
        for offset in (0, 10_000):
            payload = self.get_json(
                DATA_TRADES_URL,
                {
                    "market": condition_id,
                    "limit": 10_000,
                    "offset": offset,
                    "takerOnly": "false",
                },
            )
            if not isinstance(payload, list):
                raise ValueError("Data API trades response is not an array")
            page = [item for item in payload if isinstance(item, dict)]
            trades.extend(page)
            if len(page) < 10_000:
                break
            if offset == 10_000:
                truncated = True
        return trades, truncated

    def trade_updates(
        self,
        market_id: str,
        condition_id: str,
        symbol: str,
        window_seconds: int,
        trades: list[dict[str, Any]],
        now: datetime,
        state: dict[str, Any] | None = None,
    ) -> tuple[list[dict[str, Any]], dict[str, int]]:
        state = self.state if state is None else state
        seen = state["trade_seen"].setdefault(condition_id, {})
        cutoff = int(now.timestamp()) - self.args.market_lookback_secs
        updates = []
        parsed_trades: list[tuple[int, dict[str, Any]]] = []
        malformed_reasons: dict[str, int] = {}

        def reject(reason: str) -> None:
            malformed_reasons[reason] = malformed_reasons.get(reason, 0) + 1

        for trade in trades:
            try:
                timestamp = int(trade.get("timestamp"))
            except (TypeError, ValueError):
                reject("invalid_timestamp")
                continue
            if trade.get("conditionId") != condition_id:
                reject("condition_mismatch")
                continue
            if not isinstance(trade.get("transactionHash"), str) or not trade["transactionHash"]:
                reject("missing_transaction_hash")
                continue
            if not isinstance(trade.get("asset"), str) or not trade["asset"]:
                reject("missing_asset")
                continue
            if trade.get("side") not in {"BUY", "SELL"}:
                reject("invalid_side")
                continue
            if not isinstance(trade.get("proxyWallet"), str) or not trade["proxyWallet"]:
                reject("missing_proxy_wallet")
                continue
            try:
                size = Decimal(str(trade.get("size")))
            except (InvalidOperation, ValueError):
                size = Decimal("NaN")
            if not size.is_finite() or size <= 0:
                reject("invalid_size")
                continue
            try:
                price = Decimal(str(trade.get("price")))
            except (InvalidOperation, ValueError):
                price = Decimal("NaN")
            if not price.is_finite() or not Decimal("0") <= price <= Decimal("1"):
                reject("invalid_price")
                continue
            if trade.get("outcomeIndex") not in {0, 1}:
                reject("invalid_outcome_index")
                continue
            if not isinstance(trade.get("outcome"), str) or not trade["outcome"]:
                reject("missing_outcome")
                continue
            parsed_trades.append((timestamp, trade))
        for timestamp, trade in sorted(parsed_trades, key=lambda value: value[0]):
            record_id = stable_trade_id(trade)
            if timestamp < cutoff or record_id in seen:
                continue
            seen[record_id] = timestamp
            updates.append(
                {
                    "kind": "polymarket_trade",
                    "record_id": record_id,
                    "record_id_version": TRADE_ID_VERSION,
                    "market_id": market_id,
                    "condition_id": condition_id,
                    "token_id": str(trade.get("asset", "")),
                    "symbol": symbol,
                    "market_window_secs": window_seconds,
                    "side": trade.get("side"),
                    "size": trade.get("size"),
                    "price": trade.get("price"),
                    "trade_ts": iso_z(datetime.fromtimestamp(timestamp, tz=timezone.utc)),
                    "trade_ts_unix": timestamp,
                    "transaction_hash": trade.get("transactionHash"),
                    "proxy_wallet": trade.get("proxyWallet"),
                    "outcome": trade.get("outcome"),
                    "outcome_index": trade.get("outcomeIndex"),
                    "source": "polymarket_data_api",
                    "received_at": iso_z(now),
                    "trade": trade,
                }
            )
        state["trade_seen"][condition_id] = {
            key: timestamp for key, timestamp in seen.items() if int(timestamp) >= cutoff
        }
        return updates, malformed_reasons

    def collect_once(self) -> dict[str, Any]:
        now = utc_now()
        retrieved_at = iso_z(now)
        updates: list[dict[str, Any]] = []
        errors: list[str] = []
        truncated_markets: list[str] = []
        trade_polls = 0
        successful_trade_polls = 0
        malformed_trade_reasons: dict[str, int] = {}
        overdue_unresolved_markets: list[str] = []
        next_state = copy.deepcopy(self.state)
        discovered = self.discover_markets(now)
        targets: dict[str, tuple[dict[str, Any], str, int]] = {}

        for market in discovered:
            target = is_target_market(market, self.symbols)
            if target is None:
                continue
            symbol, window_seconds = target
            market_id = str(market.get("id", ""))
            condition_id = str(market.get("conditionId", ""))
            targets[market_id] = (market, symbol, window_seconds)
            previous = next_state["markets"].get(market_id, {})
            tracked = dict(previous)
            tracked.update({
                "condition_id": condition_id,
                "symbol": symbol,
                "market_window_secs": window_seconds,
                "end_time": market.get("endDate"),
                "settled": bool(previous.get("settled", False)),
                "trade_complete": bool(previous.get("trade_complete", False)),
                "last_metadata_hash": previous.get("last_metadata_hash"),
            })
            next_state["markets"][market_id] = tracked

        for market_id, tracked in list(next_state["markets"].items()):
            if not isinstance(tracked, dict):
                continue
            end_time = parse_datetime(tracked.get("end_time"))
            if end_time is not None and end_time < now - timedelta(seconds=self.args.market_lookback_secs):
                if tracked.get("settled") and tracked.get("trade_complete"):
                    next_state["markets"].pop(market_id, None)
                    condition_id = tracked.get("condition_id")
                    if isinstance(condition_id, str):
                        next_state["trade_seen"].pop(condition_id, None)
                    continue
                if end_time < now - timedelta(seconds=self.args.settlement_lookback_secs):
                    overdue_unresolved_markets.append(market_id)
            if (
                market_id not in targets
                and end_time is not None
                and end_time <= now
                and not (tracked.get("settled") and tracked.get("trade_complete"))
            ):
                try:
                    market = self.get_json(GAMMA_MARKET_URL.format(market_id=market_id))
                    if isinstance(market, dict):
                        targets[market_id] = (
                            market,
                            str(tracked.get("symbol", "")),
                            int(tracked.get("market_window_secs", 0)),
                        )
                        tracked.pop("settlement_failure_since", None)
                        tracked.pop("settlement_last_error", None)
                except Exception as error:
                    errors.append(f"settlement {market_id}: {error}")
                    tracked.setdefault("settlement_failure_since", retrieved_at)
                    tracked["settlement_last_error"] = str(error)

        for market_id, (market, symbol, window_seconds) in targets.items():
            condition_id = str(market.get("conditionId", ""))
            tracked = next_state["markets"].setdefault(market_id, {})
            metadata_hash = stable_payload_hash(market)
            if tracked.get("last_metadata_hash") != metadata_hash:
                updates.append(
                    {
                        "kind": "market_metadata",
                        "market_id": market_id,
                        "condition_id": condition_id,
                        "symbol": symbol,
                        "market_window_secs": window_seconds,
                        "source": "gamma_api",
                        "retrieved_at": retrieved_at,
                        "market": market,
                    }
                )
                tracked["last_metadata_hash"] = metadata_hash

            settlement = settlement_from_market(market, symbol, window_seconds, retrieved_at)
            was_settled = bool(tracked.get("settled"))
            if condition_id and not tracked.get("trade_complete"):
                trade_polls += 1
                try:
                    trades, truncated = self.fetch_trades(condition_id)
                    successful_trade_polls += 1
                    trade_updates, malformed_reasons = self.trade_updates(
                        market_id,
                        condition_id,
                        symbol,
                        window_seconds,
                        trades,
                        now,
                        next_state,
                    )
                    updates.extend(trade_updates)
                    for reason, count in malformed_reasons.items():
                        malformed_trade_reasons[reason] = (
                            malformed_trade_reasons.get(reason, 0) + count
                        )
                    tracked["last_trade_success_at"] = retrieved_at
                    if malformed_reasons:
                        detail = f"malformed trade rows: {dict(sorted(malformed_reasons.items()))}"
                        errors.append(f"trades {condition_id}: {detail}")
                        tracked.setdefault("trade_failure_since", retrieved_at)
                        tracked["trade_last_error"] = detail
                    else:
                        tracked.pop("trade_failure_since", None)
                        tracked.pop("trade_last_error", None)
                    if truncated:
                        truncated_markets.append(condition_id)
                    if settlement is not None:
                        if advance_trade_finalization(
                            tracked,
                            now,
                            retrieved_at,
                            len(trade_updates),
                            truncated or bool(malformed_reasons),
                            was_settled,
                            self.args.trade_finalization_lag_secs,
                            self.args.trade_finalization_stable_polls,
                        ):
                            tracked["trade_complete"] = True
                except Exception as error:
                    errors.append(f"trades {condition_id}: {error}")
                    tracked.setdefault("trade_failure_since", retrieved_at)
                    tracked["trade_last_error"] = str(error)
            if settlement is not None and not tracked.get("settled"):
                updates.append(settlement)
                tracked["settled"] = True
            if self.args.per_market_delay_ms:
                time.sleep(self.args.per_market_delay_ms / 1000)

        stale_trade_markets = sorted(
            str(tracked.get("condition_id"))
            for tracked in next_state["markets"].values()
            if isinstance(tracked, dict)
            and (failed_at := parse_datetime(tracked.get("trade_failure_since"))) is not None
            and (now - failed_at).total_seconds() > self.args.stale_after_secs
        )
        stale_settlement_markets = sorted(
            str(market_id)
            for market_id, tracked in next_state["markets"].items()
            if isinstance(tracked, dict)
            and (failed_at := parse_datetime(tracked.get("settlement_failure_since"))) is not None
            and (now - failed_at).total_seconds() > self.args.stale_after_secs
        )

        self.writer.write_updates(updates, now)
        self.state = next_state
        atomic_json(self.state_path, self.state)
        self.last_success_monotonic = time.monotonic()

        usage = shutil.disk_usage(self.spool_dir)
        health = {
            "updated_at": retrieved_at,
            "last_success_at": retrieved_at,
            "target_markets": len(targets),
            "tracked_markets": len(self.state["markets"]),
            "records_written": len(updates),
            "record_types": {
                kind: sum(1 for item in updates if item.get("kind") == kind)
                for kind in ("market_metadata", "polymarket_trade", "market_settlement")
            },
            "api_errors": errors,
            "trade_polls": trade_polls,
            "successful_trade_polls": successful_trade_polls,
            "malformed_trade_rows": sum(malformed_trade_reasons.values()),
            "malformed_trade_reasons": dict(sorted(malformed_trade_reasons.items())),
            "truncated_trade_markets": truncated_markets,
            "stale_trade_markets": stale_trade_markets,
            "stale_settlement_markets": stale_settlement_markets,
            "overdue_unresolved_markets": sorted(overdue_unresolved_markets),
            "active_tape_bytes": self.writer.active.stat().st_size if self.writer.active.exists() else 0,
            "free_disk_bytes": usage.free,
        }
        atomic_json(self.health_path, health)
        if truncated_markets:
            raise DataCompletenessError(
                f"trade pagination exceeded API offset limit for {truncated_markets}"
            )
        if stale_trade_markets:
            raise DataCompletenessError(f"stale trade markets: {stale_trade_markets}")
        if stale_settlement_markets:
            raise DataCompletenessError(
                f"stale settlement markets: {stale_settlement_markets}"
            )
        return health

    def run(self) -> None:
        try:
            if self.args.once:
                print(
                    json.dumps(self.collect_once(), sort_keys=True, separators=(",", ":")),
                    flush=True,
                )
                return
            while True:
                started = time.monotonic()
                try:
                    health = self.collect_once()
                    print(json.dumps(health, sort_keys=True, separators=(",", ":")), flush=True)
                except Exception as error:
                    print(f"Polymarket reference poll failed: {error}", flush=True)
                    if isinstance(error, DataCompletenessError):
                        raise
                    if time.monotonic() - self.last_success_monotonic > self.args.stale_after_secs:
                        raise
                elapsed = time.monotonic() - started
                time.sleep(max(0.0, self.args.poll_interval_secs - elapsed))
        finally:
            self.writer.close()


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--spool-dir", type=Path, default=Path("/data/monday/spool/polymarket-reference")
    )
    parser.add_argument(
        "--symbols",
        type=lambda value: [item.strip().upper() for item in value.split(",") if item.strip()],
        default=["BTCUSDT", "ETHUSDT", "SOLUSDT", "XRPUSDT", "DOGEUSDT", "HYPEUSDT", "BNBUSDT"],
    )
    parser.add_argument("--poll-interval-secs", type=float, default=30.0)
    parser.add_argument("--market-lookback-secs", type=int, default=7200)
    parser.add_argument("--settlement-lookback-secs", type=int, default=86400)
    parser.add_argument("--max-markets", type=int, default=1200)
    parser.add_argument("--http-timeout", type=float, default=20.0)
    parser.add_argument("--stale-after-secs", type=float, default=180.0)
    parser.add_argument("--trade-finalization-lag-secs", type=int, default=1800)
    parser.add_argument("--trade-finalization-stable-polls", type=int, default=3)
    parser.add_argument("--per-market-delay-ms", type=int, default=100)
    parser.add_argument("--once", action="store_true")
    args = parser.parse_args()
    if (
        args.poll_interval_secs <= 0
        or args.market_lookback_secs <= 0
        or args.settlement_lookback_secs <= 0
        or args.max_markets <= 0
        or args.trade_finalization_lag_secs <= 0
        or args.trade_finalization_stable_polls <= 0
        or args.per_market_delay_ms < 0
    ):
        parser.error("poll interval, market lookback, and max markets must be positive")
    return args


if __name__ == "__main__":
    ReferenceCollector(parse_args()).run()
