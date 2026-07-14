#!/usr/bin/env python3
"""Import Polymarket V2 Envio indexer events into the Ploy database.

The importer is intentionally a sidecar bridge. It persists chain-indexed truth
for reconciliation and research, but it does not feed realtime strategy logic.
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import urllib.error
import urllib.request
from collections import Counter, defaultdict
from dataclasses import dataclass
from datetime import datetime, timezone
from decimal import Decimal
from pathlib import Path
from typing import Any


DB_URL = (
    os.environ.get("PLOY_DATABASE__URL")
    or os.environ.get("DATABASE_URL")
    or "postgresql://postgres:postgres@localhost:5432/ploy"
)

ZERO_BYTES32 = "0x0000000000000000000000000000000000000000000000000000000000000000"

GRAPHQL_FIELDS = {
    "OrderFill": """
      id orderHash maker taker side tokenId makerAmountFilled takerAmountFilled
      fee builder metadata exchange timestamp blockNumber transactionHash txFrom
      market { id slug conditionId }
    """,
    "OrderMatch": """
      id takerOrderHash takerOrderMaker side tokenId makerAmountFilled
      takerAmountFilled exchange timestamp blockNumber transactionHash
      market { id slug conditionId }
    """,
    "FeeEvent": """
      id receiver amount timestamp blockNumber transactionHash
    """,
    "PolyUSDTransfer": """
      id from to amount timestamp blockNumber transactionHash
    """,
    "PolyUSDWrap": """
      id eventType caller asset to amount timestamp blockNumber transactionHash
    """,
}

GRAPHQL_ENTITY_BY_TABLE = {
    "order_fills": "OrderFill",
    "order_matches": "OrderMatch",
    "fee_events": "FeeEvent",
    "polyusd_transfers": "PolyUSDTransfer",
    "polyusd_wraps": "PolyUSDWrap",
}


@dataclass(frozen=True)
class NormalizedEvent:
    table: str
    row: dict[str, Any]


def parse_event_id(value: Any) -> tuple[int, int | None, int | None]:
    """Parse Envio ids like `137_84902320_4` into chain/block/log parts."""

    if not value:
        return (137, None, None)
    parts = str(value).split("_")
    if len(parts) < 3:
        return (137, None, None)
    try:
        return (int(parts[0]), int(parts[1]), int(parts[2]))
    except ValueError:
        return (137, None, None)


def get_any(row: dict[str, Any], *keys: str, default: Any = None) -> Any:
    for key in keys:
        if key in row and row[key] is not None:
            return row[key]
    return default


def parse_timestamp(value: Any) -> datetime:
    if value is None:
        raise ValueError("event timestamp is required")
    if isinstance(value, (int, float)):
        return datetime.fromtimestamp(value, tz=timezone.utc)
    text = str(value)
    if text.isdigit():
        return datetime.fromtimestamp(int(text), tz=timezone.utc)
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    parsed = datetime.fromisoformat(text)
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def as_decimal(value: Any) -> Decimal:
    if value is None:
        return Decimal(0)
    return Decimal(str(value))


def as_text(value: Any, default: str = "") -> str:
    if value is None:
        return default
    return str(value)


def market_id_from(row: dict[str, Any]) -> str | None:
    direct = get_any(row, "market_id", "marketId")
    if direct:
        return str(direct)
    market = row.get("market")
    if isinstance(market, dict):
        return as_text(get_any(market, "slug", "id", "conditionId"), default="") or None
    if market:
        return str(market)
    return None


def normalize_common(row: dict[str, Any]) -> dict[str, Any]:
    chain_id, id_block, id_log = parse_event_id(row.get("id"))
    block_number = int(get_any(row, "blockNumber", "block_number", default=id_block))
    log_index = int(get_any(row, "logIndex", "log_index", default=id_log))
    return {
        "chain_id": int(get_any(row, "chainId", "chain_id", default=chain_id)),
        "block_number": block_number,
        "log_index": log_index,
        "block_timestamp": parse_timestamp(get_any(row, "timestamp", "blockTimestamp")),
        "transaction_hash": as_text(get_any(row, "transactionHash", "transaction_hash")),
    }


def normalize_order_fill(row: dict[str, Any]) -> NormalizedEvent:
    common = normalize_common(row)
    normalized = {
        **common,
        "tx_from": as_text(get_any(row, "txFrom", "tx_from"), default=""),
        "exchange": as_text(row.get("exchange")),
        "order_hash": as_text(get_any(row, "orderHash", "order_hash")),
        "maker": as_text(row.get("maker")),
        "taker": as_text(row.get("taker")),
        "side": int(row.get("side")),
        "token_id": as_text(get_any(row, "tokenId", "token_id")),
        "market_id": market_id_from(row),
        "maker_amount_raw": as_decimal(get_any(row, "makerAmountFilled", "maker_amount_raw")),
        "taker_amount_raw": as_decimal(get_any(row, "takerAmountFilled", "taker_amount_raw")),
        "fee_raw": as_decimal(get_any(row, "fee", "fee_raw")),
        "builder": as_text(row.get("builder"), ZERO_BYTES32),
        "metadata": as_text(row.get("metadata"), ZERO_BYTES32),
        "raw_event": row,
    }
    return NormalizedEvent("polymarket_v2_order_fills", normalized)


def normalize_order_match(row: dict[str, Any]) -> NormalizedEvent:
    common = normalize_common(row)
    normalized = {
        **common,
        "exchange": as_text(row.get("exchange")),
        "taker_order_hash": as_text(get_any(row, "takerOrderHash", "taker_order_hash")),
        "taker_order_maker": as_text(get_any(row, "takerOrderMaker", "taker_order_maker")),
        "side": int(row.get("side")),
        "token_id": as_text(get_any(row, "tokenId", "token_id")),
        "market_id": market_id_from(row),
        "maker_amount_raw": as_decimal(get_any(row, "makerAmountFilled", "maker_amount_raw")),
        "taker_amount_raw": as_decimal(get_any(row, "takerAmountFilled", "taker_amount_raw")),
        "raw_event": row,
    }
    return NormalizedEvent("polymarket_v2_order_matches", normalized)


def normalize_fee_event(row: dict[str, Any]) -> NormalizedEvent:
    common = normalize_common(row)
    normalized = {
        **common,
        "receiver": as_text(row.get("receiver")),
        "amount_raw": as_decimal(get_any(row, "amount", "amount_raw")),
        "raw_event": row,
    }
    return NormalizedEvent("polymarket_v2_fee_events", normalized)


def normalize_polyusd_transfer(row: dict[str, Any]) -> NormalizedEvent:
    common = normalize_common(row)
    normalized = {
        **common,
        "event_type": "transfer",
        "address_from": as_text(get_any(row, "from", "address_from"), default=""),
        "address_to": as_text(get_any(row, "to", "address_to"), default=""),
        "caller": None,
        "asset": None,
        "amount_raw": as_decimal(get_any(row, "amount", "amount_raw")),
        "raw_event": row,
    }
    return NormalizedEvent("polymarket_v2_polyusd_events", normalized)


def normalize_polyusd_wrap(row: dict[str, Any]) -> NormalizedEvent:
    common = normalize_common(row)
    event_type = as_text(get_any(row, "eventType", "event_type"), default="wrap").lower()
    if event_type not in {"wrap", "unwrap"}:
        raise ValueError(f"unsupported PolyUSD wrap eventType={event_type!r}")
    normalized = {
        **common,
        "event_type": event_type,
        "address_from": None,
        "address_to": as_text(get_any(row, "to", "address_to"), default=""),
        "caller": as_text(row.get("caller"), default=""),
        "asset": as_text(row.get("asset"), default=""),
        "amount_raw": as_decimal(get_any(row, "amount", "amount_raw")),
        "raw_event": row,
    }
    return NormalizedEvent("polymarket_v2_polyusd_events", normalized)


NORMALIZERS = {
    "OrderFill": normalize_order_fill,
    "OrderMatch": normalize_order_match,
    "FeeEvent": normalize_fee_event,
    "PolyUSDTransfer": normalize_polyusd_transfer,
    "PolyUSDWrap": normalize_polyusd_wrap,
}


INSERT_SQL = {
    "polymarket_v2_order_fills": """
        INSERT INTO polymarket_v2_order_fills (
          chain_id, block_number, log_index, block_timestamp, transaction_hash,
          tx_from, exchange, order_hash, maker, taker, side, token_id, market_id,
          maker_amount_raw, taker_amount_raw, fee_raw, builder, metadata, raw_event
        ) VALUES (
          %(chain_id)s, %(block_number)s, %(log_index)s, %(block_timestamp)s,
          %(transaction_hash)s, %(tx_from)s, %(exchange)s, %(order_hash)s,
          %(maker)s, %(taker)s, %(side)s, %(token_id)s, %(market_id)s,
          %(maker_amount_raw)s, %(taker_amount_raw)s, %(fee_raw)s, %(builder)s,
          %(metadata)s, %(raw_event)s
        )
        ON CONFLICT (chain_id, block_number, log_index) DO UPDATE SET
          block_timestamp = EXCLUDED.block_timestamp,
          transaction_hash = EXCLUDED.transaction_hash,
          market_id = EXCLUDED.market_id,
          raw_event = EXCLUDED.raw_event,
          ingested_at = NOW()
    """,
    "polymarket_v2_order_matches": """
        INSERT INTO polymarket_v2_order_matches (
          chain_id, block_number, log_index, block_timestamp, transaction_hash,
          exchange, taker_order_hash, taker_order_maker, side, token_id, market_id,
          maker_amount_raw, taker_amount_raw, raw_event
        ) VALUES (
          %(chain_id)s, %(block_number)s, %(log_index)s, %(block_timestamp)s,
          %(transaction_hash)s, %(exchange)s, %(taker_order_hash)s,
          %(taker_order_maker)s, %(side)s, %(token_id)s, %(market_id)s,
          %(maker_amount_raw)s, %(taker_amount_raw)s, %(raw_event)s
        )
        ON CONFLICT (chain_id, block_number, log_index) DO UPDATE SET
          block_timestamp = EXCLUDED.block_timestamp,
          transaction_hash = EXCLUDED.transaction_hash,
          market_id = EXCLUDED.market_id,
          raw_event = EXCLUDED.raw_event,
          ingested_at = NOW()
    """,
    "polymarket_v2_fee_events": """
        INSERT INTO polymarket_v2_fee_events (
          chain_id, block_number, log_index, block_timestamp, transaction_hash,
          receiver, amount_raw, raw_event
        ) VALUES (
          %(chain_id)s, %(block_number)s, %(log_index)s, %(block_timestamp)s,
          %(transaction_hash)s, %(receiver)s, %(amount_raw)s, %(raw_event)s
        )
        ON CONFLICT (chain_id, block_number, log_index) DO UPDATE SET
          block_timestamp = EXCLUDED.block_timestamp,
          amount_raw = EXCLUDED.amount_raw,
          raw_event = EXCLUDED.raw_event,
          ingested_at = NOW()
    """,
    "polymarket_v2_polyusd_events": """
        INSERT INTO polymarket_v2_polyusd_events (
          chain_id, block_number, log_index, block_timestamp, transaction_hash,
          event_type, address_from, address_to, caller, asset, amount_raw, raw_event
        ) VALUES (
          %(chain_id)s, %(block_number)s, %(log_index)s, %(block_timestamp)s,
          %(transaction_hash)s, %(event_type)s, %(address_from)s, %(address_to)s,
          %(caller)s, %(asset)s, %(amount_raw)s, %(raw_event)s
        )
        ON CONFLICT (chain_id, block_number, log_index, event_type) DO UPDATE SET
          block_timestamp = EXCLUDED.block_timestamp,
          amount_raw = EXCLUDED.amount_raw,
          raw_event = EXCLUDED.raw_event,
          ingested_at = NOW()
    """,
}


def graphql_query(entity: str) -> str:
    fields = GRAPHQL_FIELDS[entity]
    return f"""
    query ImportPolymarketV2($limit: Int!, $offset: Int!, $minBlock: Int!) {{
      {entity}(
        limit: $limit,
        offset: $offset,
        where: {{ blockNumber: {{ _gte: $minBlock }} }},
        order_by: [{{ blockNumber: asc }}, {{ id: asc }}]
      ) {{
        {fields}
      }}
    }}
    """


def post_graphql(endpoint: str, query: str, variables: dict[str, Any]) -> dict[str, Any]:
    request = urllib.request.Request(
        endpoint,
        data=json.dumps({"query": query, "variables": variables}).encode("utf-8"),
        headers={"content-type": "application/json"},
        method="POST",
    )
    try:
        with urllib.request.urlopen(request, timeout=30) as response:
            return json.loads(response.read().decode("utf-8"))
    except urllib.error.HTTPError as exc:
        body = exc.read().decode("utf-8", errors="replace")
        raise RuntimeError(f"GraphQL HTTP {exc.code}: {body}") from exc


def fetch_graphql(endpoint: str, entity: str, min_block: int, page_size: int) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    offset = 0
    query = graphql_query(entity)
    while True:
        payload = post_graphql(
            endpoint,
            query,
            {"limit": page_size, "offset": offset, "minBlock": min_block},
        )
        if payload.get("errors"):
            raise RuntimeError(json.dumps(payload["errors"], indent=2))
        page = payload.get("data", {}).get(entity) or []
        rows.extend(page)
        if len(page) < page_size:
            return rows
        offset += page_size


def read_input(path: Path) -> dict[str, list[dict[str, Any]]]:
    if path.suffix == ".jsonl":
        grouped: dict[str, list[dict[str, Any]]] = defaultdict(list)
        with path.open() as handle:
            for line in handle:
                if not line.strip():
                    continue
                payload = json.loads(line)
                entity = payload.pop("entity", None) or payload.pop("_entity", None)
                if not entity:
                    raise ValueError("JSONL rows must include entity or _entity")
                grouped[entity].append(payload)
        return dict(grouped)

    payload = json.loads(path.read_text())
    if isinstance(payload, list):
        grouped = defaultdict(list)
        for item in payload:
            entity = item.pop("entity", None) or item.pop("_entity", None)
            if not entity:
                raise ValueError("JSON array rows must include entity or _entity")
            grouped[entity].append(item)
        return dict(grouped)
    if isinstance(payload, dict):
        return {key: value for key, value in payload.items() if isinstance(value, list)}
    raise ValueError("input must be a JSON object, JSON array, or JSONL")


def normalize_rows(grouped: dict[str, list[dict[str, Any]]]) -> list[NormalizedEvent]:
    events: list[NormalizedEvent] = []
    for entity, rows in grouped.items():
        normalizer = NORMALIZERS.get(entity)
        if normalizer is None:
            raise ValueError(f"unsupported entity {entity!r}")
        for row in rows:
            events.append(normalizer(dict(row)))
    return events


def jsonb_wrapper(value: dict[str, Any]) -> Any:
    try:
        from psycopg.types.json import Jsonb  # type: ignore
    except ImportError as exc:
        raise SystemExit(
            "psycopg is required for DB import. Install with: python3 -m pip install psycopg[binary]"
        ) from exc
    return Jsonb(value)


def write_events(db_url: str, events: list[NormalizedEvent], source: str) -> Counter:
    try:
        import psycopg  # type: ignore
    except ImportError as exc:
        raise SystemExit(
            "psycopg is required for DB import. Install with: python3 -m pip install psycopg[binary]"
        ) from exc

    counts: Counter = Counter()
    max_event: NormalizedEvent | None = None
    with psycopg.connect(db_url) as conn:
        with conn.cursor() as cur:
            for event in events:
                row = dict(event.row)
                row["raw_event"] = jsonb_wrapper(row["raw_event"])
                cur.execute(INSERT_SQL[event.table], row)
                counts[event.table] += 1
                if max_event is None or row["block_number"] > max_event.row["block_number"]:
                    max_event = event

            if max_event is not None:
                cur.execute(
                    """
                    INSERT INTO polymarket_v2_indexer_sync_state (
                      source, last_block_number, last_block_timestamp, last_transaction_hash
                    ) VALUES (%s, %s, %s, %s)
                    ON CONFLICT (source) DO UPDATE SET
                      last_block_number = GREATEST(
                        polymarket_v2_indexer_sync_state.last_block_number,
                        EXCLUDED.last_block_number
                      ),
                      last_block_timestamp = EXCLUDED.last_block_timestamp,
                      last_transaction_hash = EXCLUDED.last_transaction_hash,
                      updated_at = NOW()
                    """,
                    (
                        source,
                        max_event.row["block_number"],
                        max_event.row["block_timestamp"],
                        max_event.row["transaction_hash"],
                    ),
                )
        conn.commit()
    return counts


def read_sync_min_block(db_url: str, source: str, fallback: int) -> int:
    try:
        import psycopg  # type: ignore
    except ImportError as exc:
        raise SystemExit(
            "psycopg is required for sync-state reads. Install with: python3 -m pip install psycopg[binary]"
        ) from exc

    with psycopg.connect(db_url) as conn:
        with conn.cursor() as cur:
            cur.execute(
                "SELECT last_block_number FROM polymarket_v2_indexer_sync_state WHERE source = %s",
                (source,),
            )
            row = cur.fetchone()
    if not row or row[0] is None:
        return fallback
    return max(fallback, int(row[0]) + 1)


def selected_graphql_entities(names: str) -> list[str]:
    entities: list[str] = []
    for name in names.split(","):
        key = name.strip()
        if not key:
            continue
        entity = GRAPHQL_ENTITY_BY_TABLE.get(key, key)
        if entity not in GRAPHQL_FIELDS:
            raise ValueError(f"unknown entity/table selector {key!r}")
        entities.append(entity)
    return entities


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--db-url", default=DB_URL)
    parser.add_argument("--endpoint", default=os.environ.get("PLOY_PM_V2_INDEXER_URL", ""))
    parser.add_argument("--input", type=Path, help="JSON/JSONL export from the Envio indexer")
    parser.add_argument("--min-block", type=int, default=84902320)
    parser.add_argument(
        "--from-sync-state",
        action="store_true",
        help="Start from persisted polymarket_v2_indexer_sync_state when querying GraphQL",
    )
    parser.add_argument("--page-size", type=int, default=250)
    parser.add_argument(
        "--entities",
        default="order_fills,order_matches,fee_events,polyusd_transfers,polyusd_wraps",
        help="Comma-separated entity names or aliases",
    )
    parser.add_argument("--source", default="envio_polymarket_v2_indexer")
    parser.add_argument("--dry-run", action="store_true")
    args = parser.parse_args(argv)

    if not args.input and not args.endpoint:
        parser.error("provide --input or --endpoint/PLOY_PM_V2_INDEXER_URL")

    if args.input:
        grouped = read_input(args.input)
    else:
        if args.from_sync_state:
            args.min_block = read_sync_min_block(args.db_url, args.source, args.min_block)
        grouped = {}
        for entity in selected_graphql_entities(args.entities):
            grouped[entity] = fetch_graphql(args.endpoint, entity, args.min_block, args.page_size)

    events = normalize_rows(grouped)
    counts = Counter(event.table for event in events)
    max_block = max((event.row["block_number"] for event in events), default=None)

    print(
        json.dumps(
            {
                "entities": {entity: len(rows) for entity, rows in grouped.items()},
                "tables": counts,
                "max_block": max_block,
                "dry_run": args.dry_run,
            },
            default=str,
            sort_keys=True,
        )
    )

    if args.dry_run or not events:
        return 0

    written = write_events(args.db_url, events, args.source)
    print(json.dumps({"written": written}, default=str, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
