#!/usr/bin/env python3
"""Attach official settlement prices to replay runtime evidence events."""

from __future__ import annotations

import argparse
import json
import re
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any


ENTRY_INTENT_RE = re.compile(r"^tl_[a-z0-9]+_(up|down)_([^_]+)_(\d+)$", re.IGNORECASE)
SETTLEMENT_INTENT_RE = re.compile(r"^tl_settle_([^_]+)_(up|down)$", re.IGNORECASE)


def load_settlement_prices(path: Path) -> dict[tuple[str, str], str]:
    with path.open(encoding="utf-8") as handle:
        payload = json.load(handle)
    if isinstance(payload, dict):
        payload = payload.get("settlements", [])
    prices: dict[tuple[str, str], str] = {}
    if not isinstance(payload, list):
        return prices
    for item in payload:
        if not isinstance(item, dict):
            continue
        event_id = item.get("event_id")
        token_id = item.get("token_id")
        settlement = item.get("settlement")
        if event_id is None or token_id is None or settlement is None:
            continue
        prices[(str(event_id), str(token_id))] = str(settlement)
    return prices


def is_open_settlement(value: Any) -> bool:
    if value is None:
        return True
    return str(value).strip().lower() in {"", "open"}


def infer_from_intent_id(intent_id: Any) -> tuple[str | None, str | None, str | None]:
    if intent_id is None:
        return None, None, None
    text = str(intent_id)
    if match := ENTRY_INTENT_RE.match(text):
        market_side, event_id, millis = match.groups()
        return event_id, market_side.upper(), millis
    if match := SETTLEMENT_INTENT_RE.match(text):
        event_id, market_side = match.groups()
        return event_id, market_side.upper(), None
    return None, None, None


def iso_from_millis(millis: str | None) -> str | None:
    if millis is None:
        return None
    try:
        value = int(millis)
    except ValueError:
        return None
    from datetime import datetime, timezone

    return datetime.fromtimestamp(value / 1000, timezone.utc).isoformat(timespec="milliseconds").replace(
        "+00:00", "Z"
    )


def runtime_rows_by_intent(payload: dict[str, Any], row_name: str) -> dict[str, dict[str, Any]]:
    runtime = payload.get("runtime_evidence") or {}
    rows = runtime.get(row_name) or []
    if not isinstance(rows, list):
        return {}
    indexed: dict[str, dict[str, Any]] = {}
    for row in rows:
        if not isinstance(row, dict):
            continue
        intent_id = row.get("intent_id")
        if intent_id is not None:
            indexed[str(intent_id)] = row
    return indexed


def event_cashflow_by_event_token(
    payload: dict[str, Any],
    prices: dict[tuple[str, str], str],
) -> dict[tuple[str, str], str]:
    totals: dict[tuple[str, str], Decimal] = {}
    buy_quantities: dict[tuple[str, str], Decimal] = {}
    has_sell_fill: set[tuple[str, str]] = set()
    runtime = payload.get("runtime_evidence") or {}
    fills = runtime.get("fills") or []
    if not isinstance(fills, list):
        return {}
    for fill in fills:
        if not isinstance(fill, dict):
            continue
        intent_id = fill.get("intent_id")
        event_id = fill.get("event_id") or fill.get("market_id")
        if event_id is None:
            event_id, _, _ = infer_from_intent_id(intent_id)
        token_id = fill.get("token_id")
        if event_id is None or token_id is None:
            continue
        try:
            quantity = Decimal(str(fill.get("quantity", "0")))
            price = Decimal(str(fill.get("price", "0")))
            fee = Decimal(str(fill.get("fee", "0")))
        except InvalidOperation:
            continue
        side = str(fill.get("fill_side") or fill.get("side") or "").upper()
        key = (str(event_id), str(token_id))
        signed = quantity * price
        if side == "BUY":
            signed = -signed
            buy_quantities[key] = buy_quantities.get(key, Decimal("0")) + quantity
        elif side != "SELL":
            continue
        else:
            has_sell_fill.add(key)
        totals[key] = totals.get(key, Decimal("0")) + signed - fee
    for key, settlement in prices.items():
        if key in has_sell_fill:
            continue
        buy_quantity = buy_quantities.get(key)
        if buy_quantity is None:
            continue
        try:
            settlement_price = Decimal(str(settlement))
        except InvalidOperation:
            continue
        totals[key] = totals.get(key, Decimal("0")) + buy_quantity * settlement_price
    return {key: format(value.normalize(), "f") for key, value in totals.items()}


def backfill_replay_event_identity(
    payload: dict[str, Any],
    prices: dict[tuple[str, str], str],
) -> int:
    runtime = payload.get("runtime_evidence") or {}
    events = runtime.get("events") or []
    if not isinstance(events, list):
        return 0
    fills_by_intent = runtime_rows_by_intent(payload, "fills")
    orders_by_intent = runtime_rows_by_intent(payload, "orders")
    cashflow_by_event_token = event_cashflow_by_event_token(payload, prices)
    changed = 0

    for event in events:
        if not isinstance(event, dict):
            continue
        intent_id = event.get("intent_id")
        fill = fills_by_intent.get(str(intent_id)) if intent_id is not None else None
        order = orders_by_intent.get(str(intent_id)) if intent_id is not None else None
        inferred_event_id, inferred_market_side, inferred_millis = infer_from_intent_id(intent_id)

        event_id = event.get("event_id") or event.get("market_id") or inferred_event_id
        if event.get("event_id") in (None, "") and event_id is not None:
            event["event_id"] = str(event_id)
            changed += 1
        if event.get("market_id") in (None, "") and event_id is not None:
            event["market_id"] = str(event_id)
            changed += 1
        if event.get("market_side") in (None, "") and inferred_market_side is not None:
            event["market_side"] = inferred_market_side
            changed += 1

        side = event.get("side")
        fill_side = (fill or {}).get("fill_side") or (order or {}).get("order_side")
        if (side is None or str(side).upper() == "UNKNOWN") and fill_side is not None:
            event["side"] = str(fill_side).upper()
            changed += 1

        if event.get("decision_ts") in (None, ""):
            decision_ts = (order or {}).get("created_at") or (fill or {}).get("fill_timestamp") or iso_from_millis(
                inferred_millis
            )
            if decision_ts is not None:
                event["decision_ts"] = decision_ts
                changed += 1

        token_id = event.get("token_id")
        if event_id is not None and token_id is not None and str(event.get("side", "")).upper() == "BUY":
            total_pnl = cashflow_by_event_token.get((str(event_id), str(token_id)))
            if total_pnl is not None:
                event["pnl"] = total_pnl
                changed += 1

    return changed


def enrich_payload(payload: dict[str, Any], prices: dict[tuple[str, str], str]) -> dict[str, int]:
    backfilled = backfill_replay_event_identity(payload, prices)
    events = ((payload.get("runtime_evidence") or {}).get("events") or [])
    stats = {
        "runtime_events": 0,
        "runtime_events_identity_backfilled": backfilled,
        "runtime_events_open": 0,
        "runtime_events_settlement_enriched": 0,
        "settlement_price_count": len(prices),
    }
    if not isinstance(events, list):
        return stats
    for event in events:
        if not isinstance(event, dict):
            continue
        stats["runtime_events"] += 1
        if not is_open_settlement(event.get("settlement")):
            continue
        stats["runtime_events_open"] += 1
        if str(event.get("side") or "").upper() == "SELL":
            continue
        event_id = event.get("event_id") or event.get("market_id")
        token_id = event.get("token_id")
        if event_id is None or token_id is None:
            continue
        settlement = prices.get((str(event_id), str(token_id)))
        if settlement is None:
            continue
        event["settlement"] = settlement
        stats["runtime_events_settlement_enriched"] += 1
    runtime_evidence = payload.setdefault("runtime_evidence", {})
    if isinstance(runtime_evidence, dict):
        runtime_evidence["settlement_enrichment"] = stats
    return stats


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input-json", required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--settlements-json", required=True)
    parser.add_argument("--report-json", required=True)
    args = parser.parse_args()

    input_path = Path(args.input_json)
    with input_path.open(encoding="utf-8") as handle:
        payload = json.load(handle)
    if not isinstance(payload, dict):
        raise SystemExit(f"{input_path}: expected JSON object")

    stats = enrich_payload(payload, load_settlement_prices(Path(args.settlements_json)))

    output_path = Path(args.output_json)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2, sort_keys=True)
        handle.write("\n")

    report_path = Path(args.report_json)
    report_path.parent.mkdir(parents=True, exist_ok=True)
    with report_path.open("w", encoding="utf-8") as handle:
        json.dump(stats, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(json.dumps(stats, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
