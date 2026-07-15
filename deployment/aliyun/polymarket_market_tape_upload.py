#!/usr/bin/env python3
"""Validate, compress, and upload closed Polymarket tape sessions to OSS."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
from collections import Counter, defaultdict
from dataclasses import dataclass
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any


ACTIVE_TAPE = "market-updates.ndjson"
ROTATED_TAPE_GLOB = "market-updates.*.ndjson"
ALLOWED_KINDS = {
    "quote",
    "event_discovered",
    "event_expired",
    "reference_price",
}


@dataclass(frozen=True)
class Artifacts:
    source: Path
    data: Path
    manifest: Path
    success: Path
    object_prefix: str


def utc_now() -> str:
    return datetime.now(timezone.utc).isoformat().replace("+00:00", "Z")


def parse_timestamp(value: Any, field: str, line_number: int) -> datetime:
    if not isinstance(value, str):
        raise ValueError(f"line {line_number}: {field} must be a string")
    try:
        parsed = datetime.fromisoformat(value.replace("Z", "+00:00"))
    except ValueError as error:
        raise ValueError(f"line {line_number}: invalid {field}: {value}") from error
    if parsed.tzinfo is None:
        raise ValueError(f"line {line_number}: {field} must include a timezone")
    return parsed.astimezone(timezone.utc)


def decimal_or_none(value: Any) -> Decimal | None:
    if value is None:
        return None
    try:
        return Decimal(str(value))
    except (InvalidOperation, ValueError):
        return None


def sha256_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def atomic_json(path: Path, payload: dict[str, Any]) -> None:
    temporary = path.with_suffix(path.suffix + ".tmp")
    with temporary.open("w", encoding="utf-8") as handle:
        json.dump(payload, handle, sort_keys=True, separators=(",", ":"))
        handle.write("\n")
        handle.flush()
        os.fsync(handle.fileno())
    os.replace(temporary, path)


def discover_rotated_tapes(spool_dir: Path) -> list[Path]:
    return sorted(path for path in spool_dir.glob(ROTATED_TAPE_GLOB) if path.is_file())


def scan_tape(path: Path, dataset: str, quote_depth_levels: int, quote_sample_ms: int) -> dict[str, Any]:
    before = path.stat()
    event_types: Counter[str] = Counter()
    present_fields: dict[str, Counter[str]] = defaultdict(Counter)
    non_null_fields: dict[str, Counter[str]] = defaultdict(Counter)
    symbols: set[str] = set()
    token_ids: set[str] = set()
    first_recorded_at: str | None = None
    last_recorded_at: str | None = None
    previous_recorded_at: datetime | None = None
    first_sequence: int | None = None
    last_sequence: int | None = None
    expected_sequence = 0
    crossed_quotes = 0
    one_sided_quotes = 0
    empty_quotes = 0
    out_of_range_prices = 0
    negative_sizes = 0
    max_bid_levels = 0
    max_ask_levels = 0

    with path.open("rb") as handle:
        for line_number, raw_line in enumerate(handle, start=1):
            if not raw_line.endswith(b"\n"):
                raise ValueError(f"line {line_number}: tape ends with an incomplete record")
            try:
                record = json.loads(raw_line)
            except json.JSONDecodeError as error:
                raise ValueError(f"line {line_number}: invalid JSON") from error
            if not isinstance(record, dict):
                raise ValueError(f"line {line_number}: record must be an object")

            sequence = record.get("sequence")
            if sequence != expected_sequence:
                raise ValueError(
                    f"line {line_number}: sequence gap expected={expected_sequence} actual={sequence}"
                )
            recorded_at = parse_timestamp(record.get("recorded_at"), "recorded_at", line_number)
            if previous_recorded_at is not None and recorded_at < previous_recorded_at:
                raise ValueError(f"line {line_number}: recorded_at moved backwards")

            update = record.get("update")
            if not isinstance(update, dict):
                raise ValueError(f"line {line_number}: update must be an object")
            kind = update.get("kind")
            if kind not in ALLOWED_KINDS:
                raise ValueError(f"line {line_number}: unsupported update kind {kind!r}")

            event_types[kind] += 1
            for field, value in update.items():
                present_fields[kind][field] += 1
                if value is not None:
                    non_null_fields[kind][field] += 1

            symbol = update.get("symbol")
            if isinstance(symbol, str) and symbol:
                symbols.add(symbol)
            token_id = update.get("token_id")
            if isinstance(token_id, str) and token_id:
                token_ids.add(token_id)

            if kind == "quote":
                bid = decimal_or_none(update.get("bid"))
                ask = decimal_or_none(update.get("ask"))
                bid_size = decimal_or_none(update.get("bid_size"))
                ask_size = decimal_or_none(update.get("ask_size"))
                bid_levels = update.get("bid_levels") or []
                ask_levels = update.get("ask_levels") or []
                if not isinstance(bid_levels, list) or not isinstance(ask_levels, list):
                    raise ValueError(f"line {line_number}: quote levels must be arrays")
                if len(bid_levels) > quote_depth_levels or len(ask_levels) > quote_depth_levels:
                    raise ValueError(f"line {line_number}: quote exceeds configured depth")
                max_bid_levels = max(max_bid_levels, len(bid_levels))
                max_ask_levels = max(max_ask_levels, len(ask_levels))
                if bid is None and ask is None:
                    empty_quotes += 1
                elif bid is None or ask is None:
                    one_sided_quotes += 1
                elif bid > ask:
                    crossed_quotes += 1
                for price in (bid, ask):
                    if price is not None and not Decimal("0") <= price <= Decimal("1"):
                        out_of_range_prices += 1
                for size in (bid_size, ask_size):
                    if size is not None and size < 0:
                        negative_sizes += 1

            recorded_at_text = record["recorded_at"]
            first_recorded_at = first_recorded_at or recorded_at_text
            last_recorded_at = recorded_at_text
            first_sequence = sequence if first_sequence is None else first_sequence
            last_sequence = sequence
            expected_sequence += 1
            previous_recorded_at = recorded_at

    after = path.stat()
    if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
        raise ValueError("tape changed while being validated; refusing to archive an active file")
    if expected_sequence == 0 or first_recorded_at is None or last_recorded_at is None:
        raise ValueError("tape is empty")

    partition = parse_timestamp(first_recorded_at, "recorded_at", 1)
    return {
        "schema": "monday.polymarket.market_updates.v1",
        "venue": "polymarket",
        "dataset": dataset,
        "format": "ndjson.zst",
        "replay_scope": "complete_sampled_normalized_session",
        "venue_depth_complete": False,
        "session_complete": True,
        "events": expected_sequence,
        "event_types": dict(sorted(event_types.items())),
        "start_sequence": first_sequence,
        "end_sequence": last_sequence,
        "sequence_gaps": 0,
        "start_recorded_at": first_recorded_at,
        "end_recorded_at": last_recorded_at,
        "date": partition.strftime("%Y-%m-%d"),
        "hour": partition.strftime("%H"),
        "symbols": sorted(symbols),
        "token_count": len(token_ids),
        "recording_policy": {
            "quote_sample_ms": quote_sample_ms,
            "quote_depth_levels": quote_depth_levels,
            "event_scoped_quotes": True,
        },
        "field_presence": {
            kind: dict(sorted(fields.items())) for kind, fields in sorted(present_fields.items())
        },
        "field_non_null": {
            kind: dict(sorted(fields.items())) for kind, fields in sorted(non_null_fields.items())
        },
        "quality": {
            "crossed_quotes": crossed_quotes,
            "one_sided_quotes": one_sided_quotes,
            "empty_quotes": empty_quotes,
            "out_of_range_prices": out_of_range_prices,
            "negative_sizes": negative_sizes,
            "max_bid_levels": max_bid_levels,
            "max_ask_levels": max_ask_levels,
        },
        "source_file": path.name,
        "source_bytes": before.st_size,
    }


def prepare_artifacts(
    source: Path,
    dataset: str,
    quote_depth_levels: int,
    quote_sample_ms: int,
    zstd_timeout: int,
) -> tuple[Artifacts, dict[str, Any]]:
    metadata = scan_tape(source, dataset, quote_depth_levels, quote_sample_ms)
    data = source.with_suffix(source.suffix + ".zst")
    temporary_data = data.with_suffix(data.suffix + ".tmp")
    subprocess.run(
        ["zstd", "-q", "-f", "-T1", "-3", str(source), "-o", str(temporary_data)],
        check=True,
        timeout=zstd_timeout,
    )
    with temporary_data.open("rb") as handle:
        os.fsync(handle.fileno())
    os.replace(temporary_data, data)

    digest = sha256_file(data)
    metadata.update({"file": data.name, "bytes": data.stat().st_size, "sha256": digest})
    manifest = data.with_name(data.name + ".manifest.json")
    success = data.with_name(data.name + "._SUCCESS")
    atomic_json(manifest, metadata)
    success.write_text(digest + "\n", encoding="utf-8")
    with success.open("rb") as handle:
        os.fsync(handle.fileno())
    object_prefix = (
        f"lake/raw/venue=polymarket/dataset={dataset}/"
        f"date={metadata['date']}/hour={metadata['hour']}"
    )
    return Artifacts(source, data, manifest, success, object_prefix), metadata


def upload_artifacts(
    artifacts: Artifacts,
    bucket: str,
    endpoint: str,
    region: str,
    profile: str,
    timeout: int,
) -> str:
    for source in (artifacts.data, artifacts.manifest, artifacts.success):
        destination = f"oss://{bucket}/{artifacts.object_prefix}/{source.name}"
        subprocess.run(
            [
                "aliyun",
                "ossutil",
                "cp",
                str(source),
                destination,
                "--profile",
                profile,
                "--endpoint",
                endpoint,
                "--region",
                region,
                "--force",
            ],
            check=True,
            timeout=timeout,
        )
    for path in (artifacts.source, artifacts.data, artifacts.manifest, artifacts.success):
        path.unlink()
    return f"oss://{bucket}/{artifacts.object_prefix}/{artifacts.data.name}"


def read_status(path: Path) -> dict[str, Any]:
    try:
        value = json.loads(path.read_text(encoding="utf-8"))
        return value if isinstance(value, dict) else {}
    except (FileNotFoundError, json.JSONDecodeError):
        return {}


def run(args: argparse.Namespace) -> int:
    spool_dir = args.spool_dir
    spool_dir.mkdir(parents=True, exist_ok=True)
    status_path = spool_dir / "upload-status.json"
    status = read_status(status_path)
    try:
        for source in discover_rotated_tapes(spool_dir):
            artifacts, _ = prepare_artifacts(
                source,
                args.dataset,
                args.quote_depth_levels,
                args.quote_sample_ms,
                args.zstd_timeout,
            )
            uploaded = upload_artifacts(
                artifacts,
                args.bucket,
                args.endpoint,
                args.region,
                args.profile,
                args.oss_timeout,
            )
            status.update(
                {
                    "last_success_at": utc_now(),
                    "last_uploaded_object": uploaded,
                    "last_error_at": None,
                    "last_error": None,
                }
            )
        status.update(
            {
                "updated_at": utc_now(),
                "pending_segments": len(discover_rotated_tapes(spool_dir)),
            }
        )
        atomic_json(status_path, status)
        return 0
    except Exception as error:
        status.update(
            {
                "updated_at": utc_now(),
                "pending_segments": len(discover_rotated_tapes(spool_dir)),
                "last_error_at": utc_now(),
                "last_error": str(error),
            }
        )
        atomic_json(status_path, status)
        print(f"Polymarket tape upload failed: {error}", flush=True)
        return 1


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--spool-dir",
        type=Path,
        default=Path("/data/monday/spool/polymarket"),
    )
    parser.add_argument("--dataset", default="crypto_expiry")
    parser.add_argument("--quote-depth-levels", type=int, default=1)
    parser.add_argument("--quote-sample-ms", type=int, default=1000)
    parser.add_argument("--bucket", default=os.getenv("OSS_BUCKET", "monday-lob-apne1-1045353359"))
    parser.add_argument(
        "--endpoint",
        default=os.getenv("OSS_ENDPOINT", "oss-ap-northeast-1-internal.aliyuncs.com"),
    )
    parser.add_argument("--region", default=os.getenv("OSS_REGION", "ap-northeast-1"))
    parser.add_argument("--profile", default=os.getenv("ALIYUN_PROFILE", "ecs-role"))
    parser.add_argument(
        "--zstd-timeout",
        type=int,
        default=int(os.getenv("ZSTD_TIMEOUT_SECONDS", "300")),
    )
    parser.add_argument(
        "--oss-timeout",
        type=int,
        default=int(os.getenv("OSS_COPY_TIMEOUT_SECONDS", "300")),
    )
    args = parser.parse_args()
    if not re.fullmatch(r"[a-z0-9_-]+", args.dataset):
        parser.error("dataset must match [a-z0-9_-]+")
    if args.quote_depth_levels < 0 or args.quote_sample_ms < 0:
        parser.error("recording policy values must be non-negative")
    return args


if __name__ == "__main__":
    raise SystemExit(run(parse_args()))
