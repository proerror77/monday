#!/usr/bin/env python3
"""Validate, compress, and upload closed Polymarket tape sessions to OSS."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import re
import subprocess
import tempfile
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
    "market_metadata",
    "polymarket_trade",
    "market_settlement",
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


def decimal_or_none(value: Any, field: str, line_number: int) -> Decimal | None:
    if value is None:
        return None
    try:
        return Decimal(str(value))
    except (InvalidOperation, ValueError) as error:
        raise ValueError(f"line {line_number}: {field} must be numeric") from error


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
    known_event_tokens: set[str] = set()
    contextless_quote_tokens: set[str] = set()
    first_recorded_at: str | None = None
    last_recorded_at: str | None = None
    previous_recorded_at: datetime | None = None
    first_sequence: int | None = None
    last_sequence: int | None = None
    expected_sequence: int | None = None
    crossed_quotes = 0
    one_sided_quotes = 0
    empty_quotes = 0
    out_of_range_prices = 0
    negative_sizes = 0
    max_bid_levels = 0
    max_ask_levels = 0
    contextless_quotes = 0
    market_ids: set[str] = set()
    condition_ids: set[str] = set()
    record_ids: set[str] = set()
    record_id_versions: set[str] = set()
    duplicate_record_ids = 0
    source_field_presence: dict[str, Counter[str]] = defaultdict(Counter)
    source_field_non_null: dict[str, Counter[str]] = defaultdict(Counter)

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
            if not isinstance(sequence, int) or isinstance(sequence, bool) or sequence < 0:
                raise ValueError(f"line {line_number}: sequence must be a non-negative integer")
            if expected_sequence is None:
                expected_sequence = sequence
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
            if kind == "event_discovered":
                for token_field in ("up_token", "down_token"):
                    event_token = update.get(token_field)
                    if isinstance(event_token, str) and event_token:
                        known_event_tokens.add(event_token)

            market_id = update.get("market_id")
            if isinstance(market_id, str) and market_id:
                market_ids.add(market_id)
            condition_id = update.get("condition_id")
            if isinstance(condition_id, str) and condition_id:
                condition_ids.add(condition_id)
            record_id = update.get("record_id")
            if isinstance(record_id, str) and record_id:
                if record_id in record_ids:
                    duplicate_record_ids += 1
                record_ids.add(record_id)
            if kind == "polymarket_trade":
                version = update.get("record_id_version")
                record_id_versions.add(
                    version if isinstance(version, str) and version else "v1_legacy"
                )

            raw_field = {
                "market_metadata": "market",
                "polymarket_trade": "trade",
                "market_settlement": "market",
            }.get(kind)
            if raw_field is not None:
                raw_payload = update.get(raw_field)
                if not isinstance(raw_payload, dict):
                    raise ValueError(f"line {line_number}: {kind}.{raw_field} must be an object")
                for field, value in raw_payload.items():
                    source_field_presence[kind][field] += 1
                    if value is not None:
                        source_field_non_null[kind][field] += 1

            if kind == "market_metadata":
                for required in ("market_id", "condition_id", "symbol", "retrieved_at"):
                    if not update.get(required):
                        raise ValueError(f"line {line_number}: market_metadata requires {required}")
            elif kind == "polymarket_trade":
                for required in (
                    "record_id",
                    "condition_id",
                    "token_id",
                    "symbol",
                    "side",
                    "trade_ts",
                    "transaction_hash",
                ):
                    if update.get(required) in (None, ""):
                        raise ValueError(f"line {line_number}: polymarket_trade requires {required}")
                if update.get("side") not in {"BUY", "SELL"}:
                    raise ValueError(f"line {line_number}: polymarket_trade side must be BUY or SELL")
                decimal_or_none(update.get("size"), "size", line_number)
                price = decimal_or_none(update.get("price"), "price", line_number)
                if price is None or not Decimal("0") <= price <= Decimal("1"):
                    raise ValueError(f"line {line_number}: polymarket_trade price must be within [0, 1]")
                parse_timestamp(update.get("trade_ts"), "trade_ts", line_number)
            elif kind == "market_settlement":
                for required in (
                    "market_id",
                    "condition_id",
                    "symbol",
                    "winning_token_id",
                    "winning_outcome",
                    "resolution_source",
                    "retrieved_at",
                ):
                    if update.get(required) in (None, ""):
                        raise ValueError(f"line {line_number}: market_settlement requires {required}")

            if kind == "quote":
                if not isinstance(token_id, str) or token_id not in known_event_tokens:
                    contextless_quotes += 1
                    if isinstance(token_id, str):
                        contextless_quote_tokens.add(token_id)
                bid = decimal_or_none(update.get("bid"), "bid", line_number)
                ask = decimal_or_none(update.get("ask"), "ask", line_number)
                bid_size = decimal_or_none(update.get("bid_size"), "bid_size", line_number)
                ask_size = decimal_or_none(update.get("ask_size"), "ask_size", line_number)
                bid_levels = update.get("bid_levels") or []
                ask_levels = update.get("ask_levels") or []
                if not isinstance(bid_levels, list) or not isinstance(ask_levels, list):
                    raise ValueError(f"line {line_number}: quote levels must be arrays")
                if quote_depth_levels and (
                    len(bid_levels) > quote_depth_levels or len(ask_levels) > quote_depth_levels
                ):
                    raise ValueError(f"line {line_number}: quote exceeds configured depth")
                max_bid_levels = max(max_bid_levels, len(bid_levels))
                max_ask_levels = max(max_ask_levels, len(ask_levels))
                for side, levels in (("bid_levels", bid_levels), ("ask_levels", ask_levels)):
                    for level_index, level in enumerate(levels):
                        if not isinstance(level, dict):
                            raise ValueError(
                                f"line {line_number}: {side}[{level_index}] must be an object"
                            )
                        level_price = decimal_or_none(
                            level.get("price"), f"{side}[{level_index}].price", line_number
                        )
                        level_size = decimal_or_none(
                            level.get("size"), f"{side}[{level_index}].size", line_number
                        )
                        if level_price is None or level_size is None:
                            raise ValueError(
                                f"line {line_number}: {side}[{level_index}] requires price and size"
                            )
                        if not Decimal("0") <= level_price <= Decimal("1"):
                            out_of_range_prices += 1
                        if level_size < 0:
                            negative_sizes += 1
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
            expected_sequence = sequence + 1
            previous_recorded_at = recorded_at

    after = path.stat()
    if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
        raise ValueError("tape changed while being validated; refusing to archive an active file")
    if first_sequence is None or first_recorded_at is None or last_recorded_at is None:
        raise ValueError("tape is empty")

    partition = parse_timestamp(first_recorded_at, "recorded_at", 1)
    event_context_complete = contextless_quotes == 0
    has_quotes = event_types.get("quote", 0) > 0
    has_reference_records = any(
        event_types.get(kind, 0) > 0
        for kind in ("market_metadata", "polymarket_trade", "market_settlement")
    )
    depth_complete = has_quotes and quote_depth_levels == 0
    temporal_updates_complete = has_quotes and quote_sample_ms == 0
    replay_scope = (
        "complete_reference_hour_segment"
        if has_reference_records and not has_quotes
        else (
            (
                (
                    "complete_full_depth_normalized_hour_segment"
                    if temporal_updates_complete
                    else "complete_full_depth_sampled_normalized_hour_segment"
                )
                if depth_complete
                else "complete_sampled_normalized_hour_segment"
            )
            if event_context_complete
            else "sampled_normalized_hour_segment_requires_prior_event_context"
        )
    )
    return {
        "schema": "monday.polymarket.raw.v1",
        "canonical": True,
        "venue": "polymarket",
        "dataset": dataset,
        "format": "ndjson.zst",
        "replay_scope": replay_scope,
        "venue_depth_complete": depth_complete,
        "temporal_updates_complete": temporal_updates_complete,
        "segment_complete": True,
        "source_session_closed": True,
        "event_context_complete": event_context_complete,
        "contextless_quote_tokens": sorted(contextless_quote_tokens),
        "events": last_sequence - first_sequence + 1,
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
        "market_count": len(market_ids),
        "condition_count": len(condition_ids),
        "record_id_versions": sorted(record_id_versions),
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
        "source_field_presence": {
            kind: dict(sorted(fields.items()))
            for kind, fields in sorted(source_field_presence.items())
        },
        "source_field_non_null": {
            kind: dict(sorted(fields.items()))
            for kind, fields in sorted(source_field_non_null.items())
        },
        "quality": {
            "crossed_quotes": crossed_quotes,
            "one_sided_quotes": one_sided_quotes,
            "empty_quotes": empty_quotes,
            "out_of_range_prices": out_of_range_prices,
            "negative_sizes": negative_sizes,
            "max_bid_levels": max_bid_levels,
            "max_ask_levels": max_ask_levels,
            "contextless_quotes": contextless_quotes,
            "duplicate_record_ids": duplicate_record_ids,
        },
        "source_file": path.name,
        "source_bytes": before.st_size,
    }


def split_tape_by_utc_hour(source: Path) -> list[Path]:
    """Copy a validated closed tape into deterministic UTC-hour chunks."""
    staging_dir = source.parent / ".upload-staging" / source.name
    staging_dir.mkdir(parents=True, exist_ok=True)
    for temporary in staging_dir.glob("*.tmp"):
        temporary.unlink()

    chunks: list[Path] = []
    output = None
    output_path: Path | None = None
    current_hour: str | None = None
    try:
        with source.open("rb") as handle:
            for line_number, raw_line in enumerate(handle, start=1):
                record = json.loads(raw_line)
                recorded_at = parse_timestamp(record.get("recorded_at"), "recorded_at", line_number)
                hour = recorded_at.strftime("%Y%m%dT%H")
                if hour != current_hour:
                    if output is not None and output_path is not None:
                        output.flush()
                        os.fsync(output.fileno())
                        output.close()
                        final_path = output_path.with_suffix("")
                        os.replace(output_path, final_path)
                        chunks.append(final_path)
                    output_path = staging_dir / f"{source.stem}.{hour}.ndjson.tmp"
                    output = output_path.open("wb")
                    current_hour = hour
                output.write(raw_line)
        if output is not None and output_path is not None:
            output.flush()
            os.fsync(output.fileno())
            output.close()
            output = None
            final_path = output_path.with_suffix("")
            os.replace(output_path, final_path)
            chunks.append(final_path)
    finally:
        if output is not None:
            output.close()
    return chunks


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
    verify_remote_artifacts(artifacts, bucket, endpoint, region, profile, timeout)
    for path in (artifacts.source, artifacts.data, artifacts.manifest, artifacts.success):
        path.unlink()
    return f"oss://{bucket}/{artifacts.object_prefix}/{artifacts.data.name}"


def verify_remote_artifacts(
    artifacts: Artifacts,
    bucket: str,
    endpoint: str,
    region: str,
    profile: str,
    timeout: int,
) -> None:
    """Read all three objects back before deleting the local closed tape."""
    expected_manifest = json.loads(artifacts.manifest.read_text(encoding="utf-8"))
    with tempfile.TemporaryDirectory(prefix=".oss-verify-", dir=artifacts.source.parent) as directory:
        verify_dir = Path(directory)
        downloaded: dict[str, Path] = {}
        for source in (artifacts.data, artifacts.manifest, artifacts.success):
            destination = verify_dir / source.name
            remote = f"oss://{bucket}/{artifacts.object_prefix}/{source.name}"
            subprocess.run(
                [
                    "aliyun",
                    "ossutil",
                    "cp",
                    remote,
                    str(destination),
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
            downloaded[source.name] = destination

        remote_data = downloaded[artifacts.data.name]
        if remote_data.stat().st_size != expected_manifest["bytes"]:
            raise ValueError("remote data size does not match manifest")
        if sha256_file(remote_data) != expected_manifest["sha256"]:
            raise ValueError("remote data sha256 does not match manifest")
        if downloaded[artifacts.manifest.name].read_bytes() != artifacts.manifest.read_bytes():
            raise ValueError("remote manifest does not match local manifest")
        if downloaded[artifacts.success.name].read_text(encoding="utf-8").strip() != expected_manifest[
            "sha256"
        ]:
            raise ValueError("remote _SUCCESS does not match manifest")


def archive_source(source: Path, args: argparse.Namespace) -> list[str]:
    """Validate one closed session, upload UTC-hour chunks, then delete it."""
    source_manifest = scan_tape(
        source, args.dataset, args.quote_depth_levels, args.quote_sample_ms
    )
    if source_manifest["start_sequence"] != 0:
        raise ValueError(
            f"closed source tape must start at sequence 0; actual={source_manifest['start_sequence']}"
        )
    uploaded: list[str] = []
    chunks = split_tape_by_utc_hour(source)
    for chunk in chunks:
        artifacts, _ = prepare_artifacts(
            chunk,
            args.dataset,
            args.quote_depth_levels,
            args.quote_sample_ms,
            args.zstd_timeout,
        )
        uploaded.append(
            upload_artifacts(
                artifacts,
                args.bucket,
                args.endpoint,
                args.region,
                args.profile,
                args.oss_timeout,
            )
        )
    source.unlink()
    staging_dir = source.parent / ".upload-staging" / source.name
    try:
        staging_dir.rmdir()
        staging_dir.parent.rmdir()
    except OSError:
        pass
    return uploaded


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
    failures: list[dict[str, str]] = []
    for source in discover_rotated_tapes(spool_dir):
        try:
            uploaded = archive_source(source, args)
            status.update(
                {
                    "last_success_at": utc_now(),
                    "last_uploaded_object": uploaded[-1],
                }
            )
        except Exception as error:
            failures.append({"source": source.name, "error": str(error)})
            print(f"Polymarket tape upload failed for {source.name}: {error}", flush=True)

    status.update(
        {
            "updated_at": utc_now(),
            "pending_segments": len(discover_rotated_tapes(spool_dir)),
            "failed_segments": failures,
            "last_error_at": utc_now() if failures else None,
            "last_error": failures[-1]["error"] if failures else None,
        }
    )
    atomic_json(status_path, status)
    return 1 if failures else 0


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--spool-dir",
        type=Path,
        default=Path("/data/monday/spool/polymarket"),
    )
    parser.add_argument("--dataset", default="crypto_expiry")
    parser.add_argument(
        "--quote-depth-levels",
        type=int,
        default=0,
        help="maximum persisted levels per side; 0 means unbounded",
    )
    parser.add_argument("--quote-sample-ms", type=int, default=0)
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
