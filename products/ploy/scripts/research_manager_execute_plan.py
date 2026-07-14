#!/usr/bin/env python3
"""Turn a Research Manager plan artifact into bounded research follow-up actions."""

from __future__ import annotations

import argparse
import json
import os
import re
import subprocess
import sys
import tempfile
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any


EXECUTE_ACK = "execute-research-manager-plan"
SCHEMA_VERSION = "research_manager_executor.v1"
ARCHIVED_RECORDING_SUFFIX_RE = re.compile(r"\.\d{8}T\d{6}Z?$")


def _looks_like_mutable_recording_path(raw_path: str) -> bool:
    if not raw_path:
        return False
    name = Path(raw_path).name
    if not name.endswith(".ndjson"):
        return False
    return ARCHIVED_RECORDING_SUFFIX_RE.search(name.removesuffix(".ndjson")) is None


def _load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def _write_json(path: Path, payload: dict[str, Any]) -> None:
    path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")


def _source_snapshot_run_id_from_text(raw: str) -> str:
    for line in raw.splitlines():
        key, sep, value = line.partition("=")
        if sep and key.strip() == "source_snapshot_run_id":
            return value.strip()
    return ""


def _source_snapshot_run_id_from_alpha_artifact(run_id: str, artifact_name: str) -> str:
    if not run_id or not artifact_name:
        return ""
    if not (os.environ.get("GITHUB_TOKEN") or os.environ.get("GH_TOKEN")):
        return ""
    if not os.environ.get("GITHUB_REPOSITORY"):
        return ""

    downloader = Path(__file__).resolve().with_name("download_github_artifact.py")
    with tempfile.TemporaryDirectory(prefix="ploy-alpha-provenance-") as tmp:
        out = Path(tmp)
        completed = subprocess.run(
            [
                sys.executable,
                str(downloader),
                "--run-id",
                run_id,
                "--name",
                artifact_name,
                "--output-dir",
                str(out),
                "--require",
                "snapshot-provenance/source.txt",
            ],
            text=True,
            capture_output=True,
        )
        if completed.returncode != 0:
            return ""
        source = out / "snapshot-provenance" / "source.txt"
        if not source.is_file():
            return ""
        return _source_snapshot_run_id_from_text(source.read_text(encoding="utf-8"))


def _date_from_ts(raw: str | None) -> str:
    if not raw:
        return "auto"
    return raw.split("T", 1)[0]


def _parse_utc_ts(raw: str | None) -> datetime | None:
    if not raw:
        return None
    parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def _format_utc_ts(ts: datetime) -> str:
    return ts.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def _bounded_snapshot_window(
    market_data: dict[str, Any],
    max_window_days: int,
) -> dict[str, Any]:
    start = _parse_utc_ts(market_data.get("dataset_start_ts"))
    end = _parse_utc_ts(market_data.get("dataset_end_ts"))
    if not start or not end or end <= start:
        return {
            "start_date": _date_from_ts(market_data.get("dataset_start_ts")),
            "end_date": _date_from_ts(market_data.get("dataset_end_ts")),
            "start_ts": market_data.get("dataset_start_ts") or "",
            "end_ts": market_data.get("dataset_end_ts") or "",
            "truncated": False,
            "max_window_days": max_window_days,
        }
    capped_end = min(end, start + timedelta(days=max_window_days))
    return {
        "start_date": start.date().isoformat(),
        "end_date": capped_end.date().isoformat(),
        "start_ts": _format_utc_ts(start),
        "end_ts": _format_utc_ts(capped_end),
        "truncated": capped_end < end,
        "max_window_days": max_window_days,
        "source_end_ts": _format_utc_ts(end),
    }


def _parse_symbols(raw: str) -> list[str]:
    return [item.strip() for item in raw.split(",") if item.strip()]


def _dispatch_gh_workflow(workflow: str, ref: str, fields: dict[str, str]) -> dict[str, Any]:
    cmd = ["gh", "workflow", "run", workflow, "--ref", ref]
    for key, value in fields.items():
        cmd.extend(["-f", f"{key}={value}"])
    completed = subprocess.run(cmd, text=True, capture_output=True)
    return {
        "workflow": workflow,
        "ref": ref,
        "ok": completed.returncode == 0,
        "returncode": completed.returncode,
        "stdout": completed.stdout.strip(),
        "stderr": completed.stderr.strip(),
    }


def _research_snapshot_dispatch(
    *,
    plan: dict[str, Any],
    git_ref: str,
    symbols: str,
    stake_usd: str,
    max_snapshot_window_days: int,
) -> dict[str, Any]:
    market_data = ((plan.get("input") or {}).get("market_data_health") or {})
    window = _bounded_snapshot_window(
        market_data,
        max_window_days=max(1, max_snapshot_window_days),
    )
    options = {
        "start_ts": window["start_ts"],
        "end_ts": window["end_ts"],
        "data_profile": "pm5d-execution",
        "data_gate": "warn",
        "upload_sampled_snapshot": True,
    }
    blockers: list[str] = []
    if not _parse_symbols(symbols):
        blockers.append("missing_symbols")
    fields = {
        "git_ref": git_ref,
        "start_date": window["start_date"],
        "end_date": window["end_date"],
        "symbols": symbols,
        "stake_usd": stake_usd,
        "options_json": json.dumps(options, separators=(",", ":"), sort_keys=True),
    }
    return {
        "workflow": "research-snapshot.yml",
        "reason": "refresh snapshot/data audit for Research Manager fix_data plan",
        "ready": not blockers,
        "blockers": blockers,
        "fields": fields,
        "bounded_window": window,
    }


def _bounded_execution_surface_window(
    market_data: dict[str, Any],
    max_hours: int,
) -> dict[str, Any]:
    start = _parse_utc_ts(market_data.get("dataset_start_ts"))
    end = _parse_utc_ts(market_data.get("dataset_end_ts"))
    if not start or not end or end <= start:
        max_hours = max(1, max_hours)
        return {
            "start_date": _date_from_ts(market_data.get("dataset_start_ts")),
            "end_date": _date_from_ts(market_data.get("dataset_end_ts")),
            "start_ts": market_data.get("dataset_start_ts") or "",
            "end_ts": market_data.get("dataset_end_ts") or "",
            "truncated": False,
            "max_hours": max_hours,
        }
    if max_hours <= 0:
        max_hours = max(1, int((end - start + timedelta(seconds=3599)).total_seconds() // 3600))
    capped_end = min(end, start + timedelta(hours=max_hours))
    return {
        "start_date": start.date().isoformat(),
        "end_date": capped_end.date().isoformat(),
        "start_ts": _format_utc_ts(start),
        "end_ts": _format_utc_ts(capped_end),
        "truncated": capped_end < end,
        "max_hours": max_hours,
        "source_end_ts": _format_utc_ts(end),
    }


def _full_depth_execution_surface_dispatch(
    *,
    plan: dict[str, Any],
    git_ref: str,
    max_hours: int,
) -> dict[str, Any]:
    market_data = ((plan.get("input") or {}).get("market_data_health") or {})
    window = _bounded_execution_surface_window(market_data, max_hours=max_hours)
    options = {
        "start_ts": window["start_ts"],
        "end_ts": window["end_ts"],
        "max_hours": window["max_hours"],
        "fail_if_incomplete": True,
    }
    blockers: list[str] = []
    if not window["start_ts"] or not window["end_ts"]:
        blockers.append("missing_dataset_window")
    fields = {
        "git_ref": git_ref,
        "start_date": window["start_date"],
        "end_date": window["end_date"],
        "options_json": json.dumps(options, separators=(",", ":"), sort_keys=True),
    }
    return {
        "workflow": "collect-full-depth-execution-surface.yml",
        "reason": "materialize full-fidelity Polymarket CLOB archive evidence for promotion surface",
        "ready": not blockers,
        "blockers": blockers,
        "fields": fields,
        "bounded_window": window,
    }


def _official_settlement_repair_dispatch(
    *,
    plan: dict[str, Any],
    git_ref: str,
    symbols: str,
    execute: bool,
) -> dict[str, Any]:
    market_data = ((plan.get("input") or {}).get("market_data_health") or {})
    window = _bounded_snapshot_window(market_data, max_window_days=2)
    options = {
        "start_ts": window["start_ts"],
        "end_ts": window["end_ts"],
        "mode": "execute" if execute else "dry_run",
    }
    blockers: list[str] = []
    if not window["start_ts"] or not window["end_ts"]:
        blockers.append("missing_dataset_window")
    if not _parse_symbols(symbols):
        blockers.append("missing_symbols")
    fields = {
        "git_ref": git_ref,
        "start_date": window["start_date"],
        "end_date": window["end_date"],
        "symbols": symbols,
        "options_json": json.dumps(options, separators=(",", ":"), sort_keys=True),
        "execute_ack": "repair-official-settlement-coverage" if execute else "",
    }
    return {
        "workflow": "repair-official-settlement-coverage.yml",
        "reason": "repair bounded official Polymarket settlement coverage for replay-traded events",
        "ready": not blockers,
        "blockers": blockers,
        "fields": fields,
        "bounded_window": window,
    }


def _walk_forward_dispatch(
    *,
    git_ref: str,
    snapshot_run_id: str,
    symbols: str,
    stake_usd: str,
    chain_remaining: int,
    alpha_search_plan_target: str = "",
    allowed_target: str = "",
    alpha_search_plan_run_id: str = "",
    alpha_search_plan_artifact_name: str = "",
    alpha_search_llm_prior: dict[str, Any] | None = None,
    candidate_strategy_replay_run_id: str = "",
    candidate_strategy_replay_artifact_name: str = "",
    full_depth_execution_surface_run_id: str = "",
    full_depth_execution_surface_artifact_name: str = "",
    extra_blockers: list[str] | None = None,
    snapshot_resolution: dict[str, str] | None = None,
) -> dict[str, Any]:
    blockers: list[str] = []
    if not snapshot_run_id:
        blockers.append("missing_snapshot_run_id")
    if not _parse_symbols(symbols):
        blockers.append("missing_symbols")
    if extra_blockers:
        blockers.extend(extra_blockers)
    options = {
        "chain_next_run": chain_remaining > 0,
        "chain_remaining": max(chain_remaining - 1, 0),
        "create_config_pr": False,
        "create_handoff_issue": False,
        "fail_if_blocked": False,
        "persist_research_trace": True,
    }
    if alpha_search_plan_target:
        options["alpha_search_plan_target"] = alpha_search_plan_target
    if allowed_target:
        options["allowed_target"] = allowed_target
    if alpha_search_plan_run_id:
        options["alpha_search_plan_run_id"] = alpha_search_plan_run_id
    if alpha_search_plan_artifact_name:
        options["alpha_search_plan_artifact_name"] = alpha_search_plan_artifact_name
    if alpha_search_llm_prior and not alpha_search_plan_run_id:
        options["alpha_search_llm_prior_json"] = json.dumps(
            alpha_search_llm_prior,
            separators=(",", ":"),
            sort_keys=True,
        )
    if candidate_strategy_replay_run_id:
        options["candidate_strategy_replay_run_id"] = candidate_strategy_replay_run_id
    if candidate_strategy_replay_artifact_name:
        options["candidate_strategy_replay_artifact_name"] = candidate_strategy_replay_artifact_name
    if full_depth_execution_surface_run_id:
        options["full_depth_execution_surface_run_id"] = full_depth_execution_surface_run_id
    if full_depth_execution_surface_artifact_name:
        options["full_depth_execution_surface_artifact_name"] = full_depth_execution_surface_artifact_name
    fields = {
        "git_ref": git_ref,
        "snapshot_run_id": snapshot_run_id,
        "start_date": "auto",
        "end_date": "auto",
        "symbols": symbols,
        "stake_usd": stake_usd,
        "options_json": json.dumps(options, separators=(",", ":"), sort_keys=True),
    }
    dispatch = {
        "workflow": "factor-walk-forward-v2-hosted-artifact.yml",
        "reason": "bounded hosted walk-forward continuation from Research Manager plan",
        "ready": not blockers,
        "blockers": blockers,
        "fields": fields,
    }
    if snapshot_resolution:
        dispatch["snapshot_resolution"] = snapshot_resolution
    return dispatch


def _runtime_candidate_replay_dispatch(
    *,
    deployment_id: str,
    config_path: str,
    recording_path: str,
    runtime_score: str,
    strategy_profile: str,
    issue_number: str,
    min_trade_count: str,
    min_fill_rate: str,
    min_roi: str,
    source_target: str,
    source_horizon: str,
) -> dict[str, Any]:
    blockers: list[str] = []
    if not runtime_score:
        blockers.append("missing_runtime_score")
    if not deployment_id:
        blockers.append("missing_deployment_id")
    if not config_path:
        blockers.append("missing_config_path")
    if not recording_path:
        blockers.append("missing_recording_path")
    if not strategy_profile:
        blockers.append("missing_strategy_profile")

    options = {
        "full_depth_entry": True,
        "skip_settlement_exits": False,
        "source_target": source_target,
        "source_horizon": source_horizon,
    }
    fields = {
        "deployment_id": deployment_id,
        "config_path": config_path,
        "recording_path": recording_path,
        "runtime_score": runtime_score,
        "strategy_profile": strategy_profile,
        "issue_number": issue_number,
        "min_trade_count": min_trade_count,
        "min_fill_rate": min_fill_rate,
        "min_roi": min_roi,
        "options_json": json.dumps(options, separators=(",", ":"), sort_keys=True),
    }
    return {
        "workflow": "runtime-candidate-replay.yml",
        "reason": "build runtime_market_update_replay evidence for Research Manager fix_runtime plan",
        "ready": not blockers,
        "blockers": blockers,
        "fields": fields,
    }


def _recorded_replay_parity_dispatch(
    *,
    deployment_id: str,
    config_path: str,
    recording_path: str,
    issue_number: str,
) -> dict[str, Any]:
    blockers: list[str] = []
    if not deployment_id:
        blockers.append("missing_deployment_id")
    if not config_path:
        blockers.append("missing_config_path")
    if not recording_path:
        blockers.append("missing_recording_path")

    fields = {
        "deployment_id": deployment_id,
        "config_path": config_path,
        "recording_path": recording_path,
        "since": "auto",
        "until": "auto",
        "issue_number": issue_number,
        "approval_environment": "tango-1-1-build-only",
        "skip_settlement_exits": "false",
        "runner_source": "deployed",
    }
    return {
        "workflow": "recorded-replay-parity.yml",
        "reason": "compare deployed runtime scorer decisions against recorded dry-run evidence",
        "ready": not blockers,
        "blockers": blockers,
        "fields": fields,
    }


def _ready_handoff_from_plan(plan_payload: dict[str, Any]) -> dict[str, str]:
    latest_runs = ((plan_payload.get("input") or {}).get("latest_runs") or {})
    ready_handoffs = latest_runs.get("ready_handoffs")
    if isinstance(ready_handoffs, list):
        for item in ready_handoffs:
            if not isinstance(item, dict):
                continue
            output = item.get("output_json")
            if not isinstance(output, dict):
                continue
            ready_handoff = _ready_handoff_fields(
                run_id=str(item.get("run_id") or item.get("workflow_run_id") or ""),
                output=output,
            )
            if ready_handoff:
                return ready_handoff

    runs = latest_runs.get("runs")
    if not isinstance(runs, list):
        return {}

    for run in runs:
        if not isinstance(run, dict):
            continue
        run_id = str(run.get("run_id") or run.get("workflow_run_id") or "")
        if not run_id:
            continue
        artifacts = run.get("artifacts")
        if not isinstance(artifacts, list):
            continue
        for artifact in artifacts:
            if not isinstance(artifact, dict):
                continue
            output = artifact.get("output_json")
            if not isinstance(output, dict):
                continue
            ready_handoff = _ready_handoff_fields(run_id=run_id, output=output)
            if ready_handoff:
                return ready_handoff
    return {}


def _ready_handoff_fields(*, run_id: str, output: dict[str, Any]) -> dict[str, str]:
    if output.get("kind") != "autofactor_strategy_handoff":
        return {}
    if output.get("status") != "ready":
        return {}
    if output.get("recommended_action") != "create_dry_run_handoff":
        return {}
    if not run_id:
        return {}
    strategies = output.get("strategies")
    strategy = strategies[0] if isinstance(strategies, list) and strategies else {}
    if not isinstance(strategy, dict):
        strategy = {}
    replay = output.get("candidate_strategy_replay")
    if not isinstance(replay, dict):
        replay = {}
    contract = replay.get("decision_contract")
    if not isinstance(contract, dict):
        contract = {}

    return {
        "factor_walk_forward_run_id": run_id,
        "artifact_name": f"factor-walk-forward-v2-{run_id}",
        "required_strategy_profile": str(
            strategy.get("strategy_profile")
            or replay.get("strategy_profile")
            or "settlement_probability"
        ),
        "allowed_target": str(
            strategy.get("target")
            or contract.get("target")
            or "full_depth_settlement_executable_pnl"
        ),
        "runtime_score": str(
            strategy.get("runtime_score") or replay.get("runtime_score") or ""
        ),
        "strategy_config": (
            "config/strategies/"
            "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml"
        ),
    }


def _autofactor_promotion_handoff_dispatch(
    *,
    plan_payload: dict[str, Any],
    git_ref: str,
    create_handoff_issue: bool,
    create_config_pr: bool,
    issue_number: str,
) -> dict[str, Any]:
    ready_handoff = _ready_handoff_from_plan(plan_payload)
    blockers: list[str] = []
    if not ready_handoff:
        blockers.append("missing_ready_autofactor_handoff")
    if not create_handoff_issue and not create_config_pr:
        blockers.append("missing_ready_handoff_side_effect")

    fields = {
        "git_ref": git_ref,
        "factor_walk_forward_run_id": ready_handoff.get("factor_walk_forward_run_id", ""),
        "artifact_name": ready_handoff.get("artifact_name", ""),
        "required_strategy_profile": ready_handoff.get(
            "required_strategy_profile",
            "settlement_probability",
        ),
        "allowed_target": ready_handoff.get(
            "allowed_target",
            "full_depth_settlement_executable_pnl",
        ),
        "issue_number": issue_number,
        "create_handoff_issue": "true" if create_handoff_issue else "false",
        "create_config_pr": "true" if create_config_pr else "false",
        "strategy_config": ready_handoff.get(
            "strategy_config",
            "config/strategies/"
            "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
        ),
        "fail_if_blocked": "true",
    }
    return {
        "workflow": "autofactor-strategy-promotion.yml",
        "reason": "execute ready AutoFactor handoff through durable trace-gated promotion workflow",
        "ready": not blockers,
        "blockers": blockers,
        "fields": fields,
        "runtime_score": ready_handoff.get("runtime_score", ""),
    }


def _string_list(value: Any) -> list[str]:
    if not isinstance(value, list):
        return []
    return [str(item) for item in value if str(item)]


def _runtime_candidate_from_plan(
    plan_payload: dict[str, Any],
    *,
    strategy_profile: str,
    source_target: str,
    source_horizon: str,
) -> dict[str, str] | None:
    """Select the newest typed, unblocked runtime contract from the trace plan."""

    summary = ((plan_payload.get("input") or {}).get("factor_registry_summary") or {})
    candidates: list[Any] = []
    runtime_ready = summary.get("runtime_ready_candidates")
    if isinstance(runtime_ready, list):
        candidates.extend(runtime_ready)
    recent = summary.get("recent_factors")
    if isinstance(recent, list):
        candidates.extend(recent)
    if not candidates:
        return None
    for item in candidates:
        if not isinstance(item, dict):
            continue
        status = str(item.get("status") or "")
        if status and status not in {"candidate", "watchlist"}:
            continue
        contract = item.get("runtime_contract")
        if not isinstance(contract, dict):
            continue
        blockers = set(_string_list(item.get("blockers"))) | set(
            _string_list(contract.get("blockers"))
        )
        if blockers:
            continue
        if contract.get("version") != "autofactor_runtime_contract_v1":
            continue
        runtime_score = str(contract.get("runtime_score") or "")
        contract_profile = str(contract.get("strategy_profile") or "")
        contract_target = str(contract.get("target") or item.get("target") or "")
        contract_horizon = str(contract.get("horizon") or item.get("horizon") or "")
        if not runtime_score or not contract_profile:
            continue
        if strategy_profile and contract_profile != strategy_profile:
            continue
        if source_target and contract_target and contract_target != source_target:
            continue
        if source_horizon and contract_horizon and contract_horizon != source_horizon:
            continue
        return {
            "factor_name": str(item.get("factor_name") or item.get("name") or ""),
            "runtime_score": runtime_score,
            "strategy_profile": contract_profile,
            "source_target": contract_target or source_target,
            "source_horizon": contract_horizon or source_horizon,
        }
    return None


def _latest_run(plan_payload: dict[str, Any]) -> dict[str, Any]:
    latest_runs = ((plan_payload.get("input") or {}).get("latest_runs") or {})
    runs = latest_runs.get("runs")
    if isinstance(runs, list) and runs and isinstance(runs[0], dict):
        return runs[0]
    return {}


def _latest_candidate_replay_contract(plan_payload: dict[str, Any]) -> dict[str, Any]:
    summary = ((plan_payload.get("input") or {}).get("factor_registry_summary") or {})
    for replay_key in ("recent_candidate_replays", "ready_candidate_replays"):
        replays = summary.get(replay_key)
        if not isinstance(replays, list):
            continue
        for item in replays:
            if not isinstance(item, dict):
                continue
            artifact = item.get("artifact_json")
            replay = artifact if isinstance(artifact, dict) else item
            contract = _candidate_replay_contract(replay, fallback=item)
            if contract:
                return contract

    latest_run = _latest_run(plan_payload)
    artifacts = latest_run.get("artifacts")
    if not isinstance(artifacts, list):
        return {}
    for artifact in artifacts:
        if not isinstance(artifact, dict):
            continue
        output = artifact.get("output_json")
        if not isinstance(output, dict):
            continue
        replay = output.get("candidate_strategy_replay")
        if not isinstance(replay, dict):
            continue
        contract = _candidate_replay_contract(replay, fallback={})
        if contract:
            return contract
    return {}


def _candidate_replay_contract(
    replay: dict[str, Any], *, fallback: dict[str, Any]
) -> dict[str, Any]:
    if replay.get("basis") != "runtime_market_update_replay":
        return {}
    contract = replay.get("decision_contract")
    if not isinstance(contract, dict):
        contract = {}
    identity = replay.get("identity")
    if not isinstance(identity, dict):
        identity = {}
    recording_path = str(replay.get("recording_path") or identity.get("recording_path") or "")
    recording_sha256 = str(
        replay.get("recording_sha256") or identity.get("recording_sha256") or ""
    )
    if _looks_like_mutable_recording_path(recording_path) and not recording_sha256:
        return {}
    source_factor = replay.get("source_factor")
    if not isinstance(source_factor, dict):
        source_factor = {}
    metrics = replay.get("metrics") if isinstance(replay.get("metrics"), dict) else {}
    if not metrics and isinstance(fallback.get("metrics"), dict):
        metrics = fallback["metrics"]
    blocking_flags = replay.get("blocking_risk_flags")
    if not isinstance(blocking_flags, list):
        blocking_flags = replay.get("blockers")
    if not isinstance(blocking_flags, list):
        blocking_flags = fallback.get("blocking_risk_flags")
    if not isinstance(blocking_flags, list):
        blocking_flags = []
    return {
        "target": str(
            contract.get("target")
            or source_factor.get("target")
            or fallback.get("target")
            or ""
        ),
        "horizon": str(
            contract.get("horizon")
            or source_factor.get("horizon")
            or fallback.get("horizon")
            or ""
        ),
        "runtime_score": str(
            replay.get("runtime_score") or fallback.get("runtime_score") or ""
        ),
        "source_run_id": str(
            replay.get("run_id") or fallback.get("run_id") or ""
        ),
        "strategy_profile": str(
            replay.get("strategy_profile") or fallback.get("strategy_profile") or ""
        ),
        "workflow_run_id": str(
            replay.get("workflow_run_id") or fallback.get("workflow_run_id") or ""
        ),
        "source_workflow": str(
            replay.get("source_workflow") or fallback.get("source_workflow") or ""
        ),
        "recording_path": recording_path,
        "recording_sha256": recording_sha256,
        "source_factor_name": str(
            source_factor.get("name") or source_factor.get("factor_name") or ""
        ),
        "metrics": {
            key: metrics[key]
            for key in (
                "trade_count",
                "unique_event_count",
                "entry_fill_rate",
                "roi",
                "total_pnl",
                "avg_entry_price",
            )
            if key in metrics
        },
        "blocking_risk_flags": [str(item) for item in blocking_flags if str(item)],
    }


def _alpha_search_plan_run_id_from_replay_contract(replay_contract: dict[str, Any]) -> str:
    source_run_id = str(replay_contract.get("source_run_id") or "")
    workflow_run_id = str(replay_contract.get("workflow_run_id") or "")
    if not source_run_id:
        return ""
    if workflow_run_id and source_run_id == workflow_run_id:
        return ""
    return source_run_id


def _latest_valid_full_depth_surface_artifact(plan_payload: dict[str, Any]) -> dict[str, str]:
    latest_run = _latest_run(plan_payload)
    latest_run_id = str(latest_run.get("run_id") or latest_run.get("workflow_run_id") or "")
    if not latest_run_id:
        return {}
    artifacts = latest_run.get("artifacts")
    if not isinstance(artifacts, list):
        return {}
    for artifact in artifacts:
        if not isinstance(artifact, dict):
            continue
        output = artifact.get("output_json")
        if not isinstance(output, dict):
            continue
        snapshot_contract = output.get("source_snapshot_contract")
        if not isinstance(snapshot_contract, dict):
            continue
        proofs = snapshot_contract.get("full_depth_execution_surface_proofs")
        if not isinstance(proofs, list):
            continue
        for proof in proofs:
            if not isinstance(proof, dict):
                continue
            if proof.get("valid") is not True:
                continue
            if proof.get("surface") != "clob_orderbook_snapshots":
                continue
            blockers = proof.get("blockers")
            if isinstance(blockers, list) and blockers:
                continue
            return {
                "run_id": latest_run_id,
                "artifact_name": f"factor-walk-forward-v2-{latest_run_id}",
            }
    return {}


def _runtime_replay_args(args: argparse.Namespace, plan_payload: dict[str, Any]) -> dict[str, Any]:
    selected = _runtime_candidate_from_plan(
        plan_payload,
        strategy_profile=args.runtime_strategy_profile,
        source_target=args.runtime_source_target,
        source_horizon=args.runtime_source_horizon,
    )
    return {
        "runtime_score": args.runtime_score or (selected or {}).get("runtime_score", ""),
        "strategy_profile": args.runtime_strategy_profile
        or (selected or {}).get("strategy_profile", ""),
        "source_target": (selected or {}).get("source_target") or args.runtime_source_target,
        "source_horizon": (selected or {}).get("source_horizon") or args.runtime_source_horizon,
        "selected_factor_name": (selected or {}).get("factor_name", ""),
        "extra_blockers": [] if args.runtime_score or selected else ["missing_runtime_candidate_contract"],
    }


def _blocker_actions(plan: dict[str, Any]) -> list[dict[str, str]]:
    raw = plan.get("blocker_actions")
    if not isinstance(raw, list):
        return []
    actions: list[dict[str, str]] = []
    for item in raw:
        if not isinstance(item, dict):
            continue
        blocker_family = str(item.get("blocker_family") or "")
        action = str(item.get("action") or "")
        reason = str(item.get("reason") or "")
        if blocker_family and action:
            actions.append(
                {
                    "blocker_family": blocker_family,
                    "action": action,
                    "reason": reason,
                }
            )
    return actions


def _typed_prior_constraints(blocker_actions: list[dict[str, str]]) -> list[str]:
    constraints = [
        "reuse only runtime-contract-mappable inputs",
        "do not promote without runtime_market_update_replay evidence",
        "preserve one-event-one-decision accounting",
    ]
    action_names = {item["action"] for item in blocker_actions}
    if "increase_distinct_event_coverage_or_reduce_selectivity" in action_names:
        constraints.append("prefer candidates with broader distinct-event coverage and avoid ultra-narrow buckets")
    if "prefer_high_fillability_depth_filters" in action_names:
        constraints.append("prefer candidates with stronger full-depth fillability and capacity filters")
    if "collect_full_depth_execution_surface" in action_names:
        constraints.append("block promotion until full-depth execution-surface evidence replaces sampled snapshots")
    if "repair_official_settlement_coverage" in action_names:
        constraints.append("block promotion until official settlement coverage exists for all replay-traded events")
    if "repair_runtime_contract_mapping" in action_names:
        constraints.append("only select factors with typed unblocked runtime contracts")
    if "mutate_or_reject_negative_runtime_edge" in action_names:
        constraints.append(
            "penalize losing runtime-replayed factor families and require positive executable ROI before handoff"
        )
    return constraints


def _runtime_score_base_factor(runtime_score: str) -> str:
    prefix = "autofactor_formula:"
    if runtime_score.startswith(prefix):
        return runtime_score[len(prefix) :]
    return runtime_score


def _normalized_factor_key(raw: str) -> str:
    value = _runtime_score_base_factor(str(raw or "").strip())
    while True:
        next_value = value
        for prefix in ("mut2_", "llm_", "mcts_", "mut_"):
            if next_value.startswith(prefix):
                next_value = next_value[len(prefix) :]
                break
        if next_value == value:
            break
        value = next_value
    marker = "_runtime_pass_through_"
    if marker in value:
        value = value.split(marker, 1)[0]
    return value


def _normalized_factor_family(raw: str) -> str:
    value = _normalized_factor_key(raw)
    suffixes = (
        "_select_entry_price_quality_ge_075",
        "_select_entry_price_quality_ge_050",
        "_select_entry_price_quality_ge_025",
        "_select_full_depth_entry_ge_075",
        "_select_full_depth_entry_ge_050",
        "_select_full_depth_entry_ge_025",
        "_select_near_strike_ge_075",
        "_select_near_strike_ge_050",
        "_select_near_strike_ge_025",
        "_runtime_pass_through_add_spread_penalty",
        "_runtime_pass_through_add_capacity_gate",
        "_add_spread_penalty",
        "_add_capacity_gate",
        "_add_feature_gate",
        "_entry_price_quality",
        "_full_depth_entry_gate",
        "_spread_adjusted",
        "_near_strike",
        "_capacity",
        "_squashed",
        "_pm_lag",
        "_clip",
    )
    changed = True
    while changed:
        changed = False
        for suffix in suffixes:
            if value.endswith(suffix) and len(value) > len(suffix):
                value = value[: -len(suffix)]
                changed = True
                break
        if not changed and value.endswith("_x") and len(value) > 2:
            value = value[:-2]
            changed = True
    return value


def _negative_economics_prior_payload(
    blocker_actions: list[dict[str, str]],
    replay_contract: dict[str, Any],
) -> dict[str, list[dict[str, Any]]]:
    action_names = {item["action"] for item in blocker_actions}
    if "mutate_or_reject_negative_runtime_edge" not in action_names:
        return {"runtime_avoid_factors": [], "mutations": []}

    runtime_score = str(replay_contract.get("runtime_score") or "")
    base_factor = str(
        replay_contract.get("source_factor_name") or ""
    ) or _runtime_score_base_factor(runtime_score)
    factor_family = _normalized_factor_family(base_factor)
    metrics = (
        replay_contract.get("metrics") if isinstance(replay_contract.get("metrics"), dict) else {}
    )
    flags = replay_contract.get("blocking_risk_flags")
    if not isinstance(flags, list):
        flags = []

    runtime_avoid_factors: list[dict[str, Any]] = []
    if base_factor and factor_family:
        runtime_avoid_factors.append(
            {
                "base_factor": base_factor,
                "factor_family": factor_family,
                "runtime_score": runtime_score,
                "reason": "negative_runtime_edge",
                "metrics": {
                    **metrics,
                    "blocking_risk_flags": flags,
                },
            }
        )

    fallback_specs = [
        {
            "base_factor": "auto_settlement_model_full_depth_settlement_edge",
            "mutation_type": "add_capacity_gate",
            "name": "llm_model_full_depth_edge_full_depth_gate",
            "feature": "full_depth_entry_fillable_gate",
        },
        {
            "base_factor": "auto_settlement_model_full_depth_settlement_edge",
            "mutation_type": "add_spread_penalty",
            "name": "llm_model_full_depth_edge_spread_penalty",
            "feature": "side_spread",
            "constant": 0.01,
        },
        {
            "base_factor": "auto_settlement_full_depth_settlement_edge",
            "mutation_type": "add_near_strike_interaction",
            "name": "llm_full_depth_edge_near_strike",
        },
        {
            "base_factor": "auto_settlement_conservative_settlement_edge",
            "mutation_type": "invert_or_contrarian",
            "name": "llm_conservative_settlement_edge_contrarian",
        },
    ]
    avoided_families = {item["factor_family"] for item in runtime_avoid_factors}
    mutations = [
        item
        for item in fallback_specs
        if _normalized_factor_family(item["base_factor"]) not in avoided_families
    ]
    if not mutations:
        mutations = fallback_specs[:1]
    for item in mutations:
        item["feedback_reason"] = "negative_runtime_edge"
        item["runtime_metrics"] = metrics

    return {
        "runtime_avoid_factors": runtime_avoid_factors,
        "mutations": mutations,
    }


def build_executor_payload(args: argparse.Namespace, plan_payload: dict[str, Any]) -> dict[str, Any]:
    if plan_payload.get("schema_version") != "research_trace_plan.v1":
        raise SystemExit("research_trace_plan schema mismatch")
    plan = plan_payload.get("plan") or {}
    actions = [str(action) for action in plan.get("actions") or []]
    blocker_actions = _blocker_actions(plan)
    dispatches: list[dict[str, Any]] = []
    typed_prior: dict[str, Any] | None = None
    side_effect_mode = args.mode == "execute" and args.execute_ack == EXECUTE_ACK

    action_names = {item["action"] for item in blocker_actions}
    snapshot_actions = {"rerun_snapshot_data_audit", "repair_or_exclude_missing_data_surface"}
    latest_replay_contract = _latest_candidate_replay_contract(plan_payload)
    latest_full_depth_surface_artifact = _latest_valid_full_depth_surface_artifact(plan_payload)
    walk_forward_target = latest_replay_contract.get("target") or getattr(
        args,
        "runtime_source_target",
        "full_depth_settlement_executable_pnl",
    )
    alpha_search_plan_run_id = ""
    if plan.get("theme") == "revise_prior":
        alpha_search_plan_run_id = _alpha_search_plan_run_id_from_replay_contract(
            latest_replay_contract
        )
    alpha_search_plan_artifact_name = (
        f"factor-walk-forward-v2-{alpha_search_plan_run_id}"
        if alpha_search_plan_run_id
        else ""
    )
    walk_forward_snapshot_run_id = args.snapshot_run_id
    walk_forward_extra_blockers: list[str] = []
    walk_forward_snapshot_resolution: dict[str, str] = {}
    if alpha_search_plan_run_id and getattr(args, "resolve_snapshot_provenance", False):
        resolved_source_snapshot_run_id = _source_snapshot_run_id_from_alpha_artifact(
            alpha_search_plan_run_id,
            alpha_search_plan_artifact_name,
        )
        if resolved_source_snapshot_run_id:
            walk_forward_snapshot_resolution = {
                "source": "alpha_search_plan_artifact_snapshot_provenance",
                "alpha_search_plan_run_id": alpha_search_plan_run_id,
                "alpha_search_plan_artifact_name": alpha_search_plan_artifact_name,
                "source_snapshot_run_id": resolved_source_snapshot_run_id,
            }
            if (
                not walk_forward_snapshot_run_id
                or walk_forward_snapshot_run_id == alpha_search_plan_run_id
            ):
                walk_forward_snapshot_run_id = resolved_source_snapshot_run_id
                walk_forward_snapshot_resolution["status"] = "applied"
            else:
                walk_forward_snapshot_resolution["status"] = "provided_snapshot_run_id_kept"
                walk_forward_snapshot_resolution["provided_snapshot_run_id"] = (
                    walk_forward_snapshot_run_id
                )
        elif walk_forward_snapshot_run_id == alpha_search_plan_run_id:
            walk_forward_extra_blockers.append(
                "snapshot_run_id_points_to_alpha_search_plan_without_source_snapshot_provenance"
            )
            walk_forward_snapshot_resolution = {
                "source": "alpha_search_plan_artifact_snapshot_provenance",
                "alpha_search_plan_run_id": alpha_search_plan_run_id,
                "alpha_search_plan_artifact_name": alpha_search_plan_artifact_name,
                "status": "unresolved",
            }
    negative_economics_prior = _negative_economics_prior_payload(
        blocker_actions,
        latest_replay_contract,
    )

    if (
        plan.get("theme") == "revise_prior"
        or "generate_typed_llm_prior_json" in actions
        or blocker_actions
    ):
        typed_prior = {
            "schema_version": "research_manager_typed_prior.v1",
            "source": "research_trace_plan",
            "evidence_stage": plan.get("evidence_stage", ""),
            "theme": plan.get("theme", ""),
            "actions": actions,
            "blocker_actions": blocker_actions,
            "constraints": _typed_prior_constraints(blocker_actions),
            "runtime_avoid_factors": negative_economics_prior["runtime_avoid_factors"],
            "mutations": negative_economics_prior["mutations"],
        }

    if any(action in snapshot_actions for action in actions) or (
        plan.get("theme") == "fix_data" and not actions
    ):
        dispatches.append(
            _research_snapshot_dispatch(
                plan=plan_payload,
                git_ref=args.git_ref,
                symbols=args.symbols,
                stake_usd=args.stake_usd,
                max_snapshot_window_days=args.max_snapshot_window_days,
            )
        )

    if "collect_full_depth_execution_surface" in actions or (
        "collect_full_depth_execution_surface" in action_names
    ):
        dispatches.append(
            _full_depth_execution_surface_dispatch(
                plan=plan_payload,
                git_ref=args.git_ref,
                max_hours=getattr(args, "max_full_depth_surface_hours", 0),
            )
        )

    if "repair_official_settlement_coverage" in actions or (
        "repair_official_settlement_coverage" in action_names
    ):
        dispatches.append(
            _official_settlement_repair_dispatch(
                plan=plan_payload,
                git_ref=args.git_ref,
                symbols=args.symbols,
                execute=side_effect_mode,
            )
        )

    if any(
        action in {"continue_hosted_alpha_search", "rerun_alpha_search_with_bounded_mutations"}
        for action in actions
    ):
        dispatches.append(
            _walk_forward_dispatch(
                git_ref=args.git_ref,
                snapshot_run_id=walk_forward_snapshot_run_id,
                symbols=args.symbols,
                stake_usd=args.stake_usd,
                chain_remaining=args.chain_remaining,
                alpha_search_plan_target=walk_forward_target,
                allowed_target=walk_forward_target,
                alpha_search_plan_run_id=alpha_search_plan_run_id,
                alpha_search_plan_artifact_name=alpha_search_plan_artifact_name,
                alpha_search_llm_prior=typed_prior,
                candidate_strategy_replay_run_id=getattr(
                    args,
                    "candidate_strategy_replay_run_id",
                    "",
                )
                or latest_replay_contract.get("workflow_run_id", ""),
                candidate_strategy_replay_artifact_name=getattr(
                    args,
                    "candidate_strategy_replay_artifact_name",
                    "",
                )
                or (
                    f"runtime-candidate-replay-{latest_replay_contract['workflow_run_id']}"
                    if latest_replay_contract.get("workflow_run_id")
                    and latest_replay_contract.get("source_workflow")
                    == "runtime-candidate-replay.yml"
                    else ""
                ),
                full_depth_execution_surface_run_id=getattr(
                    args,
                    "full_depth_execution_surface_run_id",
                    "",
                )
                or latest_full_depth_surface_artifact.get("run_id", ""),
                full_depth_execution_surface_artifact_name=getattr(
                    args,
                    "full_depth_execution_surface_artifact_name",
                    "",
                )
                or latest_full_depth_surface_artifact.get("artifact_name", ""),
                extra_blockers=walk_forward_extra_blockers,
                snapshot_resolution=walk_forward_snapshot_resolution,
            )
        )

    runtime_replay_actions = {
        "compare_runtime_scorer_contract",
        "build_runtime_candidate_replay",
        "build_runtime_market_update_replay",
    }
    if any(action in runtime_replay_actions for action in actions) or (
        action_names & runtime_replay_actions
    ):
        replay_args = _runtime_replay_args(args, plan_payload)
        dispatches.append(
            _runtime_candidate_replay_dispatch(
                deployment_id=args.runtime_deployment_id,
                config_path=args.runtime_config_path,
                recording_path=args.runtime_recording_path,
                runtime_score=replay_args["runtime_score"],
                strategy_profile=replay_args["strategy_profile"],
                issue_number=getattr(args, "runtime_issue_number", ""),
                min_trade_count=args.runtime_min_trade_count,
                min_fill_rate=args.runtime_min_fill_rate,
                min_roi=args.runtime_min_roi,
                source_target=replay_args["source_target"],
                source_horizon=replay_args["source_horizon"],
            )
        )
        if replay_args["extra_blockers"]:
            dispatches[-1]["blockers"].extend(replay_args["extra_blockers"])
            dispatches[-1]["blockers"] = sorted(set(dispatches[-1]["blockers"]))
            dispatches[-1]["ready"] = False
        if replay_args["selected_factor_name"]:
            dispatches[-1]["selected_factor_name"] = replay_args["selected_factor_name"]

    if "run_recorded_replay_parity" in actions:
        dispatches.append(
            _recorded_replay_parity_dispatch(
                deployment_id=args.runtime_deployment_id,
                config_path=args.runtime_config_path,
                recording_path=args.runtime_recording_path,
                issue_number=getattr(args, "runtime_issue_number", ""),
            )
        )

    if plan.get("theme") == "ready_handoff" or any(
        action in {"create_dry_run_handoff_issue", "open_config_pr_from_ready_handoff"}
        for action in actions
    ):
        dispatches.append(
            _autofactor_promotion_handoff_dispatch(
                plan_payload=plan_payload,
                git_ref=args.git_ref,
                create_handoff_issue="create_dry_run_handoff_issue" in actions,
                create_config_pr="open_config_pr_from_ready_handoff" in actions,
                issue_number=getattr(args, "runtime_issue_number", ""),
            )
        )

    executable_dispatches = [item for item in dispatches if item["ready"]]
    blocked_dispatches = [item for item in dispatches if not item["ready"]]
    mode = "execute" if side_effect_mode else "dry_run"
    if args.mode == "execute" and args.execute_ack != EXECUTE_ACK:
        blocked_dispatches.append(
            {
                "workflow": "<all>",
                "reason": "execute mode requested without ACK",
                "ready": False,
                "blockers": ["missing_execute_ack"],
                "fields": {},
            }
        )

    return {
        "schema_version": SCHEMA_VERSION,
        "mode": mode,
        "source_plan": {
            "schema_version": plan_payload.get("schema_version"),
            "theme": plan.get("theme", ""),
            "priority": plan.get("priority", ""),
            "evidence_stage": plan.get("evidence_stage", ""),
            "actions": actions,
            "blocker_actions": blocker_actions,
        },
        "dispatches": dispatches,
        "executable_dispatch_count": len(executable_dispatches),
        "blocked_dispatches": blocked_dispatches,
        "typed_prior": typed_prior,
        "side_effects_enabled": mode == "execute",
        "dispatch_attempts": [],
    }


def render_markdown(payload: dict[str, Any]) -> str:
    lines = [
        "# Research Manager Executor",
        "",
        f"- Mode: `{payload['mode']}`",
        f"- Source theme: `{payload['source_plan']['theme']}`",
        f"- Source evidence stage: `{payload['source_plan']['evidence_stage']}`",
        f"- Executable dispatches: `{payload['executable_dispatch_count']}`",
        "",
        "## Dispatch Plan",
        "",
    ]
    if not payload["dispatches"]:
        lines.append("- `<none>`")
    for item in payload["dispatches"]:
        status = "ready" if item["ready"] else "blocked"
        blockers = ", ".join(item["blockers"]) if item["blockers"] else "<none>"
        selected = item.get("selected_factor_name")
        suffix = f"; selected factor: `{selected}`" if selected else ""
        lines.append(f"- `{item['workflow']}`: `{status}`; blockers: `{blockers}`{suffix}")
    if payload.get("typed_prior"):
        lines.extend(["", "## Typed Prior", "", "- `research_manager_typed_prior.v1` generated"])
        blocker_actions = payload["typed_prior"].get("blocker_actions") or []
        if blocker_actions:
            lines.extend(["", "### Blocker Actions", ""])
            for item in blocker_actions:
                lines.append(f"- `{item['blocker_family']}` -> `{item['action']}`")
    if payload.get("dispatch_attempts"):
        lines.extend(["", "## Dispatch Attempts", ""])
        for item in payload["dispatch_attempts"]:
            status = "ok" if item.get("ok") else "failed"
            lines.append(f"- `{item['workflow']}`: `{status}`")
    return "\n".join(lines) + "\n"


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--plan-json", required=True, type=Path)
    parser.add_argument("--output-dir", required=True, type=Path)
    parser.add_argument("--mode", choices=["dry_run", "execute"], default="dry_run")
    parser.add_argument("--execute-ack", default="")
    parser.add_argument("--git-ref", default="main")
    parser.add_argument("--snapshot-run-id", default="")
    parser.add_argument("--symbols", default="")
    parser.add_argument("--stake-usd", default="15")
    parser.add_argument("--chain-remaining", type=int, default=1)
    parser.add_argument("--candidate-strategy-replay-run-id", default="")
    parser.add_argument("--candidate-strategy-replay-artifact-name", default="")
    parser.add_argument("--full-depth-execution-surface-run-id", default="")
    parser.add_argument("--full-depth-execution-surface-artifact-name", default="")
    parser.add_argument("--max-snapshot-window-days", type=int, default=2)
    parser.add_argument(
        "--max-full-depth-surface-hours",
        type=int,
        default=0,
        help="Cap full-depth collection hours; 0 means cover the full dataset window.",
    )
    parser.add_argument(
        "--runtime-deployment-id",
        default="pm5d.threelayer.settlement-probability-btc-eth.dryrun",
    )
    parser.add_argument(
        "--runtime-config-path",
        default="/opt/ploy/config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
    )
    parser.add_argument(
        "--runtime-recording-path",
        default="/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
    )
    parser.add_argument(
        "--runtime-score",
        default="",
    )
    parser.add_argument("--runtime-strategy-profile", default="settlement_probability")
    parser.add_argument("--runtime-issue-number", default="")
    parser.add_argument("--runtime-min-trade-count", default="50")
    parser.add_argument("--runtime-min-fill-rate", default="0.30")
    parser.add_argument("--runtime-min-roi", default="0")
    parser.add_argument("--runtime-source-target", default="full_depth_settlement_executable_pnl")
    parser.add_argument("--runtime-source-horizon", default="5m")
    parser.add_argument(
        "--resolve-snapshot-provenance",
        action=argparse.BooleanOptionalAction,
        default=True,
        help=(
            "Resolve hosted walk-forward continuations back to the source research "
            "snapshot run id recorded in the prior alpha-search artifact."
        ),
    )
    args = parser.parse_args()

    args.output_dir.mkdir(parents=True, exist_ok=True)
    payload = build_executor_payload(args, _load_json(args.plan_json))
    if payload["side_effects_enabled"]:
        attempts = []
        for item in payload["dispatches"]:
            if item["ready"]:
                attempts.append(_dispatch_gh_workflow(item["workflow"], args.git_ref, item["fields"]))
        payload["dispatch_attempts"] = attempts

    _write_json(args.output_dir / "research-manager-executor.json", payload)
    (args.output_dir / "research-manager-executor.md").write_text(
        render_markdown(payload),
        encoding="utf-8",
    )
    if payload.get("typed_prior"):
        _write_json(args.output_dir / "next-llm-prior.json", payload["typed_prior"])


if __name__ == "__main__":
    main()
