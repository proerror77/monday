import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "evaluate_autofactor_strategy_promotion.py"


READY_GATE = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=true stake_usd=15.00 min_entry_fill_rate=0.0500 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=2488 event_complete_rows=51989 replay_parity_ready=true
gate,passed,evidence
data_quality,true,mode=event_complete event_complete_events=2488 event_complete_rows=51989
deribit_vol_surface,true,require_deribit=false include_deribit=false
recorded_replay_parity,true,blocking_flags=<none>
"""

LOW_SLIPPAGE_HEALTH = """=== Factor Walk-Forward V2 Data Health ===
source_obs=1000 v2_rows=2000 settlement_labels=2000 executable_pnl_rows=1500 deribit_rows=0
entry_fill_rate=80.00% rejection_rate=20.00% exit_fill_rate=75.00% avg_pm_lag_secs=1.20
full_depth_entry_fill_rate=95.00% full_depth_exit_fill_rate=90.00% full_depth_pnl_rows=1900 avg_entry_sweep_slip_bps=80.00 avg_exit_sweep_slip_bps=40.00
"""

HIGH_SLIPPAGE_HEALTH = """=== Factor Walk-Forward V2 Data Health ===
source_obs=1000 v2_rows=2000 settlement_labels=2000 executable_pnl_rows=1500 deribit_rows=0
entry_fill_rate=80.00% rejection_rate=20.00% exit_fill_rate=75.00% avg_pm_lag_secs=1.20
full_depth_entry_fill_rate=95.00% full_depth_exit_fill_rate=90.00% full_depth_pnl_rows=1900 avg_entry_sweep_slip_bps=1912.92 avg_exit_sweep_slip_bps=40.00
"""

BLOCKED_GATE = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=false stake_usd=15.00 min_entry_fill_rate=0.0500 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=0 event_complete_rows=0 replay_parity_ready=false
gate,passed,evidence
data_quality,false,mode=event_complete event_complete_events=0 event_complete_rows=0
deribit_vol_surface,true,require_deribit=false include_deribit=false
recorded_replay_parity,false,missing replay parity
"""

MODEL_BLOCKED_GATE = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=false stake_usd=15.00 min_entry_fill_rate=0.0500 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=true include_deribit=true data_quality_mode=event_complete event_complete_events=292 event_complete_rows=468 replay_parity_ready=true
gate,passed,evidence
data_quality,true,mode=event_complete event_complete_events=292 event_complete_rows=468
deribit_vol_surface,true,require_deribit=true include_deribit=true
recorded_replay_parity,true,blocking_flags=<none>
symbol_holdout,false,no non-naive model passes all symbol holdouts
walk_forward_oos,false,no non-naive model has non-empty OOS windows with positive_window_ratio >= 0.60
"""

HARD_GATE_REPLAY_BLOCKED_GATE = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=false stake_usd=15.00 min_entry_fill_rate=0.3000 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=739 event_complete_rows=2846 replay_parity_ready=false
gate,passed,evidence
global_full_depth_entry_fillability,false,global_full_depth_entry_fill_rate=0.1458 min_required=0.3000
recorded_replay_parity,false,replay_parity_json=artifacts/replay-parity/parity-evaluation.json runtime_ready=true event_ready=false blocking_flags=<none> advisory_flags=<none> decision=continue
"""

GLOBAL_FILLABILITY_BLOCKED_GATE = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=false stake_usd=15.00 min_entry_fill_rate=0.3000 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=511 event_complete_rows=1702 replay_parity_ready=true
gate,passed,evidence
data_quality,true,mode=event_complete event_complete_events=511 event_complete_rows=1702
recorded_replay_parity,true,blocking_flags=<none>
global_full_depth_entry_fillability,false,global_full_depth_entry_fill_rate=0.1311 min_required=0.3000
"""

AUTOFACTOR_REPORT = """
# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable repricing PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,spread_adjusted_external_move,full_depth_settlement_executable_pnl,candidate,passed,63255,0.112526,0.058830,54,0.948193,0.8889,0.5000,12651,1.902254,0.6656,0.9000,12651,1,5
2,settlement_fair_edge,full_depth_settlement_executable_pnl,reject,nonpositive_rank_ic,57777,-0.208894,-0.007501,45,-2.321308,0.0222,0.5000,11555,-2.981937,0.5035,0.9000,11555,1,1

# AutoFactor target=full_depth_reprice_pnl_10s
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable repricing PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,spread_adjusted_external_move,full_depth_reprice_pnl_10s,candidate,passed,49789,0.327294,0.123771,40,3.301626,1.0000,0.5000,9958,1.625037,0.5354,0.9000,9958,1,5
"""

AUTOFACTOR_SETTLEMENT_AUTO_REPORT = """
# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,auto_settlement_conservative_settlement_edge,full_depth_settlement_executable_pnl,candidate,passed,49831,0.110842,0.150273,43,1.064178,0.9535,6,0.8333,1.0000,9966,2.666226,0.6836,0.9000,9966,1,1
2,auto_settlement_conservative_settlement_edge_x_near_strike,full_depth_settlement_executable_pnl,candidate,passed,49831,0.107526,0.157904,43,1.044245,0.9302,6,0.8333,1.0000,9966,2.631575,0.6790,0.9000,9966,1,3
3,auto_settlement_conservative_settlement_edge_x_entry_price_quality,full_depth_settlement_executable_pnl,candidate,passed,49831,0.106112,0.155901,43,1.031294,0.9070,6,0.8333,1.0000,9966,2.512771,0.6712,0.9000,9966,1,3
4,auto_settlement_full_depth_settlement_edge_x_external_pressure,full_depth_settlement_executable_pnl,reject,nonpositive_rank_ic,57777,-0.021993,0.069182,45,0.217558,0.4667,6,0.5000,0.5000,11555,-0.301991,0.4785,0.9000,11555,1,3
"""

AUTOFACTOR_PREDICTIVE_EXTERNAL_REPORT = """
# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,amplitude_weighted_momentum_30s_sigma,full_depth_settlement_executable_pnl,candidate,passed,3167,0.083421,0.044219,8,1.102341,0.8750,2,1.0000,1.0000,634,1.471203,0.6120,0.9000,634,1,3
"""

AUTOFACTOR_PREDICTIVE_MUTATION_REPORT = """
# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_avg_entry_sweep_slip_bps,top_bucket_avg_entry_sweep_levels,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted,full_depth_settlement_executable_pnl,candidate,passed,529,0.201235,0.117332,4,1.668090,1.0000,2,1.0000,0.7500,106,4.083066,0.6981,1.0000,18.51,1.42,106,1,8
2,spread_adjusted_external_move,full_depth_settlement_executable_pnl,candidate,passed,529,0.214428,0.141198,4,1.412625,1.0000,2,1.0000,1.0000,106,3.710549,0.6981,1.0000,23.70,1.40,106,1,5
"""

AUTOFACTOR_LLM_RUNTIME_PASS_THROUGH_MUTATION_REPORT = """
# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_avg_entry_sweep_slip_bps,top_bucket_avg_entry_sweep_levels,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,llm_mut_spread_adjusted_external_move_near_strike_runtime_pass_through_add_spread_penalty,full_depth_settlement_executable_pnl,candidate,passed,529,0.139249,0.117332,4,1.668090,0.8750,2,1.0000,0.7500,106,3.669036,0.6981,1.0000,18.51,1.42,106,1,10
"""

AUTOFACTOR_POLY_LAG_PRESSURE_REPORT = """
# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_avg_entry_sweep_slip_bps,top_bucket_avg_entry_sweep_levels,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,mut_poly_lag_pressure_spread_adjusted,full_depth_settlement_executable_pnl,candidate,passed,529,0.181235,0.107332,4,1.568090,1.0000,2,1.0000,0.7500,106,3.083066,0.6681,1.0000,18.51,1.42,106,1,8
"""

AUTOFACTOR_COMPOSED_MODEL_REPORT = """
# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_avg_entry_sweep_slip_bps,top_bucket_avg_entry_sweep_levels,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted,full_depth_settlement_executable_pnl,candidate,passed,529,0.181235,0.107332,4,1.568090,1.0000,2,1.0000,0.7500,106,3.083066,0.6681,1.0000,18.51,1.42,106,1,8
"""

AUTOFACTOR_TRADEABLE_HARD_GATE_REPORT = """
# AutoFactor target=tradeable_full_depth_settlement_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,mut_amplitude_weighted_momentum_30s_sigma_full_depth_entry_gate,tradeable_full_depth_settlement_pnl,candidate,passed,3335,0.080512,0.076800,14,0.951390,0.9286,2,1.0000,0.7500,667,0.872484,0.5757,1.0000,667,1,6
2,mut_spread_adjusted_external_move_full_depth_entry_gate,tradeable_full_depth_settlement_pnl,candidate,passed,3335,0.065967,0.092183,14,1.268732,0.9286,2,1.0000,0.5000,667,1.388814,0.6042,1.0000,667,1,7
"""

AUTOFACTOR_TOP_BUCKET_EXECUTION_REPORT = """
# AutoFactor target=tradeable_full_depth_settlement_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_avg_entry_sweep_slip_bps,top_bucket_avg_entry_sweep_levels,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,auto_settlement_conservative_settlement_edge,tradeable_full_depth_settlement_pnl,candidate,passed,3335,0.080512,0.076800,14,0.951390,0.9286,2,1.0000,0.7500,667,0.872484,0.5757,1.0000,80.00,2.20,667,1,1
"""

AUTOFACTOR_TOP_BUCKET_BAD_EXECUTION_REPORT = """
# AutoFactor target=tradeable_full_depth_settlement_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_avg_entry_sweep_slip_bps,top_bucket_avg_entry_sweep_levels,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,auto_settlement_conservative_settlement_edge,tradeable_full_depth_settlement_pnl,candidate,passed,3335,0.080512,0.076800,14,0.951390,0.9286,2,1.0000,0.7500,667,0.872484,0.5757,1.0000,450.00,3.40,667,1,1
"""

CORE_SUITE_REPORT = f"""
# Snapshot
snapshot_schema=1
snapshot_data_audit_status=critical

=== Factor Walk-Forward V2 ===
factor,target,decision

=== Full Depth Execution Matrix ===
candidate,bucket,count

{READY_GATE}

{AUTOFACTOR_SETTLEMENT_AUTO_REPORT}
"""

DEFAULT_REPLAY_PAYLOAD = {
    "schema_version": 1,
    "kind": "autofactor_candidate_strategy_replay",
    "candidate_replay_id": "candidate_replay:0123456789abcdef0123456789abcdef",
    "identity": {
        "basis": "runtime_market_update_replay",
        "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge",
        "strategy_profile": "settlement_probability",
        "workflow_run_id": "26306734877",
        "recording_path": "/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.20260524T155939.ndjson",
        "recording_sha256": "a" * 64,
    },
    "evidence_stage": "executable_replay",
    "basis": "runtime_market_update_replay",
    "strategy_profile": "settlement_probability",
    "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge",
    "promotion_ready": True,
    "promotion_decision": "promote_to_runtime",
    "source_workflow": "runtime-candidate-replay.yml",
    "workflow_run_id": "26306734877",
    "workflow_run_url": "https://github.com/proerror77/ploy/actions/runs/26306734877",
    "artifact_name": "runtime-candidate-replay-26306734877",
    "source_factor": {
        "name": "auto_settlement_conservative_settlement_edge",
        "target": "full_depth_settlement_executable_pnl",
        "horizon": "5m",
    },
    "decision_contract": {
        "event_level": True,
        "one_decision_per_event": True,
        "official_settlement": True,
        "full_depth_entry": True,
        "target": "full_depth_settlement_executable_pnl",
        "horizon": "5m",
        "max_sweep_levels": 3,
        "stake_usd": 15,
    },
    "metrics": {
        "trade_count": 100,
        "unique_event_count": 100,
        "total_pnl": 12.5,
        "roi": 0.03,
        "entry_fill_rate": 0.95,
    },
    "blocking_risk_flags": [],
}


def replay_for_target(runtime_score: str, target: str = "full_depth_settlement_executable_pnl") -> dict:
    payload = dict(DEFAULT_REPLAY_PAYLOAD)
    payload["runtime_score"] = runtime_score
    payload["identity"] = dict(DEFAULT_REPLAY_PAYLOAD["identity"])
    payload["identity"]["runtime_score"] = runtime_score
    payload["source_factor"] = dict(DEFAULT_REPLAY_PAYLOAD["source_factor"])
    payload["source_factor"]["target"] = target
    payload["decision_contract"] = dict(DEFAULT_REPLAY_PAYLOAD["decision_contract"])
    payload["decision_contract"]["target"] = target
    return payload


SAMPLED_EXECUTION_SNAPSHOT_MANIFEST = {
    "schema_version": "research_snapshot_manifest_v1",
    "snapshot_hash": "snapshot:sampled-execution",
    "source_kind": "complete_sampled_research_snapshot",
    "start": "2026-05-17T00:00:00Z",
    "end": "2026-05-18T00:00:00Z",
    "source_surfaces": [
        {
            "name": "clob_orderbook_snapshots",
            "gate_category": "required_for_execution",
            "raw_full_fidelity": True,
            "snapshot_sampled": True,
        }
    ],
}

VALID_FULL_DEPTH_EXECUTION_SURFACE = {
    "schema_version": "full_depth_execution_surface.v1",
    "surface": "clob_orderbook_snapshots",
    "source": "orderbook_snapshot_archive",
    "start_ts": "2026-05-17T00:00:00Z",
    "end_ts": "2026-05-18T00:00:00Z",
    "checked_hours": 24,
    "existing_hours": 24,
    "row_count": 2_214_371,
    "full_fidelity": True,
    "incomplete": False,
}

ZERO_COVERAGE_DATA_AUDIT = {
    "overall_status": "ok",
    "audit_window_start_ts": "2026-05-17T00:00:00Z",
    "audit_window_end_ts": "2026-05-18T00:00:00Z",
    "required_sources": ["polymarket_orderbooks"],
    "gap_audits": [
        {
            "source_id": "polymarket_orderbooks",
            "status": "ok",
            "coverage_status": "ok",
            "expected_buckets": 288,
            "present_buckets": 0,
            "missing_buckets": 288,
            "coverage_pct": 0.0,
        }
    ],
}


class AutoFactorStrategyPromotionTests(unittest.TestCase):
    def run_script(
        self,
        report,
        *extra_args,
        check=True,
        replay_payload=DEFAULT_REPLAY_PAYLOAD,
        registry_preview_payload=None,
        snapshot_manifest_payload=None,
        snapshot_data_audit_payload=None,
        full_depth_execution_surface_payload=None,
    ):
        with tempfile.TemporaryDirectory() as tmp:
            report_path = Path(tmp) / "report.txt"
            replay_path = Path(tmp) / "candidate-strategy-replay.json"
            registry_path = Path(tmp) / "factor-registry-preview.json"
            snapshot_manifest_path = Path(tmp) / "manifest.json"
            data_audit_path = Path(tmp) / "data-gap-audit.json"
            execution_surface_path = Path(tmp) / "full-depth-execution-surface.json"
            output_json = Path(tmp) / "promotion.json"
            output_registry = Path(tmp) / "registry.json"
            output_handoff = Path(tmp) / "handoff.json"
            output_handoff_md = Path(tmp) / "handoff.md"
            report_path.write_text(report, encoding="utf-8")
            replay_args = []
            if replay_payload is not None:
                replay_path.write_text(
                    json.dumps(replay_payload, indent=2, sort_keys=True),
                    encoding="utf-8",
                )
                replay_args = ["--candidate-strategy-replay-json", str(replay_path)]
            registry_args = []
            if registry_preview_payload is not None:
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
                snapshot_manifest_path.write_text(
                    json.dumps(snapshot_manifest_payload, indent=2, sort_keys=True),
                    encoding="utf-8",
                )
                snapshot_args = ["--snapshot-manifest-json", str(snapshot_manifest_path)]
            data_audit_args = []
            if snapshot_data_audit_payload is not None:
                data_audit_path.write_text(
                    json.dumps(snapshot_data_audit_payload, indent=2, sort_keys=True),
                    encoding="utf-8",
                )
                data_audit_args = ["--snapshot-data-audit-json", str(data_audit_path)]
            execution_surface_args = []
            if full_depth_execution_surface_payload is not None:
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
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--report",
                    str(report_path),
                    "--output-json",
                    str(output_json),
                    "--output-registry-json",
                    str(output_registry),
                    "--output-handoff-json",
                    str(output_handoff),
                    "--output-handoff-md",
                    str(output_handoff_md),
                    *replay_args,
                    *registry_args,
                    *snapshot_args,
                    *data_audit_args,
                    *execution_surface_args,
                    *extra_args,
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=check,
            )
            payload = json.loads(output_json.read_text(encoding="utf-8"))
            registry = json.loads(output_registry.read_text(encoding="utf-8"))
            handoff = json.loads(output_handoff.read_text(encoding="utf-8"))
            handoff_md = output_handoff_md.read_text(encoding="utf-8")
            return result, payload, registry, handoff, handoff_md

    def test_registry_contract_blocks_promotion_name_inference_when_missing(self):
        _, payload, registry, handoff, _ = self.run_script(
            READY_GATE + AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
            registry_preview_payload={
                "version": "alpha_search_artifacts_v1",
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
                "factors": [],
            },
        )

        first = payload["evaluated_factors"][0]
        self.assertFalse(first["qualified"])
        self.assertIn(
            "missing_runtime_contract:auto_settlement_conservative_settlement_edge",
            first["blockers"],
        )
        self.assertEqual(registry["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")

    def test_registry_contract_blocker_prevents_promotion(self):
        report = (READY_GATE + AUTOFACTOR_SETTLEMENT_AUTO_REPORT).replace(
            "auto_settlement_conservative_settlement_edge,full_depth_settlement_executable_pnl",
            "auto_settlement_conservative_settlement_edge_x_iv_change,full_depth_settlement_executable_pnl",
        )
        replay = dict(DEFAULT_REPLAY_PAYLOAD)
        replay["runtime_score"] = (
            "autofactor_formula:auto_settlement_conservative_settlement_edge_x_iv_change"
        )
        replay["identity"] = dict(DEFAULT_REPLAY_PAYLOAD["identity"])
        replay["identity"]["runtime_score"] = replay["runtime_score"]
        _, payload, registry, handoff, _ = self.run_script(
            report,
            replay_payload=replay,
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

        first = payload["evaluated_factors"][0]
        self.assertFalse(first["qualified"])
        self.assertIn("runtime_input_not_supplied:iv_change_1m", first["blockers"])
        self.assertEqual(registry["entries"][0]["runtime_contract"]["input_names"][-1], "iv_change_1m")
        self.assertEqual(handoff["status"], "blocked")

    def test_registry_contract_uses_canonical_runtime_inputs_for_promotion(self):
        report = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=true stake_usd=15.00 min_entry_fill_rate=0.0500 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=2488 event_complete_rows=51989 replay_parity_ready=true
gate,passed,evidence
data_quality,true,mode=event_complete event_complete_events=2488 event_complete_rows=51989
recorded_replay_parity,true,blocking_flags=<none>

# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,auto_settlement_model_full_depth_settlement_edge_x_near_strike_x_capacity,full_depth_settlement_executable_pnl,candidate,passed,49831,0.110842,0.150273,43,1.064178,0.9535,6,0.8333,1.0000,9966,2.666226,0.6836,0.9000,9966,1,5
"""
        runtime_score = (
            "autofactor_formula:"
            "auto_settlement_model_full_depth_settlement_edge_x_near_strike_x_capacity"
        )
        _, payload, registry, handoff, _ = self.run_script(
            report,
            replay_payload=replay_for_target(runtime_score),
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
                            "ast_input_names": [
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

        first = payload["evaluated_factors"][0]
        self.assertTrue(first["qualified"])
        self.assertNotIn("runtime_input_unsupported:near_strike_score", first["blockers"])
        self.assertNotIn("runtime_input_unsupported:entry_capacity_score", first["blockers"])
        self.assertEqual(registry["entries"][0]["runtime_contract"]["runtime_input_names"][-1], "settlement_edge")
        self.assertEqual(handoff["status"], "ready")

    def test_blocks_candidate_when_runtime_profile_is_not_required_profile(self):
        _, payload, registry, handoff, handoff_md = self.run_script(
            READY_GATE + AUTOFACTOR_REPORT
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(registry["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        self.assertEqual(handoff["recommended_action"], "do_not_promote")
        self.assertIn("No dry-run handoff issue or config", handoff_md)
        spread = next(
            item
            for item in payload["evaluated_factors"]
            if item["factor"]["name"] == "spread_adjusted_external_move"
            and item["factor"]["target"] == "full_depth_settlement_executable_pnl"
        )
        self.assertIn(
            "runtime_profile_mismatch:repricing_momentum!=settlement_probability",
            spread["blockers"],
        )
        fair_edge = next(
            item
            for item in payload["evaluated_factors"]
            if item["factor"]["name"] == "settlement_fair_edge"
            and item["factor"]["target"] == "full_depth_settlement_executable_pnl"
        )
        self.assertIn("autofactor_not_candidate:reject:nonpositive_rank_ic", fair_edge["blockers"])
        self.assertIn("empty_runtime_strategy_profile", fair_edge["blockers"])

    def test_qualifies_settlement_native_auto_factors_when_gate_is_ready(self):
        _, payload, registry, handoff, handoff_md = self.run_script(
            READY_GATE + AUTOFACTOR_SETTLEMENT_AUTO_REPORT
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(registry["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(handoff["blocked_factor_count"], 3)
        self.assertEqual(len(handoff["strategies"]), 1)
        self.assertEqual(
            handoff["strategies"][0]["runtime_score"],
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
        )
        self.assertEqual(
            handoff["strategies"][0]["strategy_profile"],
            "settlement_probability",
        )
        self.assertIn("Candidate strategy replay ready: `true`", handoff_md)
        self.assertIn(
            "Candidate strategy replay id: `candidate_replay:0123456789abcdef0123456789abcdef`",
            handoff_md,
        )
        self.assertIn("top bucket avg label", handoff_md)
        self.assertNotIn("top bucket pnl", handoff_md.lower())
        near_strike = next(
            item
            for item in payload["evaluated_factors"]
            if item["factor"]["name"] == "auto_settlement_conservative_settlement_edge_x_near_strike"
        )
        self.assertIn(
            "candidate_strategy_replay_runtime_score_mismatch:"
            "autofactor_formula:auto_settlement_conservative_settlement_edge!="
            "autofactor_formula:auto_settlement_conservative_settlement_edge_x_near_strike",
            near_strike["blockers"],
        )
        rejected = next(
            item
            for item in payload["evaluated_factors"]
            if item["factor"]["name"] == "auto_settlement_full_depth_settlement_edge_x_external_pressure"
        )
        self.assertFalse(rejected["qualified"])
        self.assertIn("autofactor_not_candidate:reject:nonpositive_rank_ic", rejected["blockers"])

    def test_blocks_when_global_entry_sweep_slippage_is_too_high(self):
        _, payload, registry, handoff, handoff_md = self.run_script(
            HIGH_SLIPPAGE_HEALTH + READY_GATE + AUTOFACTOR_SETTLEMENT_AUTO_REPORT
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(registry["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        first = payload["evaluated_factors"][0]
        self.assertFalse(first["qualified"])
        self.assertIn(
            "global_entry_sweep_slippage_too_high:1912.92>200.00",
            first["blockers"],
        )
        self.assertEqual(handoff["execution_quality"]["avg_entry_sweep_slip_bps"], 1912.92)
        self.assertIn("Avg entry sweep slip bps: `1912.92`", handoff_md)

    def test_allows_low_global_entry_sweep_slippage(self):
        _, payload, registry, handoff, _ = self.run_script(
            LOW_SLIPPAGE_HEALTH + READY_GATE + AUTOFACTOR_SETTLEMENT_AUTO_REPORT
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(registry["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(payload["execution_quality"]["avg_entry_sweep_slip_bps"], 80.0)

    def test_uses_top_bucket_execution_quality_before_global_slippage(self):
        _, payload, _, handoff, _ = self.run_script(
            HIGH_SLIPPAGE_HEALTH + READY_GATE + AUTOFACTOR_TOP_BUCKET_EXECUTION_REPORT,
            replay_payload=replay_for_target(
                "autofactor_formula:auto_settlement_conservative_settlement_edge",
                "tradeable_full_depth_settlement_pnl",
            ),
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        first = payload["qualified_strategies"][0]["factor"]
        self.assertEqual(first["top_bucket_avg_entry_sweep_slip_bps"], 80.0)
        self.assertEqual(first["top_bucket_avg_entry_sweep_levels"], 2.2)

    def test_blocks_top_bucket_slippage_and_level_count(self):
        _, payload, _, handoff, _ = self.run_script(
            LOW_SLIPPAGE_HEALTH + READY_GATE + AUTOFACTOR_TOP_BUCKET_BAD_EXECUTION_REPORT,
            replay_payload=replay_for_target(
                "autofactor_formula:auto_settlement_conservative_settlement_edge",
                "tradeable_full_depth_settlement_pnl",
            ),
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        blockers = payload["evaluated_factors"][0]["blockers"]
        self.assertIn("top_bucket_entry_sweep_slippage_too_high:450.00>200.00", blockers)
        self.assertIn("top_bucket_entry_sweep_levels_too_high:3.40>3.00", blockers)

    def test_top_bucket_execution_can_override_global_fillability_blocker(self):
        _, payload, _, handoff, _ = self.run_script(
            LOW_SLIPPAGE_HEALTH
            + GLOBAL_FILLABILITY_BLOCKED_GATE
            + AUTOFACTOR_TOP_BUCKET_EXECUTION_REPORT,
            replay_payload=replay_for_target(
                "autofactor_formula:auto_settlement_conservative_settlement_edge",
                "tradeable_full_depth_settlement_pnl",
            ),
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        blockers = payload["qualified_strategies"][0].get("blockers", [])
        self.assertNotIn("global_full_depth_entry_fillability", ",".join(blockers))

    def test_sampled_execution_snapshot_blocks_top_bucket_fillability_override(self):
        _, payload, _, handoff, _ = self.run_script(
            LOW_SLIPPAGE_HEALTH
            + GLOBAL_FILLABILITY_BLOCKED_GATE
            + AUTOFACTOR_TOP_BUCKET_EXECUTION_REPORT,
            replay_payload=replay_for_target(
                "autofactor_formula:auto_settlement_conservative_settlement_edge",
                "tradeable_full_depth_settlement_pnl",
            ),
            snapshot_manifest_payload=SAMPLED_EXECUTION_SNAPSHOT_MANIFEST,
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        self.assertEqual(
            payload["source_snapshot_contract"]["blocking_risk_flags"],
            ["sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots"],
        )
        self.assertEqual(
            handoff["source_snapshot_contract"]["blocking_risk_flags"],
            ["sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots"],
        )
        blockers = payload["evaluated_factors"][0]["blockers"]
        self.assertIn(
            "snapshot_contract_blocks_execution_claim:"
            "sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots",
            blockers,
        )
        self.assertIn(
            "global_promotion_gate_not_ready:"
            "global_full_depth_entry_fillability: "
            "global_full_depth_entry_fill_rate=0.1311 min_required=0.3000",
            blockers,
        )

    def test_full_depth_execution_surface_proof_unlocks_sampled_snapshot_blocker(self):
        _, payload, _, handoff, _ = self.run_script(
            LOW_SLIPPAGE_HEALTH
            + GLOBAL_FILLABILITY_BLOCKED_GATE
            + AUTOFACTOR_TOP_BUCKET_EXECUTION_REPORT,
            replay_payload=replay_for_target(
                "autofactor_formula:auto_settlement_conservative_settlement_edge",
                "tradeable_full_depth_settlement_pnl",
            ),
            snapshot_manifest_payload=SAMPLED_EXECUTION_SNAPSHOT_MANIFEST,
            full_depth_execution_surface_payload=VALID_FULL_DEPTH_EXECUTION_SURFACE,
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(payload["source_snapshot_contract"]["blocking_risk_flags"], [])
        self.assertEqual(
            payload["source_snapshot_contract"]["satisfied_execution_surfaces"],
            ["clob_orderbook_snapshots"],
        )
        self.assertTrue(
            payload["source_snapshot_contract"]["full_depth_execution_surface_proofs"][0][
                "valid"
            ]
        )

    def test_zero_coverage_data_audit_blocks_even_with_full_depth_proof(self):
        _, payload, _, handoff, _ = self.run_script(
            LOW_SLIPPAGE_HEALTH
            + GLOBAL_FILLABILITY_BLOCKED_GATE
            + AUTOFACTOR_TOP_BUCKET_EXECUTION_REPORT,
            replay_payload=replay_for_target(
                "autofactor_formula:auto_settlement_conservative_settlement_edge",
                "tradeable_full_depth_settlement_pnl",
            ),
            snapshot_manifest_payload=SAMPLED_EXECUTION_SNAPSHOT_MANIFEST,
            snapshot_data_audit_payload=ZERO_COVERAGE_DATA_AUDIT,
            full_depth_execution_surface_payload=VALID_FULL_DEPTH_EXECUTION_SURFACE,
        )

        expected = "data_audit_zero_coverage:polymarket_orderbooks:0<288"
        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        self.assertIn(expected, payload["source_snapshot_contract"]["blocking_risk_flags"])
        self.assertIn(
            f"snapshot_contract_blocks_execution_claim:{expected}",
            payload["evaluated_factors"][0]["blockers"],
        )

    def test_invalid_full_depth_execution_surface_proof_fails_closed(self):
        proof = dict(VALID_FULL_DEPTH_EXECUTION_SURFACE)
        proof["full_fidelity"] = False

        _, payload, _, handoff, _ = self.run_script(
            LOW_SLIPPAGE_HEALTH
            + GLOBAL_FILLABILITY_BLOCKED_GATE
            + AUTOFACTOR_TOP_BUCKET_EXECUTION_REPORT,
            replay_payload=replay_for_target(
                "autofactor_formula:auto_settlement_conservative_settlement_edge",
                "tradeable_full_depth_settlement_pnl",
            ),
            snapshot_manifest_payload=SAMPLED_EXECUTION_SNAPSHOT_MANIFEST,
            full_depth_execution_surface_payload=proof,
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        self.assertIn(
            "sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots",
            payload["source_snapshot_contract"]["blocking_risk_flags"],
        )
        self.assertIn(
            "full_depth_execution_surface_invalid:clob_orderbook_snapshots:not_full_fidelity",
            payload["source_snapshot_contract"]["blocking_risk_flags"],
        )

    def test_qualifies_predictive_external_formula_when_gate_is_ready(self):
        replay = replay_for_target(
            "autofactor_formula:amplitude_weighted_momentum_30s_sigma"
        )
        _, payload, registry, handoff, handoff_md = self.run_script(
            READY_GATE + AUTOFACTOR_PREDICTIVE_EXTERNAL_REPORT,
            replay_payload=replay,
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(registry["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(
            handoff["strategies"][0]["runtime_score"],
            "autofactor_formula:amplitude_weighted_momentum_30s_sigma",
        )
        self.assertEqual(
            handoff["strategies"][0]["strategy_family"],
            "predictive_settlement_probability",
        )
        self.assertIn("amplitude_weighted_momentum_30s_sigma", handoff_md)

    def test_qualifies_runtime_supported_predictive_formula_mutation(self):
        replay = replay_for_target(
            "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted"
        )
        _, payload, registry, handoff, handoff_md = self.run_script(
            READY_GATE + AUTOFACTOR_PREDICTIVE_MUTATION_REPORT,
            replay_payload=replay,
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(registry["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(
            handoff["strategies"][0]["runtime_score"],
            "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
        )
        self.assertEqual(
            handoff["strategies"][0]["strategy_family"],
            "predictive_settlement_probability",
        )
        self.assertIn("mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted", handoff_md)
        bare_spread = next(
            item
            for item in payload["evaluated_factors"]
            if item["factor"]["name"] == "spread_adjusted_external_move"
        )
        self.assertIn(
            "runtime_profile_mismatch:repricing_momentum!=settlement_probability",
            bare_spread["blockers"],
        )

    def test_qualifies_llm_runtime_pass_through_predictive_formula_mutation(self):
        runtime_score = (
            "autofactor_formula:"
            "llm_mut_spread_adjusted_external_move_near_strike_runtime_pass_through_add_spread_penalty"
        )
        replay = replay_for_target(runtime_score)
        _, payload, registry, handoff, handoff_md = self.run_script(
            READY_GATE + AUTOFACTOR_LLM_RUNTIME_PASS_THROUGH_MUTATION_REPORT,
            replay_payload=replay,
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(registry["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(handoff["strategies"][0]["runtime_score"], runtime_score)
        self.assertEqual(
            handoff["strategies"][0]["strategy_family"],
            "predictive_settlement_probability",
        )
        self.assertNotIn(
            "missing_runtime_strategy_mapping",
            handoff["strategies"][0].get("blockers", []),
        )
        self.assertIn("runtime_pass_through_add_spread_penalty", handoff_md)

    def test_qualifies_poly_lag_pressure_predictive_formula_mutation(self):
        replay = replay_for_target("autofactor_formula:mut_poly_lag_pressure_spread_adjusted")
        _, payload, registry, handoff, handoff_md = self.run_script(
            READY_GATE + AUTOFACTOR_POLY_LAG_PRESSURE_REPORT,
            replay_payload=replay,
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(registry["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(
            handoff["strategies"][0]["runtime_score"],
            "autofactor_formula:mut_poly_lag_pressure_spread_adjusted",
        )
        self.assertEqual(
            handoff["strategies"][0]["strategy_family"],
            "predictive_settlement_probability",
        )
        self.assertIn("mut_poly_lag_pressure_spread_adjusted", handoff_md)

    def test_qualifies_composed_settlement_model_formula_mutation(self):
        runtime_score = (
            "autofactor_formula:"
            "mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted"
        )
        replay = replay_for_target(runtime_score)
        _, payload, registry, handoff, handoff_md = self.run_script(
            READY_GATE + AUTOFACTOR_COMPOSED_MODEL_REPORT,
            replay_payload=replay,
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(registry["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(handoff["strategies"][0]["runtime_score"], runtime_score)
        self.assertEqual(
            handoff["strategies"][0]["strategy_family"],
            "settlement_probability",
        )
        self.assertIn(
            "mut_auto_settlement_model_full_depth_settlement_edge_x_external_pressure_spread_adjusted",
            handoff_md,
        )

    def test_qualifies_tradeable_hard_gate_predictive_formula_when_gate_is_ready(self):
        replay = replay_for_target(
            "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_full_depth_entry_gate",
            "tradeable_full_depth_settlement_pnl",
        )
        _, payload, registry, handoff, handoff_md = self.run_script(
            READY_GATE + AUTOFACTOR_TRADEABLE_HARD_GATE_REPORT,
            replay_payload=replay,
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(registry["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(len(handoff["strategies"]), 1)
        self.assertEqual(
            handoff["strategies"][0]["runtime_score"],
            "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_full_depth_entry_gate",
        )
        self.assertEqual(
            handoff["strategies"][0]["strategy_family"],
            "predictive_settlement_probability",
        )
        self.assertIn("full_depth_entry_gate", handoff_md)

    def test_hard_gate_predictive_formula_keeps_global_fillability_blocker(self):
        _, payload, _, handoff, _ = self.run_script(
            HARD_GATE_REPLAY_BLOCKED_GATE + AUTOFACTOR_TRADEABLE_HARD_GATE_REPORT
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        first = payload["evaluated_factors"][0]
        self.assertFalse(first["qualified"])
        self.assertIn(
            "global_promotion_gate_not_ready:global_full_depth_entry_fillability: "
            "global_full_depth_entry_fill_rate=0.1458 min_required=0.3000",
            first["blockers"],
        )
        self.assertNotIn("recorded_replay_parity", ",".join(first["blockers"]))

    def test_core_suite_report_is_sufficient_for_handoff_evaluation(self):
        self.assertNotIn("=== Fillability Review V1 Data Health ===", CORE_SUITE_REPORT)
        self.assertNotIn("=== Liquidity Gate V1 ===", CORE_SUITE_REPORT)
        self.assertNotIn("=== Meta Label Walk-Forward V1 ===", CORE_SUITE_REPORT)

        _, payload, registry, handoff, handoff_md = self.run_script(CORE_SUITE_REPORT)

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(registry["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(
            handoff["strategies"][0]["runtime_score"],
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
        )
        self.assertIn("Promote AutoFactor strategy handoff to dry-run", handoff_md)

    def test_qualifies_when_allowed_target_and_runtime_profile_match(self):
        replay = replay_for_target(
            "spread_adjusted_external_move_score",
            "full_depth_reprice_pnl_10s",
        )
        replay["strategy_profile"] = "repricing_momentum"
        replay["source_factor"]["horizon"] = "10s"
        replay["decision_contract"]["horizon"] = "10s"
        _, payload, registry, handoff, handoff_md = self.run_script(
            READY_GATE + AUTOFACTOR_REPORT,
            "--allowed-target",
            "full_depth_reprice_pnl_10s",
            "--required-strategy-profile",
            "repricing_momentum",
            replay_payload=replay,
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(registry["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(handoff["recommended_action"], "create_dry_run_handoff")
        self.assertEqual(
            handoff["strategies"][0]["runtime_score"],
            "spread_adjusted_external_move_score",
        )
        self.assertIn("Promote AutoFactor strategy handoff to dry-run", handoff_md)
        self.assertIn("spread_adjusted_external_move_score", handoff_md)
        self.assertEqual(
            payload["qualified_strategies"][0]["factor"]["name"],
            "spread_adjusted_external_move",
        )

    def test_blocks_when_promotion_gate_is_not_ready(self):
        result, payload, _, handoff, _ = self.run_script(
            BLOCKED_GATE + AUTOFACTOR_REPORT,
            "--allowed-target",
            "full_depth_reprice_pnl_10s",
            "--required-strategy-profile",
            "repricing_momentum",
            "--fail-if-blocked",
            check=False,
        )

        self.assertEqual(result.returncode, 3)
        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        self.assertIn("promotion_gate_not_ready", payload["evaluated_factors"][0]["blockers"])

    def test_formula_candidates_use_formula_specific_model_gates(self):
        _, payload, _, handoff, _ = self.run_script(
            MODEL_BLOCKED_GATE + AUTOFACTOR_SETTLEMENT_AUTO_REPORT
        )

        self.assertEqual(payload["decision"], "qualified")
        self.assertEqual(handoff["status"], "ready")
        self.assertEqual(len(handoff["strategies"]), 1)
        first = payload["qualified_strategies"][0]["factor"]
        self.assertEqual(first["symbol_count"], 6)
        self.assertEqual(first["symbol_positive_ratio"], 0.8333)

    def test_formula_candidates_require_symbol_stability(self):
        weak_symbol_report = AUTOFACTOR_SETTLEMENT_AUTO_REPORT.replace(
            ",6,0.8333,",
            ",6,0.5000,",
        )

        _, payload, _, handoff, _ = self.run_script(
            MODEL_BLOCKED_GATE + weak_symbol_report
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        first = payload["evaluated_factors"][0]
        self.assertIn("formula_symbol_holdout_unstable:0.5000<0.60", first["blockers"])

    def test_blocks_runtime_mapped_candidate_with_too_few_top_bucket_trades(self):
        sparse_report = AUTOFACTOR_SETTLEMENT_AUTO_REPORT.replace(
            ",9966,2.666226,",
            ",12,2.666226,",
            1,
        )

        _, payload, _, handoff, _ = self.run_script(READY_GATE + sparse_report)

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        sparse = next(
            item
            for item in payload["evaluated_factors"]
            if item["factor"]["name"] == "auto_settlement_conservative_settlement_edge"
        )
        self.assertFalse(sparse["qualified"])
        self.assertIn("top_bucket_sample_too_small:12<50", sparse["blockers"])

    def test_blocks_runtime_mapped_candidate_with_low_top_bucket_fillability(self):
        thin_report = AUTOFACTOR_SETTLEMENT_AUTO_REPORT.replace(
            ",0.6836,0.9000,9966,1,1",
            ",0.6836,0.1458,9966,1,1",
            1,
        )

        _, payload, _, handoff, _ = self.run_script(READY_GATE + thin_report)

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        thin = next(
            item
            for item in payload["evaluated_factors"]
            if item["factor"]["name"] == "auto_settlement_conservative_settlement_edge"
        )
        self.assertFalse(thin["qualified"])
        self.assertIn(
            "top_bucket_full_depth_entry_fill_rate_too_low:0.1458<0.3000",
            thin["blockers"],
        )

    def test_blocks_candidate_when_one_event_decision_gate_is_missing(self):
        legacy_report = AUTOFACTOR_SETTLEMENT_AUTO_REPORT.replace(
            ",top_bucket_unique_event_count,top_bucket_max_event_decisions",
            "",
        )
        legacy_report = legacy_report.replace(",9966,1,1", ",1", 1)

        _, payload, _, handoff, _ = self.run_script(READY_GATE + legacy_report)

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        first = payload["evaluated_factors"][0]
        self.assertIn("missing_one_event_decision_gate", first["blockers"])

    def test_blocks_candidate_when_top_bucket_repeats_event(self):
        duplicate_event_report = AUTOFACTOR_SETTLEMENT_AUTO_REPORT.replace(
            ",9966,1,1",
            ",4000,3,1",
            1,
        )

        _, payload, _, handoff, _ = self.run_script(READY_GATE + duplicate_event_report)

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        first = next(
            item
            for item in payload["evaluated_factors"]
            if item["factor"]["name"] == "auto_settlement_conservative_settlement_edge"
        )
        self.assertFalse(first["qualified"])
        self.assertIn(
            "one_event_decision_violation:max_event_decisions=3",
            first["blockers"],
        )

    def test_blocks_ready_factor_when_candidate_strategy_replay_is_missing(self):
        _, payload, _, handoff, handoff_md = self.run_script(
            READY_GATE + AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
            replay_payload=None,
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        first = payload["evaluated_factors"][0]
        self.assertIn("missing_candidate_strategy_replay", first["blockers"])
        self.assertIn("candidate_strategy_replay_not_ready", first["blockers"])
        self.assertIn("Candidate strategy replay ready: `false`", handoff_md)

    def test_blocks_top_bucket_aggregate_as_candidate_strategy_replay(self):
        replay = {
            **DEFAULT_REPLAY_PAYLOAD,
            "basis": "factor_walk_forward_top_bucket_aggregate",
            "identity": {
                **DEFAULT_REPLAY_PAYLOAD["identity"],
                "basis": "factor_walk_forward_top_bucket_aggregate",
            },
        }

        _, payload, _, handoff, _ = self.run_script(
            READY_GATE + AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
            replay_payload=replay,
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        first = payload["evaluated_factors"][0]
        self.assertIn(
            "candidate_strategy_replay_not_runtime_replay:"
            "factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
            first["blockers"],
        )

    def test_blocks_runtime_replay_from_wrong_horizon(self):
        replay = replay_for_target(
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
            "full_depth_reprice_pnl_30s",
        )
        replay["source_factor"]["horizon"] = "30s"
        replay["decision_contract"]["horizon"] = "30s"

        _, payload, _, handoff, _ = self.run_script(
            READY_GATE + AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
            replay_payload=replay,
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        blockers = payload["evaluated_factors"][0]["blockers"]
        self.assertIn(
            "candidate_strategy_replay_target_mismatch:"
            "full_depth_reprice_pnl_30s!=full_depth_settlement_executable_pnl",
            blockers,
        )
        self.assertIn("candidate_strategy_replay_horizon_mismatch:30s!=5m", blockers)

    def test_blocks_legacy_runtime_replay_without_durable_provenance(self):
        replay = {
            key: value
            for key, value in DEFAULT_REPLAY_PAYLOAD.items()
            if key
            not in {
                "candidate_replay_id",
                "identity",
                "promotion_decision",
                "source_workflow",
                "workflow_run_id",
                "workflow_run_url",
                "artifact_name",
            }
        }

        _, payload, _, handoff, handoff_md = self.run_script(
            READY_GATE + AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
            replay_payload=replay,
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        first = payload["evaluated_factors"][0]
        self.assertIn(
            "candidate_strategy_replay_missing_candidate_replay_id",
            first["blockers"],
        )
        self.assertIn("candidate_strategy_replay_missing_identity", first["blockers"])
        self.assertIn("candidate_strategy_replay_missing_source_workflow", first["blockers"])
        self.assertIn("candidate_strategy_replay_missing_workflow_run_id", first["blockers"])
        self.assertIn("candidate_strategy_replay_missing_workflow_run_url", first["blockers"])
        self.assertIn("candidate_strategy_replay_missing_artifact_name", first["blockers"])
        self.assertIn("Candidate strategy replay id: ``", handoff_md)

    def test_blocks_mutable_runtime_replay_without_recording_hash(self):
        replay = {
            **DEFAULT_REPLAY_PAYLOAD,
            "identity": {
                **DEFAULT_REPLAY_PAYLOAD["identity"],
                "recording_path": "/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
                "recording_sha256": "",
            },
        }

        _, payload, _, handoff, _ = self.run_script(
            READY_GATE + AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
            replay_payload=replay,
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        blockers = payload["evaluated_factors"][0]["blockers"]
        self.assertIn("candidate_strategy_replay_missing_recording_sha256", blockers)
        self.assertIn("candidate_strategy_replay_mutable_recording_without_sha256", blockers)

    def test_blocks_replay_without_executable_strategy_contract(self):
        replay = {
            **DEFAULT_REPLAY_PAYLOAD,
            "promotion_ready": True,
            "decision_contract": {
                "event_level": True,
                "one_decision_per_event": True,
                "official_settlement": False,
                "full_depth_entry": False,
            },
            "metrics": {
                "trade_count": 8,
                "unique_event_count": 8,
                "roi": -0.01,
                "entry_fill_rate": 0.20,
            },
        }

        _, payload, _, handoff, _ = self.run_script(
            READY_GATE + AUTOFACTOR_SETTLEMENT_AUTO_REPORT,
            replay_payload=replay,
        )

        self.assertEqual(payload["decision"], "blocked")
        self.assertEqual(handoff["status"], "blocked")
        blockers = payload["evaluated_factors"][0]["blockers"]
        self.assertIn("candidate_strategy_replay_missing_contract:official_settlement", blockers)
        self.assertIn("candidate_strategy_replay_missing_contract:full_depth_entry", blockers)
        self.assertIn("candidate_strategy_replay_trade_count_too_small:8<50", blockers)
        self.assertIn("candidate_strategy_replay_entry_fill_rate_too_low:0.2000<0.3000", blockers)
        self.assertIn("candidate_strategy_replay_roi_too_low:-0.010000<0.000000", blockers)


if __name__ == "__main__":
    unittest.main()
