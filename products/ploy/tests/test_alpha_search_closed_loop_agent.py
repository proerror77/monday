import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from scripts import alpha_search_closed_loop_agent as agent

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "alpha_search_closed_loop_agent.py"


def write_json(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload), encoding="utf-8")


def artifact(
    root: Path,
    *,
    target: str = "full_depth_settlement_executable_pnl",
    handoff_status: str = "blocked",
    chain_reason: str = "chain_next_run_false",
    should_dispatch: bool = False,
    selected_nodes: list[dict] | None = None,
    feedback: dict | None = None,
    promotion: dict | None = None,
    registry_preview: dict | None = None,
    candidate_strategy_replay: dict | None = None,
    avoided_subtrees: list[dict] | None = None,
    write_feedback: bool = True,
) -> Path:
    factor_root = root / "factor-walk-forward-v2"
    alpha_root = factor_root / "alpha-search" / target
    if write_feedback:
        write_json(
            alpha_root / "search-feedback.json",
            feedback
            or {
                "target": target,
                "candidate_count": 4,
                "rejected_count": 1,
                "passed_count": 0,
                "best_candidate": "auto_settlement_full_depth_settlement_edge",
                "best_reward": 1.25,
            },
        )
    write_json(
        alpha_root / "mcts-expansion-plan.json",
        {
            "target": target,
            "selected_nodes": selected_nodes
            if selected_nodes is not None
            else [
                {
                    "factor_name": "auto_settlement_full_depth_settlement_edge",
                    "selected_dimension": "execution_quality",
                    "proposed_mutation": "add_capacity_gate",
                }
            ],
        },
    )
    write_json(
        alpha_root / "search-space.json",
        {
            "target": target,
            "feature_pool": [
                "entry_capacity_score",
                "full_depth_entry_fillable_gate",
                "near_strike_score",
                "side_spread",
            ],
        },
    )
    write_json(
        factor_root / "autofactor-strategy-handoff.json",
        {
            "status": handoff_status,
            "recommended_action": "create_dry_run_handoff"
            if handoff_status == "ready"
            else "do_not_promote",
        },
    )
    write_json(
        factor_root / "autofactor-strategy-promotion.json",
        promotion or {"decision": "blocked", "evaluated_factors": []},
    )
    if registry_preview is not None:
        write_json(alpha_root / "factor-registry-preview.json", registry_preview)
    if avoided_subtrees is not None:
        write_json(alpha_root / "avoided-subtrees.json", avoided_subtrees)
    write_json(
        root / "alpha-search-chain" / "chain-decision.json",
        {
            "current_run_id": "123456789",
            "reason": chain_reason,
            "should_dispatch": should_dispatch,
        },
    )
    if candidate_strategy_replay is not None:
        write_json(
            root / "candidate-strategy-replay" / "candidate-strategy-replay.json",
            candidate_strategy_replay,
        )
    return root


def run_artifact(parent: Path, run_id: str, *, best_reward: float) -> Path:
    return artifact(
        parent / f"factor-walk-forward-v2-{run_id}",
        feedback={
            "target": agent.DEFAULT_TARGET,
            "candidate_count": 4,
            "rejected_count": 1,
            "passed_count": 0,
            "best_candidate": "auto_settlement_full_depth_settlement_edge",
            "best_reward": best_reward,
        },
    )


class AlphaSearchClosedLoopAgentTest(unittest.TestCase):
    def test_ready_handoff_wins(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), handoff_status="ready")
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "ready_handoff")
            self.assertFalse(decision["allow_dispatch"])
            self.assertFalse(decision["profit_claim"])
            self.assertFalse(decision["external_llm_called"])
            self.assertFalse(decision["prior_revision_required"])

    def test_continue_search_when_chain_dispatches(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), chain_reason="continue", should_dispatch=True)
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "continue_search")
            self.assertTrue(decision["allow_dispatch"])

    def test_reward_stagnation_generates_prior_revision(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), chain_reason="reward_stagnation")
            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            self.assertEqual(decision["decision"], "revise_prior")
            self.assertFalse(decision["allow_dispatch"])
            self.assertTrue(decision["prior_revision_required"])

    def test_overfit_prior_remove_component_names_existing_feature(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                chain_reason="reward_stagnation",
                selected_nodes=[
                    {
                        "factor_name": "auto_settlement_model_full_depth_settlement_edge_x_near_strike_x_capacity",
                        "selected_dimension": "overfit_risk",
                        "proposed_mutation": "remove_component",
                    }
                ],
            )
            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 1)

            self.assertEqual(prior["mutations"][0]["mutation_type"], "remove_component")
            self.assertEqual(prior["mutations"][0]["feature"], "near_strike_score")

    def test_high_rejection_generates_prior_from_selected_nodes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                feedback={
                    "target": agent.DEFAULT_TARGET,
                    "candidate_count": 10,
                    "rejected_count": 8,
                    "passed_count": 0,
                    "best_candidate": "auto_settlement_full_depth_settlement_edge",
                    "best_reward": 1.25,
                },
            )
            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 3)
            self.assertEqual(decision["decision"], "revise_prior")
            self.assertFalse(decision["allow_dispatch"])
            self.assertEqual(prior["mutations"][0]["mutation_type"], "add_capacity_gate")
            self.assertEqual(prior["mutations"][0]["feature"], "full_depth_entry_fillable_gate")

    def test_multiple_artifacts_detect_reward_stagnation(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            first = run_artifact(root, "11111111", best_reward=2.0)
            second = run_artifact(root, "22222222", best_reward=2.0)
            decision = agent.closed_loop_decision(
                [
                    agent.load_artifact(first, agent.DEFAULT_TARGET),
                    agent.load_artifact(second, agent.DEFAULT_TARGET),
                ]
            )
            self.assertEqual(decision["decision"], "revise_prior")
            self.assertFalse(decision["allow_dispatch"])
            self.assertEqual(decision["artifact_count"], 2)

    def test_no_selected_nodes_generates_safe_fallback_prior(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), chain_reason="no_selected_nodes", selected_nodes=[])
            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 3)
            self.assertEqual(decision["decision"], "revise_prior")
            self.assertFalse(decision["allow_dispatch"])
            self.assertTrue(decision["prior_revision_required"])
            self.assertEqual(
                prior["mutations"][0]["base_factor"],
                "auto_settlement_model_full_depth_settlement_edge",
            )

    def test_runtime_blocker_routes_to_fix_runtime(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                chain_reason="continue",
                should_dispatch=True,
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {"blockers": ["missing_runtime_strategy_mapping"]}
                    ],
                },
            )
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "fix_runtime")

    def test_data_audit_blocker_takes_priority_over_aggregate_replay(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                chain_reason="continue",
                should_dispatch=True,
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "snapshot_contract_blocks_execution_claim:"
                                "data_audit_zero_coverage:polymarket_orderbooks:0<288",
                                "requires_runtime_replay_not_top_bucket_aggregate",
                            ]
                        }
                    ],
                },
            )
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )

        self.assertEqual(decision["decision"], "fix_data")
        self.assertEqual(decision["reason"], "promotion_blockers_require_fix_data")

    def test_unmapped_best_candidate_revises_prior_with_runtime_avoid(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                chain_reason="continue",
                should_dispatch=True,
                selected_nodes=[
                    {
                        "factor_name": "mcts_mcts_ofi_l5_depth_norm_spread_adjusted_capacity",
                        "selected_dimension": "execution_quality",
                        "proposed_mutation": "add_capacity_gate",
                    }
                ],
                feedback={
                    "target": agent.DEFAULT_TARGET,
                    "candidate_count": 20,
                    "rejected_count": 5,
                    "passed_count": 2,
                    "best_candidate": "mcts_mcts_ofi_l5_depth_norm_spread_adjusted_capacity",
                    "best_reward": 5.71,
                },
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "missing_runtime_strategy_mapping",
                                "requires_runtime_replay_not_top_bucket_aggregate",
                            ],
                            "factor": {
                                "name": "mcts_mcts_ofi_l5_depth_norm_spread_adjusted_capacity",
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                                "top_bucket_avg_label": 5.40,
                                "positive_window_ratio": 1.0,
                                "symbol_positive_ratio": 1.0,
                                "spearman_ic": 0.20,
                            },
                        }
                    ],
                },
            )

            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 3)

            self.assertEqual(decision["decision"], "revise_prior")
            self.assertEqual(decision["reason"], "missing_runtime_strategy_mapping")
            self.assertIsNone(decision["runtime_replay_request"])
            self.assertEqual(
                decision["runtime_unmapped_feedback"]["base_factor"],
                "mcts_mcts_ofi_l5_depth_norm_spread_adjusted_capacity",
            )
            self.assertEqual(
                prior["runtime_avoid_factors"][0]["factor_family"],
                "ofi_l5_depth_norm",
            )
            self.assertEqual(
                prior["runtime_avoid_factors"][0]["reason"],
                "missing_runtime_strategy_mapping",
            )

    def test_execution_blockers_take_priority_over_runtime_mapping(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                chain_reason="continue",
                should_dispatch=True,
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "one_event_decision_violation:max_event_decisions=3",
                                "missing_runtime_strategy_mapping",
                            ]
                        }
                    ],
                },
            )
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "revise_prior")
            self.assertFalse(decision["allow_dispatch"])
            self.assertTrue(decision["prior_revision_required"])

    def test_non_target_factor_blockers_do_not_drive_target_action(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                chain_reason="continue",
                should_dispatch=True,
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "target_not_allowed",
                                "one_event_decision_violation:max_event_decisions=4",
                            ],
                            "factor": {"target": "full_depth_reprice_pnl_10s"},
                        }
                    ],
                },
            )
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "continue_search")

    def test_missing_recorded_replay_artifact_routes_to_fix_workflow(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "recorded_replay_parity: blocked: no recorded replay parity artifact was supplied to this report"
                            ]
                        }
                    ],
                },
            )
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "fix_workflow")
            self.assertFalse(decision["allow_dispatch"])

    def test_target_factor_blockers_take_priority_over_handoff_global_blockers(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "global_promotion_gate_not_ready:recorded_replay_parity: blocked: no recorded replay parity artifact was supplied to this report"
                            ],
                            "factor": {"target": agent.DEFAULT_TARGET},
                        }
                    ],
                },
            )
            handoff = path / "factor-walk-forward-v2" / "autofactor-strategy-handoff.json"
            payload = json.loads(handoff.read_text(encoding="utf-8"))
            payload["promotion_gate"] = {
                "blocked_gates": [
                    "global_full_depth_entry_fillability: global_full_depth_entry_fill_rate=0.1311 min_required=0.3000"
                ]
            }
            write_json(handoff, payload)

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "fix_workflow")

    def test_runtime_mappable_target_candidate_blockers_take_priority(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "top_bucket_entry_sweep_slippage_too_high:450.00>200.00",
                                "missing_runtime_strategy_mapping",
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": None,
                        },
                        {
                            "blockers": [
                                "global_promotion_gate_not_ready:recorded_replay_parity: blocked: no recorded replay parity artifact was supplied to this report"
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": "autofactor_formula:edge",
                                "strategy_profile": "settlement_probability",
                            },
                        },
                    ],
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "fix_workflow")

    def test_profile_matched_runtime_candidate_blockers_take_priority(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "runtime_profile_mismatch:spread_adjusted_external_move:repricing_momentum!=settlement_probability"
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": "repricing_momentum",
                                "strategy_profile": "repricing_momentum",
                            },
                        },
                        {
                            "blockers": [
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay"
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
                                "strategy_profile": "settlement_probability",
                            },
                        },
                    ],
                    "candidate_strategy_replay": {
                        "runtime_score": "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
                        "strategy_profile": "settlement_probability",
                    },
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "fix_runtime")
            self.assertEqual(
                decision["promotion_blockers"],
                [
                    "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay"
                ],
            )

    def test_runtime_replay_candidate_blockers_take_priority_within_profile(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            runtime_score = "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted"
            other_runtime_score = "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_capacity"
            path = artifact(
                Path(tmp),
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "top_bucket_entry_sweep_slippage_too_high:450.00>200.00",
                                f"candidate_strategy_replay_runtime_score_mismatch:{runtime_score}!={other_runtime_score}",
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": other_runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        },
                        {
                            "blockers": [
                                "requires_runtime_replay_not_top_bucket_aggregate",
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        },
                    ],
                    "candidate_strategy_replay": {
                        "runtime_score": runtime_score,
                        "strategy_profile": "settlement_probability",
                    },
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "fix_runtime")
            self.assertEqual(
                decision["promotion_blockers"],
                [
                    "requires_runtime_replay_not_top_bucket_aggregate",
                    "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                ],
            )

    def test_fix_runtime_includes_runtime_replay_request(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            runtime_score = "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted"
            path = artifact(
                Path(tmp),
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay"
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        }
                    ],
                    "candidate_strategy_replay": {
                        "runtime_score": runtime_score,
                        "strategy_profile": "settlement_probability",
                    },
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            request = decision["runtime_replay_request"]
            self.assertEqual(decision["decision"], "fix_runtime")
            self.assertEqual(request["workflow"], "runtime-candidate-replay.yml")
            self.assertEqual(request["git_ref"], "main")
            self.assertEqual(request["inputs"]["runtime_score"], runtime_score)
            self.assertEqual(request["inputs"]["strategy_profile"], "settlement_probability")
            self.assertEqual(
                json.loads(request["inputs"]["options_json"]),
                {
                    "full_depth_entry": True,
                    "skip_settlement_exits": False,
                    "source_horizon": "5m",
                    "source_target": agent.DEFAULT_TARGET,
                },
            )

    def test_aggregate_replay_missing_artifact_fields_still_routes_to_fix_runtime(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            runtime_score = "autofactor_formula:mut_spread_adjusted_external_move_select_entry_price_quality_ge_050"
            path = artifact(
                Path(tmp),
                target="tradeable_full_depth_settlement_pnl",
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "requires_runtime_replay_not_top_bucket_aggregate",
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                                "candidate_strategy_replay_missing_source_workflow",
                                "candidate_strategy_replay_missing_workflow_run_id",
                                "candidate_strategy_replay_missing_workflow_run_url",
                                "candidate_strategy_replay_missing_artifact_name",
                            ],
                            "factor": {
                                "name": "mut_spread_adjusted_external_move_select_entry_price_quality_ge_050",
                                "target": "tradeable_full_depth_settlement_pnl",
                                "decision": "candidate",
                                "reason": "passed",
                                "top_bucket_avg_label": 3.54,
                            },
                            "runtime_mapping": {
                                "runtime_score": runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        }
                    ],
                    "candidate_strategy_replay": {
                        "basis": "factor_walk_forward_top_bucket_aggregate",
                        "runtime_score": runtime_score,
                        "strategy_profile": "settlement_probability",
                    },
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, "tradeable_full_depth_settlement_pnl")]
            )
            request = decision["runtime_replay_request"]

        self.assertEqual(decision["decision"], "fix_runtime")
        self.assertEqual(request["workflow"], "runtime-candidate-replay.yml")
        self.assertEqual(request["inputs"]["runtime_score"], runtime_score)
        self.assertEqual(
            json.loads(request["inputs"]["options_json"]),
            {
                "full_depth_entry": True,
                "skip_settlement_exits": False,
                "source_horizon": "5m",
                "source_target": "tradeable_full_depth_settlement_pnl",
            },
        )

    def test_fix_runtime_request_uses_best_current_runtime_mappable_factor(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            stale_runtime_score = (
                "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted"
            )
            better_runtime_score = (
                "autofactor_formula:mut_spread_adjusted_external_move_spread_adjusted"
            )
            path = artifact(
                Path(tmp),
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "requires_runtime_replay_not_top_bucket_aggregate",
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                            ],
                            "factor": {
                                "name": "mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                                "positive_window_ratio": 1.0,
                                "symbol_positive_ratio": 1.0,
                                "spearman_ic": 0.10,
                            },
                            "runtime_mapping": {
                                "runtime_score": stale_runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        },
                        {
                            "blockers": [
                                "requires_runtime_replay_not_top_bucket_aggregate",
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                                f"candidate_strategy_replay_runtime_score_mismatch:{stale_runtime_score}!={better_runtime_score}",
                            ],
                            "factor": {
                                "name": "mut_spread_adjusted_external_move_spread_adjusted",
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                                "positive_window_ratio": 1.0,
                                "symbol_positive_ratio": 1.0,
                                "spearman_ic": 0.22,
                            },
                            "runtime_mapping": {
                                "runtime_score": better_runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        },
                    ],
                    "candidate_strategy_replay": {
                        "runtime_score": stale_runtime_score,
                        "strategy_profile": "settlement_probability",
                    },
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )

            request = decision["runtime_replay_request"]
            self.assertEqual(decision["decision"], "fix_runtime")
            self.assertEqual(
                request["source_factor"],
                "mut_spread_adjusted_external_move_spread_adjusted",
            )
            self.assertEqual(request["inputs"]["runtime_score"], better_runtime_score)

    def test_runtime_replay_request_takes_priority_over_high_reject_ratio(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            stale_runtime_score = (
                "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_spread_adjusted_capacity"
            )
            better_runtime_score = (
                "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted"
            )
            path = artifact(
                Path(tmp),
                target="tradeable_full_depth_settlement_pnl",
                feedback={
                    "target": "tradeable_full_depth_settlement_pnl",
                    "candidate_count": 1079,
                    "rejected_count": 958,
                    "passed_count": 31,
                    "best_candidate": "mut_auto_settlement_model_conservative_settlement_edge_spread_adjusted_spread_adjusted",
                    "best_reward": 5.7282,
                },
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "roi_too_low:-0.079091<0.000000",
                                "candidate_strategy_replay_not_ready",
                                f"candidate_strategy_replay_runtime_score_mismatch:{stale_runtime_score}!={better_runtime_score}",
                                "candidate_strategy_replay_roi_too_low:-0.079091<0.000000",
                            ],
                            "factor": {
                                "name": "mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted",
                                "target": "tradeable_full_depth_settlement_pnl",
                                "decision": "candidate",
                                "reason": "passed",
                                "top_bucket_n": 216,
                                "top_bucket_avg_label": 16.533648,
                                "top_bucket_full_depth_entry_fill_rate": 1.0,
                                "positive_window_ratio": 0.8333,
                                "symbol_positive_ratio": 1.0,
                                "spearman_ic": 0.125437,
                            },
                            "runtime_mapping": {
                                "runtime_score": better_runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        }
                    ],
                    "candidate_strategy_replay": {
                        "runtime_score": stale_runtime_score,
                        "strategy_profile": "settlement_probability",
                    },
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, "tradeable_full_depth_settlement_pnl")]
            )

        request = decision["runtime_replay_request"]
        self.assertEqual("fix_runtime", decision["decision"])
        self.assertEqual(
            "runtime_mappable_candidate_needs_runtime_replay",
            decision["reason"],
        )
        self.assertEqual(
            "mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted",
            request["source_factor"],
        )
        self.assertEqual(request["inputs"]["runtime_score"], better_runtime_score)
        self.assertEqual(
            json.loads(request["inputs"]["options_json"])["source_target"],
            "tradeable_full_depth_settlement_pnl",
        )

    def test_runtime_replay_request_can_use_registry_preview_candidate(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            runtime_score = (
                "autofactor_formula:mut_spread_adjusted_external_move_entry_price_quality"
            )
            path = artifact(
                Path(tmp),
                feedback={
                    "target": agent.DEFAULT_TARGET,
                    "candidate_count": 1082,
                    "rejected_count": 874,
                    "passed_count": 96,
                    "best_candidate": "mut_auto_settlement_model_conservative_settlement_edge_spread_adjusted_spread_adjusted",
                    "best_reward": 6.2416,
                    "runtime_avoid_factors": [
                        {
                            "factor_family": "auto_settlement_model_full_depth_settlement_edge",
                            "runtime_score": "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted",
                            "reason": "negative_dry_run_runtime_edge",
                        }
                    ],
                },
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                                "incomplete_runtime_contract_mapping:mut_auto_settlement_model_conservative_settlement_edge_spread_adjusted_spread_adjusted",
                                "runtime_contract_unmapped_factor",
                            ],
                            "factor": {
                                "name": "mut_auto_settlement_model_conservative_settlement_edge_spread_adjusted_spread_adjusted",
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": "",
                                "strategy_profile": "",
                            },
                        }
                    ],
                    "candidate_strategy_replay": {
                        "basis": "factor_walk_forward_top_bucket_aggregate",
                        "runtime_score": "",
                        "strategy_profile": "settlement_probability",
                    },
                },
                registry_preview={
                    "version": "alpha_search_artifacts_v1",
                    "target": agent.DEFAULT_TARGET,
                    "horizon": "5m",
                    "factors": [
                        {
                            "factor_name": "mut_auto_settlement_model_conservative_settlement_edge_spread_adjusted_spread_adjusted",
                            "target": agent.DEFAULT_TARGET,
                            "status": "candidate",
                            "runtime_contract": {
                                "runtime_score": "",
                                "strategy_profile": "",
                                "strategy_family": "",
                                "blockers": ["runtime_contract_unmapped_factor"],
                            },
                            "metrics": {
                                "reward": 6.2416,
                                "top_bucket_unique_event_count": 129,
                                "top_bucket_avg_label": 9.3898,
                                "top_bucket_full_depth_entry_fill_rate": 1.0,
                                "positive_window_ratio": 0.8888,
                                "spearman_ic": 0.1823,
                            },
                            "blockers": ["runtime_contract_unmapped_factor"],
                        },
                        {
                            "factor_name": "mut_spread_adjusted_external_move_entry_price_quality",
                            "target": agent.DEFAULT_TARGET,
                            "status": "candidate",
                            "runtime_contract": {
                                "runtime_score": runtime_score,
                                "strategy_profile": "settlement_probability",
                                "strategy_family": "predictive_settlement_probability",
                                "blockers": [],
                            },
                            "metrics": {
                                "reward": 6.0245,
                                "top_bucket_unique_event_count": 129,
                                "top_bucket_avg_label": 8.4060,
                                "top_bucket_full_depth_entry_fill_rate": 1.0,
                                "positive_window_ratio": 0.8888,
                                "spearman_ic": 0.221,
                            },
                            "blockers": [],
                        },
                    ],
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )

        request = decision["runtime_replay_request"]
        self.assertEqual("fix_runtime", decision["decision"])
        self.assertEqual(
            "runtime_mappable_candidate_needs_runtime_replay",
            decision["reason"],
        )
        self.assertEqual(
            "mut_spread_adjusted_external_move_entry_price_quality",
            request["source_factor"],
        )
        self.assertEqual(runtime_score, request["inputs"]["runtime_score"])

    def test_unmapped_runtime_contract_without_replay_request_revises_prior(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            blocked_factor = (
                "mcts_mcts_auto_settlement_model_conservative_settlement_edge_"
                "spread_adjusted_spread_adjusted_spread_adjusted"
            )
            blocked_family = "auto_settlement_model_conservative_settlement_edge"
            bayes_factor = "mut_bayes_model_market_reversal_select_entry_price_quality_ge_025"
            spread_runtime_score = (
                "autofactor_formula:mut_spread_adjusted_external_move_entry_price_quality"
            )
            path = artifact(
                Path(tmp),
                chain_reason="chain_next_run_false",
                selected_nodes=[
                    {
                        "factor_name": blocked_factor,
                        "selected_dimension": "exploit",
                        "proposed_mutation": "add_capacity_gate",
                    },
                    {
                        "factor_name": bayes_factor,
                        "selected_dimension": "exploit",
                        "proposed_mutation": "add_capacity_gate",
                    },
                ],
                feedback={
                    "target": agent.DEFAULT_TARGET,
                    "candidate_count": 1289,
                    "rejected_count": 875,
                    "passed_count": 256,
                    "best_candidate": blocked_factor,
                    "best_reward": 6.3511,
                    "runtime_avoid_factors": [
                        {
                            "base_factor": "mut_spread_adjusted_external_move_select_entry_price_quality_ge_075",
                            "factor_family": "spread_adjusted_external_move",
                            "runtime_score": (
                                "autofactor_formula:"
                                "mut_spread_adjusted_external_move_select_entry_price_quality_ge_075"
                            ),
                            "reason": "negative_runtime_replay_edge",
                        }
                    ],
                },
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "candidate_strategy_replay_not_runtime_replay:"
                                "factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                                "runtime_contract_unmapped_factor",
                                "empty_runtime_strategy_profile",
                            ],
                            "factor": {
                                "name": blocked_factor,
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": "",
                                "strategy_profile": "",
                            },
                        }
                    ],
                    "candidate_strategy_replay": {
                        "basis": "factor_walk_forward_top_bucket_aggregate",
                        "runtime_score": "",
                        "strategy_profile": "settlement_probability",
                    },
                },
                registry_preview={
                    "version": "alpha_search_artifacts_v1",
                    "target": agent.DEFAULT_TARGET,
                    "horizon": "5m",
                    "factors": [
                        {
                            "factor_name": blocked_factor,
                            "target": agent.DEFAULT_TARGET,
                            "status": "candidate",
                            "runtime_contract": {
                                "runtime_score": "",
                                "strategy_profile": "",
                                "strategy_family": "",
                                "factor_family": blocked_family,
                                "blockers": ["runtime_contract_unmapped_factor"],
                            },
                            "metrics": {
                                "reward": 6.3511,
                                "top_bucket_unique_event_count": 129,
                                "top_bucket_avg_label": 8.8691,
                                "top_bucket_full_depth_entry_fill_rate": 1.0,
                                "positive_window_ratio": 0.8888,
                                "spearman_ic": 0.2316,
                            },
                            "blockers": ["runtime_contract_unmapped_factor"],
                        },
                        {
                            "factor_name": bayes_factor,
                            "target": agent.DEFAULT_TARGET,
                            "status": "candidate",
                            "runtime_contract": {
                                "runtime_score": "",
                                "strategy_profile": "",
                                "strategy_family": "",
                                "factor_family": "bayes_model_market_reversal",
                                "blockers": [
                                    "runtime_contract_unmapped_factor",
                                    "runtime_input_unsupported:bayes_model_disagreement",
                                ],
                            },
                            "metrics": {
                                "reward": 6.15,
                                "top_bucket_unique_event_count": 126,
                                "top_bucket_avg_label": 7.1,
                                "top_bucket_full_depth_entry_fill_rate": 1.0,
                                "positive_window_ratio": 0.7777,
                                "spearman_ic": 0.18,
                            },
                            "blockers": [
                                "runtime_contract_unmapped_factor",
                                "runtime_input_unsupported:bayes_model_disagreement",
                            ],
                        },
                        {
                            "factor_name": "mut_spread_adjusted_external_move_entry_price_quality",
                            "target": agent.DEFAULT_TARGET,
                            "status": "candidate",
                            "runtime_contract": {
                                "runtime_score": spread_runtime_score,
                                "strategy_profile": "settlement_probability",
                                "strategy_family": "predictive_settlement_probability",
                                "factor_family": "spread_adjusted_external_move",
                                "blockers": [],
                            },
                            "metrics": {
                                "reward": 6.02,
                                "top_bucket_unique_event_count": 129,
                                "top_bucket_avg_label": 8.4,
                                "top_bucket_full_depth_entry_fill_rate": 1.0,
                                "positive_window_ratio": 0.8888,
                                "spearman_ic": 0.221,
                            },
                            "blockers": [],
                        },
                    ],
                },
            )

            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 5)

        self.assertEqual("revise_prior", decision["decision"])
        self.assertEqual("runtime_contract_unmapped_factor", decision["reason"])
        self.assertEqual([], decision["runtime_replay_requests"])
        self.assertEqual(blocked_factor, decision["runtime_unmapped_feedback"]["base_factor"])
        self.assertEqual(
            blocked_family,
            decision["runtime_unmapped_feedback"]["factor_family"],
        )
        avoid_families = {
            item["factor_family"] for item in prior["runtime_avoid_factors"]
        }
        self.assertIn(blocked_family, avoid_families)
        self.assertIn("bayes_model_market_reversal", avoid_families)
        self.assertIn("spread_adjusted_external_move", avoid_families)
        mutation_bases = {item["base_factor"] for item in prior["mutations"]}
        self.assertNotIn(blocked_factor, mutation_bases)
        self.assertNotIn(bayes_factor, mutation_bases)

    def test_fix_runtime_includes_batch_runtime_replay_requests(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            scores = [
                (
                    "mut_settlement_capacity",
                    "autofactor_formula:mut_settlement_capacity",
                    6.0,
                    0.75,
                    [],
                ),
                (
                    "mut_external_entry_quality",
                    "autofactor_formula:mut_external_entry_quality",
                    3.0,
                    0.90,
                    [],
                ),
                (
                    "mut_contract_blocked_external_pressure",
                    "autofactor_formula:mut_contract_blocked_external_pressure",
                    10.0,
                    1.00,
                    ["runtime_input_semantics_mismatch:external_pressure"],
                ),
                (
                    "mut_duplicate_score",
                    "autofactor_formula:mut_external_entry_quality",
                    1.0,
                    0.50,
                    [],
                ),
            ]
            path = artifact(
                Path(tmp),
                target="tradeable_full_depth_settlement_pnl",
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "requires_runtime_replay_not_top_bucket_aggregate",
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                            ]
                            + blockers,
                            "factor": {
                                "name": name,
                                "target": "tradeable_full_depth_settlement_pnl",
                                "decision": "candidate",
                                "reason": "passed",
                                "top_bucket_avg_label": avg_label,
                                "top_bucket_full_depth_entry_fill_rate": fill_rate,
                                "top_bucket_n": 400,
                                "positive_window_ratio": 0.75,
                                "symbol_positive_ratio": 1.0,
                                "spearman_ic": 0.10,
                            },
                            "runtime_mapping": {
                                "runtime_score": runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        }
                        for name, runtime_score, avg_label, fill_rate, blockers in scores
                    ],
                    "candidate_strategy_replay": {
                        "basis": "factor_walk_forward_top_bucket_aggregate",
                        "runtime_score": "autofactor_formula:mut_external_entry_quality",
                        "strategy_profile": "settlement_probability",
                    },
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, "tradeable_full_depth_settlement_pnl")]
            )
            requests = decision["runtime_replay_requests"]

        self.assertEqual(decision["decision"], "fix_runtime")
        self.assertEqual([item["source_factor"] for item in requests], [
            "mut_settlement_capacity",
            "mut_external_entry_quality",
        ])
        self.assertEqual(
            [item["inputs"]["runtime_score"] for item in requests],
            [
                "autofactor_formula:mut_settlement_capacity",
                "autofactor_formula:mut_external_entry_quality",
            ],
        )
        for request in requests:
            self.assertEqual(
                json.loads(request["inputs"]["options_json"])["source_target"],
                "tradeable_full_depth_settlement_pnl",
            )

    def test_fix_runtime_request_prefers_feedback_best_candidate(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            best_runtime_score = "autofactor_formula:mut_spread_adjusted_external_move_near_strike"
            higher_ic_runtime_score = "autofactor_formula:mut_spread_adjusted_external_move_squashed"
            path = artifact(
                Path(tmp),
                feedback={
                    "target": agent.DEFAULT_TARGET,
                    "candidate_count": 20,
                    "rejected_count": 5,
                    "passed_count": 2,
                    "best_candidate": "mut_spread_adjusted_external_move_near_strike",
                    "best_reward": 6.75,
                },
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "requires_runtime_replay_not_top_bucket_aggregate",
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                            ],
                            "factor": {
                                "name": "mut_spread_adjusted_external_move_squashed",
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                                "positive_window_ratio": 1.0,
                                "symbol_positive_ratio": 1.0,
                                "spearman_ic": 0.40,
                                "top_bucket_avg_label": 2.88,
                            },
                            "runtime_mapping": {
                                "runtime_score": higher_ic_runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        },
                        {
                            "blockers": [
                                "requires_runtime_replay_not_top_bucket_aggregate",
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
                            ],
                            "factor": {
                                "name": "mut_spread_adjusted_external_move_near_strike",
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                                "positive_window_ratio": 1.0,
                                "symbol_positive_ratio": 1.0,
                                "spearman_ic": 0.20,
                                "top_bucket_avg_label": 3.01,
                            },
                            "runtime_mapping": {
                                "runtime_score": best_runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        },
                    ],
                    "candidate_strategy_replay": {
                        "runtime_score": higher_ic_runtime_score,
                        "strategy_profile": "settlement_probability",
                    },
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            request = decision["runtime_replay_request"]

            self.assertEqual(decision["decision"], "fix_runtime")
            self.assertEqual(
                request["source_factor"],
                "mut_spread_adjusted_external_move_near_strike",
            )
            self.assertEqual(request["inputs"]["runtime_score"], best_runtime_score)

    def test_fix_runtime_request_skips_runtime_avoid_families(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            avoided_runtime_score = (
                "autofactor_formula:mut_spread_adjusted_external_move_spread_adjusted"
            )
            next_runtime_score = "autofactor_formula:mut_poly_lag_pressure_spread_adjusted"
            path = artifact(
                Path(tmp),
                feedback={
                    "target": agent.DEFAULT_TARGET,
                    "candidate_count": 20,
                    "rejected_count": 5,
                    "passed_count": 2,
                    "best_candidate": "mut_poly_lag_pressure_spread_adjusted",
                    "best_reward": 4.12,
                    "runtime_avoid_factors": [
                        {
                            "base_factor": "mut_spread_adjusted_external_move_near_strike",
                            "factor_family": "spread_adjusted_external_move",
                            "runtime_score": "autofactor_formula:mut_spread_adjusted_external_move_near_strike",
                            "reason": "runtime_pass_through_collapse",
                        }
                    ],
                },
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [
                        {
                            "blockers": ["requires_runtime_replay_not_top_bucket_aggregate"],
                            "factor": {
                                "name": "mut_spread_adjusted_external_move_spread_adjusted",
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                                "positive_window_ratio": 1.0,
                                "symbol_positive_ratio": 1.0,
                                "spearman_ic": 0.50,
                                "top_bucket_avg_label": 3.0,
                            },
                            "runtime_mapping": {
                                "runtime_score": avoided_runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        },
                        {
                            "blockers": ["requires_runtime_replay_not_top_bucket_aggregate"],
                            "factor": {
                                "name": "mut_poly_lag_pressure_spread_adjusted",
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                                "positive_window_ratio": 0.8,
                                "symbol_positive_ratio": 1.0,
                                "spearman_ic": 0.10,
                                "top_bucket_avg_label": 1.0,
                            },
                            "runtime_mapping": {
                                "runtime_score": next_runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        },
                    ],
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            request = decision["runtime_replay_request"]

            self.assertEqual(decision["decision"], "fix_runtime")
            self.assertEqual(request["source_factor"], "mut_poly_lag_pressure_spread_adjusted")
            self.assertEqual(request["inputs"]["runtime_score"], next_runtime_score)

    def test_fix_runtime_fallback_request_skips_runtime_avoid_families(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            avoided_runtime_score = (
                "autofactor_formula:mut_spread_adjusted_external_move_spread_adjusted"
            )
            path = artifact(
                Path(tmp),
                feedback={
                    "target": agent.DEFAULT_TARGET,
                    "candidate_count": 20,
                    "rejected_count": 5,
                    "passed_count": 2,
                    "best_candidate": "mut_poly_lag_pressure_spread_adjusted",
                    "best_reward": 4.12,
                    "runtime_avoid_factors": [
                        {
                            "base_factor": "mut_spread_adjusted_external_move_near_strike",
                            "factor_family": "spread_adjusted_external_move",
                            "runtime_score": "autofactor_formula:mut_spread_adjusted_external_move_near_strike",
                            "reason": "runtime_pass_through_collapse",
                        }
                    ],
                },
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [],
                    "candidate_strategy_replay": {
                        "runtime_score": avoided_runtime_score,
                        "strategy_profile": "settlement_probability",
                    },
                },
            )

            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )

            self.assertIsNone(decision["runtime_replay_request"])

    def test_runtime_pass_through_collapse_generates_targeted_prior(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            runtime_score = "autofactor_formula:mut_spread_adjusted_external_move_squashed"
            path = artifact(
                Path(tmp),
                chain_reason="continue",
                should_dispatch=True,
                candidate_strategy_replay={
                    "basis": "runtime_market_update_replay",
                    "promotion_ready": False,
                    "runtime_score": runtime_score,
                    "metrics": {
                        "trade_count": 2,
                        "unique_event_count": 2,
                        "roi": -0.22916199066660334,
                    },
                    "score_counterfactual": {
                        "configured_entry_threshold": "0.25",
                        "depth_fillable": 2918,
                        "direct_pass_counts": {
                            "0.05": 1188,
                            "0.10": 1059,
                            "0.15": 936,
                            "0.25": 744,
                        },
                        "formula_evaluations": 2918,
                    },
                    "strategy_diagnostics": {
                        "entry_signals": 2,
                        "settlement_autofactor_depth_fillable": 2918,
                        "settlement_autofactor_executable_edge_pass_min_edge": 5,
                        "settlement_autofactor_formula_evaluations": 2918,
                        "skip_edge_score": 742,
                        "skip_entry_score": 2174,
                        "skip_settlement_side_score": 2017,
                    },
                },
                promotion={
                    "decision": "blocked",
                    "candidate_strategy_replay": {
                        "basis": "runtime_market_update_replay",
                        "ready": False,
                        "runtime_score": runtime_score,
                        "metrics": {"trade_count": 2, "roi": -0.22916199066660334},
                        "blockers": [
                            "trade_count_too_small:2<50",
                            "roi_too_low:-0.229162<0.000000",
                        ],
                    },
                    "evaluated_factors": [],
                },
            )

            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 3)

            self.assertEqual(decision["decision"], "revise_prior")
            self.assertEqual(decision["reason"], "runtime_pass_through_collapse")
            self.assertIn(
                "runtime_entry_pass_through_too_low:2/744<50",
                decision["promotion_blockers"],
            )
            self.assertEqual(
                decision["runtime_pass_through_feedback"]["metrics"][
                    "executable_edge_pass_min_edge"
                ],
                5,
            )
            self.assertEqual(
                prior["mutations"][0]["base_factor"],
                "mut_spread_adjusted_external_move_squashed",
            )
            self.assertEqual(prior["mutations"][0]["mutation_type"], "add_spread_penalty")
            self.assertEqual(prior["mutations"][0]["feature"], "side_spread")
            self.assertEqual(prior["mutations"][1]["mutation_type"], "add_capacity_gate")
            self.assertEqual(
                prior["runtime_avoid_factors"][0]["factor_family"],
                "spread_adjusted_external_move",
            )
            self.assertEqual(
                agent.factor_family(
                    "llm_mut_spread_adjusted_external_move_squashed_add_capacity_gate"
                ),
                "spread_adjusted_external_move",
            )
            self.assertEqual(
                agent.factor_family(
                    "mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_x_full_depth_entry_gate_spread_adjusted"
                ),
                "auto_settlement_model_full_depth_settlement_edge_x_external_pressure",
            )
            self.assertEqual(
                agent.factor_family(
                    "mut_auto_settlement_model_full_depth_settlement_edge_spread_adjusted_select_near_strike_ge_025"
                ),
                "auto_settlement_model_full_depth_settlement_edge",
            )

    def test_negative_runtime_replay_roi_generates_prior_feedback(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            runtime_score = (
                "autofactor_formula:"
                "mut_spread_adjusted_external_move_select_entry_price_quality_ge_075"
            )
            path = artifact(
                Path(tmp),
                chain_reason="continue",
                should_dispatch=True,
                candidate_strategy_replay={
                    "basis": "runtime_market_update_replay",
                    "promotion_ready": False,
                    "runtime_score": runtime_score,
                    "metrics": {
                        "entry_fill_rate": 1.0,
                        "trade_count": 53,
                        "unique_event_count": 53,
                        "roi": -0.07268639111313208,
                        "total_pnl": -57.78568093494,
                    },
                    "score_counterfactual": {
                        "configured_entry_threshold": "0.25",
                        "depth_fillable": 903,
                        "diagnosis": "reverse_direction_stronger_at_configured_threshold",
                        "direct_pass_counts": {
                            "0.05": 63,
                            "0.10": 53,
                            "0.15": 47,
                            "0.25": 35,
                        },
                        "formula_evaluations": 185,
                    },
                },
                promotion={
                    "decision": "blocked",
                    "required_strategy_profile": "settlement_probability",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "candidate_strategy_replay_not_ready",
                                "candidate_strategy_replay_missing_contract:official_settlement",
                                "candidate_strategy_replay_roi_too_low:-0.072686<0.000000",
                            ],
                            "factor": {
                                "name": "mut_spread_adjusted_external_move_select_entry_price_quality_ge_075",
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                                "top_bucket_avg_label": 1.12,
                                "positive_window_ratio": 1.0,
                                "symbol_positive_ratio": 1.0,
                                "spearman_ic": 0.13,
                            },
                            "runtime_mapping": {
                                "runtime_score": runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        }
                    ],
                    "candidate_strategy_replay": {
                        "basis": "runtime_market_update_replay",
                        "ready": False,
                        "runtime_score": runtime_score,
                        "metrics": {
                            "trade_count": 53,
                            "entry_fill_rate": 1.0,
                            "roi": -0.07268639111313208,
                        },
                        "blockers": [
                            "official_settlement_missing:52<53",
                            "roi_too_low:-0.072686<0.000000",
                        ],
                    },
                },
            )

            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 3)

            self.assertEqual(decision["decision"], "revise_prior")
            self.assertEqual(decision["reason"], "negative_runtime_replay_edge")
            self.assertIn(
                "runtime_replay_roi_too_low:-0.072686<0.000000",
                decision["promotion_blockers"],
            )
            self.assertEqual(decision["runtime_replay_requests"], [])
            self.assertEqual(
                decision["runtime_pass_through_feedback"]["metrics"]["entry_signals"],
                53,
            )
            self.assertEqual(
                decision["runtime_pass_through_feedback"]["metrics"][
                    "score_counterfactual_diagnosis"
                ],
                "reverse_direction_stronger_at_configured_threshold",
            )
            self.assertEqual(
                prior["runtime_avoid_factors"][0]["reason"],
                "negative_runtime_replay_edge",
            )
            self.assertEqual(prior["mutations"][0]["mutation_type"], "add_spread_penalty")

    def test_roi_blockers_are_prior_feedback_even_with_missing_settlement(self) -> None:
        self.assertEqual(
            agent.classify_blockers(
                [
                    "candidate_strategy_replay_missing_contract:official_settlement",
                    "candidate_strategy_replay_roi_too_low:-0.072686<0.000000",
                ]
            ),
            "revise_prior",
        )

    def test_zero_direct_signal_collapse_wins_over_unmapped_same_family(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            base_factor = (
                "mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted"
            )
            runtime_score = f"autofactor_formula:{base_factor}"
            path = artifact(
                Path(tmp),
                chain_reason="continue",
                should_dispatch=True,
                selected_nodes=[
                    {
                        "factor_name": "mcts_mcts_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted_spread_adjusted",
                        "selected_dimension": "execution_quality",
                        "proposed_mutation": "add_capacity_gate",
                    },
                    {
                        "factor_name": "mut_amplitude_weighted_momentum_30s_vol_gap_spread_adjusted",
                        "selected_dimension": "effectiveness",
                        "proposed_mutation": "replace_denominator",
                    },
                ],
                candidate_strategy_replay={
                    "basis": "runtime_market_update_replay",
                    "promotion_ready": False,
                    "runtime_score": runtime_score,
                    "metrics": {"trade_count": 0, "unique_event_count": 0, "roi": 0.0},
                    "score_counterfactual": {
                        "configured_entry_threshold": "0.25",
                        "depth_fillable": 2508,
                        "direct_pass_counts": {
                            "0.05": 0,
                            "0.10": 0,
                            "0.15": 0,
                            "0.25": 0,
                        },
                        "formula_evaluations": 2508,
                    },
                    "strategy_diagnostics": {
                        "entry_signals": 0,
                        "settlement_autofactor_depth_fillable": 2508,
                        "settlement_autofactor_executable_edge_pass_min_edge": 2255,
                        "settlement_autofactor_formula_evaluations": 2508,
                        "skip_entry_score": 2508,
                    },
                },
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "missing_runtime_strategy_mapping",
                                "candidate_strategy_replay_not_ready",
                            ],
                            "factor": {
                                "name": "mcts_mcts_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted_spread_adjusted",
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                                "top_bucket_avg_label": 1.33,
                                "positive_window_ratio": 1.0,
                                "symbol_positive_ratio": 1.0,
                                "spearman_ic": 0.19,
                            },
                        }
                    ],
                },
            )

            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 3)

            self.assertEqual(decision["decision"], "revise_prior")
            self.assertEqual(decision["reason"], "runtime_pass_through_collapse")
            self.assertEqual(
                prior["runtime_avoid_factors"][0]["reason"],
                "runtime_pass_through_collapse",
            )
            self.assertEqual(
                prior["runtime_avoid_factors"][0]["base_factor"],
                base_factor,
            )
            self.assertNotIn(
                "auto_settlement_model_full_depth_settlement_edge_x_external_pressure",
                " ".join(item["base_factor"] for item in prior["mutations"]),
            )
            self.assertEqual(
                prior["mutations"][0]["base_factor"],
                "mut_amplitude_weighted_momentum_30s_vol_gap_spread_adjusted",
            )

    def test_runtime_avoid_factors_accumulate_from_search_feedback(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            near_strike_score = "autofactor_formula:mut_spread_adjusted_external_move_near_strike"
            path = artifact(
                Path(tmp),
                chain_reason="continue",
                should_dispatch=True,
                feedback={
                    "target": agent.DEFAULT_TARGET,
                    "candidate_count": 4,
                    "rejected_count": 1,
                    "passed_count": 0,
                    "best_candidate": "mut_spread_adjusted_external_move_squashed",
                    "best_reward": 1.25,
                    "runtime_avoid_factors": [
                        {
                            "base_factor": "mut_spread_adjusted_external_move_squashed",
                            "factor_family": "spread_adjusted_external_move",
                            "runtime_score": "autofactor_formula:mut_spread_adjusted_external_move_squashed",
                            "reason": "runtime_pass_through_collapse",
                        }
                    ],
                },
                candidate_strategy_replay={
                    "basis": "runtime_market_update_replay",
                    "promotion_ready": False,
                    "runtime_score": near_strike_score,
                    "metrics": {"trade_count": 0, "unique_event_count": 0, "roi": 0.0},
                    "score_counterfactual": {
                        "configured_entry_threshold": "0.25",
                        "depth_fillable": 2934,
                        "direct_pass_counts": {"0.25": 146},
                        "formula_evaluations": 2934,
                    },
                    "strategy_diagnostics": {
                        "entry_signals": 0,
                        "settlement_autofactor_depth_fillable": 2934,
                        "settlement_autofactor_executable_edge_pass_min_edge": 5,
                        "settlement_autofactor_formula_evaluations": 2934,
                    },
                },
            )

            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 3)

            self.assertEqual(decision["decision"], "revise_prior")
            self.assertEqual(len(prior["runtime_avoid_factors"]), 1)
            self.assertEqual(
                prior["runtime_avoid_factors"][0]["factor_family"],
                "spread_adjusted_external_move",
            )
            self.assertEqual(
                prior["runtime_avoid_factors"][0]["base_factor"],
                "mut_spread_adjusted_external_move_near_strike",
            )

    def test_prior_runtime_avoid_factors_carry_forward_and_skip_mutations(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                chain_reason="reward_stagnation",
                selected_nodes=[
                    {
                        "factor_name": "mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
                        "selected_dimension": "exploit",
                        "proposed_mutation": "add_capacity_gate",
                    },
                    {
                        "factor_name": "mut_spread_adjusted_external_move_squashed",
                        "selected_dimension": "execution_quality",
                        "proposed_mutation": "add_capacity_gate",
                    },
                    {
                        "factor_name": "mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_x_full_depth_entry_gate_spread_adjusted",
                        "selected_dimension": "execution_quality",
                        "proposed_mutation": "add_capacity_gate",
                    },
                ],
            )
            write_json(
                path
                / "alpha-search-chain"
                / "input-alpha-search-plan"
                / "next-llm-prior.json",
                {
                    "runtime_avoid_factors": [
                        {
                            "base_factor": "mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
                            "factor_family": "amplitude_weighted_momentum_30s_sigma",
                            "runtime_score": "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
                            "reason": "runtime_pass_through_collapse",
                        },
                        {
                            "base_factor": "mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted",
                            "factor_family": "auto_settlement_model_full_depth_settlement_edge_x_external_pressure",
                            "runtime_score": "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted",
                            "reason": "runtime_pass_through_collapse",
                        }
                    ]
                },
            )
            write_json(
                path
                / "alpha-search-chain"
                / "input-alpha-search-plan"
                / "search-feedback.json",
                {
                    "runtime_avoid_factors": [
                        {
                            "base_factor": "mut_poly_lag_pressure_spread_adjusted",
                            "factor_family": "poly_lag_pressure",
                            "runtime_score": "autofactor_formula:mut_poly_lag_pressure_spread_adjusted",
                            "reason": "runtime_pass_through_collapse",
                        }
                    ]
                },
            )

            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 3)

            self.assertEqual(decision["decision"], "revise_prior")
            self.assertEqual(
                [item["factor_family"] for item in prior["runtime_avoid_factors"]],
                [
                    "amplitude_weighted_momentum_30s_sigma",
                    "auto_settlement_model_full_depth_settlement_edge_x_external_pressure",
                    "poly_lag_pressure",
                ],
            )
            self.assertEqual(len(prior["mutations"]), 1)
            self.assertEqual(
                prior["mutations"][0]["base_factor"],
                "mut_spread_adjusted_external_move_squashed",
            )

    def test_structural_avoid_signatures_enter_next_prior(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            crowded_signature = "SafeDiv(Input(external_move_since_poly_update),Add(Const(_),Input(side_spread)))"
            path = artifact(
                Path(tmp),
                chain_reason="reward_stagnation",
                avoided_subtrees=[
                    {
                        "root_gene": "SafeDiv",
                        "structural_signature": crowded_signature,
                        "depth": 3,
                        "count": 4,
                        "action": "penalize",
                        "reason": "structural_signature_crowding",
                    }
                ],
            )

            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 3)

            self.assertEqual(decision["decision"], "revise_prior")
            self.assertEqual(
                prior["structural_avoid_signatures"],
                [
                    {
                        "structural_signature": crowded_signature,
                        "root_gene": "SafeDiv",
                        "count": 4,
                        "reason": "structural_signature_crowding",
                    }
                ],
            )

    def test_prior_structural_avoid_signatures_carry_forward(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), chain_reason="reward_stagnation")
            write_json(
                path
                / "alpha-search-chain"
                / "input-alpha-search-plan"
                / "next-llm-prior.json",
                {
                    "structural_avoid_signatures": [
                        {
                            "structural_signature": "Mul(Input(a),Input(b))",
                            "root_gene": "Mul",
                            "count": 5,
                            "reason": "structural_signature_crowding",
                        }
                    ]
                },
            )

            runs = [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            decision = agent.closed_loop_decision(runs)
            prior = agent.build_prior(runs, decision, 3)

            self.assertEqual(
                prior["structural_avoid_signatures"][0]["structural_signature"],
                "Mul(Input(a),Input(b))",
            )
            self.assertEqual(prior["structural_avoid_signatures"][0]["count"], 5)

    def test_cli_markdown_includes_runtime_replay_request(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            runtime_score = "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted"
            path = artifact(
                root,
                promotion={
                    "decision": "blocked",
                    "evaluated_factors": [
                        {
                            "blockers": [
                                "candidate_strategy_replay_not_runtime_replay:factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay"
                            ],
                            "factor": {
                                "target": agent.DEFAULT_TARGET,
                                "decision": "candidate",
                                "reason": "passed",
                            },
                            "runtime_mapping": {
                                "runtime_score": runtime_score,
                                "strategy_profile": "settlement_probability",
                            },
                        }
                    ],
                    "candidate_strategy_replay": {
                        "runtime_score": runtime_score,
                        "strategy_profile": "settlement_probability",
                    },
                },
            )
            output_json = root / "decision.json"
            output_md = root / "decision.md"
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    str(path),
                    "--output-json",
                    str(output_json),
                    "--output-md",
                    str(output_md),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )

            decision = json.loads(output_json.read_text(encoding="utf-8"))
            markdown = output_md.read_text(encoding="utf-8")
            self.assertEqual(decision["decision"], "fix_runtime")
            self.assertIn("## Runtime Replay Request", markdown)
            self.assertIn(f"- runtime_score: `{runtime_score}`", markdown)
            self.assertIn("- options_json: `", markdown)

    def test_missing_feedback_routes_to_fix_data(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), write_feedback=False)
            decision = agent.closed_loop_decision(
                [agent.load_artifact(path, agent.DEFAULT_TARGET)]
            )
            self.assertEqual(decision["decision"], "fix_data")

    def test_cli_writes_json_markdown_and_prior_for_revise_prior(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            path = artifact(
                root,
                feedback={
                    "target": agent.DEFAULT_TARGET,
                    "candidate_count": 10,
                    "rejected_count": 8,
                    "passed_count": 0,
                    "best_candidate": "auto_settlement_full_depth_settlement_edge",
                    "best_reward": 1.25,
                },
            )
            output_json = root / "decision.json"
            output_md = root / "decision.md"
            output_prior = root / "llm-priors-draft.json"
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    str(path),
                    "--output-json",
                    str(output_json),
                    "--output-md",
                    str(output_md),
                    "--output-prior-json",
                    str(output_prior),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            decision = json.loads(output_json.read_text(encoding="utf-8"))
            prior = json.loads(output_prior.read_text(encoding="utf-8"))
            markdown = output_md.read_text(encoding="utf-8")
            self.assertEqual(decision["decision"], "revise_prior")
            self.assertEqual(prior["kind"], "typed_llm_prior_draft")
            self.assertEqual(prior["mutations"][0]["mutation_type"], "add_capacity_gate")
            self.assertIn("No profitability guarantee", markdown)


if __name__ == "__main__":
    unittest.main()
