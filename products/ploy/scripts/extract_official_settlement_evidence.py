#!/usr/bin/env python3
"""Build replay settlement evidence from official token settlements.

Recorded replay needs two different settlement views:

- event-level `resolved_up_won` for EventExpired replay updates
- token-level settlement prices for runtime evidence PnL enrichment

The recording owns the event semantics (`event_id`, `up_token`, `down_token`).
`pm_token_settlements` owns official token settlement prices. This helper joins
those two surfaces without using dry-run report rows as settlement labels.
"""

from __future__ import annotations

import argparse
import json
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any


def is_winning_price(value: Any) -> bool | None:
    try:
        price = Decimal(str(value))
    except InvalidOperation:
        return None
    if price >= Decimal("0.99"):
        return True
    if price <= Decimal("0.01"):
        return False
    return None


def iter_recording(path: Path):
    with path.open(encoding="utf-8") as handle:
        for line_number, raw in enumerate(handle, start=1):
            line = raw.strip()
            if not line:
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError as exc:
                raise SystemExit(f"{path}:{line_number}: invalid NDJSON: {exc}") from exc
            if not isinstance(record, dict):
                raise SystemExit(f"{path}:{line_number}: expected JSON object record")
            yield record


def extract_recording_token_map(path: Path) -> tuple[dict[str, dict[str, str]], dict[str, tuple[str, str]]]:
    events: dict[str, dict[str, str]] = {}
    token_to_event_side: dict[str, tuple[str, str]] = {}

    for record in iter_recording(path):
        update = record.get("update")
        if not isinstance(update, dict) or update.get("kind") != "event_discovered":
            continue

        event_id = update.get("event_id")
        up_token = update.get("up_token")
        down_token = update.get("down_token")
        if event_id is None or up_token is None or down_token is None:
            continue

        event_key = str(event_id)
        up_key = str(up_token)
        down_key = str(down_token)
        events[event_key] = {"up_token": up_key, "down_token": down_key}
        token_to_event_side[up_key] = (event_key, "UP")
        token_to_event_side[down_key] = (event_key, "DOWN")

    return events, token_to_event_side


def load_db_rows(path: Path) -> list[dict[str, Any]]:
    with path.open(encoding="utf-8") as handle:
        payload = json.load(handle)
    if isinstance(payload, dict):
        payload = payload.get("settlements", [])
    if not isinstance(payload, list):
        return []
    return [row for row in payload if isinstance(row, dict)]


def build_settlements(
    db_rows: list[dict[str, Any]],
    token_to_event_side: dict[str, tuple[str, str]],
) -> tuple[list[dict[str, Any]], dict[str, Any]]:
    settlements: dict[tuple[str, str], dict[str, Any]] = {}
    event_outcomes: dict[str, set[bool]] = {}
    skipped = {
        "unmapped_token": 0,
        "unresolved": 0,
        "missing_settlement": 0,
        "ambiguous_price": 0,
    }

    for row in db_rows:
        token_id = row.get("token_id")
        if token_id is None:
            skipped["unmapped_token"] += 1
            continue
        token_key = str(token_id)
        event_side = token_to_event_side.get(token_key)
        if event_side is None:
            skipped["unmapped_token"] += 1
            continue
        if row.get("resolved") is False:
            skipped["unresolved"] += 1
            continue
        settlement = row.get("settlement", row.get("settled_price"))
        if settlement is None:
            skipped["missing_settlement"] += 1
            continue

        token_won = is_winning_price(settlement)
        if token_won is None:
            skipped["ambiguous_price"] += 1
            continue

        event_id, side = event_side
        resolved_up_won = token_won if side == "UP" else not token_won
        event_outcomes.setdefault(event_id, set()).add(resolved_up_won)
        settlements[(event_id, token_key)] = {
            "event_id": event_id,
            "token_id": token_key,
            "settlement": str(settlement),
            "resolved_up_won": resolved_up_won,
            "source": "pm_token_settlements",
        }

    conflicts = sorted(event_id for event_id, outcomes in event_outcomes.items() if len(outcomes) > 1)
    if conflicts:
        settlements = {
            key: value for key, value in settlements.items() if value["event_id"] not in set(conflicts)
        }

    output = sorted(settlements.values(), key=lambda item: (item["event_id"], item["token_id"]))
    report = {
        "db_settlement_rows": len(db_rows),
        "official_settlement_count": len(output),
        "official_settlement_event_count": len({item["event_id"] for item in output}),
        "conflicting_event_count": len(conflicts),
        "conflicting_events": conflicts,
        "skipped": skipped,
    }
    return output, report


def write_json(path: Path, payload: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2, sort_keys=True)
        handle.write("\n")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--recording", required=True)
    parser.add_argument("--db-settlements-json")
    parser.add_argument("--output-json")
    parser.add_argument("--report-json")
    parser.add_argument("--output-token-ids")
    args = parser.parse_args()

    events, token_to_event_side = extract_recording_token_map(Path(args.recording))

    if args.output_token_ids:
        write_json(Path(args.output_token_ids), sorted(token_to_event_side))

    if args.db_settlements_json:
        if not args.output_json:
            raise SystemExit("--output-json is required with --db-settlements-json")
        settlements, report = build_settlements(
            load_db_rows(Path(args.db_settlements_json)),
            token_to_event_side,
        )
        report["recording_event_count"] = len(events)
        report["recording_token_count"] = len(token_to_event_side)
        write_json(Path(args.output_json), settlements)
        if args.report_json:
            write_json(Path(args.report_json), report)
        print(json.dumps(report, sort_keys=True))
    elif args.output_json:
        raise SystemExit("--db-settlements-json is required with --output-json")


if __name__ == "__main__":
    main()
