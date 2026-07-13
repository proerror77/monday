#!/usr/bin/env python3
"""Fail-closed validation for the protected live-promotion workflow."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
from typing import Any


def load_object(path: Path) -> dict[str, Any]:
    payload = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(payload, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return payload


def require(condition: bool, message: str, failures: list[str]) -> None:
    if not condition:
        failures.append(message)


def validate(
    replay: dict[str, Any],
    parity: dict[str, Any],
    parity_provenance: dict[str, Any],
    dryrun: dict[str, Any],
    *,
    git_sha: str,
    min_replay_trades: int,
    min_dryrun_closed_trades: int,
    max_drawdown_usd: float,
    expected_deployment_id: str,
    expected_strategy_profile: str,
    expected_runtime_score: str,
    expected_config_sha256: str,
) -> dict[str, Any]:
    failures: list[str] = []
    replay_metrics = replay.get("metrics") or {}
    replay_blockers = replay.get("blocking_risk_flags") or []
    decision_contract = replay.get("decision_contract") or {}
    runtime = parity.get("runtime_evidence_comparison") or {}
    parity_blockers = parity.get("blocking_risk_flags") or parity.get("risk_flags") or []
    matching_dryruns = [
        item
        for item in dryrun.get("strategies") or []
        if isinstance(item, dict) and item.get("deployment_id") == expected_deployment_id
    ]
    require(len(matching_dryruns) == 1, "dryrun_deployment_slice_missing_or_ambiguous", failures)
    dryrun_slice = matching_dryruns[0] if len(matching_dryruns) == 1 else {}
    dryrun_summary = dryrun_slice.get("summary") or {}
    dryrun_metrics = dryrun_slice.get("metrics") or {}

    require(min_replay_trades >= 20, "min_replay_trades_below_policy_floor", failures)
    require(
        min_dryrun_closed_trades >= 30,
        "min_dryrun_closed_trades_below_policy_floor",
        failures,
    )
    require(0 < max_drawdown_usd <= 25, "max_drawdown_policy_limit_invalid", failures)

    require(bool(re.fullmatch(r"[0-9a-f]{40}", git_sha)), "invalid_git_sha", failures)
    require(replay.get("evidence_stage") == "executable_replay", "replay_stage_not_executable", failures)
    require(replay.get("basis") == "runtime_market_update_replay", "replay_basis_not_runtime_market_update", failures)
    require(replay.get("promotion_ready") is True, "replay_not_promotion_ready", failures)
    require(not replay_blockers, f"replay_blockers:{replay_blockers}", failures)
    require(replay.get("runner_git_sha") == git_sha, "replay_runner_sha_mismatch", failures)
    require(replay.get("deployment_id") == expected_deployment_id, "replay_deployment_mismatch", failures)
    require(replay.get("strategy_profile") == expected_strategy_profile, "replay_strategy_profile_mismatch", failures)
    require(replay.get("runtime_score") == expected_runtime_score, "replay_runtime_score_mismatch", failures)
    require(replay.get("config_sha256") == expected_config_sha256, "replay_config_sha_mismatch", failures)
    require(bool(replay.get("recording_sha256")), "replay_recording_sha_missing", failures)
    require(int(replay_metrics.get("trade_count") or 0) >= min_replay_trades, "replay_trade_count_too_small", failures)
    require(replay_metrics.get("trade_count") == replay_metrics.get("unique_event_count"), "replay_not_one_trade_per_event", failures)
    require(decision_contract.get("official_settlement") is True, "official_settlement_not_proven", failures)
    require(decision_contract.get("full_depth_entry") is True, "full_depth_entry_not_proven", failures)
    require(float(replay_metrics.get("roi") or 0) > 0, "replay_roi_not_positive", failures)

    orders = runtime.get("orders") or {}
    fills = runtime.get("fills") or {}
    require(runtime.get("strict_parity_ready") is True, "strict_runtime_parity_not_ready", failures)
    require(not parity_blockers, f"parity_blockers:{parity_blockers}", failures)
    require(int(orders.get("shared_count") or 0) > 0, "parity_has_no_shared_orders", failures)
    require(int(fills.get("shared_count") or 0) > 0, "parity_has_no_shared_fills", failures)
    require(
        (parity.get("filters") or {}).get("deployment_id") == expected_deployment_id,
        "parity_deployment_mismatch",
        failures,
    )
    require(parity_provenance.get("deployment_id") == expected_deployment_id, "parity_provenance_deployment_mismatch", failures)
    require(parity_provenance.get("config_sha256") == expected_config_sha256, "parity_config_sha_mismatch", failures)
    require(parity_provenance.get("runner_source") == "workflow_ref", "parity_runner_not_workflow_ref", failures)
    require(parity_provenance.get("runner_git_sha") == git_sha, "parity_runner_sha_mismatch", failures)
    require(parity_provenance.get("skip_settlement_exits") is False, "parity_skipped_settlement_exits", failures)
    require(
        parity_provenance.get("recording_sha256") == replay.get("recording_sha256"),
        "parity_recording_sha_mismatch",
        failures,
    )

    closed_trades = int(dryrun_summary.get("closed_trades") or 0)
    drawdown = float(dryrun_metrics.get("max_drawdown") or 0)
    require(closed_trades >= min_dryrun_closed_trades, "dryrun_closed_trade_count_too_small", failures)
    require(float(dryrun_summary.get("realized_pnl") or 0) > 0, "dryrun_realized_pnl_not_positive", failures)
    require(drawdown <= 0, "dryrun_drawdown_sign_invalid", failures)
    require(abs(drawdown) <= max_drawdown_usd, "dryrun_drawdown_limit_exceeded", failures)
    require(int(dryrun_summary.get("open_positions") or 0) == 0, "dryrun_has_open_positions", failures)

    result = {
        "schema_version": 1,
        "git_sha": git_sha,
        "ready_for_human_live_approval": not failures,
        "failures": failures,
        "replay_trade_count": replay_metrics.get("trade_count"),
        "replay_roi": replay_metrics.get("roi"),
        "dryrun_closed_trades": closed_trades,
        "dryrun_realized_pnl": dryrun_summary.get("realized_pnl"),
        "dryrun_max_drawdown": drawdown,
        "strict_parity_ready": runtime.get("strict_parity_ready"),
    }
    return result


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--replay", type=Path, required=True)
    parser.add_argument("--parity", type=Path, required=True)
    parser.add_argument("--parity-provenance", type=Path, required=True)
    parser.add_argument("--dryrun", type=Path, required=True)
    parser.add_argument("--git-sha", required=True)
    parser.add_argument("--min-replay-trades", type=int, required=True)
    parser.add_argument("--min-dryrun-closed-trades", type=int, required=True)
    parser.add_argument("--max-drawdown-usd", type=float, required=True)
    parser.add_argument("--expected-deployment-id", required=True)
    parser.add_argument("--expected-strategy-profile", required=True)
    parser.add_argument("--expected-runtime-score", required=True)
    parser.add_argument("--expected-config-sha256", required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()

    result = validate(
        load_object(args.replay),
        load_object(args.parity),
        load_object(args.parity_provenance),
        load_object(args.dryrun),
        git_sha=args.git_sha,
        min_replay_trades=args.min_replay_trades,
        min_dryrun_closed_trades=args.min_dryrun_closed_trades,
        max_drawdown_usd=args.max_drawdown_usd,
        expected_deployment_id=args.expected_deployment_id,
        expected_strategy_profile=args.expected_strategy_profile,
        expected_runtime_score=args.expected_runtime_score,
        expected_config_sha256=args.expected_config_sha256,
    )
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["ready_for_human_live_approval"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
