#!/usr/bin/env python3
"""Resolve a recorded replay parity window from recording and dry-run evidence.

The recorded replay parity workflow replays a bounded MarketUpdate recording.
After runtime restarts, that recording may only cover the new process window
while the dry-run report still contains older closed rows. This helper selects
the latest dry-run evidence rows that actually overlap the recording coverage.
"""

from __future__ import annotations

import argparse
import json
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any

from replay_dryrun_parity import (
    extract_runtime_events,
    extract_runtime_fills,
    extract_runtime_orders,
    load_json,
    normalize_text,
    parse_timestamp,
)


WINDOW_PAD = timedelta(seconds=60)


def update_timestamps(record: dict[str, Any]) -> list[datetime]:
    timestamps: list[datetime] = []
    for key in ("recorded_at", "timestamp", "ts"):
        parsed = parse_timestamp(record.get(key))
        if parsed is not None:
            timestamps.append(parsed)

    update = record.get("update")
    if isinstance(update, dict):
        for key in ("ts", "timestamp", "start_time", "end_time"):
            parsed = parse_timestamp(update.get(key))
            if parsed is not None:
                timestamps.append(parsed)
    return timestamps


def recording_bounds(path: Path) -> dict[str, Any]:
    min_ts: datetime | None = None
    max_ts: datetime | None = None
    records = 0
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
                continue
            records += 1
            for ts in update_timestamps(record):
                min_ts = ts if min_ts is None or ts < min_ts else min_ts
                max_ts = ts if max_ts is None or ts > max_ts else max_ts
    if min_ts is None or max_ts is None:
        raise SystemExit(f"{path}: no parseable recording timestamps found")
    return {"records": records, "min_ts": min_ts, "max_ts": max_ts}


def row_ts(row: dict[str, Any]) -> datetime | None:
    for key in ("decision_ts", "created_at", "fill_timestamp", "opened_at", "timestamp"):
        parsed = parse_timestamp(row.get(key))
        if parsed is not None:
            return parsed
    return None


def row_closed(row: dict[str, Any]) -> bool:
    settlement = normalize_text(row.get("settlement"), upper=True)
    status = normalize_text(row.get("status"), upper=True)
    fill_status = normalize_text(row.get("fill_status"), upper=True)
    return bool(
        settlement
        and settlement != "OPEN"
        or status in {"CLOSED", "SETTLED"}
        or fill_status in {"CLOSED", "SETTLED"}
    )


def candidate_rows(report: Any, deployment_id: str, start: datetime, end: datetime) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    seen: set[tuple[str, str]] = set()
    for source, extracted in (
        ("event", extract_runtime_events(report)),
        ("order", extract_runtime_orders(report)),
        ("fill", extract_runtime_fills(report)),
    ):
        for row in extracted:
            if normalize_text(row.get("deployment_id")) != deployment_id:
                continue
            ts = row_ts(row)
            if ts is None or ts < start or ts > end:
                continue
            key = (source, normalize_text(row.get("intent_id")) or normalize_text(row.get("event_id")) or ts.isoformat())
            if key in seen:
                continue
            seen.add(key)
            enriched = dict(row)
            enriched["_source"] = source
            enriched["_ts"] = ts
            rows.append(enriched)
    return rows


def iso(ts: datetime) -> str:
    return ts.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def resolve_window(
    *,
    recording_path: Path,
    dryrun_report_path: Path,
    deployment_id: str,
    max_rows: int,
) -> dict[str, Any]:
    bounds = recording_bounds(recording_path)
    recording_start = bounds["min_ts"] - WINDOW_PAD
    recording_end = bounds["max_ts"] + WINDOW_PAD
    report = load_json(dryrun_report_path)
    rows = candidate_rows(report, deployment_id, recording_start, recording_end)
    if not rows:
        raise SystemExit(
            "no dry-run runtime evidence rows overlap recording coverage "
            f"{iso(recording_start)} -> {iso(recording_end)} for {deployment_id}"
        )

    closed_rows = [row for row in rows if row_closed(row)]
    selected_pool = closed_rows if closed_rows else rows
    selected = sorted(selected_pool, key=lambda row: row["_ts"], reverse=True)[:max_rows]
    min_selected = min(row["_ts"] for row in selected)
    max_selected = max(row["_ts"] for row in selected)
    since = min_selected - WINDOW_PAD
    until = max_selected + WINDOW_PAD

    return {
        "mode": "auto_recording_intersection",
        "recording": {
            "records": bounds["records"],
            "since": iso(bounds["min_ts"]),
            "until": iso(bounds["max_ts"]),
        },
        "selected_row_count": len(selected),
        "selected_closed_row_count": len([row for row in selected if row_closed(row)]),
        "selected_sources": sorted({str(row.get("_source")) for row in selected}),
        "since": iso(since),
        "until": iso(until),
    }


def write_env(path: Path, window: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("w", encoding="utf-8") as handle:
        handle.write(f"RESOLVED_SINCE={window['since']}\n")
        handle.write(f"RESOLVED_UNTIL={window['until']}\n")
        handle.write(f"RESOLVED_WINDOW_MODE={window['mode']}\n")
        handle.write(f"RESOLVED_CLOSED_ROW_COUNT={window['selected_closed_row_count']}\n")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--recording", required=True)
    parser.add_argument("--dryrun-json", required=True)
    parser.add_argument("--deployment-id", required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-env", required=True)
    parser.add_argument("--max-rows", type=int, default=20)
    args = parser.parse_args()

    window = resolve_window(
        recording_path=Path(args.recording),
        dryrun_report_path=Path(args.dryrun_json),
        deployment_id=args.deployment_id,
        max_rows=args.max_rows,
    )
    output_json = Path(args.output_json)
    output_json.parent.mkdir(parents=True, exist_ok=True)
    with output_json.open("w", encoding="utf-8") as handle:
        json.dump(window, handle, indent=2, sort_keys=True)
        handle.write("\n")
    write_env(Path(args.output_env), window)
    print(json.dumps(window, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
