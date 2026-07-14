#!/usr/bin/env python3
"""Extract replay settlement evidence from the dry-run report being compared."""

from __future__ import annotations

import argparse
import json
import re
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any


ENTRY_INTENT_RE = re.compile(r"^tl_[a-z0-9]+_(up|down)_([^_]+)_(\d+)$", re.IGNORECASE)
SETTLEMENT_INTENT_RE = re.compile(r"^tl_settle_([^_]+)_(up|down)$", re.IGNORECASE)


def parse_ts(value: str | None) -> datetime | None:
    if not value:
        return None
    text = value.strip()
    if text.endswith("Z"):
        text = text[:-1] + "+00:00"
    try:
        parsed = datetime.fromisoformat(text)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def infer_market_side(intent_id: Any) -> str | None:
    if intent_id is None:
        return None
    text = str(intent_id)
    if match := ENTRY_INTENT_RE.match(text):
        return match.group(1).upper()
    if match := SETTLEMENT_INTENT_RE.match(text):
        return match.group(2).upper()
    return None


def is_open(value: Any) -> bool:
    if value is None:
        return True
    return str(value).strip().lower() in {"", "open"}


def resolved_up_won(market_side: str, settlement: Any) -> bool | None:
    try:
        price = Decimal(str(settlement))
    except InvalidOperation:
        return None
    side = market_side.upper()
    if side == "UP" and price >= Decimal("0.99"):
        return True
    if side == "UP" and price <= Decimal("0.01"):
        return False
    if side == "DOWN" and price >= Decimal("0.99"):
        return False
    if side == "DOWN" and price <= Decimal("0.01"):
        return True
    return None


def iter_runtime_events(payload: Any) -> list[dict[str, Any]]:
    if not isinstance(payload, dict):
        return []
    rows = ((payload.get("runtime_evidence") or {}).get("events") or [])
    return [row for row in rows if isinstance(row, dict)]


def extract_settlements(
    payload: Any,
    *,
    deployment_id: str | None,
    since: datetime | None,
    until: datetime | None,
) -> list[dict[str, Any]]:
    settlements: dict[tuple[str, str], dict[str, Any]] = {}
    for row in iter_runtime_events(payload):
        if deployment_id and row.get("deployment_id") != deployment_id:
            continue
        ts = parse_ts(str(row.get("decision_ts"))) if row.get("decision_ts") is not None else None
        if since and (ts is None or ts < since):
            continue
        if until and (ts is None or ts > until):
            continue
        if str(row.get("side") or "").upper() == "SELL":
            continue
        event_id = row.get("event_id") or row.get("market_id")
        token_id = row.get("token_id")
        settlement = row.get("settlement")
        if event_id is None or token_id is None or is_open(settlement):
            continue
        market_side = row.get("market_side") or infer_market_side(row.get("intent_id"))
        if market_side is None:
            continue
        resolved = resolved_up_won(str(market_side), settlement)
        if resolved is None:
            continue
        key = (str(event_id), str(token_id))
        settlements[key] = {
            "event_id": str(event_id),
            "token_id": str(token_id),
            "settlement": str(settlement),
            "resolved_up_won": resolved,
        }
    return sorted(settlements.values(), key=lambda item: (item["event_id"], item["token_id"]))


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--dryrun-json", required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--deployment-id", default="")
    parser.add_argument("--since", default="")
    parser.add_argument("--until", default="")
    args = parser.parse_args()

    with Path(args.dryrun_json).open(encoding="utf-8") as handle:
        payload = json.load(handle)

    settlements = extract_settlements(
        payload,
        deployment_id=args.deployment_id or None,
        since=parse_ts(args.since),
        until=parse_ts(args.until),
    )

    output_path = Path(args.output_json)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("w", encoding="utf-8") as handle:
        json.dump(settlements, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(json.dumps({"settlement_event_count": len(settlements)}, sort_keys=True))


if __name__ == "__main__":
    main()
