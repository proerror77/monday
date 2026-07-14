import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from tests.test_autofactor_strategy_promotion import (
    AUTOFACTOR_REPORT,
    AUTOFACTOR_LLM_RUNTIME_PASS_THROUGH_MUTATION_REPORT,
    AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
    SAMPLED_EXECUTION_SNAPSHOT_MANIFEST,
    VALID_FULL_DEPTH_EXECUTION_SURFACE,
    ZERO_COVERAGE_DATA_AUDIT,
)


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "build_autofactor_candidate_strategy_replay.py"


class BuildAutoFactorCandidateStrategyReplayTests(unittest.TestCase):
    def run_script(
        self,
        report: str,
        *extra_args: str,
        registry_preview_payload: dict | None = None,
        snapshot_manifest_payload: dict | None = None,
        snapshot_data_audit_payload: dict | None = None,
        full_depth_execution_surface_payload: dict | None = None,
    ) -> tuple[dict, str]:
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            report_path = tmp_path / "report.txt"
            output_json = tmp_path / "candidate-strategy-replay.json"
            output_md = tmp_path / "candidate-strategy-replay.md"
            report_path.write_text(report, encoding="utf-8")
            registry_args = []
            if registry_preview_payload is not None:
                registry_path = tmp_path / "factor-registry-preview.json"
                registry_path.write_text(
                    json.dumps(registry_preview_payload, indent=2, sort_keys=True),
                    encoding="utf-8",
                )
                registry_args = [
                    "--factor-registry-preview-json",
                    str(registry_path),
                    "--require-runtime-contract",
                ]
            snapshot_args = []
            if snapshot_manifest_payload is not None:
                snapshot_manifest_path = tmp_path / "manifest.json"
                snapshot_manifest_path.write_text(
                    json.dumps(snapshot_manifest_payload, indent=2, sort_keys=True),
                    encoding="utf-8",
                )
                snapshot_args = ["--snapshot-manifest-json", str(snapshot_manifest_path)]
            data_audit_args = []
            if snapshot_data_audit_payload is not None:
                data_audit_path = tmp_path / "data-gap-audit.json"
                data_audit_path.write_text(
                    json.dumps(snapshot_data_audit_payload, indent=2, sort_keys=True),
                    encoding="utf-8",
                )
                data_audit_args = ["--snapshot-data-audit-json", str(data_audit_path)]
            execution_surface_args = []
            if full_depth_execution_surface_payload is not None:
                execution_surface_path = tmp_path / "full-depth-execution-surface.json"
                execution_surface_path.write_text(
                    json.dumps(
                        full_depth_execution_surface_payload, indent=2, sort_keys=True
                    ),
                    encoding="utf-8",
                )
                execution_surface_args = [
                    "--full-depth-execution-surface-json",
                    str(execution_surface_path),
                ]
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--report",
                    str(report_path),
                    "--output-json",
                    str(output_json),
                    "--output-md",
                    str(output_md),
                    *registry_args,
                    *snapshot_args,
                    *data_audit_args,
                    *execution_surface_args,
                    *extra_args,
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            return (
                json.loads(output_json.read_text(encoding="utf-8")),
                output_md.read_text(encoding="utf-8"),
            )

    def test_registry_contract_blocks_name_inference_when_missing(self):
        payload, _ = self.run_script(
            AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
            registry_preview_payload={
                "version": "alpha_search_artifacts_v1",
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
                "factors": [],
            },
        )

        self.assertFalse(payload["promotion_ready"])
        self.assertEqual(payload["runtime_score"], "")
        self.assertIn(
            "missing_runtime_contract:auto_settlement_conservative_settlement_edge",
            payload["blocking_risk_flags"],
        )

    def test_registry_contract_blocks_unsupported_contract_version(self):
        payload, _ = self.run_script(
            AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
            registry_preview_payload={
                "version": "alpha_search_artifacts_v1",
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
                "factors": [
                    {
                        "factor_name": "auto_settlement_conservative_settlement_edge",
                        "runtime_contract": {
                            "version": "legacy_runtime_contract",
                            "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge",
                            "strategy_profile": "settlement_probability",
                            "strategy_family": "settlement_probability",
                            "input_names": ["conservative_settlement_edge"],
                            "blockers": [],
                        },
                        "blockers": [],
                    }
                ],
            },
        )

        self.assertFalse(payload["promotion_ready"])
        self.assertEqual(payload["runtime_score"], "")
        self.assertIn("unsupported_runtime_contract_version", payload["blocking_risk_flags"])

    def test_registry_contract_blocks_noncanonical_runtime_input(self):
        report = AUTOFACTOR_SETTLEMENT_AUTO_REPORT.replace(
            "auto_settlement_conservative_settlement_edge,full_depth_settlement_executable_pnl",
            "auto_settlement_conservative_settlement_edge_x_iv_change,full_depth_settlement_executable_pnl",
        )
        payload, _ = self.run_script(
            report,
            registry_preview_payload={
                "version": "alpha_search_artifacts_v1",
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
                "factors": [
                    {
                        "factor_name": "auto_settlement_conservative_settlement_edge_x_iv_change",
                        "runtime_contract": {
                            "version": "autofactor_runtime_contract_v1",
                            "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge_x_iv_change",
                            "strategy_profile": "settlement_probability",
                            "strategy_family": "settlement_probability",
                            "input_names": [
                                "conservative_settlement_edge",
                                "iv_change_1m",
                            ],
                            "blockers": ["runtime_input_not_supplied:iv_change_1m"],
                        },
                        "blockers": [],
                    }
                ],
            },
        )

        self.assertFalse(payload["promotion_ready"])
        self.assertEqual(payload["runtime_score"], "")
        self.assertIn(
            "runtime_input_not_supplied:iv_change_1m",
            payload["blocking_risk_flags"],
        )

    def test_registry_contract_prefers_canonical_runtime_inputs(self):
        report = """# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,auto_settlement_model_full_depth_settlement_edge_x_near_strike_x_capacity,full_depth_settlement_executable_pnl,candidate,passed,49831,0.110842,0.150273,43,1.064178,0.9535,6,0.8333,1.0000,9966,2.666226,0.6836,0.9000,9966,1,5
"""
        runtime_score = (
            "autofactor_formula:"
            "auto_settlement_model_full_depth_settlement_edge_x_near_strike_x_capacity"
        )
        payload, _ = self.run_script(
            report,
            registry_preview_payload={
                "version": "alpha_search_artifacts_v1",
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
                "factors": [
                    {
                        "factor_name": (
                            "auto_settlement_model_full_depth_settlement_edge_x_near_strike_x_capacity"
                        ),
                        "runtime_contract": {
                            "version": "autofactor_runtime_contract_v1",
                            "runtime_score": runtime_score,
                            "strategy_profile": "settlement_probability",
                            "strategy_family": "settlement_probability",
                            "input_names": [
                                "entry_capacity_score",
                                "model_full_depth_settlement_edge",
                                "near_strike_score",
                            ],
                            "runtime_input_names": [
                                "direction_sign",
                                "distance_over_sigma",
                                "entry_capacity_ratio",
                                "settlement_edge",
                            ],
                            "blockers": [],
                        },
                        "blockers": [],
                    }
                ],
            },
        )

        self.assertEqual(payload["runtime_score"], runtime_score)
        self.assertIn(
            "requires_runtime_replay_not_top_bucket_aggregate",
            payload["blocking_risk_flags"],
        )
        self.assertNotIn(
            "runtime_input_unsupported:near_strike_score",
            payload["blocking_risk_flags"],
        )

    def test_builds_blocked_aggregate_for_runtime_mappable_settlement_candidate(self):
        payload, markdown = self.run_script(AUTOFACTOR_SETTLEMENT_AUTO_REPORT)

        self.assertFalse(payload["promotion_ready"])
        self.assertEqual(
            payload["runtime_score"],
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
        )
        self.assertRegex(payload["candidate_replay_id"], r"^candidate_replay:[0-9a-f]{32}$")
        self.assertEqual(payload["evidence_stage"], "diagnostic")
        self.assertEqual(payload["basis"], "factor_walk_forward_top_bucket_aggregate")
        self.assertEqual(payload["strategy_profile"], "settlement_probability")
        self.assertEqual(payload["promotion_decision"], "blocked")
        self.assertEqual(payload["identity"]["basis"], "factor_walk_forward_top_bucket_aggregate")
        self.assertTrue(payload["decision_contract"]["event_level"])
        self.assertTrue(payload["decision_contract"]["one_decision_per_event"])
        self.assertTrue(payload["decision_contract"]["official_settlement"])
        self.assertTrue(payload["decision_contract"]["full_depth_entry"])
        self.assertEqual(payload["source_factor"]["horizon"], "5m")
        self.assertEqual(
            payload["decision_contract"]["target"],
            "full_depth_settlement_executable_pnl",
        )
        self.assertEqual(payload["decision_contract"]["horizon"], "5m")
        self.assertEqual(payload["metrics"]["trade_count"], 9966)
        self.assertEqual(payload["metrics"]["unique_event_count"], 9966)
        self.assertGreater(payload["metrics"]["total_pnl"], 0)
        self.assertGreater(payload["metrics"]["roi"], 0)
        self.assertIn(
            "requires_runtime_replay_not_top_bucket_aggregate",
            payload["blocking_risk_flags"],
        )
        self.assertIn("Promotion ready: `false`", markdown)

    def test_sampled_execution_snapshot_adds_blocking_risk_flag(self):
        payload, _ = self.run_script(
            AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
            snapshot_manifest_payload=SAMPLED_EXECUTION_SNAPSHOT_MANIFEST,
        )

        self.assertFalse(payload["promotion_ready"])
        self.assertEqual(
            payload["source_snapshot_contract"]["blocking_risk_flags"],
            ["sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots"],
        )
        self.assertIn(
            "sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots",
            payload["blocking_risk_flags"],
        )

    def test_full_depth_execution_surface_proof_removes_sampled_snapshot_flag(self):
        payload, _ = self.run_script(
            AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
            snapshot_manifest_payload=SAMPLED_EXECUTION_SNAPSHOT_MANIFEST,
            full_depth_execution_surface_payload=VALID_FULL_DEPTH_EXECUTION_SURFACE,
        )

        self.assertFalse(payload["promotion_ready"])
        self.assertEqual(payload["source_snapshot_contract"]["blocking_risk_flags"], [])
        self.assertNotIn(
            "sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots",
            payload["blocking_risk_flags"],
        )
        self.assertIn(
            "requires_runtime_replay_not_top_bucket_aggregate",
            payload["blocking_risk_flags"],
        )

    def test_zero_coverage_data_audit_blocks_candidate_replay_contract(self):
        payload, _ = self.run_script(
            AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
            snapshot_manifest_payload=SAMPLED_EXECUTION_SNAPSHOT_MANIFEST,
            snapshot_data_audit_payload=ZERO_COVERAGE_DATA_AUDIT,
            full_depth_execution_surface_payload=VALID_FULL_DEPTH_EXECUTION_SURFACE,
        )

        expected = "data_audit_zero_coverage:polymarket_orderbooks:0<288"
        self.assertFalse(payload["promotion_ready"])
        self.assertIn(expected, payload["source_snapshot_contract"]["blocking_risk_flags"])
        self.assertIn(expected, payload["blocking_risk_flags"])

    def test_invalid_full_depth_execution_surface_proof_remains_blocked(self):
        proof = dict(VALID_FULL_DEPTH_EXECUTION_SURFACE)
        proof["row_count"] = 0
        payload, _ = self.run_script(
            AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
            snapshot_manifest_payload=SAMPLED_EXECUTION_SNAPSHOT_MANIFEST,
            full_depth_execution_surface_payload=proof,
        )

        self.assertIn(
            "sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots",
            payload["blocking_risk_flags"],
        )
        self.assertIn(
            "full_depth_execution_surface_invalid:clob_orderbook_snapshots:row_count_empty",
            payload["blocking_risk_flags"],
        )

    def test_blocks_when_only_candidate_is_wrong_profile(self):
        payload, markdown = self.run_script(
            AUTOFACTOR_REPORT,
            "--allowed-target",
            "full_depth_reprice_pnl_10s",
            "--required-strategy-profile",
            "settlement_probability",
        )

        self.assertFalse(payload["promotion_ready"])
        self.assertEqual(payload["runtime_score"], "")
        self.assertIn("runtime_profile_mismatch", ",".join(payload["blocking_risk_flags"]))
        self.assertIn("Promotion ready: `false`", markdown)

    def test_ignores_unmapped_mutation_even_when_aggregate_pnl_is_higher(self):
        report = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=true stake_usd=15.00 min_entry_fill_rate=0.3000 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=100 event_complete_rows=200 replay_parity_ready=false
gate,passed,evidence
recorded_replay_parity,false,post-dry-run gate pending

# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_avg_entry_sweep_slip_bps,top_bucket_avg_entry_sweep_levels,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,mut_auto_settlement_full_depth_settlement_edge_x_near_strike_near_strike,full_depth_settlement_executable_pnl,candidate,passed,528,0.300000,0.250000,4,6.400000,1.0000,2,1.0000,1.0000,106,6.313282,0.7075,1.0000,0.00,1.40,106,1,5
2,auto_settlement_full_depth_settlement_edge_x_near_strike,full_depth_settlement_executable_pnl,candidate,passed,528,0.288889,0.254860,4,5.260247,1.0000,2,1.0000,1.0000,106,6.012908,0.6981,1.0000,0.00,1.42,106,1,3
"""
        payload, _ = self.run_script(report)

        self.assertFalse(payload["promotion_ready"])
        self.assertEqual(
            payload["runtime_score"],
            "autofactor_formula:auto_settlement_full_depth_settlement_edge_x_near_strike",
        )
        self.assertEqual(
            payload["source_factor"]["name"],
            "auto_settlement_full_depth_settlement_edge_x_near_strike",
        )

    def test_selects_runtime_mappable_predictive_formula_mutation(self):
        report = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=false stake_usd=15.00 min_entry_fill_rate=0.3000 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=100 event_complete_rows=200 replay_parity_ready=false
gate,passed,evidence
recorded_replay_parity,false,post-dry-run gate pending

# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_avg_entry_sweep_slip_bps,top_bucket_avg_entry_sweep_levels,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted,full_depth_settlement_executable_pnl,candidate,passed,529,0.201235,0.117332,4,1.668090,1.0000,2,1.0000,0.7500,106,4.083066,0.6981,1.0000,18.51,1.42,106,1,8
2,amplitude_weighted_momentum_30s_sigma,full_depth_settlement_executable_pnl,candidate,passed,529,0.165183,0.100246,4,1.474733,1.0000,2,1.0000,1.0000,106,2.366817,0.6321,1.0000,44.93,1.46,106,1,4
"""
        payload, _ = self.run_script(report)

        self.assertEqual(
            payload["runtime_score"],
            "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
        )
        self.assertEqual(
            payload["source_factor"]["name"],
            "mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
        )

    def test_prefers_selector_gate_candidate_over_higher_top_bucket_label(self):
        report = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=false stake_usd=15.00 min_entry_fill_rate=0.3000 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=100 event_complete_rows=200 replay_parity_ready=false
gate,passed,evidence
recorded_replay_parity,false,post-dry-run gate pending

# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_avg_entry_sweep_slip_bps,top_bucket_avg_entry_sweep_levels,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,mut_spread_adjusted_external_move_select_near_strike_ge_075,full_depth_settlement_executable_pnl,candidate,passed,1226,0.072616,0.051222,8,2.161689,1.0000,2,1.0000,0.7500,246,3.060470,0.6707,1.0000,12.32,1.46,246,1,7
2,mut_spread_adjusted_external_move_spread_adjusted,full_depth_settlement_executable_pnl,candidate,passed,1249,0.140001,0.082941,8,0.994307,0.8750,2,1.0000,0.7500,250,3.369123,0.7080,1.0000,11.41,1.41,250,1,9
"""
        payload, _ = self.run_script(report)

        self.assertEqual(
            payload["runtime_score"],
            "autofactor_formula:mut_spread_adjusted_external_move_select_near_strike_ge_075",
        )
        self.assertEqual(
            payload["source_factor"]["name"],
            "mut_spread_adjusted_external_move_select_near_strike_ge_075",
        )

    def test_selects_llm_runtime_pass_through_predictive_formula_mutation(self):
        payload, _ = self.run_script(AUTOFACTOR_LLM_RUNTIME_PASS_THROUGH_MUTATION_REPORT)

        self.assertEqual(
            payload["runtime_score"],
            "autofactor_formula:"
            "llm_mut_spread_adjusted_external_move_near_strike_runtime_pass_through_add_spread_penalty",
        )
        self.assertEqual(
            payload["source_factor"]["name"],
            "llm_mut_spread_adjusted_external_move_near_strike_runtime_pass_through_add_spread_penalty",
        )

    def test_selects_poly_lag_pressure_predictive_formula_mutation(self):
        report = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=false stake_usd=15.00 min_entry_fill_rate=0.3000 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=100 event_complete_rows=200 replay_parity_ready=false
gate,passed,evidence
recorded_replay_parity,false,post-dry-run gate pending

# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_avg_entry_sweep_slip_bps,top_bucket_avg_entry_sweep_levels,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,mut_poly_lag_pressure_spread_adjusted,full_depth_settlement_executable_pnl,candidate,passed,529,0.181235,0.107332,4,1.568090,1.0000,2,1.0000,0.7500,106,3.083066,0.6681,1.0000,18.51,1.42,106,1,8
"""
        payload, _ = self.run_script(report)

        self.assertEqual(
            payload["runtime_score"],
            "autofactor_formula:mut_poly_lag_pressure_spread_adjusted",
        )
        self.assertEqual(
            payload["source_factor"]["name"],
            "mut_poly_lag_pressure_spread_adjusted",
        )
        self.assertEqual(payload["strategy_profile"], "settlement_probability")

    def test_selects_composed_settlement_model_formula_mutation(self):
        report = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=false stake_usd=15.00 min_entry_fill_rate=0.3000 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=100 event_complete_rows=200 replay_parity_ready=false
gate,passed,evidence
recorded_replay_parity,false,post-dry-run gate pending

# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_avg_entry_sweep_slip_bps,top_bucket_avg_entry_sweep_levels,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted,full_depth_settlement_executable_pnl,candidate,passed,529,0.181235,0.107332,4,1.568090,1.0000,2,1.0000,0.7500,106,3.083066,0.6681,1.0000,18.51,1.42,106,1,8
"""
        payload, _ = self.run_script(report)

        self.assertEqual(
            payload["runtime_score"],
            "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted",
        )
        self.assertEqual(
            payload["source_factor"]["name"],
            "mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted",
        )
        self.assertEqual(payload["strategy_profile"], "settlement_probability")

    def test_keeps_bare_spread_adjusted_external_move_in_repricing_lane(self):
        payload, _ = self.run_script(
            AUTOFACTOR_REPORT,
            "--allowed-target",
            "full_depth_settlement_executable_pnl",
            "--required-strategy-profile",
            "settlement_probability",
        )

        self.assertFalse(payload["promotion_ready"])
        self.assertEqual(payload["runtime_score"], "")
        self.assertIn("runtime_profile_mismatch", ",".join(payload["blocking_risk_flags"]))


if __name__ == "__main__":
    unittest.main()
