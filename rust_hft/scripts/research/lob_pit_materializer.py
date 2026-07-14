#!/usr/bin/env python3
"""Materialize replay-safe Binance LOB segments into PIT feature rows."""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import subprocess
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from decimal import Decimal
from pathlib import Path
from typing import Iterable


SCHEMA = "binance.lob_tape.v2"
MATERIALIZATION_SCHEMA = "binance-lob-pit-v1"


class MaterializationError(RuntimeError):
    pass


@dataclass(frozen=True)
class VerifiedSegment:
    path: Path
    manifest_path: Path
    success_path: Path
    sha256: str
    start_received_at_ns: int
    end_received_at_ns: int
    events: int


@dataclass
class BookState:
    session_id: str
    last_update_id: int
    bridged: bool
    bids: dict[Decimal, Decimal]
    asks: dict[Decimal, Decimal]


@dataclass(frozen=True)
class BookSample:
    series_id: int
    time_ns: int
    mid_price: float
    spread_bps: float
    bid_depth: float
    ask_depth: float
    imbalance: float


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as source:
        for block in iter(lambda: source.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


def load_json(path: Path) -> dict:
    try:
        value = json.loads(path.read_text())
    except (OSError, json.JSONDecodeError) as error:
        raise MaterializationError(f"invalid manifest {path}: {error}") from error
    if not isinstance(value, dict):
        raise MaterializationError(f"manifest must be an object: {path}")
    return value


def verify_segment(path: Path, market: str, symbol: str) -> VerifiedSegment:
    manifest_path = path.with_name(path.name + ".manifest.json")
    success_path = path.with_name(path.name + "._SUCCESS")
    if not path.is_file() or not manifest_path.is_file() or not success_path.is_file():
        raise MaterializationError(
            f"segment requires data, manifest, and _SUCCESS files: {path}"
        )
    manifest = load_json(manifest_path)
    required = {
        "schema",
        "venue",
        "market",
        "file",
        "bytes",
        "sha256",
        "events",
        "event_types",
        "has_replay_safe_checkpoint",
        "start_received_at_ns",
        "end_received_at_ns",
    }
    if required.difference(manifest):
        missing = ", ".join(sorted(required.difference(manifest)))
        raise MaterializationError(f"segment manifest is missing fields: {missing}")
    if (
        manifest["schema"] != SCHEMA
        or manifest["venue"] != "binance"
        or manifest["market"] != market
        or manifest["file"] != path.name
        or manifest["bytes"] != path.stat().st_size
    ):
        raise MaterializationError(f"segment manifest identity mismatch: {path}")
    symbols = manifest.get("symbols")
    if not isinstance(symbols, list) or symbol.lower() not in {
        str(item).lower() for item in symbols
    }:
        raise MaterializationError(f"segment does not declare symbol {symbol}: {path}")
    event_types = manifest["event_types"]
    if not isinstance(event_types, dict) or int(event_types.get("sequence_gap", 0)) > 0:
        raise MaterializationError(f"segment manifest contains a sequence gap: {path}")
    if not manifest["has_replay_safe_checkpoint"]:
        raise MaterializationError(f"segment is not marked replay safe: {path}")
    expected_hash = str(manifest["sha256"])
    try:
        success_hash = success_path.read_text().strip()
    except OSError as error:
        raise MaterializationError(f"failed to read _SUCCESS marker: {error}") from error
    if success_hash != expected_hash:
        raise MaterializationError(f"_SUCCESS marker does not match manifest: {path}")
    actual_hash = sha256_file(path)
    if actual_hash != expected_hash:
        raise MaterializationError(f"segment SHA256 does not match manifest: {path}")
    start_ns = int(manifest["start_received_at_ns"])
    end_ns = int(manifest["end_received_at_ns"])
    if start_ns <= 0 or end_ns < start_ns or int(manifest["events"]) <= 0:
        raise MaterializationError(f"segment time bounds or event count are invalid: {path}")
    return VerifiedSegment(
        path=path,
        manifest_path=manifest_path,
        success_path=success_path,
        sha256=actual_hash,
        start_received_at_ns=start_ns,
        end_received_at_ns=end_ns,
        events=int(manifest["events"]),
    )


def verified_segments(paths: Iterable[Path], market: str, symbol: str) -> list[VerifiedSegment]:
    segments = sorted(
        (verify_segment(path, market, symbol) for path in paths),
        key=lambda item: (item.start_received_at_ns, item.path.name),
    )
    if not segments:
        raise MaterializationError("at least one LOB segment is required")
    for previous, current in zip(segments, segments[1:]):
        if current.start_received_at_ns < previous.start_received_at_ns:
            raise MaterializationError("LOB segments are not time ordered")
        if current.sha256 == previous.sha256:
            raise MaterializationError("duplicate LOB segment supplied")
    return segments


def decimal_levels(levels: object, side: str) -> dict[Decimal, Decimal]:
    if not isinstance(levels, list):
        raise MaterializationError(f"{side} levels are not an array")
    parsed: dict[Decimal, Decimal] = {}
    try:
        for level in levels:
            if not isinstance(level, list) or len(level) != 2:
                raise MaterializationError(f"invalid {side} price level")
            price = Decimal(str(level[0]))
            quantity = Decimal(str(level[1]))
            if not price.is_finite() or not quantity.is_finite() or price <= 0 or quantity < 0:
                raise MaterializationError(f"invalid {side} price or quantity")
            if quantity > 0:
                parsed[price] = quantity
    except ArithmeticError as error:
        raise MaterializationError(f"invalid decimal in {side} levels") from error
    return parsed


def apply_levels(book: dict[Decimal, Decimal], levels: object, side: str) -> None:
    if not isinstance(levels, list):
        raise MaterializationError(f"{side} update levels are not an array")
    try:
        for level in levels:
            if not isinstance(level, list) or len(level) != 2:
                raise MaterializationError(f"invalid {side} update level")
            price = Decimal(str(level[0]))
            quantity = Decimal(str(level[1]))
            if not price.is_finite() or not quantity.is_finite() or price <= 0 or quantity < 0:
                raise MaterializationError(f"invalid {side} update")
            if quantity == 0:
                book.pop(price, None)
            else:
                book[price] = quantity
    except ArithmeticError as error:
        raise MaterializationError(f"invalid decimal in {side} update") from error


def install_snapshot(event: dict) -> BookState:
    payload = event.get("snapshot")
    if not isinstance(payload, dict):
        raise MaterializationError("snapshot event has no snapshot payload")
    try:
        update_id = int(payload["lastUpdateId"])
        session_id = str(event["session_id"])
    except (KeyError, TypeError, ValueError) as error:
        raise MaterializationError("snapshot identity is invalid") from error
    if update_id <= 0 or not session_id:
        raise MaterializationError("snapshot identity is invalid")
    return BookState(
        session_id=session_id,
        last_update_id=update_id,
        bridged=False,
        bids=decimal_levels(payload.get("bids"), "bid"),
        asks=decimal_levels(payload.get("asks"), "ask"),
    )


def install_checkpoint(event: dict) -> BookState:
    try:
        update_id = int(event["last_update_id"])
        session_id = str(event["session_id"])
    except (KeyError, TypeError, ValueError) as error:
        raise MaterializationError("checkpoint identity is invalid") from error
    if (
        event.get("replay_safe") is not True
        or event.get("synced") is not True
        or event.get("bridged") is not True
        or update_id <= 0
        or not session_id
    ):
        raise MaterializationError("checkpoint is not replay safe")
    return BookState(
        session_id=session_id,
        last_update_id=update_id,
        bridged=True,
        bids=decimal_levels(event.get("bids"), "bid"),
        asks=decimal_levels(event.get("asks"), "ask"),
    )


def validate_checkpoint(state: BookState, checkpoint_state: BookState) -> None:
    if (
        state.session_id != checkpoint_state.session_id
        or state.last_update_id != checkpoint_state.last_update_id
        or state.bridged != checkpoint_state.bridged
        or state.bids != checkpoint_state.bids
        or state.asks != checkpoint_state.asks
    ):
        raise MaterializationError("checkpoint does not match replayed order book")


def apply_diff(state: BookState, event: dict, symbol: str, market: str) -> None:
    frame = event.get("frame")
    data = frame.get("data") if isinstance(frame, dict) else None
    if not isinstance(data, dict) or str(data.get("s", "")).upper() != symbol:
        raise MaterializationError("diff event identity is invalid")
    if str(event.get("session_id", "")) != state.session_id:
        raise MaterializationError("diff session does not match replay state")
    try:
        first_update_id = int(data["U"])
        final_update_id = int(data["u"])
    except (KeyError, TypeError, ValueError) as error:
        raise MaterializationError("diff update range is invalid") from error
    if final_update_id <= state.last_update_id:
        return
    if first_update_id > final_update_id:
        raise MaterializationError("diff update range is reversed")
    previous_final_update_id = data.get("pu")
    accepted = False
    expected = state.last_update_id + (1 if market == "spot" else 0)
    if not state.bridged:
        if market == "usdm" and previous_final_update_id is not None:
            accepted = int(previous_final_update_id) == state.last_update_id
        accepted = accepted or first_update_id <= expected <= final_update_id
    elif market == "usdm":
        accepted = (
            previous_final_update_id is not None
            and int(previous_final_update_id) == state.last_update_id
        )
        expected = state.last_update_id
    else:
        accepted = first_update_id <= expected <= final_update_id
    if not accepted:
        raise MaterializationError(
            f"Binance sequence gap: expected {expected}, received "
            f"{first_update_id}-{final_update_id}"
        )
    apply_levels(state.bids, data.get("b", []), "bid")
    apply_levels(state.asks, data.get("a", []), "ask")
    state.last_update_id = final_update_id
    state.bridged = True


def sample_book(state: BookState, series_id: int, time_ns: int, depth: int) -> BookSample:
    if depth <= 0 or not state.bids or not state.asks:
        raise MaterializationError("cannot sample an empty order book")
    bid_levels = sorted(state.bids.items(), reverse=True)[:depth]
    ask_levels = sorted(state.asks.items())[:depth]
    best_bid = bid_levels[0][0]
    best_ask = ask_levels[0][0]
    if best_bid >= best_ask:
        raise MaterializationError("replayed order book is crossed")
    mid = (best_bid + best_ask) / Decimal(2)
    bid_depth = sum((quantity for _, quantity in bid_levels), Decimal(0))
    ask_depth = sum((quantity for _, quantity in ask_levels), Decimal(0))
    total_depth = bid_depth + ask_depth
    if mid <= 0 or total_depth <= 0:
        raise MaterializationError("replayed order book has invalid depth")
    spread_bps = (best_ask - best_bid) / mid * Decimal(10_000)
    imbalance = (bid_depth - ask_depth) / total_depth
    return BookSample(
        series_id=series_id,
        time_ns=time_ns,
        mid_price=float(mid),
        spread_bps=float(spread_bps),
        bid_depth=float(bid_depth),
        ask_depth=float(ask_depth),
        imbalance=float(imbalance),
    )


def event_symbol(event: dict) -> str | None:
    event_type = event.get("type")
    if event_type == "diff":
        frame = event.get("frame")
        data = frame.get("data") if isinstance(frame, dict) else None
        return str(data.get("s", "")).upper() if isinstance(data, dict) else None
    if event_type in {"snapshot", "checkpoint", "sequence_gap"}:
        return str(event.get("symbol", "")).upper()
    return None


def iter_segment_events(segment: VerifiedSegment, symbol: str):
    process = subprocess.Popen(
        ["zstd", "-q", "-dc", str(segment.path)],
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        encoding="utf-8",
    )
    assert process.stdout is not None
    completed = False
    try:
        for line_number, line in enumerate(process.stdout, start=1):
            if symbol not in line:
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError as error:
                raise MaterializationError(
                    f"invalid JSON in {segment.path} line {line_number}: {error}"
                ) from error
            if isinstance(event, dict) and event_symbol(event) == symbol:
                yield event
        completed = True
    finally:
        process.stdout.close()
        if not completed and process.poll() is None:
            process.terminate()
        if not completed:
            process.wait()
    stderr = process.stderr.read() if process.stderr is not None else ""
    return_code = process.wait()
    if return_code != 0:
        raise MaterializationError(
            f"zstd failed for {segment.path}: {stderr.strip() or return_code}"
        )


def replay_samples(
    segments: list[VerifiedSegment],
    symbol: str,
    market: str,
    bucket_ns: int,
    depth: int,
) -> list[BookSample]:
    state: BookState | None = None
    pending_diffs: list[dict] = []
    samples: list[BookSample] = []
    next_bucket_ns: int | None = None
    series_id = 0
    saw_seed = False

    def start_series(new_state: BookState, received_at_ns: int) -> None:
        nonlocal state, next_bucket_ns, pending_diffs, series_id, saw_seed
        state = new_state
        pending_diffs = []
        series_id += 1
        saw_seed = True
        next_bucket_ns = ((received_at_ns + bucket_ns - 1) // bucket_ns) * bucket_ns

    def emit_before(received_at_ns: int) -> None:
        nonlocal next_bucket_ns
        if state is None or next_bucket_ns is None:
            return
        while next_bucket_ns < received_at_ns:
            samples.append(sample_book(state, series_id, next_bucket_ns, depth))
            next_bucket_ns += bucket_ns

    def emit_at(received_at_ns: int) -> None:
        nonlocal next_bucket_ns
        if state is None or next_bucket_ns is None:
            return
        while next_bucket_ns <= received_at_ns:
            samples.append(sample_book(state, series_id, next_bucket_ns, depth))
            next_bucket_ns += bucket_ns

    for segment in segments:
        for event in iter_segment_events(segment, symbol):
            try:
                received_at_ns = int(event["received_at_ns"])
            except (KeyError, TypeError, ValueError) as error:
                raise MaterializationError("LOB event receive timestamp is invalid") from error
            event_type = event.get("type")
            if event_type == "sequence_gap":
                raise MaterializationError("LOB tape contains a sequence gap event")
            if event_type == "snapshot":
                snapshot_state = install_snapshot(event)
                for pending in pending_diffs:
                    if str(pending.get("session_id", "")) != snapshot_state.session_id:
                        raise MaterializationError(
                            "buffered diff session does not match its snapshot"
                        )
                    apply_diff(snapshot_state, pending, symbol, market)
                if state is None or snapshot_state.session_id != state.session_id:
                    start_series(snapshot_state, received_at_ns)
                else:
                    emit_before(received_at_ns)
                    start_series(snapshot_state, received_at_ns)
                emit_at(received_at_ns)
                continue
            if event_type == "checkpoint":
                checkpoint_state = install_checkpoint(event)
                if state is None:
                    # A closing checkpoint may seed the next segment, but it cannot
                    # make earlier diffs in this segment replayable retroactively.
                    start_series(checkpoint_state, received_at_ns)
                elif checkpoint_state.session_id != state.session_id:
                    start_series(checkpoint_state, received_at_ns)
                else:
                    emit_before(received_at_ns)
                    validate_checkpoint(state, checkpoint_state)
                emit_at(received_at_ns)
                continue
            if event_type != "diff":
                continue
            if state is None:
                pending_diffs.append(event)
                if len(pending_diffs) > 100_000:
                    raise MaterializationError("too many diffs buffered before replay seed")
                continue
            if str(event.get("session_id", "")) != state.session_id:
                state = None
                next_bucket_ns = None
                pending_diffs = [event]
                continue
            emit_before(received_at_ns)
            apply_diff(state, event, symbol, market)
            emit_at(received_at_ns)

    if not saw_seed:
        raise MaterializationError(f"no replay seed found for {symbol}")
    return samples


def rfc3339_ns(value_ns: int) -> str:
    seconds, nanoseconds = divmod(value_ns, 1_000_000_000)
    value = datetime.fromtimestamp(seconds, timezone.utc)
    if nanoseconds == 0:
        return value.strftime("%Y-%m-%dT%H:%M:%SZ")
    fraction = f"{nanoseconds:09d}".rstrip("0")
    return value.strftime("%Y-%m-%dT%H:%M:%S") + f".{fraction}Z"


def utc_now_text() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def source_revision(segments: list[VerifiedSegment]) -> str:
    digest = hashlib.sha256()
    for segment in segments:
        digest.update(segment.sha256.encode())
        digest.update(b"\n")
    return digest.hexdigest()


def materialize_rows(
    samples: list[BookSample],
    mission_id: str,
    market: str,
    symbol: str,
    revision: str,
    horizon: int,
    depth: int,
    ingestion_time: str,
) -> list[dict]:
    if horizon <= 0:
        raise MaterializationError("label horizon must be positive")
    rows: list[dict] = []
    source_revisions = {f"binance-{market}-lob": revision}
    for index in range(1, len(samples) - horizon):
        previous = samples[index - 1]
        current = samples[index]
        future = samples[index + horizon]
        if not (
            previous.series_id == current.series_id == future.series_id
            and current.mid_price > 0
            and previous.mid_price > 0
        ):
            continue
        previous_total = previous.bid_depth + previous.ask_depth
        if previous_total <= 0:
            continue
        mid_return = current.mid_price / previous.mid_price - 1.0
        label = future.mid_price / current.mid_price - 1.0
        ofi = (
            (current.bid_depth - previous.bid_depth)
            - (current.ask_depth - previous.ask_depth)
        ) / previous_total
        features = {
            f"ask_depth_top{depth}": current.ask_depth,
            f"bid_depth_top{depth}": current.bid_depth,
            f"book_imbalance_top{depth}": current.imbalance,
            "mid_price": current.mid_price,
            "mid_return_1": mid_return,
            f"ofi_top{depth}": ofi,
            "spread_bps": current.spread_bps,
        }
        if not all(math.isfinite(value) for value in [*features.values(), label]):
            raise MaterializationError("materialized feature or label is not finite")
        rows.append(
            {
                "event_time": rfc3339_ns(current.time_ns),
                "feature_available_time": rfc3339_ns(current.time_ns),
                "label_available_time": rfc3339_ns(future.time_ns),
                "ingestion_time": ingestion_time,
                "symbol": symbol,
                "source_revisions": source_revisions,
                "modalities": ["lob"],
                "features": features,
                "label": label,
            }
        )
    if len(rows) < 3:
        raise MaterializationError("materialization produced fewer than three PIT rows")
    return rows


def write_atomic(path: Path, payload: bytes) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.{os.getpid()}.tmp")
    temporary.write_bytes(payload)
    os.replace(temporary, path)


def run(args: argparse.Namespace) -> dict:
    mission_id = args.mission_id.strip()
    symbol = args.symbol.strip().upper()
    if not mission_id or not symbol:
        raise MaterializationError("mission id and symbol are required")
    if args.bucket_ms <= 0 or args.top_depth <= 0:
        raise MaterializationError("bucket and top depth must be positive")
    segments = verified_segments(args.segment, args.market, symbol)
    revision = source_revision(segments)
    samples = replay_samples(
        segments,
        symbol,
        args.market,
        args.bucket_ms * 1_000_000,
        args.top_depth,
    )
    created_at = utc_now_text()
    rows = materialize_rows(
        samples,
        mission_id,
        args.market,
        symbol,
        revision,
        args.label_horizon_buckets,
        args.top_depth,
        created_at,
    )
    output_bytes = b"".join(
        json.dumps(row, separators=(",", ":"), sort_keys=True).encode() + b"\n"
        for row in rows
    )
    output_hash = hashlib.sha256(output_bytes).hexdigest()
    write_atomic(args.output, output_bytes)
    report = {
        "dataset_kind": "lob_point_in_time_materialization",
        "schema_version": MATERIALIZATION_SCHEMA,
        "mission_id": mission_id,
        "symbol": symbol,
        "market": args.market,
        "bucket_ms": args.bucket_ms,
        "label_horizon_buckets": args.label_horizon_buckets,
        "top_depth": args.top_depth,
        "source_revision": revision,
        "source_segments": [
            {
                "path": str(segment.path),
                "sha256": segment.sha256,
                "start_received_at_ns": segment.start_received_at_ns,
                "end_received_at_ns": segment.end_received_at_ns,
                "events": segment.events,
            }
            for segment in segments
        ],
        "rows": len(rows),
        "first_event_time": rows[0]["event_time"],
        "last_event_time": rows[-1]["event_time"],
        "artifact_path": str(args.output),
        "artifact_sha256": output_hash,
        "created_at": created_at,
    }
    write_atomic(
        args.manifest_out,
        (json.dumps(report, indent=2, sort_keys=True) + "\n").encode(),
    )
    return report


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Replay Binance LOB segments into point-in-time feature rows"
    )
    parser.add_argument("--mission-id", required=True)
    parser.add_argument("--symbol", required=True)
    parser.add_argument("--market", choices=("spot", "usdm"), required=True)
    parser.add_argument("--bucket-ms", type=int, default=1_000)
    parser.add_argument("--label-horizon-buckets", type=int, default=5)
    parser.add_argument("--top-depth", type=int, default=5)
    parser.add_argument("--segment", type=Path, action="append", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--manifest-out", type=Path, required=True)
    return parser.parse_args()


def main() -> int:
    try:
        report = run(parse_args())
    except (MaterializationError, OSError, subprocess.SubprocessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 2
    json.dump(report, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
