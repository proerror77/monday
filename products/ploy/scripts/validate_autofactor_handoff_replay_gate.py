#!/usr/bin/env python3
"""Validate that an AutoFactor handoff is backed by runtime replay evidence."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any


REQUIRED_REPLAY_BASIS = "runtime_market_update_replay"
REQUIRED_REPLAY_WORKFLOW = "runtime-candidate-replay.yml"
REQUIRED_ARTIFACT_PREFIX = "runtime-candidate-replay-"
REQUIRED_CONTRACT_FLAGS = (
    "event_level",
    "one_decision_per_event",
    "official_settlement",
    "full_depth_entry",
)


def _is_ready_replay(replay: dict[str, Any]) -> bool:
    return replay.get("ready") is True or replay.get("promotion_ready") is True


def validate_handoff_payload(handoff: dict[str, Any]) -> list[str]:
    blockers: list[str] = []
    if handoff.get("status") != "ready":
        blockers.append(f"handoff_not_ready:{handoff.get('status') or '<missing>'}")

    strategies = handoff.get("strategies")
    if not isinstance(strategies, list) or not strategies:
        blockers.append("handoff_missing_ready_strategies")
        strategies = []

    replay = handoff.get("candidate_strategy_replay")
    if not isinstance(replay, dict):
        blockers.append("handoff_missing_candidate_strategy_replay")
        replay = {}

    if not _is_ready_replay(replay):
        blockers.append("candidate_strategy_replay_not_ready")
    if replay.get("basis") != REQUIRED_REPLAY_BASIS:
        blockers.append(
            "candidate_strategy_replay_not_runtime_replay:"
            f"{replay.get('basis') or '<missing>'}!={REQUIRED_REPLAY_BASIS}"
        )
    if replay.get("source_workflow") != REQUIRED_REPLAY_WORKFLOW:
        blockers.append(
            "candidate_strategy_replay_source_workflow_mismatch:"
            f"{replay.get('source_workflow') or '<missing>'}!={REQUIRED_REPLAY_WORKFLOW}"
        )
    artifact_name = str(replay.get("artifact_name") or "")
    if not artifact_name.startswith(REQUIRED_ARTIFACT_PREFIX):
        blockers.append(
            "candidate_strategy_replay_artifact_mismatch:"
            f"{artifact_name or '<missing>'}!={REQUIRED_ARTIFACT_PREFIX}*"
        )
    for field in ("workflow_run_id", "workflow_run_url", "candidate_replay_id"):
        if not replay.get(field):
            blockers.append(f"candidate_strategy_replay_missing_{field}")

    contract = replay.get("decision_contract")
    if not isinstance(contract, dict):
        contract = {}
    for flag in REQUIRED_CONTRACT_FLAGS:
        if contract.get(flag) is not True:
            blockers.append(f"candidate_strategy_replay_missing_contract:{flag}")

    replay_runtime_score = str(replay.get("runtime_score") or "")
    if not replay_runtime_score:
        blockers.append("candidate_strategy_replay_missing_runtime_score")
    source_factor = replay.get("source_factor")
    if not isinstance(source_factor, dict):
        source_factor = {}
    source_target = str(source_factor.get("target") or "")
    source_horizon = str(source_factor.get("horizon") or "")
    if not source_target:
        blockers.append("candidate_strategy_replay_missing_source_target")
    if not source_horizon:
        blockers.append("candidate_strategy_replay_missing_source_horizon")
    contract_target = str(contract.get("target") or "")
    contract_horizon = str(contract.get("horizon") or "")
    if contract_target and source_target and contract_target != source_target:
        blockers.append(
            "candidate_strategy_replay_contract_target_mismatch:"
            f"{contract_target}!={source_target}"
        )
    if contract_horizon and source_horizon and contract_horizon != source_horizon:
        blockers.append(
            "candidate_strategy_replay_contract_horizon_mismatch:"
            f"{contract_horizon}!={source_horizon}"
        )
    for index, strategy in enumerate(strategies, start=1):
        if not isinstance(strategy, dict):
            blockers.append(f"handoff_strategy_{index}_invalid")
            continue
        strategy_runtime_score = str(strategy.get("runtime_score") or "")
        if not strategy_runtime_score:
            blockers.append(f"handoff_strategy_{index}_missing_runtime_score")
        elif replay_runtime_score and strategy_runtime_score != replay_runtime_score:
            blockers.append(
                "candidate_strategy_replay_runtime_score_mismatch:"
                f"strategy_{index}:{replay_runtime_score}!={strategy_runtime_score}"
            )
    return blockers


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--handoff-json", required=True)
    parser.add_argument("--output-json", default="")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    handoff_path = Path(args.handoff_json)
    handoff = json.loads(handoff_path.read_text(encoding="utf-8"))
    blockers = validate_handoff_payload(handoff)
    result = {
        "schema_version": 1,
        "kind": "autofactor_handoff_replay_gate",
        "handoff_json": str(handoff_path),
        "ready": not blockers,
        "blockers": blockers,
    }
    if args.output_json:
        output_path = Path(args.output_json)
        output_path.parent.mkdir(parents=True, exist_ok=True)
        output_path.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(result, sort_keys=True))
    if blockers:
        for blocker in blockers:
            print(f"AutoFactor handoff replay gate blocked: {blocker}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
