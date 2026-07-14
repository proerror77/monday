import json
import tempfile
import unittest
from argparse import Namespace
from pathlib import Path
from unittest.mock import patch

from scripts.research_manager_execute_plan import (
    EXECUTE_ACK,
    build_executor_payload,
    main,
)


def plan_payload(theme: str, actions: list[str], factor_registry_summary: dict | None = None) -> dict:
    return {
        "schema_version": "research_trace_plan.v1",
        "input": {
            "market_data_health": {
                "dataset_start_ts": "2026-04-21T00:00:00Z",
                "dataset_end_ts": "2026-04-23T00:00:00Z",
            },
            "factor_registry_summary": factor_registry_summary or {},
        },
        "plan": {
            "theme": theme,
            "priority": "high",
            "evidence_stage": "factor_attribution",
            "actions": actions,
        },
    }


def base_args(**overrides):
    values = {
        "mode": "dry_run",
        "execute_ack": "",
        "git_ref": "main",
        "snapshot_run_id": "",
        "symbols": "BTCUSDT",
        "stake_usd": "15",
        "chain_remaining": 1,
        "candidate_strategy_replay_run_id": "",
        "candidate_strategy_replay_artifact_name": "",
        "full_depth_execution_surface_run_id": "",
        "full_depth_execution_surface_artifact_name": "",
        "max_snapshot_window_days": 2,
        "max_full_depth_surface_hours": 0,
        "runtime_deployment_id": "pm5d.threelayer.settlement-probability-btc-eth.dryrun",
        "runtime_config_path": "/opt/ploy/config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
        "runtime_recording_path": "/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
        "runtime_score": "",
        "runtime_strategy_profile": "settlement_probability",
        "runtime_issue_number": "",
        "runtime_min_trade_count": "50",
        "runtime_min_fill_rate": "0.30",
        "runtime_min_roi": "0",
        "runtime_source_target": "full_depth_settlement_executable_pnl",
        "runtime_source_horizon": "5m",
        "resolve_snapshot_provenance": False,
    }
    values.update(overrides)
    return Namespace(**values)


class ResearchManagerExecutePlanTest(unittest.TestCase):
    def test_workflow_passes_issue_number_to_downstream_dispatches(self) -> None:
        root = Path(__file__).resolve().parents[1]
        workflow = (root / ".github" / "workflows" / "research-manager-execute-plan.yml").read_text(
            encoding="utf-8"
        )

        self.assertIn(
            '--runtime-issue-number "${{ github.event.inputs.issue_number }}"',
            workflow,
        )

    def test_fix_data_plan_creates_snapshot_dispatch_in_dry_run(self) -> None:
        payload = build_executor_payload(
            Namespace(
                mode="dry_run",
                execute_ack="",
                git_ref="main",
                snapshot_run_id="",
                symbols="BTCUSDT,ETHUSDT",
                stake_usd="15",
                chain_remaining=1,
                max_snapshot_window_days=2,
            ),
            plan_payload("fix_data", ["rerun_snapshot_data_audit"]),
        )
        self.assertEqual("research_manager_executor.v1", payload["schema_version"])
        self.assertEqual("dry_run", payload["mode"])
        self.assertEqual(1, payload["executable_dispatch_count"])
        self.assertEqual("research-snapshot.yml", payload["dispatches"][0]["workflow"])
        self.assertEqual("2026-04-21", payload["dispatches"][0]["fields"]["start_date"])
        self.assertEqual("2026-04-23", payload["dispatches"][0]["fields"]["end_date"])
        options = json.loads(payload["dispatches"][0]["fields"]["options_json"])
        self.assertTrue(options["upload_sampled_snapshot"])
        self.assertEqual("2026-04-21T00:00:00Z", options["start_ts"])
        self.assertEqual("2026-04-23T00:00:00Z", options["end_ts"])
        self.assertNotIn("upload_full_snapshot", options)

    def test_execute_mode_requires_ack(self) -> None:
        payload = build_executor_payload(
            Namespace(
                mode="execute",
                execute_ack="wrong",
                git_ref="main",
                snapshot_run_id="123",
                symbols="BTCUSDT",
                stake_usd="15",
                chain_remaining=1,
                max_snapshot_window_days=2,
            ),
            plan_payload("continue_search", ["continue_hosted_alpha_search"]),
        )
        self.assertEqual("dry_run", payload["mode"])
        self.assertIn("missing_execute_ack", payload["blocked_dispatches"][-1]["blockers"])

    def test_continue_search_maps_to_bounded_walk_forward(self) -> None:
        payload = build_executor_payload(
            Namespace(
                mode="execute",
                execute_ack=EXECUTE_ACK,
                git_ref="main",
                snapshot_run_id="26327019766",
                symbols="BTCUSDT",
                stake_usd="15",
                chain_remaining=2,
                max_snapshot_window_days=2,
            ),
            plan_payload("continue_search", ["continue_hosted_alpha_search"]),
        )
        self.assertEqual("execute", payload["mode"])
        dispatch = payload["dispatches"][0]
        self.assertTrue(dispatch["ready"])
        self.assertEqual("factor-walk-forward-v2-hosted-artifact.yml", dispatch["workflow"])
        options = json.loads(dispatch["fields"]["options_json"])
        self.assertTrue(options["chain_next_run"])
        self.assertEqual(1, options["chain_remaining"])

    def test_revise_prior_walk_forward_carries_typed_prior_and_evidence_artifacts(self) -> None:
        plan = plan_payload(
            "revise_prior",
            ["generate_typed_llm_prior_json", "rerun_alpha_search_with_bounded_mutations"],
        )
        plan["input"]["latest_runs"] = {
            "runs": [
                {
                    "artifacts": [
                        {
                            "output_json": {
                                "candidate_strategy_replay": {
                                    "basis": "runtime_market_update_replay",
                                    "runtime_score": "autofactor_formula:prior_candidate",
                                    "strategy_profile": "settlement_probability",
                                    "metrics": {
                                        "trade_count": 53,
                                        "unique_event_count": 53,
                                        "entry_fill_rate": 1.0,
                                        "roi": -0.079091,
                                        "total_pnl": -62.8774,
                                    },
                                    "blocking_risk_flags": ["roi_too_low:-0.079091<0.000000"],
                                    "decision_contract": {
                                        "target": "tradeable_full_depth_settlement_pnl",
                                        "horizon": "5m",
                                    },
                                }
                            }
                        }
                    ]
                }
            ]
        }
        plan["plan"]["blocker_actions"] = [
            {
                "blocker_family": "strategy_economics",
                "action": "mutate_or_reject_negative_runtime_edge",
                "reason": "Latest replay or walk-forward evidence failed economic/OOS gates.",
            }
        ]
        payload = build_executor_payload(
            base_args(
                mode="execute",
                execute_ack=EXECUTE_ACK,
                snapshot_run_id="26354463118",
                full_depth_execution_surface_run_id="26351573860",
                full_depth_execution_surface_artifact_name="full-depth-execution-surface-26351573860",
                candidate_strategy_replay_run_id="26355035577",
                candidate_strategy_replay_artifact_name="runtime-candidate-replay-26355035577",
            ),
            plan,
        )

        dispatch = payload["dispatches"][0]
        self.assertEqual("factor-walk-forward-v2-hosted-artifact.yml", dispatch["workflow"])
        options = json.loads(dispatch["fields"]["options_json"])
        self.assertEqual(
            "26351573860",
            options["full_depth_execution_surface_run_id"],
        )
        self.assertEqual(
            "full-depth-execution-surface-26351573860",
            options["full_depth_execution_surface_artifact_name"],
        )
        self.assertEqual("26355035577", options["candidate_strategy_replay_run_id"])
        self.assertEqual(
            "tradeable_full_depth_settlement_pnl",
            options["alpha_search_plan_target"],
        )
        self.assertEqual("tradeable_full_depth_settlement_pnl", options["allowed_target"])
        prior = json.loads(options["alpha_search_llm_prior_json"])
        self.assertEqual("research_manager_typed_prior.v1", prior["schema_version"])
        self.assertEqual("revise_prior", prior["theme"])
        self.assertEqual(plan["plan"]["blocker_actions"], prior["blocker_actions"])
        self.assertEqual(
            [
                {
                    "base_factor": "prior_candidate",
                    "factor_family": "prior_candidate",
                    "runtime_score": "autofactor_formula:prior_candidate",
                    "reason": "negative_runtime_edge",
                    "metrics": {
                        "trade_count": 53,
                        "unique_event_count": 53,
                        "entry_fill_rate": 1.0,
                        "roi": -0.079091,
                        "total_pnl": -62.8774,
                        "blocking_risk_flags": ["roi_too_low:-0.079091<0.000000"],
                    },
                }
            ],
            prior["runtime_avoid_factors"],
        )
        self.assertTrue(prior["mutations"])
        self.assertEqual("add_capacity_gate", prior["mutations"][0]["mutation_type"])
        self.assertEqual(
            "auto_settlement_model_full_depth_settlement_edge",
            prior["mutations"][0]["base_factor"],
        )

    def test_revise_prior_prefers_recent_negative_replay_over_stale_ready_replay(self) -> None:
        stale_ready_replay = {
            "artifact_json": {
                "basis": "runtime_market_update_replay",
                "runtime_score": "autofactor_formula:stale_ready",
                "source_workflow": "runtime-candidate-replay.yml",
                "workflow_run_id": "26367311478",
                "strategy_profile": "settlement_probability",
                "metrics": {
                    "trade_count": 50,
                    "unique_event_count": 50,
                    "entry_fill_rate": 1.0,
                    "roi": 0.116538,
                    "total_pnl": 87.4035,
                },
                "decision_contract": {
                    "target": "tradeable_full_depth_settlement_pnl",
                    "horizon": "5m",
                },
                "recording_path": "/opt/ploy/data/recordings/archived.20260524T155939.ndjson",
                "recording_sha256": "abc123",
            },
            "metrics": {"roi": 0.116538},
        }
        recent_negative_replay = {
            "artifact_json": {
                "basis": "runtime_market_update_replay",
                "runtime_score": "autofactor_formula:latest_negative",
                "source_workflow": "runtime-candidate-replay.yml",
                "workflow_run_id": "26528933436",
                "strategy_profile": "settlement_probability",
                "metrics": {
                    "trade_count": 145,
                    "unique_event_count": 145,
                    "entry_fill_rate": 1.0,
                    "roi": -0.013906620311657932,
                    "total_pnl": -30.246899177856,
                },
                "blocking_risk_flags": [
                    "official_settlement_missing:142<145",
                    "roi_too_low:-0.013907<0.000000",
                ],
                "decision_contract": {
                    "target": "full_depth_settlement_executable_pnl",
                    "horizon": "5m",
                },
                "recording_path": "/opt/ploy/data/recordings/live.ndjson",
                "recording_sha256": "def456",
            },
            "metrics": {"roi": -0.013906620311657932},
        }
        plan = plan_payload(
            "revise_prior",
            ["generate_typed_llm_prior_json", "rerun_alpha_search_with_bounded_mutations"],
            {
                "ready_candidate_replays": [stale_ready_replay],
                "recent_candidate_replays": [recent_negative_replay, stale_ready_replay],
            },
        )
        plan["plan"]["blocker_actions"] = [
            {
                "blocker_family": "strategy_economics",
                "action": "mutate_or_reject_negative_runtime_edge",
                "reason": "Latest replay or walk-forward evidence failed economic/OOS gates.",
            }
        ]

        payload = build_executor_payload(
            base_args(mode="execute", execute_ack=EXECUTE_ACK, snapshot_run_id="26516561409"),
            plan,
        )

        dispatch = payload["dispatches"][0]
        options = json.loads(dispatch["fields"]["options_json"])
        self.assertEqual("26528933436", options["candidate_strategy_replay_run_id"])
        self.assertEqual(
            "runtime-candidate-replay-26528933436",
            options["candidate_strategy_replay_artifact_name"],
        )
        self.assertEqual(
            "full_depth_settlement_executable_pnl",
            options["alpha_search_plan_target"],
        )
        prior = json.loads(options["alpha_search_llm_prior_json"])
        self.assertEqual(
            [
                {
                    "base_factor": "latest_negative",
                    "factor_family": "latest_negative",
                    "runtime_score": "autofactor_formula:latest_negative",
                    "reason": "negative_runtime_edge",
                    "metrics": {
                        "trade_count": 145,
                        "unique_event_count": 145,
                        "entry_fill_rate": 1.0,
                        "roi": -0.013906620311657932,
                        "total_pnl": -30.246899177856,
                        "blocking_risk_flags": [
                            "official_settlement_missing:142<145",
                            "roi_too_low:-0.013907<0.000000",
                        ],
                    },
                }
            ],
            prior["runtime_avoid_factors"],
        )

    def test_revise_prior_chains_latest_alpha_search_plan_artifact_prior(self) -> None:
        recent_negative_replay = {
            "run_id": "26541671208",
            "artifact_json": {
                "basis": "runtime_market_update_replay",
                "runtime_score": "autofactor_formula:latest_negative",
                "source_workflow": "runtime-candidate-replay.yml",
                "workflow_run_id": "26528933436",
                "strategy_profile": "settlement_probability",
                "metrics": {
                    "trade_count": 145,
                    "unique_event_count": 145,
                    "entry_fill_rate": 1.0,
                    "roi": -0.013906620311657932,
                    "total_pnl": -30.246899177856,
                },
                "blocking_risk_flags": ["roi_too_low:-0.013907<0.000000"],
                "decision_contract": {
                    "target": "full_depth_settlement_executable_pnl",
                    "horizon": "5m",
                },
                "recording_path": "/opt/ploy/data/recordings/live.ndjson",
                "recording_sha256": "def456",
            },
        }
        plan = plan_payload(
            "revise_prior",
            ["generate_typed_llm_prior_json", "rerun_alpha_search_with_bounded_mutations"],
            {"recent_candidate_replays": [recent_negative_replay]},
        )
        plan["plan"]["blocker_actions"] = [
            {
                "blocker_family": "strategy_economics",
                "action": "mutate_or_reject_negative_runtime_edge",
                "reason": "Latest replay or walk-forward evidence failed economic/OOS gates.",
            }
        ]

        payload = build_executor_payload(
            base_args(mode="execute", execute_ack=EXECUTE_ACK, snapshot_run_id="26516561409"),
            plan,
        )

        options = json.loads(payload["dispatches"][0]["fields"]["options_json"])
        self.assertEqual("26541671208", options["alpha_search_plan_run_id"])
        self.assertEqual(
            "factor-walk-forward-v2-26541671208",
            options["alpha_search_plan_artifact_name"],
        )
        self.assertEqual("26528933436", options["candidate_strategy_replay_run_id"])
        self.assertNotIn("alpha_search_llm_prior_json", options)
        self.assertEqual(
            "research_manager_typed_prior.v1",
            payload["typed_prior"]["schema_version"],
        )

    def test_revise_prior_embeds_typed_prior_for_standalone_runtime_replay(self) -> None:
        recent_negative_replay = {
            "run_id": "26554215670",
            "workflow_run_id": "26554215670",
            "artifact_json": {
                "basis": "runtime_market_update_replay",
                "runtime_score": "autofactor_formula:latest_negative",
                "source_workflow": "runtime-candidate-replay.yml",
                "workflow_run_id": "26554215670",
                "strategy_profile": "settlement_probability",
                "metrics": {
                    "trade_count": 1,
                    "unique_event_count": 1,
                    "entry_fill_rate": 1.0,
                    "roi": -1.0038087836330667,
                    "total_pnl": -15.057131754496,
                },
                "blocking_risk_flags": [
                    "trade_count_too_small:1<50",
                    "roi_too_low:-1.003809<0.000000",
                ],
                "decision_contract": {
                    "target": "full_depth_settlement_executable_pnl",
                    "horizon": "5m",
                },
                "recording_path": "/opt/ploy/data/recordings/live.ndjson",
                "recording_sha256": "replay-hash",
            },
        }
        plan = plan_payload(
            "revise_prior",
            ["generate_typed_llm_prior_json", "rerun_alpha_search_with_bounded_mutations"],
            {"recent_candidate_replays": [recent_negative_replay]},
        )
        plan["plan"]["blocker_actions"] = [
            {
                "blocker_family": "strategy_economics",
                "action": "mutate_or_reject_negative_runtime_edge",
                "reason": "Latest replay or walk-forward evidence failed economic/OOS gates.",
            }
        ]

        payload = build_executor_payload(
            base_args(mode="execute", execute_ack=EXECUTE_ACK, snapshot_run_id="26553958344"),
            plan,
        )

        dispatch = payload["dispatches"][0]
        self.assertTrue(dispatch["ready"])
        options = json.loads(dispatch["fields"]["options_json"])
        self.assertNotIn("alpha_search_plan_run_id", options)
        self.assertNotIn("alpha_search_plan_artifact_name", options)
        self.assertEqual("26554215670", options["candidate_strategy_replay_run_id"])
        self.assertEqual(
            "runtime-candidate-replay-26554215670",
            options["candidate_strategy_replay_artifact_name"],
        )
        prior = json.loads(options["alpha_search_llm_prior_json"])
        self.assertEqual("research_manager_typed_prior.v1", prior["schema_version"])
        self.assertEqual(
            "autofactor_formula:latest_negative",
            prior["runtime_avoid_factors"][0]["runtime_score"],
        )

    def test_revise_prior_resolves_source_snapshot_from_alpha_artifact_provenance(self) -> None:
        recent_negative_replay = {
            "run_id": "26542589633",
            "artifact_json": {
                "basis": "runtime_market_update_replay",
                "runtime_score": "autofactor_formula:latest_negative",
                "source_workflow": "runtime-candidate-replay.yml",
                "workflow_run_id": "26528933436",
                "strategy_profile": "settlement_probability",
                "metrics": {
                    "trade_count": 145,
                    "unique_event_count": 145,
                    "entry_fill_rate": 1.0,
                    "roi": -0.013906620311657932,
                    "total_pnl": -30.246899177856,
                },
                "blocking_risk_flags": ["roi_too_low:-0.013907<0.000000"],
                "decision_contract": {
                    "target": "full_depth_settlement_executable_pnl",
                    "horizon": "5m",
                },
                "recording_path": "/opt/ploy/data/recordings/live.ndjson",
                "recording_sha256": "def456",
            },
        }
        plan = plan_payload(
            "revise_prior",
            ["generate_typed_llm_prior_json", "rerun_alpha_search_with_bounded_mutations"],
            {"recent_candidate_replays": [recent_negative_replay]},
        )
        plan["plan"]["blocker_actions"] = [
            {
                "blocker_family": "strategy_economics",
                "action": "mutate_or_reject_negative_runtime_edge",
                "reason": "Latest replay or walk-forward evidence failed economic/OOS gates.",
            }
        ]

        with patch(
            "scripts.research_manager_execute_plan._source_snapshot_run_id_from_alpha_artifact",
            return_value="26516561409",
        ) as resolver:
            payload = build_executor_payload(
                base_args(
                    mode="execute",
                    execute_ack=EXECUTE_ACK,
                    snapshot_run_id="26542589633",
                    resolve_snapshot_provenance=True,
                ),
                plan,
            )

        resolver.assert_called_once_with("26542589633", "factor-walk-forward-v2-26542589633")
        dispatch = payload["dispatches"][0]
        options = json.loads(dispatch["fields"]["options_json"])
        self.assertTrue(dispatch["ready"])
        self.assertEqual("26516561409", dispatch["fields"]["snapshot_run_id"])
        self.assertEqual("26542589633", options["alpha_search_plan_run_id"])
        self.assertEqual(
            "factor-walk-forward-v2-26542589633",
            options["alpha_search_plan_artifact_name"],
        )
        self.assertEqual(
            {
                "source": "alpha_search_plan_artifact_snapshot_provenance",
                "alpha_search_plan_run_id": "26542589633",
                "alpha_search_plan_artifact_name": "factor-walk-forward-v2-26542589633",
                "source_snapshot_run_id": "26516561409",
                "status": "applied",
            },
            dispatch["snapshot_resolution"],
        )

    def test_revise_prior_blocks_alpha_run_id_snapshot_when_provenance_unresolved(self) -> None:
        recent_negative_replay = {
            "run_id": "26542589633",
            "artifact_json": {
                "basis": "runtime_market_update_replay",
                "runtime_score": "autofactor_formula:latest_negative",
                "source_workflow": "runtime-candidate-replay.yml",
                "workflow_run_id": "26528933436",
                "strategy_profile": "settlement_probability",
                "recording_path": "/opt/ploy/data/recordings/live.ndjson",
                "recording_sha256": "def456",
            },
        }
        plan = plan_payload(
            "revise_prior",
            ["generate_typed_llm_prior_json", "rerun_alpha_search_with_bounded_mutations"],
            {"recent_candidate_replays": [recent_negative_replay]},
        )

        with patch(
            "scripts.research_manager_execute_plan._source_snapshot_run_id_from_alpha_artifact",
            return_value="",
        ):
            payload = build_executor_payload(
                base_args(
                    mode="execute",
                    execute_ack=EXECUTE_ACK,
                    snapshot_run_id="26542589633",
                    resolve_snapshot_provenance=True,
                ),
                plan,
            )

        dispatch = payload["dispatches"][0]
        self.assertFalse(dispatch["ready"])
        self.assertIn(
            "snapshot_run_id_points_to_alpha_search_plan_without_source_snapshot_provenance",
            dispatch["blockers"],
        )

    def test_negative_runtime_prior_normalizes_selector_threshold_family(self) -> None:
        plan = plan_payload(
            "revise_prior",
            ["generate_typed_llm_prior_json", "rerun_alpha_search_with_bounded_mutations"],
        )
        runtime_score = (
            "autofactor_formula:"
            "mut_auto_settlement_model_full_depth_settlement_edge_spread_adjusted_select_near_strike_ge_025"
        )
        plan["input"]["latest_runs"] = {
            "runs": [
                {
                    "artifacts": [
                        {
                            "output_json": {
                                "candidate_strategy_replay": {
                                    "basis": "runtime_market_update_replay",
                                    "runtime_score": runtime_score,
                                    "strategy_profile": "settlement_probability",
                                    "metrics": {"roi": -0.05},
                                }
                            }
                        }
                    ]
                }
            ]
        }
        plan["plan"]["blocker_actions"] = [
            {
                "blocker_family": "strategy_economics",
                "action": "mutate_or_reject_negative_runtime_edge",
                "reason": "Latest runtime evidence failed economic gates.",
            }
        ]

        payload = build_executor_payload(
            base_args(mode="execute", execute_ack=EXECUTE_ACK, snapshot_run_id="26516561409"),
            plan,
        )

        dispatch = payload["dispatches"][0]
        options = json.loads(dispatch["fields"]["options_json"])
        prior = json.loads(options["alpha_search_llm_prior_json"])
        self.assertEqual(
            "auto_settlement_model_full_depth_settlement_edge",
            prior["runtime_avoid_factors"][0]["factor_family"],
        )
        self.assertNotIn(
            "auto_settlement_model_full_depth_settlement_edge",
            {item["base_factor"] for item in prior["mutations"]},
        )

    def test_revise_prior_infers_latest_replay_and_full_depth_artifacts(self) -> None:
        plan = plan_payload(
            "revise_prior",
            ["generate_typed_llm_prior_json", "rerun_alpha_search_with_bounded_mutations"],
        )
        plan["input"]["latest_runs"] = {
            "runs": [
                {
                    "run_id": "26362501135",
                    "artifacts": [
                        {
                            "output_json": {
                                "candidate_strategy_replay": {
                                    "basis": "runtime_market_update_replay",
                                    "runtime_score": "autofactor_formula:prior_candidate",
                                    "source_workflow": "runtime-candidate-replay.yml",
                                    "workflow_run_id": "26355035577",
                                    "strategy_profile": "settlement_probability",
                                    "decision_contract": {
                                        "target": "tradeable_full_depth_settlement_pnl",
                                        "horizon": "5m",
                                    },
                                },
                                "source_snapshot_contract": {
                                    "full_depth_execution_surface_proofs": [
                                        {
                                            "valid": True,
                                            "surface": "clob_orderbook_snapshots",
                                            "path": "artifacts/full-depth-execution-surface/full-depth-execution-surface.json",
                                        }
                                    ],
                                    "satisfied_execution_surfaces": ["clob_orderbook_snapshots"],
                                },
                            }
                        }
                    ],
                }
            ]
        }

        payload = build_executor_payload(
            base_args(
                mode="execute",
                execute_ack=EXECUTE_ACK,
                snapshot_run_id="26354463118",
            ),
            plan,
        )

        options = json.loads(payload["dispatches"][0]["fields"]["options_json"])
        self.assertEqual("26355035577", options["candidate_strategy_replay_run_id"])
        self.assertEqual(
            "runtime-candidate-replay-26355035577",
            options["candidate_strategy_replay_artifact_name"],
        )
        self.assertEqual("26362501135", options["full_depth_execution_surface_run_id"])
        self.assertEqual(
            "factor-walk-forward-v2-26362501135",
            options["full_depth_execution_surface_artifact_name"],
        )
        self.assertEqual(
            "tradeable_full_depth_settlement_pnl",
            options["alpha_search_plan_target"],
        )
        self.assertEqual("tradeable_full_depth_settlement_pnl", options["allowed_target"])

    def test_revise_prior_ignores_mutable_replay_without_recording_hash(self) -> None:
        plan = plan_payload(
            "revise_prior",
            ["generate_typed_llm_prior_json", "rerun_alpha_search_with_bounded_mutations"],
        )
        plan["input"]["latest_runs"] = {
            "runs": [
                {
                    "run_id": "26362501135",
                    "artifacts": [
                        {
                            "output_json": {
                                "candidate_strategy_replay": {
                                    "basis": "runtime_market_update_replay",
                                    "runtime_score": "autofactor_formula:stale_candidate",
                                    "source_workflow": "runtime-candidate-replay.yml",
                                    "workflow_run_id": "26355035577",
                                    "strategy_profile": "settlement_probability",
                                    "recording_path": "/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
                                    "recording_sha256": "",
                                    "decision_contract": {
                                        "target": "tradeable_full_depth_settlement_pnl",
                                        "horizon": "5m",
                                    },
                                }
                            }
                        }
                    ],
                }
            ]
        }

        payload = build_executor_payload(
            base_args(
                mode="execute",
                execute_ack=EXECUTE_ACK,
                snapshot_run_id="26354463118",
            ),
            plan,
        )

        options = json.loads(payload["dispatches"][0]["fields"]["options_json"])
        self.assertNotIn("candidate_strategy_replay_run_id", options)
        self.assertNotIn("candidate_strategy_replay_artifact_name", options)

    def test_fix_runtime_plan_maps_to_candidate_replay_and_parity(self) -> None:
        payload = build_executor_payload(
            Namespace(
                mode="dry_run",
                execute_ack="",
                git_ref="main",
                snapshot_run_id="",
                symbols="BTCUSDT",
                stake_usd="15",
                chain_remaining=1,
                max_snapshot_window_days=2,
                runtime_deployment_id="pm5d.threelayer.settlement-probability-btc-eth.dryrun",
                runtime_config_path="/opt/ploy/config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
                runtime_recording_path="/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
                runtime_score="autofactor_formula:auto_settlement_full_depth_settlement_edge_x_near_strike",
                runtime_strategy_profile="settlement_probability",
                runtime_issue_number="538",
                runtime_min_trade_count="50",
                runtime_min_fill_rate="0.30",
                runtime_min_roi="0",
                runtime_source_target="full_depth_settlement_executable_pnl",
                runtime_source_horizon="5m",
            ),
            plan_payload(
                "fix_runtime",
                ["run_recorded_replay_parity", "compare_runtime_scorer_contract"],
            ),
        )

        self.assertEqual("dry_run", payload["mode"])
        self.assertEqual(2, payload["executable_dispatch_count"])
        workflows = [item["workflow"] for item in payload["dispatches"]]
        self.assertEqual(
            ["runtime-candidate-replay.yml", "recorded-replay-parity.yml"],
            workflows,
        )
        replay = payload["dispatches"][0]
        self.assertEqual(
            "autofactor_formula:auto_settlement_full_depth_settlement_edge_x_near_strike",
            replay["fields"]["runtime_score"],
        )
        replay_options = json.loads(replay["fields"]["options_json"])
        self.assertTrue(replay_options["full_depth_entry"])
        self.assertFalse(replay_options["skip_settlement_exits"])
        self.assertEqual("full_depth_settlement_executable_pnl", replay_options["source_target"])
        self.assertEqual("5m", replay_options["source_horizon"])

    def test_fix_runtime_blocks_candidate_replay_without_runtime_score(self) -> None:
        payload = build_executor_payload(
            Namespace(
                mode="dry_run",
                execute_ack="",
                git_ref="main",
                snapshot_run_id="",
                symbols="BTCUSDT",
                stake_usd="15",
                chain_remaining=1,
                max_snapshot_window_days=2,
                runtime_deployment_id="pm5d.threelayer.settlement-probability-btc-eth.dryrun",
                runtime_config_path="/opt/ploy/config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
                runtime_recording_path="/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
                runtime_score="",
                runtime_strategy_profile="settlement_probability",
                runtime_issue_number="538",
                runtime_min_trade_count="50",
                runtime_min_fill_rate="0.30",
                runtime_min_roi="0",
                runtime_source_target="full_depth_settlement_executable_pnl",
                runtime_source_horizon="5m",
            ),
            plan_payload("fix_runtime", ["compare_runtime_scorer_contract"]),
        )

        self.assertEqual(0, payload["executable_dispatch_count"])
        self.assertIn("missing_runtime_score", payload["blocked_dispatches"][0]["blockers"])
        self.assertIn(
            "missing_runtime_candidate_contract",
            payload["blocked_dispatches"][0]["blockers"],
        )

    def test_fix_runtime_derives_candidate_replay_score_from_plan_contract(self) -> None:
        payload = build_executor_payload(
            Namespace(
                mode="dry_run",
                execute_ack="",
                git_ref="main",
                snapshot_run_id="",
                symbols="BTCUSDT",
                stake_usd="15",
                chain_remaining=1,
                max_snapshot_window_days=2,
                runtime_deployment_id="pm5d.threelayer.settlement-probability-btc-eth.dryrun",
                runtime_config_path="/opt/ploy/config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
                runtime_recording_path="/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
                runtime_score="",
                runtime_strategy_profile="settlement_probability",
                runtime_issue_number="538",
                runtime_min_trade_count="50",
                runtime_min_fill_rate="0.30",
                runtime_min_roi="0",
                runtime_source_target="full_depth_settlement_executable_pnl",
                runtime_source_horizon="5m",
            ),
            plan_payload(
                "candidate_to_runtime_replay",
                ["build_runtime_candidate_replay"],
                {
                    "recent_factors": [
                        {
                            "factor_name": "auto_settlement_conservative_settlement_edge",
                            "status": "candidate",
                            "target": "full_depth_settlement_executable_pnl",
                            "horizon": "5m",
                            "blockers": [],
                            "runtime_contract": {
                                "version": "autofactor_runtime_contract_v1",
                                "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge",
                                "strategy_profile": "settlement_probability",
                                "target": "full_depth_settlement_executable_pnl",
                                "horizon": "5m",
                                "blockers": [],
                            },
                        }
                    ]
                },
            ),
        )

        self.assertEqual(1, payload["executable_dispatch_count"])
        dispatch = payload["dispatches"][0]
        self.assertTrue(dispatch["ready"])
        self.assertEqual("auto_settlement_conservative_settlement_edge", dispatch["selected_factor_name"])
        self.assertEqual(
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
            dispatch["fields"]["runtime_score"],
        )

    def test_runtime_replay_blocker_action_dispatches_candidate_replay(self) -> None:
        plan = plan_payload(
            "revise_prior",
            ["generate_typed_llm_prior_json", "rerun_alpha_search_with_bounded_mutations"],
            {
                "runtime_ready_candidates": [
                    {
                        "factor_name": "runtime_ready_factor",
                        "status": "candidate",
                        "target": "full_depth_settlement_executable_pnl",
                        "horizon": "5m",
                        "blockers": [],
                        "runtime_contract": {
                            "version": "autofactor_runtime_contract_v1",
                            "runtime_score": "autofactor_formula:runtime_ready_factor",
                            "strategy_profile": "settlement_probability",
                            "target": "full_depth_settlement_executable_pnl",
                            "horizon": "5m",
                            "blockers": [],
                        },
                    }
                ]
            },
        )
        plan["plan"]["blocker_actions"] = [
            {
                "blocker_family": "runtime_replay",
                "action": "build_runtime_market_update_replay",
                "reason": "Top-bucket aggregate evidence must be replaced by runtime replay.",
            }
        ]

        payload = build_executor_payload(base_args(snapshot_run_id="26516561409"), plan)

        self.assertEqual(2, payload["executable_dispatch_count"])
        replay_dispatch = next(
            item
            for item in payload["dispatches"]
            if item["workflow"] == "runtime-candidate-replay.yml"
        )
        self.assertTrue(replay_dispatch["ready"])
        self.assertEqual("runtime_ready_factor", replay_dispatch["selected_factor_name"])
        self.assertEqual(
            "autofactor_formula:runtime_ready_factor",
            replay_dispatch["fields"]["runtime_score"],
        )

    def test_fix_runtime_prefers_frontier_runtime_ready_candidates(self) -> None:
        payload = build_executor_payload(
            base_args(),
            plan_payload(
                "candidate_to_runtime_replay",
                ["build_runtime_candidate_replay"],
                {
                    "runtime_ready_candidates": [
                        {
                            "factor_name": "ready_factor",
                            "status": "candidate",
                            "target": "full_depth_settlement_executable_pnl",
                            "horizon": "5m",
                            "blockers": [],
                            "runtime_contract": {
                                "version": "autofactor_runtime_contract_v1",
                                "runtime_score": "autofactor_formula:ready_factor",
                                "strategy_profile": "settlement_probability",
                                "target": "full_depth_settlement_executable_pnl",
                                "horizon": "5m",
                                "blockers": [],
                            },
                        }
                    ],
                    "recent_factors": [
                        {
                            "factor_name": "blocked_recent_factor",
                            "status": "candidate",
                            "blockers": ["missing_runtime_contract"],
                            "runtime_contract": {
                                "version": "autofactor_runtime_contract_v1",
                                "runtime_score": "autofactor_formula:blocked_recent_factor",
                                "strategy_profile": "settlement_probability",
                                "blockers": ["missing_runtime_contract"],
                            },
                        }
                    ],
                },
            ),
        )

        self.assertEqual(1, payload["executable_dispatch_count"])
        dispatch = payload["dispatches"][0]
        self.assertTrue(dispatch["ready"])
        self.assertEqual("ready_factor", dispatch["selected_factor_name"])
        self.assertEqual(
            "autofactor_formula:ready_factor",
            dispatch["fields"]["runtime_score"],
        )

    def test_ready_handoff_maps_to_autofactor_promotion_side_effects(self) -> None:
        plan = plan_payload(
            "ready_handoff",
            ["create_dry_run_handoff_issue", "open_config_pr_from_ready_handoff"],
        )
        plan["input"]["latest_runs"] = {
            "runs": [
                {
                    "run_id": "26367562792",
                    "artifacts": [
                        {
                            "output_json": {
                                "kind": "autofactor_strategy_handoff",
                                "status": "ready",
                                "recommended_action": "create_dry_run_handoff",
                                "strategies": [
                                    {
                                        "runtime_score": (
                                            "autofactor_formula:"
                                            "mut_auto_settlement_model_full_depth_"
                                            "settlement_edge_x_capacity_spread_adjusted"
                                        ),
                                        "strategy_profile": "settlement_probability",
                                        "target": "tradeable_full_depth_settlement_pnl",
                                    }
                                ],
                                "candidate_strategy_replay": {
                                    "basis": "runtime_market_update_replay",
                                    "runtime_score": (
                                        "autofactor_formula:"
                                        "mut_auto_settlement_model_full_depth_"
                                        "settlement_edge_x_capacity_spread_adjusted"
                                    ),
                                    "strategy_profile": "settlement_probability",
                                    "decision_contract": {
                                        "target": "tradeable_full_depth_settlement_pnl",
                                        "horizon": "5m",
                                    },
                                },
                            }
                        }
                    ],
                }
            ]
        }

        payload = build_executor_payload(
            base_args(mode="execute", execute_ack=EXECUTE_ACK),
            plan,
        )

        self.assertEqual("execute", payload["mode"])
        self.assertEqual(1, payload["executable_dispatch_count"])
        dispatch = payload["dispatches"][0]
        self.assertTrue(dispatch["ready"])
        self.assertEqual("autofactor-strategy-promotion.yml", dispatch["workflow"])
        self.assertEqual("26367562792", dispatch["fields"]["factor_walk_forward_run_id"])
        self.assertEqual(
            "factor-walk-forward-v2-26367562792",
            dispatch["fields"]["artifact_name"],
        )
        self.assertEqual(
            "tradeable_full_depth_settlement_pnl",
            dispatch["fields"]["allowed_target"],
        )
        self.assertEqual(
            "settlement_probability",
            dispatch["fields"]["required_strategy_profile"],
        )
        self.assertEqual("true", dispatch["fields"]["create_handoff_issue"])
        self.assertEqual("true", dispatch["fields"]["create_config_pr"])
        self.assertEqual("true", dispatch["fields"]["fail_if_blocked"])
        self.assertEqual(
            "config/strategies/02-pm5d-threelayer.settlement-probability-btc-eth-dryrun.toml",
            dispatch["fields"]["strategy_config"],
        )

    def test_ready_handoff_can_come_from_frontier_summary(self) -> None:
        plan = plan_payload(
            "ready_handoff",
            ["create_dry_run_handoff_issue", "open_config_pr_from_ready_handoff"],
        )
        plan["input"]["latest_runs"] = {
            "runs": [],
            "ready_handoffs": [
                {
                    "run_id": "26377165132",
                    "event_type": "strategy_handoff",
                    "output_json": {
                        "kind": "autofactor_strategy_handoff",
                        "status": "ready",
                        "recommended_action": "create_dry_run_handoff",
                        "strategies": [
                            {
                                "runtime_score": "autofactor_formula:ready_factor",
                                "strategy_profile": "settlement_probability",
                                "target": "full_depth_settlement_executable_pnl",
                            }
                        ],
                        "candidate_strategy_replay": {
                            "basis": "runtime_market_update_replay",
                            "runtime_score": "autofactor_formula:ready_factor",
                            "strategy_profile": "settlement_probability",
                        },
                    },
                }
            ],
        }

        payload = build_executor_payload(
            base_args(mode="execute", execute_ack=EXECUTE_ACK),
            plan,
        )

        self.assertEqual(1, payload["executable_dispatch_count"])
        dispatch = payload["dispatches"][0]
        self.assertTrue(dispatch["ready"])
        self.assertEqual("autofactor-strategy-promotion.yml", dispatch["workflow"])
        self.assertEqual("26377165132", dispatch["fields"]["factor_walk_forward_run_id"])
        self.assertEqual(
            "factor-walk-forward-v2-26377165132",
            dispatch["fields"]["artifact_name"],
        )
        self.assertEqual(
            "autofactor_formula:ready_factor",
            dispatch["runtime_score"],
        )

    def test_ready_handoff_blocks_without_ready_source_artifact(self) -> None:
        payload = build_executor_payload(
            base_args(mode="execute", execute_ack=EXECUTE_ACK),
            plan_payload(
                "ready_handoff",
                ["create_dry_run_handoff_issue", "open_config_pr_from_ready_handoff"],
            ),
        )

        self.assertEqual(0, payload["executable_dispatch_count"])
        self.assertEqual("autofactor-strategy-promotion.yml", payload["dispatches"][0]["workflow"])
        self.assertIn(
            "missing_ready_autofactor_handoff",
            payload["blocked_dispatches"][0]["blockers"],
        )

    def test_cli_writes_executor_artifacts(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            plan = root / "plan.json"
            out = root / "out"
            plan.write_text(json.dumps(plan_payload("revise_prior", ["generate_typed_llm_prior_json"])))
            import sys

            argv = sys.argv
            try:
                sys.argv = [
                    "research_manager_execute_plan.py",
                    "--plan-json",
                    str(plan),
                    "--output-dir",
                    str(out),
                ]
                main()
            finally:
                sys.argv = argv
            self.assertTrue((out / "research-manager-executor.json").exists())
            self.assertTrue((out / "research-manager-executor.md").exists())
            self.assertTrue((out / "next-llm-prior.json").exists())

    def test_typed_prior_carries_machine_readable_blocker_actions(self) -> None:
        plan = plan_payload("revise_prior", ["generate_typed_llm_prior_json"])
        plan["plan"]["blocker_actions"] = [
            {
                "blocker_family": "search_power",
                "action": "increase_distinct_event_coverage_or_reduce_selectivity",
                "reason": "Runtime replay did not produce enough distinct event trades.",
            },
            {
                "blocker_family": "execution_fillability",
                "action": "prefer_high_fillability_depth_filters",
                "reason": "Candidate selected low-fillability entries.",
            },
        ]
        payload = build_executor_payload(base_args(), plan)

        prior = payload["typed_prior"]
        self.assertIsNotNone(prior)
        self.assertEqual(plan["plan"]["blocker_actions"], prior["blocker_actions"])
        self.assertIn(
            "prefer candidates with broader distinct-event coverage and avoid ultra-narrow buckets",
            prior["constraints"],
        )
        self.assertIn(
            "prefer candidates with stronger full-depth fillability and capacity filters",
            prior["constraints"],
        )

    def test_fix_data_plan_with_blocker_actions_generates_typed_prior(self) -> None:
        plan = plan_payload("fix_data", ["collect_full_depth_execution_surface"])
        plan["plan"]["blocker_actions"] = [
            {
                "blocker_family": "promotion_data_execution_surface",
                "action": "collect_full_depth_execution_surface",
                "reason": "Research snapshot data health reports sampled execution surface.",
            },
            {
                "blocker_family": "promotion_data_settlement",
                "action": "repair_official_settlement_coverage",
                "reason": "Official settlement labels are required for promotion.",
            },
        ]
        payload = build_executor_payload(base_args(), plan)

        prior = payload["typed_prior"]
        self.assertIsNotNone(prior)
        self.assertEqual("fix_data", prior["theme"])
        self.assertEqual(plan["plan"]["blocker_actions"], prior["blocker_actions"])
        self.assertIn(
            "block promotion until full-depth execution-surface evidence replaces sampled snapshots",
            prior["constraints"],
        )
        self.assertIn(
            "block promotion until official settlement coverage exists for all replay-traded events",
            prior["constraints"],
        )

    def test_blocker_actions_map_to_full_depth_and_settlement_followups(self) -> None:
        plan = plan_payload("fix_data", ["rerun_snapshot_data_audit"])
        plan["plan"]["blocker_actions"] = [
            {
                "blocker_family": "promotion_data_execution_surface",
                "action": "collect_full_depth_execution_surface",
                "reason": "Research snapshot data health reports sampled execution surface.",
            },
            {
                "blocker_family": "data_settlement",
                "action": "repair_official_settlement_coverage",
                "reason": "Runtime replay traded events are missing official settlement labels.",
            },
        ]
        payload = build_executor_payload(base_args(), plan)

        workflows = [item["workflow"] for item in payload["dispatches"]]
        self.assertEqual(
            [
                "research-snapshot.yml",
                "collect-full-depth-execution-surface.yml",
                "repair-official-settlement-coverage.yml",
            ],
            workflows,
        )
        self.assertTrue(payload["dispatches"][1]["ready"])
        full_depth_options = json.loads(payload["dispatches"][1]["fields"]["options_json"])
        self.assertEqual("2026-04-21T00:00:00Z", full_depth_options["start_ts"])
        self.assertEqual("2026-04-23T00:00:00Z", full_depth_options["end_ts"])
        self.assertEqual(48, full_depth_options["max_hours"])
        self.assertTrue(full_depth_options["fail_if_incomplete"])
        self.assertTrue(payload["dispatches"][2]["ready"])
        settlement_options = json.loads(payload["dispatches"][2]["fields"]["options_json"])
        self.assertEqual("dry_run", settlement_options["mode"])
        self.assertEqual("", payload["dispatches"][2]["fields"]["execute_ack"])
        self.assertEqual("2026-04-21T00:00:00Z", settlement_options["start_ts"])
        self.assertEqual("2026-04-23T00:00:00Z", settlement_options["end_ts"])

    def test_execute_mode_passes_execute_to_settlement_repair_after_ack(self) -> None:
        plan = plan_payload("fix_data", ["repair_official_settlement_coverage"])
        payload = build_executor_payload(
            base_args(mode="execute", execute_ack=EXECUTE_ACK),
            plan,
        )

        self.assertEqual("execute", payload["mode"])
        self.assertEqual("repair-official-settlement-coverage.yml", payload["dispatches"][0]["workflow"])
        settlement_options = json.loads(payload["dispatches"][0]["fields"]["options_json"])
        self.assertEqual("execute", settlement_options["mode"])
        self.assertEqual(
            "repair-official-settlement-coverage",
            payload["dispatches"][0]["fields"]["execute_ack"],
        )

    def test_full_depth_action_without_snapshot_rerun_does_not_dispatch_sampled_snapshot(self) -> None:
        payload = build_executor_payload(
            base_args(),
            plan_payload("fix_data", ["collect_full_depth_execution_surface"]),
        )

        self.assertEqual(1, len(payload["dispatches"]))
        self.assertEqual(
            "collect-full-depth-execution-surface.yml",
            payload["dispatches"][0]["workflow"],
        )

    def test_full_depth_collection_can_be_explicitly_bounded_for_diagnostics(self) -> None:
        payload = build_executor_payload(
            base_args(max_full_depth_surface_hours=12),
            plan_payload("fix_data", ["collect_full_depth_execution_surface"]),
        )

        options = json.loads(payload["dispatches"][0]["fields"]["options_json"])
        self.assertEqual("2026-04-21T00:00:00Z", options["start_ts"])
        self.assertEqual("2026-04-21T12:00:00Z", options["end_ts"])
        self.assertEqual(12, options["max_hours"])
        self.assertTrue(options["fail_if_incomplete"])

    def test_cli_records_dispatch_failures_without_dropping_executor_artifact(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            plan = root / "plan.json"
            out = root / "out"
            plan.write_text(
                json.dumps(
                    plan_payload(
                        "fix_runtime",
                        ["run_recorded_replay_parity", "compare_runtime_scorer_contract"],
                    )
                )
            )
            import sys

            argv = sys.argv
            attempts = [
                {"workflow": "runtime-candidate-replay.yml", "ok": True, "returncode": 0},
                {"workflow": "recorded-replay-parity.yml", "ok": False, "returncode": 1},
            ]
            try:
                sys.argv = [
                    "research_manager_execute_plan.py",
                    "--plan-json",
                    str(plan),
                    "--output-dir",
                    str(out),
                    "--mode",
                    "execute",
                    "--execute-ack",
                    EXECUTE_ACK,
                    "--runtime-score",
                    "autofactor_formula:auto_settlement_conservative_settlement_edge",
                ]
                with patch(
                    "scripts.research_manager_execute_plan._dispatch_gh_workflow",
                    side_effect=attempts,
                ):
                    main()
            finally:
                sys.argv = argv

            payload = json.loads((out / "research-manager-executor.json").read_text())
            self.assertEqual(2, len(payload["dispatch_attempts"]))
            self.assertTrue(payload["dispatch_attempts"][0]["ok"])
            self.assertFalse(payload["dispatch_attempts"][1]["ok"])
            self.assertTrue((out / "research-manager-executor.md").exists())

    def test_fix_data_snapshot_dispatch_caps_large_dataset_window(self) -> None:
        payload = build_executor_payload(
            Namespace(
                mode="dry_run",
                execute_ack="",
                git_ref="main",
                snapshot_run_id="",
                symbols="BTCUSDT,ETHUSDT,SOLUSDT",
                stake_usd="15",
                chain_remaining=1,
                max_snapshot_window_days=2,
            ),
            {
                "schema_version": "research_trace_plan.v1",
                "input": {
                    "market_data_health": {
                        "dataset_start_ts": "2026-05-16T00:00:00Z",
                        "dataset_end_ts": "2026-05-21T00:00:00Z",
                    }
                },
                "plan": {
                    "theme": "fix_data",
                    "priority": "high",
                    "evidence_stage": "factor_attribution",
                    "actions": ["rerun_snapshot_data_audit"],
                },
            },
        )

        dispatch = payload["dispatches"][0]
        options = json.loads(dispatch["fields"]["options_json"])
        self.assertEqual("2026-05-16", dispatch["fields"]["start_date"])
        self.assertEqual("2026-05-18", dispatch["fields"]["end_date"])
        self.assertEqual("2026-05-16T00:00:00Z", options["start_ts"])
        self.assertEqual("2026-05-18T00:00:00Z", options["end_ts"])
        self.assertTrue(dispatch["bounded_window"]["truncated"])


if __name__ == "__main__":
    unittest.main()
