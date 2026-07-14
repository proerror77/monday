#!/usr/bin/env python3
"""Attach official settlement outcomes to recorded replay lifecycle rows.

Recorded MarketUpdate NDJSON is the source of replay ordering. Older dry-run
recordings can contain `event_expired` rows with `resolved_up_won=null` even
after the runtime DB has official settlement accounting. This helper creates a
temporary enriched recording for replay parity checks by filling only
`event_expired.resolved_up_won`; it deliberately leaves `event_discovered`
unchanged so settlement labels are not visible at decision time.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any


def parse_bool(value: Any) -> bool | None:
    if isinstance(value, bool):
        return value
    if value is None:
        return None
    text = str(value).strip().lower()
    if text in {"true", "t", "1", "yes", "y"}:
        return True
    if text in {"false", "f", "0", "no", "n"}:
        return False
    return None


def load_settlements(path: Path) -> dict[str, bool]:
    with path.open(encoding="utf-8") as handle:
        payload = json.load(handle)

    settlements: dict[str, bool] = {}
    if isinstance(payload, dict):
        items = payload.get("settlements", payload)
        if isinstance(items, dict):
            for event_id, value in items.items():
                parsed = parse_bool(value.get("resolved_up_won") if isinstance(value, dict) else value)
                if parsed is not None:
                    settlements[str(event_id)] = parsed
            return settlements
        payload = items

    if isinstance(payload, list):
        for item in payload:
            if not isinstance(item, dict):
                continue
            event_id = item.get("event_id")
            parsed = parse_bool(item.get("resolved_up_won"))
            if event_id is not None and parsed is not None:
                settlements[str(event_id)] = parsed
    return settlements


def enrich_record(record: dict[str, Any], settlements: dict[str, bool]) -> bool:
    update = record.get("update")
    if not isinstance(update, dict):
        return False
    if update.get("kind") != "event_expired":
        return False
    if update.get("resolved_up_won") is not None:
        return False
    event_id = update.get("event_id")
    if event_id is None:
        return False
    resolved = settlements.get(str(event_id))
    if resolved is None:
        return False
    update["resolved_up_won"] = resolved
    return True


def enrich_file(input_path: Path, output_path: Path, settlements: dict[str, bool]) -> dict[str, int]:
    stats = {
        "records": 0,
        "event_expired": 0,
        "event_expired_missing_settlement": 0,
        "event_expired_enriched": 0,
        "event_discovered_seen": 0,
    }
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with input_path.open(encoding="utf-8") as source, output_path.open("w", encoding="utf-8") as dest:
        for line_number, raw in enumerate(source, start=1):
            line = raw.rstrip("\n")
            if not line:
                dest.write(raw)
                continue
            try:
                record = json.loads(line)
            except json.JSONDecodeError as exc:
                raise SystemExit(f"{input_path}:{line_number}: invalid NDJSON: {exc}") from exc
            if not isinstance(record, dict):
                raise SystemExit(f"{input_path}:{line_number}: expected JSON object record")

            update = record.get("update")
            if isinstance(update, dict):
                kind = update.get("kind")
                if kind == "event_discovered":
                    stats["event_discovered_seen"] += 1
                if kind == "event_expired":
                    stats["event_expired"] += 1
                    if update.get("resolved_up_won") is None:
                        stats["event_expired_missing_settlement"] += 1

            if enrich_record(record, settlements):
                stats["event_expired_enriched"] += 1

            stats["records"] += 1
            dest.write(json.dumps(record, separators=(",", ":"), sort_keys=True))
            dest.write("\n")
    return stats


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--input", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--settlements-json", required=True)
    parser.add_argument("--report-json", required=True)
    args = parser.parse_args()

    settlements_path = Path(args.settlements_json)
    settlements = load_settlements(settlements_path)
    stats = enrich_file(
        Path(args.input),
        Path(args.output),
        settlements,
    )
    stats["settlement_event_count"] = len(settlements)

    report_path = Path(args.report_json)
    report_path.parent.mkdir(parents=True, exist_ok=True)
    with report_path.open("w", encoding="utf-8") as handle:
        json.dump(stats, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(json.dumps(stats, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
