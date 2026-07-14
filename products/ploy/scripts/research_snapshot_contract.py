#!/usr/bin/env python3
"""Research snapshot contract helpers for promotion consumers."""

from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any


def _parse_utc_ts(raw: Any) -> datetime | None:
    if not raw:
        return None
    try:
        parsed = datetime.fromisoformat(str(raw).replace("Z", "+00:00"))
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def _proof_window_matches(
    snapshot_payload: dict[str, Any],
    proof_payload: dict[str, Any],
) -> tuple[bool, str]:
    snapshot_start = _parse_utc_ts(
        snapshot_payload.get("start") or snapshot_payload.get("start_ts")
    )
    snapshot_end = _parse_utc_ts(snapshot_payload.get("end") or snapshot_payload.get("end_ts"))
    proof_start = _parse_utc_ts(proof_payload.get("start_ts") or proof_payload.get("start"))
    proof_end = _parse_utc_ts(proof_payload.get("end_ts") or proof_payload.get("end"))
    if not snapshot_start or not snapshot_end:
        return False, "snapshot_window_missing"
    if not proof_start or not proof_end:
        return False, "proof_window_missing"
    if proof_start > snapshot_start or proof_end < snapshot_end:
        return (
            False,
            "window_not_covered:"
            f"{proof_start.isoformat()}->{proof_end.isoformat()}!covers"
            f"{snapshot_start.isoformat()}->{snapshot_end.isoformat()}",
        )
    return True, ""


def _load_full_depth_execution_surface_proof(
    path: str | None,
    *,
    snapshot_payload: dict[str, Any],
) -> dict[str, Any] | None:
    if not path:
        return None
    proof_path = Path(path)
    payload = json.loads(proof_path.read_text(encoding="utf-8"))
    surface = str(payload.get("surface") or "")
    blockers: list[str] = []
    if payload.get("schema_version") != "full_depth_execution_surface.v1":
        blockers.append("unsupported_schema_version")
    if not surface:
        blockers.append("missing_surface")
    if payload.get("full_fidelity") is not True:
        blockers.append("not_full_fidelity")
    if payload.get("incomplete") is True:
        blockers.append("incomplete")
    try:
        checked_hours = int(payload.get("checked_hours") or 0)
        existing_hours = int(payload.get("existing_hours") or 0)
    except (TypeError, ValueError):
        checked_hours = 0
        existing_hours = 0
    if checked_hours <= 0:
        blockers.append("checked_hours_empty")
    if existing_hours < checked_hours:
        blockers.append(f"missing_hours:{existing_hours}<{checked_hours}")
    try:
        row_count = int(payload.get("row_count") or 0)
    except (TypeError, ValueError):
        row_count = 0
    if row_count <= 0:
        blockers.append("row_count_empty")
    window_matches, window_blocker = _proof_window_matches(snapshot_payload, payload)
    if not window_matches:
        blockers.append(window_blocker)
    return {
        "path": str(proof_path),
        "schema_version": payload.get("schema_version", ""),
        "surface": surface,
        "source": payload.get("source", ""),
        "start_ts": payload.get("start_ts", ""),
        "end_ts": payload.get("end_ts", ""),
        "checked_hours": checked_hours,
        "existing_hours": existing_hours,
        "row_count": row_count,
        "full_fidelity": payload.get("full_fidelity") is True,
        "incomplete": payload.get("incomplete") is True,
        "valid": not blockers,
        "blockers": blockers,
    }


def _load_data_audit_contract(path: str | None) -> dict[str, Any]:
    if not path:
        return {}
    audit_path = Path(path)
    payload = json.loads(audit_path.read_text(encoding="utf-8"))
    required_sources = {str(item) for item in payload.get("required_sources") or []}
    blockers: list[str] = []
    checked_sources: list[dict[str, Any]] = []
    for item in (payload.get("gap_audits") or []) + (payload.get("window_audits") or []):
        if not isinstance(item, dict):
            continue
        source_id = str(item.get("source_id") or "")
        if required_sources and source_id not in required_sources:
            continue
        try:
            expected_buckets = int(item.get("expected_buckets") or 0)
            present_buckets = int(item.get("present_buckets") or 0)
        except (TypeError, ValueError):
            expected_buckets = 0
            present_buckets = 0
        if expected_buckets > 0 and present_buckets == 0:
            blockers.append(
                f"data_audit_zero_coverage:{source_id}:0<{expected_buckets}"
            )
        checked_sources.append(
            {
                "source_id": source_id,
                "status": item.get("status", ""),
                "coverage_status": item.get("coverage_status", ""),
                "expected_buckets": expected_buckets,
                "present_buckets": present_buckets,
                "missing_buckets": item.get("missing_buckets"),
                "coverage_pct": item.get("coverage_pct"),
            }
        )
    return {
        "path": str(audit_path),
        "overall_status": payload.get("overall_status", ""),
        "audit_window_start_ts": payload.get("audit_window_start_ts", ""),
        "audit_window_end_ts": payload.get("audit_window_end_ts", ""),
        "required_sources": sorted(required_sources),
        "checked_sources": checked_sources,
        "blocking_risk_flags": sorted(set(blockers)),
    }


def load_snapshot_execution_contract(
    path: str | None,
    *,
    full_depth_execution_surface_json: str | None = None,
    data_audit_report_json: str | None = None,
) -> dict[str, Any]:
    if not path:
        return {}
    manifest_path = Path(path)
    payload = json.loads(manifest_path.read_text(encoding="utf-8"))
    surfaces = payload.get("source_surfaces") or []
    sampled_required_execution_surfaces: list[str] = []
    raw_full_fidelity_required_execution_surfaces: list[str] = []
    for surface in surfaces:
        if not isinstance(surface, dict):
            continue
        if surface.get("gate_category") != "required_for_execution":
            continue
        name = str(surface.get("name") or "<unknown>")
        if surface.get("raw_full_fidelity") is True:
            raw_full_fidelity_required_execution_surfaces.append(name)
        if surface.get("snapshot_sampled") is True:
            sampled_required_execution_surfaces.append(name)
    proof = _load_full_depth_execution_surface_proof(
        full_depth_execution_surface_json,
        snapshot_payload=payload,
    )
    execution_surface_proofs = [proof] if proof else []
    satisfied_execution_surfaces = {
        item["surface"] for item in execution_surface_proofs if item.get("valid")
    }
    unsatisfied_sampled_surfaces = [
        name
        for name in sampled_required_execution_surfaces
        if name not in satisfied_execution_surfaces
    ]
    blocking_flags = [
        f"sampled_snapshot_required_for_execution_surface:{name}"
        for name in sorted(unsatisfied_sampled_surfaces)
    ]
    data_audit_contract = _load_data_audit_contract(data_audit_report_json)
    blocking_flags.extend(data_audit_contract.get("blocking_risk_flags") or [])
    for item in execution_surface_proofs:
        if item.get("valid"):
            continue
        surface_name = item.get("surface") or "<unknown>"
        for blocker in item.get("blockers", []):
            blocking_flags.append(f"full_depth_execution_surface_invalid:{surface_name}:{blocker}")
    return {
        "manifest_path": str(manifest_path),
        "schema_version": payload.get("schema_version", ""),
        "snapshot_hash": payload.get("snapshot_hash", ""),
        "source_kind": payload.get("source_kind", ""),
        "start": payload.get("start") or payload.get("start_ts") or "",
        "end": payload.get("end") or payload.get("end_ts") or "",
        "sampled_required_execution_surfaces": sorted(sampled_required_execution_surfaces),
        "raw_full_fidelity_required_execution_surfaces": sorted(
            raw_full_fidelity_required_execution_surfaces
        ),
        "satisfied_execution_surfaces": sorted(satisfied_execution_surfaces),
        "full_depth_execution_surface_proofs": execution_surface_proofs,
        "data_audit_contract": data_audit_contract,
        "blocking_risk_flags": blocking_flags,
    }


def snapshot_blocks_execution_claim(snapshot_contract: dict[str, Any]) -> bool:
    return bool(snapshot_contract.get("blocking_risk_flags"))
