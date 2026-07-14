#!/usr/bin/env python3
"""Classify alpha-search artifacts into the next closed-loop research action.

This script does not call an LLM and does not promote strategies. It turns the
existing CI artifact bundle into an auditable next action: continue the MCTS
chain, revise the typed prior, fix data/workflow/runtime gates, or hand off a
ready strategy through the existing promotion path.
"""

from __future__ import annotations

import argparse
import json
import math
import re
from pathlib import Path
from typing import Any

try:
    from autofactor_accounting_catalog import autofactor_target_horizon
except ModuleNotFoundError:
    from scripts.autofactor_accounting_catalog import autofactor_target_horizon


DEFAULT_TARGET = "full_depth_settlement_executable_pnl"
RUN_ID_RE = re.compile(r"(\d{8,})")
MAX_RUNTIME_REPLAY_REQUESTS = 5

ALLOWED_MUTATIONS = {
    "add_feature_gate",
    "replace_denominator",
    "add_spread_penalty",
    "add_capacity_gate",
    "add_near_strike_interaction",
    "change_time_window",
    "clip_or_squash",
    "invert_or_contrarian",
    "remove_component",
}

DIMENSION_TO_MUTATION = {
    "sample_power": "add_feature_gate",
    "stability": "add_feature_gate",
    "effectiveness": "replace_denominator",
    "monotonicity": "clip_or_squash",
    "execution_quality": "add_capacity_gate",
    "overfit_risk": "remove_component",
    "exploit": "add_capacity_gate",
}

PREFERRED_FEATURES = {
    "add_feature_gate": ["near_strike_score", "quote_freshness_score", "pm_lag_score"],
    "add_capacity_gate": ["full_depth_entry_fillable_gate", "entry_capacity_score"],
    "add_near_strike_interaction": ["near_strike_score"],
    "add_spread_penalty": ["side_spread"],
    "replace_denominator": ["side_spread", "entry_capacity_score"],
    "remove_component": [
        "near_strike_score",
        "quote_freshness_score",
        "pm_lag_score",
        "side_spread",
    ],
}

RUNTIME_CONTRACT_GAP_TOKENS = (
    "runtime_contract_unmapped_bayes_formula",
    "runtime_contract_unmapped_factor",
    "runtime_input_unsupported",
    "unsupported_runtime_input",
    "unsupported_runtime_inputs",
    "incomplete_runtime_contract_mapping",
    "missing_runtime_contract",
)


def load_json(path: Path) -> Any:
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)


def optional_json(path: Path) -> Any:
    if not path.exists():
        return None
    return load_json(path)


def artifact_root(path: Path) -> Path:
    if (path / "factor-walk-forward-v2").is_dir():
        return path / "factor-walk-forward-v2"
    return path


def infer_run_id(path: Path, chain: dict[str, Any]) -> str:
    value = chain.get("current_run_id")
    if value:
        return str(value)
    for part in reversed(path.parts):
        if match := RUN_ID_RE.search(part):
            return match.group(1)
    return ""


def load_artifact(path: Path, target: str) -> dict[str, Any]:
    root = artifact_root(path)
    alpha_root = root / "alpha-search" / target
    chain = optional_json(path / "alpha-search-chain" / "chain-decision.json")
    if chain is None:
        chain = optional_json(root.parent / "alpha-search-chain" / "chain-decision.json")
    chain = chain or {}
    candidate_strategy_replay = optional_json(
        path / "candidate-strategy-replay" / "candidate-strategy-replay.json"
    )
    if candidate_strategy_replay is None:
        candidate_strategy_replay = optional_json(
            root.parent / "candidate-strategy-replay" / "candidate-strategy-replay.json"
        )
    if candidate_strategy_replay is None:
        candidate_strategy_replay = optional_json(root / "candidate-strategy-replay.json")
    return {
        "artifact_dir": str(path),
        "root": root,
        "run_id": infer_run_id(path, chain),
        "target": target,
        "feedback": optional_json(alpha_root / "search-feedback.json") or {},
        "plan": optional_json(alpha_root / "mcts-expansion-plan.json") or {},
        "state": optional_json(alpha_root / "mcts-state.json") or {},
        "avoided_subtrees": optional_json(alpha_root / "avoided-subtrees.json") or [],
        "handoff": optional_json(root / "autofactor-strategy-handoff.json") or {},
        "promotion": optional_json(root / "autofactor-strategy-promotion.json") or {},
        "registry_preview": optional_json(alpha_root / "factor-registry-preview.json") or {},
        "chain": chain,
        "search_space": optional_json(alpha_root / "search-space.json") or {},
        "candidate_strategy_replay": candidate_strategy_replay or {},
        "input_prior": optional_json(
            path / "alpha-search-chain" / "input-alpha-search-plan" / "next-llm-prior.json"
        )
        or {},
        "input_feedback": optional_json(
            path / "alpha-search-chain" / "input-alpha-search-plan" / "search-feedback.json"
        )
        or {},
    }


def as_float(value: Any) -> float | None:
    if value is None:
        return None
    try:
        number = float(value)
    except (TypeError, ValueError):
        return None
    if not math.isfinite(number):
        return None
    return number


def selected_nodes(plan: dict[str, Any]) -> list[dict[str, Any]]:
    value = plan.get("selected_nodes")
    if not isinstance(value, list):
        return []
    return [item for item in value if isinstance(item, dict)]


def blocker_strings(payload: Any) -> list[str]:
    out: list[str] = []
    if isinstance(payload, dict):
        for key, value in payload.items():
            if key == "blockers" and isinstance(value, list):
                out.extend(str(item) for item in value)
            elif key in {"blocking_risk_flags", "blocked_gates"} and isinstance(value, list):
                out.extend(str(item) for item in value)
            else:
                out.extend(blocker_strings(value))
    elif isinstance(payload, list):
        for item in payload:
            out.extend(blocker_strings(item))
    return out


def target_blocker_strings(payload: Any, target: str) -> list[str]:
    out: list[str] = []
    eligible: list[str] = []
    profile_matched: list[str] = []
    replay_matched: list[str] = []
    if isinstance(payload, dict):
        required_profile = str(payload.get("required_strategy_profile") or "settlement_probability")
        replay = payload.get("candidate_strategy_replay")
        replay_runtime_score = ""
        if isinstance(replay, dict):
            replay_runtime_score = str(replay.get("runtime_score") or "")
        evaluated = payload.get("evaluated_factors")
        if isinstance(evaluated, list):
            for item in evaluated:
                if not isinstance(item, dict):
                    continue
                factor = item.get("factor")
                factor_target = factor.get("target") if isinstance(factor, dict) else None
                if factor_target in {None, target}:
                    row_blockers: list[str] = []
                    blockers = item.get("blockers", [])
                    if isinstance(blockers, list):
                        row_blockers.extend(str(blocker) for blocker in blockers)
                    else:
                        row_blockers.extend(blocker_strings(blockers))
                    out.extend(row_blockers)
                    runtime_mapping = item.get("runtime_mapping")
                    if (
                        isinstance(factor, dict)
                        and factor.get("decision") == "candidate"
                        and factor.get("reason") == "passed"
                        and isinstance(runtime_mapping, dict)
                        and runtime_mapping.get("runtime_score")
                    ):
                        runtime_score = str(runtime_mapping.get("runtime_score") or "")
                        eligible.extend(row_blockers)
                        if replay_runtime_score and runtime_score == replay_runtime_score:
                            replay_matched.extend(row_blockers)
                        if runtime_mapping.get("strategy_profile") == required_profile:
                            profile_matched.extend(row_blockers)
            payload = {key: value for key, value in payload.items() if key != "evaluated_factors"}
        if replay_matched:
            return replay_matched
        if profile_matched:
            return profile_matched
        if eligible:
            return eligible
        out.extend(blocker_strings(payload))
    else:
        out.extend(blocker_strings(payload))
    return out


def classify_blockers(blockers: list[str]) -> str | None:
    text = " ".join(blockers).lower()
    if not text:
        return None
    if any(
        token in text
        for token in [
            "roi_too_low",
            "total_pnl_nonpositive",
            "realized_pnl_nonpositive",
            "negative_runtime_edge",
            "negative_runtime_replay_edge",
        ]
    ):
        return "revise_prior"
    if any(
        token in text
        for token in [
            "data_audit_zero_coverage",
            "snapshot_contract_blocks_execution_claim",
            "sampled_snapshot_required_for_execution_surface",
            "official_settlement_missing",
            "candidate_strategy_replay_missing_contract:official_settlement",
        ]
    ):
        return "fix_data"
    if any(
        token in text
        for token in [
            "candidate_strategy_replay_not_runtime_replay",
            "requires_runtime_replay_not_top_bucket_aggregate",
            "candidate_strategy_replay_identity_basis_mismatch",
        ]
    ):
        return "fix_runtime"
    if any(
        token in text
        for token in [
            "one_event_decision_violation",
            "top_bucket_entry_sweep",
            "entry_sweep_slippage",
            "fill_rate",
            "fillability",
            "capacity",
            "depth",
        ]
    ):
        return "revise_prior"
    if any(
        token in text
        for token in [
            "recorded_replay_parity: blocked: no recorded replay parity artifact",
            "no recorded replay parity artifact",
            "missing-artifact",
            "missing_artifact",
        ]
    ):
        return "fix_workflow"
    if any(token in text for token in ["parity", "runtime", "mapping", "profile"]):
        return "fix_runtime"
    if any(
        token in text
        for token in ["settlement", "official", "data", "coverage", "missing", "freshness"]
    ):
        return "fix_data"
    return None


def runtime_replay_payload(run: dict[str, Any]) -> dict[str, Any]:
    replay = run.get("candidate_strategy_replay")
    if isinstance(replay, dict) and replay:
        return replay
    promotion = run.get("promotion")
    if isinstance(promotion, dict):
        replay = promotion.get("candidate_strategy_replay")
        if isinstance(replay, dict):
            return replay
    handoff = run.get("handoff")
    if isinstance(handoff, dict):
        replay = handoff.get("candidate_strategy_replay")
        if isinstance(replay, dict):
            return replay
    return {}


def runtime_score_base_factor(runtime_score: str) -> str:
    prefix = "autofactor_formula:"
    if runtime_score.startswith(prefix):
        return runtime_score[len(prefix) :]
    return runtime_score


def normalized_factor_key(raw: str) -> str:
    value = runtime_score_base_factor(str(raw or "").strip())
    while True:
        next_value = value
        for prefix in ("llm_", "mcts_", "mut_"):
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


def factor_family(raw: str) -> str:
    value = normalized_factor_key(raw)
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


def configured_direct_passes(counterfactual: dict[str, Any]) -> int | None:
    threshold = str(counterfactual.get("configured_entry_threshold") or "")
    counts = counterfactual.get("direct_pass_counts")
    if not threshold or not isinstance(counts, dict):
        return None
    value = counts.get(threshold)
    if value is None:
        try:
            value = counts.get(f"{float(threshold):.2f}")
        except ValueError:
            value = None
    try:
        return int(value)
    except (TypeError, ValueError):
        return None


def runtime_pass_through_feedback(run: dict[str, Any]) -> dict[str, Any]:
    replay = runtime_replay_payload(run)
    if not replay:
        return {}
    metrics = replay.get("metrics") if isinstance(replay.get("metrics"), dict) else {}
    diagnostics = (
        replay.get("strategy_diagnostics")
        if isinstance(replay.get("strategy_diagnostics"), dict)
        else {}
    )
    counterfactual = (
        replay.get("score_counterfactual")
        if isinstance(replay.get("score_counterfactual"), dict)
        else {}
    )
    runtime_score = str(replay.get("runtime_score") or "").strip()
    formula_evaluations = int(
        as_float(diagnostics.get("settlement_autofactor_formula_evaluations"))
        or as_float(counterfactual.get("formula_evaluations"))
        or 0
    )
    depth_fillable = int(
        as_float(diagnostics.get("settlement_autofactor_depth_fillable"))
        or as_float(counterfactual.get("depth_fillable"))
        or 0
    )
    executable_edge_pass = int(
        as_float(diagnostics.get("settlement_autofactor_executable_edge_pass_min_edge"))
        or 0
    )
    entry_signals = int(
        as_float(diagnostics.get("entry_signals"))
        or as_float(metrics.get("trade_count"))
        or 0
    )
    roi = as_float(metrics.get("roi"))
    total_pnl = as_float(metrics.get("total_pnl"))
    unique_event_count = int(as_float(metrics.get("unique_event_count")) or 0)
    entry_fill_rate = as_float(metrics.get("entry_fill_rate"))
    direct_passes = configured_direct_passes(counterfactual)
    if direct_passes is None:
        direct_passes = int(
            as_float(diagnostics.get("settlement_autofactor_predictive_score_ge_025"))
            or 0
        )

    pass_through_blockers: list[str] = []
    if direct_passes >= 50 and entry_signals < 50:
        pass_through_blockers.append(
            f"runtime_entry_pass_through_too_low:{entry_signals}/{direct_passes}<50"
        )
    if formula_evaluations >= 500 and executable_edge_pass < 50:
        pass_through_blockers.append(
            "runtime_executable_edge_pass_min_edge_too_low:"
            f"{executable_edge_pass}/{formula_evaluations}<50"
        )
    if depth_fillable >= 500 and entry_signals < 50:
        pass_through_blockers.append(
            f"runtime_depth_fillable_to_entry_signal_collapse:{entry_signals}/{depth_fillable}<50"
        )

    economic_blockers: list[str] = []
    if roi is not None and roi < 0.0:
        economic_blockers.append(f"runtime_replay_roi_too_low:{roi:.6f}<0.000000")
    elif roi is None and total_pnl is not None and total_pnl <= 0.0:
        economic_blockers.append(f"runtime_replay_total_pnl_nonpositive:{total_pnl:.6f}")

    blockers = pass_through_blockers + economic_blockers
    if not blockers:
        return {}
    reason = (
        "runtime_pass_through_collapse"
        if pass_through_blockers
        else "negative_runtime_replay_edge"
    )
    return {
        "reason": reason,
        "runtime_score": runtime_score,
        "base_factor": runtime_score_base_factor(runtime_score),
        "metrics": {
            "entry_signals": entry_signals,
            "unique_event_count": unique_event_count,
            "entry_fill_rate": entry_fill_rate,
            "roi": roi,
            "total_pnl": total_pnl,
            "direct_passes_at_configured_threshold": direct_passes,
            "formula_evaluations": formula_evaluations,
            "depth_fillable": depth_fillable,
            "executable_edge_pass_min_edge": executable_edge_pass,
            "score_counterfactual_diagnosis": counterfactual.get("diagnosis"),
            "skip_entry_score": diagnostics.get("skip_entry_score"),
            "skip_edge_score": diagnostics.get("skip_edge_score"),
            "skip_settlement_side_score": diagnostics.get("skip_settlement_side_score"),
        },
        "blockers": blockers,
    }


def runtime_gap_reason(
    blockers: list[str],
    *,
    include_contract_gaps: bool,
) -> str:
    lowered = [str(item).lower() for item in blockers]
    if any("missing_runtime_strategy_mapping" in item for item in lowered):
        return "missing_runtime_strategy_mapping"
    if not include_contract_gaps:
        return ""
    if any("runtime_input_unsupported" in item for item in lowered):
        return "unsupported_runtime_input"
    if any("unsupported_runtime_input" in item for item in lowered):
        return "unsupported_runtime_input"
    for token in RUNTIME_CONTRACT_GAP_TOKENS:
        if any(token in item for item in lowered):
            return token
    return ""


def registry_preview_runtime_gap_candidates(
    run: dict[str, Any],
    *,
    include_contract_gaps: bool,
) -> list[dict[str, Any]]:
    preview = run.get("registry_preview")
    if not isinstance(preview, dict):
        return []
    rows = preview.get("factors")
    if not isinstance(rows, list):
        return []
    target = run.get("target") or DEFAULT_TARGET
    candidates: list[dict[str, Any]] = []
    for row in rows:
        if not isinstance(row, dict):
            continue
        if row.get("target") not in {None, target}:
            continue
        if row.get("status") != "candidate":
            continue
        contract = row.get("runtime_contract")
        if not isinstance(contract, dict):
            contract = {}
        metrics = row.get("metrics")
        if not isinstance(metrics, dict):
            metrics = {}
        blockers: list[str] = []
        for source in (row.get("blockers"), contract.get("blockers")):
            if isinstance(source, list):
                blockers.extend(str(item) for item in source if str(item))
        blockers = sorted(set(blockers))
        reason = runtime_gap_reason(
            blockers,
            include_contract_gaps=include_contract_gaps,
        )
        if not reason:
            continue
        name = str(row.get("factor_name") or "")
        if not name:
            continue
        factor = {
            "name": name,
            "target": row.get("target"),
            "decision": "candidate",
            "reason": "passed",
            "top_bucket_avg_label": metrics.get("top_bucket_avg_label"),
            "positive_window_ratio": metrics.get("positive_window_ratio"),
            "symbol_positive_ratio": metrics.get("symbol_positive_ratio"),
            "spearman_ic": metrics.get("spearman_ic"),
            "reward": metrics.get("reward"),
        }
        candidates.append(
            {
                "item": row,
                "factor": factor,
                "blockers": blockers,
                "name": name,
                "reason": reason,
                "factor_family": str(
                    contract.get("factor_family") or factor_family(name)
                ),
            }
        )
    return candidates


def runtime_mapping_gap_feedback(
    run: dict[str, Any],
    *,
    include_contract_gaps: bool = False,
) -> dict[str, Any]:
    promotion = run.get("promotion")
    target = run.get("target") or DEFAULT_TARGET
    feedback = run.get("feedback") if isinstance(run.get("feedback"), dict) else {}
    best_candidate = str(feedback.get("best_candidate") or "")
    plan_nodes = selected_nodes(run.get("plan") if isinstance(run.get("plan"), dict) else {})
    top_selected = str(plan_nodes[0].get("factor_name") or "") if plan_nodes else ""

    candidates: list[dict[str, Any]] = []
    if isinstance(promotion, dict):
        evaluated = promotion.get("evaluated_factors")
        if isinstance(evaluated, list):
            for item in evaluated:
                if not isinstance(item, dict):
                    continue
                factor = item.get("factor")
                if not isinstance(factor, dict):
                    continue
                if factor.get("target") not in {None, target}:
                    continue
                if factor.get("decision") != "candidate" or factor.get("reason") != "passed":
                    continue
                blockers = blocker_strings(item)
                reason = runtime_gap_reason(
                    blockers,
                    include_contract_gaps=include_contract_gaps,
                )
                if not reason:
                    continue
                name = str(factor.get("name") or "")
                if not name:
                    continue
                candidates.append(
                    {
                        "item": item,
                        "factor": factor,
                        "blockers": blockers,
                        "name": name,
                        "reason": reason,
                        "factor_family": factor_family(name),
                    }
                )
    candidates.extend(
        registry_preview_runtime_gap_candidates(
            run,
            include_contract_gaps=include_contract_gaps,
        )
    )
    if not candidates:
        return {}

    def score(candidate: dict[str, Any]) -> tuple[float, float, float, float]:
        factor = candidate["factor"]
        name = candidate["name"]
        return (
            1.0 if best_candidate and name == best_candidate else 0.0,
            1.0 if top_selected and name == top_selected else 0.0,
            as_float(factor.get("top_bucket_avg_label")) or 0.0,
            as_float(factor.get("spearman_ic")) or 0.0,
        )

    selected = max(candidates, key=score)
    factor = selected["factor"]
    base_factor = selected["name"]
    selected_family = str(selected.get("factor_family") or factor_family(base_factor))
    related: list[dict[str, Any]] = []
    seen_families: set[str] = set()
    for candidate in sorted(candidates, key=score, reverse=True):
        name = str(candidate.get("name") or "")
        family = str(candidate.get("factor_family") or factor_family(name)).strip()
        if not name or not family or family in seen_families:
            continue
        seen_families.add(family)
        candidate_factor = (
            candidate.get("factor") if isinstance(candidate.get("factor"), dict) else {}
        )
        related.append(
            {
                "reason": candidate.get("reason") or "missing_runtime_strategy_mapping",
                "base_factor": name,
                "factor_family": family,
                "runtime_score": "",
                "metrics": {
                    "top_bucket_avg_label": candidate_factor.get("top_bucket_avg_label"),
                    "positive_window_ratio": candidate_factor.get("positive_window_ratio"),
                    "symbol_positive_ratio": candidate_factor.get("symbol_positive_ratio"),
                    "spearman_ic": candidate_factor.get("spearman_ic"),
                    "reward": candidate_factor.get("reward"),
                },
                "blockers": candidate.get("blockers") or [],
            }
        )
        if len(related) >= 8:
            break
    return {
        "reason": selected.get("reason") or "missing_runtime_strategy_mapping",
        "base_factor": base_factor,
        "factor_family": selected_family,
        "runtime_score": "",
        "metrics": {
            "top_bucket_avg_label": factor.get("top_bucket_avg_label"),
            "positive_window_ratio": factor.get("positive_window_ratio"),
            "symbol_positive_ratio": factor.get("symbol_positive_ratio"),
            "spearman_ic": factor.get("spearman_ic"),
            "reward": factor.get("reward"),
        },
        "blockers": selected["blockers"],
        "related_factors": related,
    }


def latest_run(runs: list[dict[str, Any]]) -> dict[str, Any]:
    return runs[-1]


def best_run(runs: list[dict[str, Any]]) -> dict[str, Any] | None:
    scored = [
        run
        for run in runs
        if as_float(run["feedback"].get("best_reward")) is not None
    ]
    if not scored:
        return None
    return max(scored, key=lambda run: as_float(run["feedback"].get("best_reward")) or float("-inf"))


def closed_loop_decision(runs: list[dict[str, Any]]) -> dict[str, Any]:
    current = latest_run(runs)
    handoff = current["handoff"]
    chain = current["chain"]
    feedback = current["feedback"]
    plan = current["plan"]
    target_blockers = target_blocker_strings(current["promotion"], current["target"])
    handoff_blockers = blocker_strings(handoff)
    runtime_feedback = runtime_pass_through_feedback(current)
    runtime_blockers = runtime_feedback.get("blockers") if runtime_feedback else []
    runtime_requests = runtime_replay_requests(current)
    has_runtime_replay_candidate = bool(runtime_requests)
    runtime_unmapped_feedback = runtime_mapping_gap_feedback(
        current,
        include_contract_gaps=not has_runtime_replay_candidate,
    )
    runtime_unmapped_blockers = (
        runtime_unmapped_feedback.get("blockers") if runtime_unmapped_feedback else []
    )
    blockers = list(target_blockers or handoff_blockers)
    if isinstance(runtime_blockers, list):
        blockers.extend(str(item) for item in runtime_blockers)
    if isinstance(runtime_unmapped_blockers, list):
        blockers.extend(str(item) for item in runtime_unmapped_blockers)
    blocker_action = classify_blockers(blockers)

    candidate_count = int(feedback.get("candidate_count") or 0)
    rejected_count = int(feedback.get("rejected_count") or 0)
    reject_ratio = rejected_count / candidate_count if candidate_count > 0 else 0.0
    current_reward = as_float(feedback.get("best_reward"))
    prior_rewards = [
        as_float(run["feedback"].get("best_reward"))
        for run in runs[:-1]
        if as_float(run["feedback"].get("best_reward")) is not None
    ]
    prior_best_reward = max(prior_rewards) if prior_rewards else None

    if handoff.get("status") == "ready":
        action = "ready_handoff"
        reason = "autofactor_strategy_handoff_ready"
    elif runtime_feedback:
        action = "revise_prior"
        reason = str(runtime_feedback.get("reason") or "runtime_pass_through_collapse")
    elif runtime_unmapped_feedback:
        action = "revise_prior"
        reason = str(runtime_unmapped_feedback.get("reason") or "missing_runtime_strategy_mapping")
    elif has_runtime_replay_candidate and blocker_action not in {"fix_data", "fix_workflow"}:
        action = "fix_runtime"
        reason = "runtime_mappable_candidate_needs_runtime_replay"
    elif blocker_action in {"fix_runtime", "fix_data", "fix_workflow", "revise_prior"}:
        action = blocker_action
        reason = f"promotion_blockers_require_{blocker_action}"
    elif not feedback:
        action = "fix_data"
        reason = "missing_search_feedback"
    elif candidate_count == 0:
        action = "fix_data"
        reason = "zero_search_candidates"
    elif chain.get("reason") == "reward_stagnation":
        action = "revise_prior"
        reason = "reward_stagnation"
    elif (
        current_reward is not None
        and prior_best_reward is not None
        and current_reward <= prior_best_reward
    ):
        action = "revise_prior"
        reason = "latest_best_reward_did_not_improve"
    elif chain.get("reason") == "no_selected_nodes":
        action = "revise_prior"
        reason = "no_selected_nodes"
    elif not selected_nodes(plan):
        action = "revise_prior"
        reason = "mcts_plan_has_no_selected_nodes"
    elif reject_ratio >= 0.75:
        action = "revise_prior"
        reason = "high_rejected_candidate_ratio"
    elif chain.get("reason") == "continue" and chain.get("should_dispatch") is True:
        action = "continue_search"
        reason = "mcts_chain_requested_next_run"
    elif chain.get("reason") in {
        "missing_mcts_expansion_plan",
        "missing_current_search_feedback",
        "missing_best_reward",
        "invalid_best_reward",
    }:
        action = "fix_data"
        reason = str(chain.get("reason"))
    elif handoff.get("status") == "blocked":
        action = blocker_action or "revise_prior"
        reason = "handoff_blocked_without_ready_strategy"
    else:
        action = "continue_search"
        reason = "search_has_expandable_mcts_nodes"

    allow_dispatch = (
        action == "continue_search"
        and chain.get("reason") == "continue"
        and chain.get("should_dispatch") is True
    )
    best = best_run(runs)
    runtime_requests = runtime_requests if action == "fix_runtime" else []
    runtime_request = runtime_requests[0] if runtime_requests else None
    return {
        "schema_version": 1,
        "kind": "alpha_search_closed_loop_decision",
        "target": current["target"],
        "decision": action,
        "action": action,
        "allow_dispatch": allow_dispatch,
        "reason": reason,
        "guarantee": "No profitability guarantee. This artifact only selects the next evidence-producing action.",
        "profit_claim": False,
        "external_llm_called": False,
        "evidence_stage": "walk_forward",
        "current_run": summarize_run(current),
        "best_run": summarize_run(best) if best else None,
        "artifact_count": len(runs),
        "runs": [summarize_run(run) for run in runs],
        "promotion_blockers": blockers,
        "next_steps": next_steps(action),
        "prior_revision_required": action == "revise_prior",
        "runtime_replay_request": runtime_request,
        "runtime_replay_requests": runtime_requests,
        "runtime_pass_through_feedback": runtime_feedback,
        "runtime_unmapped_feedback": runtime_unmapped_feedback,
    }


def runtime_replay_request(run: dict[str, Any]) -> dict[str, Any] | None:
    candidates = runtime_replay_requests(run, limit=1)
    if candidates:
        return candidates[0]
    return None


def runtime_replay_requests(
    run: dict[str, Any],
    *,
    limit: int = MAX_RUNTIME_REPLAY_REQUESTS,
) -> list[dict[str, Any]]:
    candidates = runtime_replay_candidates(run, limit=limit)
    if candidates:
        return candidates
    avoided_families = runtime_avoid_families(run)
    requests: list[dict[str, Any]] = []
    seen_scores: set[str] = set()
    for source in (run.get("promotion"), run.get("handoff")):
        if not isinstance(source, dict):
            continue
        replay = source.get("candidate_strategy_replay")
        if not isinstance(replay, dict):
            continue
        runtime_score = str(replay.get("runtime_score") or "").strip()
        strategy_profile = str(replay.get("strategy_profile") or "settlement_probability").strip()
        if not runtime_score:
            continue
        if runtime_score in seen_scores:
            continue
        if factor_family(runtime_score_base_factor(runtime_score)) in avoided_families:
            continue
        seen_scores.add(runtime_score)
        requests.append(
            runtime_replay_request_payload(
                target=str(run.get("target") or DEFAULT_TARGET),
                runtime_score=runtime_score,
                strategy_profile=strategy_profile or "settlement_probability",
                source_factor="",
            )
        )
        if len(requests) >= limit:
            break
    return requests


def runtime_replay_options_json(target: str) -> str:
    return json.dumps(
        {
            "full_depth_entry": True,
            "skip_settlement_exits": False,
            "source_target": target,
            "source_horizon": autofactor_target_horizon(target),
        },
        separators=(",", ":"),
        sort_keys=True,
    )


def runtime_replay_request_payload(
    *,
    target: str,
    runtime_score: str,
    strategy_profile: str,
    source_factor: str,
) -> dict[str, Any]:
    payload = {
        "workflow": "runtime-candidate-replay.yml",
        "git_ref": "main",
        "reason": "replace aggregate top-bucket diagnostic with runtime_market_update_replay evidence",
        "inputs": {
            "deployment_id": "pm5d.threelayer.settlement-probability-btc-eth.dryrun",
            "config_path": "/opt/ploy/config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
            "recording_path": "/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
            "runtime_score": runtime_score,
            "strategy_profile": strategy_profile or "settlement_probability",
            "min_trade_count": "50",
            "min_fill_rate": "0.30",
            "min_roi": "0",
            "options_json": runtime_replay_options_json(target),
        },
    }
    if source_factor:
        payload["source_factor"] = source_factor
    return payload


def runtime_replay_candidate(run: dict[str, Any]) -> dict[str, Any] | None:
    candidates = runtime_replay_candidates(run, limit=1)
    return candidates[0] if candidates else None


def has_runtime_replay_disqualifying_blocker(item: dict[str, Any]) -> bool:
    blockers = [blocker.lower() for blocker in blocker_strings(item)]
    disqualifying_tokens = (
        "missing_runtime_contract",
        "runtime_contract_blocked",
        "runtime_input_semantics_mismatch",
        "unsupported_runtime_input",
        "unsupported_runtime_inputs",
        "missing_runtime_score",
        "missing_runtime_strategy_mapping",
        "empty_runtime_strategy_profile",
        "required_strategy_profile_mismatch",
    )
    return any(
        any(token in blocker for token in disqualifying_tokens)
        for blocker in blockers
    )


def runtime_replay_candidates(
    run: dict[str, Any],
    *,
    limit: int = MAX_RUNTIME_REPLAY_REQUESTS,
) -> list[dict[str, Any]]:
    promotion = run.get("promotion")
    target = run.get("target") or DEFAULT_TARGET
    required_profile = "settlement_probability"
    evaluated: list[Any] = []
    if isinstance(promotion, dict):
        required_profile = str(
            promotion.get("required_strategy_profile") or required_profile
        )
        raw_evaluated = promotion.get("evaluated_factors")
        if isinstance(raw_evaluated, list):
            evaluated.extend(raw_evaluated)
    evaluated.extend(registry_preview_runtime_candidates(run, required_profile))
    avoided_families = runtime_avoid_families(run)
    candidates: list[dict[str, Any]] = []
    for item in evaluated:
        if not isinstance(item, dict):
            continue
        factor = item.get("factor")
        runtime_mapping = item.get("runtime_mapping")
        if not isinstance(factor, dict) or not isinstance(runtime_mapping, dict):
            continue
        if has_runtime_replay_disqualifying_blocker(item):
            continue
        if factor.get("target") not in {None, target}:
            continue
        if factor.get("decision") != "candidate" or factor.get("reason") != "passed":
            continue
        factor_name = str(factor.get("name") or "")
        if factor_family(factor_name) in avoided_families:
            continue
        if runtime_mapping.get("strategy_profile") != required_profile:
            continue
        runtime_score = str(runtime_mapping.get("runtime_score") or "").strip()
        if not runtime_score:
            continue
        candidates.append(item)
    if not candidates:
        return []

    best_candidate_name = str((run.get("feedback") or {}).get("best_candidate") or "")

    def score(item: dict[str, Any]) -> tuple[float, float, float, float, float, float, float, float, float]:
        factor = item.get("factor") if isinstance(item.get("factor"), dict) else {}
        return (
            1.0 if best_candidate_name and factor.get("name") == best_candidate_name else 0.0,
            1.0
            if factor.get("decision") == "candidate" and factor.get("reason") == "passed"
            else 0.0,
            as_float(factor.get("reward")) or 0.0,
            as_float(factor.get("top_bucket_n")) or 0.0,
            as_float(factor.get("top_bucket_avg_label")) or 0.0,
            as_float(factor.get("top_bucket_full_depth_entry_fill_rate")) or 0.0,
            as_float(factor.get("positive_window_ratio")) or 0.0,
            as_float(factor.get("symbol_positive_ratio")) or 0.0,
            as_float(factor.get("spearman_ic")) or 0.0,
        )

    requests: list[dict[str, Any]] = []
    seen_scores: set[str] = set()
    for selected in sorted(candidates, key=score, reverse=True):
        factor = selected.get("factor") if isinstance(selected.get("factor"), dict) else {}
        mapping = (
            selected.get("runtime_mapping")
            if isinstance(selected.get("runtime_mapping"), dict)
            else {}
        )
        runtime_score = str(mapping.get("runtime_score") or "").strip()
        strategy_profile = str(mapping.get("strategy_profile") or required_profile).strip()
        if not runtime_score or runtime_score in seen_scores:
            continue
        seen_scores.add(runtime_score)
        requests.append(
            runtime_replay_request_payload(
                target=str(target),
                runtime_score=runtime_score,
                strategy_profile=strategy_profile or "settlement_probability",
                source_factor=str(factor.get("name") or ""),
            )
        )
        if len(requests) >= limit:
            break
    return requests


def registry_preview_runtime_candidates(
    run: dict[str, Any],
    required_profile: str,
) -> list[dict[str, Any]]:
    preview = run.get("registry_preview")
    if not isinstance(preview, dict):
        return []
    rows = preview.get("factors")
    if not isinstance(rows, list):
        return []
    candidates: list[dict[str, Any]] = []
    for row in rows:
        if not isinstance(row, dict):
            continue
        contract = row.get("runtime_contract")
        if not isinstance(contract, dict):
            continue
        metrics = row.get("metrics")
        if not isinstance(metrics, dict):
            metrics = {}
        runtime_score = str(contract.get("runtime_score") or "").strip()
        strategy_profile = str(contract.get("strategy_profile") or "").strip()
        if not runtime_score or strategy_profile != required_profile:
            continue
        blockers: list[str] = []
        for source in (row.get("blockers"), contract.get("blockers")):
            if isinstance(source, list):
                blockers.extend(str(item) for item in source if str(item))
        blockers = sorted(set(blockers))
        factor_name = str(row.get("factor_name") or "")
        status = str(row.get("status") or "")
        candidates.append(
            {
                "blockers": blockers,
                "factor": {
                    "name": factor_name,
                    "target": row.get("target"),
                    "decision": "candidate" if status == "candidate" else status,
                    "reason": "passed" if status == "candidate" else status,
                    "reward": as_float(metrics.get("reward")) or 0.0,
                    "top_bucket_n": as_float(metrics.get("top_bucket_n"))
                    or as_float(metrics.get("top_bucket_unique_event_count"))
                    or 0.0,
                    "top_bucket_avg_label": as_float(metrics.get("top_bucket_avg_label"))
                    or 0.0,
                    "top_bucket_full_depth_entry_fill_rate": as_float(
                        metrics.get("top_bucket_full_depth_entry_fill_rate")
                    )
                    or 0.0,
                    "positive_window_ratio": as_float(metrics.get("positive_window_ratio"))
                    or 0.0,
                    "symbol_positive_ratio": as_float(metrics.get("symbol_positive_ratio"))
                    or 0.0,
                    "spearman_ic": as_float(metrics.get("spearman_ic")) or 0.0,
                },
                "runtime_mapping": {
                    "runtime_score": runtime_score,
                    "strategy_profile": strategy_profile,
                    "strategy_family": str(contract.get("strategy_family") or ""),
                },
            }
        )
    return candidates


def runtime_avoid_families(run: dict[str, Any]) -> set[str]:
    feedback = run.get("feedback") if isinstance(run, dict) else {}
    raw_items = feedback.get("runtime_avoid_factors") if isinstance(feedback, dict) else None
    if not isinstance(raw_items, list):
        return set()
    families: set[str] = set()
    for item in raw_items:
        if not isinstance(item, dict):
            continue
        family = str(item.get("factor_family") or "").strip()
        if not family:
            family = factor_family(str(item.get("base_factor") or ""))
        if family:
            families.add(family)
    return families


def summarize_run(run: dict[str, Any]) -> dict[str, Any]:
    feedback = run["feedback"]
    handoff = run["handoff"]
    chain = run["chain"]
    plan_nodes = selected_nodes(run["plan"])
    return {
        "artifact_dir": run["artifact_dir"],
        "run_id": run["run_id"],
        "candidate_count": feedback.get("candidate_count"),
        "passed_count": feedback.get("passed_count"),
        "rejected_count": feedback.get("rejected_count"),
        "best_candidate": feedback.get("best_candidate"),
        "best_reward": feedback.get("best_reward"),
        "selected_node_count": len(plan_nodes),
        "top_selected_factor": plan_nodes[0].get("factor_name") if plan_nodes else None,
        "handoff_status": handoff.get("status"),
        "recommended_action": handoff.get("recommended_action"),
        "chain_reason": chain.get("reason"),
        "chain_should_dispatch": chain.get("should_dispatch"),
    }


def next_steps(action: str) -> list[str]:
    if action == "ready_handoff":
        return [
            "Run the ready-only AutoFactor promotion/config PR path.",
            "Do not deploy live; dry-run promotion still requires normal PR and runtime gates.",
        ]
    if action == "continue_search":
        return [
            "Dispatch the next bounded hosted alpha-search iteration with the current MCTS plan.",
            "Keep evidence stage as walk_forward until promotion gates pass.",
        ]
    if action == "revise_prior":
        return [
            "Review weak dimensions and generated typed prior mutations.",
            "Pass the generated prior JSON through options_json.alpha_search_llm_prior_json on the next search run.",
        ]
    if action == "fix_data":
        return [
            "Fix missing or weak data surfaces before additional model/search tuning.",
            "Rebuild or choose a snapshot with official settlement, full-depth CLOB, and adequate event coverage.",
        ]
    if action == "fix_runtime":
        return [
            "Run runtime-candidate-replay.yml for the requested runtime score to replace the top-bucket aggregate with runtime_market_update_replay evidence.",
            "Feed the resulting runtime-candidate-replay artifact back into the AutoFactor promotion evaluator before any dry-run handoff.",
        ]
    return [
        "Repair the workflow artifact bundle before interpreting search quality.",
        "Rerun the hosted artifact workflow and require the full alpha-search artifact bundle.",
    ]


def feature_available(search_space: dict[str, Any], feature: str) -> bool:
    pool = search_space.get("feature_pool")
    return isinstance(pool, list) and feature in pool


def choose_feature(search_space: dict[str, Any], mutation_type: str) -> str | None:
    for feature in PREFERRED_FEATURES.get(mutation_type, []):
        if feature_available(search_space, feature):
            return feature
    return None


def mutation_from_node(node: dict[str, Any]) -> str:
    proposed = str(node.get("proposed_mutation") or "")
    if proposed in ALLOWED_MUTATIONS:
        return proposed
    dimension = str(node.get("selected_dimension") or "")
    return DIMENSION_TO_MUTATION.get(dimension, "clip_or_squash")


def collect_runtime_avoid_factors(
    runs: list[dict[str, Any]], decision: dict[str, Any]
) -> list[dict[str, Any]]:
    by_family: dict[str, dict[str, Any]] = {}

    def is_runtime_collapse(item: dict[str, Any]) -> bool:
        return str(item.get("reason") or "") == "runtime_pass_through_collapse"

    def add_item(item: dict[str, Any]) -> None:
        base_factor = str(item.get("base_factor") or "").strip()
        family = str(item.get("factor_family") or factor_family(base_factor)).strip()
        if not base_factor or not family:
            return
        payload = {
            "base_factor": base_factor,
            "factor_family": family,
            "runtime_score": item.get("runtime_score") or "",
            "reason": item.get("reason") or "runtime_pass_through_collapse",
        }
        if item.get("metrics") is not None:
            payload["metrics"] = item.get("metrics")
        existing = by_family.get(family)
        if existing is not None and is_runtime_collapse(existing) and not is_runtime_collapse(payload):
            return
        by_family[family] = payload

    for run in runs:
        prior = run.get("input_prior") if isinstance(run, dict) else {}
        prior_existing = (
            prior.get("runtime_avoid_factors") if isinstance(prior, dict) else None
        )
        if isinstance(prior_existing, list):
            for item in prior_existing:
                if isinstance(item, dict):
                    add_item(item)

        input_feedback = run.get("input_feedback") if isinstance(run, dict) else {}
        input_existing = (
            input_feedback.get("runtime_avoid_factors")
            if isinstance(input_feedback, dict)
            else None
        )
        if isinstance(input_existing, list):
            for item in input_existing:
                if isinstance(item, dict):
                    add_item(item)

        feedback = run.get("feedback") if isinstance(run, dict) else {}
        existing = feedback.get("runtime_avoid_factors") if isinstance(feedback, dict) else None
        if not isinstance(existing, list):
            continue
        for item in existing:
            if isinstance(item, dict):
                add_item(item)

    runtime_feedback = decision.get("runtime_pass_through_feedback")
    if isinstance(runtime_feedback, dict) and runtime_feedback:
        base_factor = str(runtime_feedback.get("base_factor") or "").strip()
        family = factor_family(base_factor)
        if base_factor and family:
            by_family[family] = {
                "base_factor": base_factor,
                "factor_family": family,
                "runtime_score": runtime_feedback.get("runtime_score"),
                "reason": runtime_feedback.get("reason") or "runtime_pass_through_collapse",
                "metrics": runtime_feedback.get("metrics"),
            }
    runtime_unmapped_feedback = decision.get("runtime_unmapped_feedback")
    if isinstance(runtime_unmapped_feedback, dict) and runtime_unmapped_feedback:
        related = runtime_unmapped_feedback.get("related_factors")
        if isinstance(related, list):
            for item in related:
                if isinstance(item, dict):
                    add_item(item)
        base_factor = str(runtime_unmapped_feedback.get("base_factor") or "").strip()
        family = str(
            runtime_unmapped_feedback.get("factor_family") or factor_family(base_factor)
        ).strip()
        if base_factor and family:
            add_item({
                "base_factor": base_factor,
                "factor_family": family,
                "runtime_score": runtime_unmapped_feedback.get("runtime_score") or "",
                "reason": runtime_unmapped_feedback.get("reason")
                or "missing_runtime_strategy_mapping",
                "metrics": runtime_unmapped_feedback.get("metrics"),
            })
    return [by_family[key] for key in sorted(by_family)]


def collect_structural_avoid_signatures(runs: list[dict[str, Any]]) -> list[dict[str, Any]]:
    by_signature: dict[str, dict[str, Any]] = {}

    def add_item(item: dict[str, Any]) -> None:
        signature = str(item.get("structural_signature") or "").strip()
        if not signature:
            return
        count = item.get("count")
        try:
            count_value = int(count)
        except (TypeError, ValueError):
            count_value = 0
        payload = {
            "structural_signature": signature,
            "root_gene": str(item.get("root_gene") or ""),
            "count": max(count_value, 3),
            "reason": str(item.get("reason") or "structural_signature_crowding"),
        }
        existing = by_signature.get(signature)
        if existing is None or int(existing.get("count") or 0) < payload["count"]:
            by_signature[signature] = payload

    for run in runs:
        prior = run.get("input_prior") if isinstance(run, dict) else {}
        prior_existing = (
            prior.get("structural_avoid_signatures") if isinstance(prior, dict) else None
        )
        if isinstance(prior_existing, list):
            for item in prior_existing:
                if isinstance(item, dict):
                    add_item(item)

        avoided = run.get("avoided_subtrees") if isinstance(run, dict) else None
        if isinstance(avoided, list):
            for item in avoided:
                if isinstance(item, dict) and str(item.get("action") or "") == "penalize":
                    add_item(item)

    return [by_signature[key] for key in sorted(by_signature)]


def runtime_feedback_has_direct_signal(runtime_feedback: dict[str, Any]) -> bool:
    metrics = runtime_feedback.get("metrics")
    if not isinstance(metrics, dict):
        return True
    direct_passes = as_float(metrics.get("direct_passes_at_configured_threshold"))
    entry_signals = as_float(metrics.get("entry_signals"))
    if direct_passes is not None and direct_passes <= 0:
        return False
    if entry_signals is not None and entry_signals <= 0 and direct_passes == 0:
        return False
    return True


def build_prior(runs: list[dict[str, Any]], decision: dict[str, Any], limit: int) -> dict[str, Any]:
    current = latest_run(runs)
    mutations = []
    runtime_avoid_factors = collect_runtime_avoid_factors(runs, decision)
    structural_avoid_signatures = collect_structural_avoid_signatures(runs)
    runtime_avoid_families = {
        str(item.get("factor_family") or "").strip()
        for item in runtime_avoid_factors
        if isinstance(item, dict)
    }
    runtime_feedback = decision.get("runtime_pass_through_feedback")
    if (
        isinstance(runtime_feedback, dict)
        and runtime_feedback
        and runtime_feedback_has_direct_signal(runtime_feedback)
    ):
        base_factor = str(runtime_feedback.get("base_factor") or "")
        if base_factor:
            for mutation_type in ("add_spread_penalty", "add_capacity_gate"):
                if len(mutations) >= limit:
                    break
                item: dict[str, Any] = {
                    "base_factor": base_factor,
                    "mutation_type": mutation_type,
                    "name": f"llm_{base_factor}_runtime_pass_through_{mutation_type}",
                    "feedback_reason": runtime_feedback.get("reason"),
                    "runtime_metrics": runtime_feedback.get("metrics"),
                }
                feature = choose_feature(current["search_space"], mutation_type)
                if feature:
                    item["feature"] = feature
                if mutation_type == "add_spread_penalty":
                    item["constant"] = 0.01
                mutations.append(item)
    for node in selected_nodes(current["plan"])[:limit]:
        if len(mutations) >= limit:
            break
        base_factor = str(node.get("factor_name") or "")
        if not base_factor:
            continue
        if factor_family(base_factor) in runtime_avoid_families:
            continue
        mutation_type = mutation_from_node(node)
        if mutation_type == "do_not_expand_collect_more_data":
            mutation_type = "add_feature_gate"
        item: dict[str, Any] = {
            "base_factor": base_factor,
            "mutation_type": mutation_type,
            "name": f"llm_{base_factor}_{mutation_type}",
        }
        feature = choose_feature(current["search_space"], mutation_type)
        if feature:
            if mutation_type == "replace_denominator":
                item["denominator_feature"] = feature
            else:
                item["feature"] = feature
        if mutation_type in {"replace_denominator", "add_spread_penalty"}:
            item["constant"] = 0.01
        if mutation_type == "clip_or_squash":
            item["lo"] = -3.0
            item["hi"] = 3.0
        if mutation_type == "change_time_window":
            item["window"] = 30
        mutations.append(item)

    fallback_base_factor = "auto_settlement_model_full_depth_settlement_edge"
    if (
        not mutations
        and decision["action"] == "revise_prior"
        and factor_family(fallback_base_factor) not in runtime_avoid_families
    ):
        mutations.append(
            {
                "base_factor": fallback_base_factor,
                "mutation_type": "add_capacity_gate",
                "name": "llm_model_full_depth_edge_capacity_gate",
                "feature": "entry_capacity_score",
            }
        )

    return {
        "schema_version": 1,
        "kind": "typed_llm_prior_draft",
        "source": "alpha_search_closed_loop_agent",
        "target": current["target"],
        "decision_reason": decision["reason"],
        "runtime_avoid_factors": runtime_avoid_factors,
        "structural_avoid_signatures": structural_avoid_signatures,
        "mutations": mutations,
    }


def write_markdown(decision: dict[str, Any], path: Path, prior_path: Path | None) -> None:
    current = decision["current_run"]
    lines = [
        "# Alpha Search Closed-Loop Decision",
        "",
        f"- Action: `{decision['action']}`",
        f"- Reason: `{decision['reason']}`",
        f"- Allow chained dispatch: `{decision['allow_dispatch']}`",
        f"- Evidence stage: `{decision['evidence_stage']}`",
        f"- Target: `{decision['target']}`",
        f"- Current run: `{current.get('run_id') or 'n/a'}`",
        f"- Best candidate: `{current.get('best_candidate') or 'n/a'}`",
        f"- Best reward: `{current.get('best_reward') if current.get('best_reward') is not None else 'n/a'}`",
        f"- Handoff status: `{current.get('handoff_status') or 'n/a'}`",
        f"- Chain reason: `{current.get('chain_reason') or 'n/a'}`",
        "",
        decision["guarantee"],
        "",
        "## Next Steps",
        "",
    ]
    lines.extend(f"- {item}" for item in decision["next_steps"])
    if prior_path is not None:
        lines.extend(["", f"Typed prior draft: `{prior_path}`"])
    runtime_request = decision.get("runtime_replay_request")
    if isinstance(runtime_request, dict):
        lines.extend(["", "## Runtime Replay Request", ""])
        lines.append(f"- Workflow: `{runtime_request.get('workflow')}`")
        lines.append(f"- Reason: `{runtime_request.get('reason')}`")
        inputs = runtime_request.get("inputs") if isinstance(runtime_request.get("inputs"), dict) else {}
        for key in (
            "deployment_id",
            "config_path",
            "recording_path",
            "runtime_score",
            "strategy_profile",
            "issue_number",
            "min_trade_count",
            "min_fill_rate",
            "min_roi",
            "options_json",
        ):
            if key in inputs:
                lines.append(f"- {key}: `{inputs[key]}`")
    runtime_requests = decision.get("runtime_replay_requests")
    if isinstance(runtime_requests, list) and len(runtime_requests) > 1:
        lines.extend(["", "## Additional Runtime Replay Requests", ""])
        for index, request in enumerate(runtime_requests[1:], start=2):
            if not isinstance(request, dict):
                continue
            inputs = request.get("inputs") if isinstance(request.get("inputs"), dict) else {}
            lines.append(
                "- {index}. `{source}` -> `{runtime_score}`".format(
                    index=index,
                    source=request.get("source_factor") or "<candidate>",
                    runtime_score=inputs.get("runtime_score", ""),
                )
            )
    if decision["promotion_blockers"]:
        lines.extend(["", "## Promotion Blockers", ""])
        lines.extend(f"- `{item}`" for item in decision["promotion_blockers"][:20])
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("artifact_dir", nargs="+", help="Downloaded alpha-search artifact directories")
    parser.add_argument("--target", default=DEFAULT_TARGET)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    parser.add_argument("--output-prior-json")
    parser.add_argument("--prior-mutation-limit", type=int, default=6)
    args = parser.parse_args()

    runs = [load_artifact(Path(value), args.target) for value in args.artifact_dir]
    decision = closed_loop_decision(runs)

    output_json = Path(args.output_json)
    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(decision, indent=2, sort_keys=True) + "\n", encoding="utf-8")

    prior_path = Path(args.output_prior_json) if args.output_prior_json else None
    if prior_path is not None and decision["prior_revision_required"]:
        prior = build_prior(runs, decision, max(1, args.prior_mutation_limit))
        prior_path.parent.mkdir(parents=True, exist_ok=True)
        prior_path.write_text(json.dumps(prior, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    elif prior_path is not None and prior_path.exists():
        prior_path.unlink()

    write_markdown(decision, Path(args.output_md), prior_path if decision["prior_revision_required"] else None)


if __name__ == "__main__":
    main()
