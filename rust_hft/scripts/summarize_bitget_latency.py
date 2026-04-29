#!/usr/bin/env python3
"""Summarize Bitget latency audit summary.json artifacts.

Usage:
  scripts/summarize_bitget_latency.py
  scripts/summarize_bitget_latency.py target/latency-audit /tmp/other-run
  scripts/summarize_bitget_latency.py --csv target/latency-audit > bitget-latency.csv
"""

from __future__ import annotations

import argparse
import csv
import json
import sys
from pathlib import Path
from typing import Any


DEFAULT_ROOT = Path("target/latency-audit")


FIELDS = [
    "run_id",
    "symbol",
    "samples",
    "dropped",
    "receiver_core",
    "engine_core",
    "busy_poll",
    "raw_queue_wait_p99_ns",
    "raw_queue_wait_p999_ns",
    "engine_total_p99_ns",
    "engine_total_p999_ns",
    "event_convert_p99_ns",
    "envelope_parse_p99_ns",
    "ws_receive_gap_p99_ns",
    "summary_path",
]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "paths",
        nargs="*",
        type=Path,
        default=[DEFAULT_ROOT],
        help="summary.json files or directories containing summary.json files",
    )
    parser.add_argument("--csv", action="store_true", help="emit CSV instead of markdown")
    parser.add_argument(
        "--sort",
        choices=("engine-p99", "engine-p999", "queue-p99", "queue-p999", "run-id"),
        default="engine-p99",
        help="sort output rows",
    )
    parser.add_argument("--limit", type=int, default=0, help="limit output rows")
    args = parser.parse_args()

    summaries = []
    for path in args.paths:
        summaries.extend(find_summary_files(path))

    rows = [load_row(path) for path in sorted(set(summaries))]
    rows.sort(key=sort_key(args.sort))
    if args.limit > 0:
        rows = rows[: args.limit]

    if args.csv:
        write_csv(rows)
    else:
        write_markdown(rows)
    return 0 if rows else 1


def find_summary_files(path: Path) -> list[Path]:
    if path.is_file():
        return [path]
    if not path.exists():
        return []
    return list(path.rglob("summary.json"))


def load_row(path: Path) -> dict[str, Any]:
    with path.open("r", encoding="utf-8") as handle:
        summary = json.load(handle)

    metrics = summary.get("metrics", {})
    row = {
        "run_id": path.parent.name,
        "symbol": summary.get("symbol", ""),
        "samples": summary.get("samples", 0),
        "dropped": summary.get("dropped", 0),
        "receiver_core": none_to_empty(summary.get("receiver_core")),
        "engine_core": none_to_empty(summary.get("engine_core")),
        "busy_poll": summary.get("busy_poll", ""),
        "raw_queue_wait_p99_ns": metric(metrics, "raw_queue_wait_ns", "p99"),
        "raw_queue_wait_p999_ns": metric(metrics, "raw_queue_wait_ns", "p999"),
        "engine_total_p99_ns": metric(metrics, "engine_total_ns", "p99"),
        "engine_total_p999_ns": metric(metrics, "engine_total_ns", "p999"),
        "event_convert_p99_ns": metric(metrics, "event_convert_ns", "p99"),
        "envelope_parse_p99_ns": metric(metrics, "envelope_parse_ns", "p99"),
        "ws_receive_gap_p99_ns": metric(metrics, "ws_receive_gap_ns", "p99"),
        "summary_path": str(path),
    }
    return row


def metric(metrics: dict[str, Any], name: str, percentile: str) -> int:
    value = metrics.get(name, {}).get(percentile, 0)
    return int(value or 0)


def none_to_empty(value: Any) -> Any:
    return "" if value is None else value


def sort_key(kind: str):
    if kind == "engine-p999":
        return lambda row: (row["engine_total_p999_ns"], row["run_id"])
    if kind == "queue-p99":
        return lambda row: (row["raw_queue_wait_p99_ns"], row["run_id"])
    if kind == "queue-p999":
        return lambda row: (row["raw_queue_wait_p999_ns"], row["run_id"])
    if kind == "run-id":
        return lambda row: row["run_id"]
    return lambda row: (row["engine_total_p99_ns"], row["run_id"])


def write_csv(rows: list[dict[str, Any]]) -> None:
    writer = csv.DictWriter(sys.stdout, fieldnames=FIELDS)
    writer.writeheader()
    writer.writerows(rows)


def write_markdown(rows: list[dict[str, Any]]) -> None:
    if not rows:
        print("No Bitget latency summary.json files found.")
        return

    visible_fields = [
        "run_id",
        "symbol",
        "samples",
        "dropped",
        "receiver_core",
        "engine_core",
        "busy_poll",
        "raw_queue_wait_p99_ns",
        "raw_queue_wait_p999_ns",
        "engine_total_p99_ns",
        "engine_total_p999_ns",
    ]
    print("| " + " | ".join(visible_fields) + " |")
    print("| " + " | ".join(["---"] * len(visible_fields)) + " |")
    for row in rows:
        print("| " + " | ".join(str(row[field]) for field in visible_fields) + " |")


if __name__ == "__main__":
    raise SystemExit(main())
