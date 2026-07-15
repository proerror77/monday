#!/usr/bin/env python3
"""Build one canonical v2 reference tape from overlapping historical tapes."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
from typing import Any

from polymarket_reference_collector import TRADE_ID_VERSION, stable_trade_id


def canonicalize(inputs: list[Path], output: Path) -> dict[str, Any]:
    rows: list[tuple[str, int, int, dict[str, Any]]] = []
    seen_trades: set[str] = set()
    input_rows = 0
    duplicate_trades = 0
    trade_rows = 0
    for source_index, source in enumerate(inputs):
        with source.open("rb") as handle:
            for line_number, raw_line in enumerate(handle, start=1):
                input_rows += 1
                if not raw_line.endswith(b"\n"):
                    raise ValueError(f"{source}:{line_number}: incomplete record")
                row = json.loads(raw_line)
                if not isinstance(row, dict) or not isinstance(row.get("update"), dict):
                    raise ValueError(f"{source}:{line_number}: invalid record")
                recorded_at = row.get("recorded_at")
                if not isinstance(recorded_at, str) or not recorded_at:
                    raise ValueError(f"{source}:{line_number}: missing recorded_at")
                update = dict(row["update"])
                if update.get("kind") == "polymarket_trade":
                    trade = update.get("trade")
                    if not isinstance(trade, dict):
                        raise ValueError(f"{source}:{line_number}: trade payload missing")
                    record_id = stable_trade_id(trade)
                    if record_id in seen_trades:
                        duplicate_trades += 1
                        continue
                    seen_trades.add(record_id)
                    update["record_id"] = record_id
                    update["record_id_version"] = TRADE_ID_VERSION
                    trade_rows += 1
                rows.append((recorded_at, source_index, line_number, update))

    rows.sort(key=lambda item: (item[0], item[1], item[2]))
    temporary = output.with_suffix(output.suffix + ".tmp")
    output.parent.mkdir(parents=True, exist_ok=True)
    with temporary.open("wb", buffering=0) as handle:
        for sequence, (recorded_at, _source, _line, update) in enumerate(rows):
            encoded = json.dumps(
                {"sequence": sequence, "recorded_at": recorded_at, "update": update},
                sort_keys=True,
                separators=(",", ":"),
            ).encode() + b"\n"
            view = memoryview(encoded)
            written = 0
            while written < len(view):
                count = handle.write(view[written:])
                if not isinstance(count, int) or count <= 0:
                    raise OSError("canonical tape write made no progress")
                written += count
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, output)
    return {
        "input_rows": input_rows,
        "output_rows": len(rows),
        "canonical_v2_trades": trade_rows,
        "duplicate_trades_removed": duplicate_trades,
        "output": str(output),
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("inputs", nargs="+", type=Path)
    return parser.parse_args()


if __name__ == "__main__":
    args = parse_args()
    print(json.dumps(canonicalize(args.inputs, args.output), sort_keys=True))
