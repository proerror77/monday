#!/usr/bin/env python3
"""Persist standalone full-depth execution-surface evidence into Research OS."""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import os
from dataclasses import dataclass
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


SCHEMA_VERSION = "full_depth_execution_surface.v1"
DEFAULT_SOURCE_WORKFLOW = "collect-full-depth-execution-surface.yml"


def parse_utc_ts(raw: str | None) -> datetime | None:
    if not raw:
        return None
    parsed = datetime.fromisoformat(str(raw).replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def file_sha256(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def string_field(payload: dict[str, Any], key: str) -> str:
    value = payload.get(key)
    return str(value).strip() if value is not None else ""


def int_field(payload: dict[str, Any], key: str) -> int:
    value = payload.get(key)
    if value is None or value == "":
        return 0
    return int(value)


def bool_field(payload: dict[str, Any], key: str, *, default: bool) -> bool:
    value = payload.get(key)
    if isinstance(value, bool):
        return value
    if value is None:
        return default
    if str(value).lower() in {"true", "1", "yes"}:
        return True
    if str(value).lower() in {"false", "0", "no"}:
        return False
    return default


@dataclass(frozen=True)
class FullDepthExecutionSurfaceRow:
    full_depth_execution_surface_id: str
    run_id: str
    source_workflow: str
    workflow_run_id: str
    workflow_run_url: str
    artifact_name: str
    artifact_sha256: str
    artifact_json: dict[str, Any]
    schema_version: str
    surface: str
    source: str
    data_snapshot_id: str | None
    window_start_ts: datetime
    window_end_ts: datetime
    checked_hours: int
    existing_hours: int
    exported_hours: int
    row_count: int
    full_fidelity: bool
    incomplete: bool
    valid: bool
    blockers: list[str]


def build_row(
    *,
    surface_json: Path,
    run_id: str,
    source_workflow: str,
    workflow_run_id: str,
    workflow_run_url: str,
    artifact_name: str,
    data_snapshot_id: str | None = None,
    dataset_start_ts: datetime | None = None,
    dataset_end_ts: datetime | None = None,
) -> FullDepthExecutionSurfaceRow:
    payload = json.loads(surface_json.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise SystemExit("surface JSON must be an object")

    artifact_sha256 = file_sha256(surface_json)
    schema_version = string_field(payload, "schema_version")
    surface = string_field(payload, "surface")
    source = string_field(payload, "source") or "unknown"
    window_start_ts = parse_utc_ts(payload.get("start_ts") or payload.get("start"))
    window_end_ts = parse_utc_ts(payload.get("end_ts") or payload.get("end"))
    if window_start_ts is None:
        raise SystemExit(f"{surface_json} missing start_ts")
    if window_end_ts is None:
        raise SystemExit(f"{surface_json} missing end_ts")

    checked_hours = int_field(payload, "checked_hours")
    existing_hours = int_field(payload, "existing_hours")
    exported_hours = int_field(payload, "exported_hours")
    row_count = int_field(payload, "row_count")
    full_fidelity = bool_field(payload, "full_fidelity", default=False)
    incomplete = bool_field(payload, "incomplete", default=True)

    blockers: list[str] = []
    if schema_version != SCHEMA_VERSION:
        blockers.append("unsupported_schema_version")
    if not surface:
        blockers.append("missing_surface")
    if not full_fidelity:
        blockers.append("not_full_fidelity")
    if incomplete:
        blockers.append("incomplete")
    if checked_hours <= 0:
        blockers.append("checked_hours_empty")
    covered_hours = existing_hours + exported_hours
    if covered_hours < checked_hours:
        blockers.append(f"missing_hours:{covered_hours}<{checked_hours}")
    if row_count <= 0:
        blockers.append("row_count_empty")
    if window_start_ts >= window_end_ts:
        blockers.append("invalid_window")
    if dataset_start_ts and dataset_end_ts:
        if window_start_ts > dataset_start_ts or window_end_ts < dataset_end_ts:
            blockers.append(
                "window_not_covered:"
                f"{window_start_ts.isoformat()}->{window_end_ts.isoformat()}"
                f"!covers{dataset_start_ts.isoformat()}->{dataset_end_ts.isoformat()}"
            )
    blockers = sorted(set(blockers))

    return FullDepthExecutionSurfaceRow(
        full_depth_execution_surface_id=string_field(payload, "full_depth_execution_surface_id")
        or f"full_depth_execution_surface:{artifact_sha256[:32]}",
        run_id=run_id,
        source_workflow=(
            source_workflow
            or string_field(payload, "source_workflow")
            or DEFAULT_SOURCE_WORKFLOW
        ),
        workflow_run_id=workflow_run_id or string_field(payload, "workflow_run_id"),
        workflow_run_url=workflow_run_url or string_field(payload, "workflow_run_url"),
        artifact_name=artifact_name or string_field(payload, "artifact_name"),
        artifact_sha256=artifact_sha256,
        artifact_json=payload,
        schema_version=schema_version,
        surface=surface,
        source=source,
        data_snapshot_id=data_snapshot_id,
        window_start_ts=window_start_ts,
        window_end_ts=window_end_ts,
        checked_hours=checked_hours,
        existing_hours=existing_hours,
        exported_hours=exported_hours,
        row_count=row_count,
        full_fidelity=full_fidelity,
        incomplete=incomplete,
        valid=not blockers,
        blockers=blockers,
    )


async def persist_row(db_url: str, row: FullDepthExecutionSurfaceRow) -> None:
    import asyncpg

    conn = await asyncpg.connect(db_url)
    try:
        await conn.execute(
            """
            INSERT INTO full_depth_execution_surfaces (
                full_depth_execution_surface_id,
                run_id,
                source_workflow,
                workflow_run_id,
                workflow_run_url,
                artifact_name,
                artifact_sha256,
                artifact_json,
                schema_version,
                surface,
                source,
                data_snapshot_id,
                window_start_ts,
                window_end_ts,
                checked_hours,
                existing_hours,
                exported_hours,
                row_count,
                full_fidelity,
                incomplete,
                valid,
                blockers_json
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9, $10, $11, $12, $13, $14,
                $15, $16, $17, $18, $19, $20, $21, $22::jsonb
            )
            ON CONFLICT (full_depth_execution_surface_id) DO UPDATE SET
                run_id = EXCLUDED.run_id,
                source_workflow = EXCLUDED.source_workflow,
                workflow_run_id = EXCLUDED.workflow_run_id,
                workflow_run_url = EXCLUDED.workflow_run_url,
                artifact_name = EXCLUDED.artifact_name,
                artifact_sha256 = EXCLUDED.artifact_sha256,
                artifact_json = EXCLUDED.artifact_json,
                schema_version = EXCLUDED.schema_version,
                surface = EXCLUDED.surface,
                source = EXCLUDED.source,
                data_snapshot_id = EXCLUDED.data_snapshot_id,
                window_start_ts = EXCLUDED.window_start_ts,
                window_end_ts = EXCLUDED.window_end_ts,
                checked_hours = EXCLUDED.checked_hours,
                existing_hours = EXCLUDED.existing_hours,
                exported_hours = EXCLUDED.exported_hours,
                row_count = EXCLUDED.row_count,
                full_fidelity = EXCLUDED.full_fidelity,
                incomplete = EXCLUDED.incomplete,
                valid = EXCLUDED.valid,
                blockers_json = EXCLUDED.blockers_json
            """,
            row.full_depth_execution_surface_id,
            row.run_id,
            row.source_workflow,
            row.workflow_run_id or None,
            row.workflow_run_url or None,
            row.artifact_name or None,
            row.artifact_sha256,
            json.dumps(row.artifact_json, sort_keys=True),
            row.schema_version,
            row.surface,
            row.source,
            row.data_snapshot_id,
            row.window_start_ts,
            row.window_end_ts,
            row.checked_hours,
            row.existing_hours,
            row.exported_hours,
            row.row_count,
            row.full_fidelity,
            row.incomplete,
            row.valid,
            json.dumps(row.blockers, sort_keys=True),
        )
    finally:
        await conn.close()


def parser() -> argparse.ArgumentParser:
    parsed = argparse.ArgumentParser()
    parsed.add_argument("--db-url", default=os.environ.get("DATABASE_URL", ""))
    parsed.add_argument("--surface-json", required=True, type=Path)
    parsed.add_argument("--run-id", default="")
    parsed.add_argument("--source-workflow", default=DEFAULT_SOURCE_WORKFLOW)
    parsed.add_argument("--workflow-run-id", default=os.environ.get("GITHUB_RUN_ID", ""))
    parsed.add_argument("--workflow-run-url", default="")
    parsed.add_argument("--artifact-name", default="")
    parsed.add_argument("--data-snapshot-id", default="")
    parsed.add_argument("--dataset-start-ts", default="")
    parsed.add_argument("--dataset-end-ts", default="")
    parsed.add_argument("--dry-run", action="store_true")
    parsed.add_argument(
        "--require-valid",
        action="store_true",
        help="Exit non-zero and do not persist when the surface proof has blockers.",
    )
    parsed.add_argument("--report-json", type=Path)
    return parsed


async def main_async() -> None:
    args = parser().parse_args()
    row = build_row(
        surface_json=args.surface_json,
        run_id=args.run_id or f"{DEFAULT_SOURCE_WORKFLOW}:{args.workflow_run_id or 'manual'}",
        source_workflow=args.source_workflow,
        workflow_run_id=args.workflow_run_id,
        workflow_run_url=args.workflow_run_url,
        artifact_name=args.artifact_name,
        data_snapshot_id=args.data_snapshot_id or None,
        dataset_start_ts=parse_utc_ts(args.dataset_start_ts),
        dataset_end_ts=parse_utc_ts(args.dataset_end_ts),
    )
    report = {
        "schema_version": "full_depth_execution_surface_persist.v1",
        "dry_run": args.dry_run,
        "full_depth_execution_surface_id": row.full_depth_execution_surface_id,
        "valid": row.valid,
        "blockers": row.blockers,
        "surface": row.surface,
        "source": row.source,
        "window_start_ts": row.window_start_ts.isoformat(),
        "window_end_ts": row.window_end_ts.isoformat(),
        "checked_hours": row.checked_hours,
        "existing_hours": row.existing_hours,
        "exported_hours": row.exported_hours,
        "row_count": row.row_count,
        "artifact_sha256": row.artifact_sha256,
    }
    text = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.report_json:
        args.report_json.write_text(text, encoding="utf-8")
    if args.require_valid and not row.valid:
        print(text, end="")
        raise SystemExit("full-depth execution surface is invalid; refusing to persist")
    if not args.dry_run:
        if not args.db_url:
            raise SystemExit("--db-url or DATABASE_URL is required unless --dry-run is set")
        await persist_row(args.db_url, row)
    print(text, end="")


def main() -> None:
    asyncio.run(main_async())


if __name__ == "__main__":
    main()
