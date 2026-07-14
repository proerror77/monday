#!/usr/bin/env python3
"""Persist standalone runtime candidate replay evidence into Research OS."""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any
from uuid import uuid4


DEFAULT_SOURCE_WORKFLOW = "runtime-candidate-replay.yml"
WRITER_AGENT = "persist_candidate_replay_tape"
EVENT_TYPE = "candidate_replay_tape"
ARTIFACT_KIND = "candidate_replay_tape"


def file_sha256(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def string_field(payload: dict[str, Any], key: str) -> str:
    value = payload.get(key)
    return str(value).strip() if value is not None else ""


def optional_string(payload: dict[str, Any], key: str) -> str | None:
    value = string_field(payload, key)
    return value or None


def canonical_json(value: Any) -> str:
    return json.dumps(value, separators=(",", ":"), sort_keys=True)


def trace_hash(
    hash_prev: str | None,
    run_id: str,
    event_type: str,
    agent_name: str,
    input_json: dict[str, Any],
    output_json: dict[str, Any],
) -> str:
    hasher = hashlib.sha256()
    for item in [
        hash_prev or "",
        run_id,
        event_type,
        agent_name,
        canonical_json(input_json),
        canonical_json(output_json),
    ]:
        hasher.update(item.encode("utf-8"))
        hasher.update(b"\n")
    return hasher.hexdigest()


def canonical_candidate_replay_evidence_stage(
    basis: str,
    explicit_evidence_stage: str | None,
) -> str:
    if basis == "runtime_market_update_replay":
        canonical = "executable_replay"
    elif basis == "factor_walk_forward_top_bucket_aggregate":
        canonical = "diagnostic"
    else:
        raise SystemExit(f"unsupported candidate replay basis: {basis}")
    if explicit_evidence_stage and explicit_evidence_stage != canonical:
        raise SystemExit(
            "candidate replay evidence_stage "
            f"{explicit_evidence_stage} contradicts basis {basis}; expected {canonical}"
        )
    return canonical


def factor_name_from_runtime_score(runtime_score: str) -> str | None:
    prefix = "autofactor_formula:"
    if runtime_score.startswith(prefix):
        return runtime_score[len(prefix) :] or None
    return None


@dataclass(frozen=True)
class CandidateReplayTapeRow:
    candidate_replay_id: str
    run_id: str
    source_workflow: str
    workflow_run_id: str
    workflow_run_url: str
    artifact_name: str
    artifact_sha256: str
    artifact_json: dict[str, Any]
    basis: str
    evidence_stage: str
    deployment_id: str | None
    strategy_profile: str
    runtime_score: str
    data_snapshot_id: str | None
    dsl_hash: str | None
    factor_name: str | None
    target: str | None
    horizon: str | None
    recording_path: str | None
    recording_sha256: str | None
    config_path: str | None
    config_sha256: str | None
    runner_source: str | None
    runner_git_sha: str | None
    decision_contract: dict[str, Any]
    acceptance_criteria: dict[str, Any]
    metrics: dict[str, Any]
    blocking_risk_flags: list[str]
    promotion_ready: bool
    promotion_decision: str
    artifact_path: Path


def build_row(
    *,
    candidate_replay_json: Path,
    run_id: str,
    source_workflow: str,
    workflow_run_id: str,
    workflow_run_url: str,
    artifact_name: str,
    data_snapshot_id: str | None,
) -> CandidateReplayTapeRow:
    payload = json.loads(candidate_replay_json.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise SystemExit("candidate replay JSON must be an object")

    artifact_sha256 = file_sha256(candidate_replay_json)
    basis = string_field(payload, "basis")
    if not basis:
        raise SystemExit(f"{candidate_replay_json} missing basis")
    evidence_stage = canonical_candidate_replay_evidence_stage(
        basis,
        optional_string(payload, "evidence_stage"),
    )
    runtime_score = string_field(payload, "runtime_score")
    source_factor = payload.get("source_factor")
    if not isinstance(source_factor, dict):
        source_factor = {}
    factor_name = (
        optional_string(source_factor, "name")
        or factor_name_from_runtime_score(runtime_score)
    )

    blockers = payload.get("blocking_risk_flags", [])
    if not isinstance(blockers, list):
        raise SystemExit("blocking_risk_flags must be an array")
    blockers = [str(item) for item in blockers]

    return CandidateReplayTapeRow(
        candidate_replay_id=string_field(payload, "candidate_replay_id")
        or f"candidate_replay:{artifact_sha256[:32]}",
        run_id=run_id,
        source_workflow=source_workflow
        or optional_string(payload, "source_workflow")
        or DEFAULT_SOURCE_WORKFLOW,
        workflow_run_id=workflow_run_id or string_field(payload, "workflow_run_id"),
        workflow_run_url=workflow_run_url or string_field(payload, "workflow_run_url"),
        artifact_name=artifact_name or string_field(payload, "artifact_name"),
        artifact_sha256=artifact_sha256,
        artifact_json=payload,
        basis=basis,
        evidence_stage=evidence_stage,
        deployment_id=optional_string(payload, "deployment_id"),
        strategy_profile=string_field(payload, "strategy_profile") or "unknown",
        runtime_score=runtime_score,
        data_snapshot_id=data_snapshot_id,
        dsl_hash=optional_string(source_factor, "dsl_hash"),
        factor_name=factor_name,
        target=optional_string(source_factor, "target"),
        horizon=optional_string(source_factor, "horizon") or "5m",
        recording_path=optional_string(payload, "recording_path"),
        recording_sha256=optional_string(payload, "recording_sha256"),
        config_path=optional_string(payload, "config_path"),
        config_sha256=optional_string(payload, "config_sha256"),
        runner_source=optional_string(payload, "runner_source"),
        runner_git_sha=optional_string(payload, "runner_git_sha"),
        decision_contract=payload.get("decision_contract")
        if isinstance(payload.get("decision_contract"), dict)
        else {},
        acceptance_criteria=payload.get("acceptance_criteria")
        if isinstance(payload.get("acceptance_criteria"), dict)
        else {},
        metrics=payload.get("metrics") if isinstance(payload.get("metrics"), dict) else {},
        blocking_risk_flags=blockers,
        promotion_ready=bool(payload.get("promotion_ready", False)),
        promotion_decision=string_field(payload, "promotion_decision") or "blocked",
        artifact_path=candidate_replay_json,
    )


async def persist_row(db_url: str, row: CandidateReplayTapeRow) -> None:
    import asyncpg

    conn = await asyncpg.connect(db_url)
    try:
        await conn.execute(
            """
            INSERT INTO candidate_replay_tapes (
                candidate_replay_id,
                run_id,
                source_workflow,
                workflow_run_id,
                workflow_run_url,
                artifact_name,
                artifact_sha256,
                artifact_json,
                basis,
                evidence_stage,
                deployment_id,
                strategy_profile,
                runtime_score,
                data_snapshot_id,
                dsl_hash,
                target,
                horizon,
                recording_path,
                recording_sha256,
                config_path,
                config_sha256,
                runner_source,
                runner_git_sha,
                decision_contract_json,
                acceptance_criteria_json,
                metrics_json,
                blocking_risk_flags_json,
                promotion_ready,
                promotion_decision
            )
            VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9, $10, $11, $12, $13,
                $14, $15, $16, $17, $18, $19, $20, $21, $22, $23, $24::jsonb,
                $25::jsonb, $26::jsonb, $27::jsonb, $28, $29
            )
            ON CONFLICT (candidate_replay_id) DO UPDATE SET
                run_id = EXCLUDED.run_id,
                source_workflow = EXCLUDED.source_workflow,
                workflow_run_id = EXCLUDED.workflow_run_id,
                workflow_run_url = EXCLUDED.workflow_run_url,
                artifact_name = EXCLUDED.artifact_name,
                artifact_sha256 = EXCLUDED.artifact_sha256,
                artifact_json = EXCLUDED.artifact_json,
                basis = EXCLUDED.basis,
                evidence_stage = EXCLUDED.evidence_stage,
                deployment_id = EXCLUDED.deployment_id,
                strategy_profile = EXCLUDED.strategy_profile,
                runtime_score = EXCLUDED.runtime_score,
                data_snapshot_id = EXCLUDED.data_snapshot_id,
                dsl_hash = EXCLUDED.dsl_hash,
                target = EXCLUDED.target,
                horizon = EXCLUDED.horizon,
                recording_path = EXCLUDED.recording_path,
                recording_sha256 = EXCLUDED.recording_sha256,
                config_path = EXCLUDED.config_path,
                config_sha256 = EXCLUDED.config_sha256,
                runner_source = EXCLUDED.runner_source,
                runner_git_sha = EXCLUDED.runner_git_sha,
                decision_contract_json = EXCLUDED.decision_contract_json,
                acceptance_criteria_json = EXCLUDED.acceptance_criteria_json,
                metrics_json = EXCLUDED.metrics_json,
                blocking_risk_flags_json = EXCLUDED.blocking_risk_flags_json,
                promotion_ready = EXCLUDED.promotion_ready,
                promotion_decision = EXCLUDED.promotion_decision
            """,
            row.candidate_replay_id,
            row.run_id,
            row.source_workflow,
            row.workflow_run_id or None,
            row.workflow_run_url or None,
            row.artifact_name or None,
            row.artifact_sha256,
            canonical_json(row.artifact_json),
            row.basis,
            row.evidence_stage,
            row.deployment_id,
            row.strategy_profile,
            row.runtime_score,
            row.data_snapshot_id,
            row.dsl_hash,
            row.target,
            row.horizon,
            row.recording_path,
            row.recording_sha256,
            row.config_path,
            row.config_sha256,
            row.runner_source,
            row.runner_git_sha,
            canonical_json(row.decision_contract),
            canonical_json(row.acceptance_criteria),
            canonical_json(row.metrics),
            canonical_json(row.blocking_risk_flags),
            row.promotion_ready,
            row.promotion_decision,
        )
        await append_experiment_trace(conn, row)
    finally:
        await conn.close()


async def append_experiment_trace(conn: Any, row: CandidateReplayTapeRow) -> None:
    latest = await conn.fetchrow(
        """
        SELECT trace_id::text, hash_current
        FROM experiment_trace
        WHERE run_id = $1
        ORDER BY created_at DESC
        LIMIT 1
        """,
        row.run_id,
    )
    parent_trace_id = latest["trace_id"] if latest else None
    hash_prev = latest["hash_current"] if latest else None
    input_json = {
        "artifact_path": str(row.artifact_path),
        "artifact_sha256": row.artifact_sha256,
    }
    hash_current = trace_hash(
        hash_prev,
        row.run_id,
        EVENT_TYPE,
        WRITER_AGENT,
        input_json,
        row.artifact_json,
    )
    await conn.execute(
        """
        INSERT INTO experiment_trace (
            trace_id,
            run_id,
            parent_trace_id,
            event_type,
            data_snapshot_id,
            dsl_hash,
            candidate_replay_id,
            artifact_kind,
            evidence_stage,
            promotion_decision,
            agent_name,
            input_json,
            output_json,
            hash_prev,
            hash_current
        )
        VALUES (
            $1::uuid, $2, $3::uuid, $4, $5, $6, $7, $8, $9, $10, $11,
            $12::jsonb, $13::jsonb, $14, $15
        )
        """,
        str(uuid4()),
        row.run_id,
        parent_trace_id,
        EVENT_TYPE,
        row.data_snapshot_id,
        row.dsl_hash,
        row.candidate_replay_id,
        ARTIFACT_KIND,
        row.evidence_stage,
        row.promotion_decision,
        WRITER_AGENT,
        canonical_json(input_json),
        canonical_json(row.artifact_json),
        hash_prev,
        hash_current,
    )


def parser() -> argparse.ArgumentParser:
    parsed = argparse.ArgumentParser()
    parsed.add_argument("--db-url", default=os.environ.get("DATABASE_URL", ""))
    parsed.add_argument("--candidate-replay-json", required=True, type=Path)
    parsed.add_argument("--run-id", default="")
    parsed.add_argument("--source-workflow", default=DEFAULT_SOURCE_WORKFLOW)
    parsed.add_argument("--workflow-run-id", default=os.environ.get("GITHUB_RUN_ID", ""))
    parsed.add_argument("--workflow-run-url", default="")
    parsed.add_argument("--artifact-name", default="")
    parsed.add_argument("--data-snapshot-id", default="")
    parsed.add_argument("--dry-run", action="store_true")
    parsed.add_argument("--report-json", type=Path)
    return parsed


async def main_async() -> None:
    args = parser().parse_args()
    row = build_row(
        candidate_replay_json=args.candidate_replay_json,
        run_id=args.run_id or args.workflow_run_id or "manual-runtime-candidate-replay",
        source_workflow=args.source_workflow,
        workflow_run_id=args.workflow_run_id,
        workflow_run_url=args.workflow_run_url,
        artifact_name=args.artifact_name,
        data_snapshot_id=args.data_snapshot_id or None,
    )
    report = {
        "schema_version": "candidate_replay_tape_persist.v1",
        "dry_run": args.dry_run,
        "candidate_replay_id": row.candidate_replay_id,
        "basis": row.basis,
        "evidence_stage": row.evidence_stage,
        "promotion_ready": row.promotion_ready,
        "promotion_decision": row.promotion_decision,
        "runtime_score": row.runtime_score,
        "workflow_run_id": row.workflow_run_id,
        "artifact_name": row.artifact_name,
        "artifact_sha256": row.artifact_sha256,
        "blocking_risk_flags": row.blocking_risk_flags,
    }
    text = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.report_json:
        args.report_json.write_text(text, encoding="utf-8")
    if not args.dry_run:
        if not args.db_url:
            raise SystemExit("--db-url or DATABASE_URL is required unless --dry-run is set")
        await persist_row(args.db_url, row)
    print(text, end="")


def main() -> None:
    asyncio.run(main_async())


if __name__ == "__main__":
    main()
