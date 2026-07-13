import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "run_factor_walk_forward_sweep.py"
WORKFLOW = ROOT / ".github" / "workflows" / "factor-walk-forward-v2-hosted-artifact.yml"


FAKE_REPORT = r'''
print("""=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=true stake_usd=15.00 min_entry_fill_rate=0.0500 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=2488 event_complete_rows=51989 replay_parity_ready=true
gate,passed,evidence
data_quality,true,mode=event_complete event_complete_events=2488 event_complete_rows=51989
deribit_vol_surface,true,require_deribit=false include_deribit=false
recorded_replay_parity,true,blocking_flags=<none>

# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,auto_settlement_conservative_settlement_edge,full_depth_settlement_executable_pnl,candidate,passed,49831,0.110842,0.150273,43,1.064178,0.9535,6,0.8333,1.0000,9966,2.666226,0.6836,0.9000,9966,1,1
""")
'''

FAKE_UNMAPPED_BEST_REPORT = r'''
print("""=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=false stake_usd=15.00 min_entry_fill_rate=0.0500 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=2488 event_complete_rows=51989 replay_parity_ready=false
gate,passed,evidence
data_quality,true,mode=event_complete event_complete_events=2488 event_complete_rows=51989
deribit_vol_surface,true,require_deribit=false include_deribit=false
recorded_replay_parity,false,shared_event_count=0

# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted,full_depth_settlement_executable_pnl,candidate,passed,3335,0.067525,0.081201,14,1.288053,0.9286,2,1.0000,1.0000,667,1.660059,0.6132,0.9000,667,1,6
2,auto_settlement_conservative_settlement_edge,full_depth_settlement_executable_pnl,candidate,passed,2798,0.067534,0.091002,12,1.018091,0.7500,2,1.0000,1.0000,560,2.507843,0.6250,0.9000,560,1,1
""")
'''

FAKE_TWO_RUNTIME_MAPPABLE_REPORT = r'''
print("""=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=true stake_usd=15.00 min_entry_fill_rate=0.0500 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=2488 event_complete_rows=51989 replay_parity_ready=true
gate,passed,evidence
data_quality,true,mode=event_complete event_complete_events=2488 event_complete_rows=51989
deribit_vol_surface,true,require_deribit=false include_deribit=false
recorded_replay_parity,true,blocking_flags=<none>

# AutoFactor target=full_depth_settlement_executable_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,mut_auto_settlement_model_full_depth_settlement_edge_x_capacity_spread_adjusted,full_depth_settlement_executable_pnl,candidate,passed,1079,0.125437,0.346161,12,1.334432,0.8333,3,1.0000,1.0000,216,16.533648,0.4722,1.0000,216,1,7
2,mut_auto_settlement_model_full_depth_settlement_edge_spread_adjusted_capacity,full_depth_settlement_executable_pnl,candidate,passed,1079,0.125437,0.346161,12,1.334432,0.8333,3,1.0000,1.0000,216,16.533648,0.4722,1.0000,216,1,7
""")
'''

FAKE_TRADEABLE_HARD_GATE_BY_FILTER = r'''
import sys
filter_value = ""
for idx, arg in enumerate(sys.argv):
    if arg == "--factor-name-filter" and idx + 1 < len(sys.argv):
        filter_value = sys.argv[idx + 1]
if filter_value == "external_move":
    print("""=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=true stake_usd=15.00 min_entry_fill_rate=0.3000 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=739 event_complete_rows=2846 replay_parity_ready=true
gate,passed,evidence
recorded_replay_parity,true,blocking_flags=<none>

# AutoFactor target=tradeable_full_depth_settlement_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,mut_spread_adjusted_external_move_full_depth_entry_gate,tradeable_full_depth_settlement_pnl,candidate,passed,3335,0.065967,0.092183,14,1.268732,0.9286,2,1.0000,0.5000,667,1.388814,0.6042,1.0000,667,1,7
""")
else:
    print("""=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=true stake_usd=15.00 min_entry_fill_rate=0.3000 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=739 event_complete_rows=2846 replay_parity_ready=true
gate,passed,evidence
recorded_replay_parity,true,blocking_flags=<none>

# AutoFactor target=tradeable_full_depth_settlement_pnl
=== AutoFactor Seed Candidate Report ===
target labels are side-aligned executable settlement PnL; reports are candidate discovery gates, not deploy decisions.
rank,name,target,decision,reason,n,spearman_ic,pearson_ic,window_count,icir,positive_window_ratio,symbol_count,symbol_positive_ratio,monotonicity,top_bucket_n,top_bucket_avg_label,top_bucket_positive_label_rate,top_bucket_full_depth_entry_fill_rate,top_bucket_unique_event_count,top_bucket_max_event_decisions,complexity
1,mut_amplitude_weighted_momentum_30s_sigma_full_depth_entry_gate,tradeable_full_depth_settlement_pnl,candidate,passed,3335,0.080512,0.076800,14,0.951390,0.9286,2,1.0000,0.7500,667,0.872484,0.5757,1.0000,667,1,6
""")
'''


class FactorWalkForwardSweepTests(unittest.TestCase):
    def replay_file(
        self,
        tmp: Path,
        runtime_score: str = "autofactor_formula:auto_settlement_conservative_settlement_edge",
        target: str = "full_depth_settlement_executable_pnl",
    ) -> Path:
        replay = tmp / f"candidate-strategy-replay-{runtime_score.replace(':', '_')}.json"
        replay.write_text(
            json.dumps(
                {
                    "schema_version": 1,
                    "kind": "autofactor_candidate_strategy_replay",
                    "candidate_replay_id": "candidate_replay:fedcba98765432100123456789abcdef",
                    "identity": {
                        "basis": "runtime_market_update_replay",
                        "recording_path": "/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.20260524T155939.ndjson",
                        "recording_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                        "runtime_score": runtime_score,
                        "strategy_profile": "settlement_probability",
                        "workflow_run_id": "26306734877",
                    },
                    "evidence_stage": "executable_replay",
                    "basis": "runtime_market_update_replay",
                    "strategy_profile": "settlement_probability",
                    "runtime_score": runtime_score,
                    "promotion_ready": True,
                    "promotion_decision": "promote_to_runtime",
                    "recording_path": "/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.20260524T155939.ndjson",
                    "recording_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
                    "source_workflow": "runtime-candidate-replay.yml",
                    "workflow_run_id": "26306734877",
                    "workflow_run_url": "https://github.com/proerror77/ploy/actions/runs/26306734877",
                    "artifact_name": "runtime-candidate-replay-26306734877",
                    "source_factor": {
                        "target": target,
                        "horizon": "5m",
                    },
                    "decision_contract": {
                        "event_level": True,
                        "one_decision_per_event": True,
                        "official_settlement": True,
                        "full_depth_entry": True,
                        "target": target,
                        "horizon": "5m",
                    },
                    "metrics": {
                        "trade_count": 100,
                        "unique_event_count": 100,
                        "total_pnl": 12.5,
                        "roi": 0.03,
                        "entry_fill_rate": 0.95,
                    },
                    "blocking_risk_flags": [],
                },
                indent=2,
                sort_keys=True,
            ),
            encoding="utf-8",
        )
        return replay

    def base_args(self, tmp: Path, binary: Path) -> list[str]:
        return [
            sys.executable,
            str(SCRIPT),
            "--binary",
            str(binary),
            "--snapshot-dir",
            str(tmp / "snapshot"),
            "--output-dir",
            str(tmp / "out"),
            "--symbols",
            "BTCUSDT,ETHUSDT",
            "--start-ts",
            "2026-04-24T00:00:00Z",
            "--end-ts",
            "2026-05-01T00:00:00Z",
            "--stake-usd",
            "15",
            "--train-window-days",
            "2",
            "--test-window-days",
            "1",
            "--step-days",
            "1",
            "--lob-sample-secs",
            "30",
            "--pm-book-sample-secs",
            "30",
            "--observation-sample-secs",
            "30",
            "--max-quote-age-secs",
            "30",
            "--top-n",
            "20",
            "--min-observations",
            "20",
            "--top-quantile",
            "0.2",
            "--factor-name-filter",
            "",
            "--report-suite",
            "core",
            "--data-quality-mode",
            "event_complete",
            "--min-event-complete-events",
            "20",
            "--min-event-complete-rows",
            "40",
            "--min-promotion-entry-fill-rate",
            "0.30",
            "--cwd",
            str(ROOT),
        ]

    def fake_binary(self, tmp: Path) -> Path:
        binary = tmp / "fake_factor_walk_forward.py"
        binary.write_text(
            "#!/usr/bin/env python3\n"
            "import sys\n"
            f"{FAKE_REPORT}\n",
            encoding="utf-8",
        )
        binary.chmod(0o755)
        return binary

    def fake_binary_with_report(self, tmp: Path, report: str) -> Path:
        binary = tmp / "fake_factor_walk_forward.py"
        binary.write_text(
            "#!/usr/bin/env python3\n"
            "import sys\n"
            f"{report}\n",
            encoding="utf-8",
        )
        binary.chmod(0o755)
        return binary

    def fake_binary_by_filter(self, tmp: Path) -> Path:
        binary = tmp / "fake_factor_walk_forward.py"
        binary.write_text(
            "#!/usr/bin/env python3\n"
            f"{FAKE_TRADEABLE_HARD_GATE_BY_FILTER}\n",
            encoding="utf-8",
        )
        binary.chmod(0o755)
        return binary

    def test_dry_run_expands_sweep_variants(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = self.fake_binary(tmp)
            sweep_json = json.dumps(
                [
                    {"label": "settlement", "factor_name_filter": "auto_settlement"},
                    {"label": "quantile-10", "top_quantile": 0.1},
                ]
            )
            subprocess.run(
                [*self.base_args(tmp, binary), "--sweep-json", sweep_json, "--dry-run"],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            summary = json.loads((tmp / "out" / "sweep-summary.json").read_text(encoding="utf-8"))

        self.assertTrue(summary["dry_run"])
        self.assertEqual([item["label"] for item in summary["variants"]], ["settlement", "quantile-10"])
        self.assertEqual(summary["variants"][0]["variant"]["factor_name_filter"], "auto_settlement")
        self.assertEqual(summary["variants"][1]["variant"]["top_quantile"], "0.1")

    def test_runs_variants_once_artifacts_are_prepared(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = self.fake_binary(tmp)
            sweep_json = json.dumps(
                [
                    {"label": "base"},
                    {"label": "settlement-only", "factor_name_filter": "auto_settlement"},
                ]
            )
            result = subprocess.run(
                [
                    *self.base_args(tmp, binary),
                    "--candidate-strategy-replay-json",
                    str(self.replay_file(tmp)),
                    "--sweep-json",
                    sweep_json,
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            summary = json.loads((tmp / "out" / "sweep-summary.json").read_text(encoding="utf-8"))
            summary_md = (tmp / "out" / "sweep-summary.md").read_text(encoding="utf-8")

        self.assertEqual(result.returncode, 0)
        self.assertEqual(summary["variant_count"], 2)
        self.assertEqual(summary["completed_count"], 2)
        self.assertEqual(summary["variants"][0]["decision"], "qualified")
        self.assertEqual(summary["variants"][0]["qualified_count"], 1)
        self.assertIn("auto_settlement_conservative_settlement_edge", summary_md)

    def test_external_replay_identity_mismatch_is_replaced_with_variant_replay(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = self.fake_binary(tmp)
            subprocess.run(
                [
                    *self.base_args(tmp, binary),
                    "--candidate-strategy-replay-json",
                    str(
                        self.replay_file(
                            tmp,
                            "autofactor_formula:stale_previous_candidate",
                        )
                    ),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )

            summary = json.loads((tmp / "out" / "sweep-summary.json").read_text(encoding="utf-8"))
            replay = json.loads(
                (tmp / "out" / "candidate-strategy-replay.json").read_text(encoding="utf-8")
            )
            promotion = json.loads(
                (tmp / "out" / "autofactor-strategy-promotion.json").read_text(
                    encoding="utf-8"
                )
            )

        variant = summary["variants"][0]
        self.assertIn(
            "candidate_strategy_replay_runtime_score_mismatch:"
            "autofactor_formula:stale_previous_candidate!="
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
            variant["ignored_candidate_strategy_replay_reasons"],
        )
        self.assertEqual(variant["candidate_replay_exit_code"], 0)
        self.assertEqual(replay["basis"], "factor_walk_forward_top_bucket_aggregate")
        self.assertEqual(
            replay["runtime_score"],
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
        )
        blockers = promotion["evaluated_factors"][0]["blockers"]
        self.assertIn(
            "candidate_strategy_replay_not_runtime_replay:"
            "factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
            blockers,
        )
        self.assertFalse(
            any(
                blocker.startswith("candidate_strategy_replay_runtime_score_mismatch:")
                for blocker in blockers
            )
        )

    def test_external_replay_matching_lower_rank_candidate_is_kept(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = self.fake_binary_with_report(tmp, FAKE_TWO_RUNTIME_MAPPABLE_REPORT)
            replay_path = self.replay_file(
                tmp,
                "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_spread_adjusted_capacity",
            )
            subprocess.run(
                [
                    *self.base_args(tmp, binary),
                    "--candidate-strategy-replay-json",
                    str(replay_path),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )

            summary = json.loads((tmp / "out" / "sweep-summary.json").read_text(encoding="utf-8"))
            replay = json.loads(
                (tmp / "out" / "candidate-strategy-replay.json").read_text(encoding="utf-8")
            )
            promotion = json.loads(
                (tmp / "out" / "autofactor-strategy-promotion.json").read_text(
                    encoding="utf-8"
                )
            )

        variant = summary["variants"][0]
        self.assertNotIn("ignored_candidate_strategy_replay_json", variant)
        self.assertEqual(replay["basis"], "runtime_market_update_replay")
        self.assertEqual(
            replay["runtime_score"],
            "autofactor_formula:mut_auto_settlement_model_full_depth_settlement_edge_spread_adjusted_capacity",
        )
        self.assertEqual(
            promotion["candidate_strategy_replay"]["basis"],
            "runtime_market_update_replay",
        )
        self.assertEqual(variant["qualified_count"], 1)

    def test_builds_blocked_aggregate_candidate_replay_when_missing(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = self.fake_binary(tmp)
            subprocess.run(
                self.base_args(tmp, binary),
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )

            summary = json.loads((tmp / "out" / "sweep-summary.json").read_text(encoding="utf-8"))
            root_replay = json.loads(
                (tmp / "out" / "candidate-strategy-replay.json").read_text(encoding="utf-8")
            )
            variant_replay = json.loads(
                (tmp / "out" / "001-base" / "candidate-strategy-replay.json").read_text(
                    encoding="utf-8"
                )
            )

        self.assertEqual(summary["best_variant"], "base")
        self.assertEqual(summary["variants"][0]["candidate_replay_exit_code"], 0)
        self.assertEqual(summary["variants"][0]["decision"], "blocked")
        self.assertEqual(summary["variants"][0]["qualified_count"], 0)
        self.assertFalse(root_replay["promotion_ready"])
        self.assertEqual(root_replay["basis"], "factor_walk_forward_top_bucket_aggregate")
        self.assertEqual(
            root_replay["runtime_score"],
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
        )
        self.assertIn(
            "requires_runtime_replay_not_top_bucket_aggregate",
            root_replay["blocking_risk_flags"],
        )
        self.assertEqual(root_replay, variant_replay)

    def test_promotes_alpha_search_artifacts_from_best_variant(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = tmp / "fake_factor_walk_forward.py"
            binary.write_text(
                "#!/usr/bin/env python3\n"
                "import pathlib, sys\n"
                "args = sys.argv[1:]\n"
                "if '--alpha-search-output-dir' in args:\n"
                "    out = pathlib.Path(args[args.index('--alpha-search-output-dir') + 1])\n"
                "    out.mkdir(parents=True, exist_ok=True)\n"
                "    filt = args[args.index('--factor-name-filter') + 1] if '--factor-name-filter' in args else ''\n"
                "    (out / 'marker.txt').write_text(filt or '<empty>', encoding='utf-8')\n"
                f"{FAKE_REPORT}\n",
                encoding="utf-8",
            )
            binary.chmod(0o755)
            sweep_json = json.dumps(
                [
                    {"label": "base"},
                    {"label": "settlement-only", "factor_name_filter": "auto_settlement"},
                ]
            )

            subprocess.run(
                [
                    *self.base_args(tmp, binary),
                    "--candidate-strategy-replay-json",
                    str(self.replay_file(tmp)),
                    "--sweep-json",
                    sweep_json,
                    "--alpha-search-output-dir",
                    str(tmp / "out" / "alpha-search"),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )

            root_marker = (tmp / "out" / "alpha-search" / "marker.txt").read_text(
                encoding="utf-8"
            )
            first_marker = (
                tmp / "out" / "001-base" / "alpha-search" / "marker.txt"
            ).read_text(encoding="utf-8")
            second_marker = (
                tmp / "out" / "002-settlement-only" / "alpha-search" / "marker.txt"
            ).read_text(encoding="utf-8")

        self.assertEqual(root_marker, "<empty>")
        self.assertEqual(first_marker, "<empty>")
        self.assertEqual(second_marker, "auto_settlement")

    def test_runtime_contract_preview_selection_matches_allowed_target(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            factor_name = "mut_spread_adjusted_external_move_full_depth_entry_gate"
            binary = tmp / "fake_factor_walk_forward.py"
            binary.write_text(
                "#!/usr/bin/env python3\n"
                "import json, pathlib, sys\n"
                "args = sys.argv[1:]\n"
                "if '--alpha-search-output-dir' in args:\n"
                "    root = pathlib.Path(args[args.index('--alpha-search-output-dir') + 1])\n"
                "    def write_preview(target, factors):\n"
                "        out = root / target\n"
                "        out.mkdir(parents=True, exist_ok=True)\n"
                "        (out / 'factor-registry-preview.json').write_text(\n"
                "            json.dumps({\n"
                "                'version': 'alpha_search_artifacts_v1',\n"
                "                'target': target,\n"
                "                'horizon': '5m',\n"
                "                'factors': factors,\n"
                "            }, indent=2, sort_keys=True),\n"
                "            encoding='utf-8',\n"
                "        )\n"
                "    contract = {\n"
                "        'version': 'autofactor_runtime_contract_v1',\n"
                "        'dsl_hash': 'dsl:tradeable',\n"
                "        'ast_json': {'op': 'mul'},\n"
                "        'runtime_score': 'autofactor_formula:"
                f"{factor_name}',\n"
                "        'strategy_profile': 'settlement_probability',\n"
                "        'strategy_family': 'predictive_settlement_probability',\n"
                "        'input_names': ['external_move_since_poly_update'],\n"
                "        'ast_input_names': ['external_move_since_poly_update'],\n"
                "        'runtime_input_names': ['direction_sign', 'drift_30s'],\n"
                "        'input_mappings': [\n"
                "            {'ast_input': 'external_move_since_poly_update', 'runtime_input': 'direction_sign'},\n"
                "            {'ast_input': 'external_move_since_poly_update', 'runtime_input': 'drift_30s'},\n"
                "        ],\n"
                "        'blockers': [],\n"
                "    }\n"
                "    write_preview('full_depth_settlement_executable_pnl', [\n"
                "        {'factor_name': 'auto_settlement_conservative_settlement_edge', 'blockers': []}\n"
                "    ])\n"
                "    write_preview('tradeable_full_depth_settlement_pnl', [\n"
                f"        {{'factor_name': {factor_name!r}, 'runtime_contract': contract, 'blockers': []}}\n"
                "    ])\n"
                f"{FAKE_TRADEABLE_HARD_GATE_BY_FILTER}\n",
                encoding="utf-8",
            )
            binary.chmod(0o755)

            subprocess.run(
                [
                    *self.base_args(tmp, binary),
                    "--factor-name-filter",
                    "external_move",
                    "--allowed-target",
                    "tradeable_full_depth_settlement_pnl",
                    "--alpha-search-output-dir",
                    str(tmp / "out" / "alpha-search"),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            summary = json.loads((tmp / "out" / "sweep-summary.json").read_text(encoding="utf-8"))
            replay = json.loads(
                (tmp / "out" / "candidate-strategy-replay.json").read_text(encoding="utf-8")
            )
            promotion = json.loads(
                (tmp / "out" / "autofactor-strategy-promotion.json").read_text(
                    encoding="utf-8"
                )
            )

        self.assertEqual(
            replay["runtime_score"],
            f"autofactor_formula:{factor_name}",
        )
        self.assertNotIn(
            f"missing_runtime_contract:{factor_name}",
            replay["blocking_risk_flags"],
        )
        self.assertEqual(
            summary["variants"][0]["best_runtime_mappable_factor"]["name"],
            factor_name,
        )
        evaluated = promotion["evaluated_factors"][0]
        self.assertEqual(evaluated["factor"]["name"], factor_name)
        self.assertEqual(
            evaluated["runtime_mapping"]["runtime_score"],
            f"autofactor_formula:{factor_name}",
        )
        self.assertNotIn(f"missing_runtime_contract:{factor_name}", evaluated["blockers"])

    def test_runtime_contract_preview_selection_fails_closed_on_target_mismatch(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = tmp / "fake_factor_walk_forward.py"
            binary.write_text(
                "#!/usr/bin/env python3\n"
                "import json, pathlib, sys\n"
                "args = sys.argv[1:]\n"
                "if '--alpha-search-output-dir' in args:\n"
                "    root = pathlib.Path(args[args.index('--alpha-search-output-dir') + 1])\n"
                "    out = root / 'full_depth_settlement_executable_pnl'\n"
                "    out.mkdir(parents=True, exist_ok=True)\n"
                "    (out / 'factor-registry-preview.json').write_text(\n"
                "        json.dumps({\n"
                "            'version': 'alpha_search_artifacts_v1',\n"
                "            'target': 'full_depth_settlement_executable_pnl',\n"
                "            'horizon': '5m',\n"
                "            'factors': [],\n"
                "        }),\n"
                "        encoding='utf-8',\n"
                "    )\n"
                f"{FAKE_TRADEABLE_HARD_GATE_BY_FILTER}\n",
                encoding="utf-8",
            )
            binary.chmod(0o755)

            result = subprocess.run(
                [
                    *self.base_args(tmp, binary),
                    "--factor-name-filter",
                    "external_move",
                    "--allowed-target",
                    "tradeable_full_depth_settlement_pnl",
                    "--alpha-search-output-dir",
                    str(tmp / "out" / "alpha-search"),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
            )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn(
            "no factor registry preview matched allowed target(s): "
            "tradeable_full_depth_settlement_pnl",
            result.stderr,
        )
        self.assertIn("full_depth_settlement_executable_pnl", result.stderr)

    def test_summary_marks_predictive_formula_mutation_runtime_mappable(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = self.fake_binary_with_report(tmp, FAKE_UNMAPPED_BEST_REPORT)
            subprocess.run(
                self.base_args(tmp, binary),
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            summary = json.loads((tmp / "out" / "sweep-summary.json").read_text(encoding="utf-8"))
            summary_md = (tmp / "out" / "sweep-summary.md").read_text(encoding="utf-8")

        variant = summary["variants"][0]
        self.assertEqual(
            variant["best_discovery_factor"]["name"],
            "mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
        )
        self.assertNotIn(
            "missing_runtime_strategy_mapping",
            variant["best_discovery_factor"]["blockers"],
        )
        self.assertEqual(
            variant["best_runtime_mappable_factor"]["name"],
            "mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
        )
        self.assertEqual(
            variant["best_runtime_mappable_factor"]["top_bucket_avg_label"],
            1.660059,
        )
        self.assertEqual(variant["best_runtime_mappable_factor"]["complexity"], 6)
        self.assertEqual(
            variant["best_runtime_mappable_factor"]["runtime_mapping"]["runtime_score"],
            "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_spread_adjusted",
        )
        self.assertEqual(variant["qualified_count"], 0)
        self.assertEqual(variant["decision"], "blocked")
        self.assertIsNone(variant["best_qualified_strategy"])
        self.assertIn("best runtime-mappable factor", summary_md)

    def test_best_variant_prefers_tradeable_profit_metrics_over_rank_ic(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = self.fake_binary_by_filter(tmp)
            sweep_json = json.dumps(
                [
                    {"label": "amplitude", "factor_name_filter": "amplitude_weighted"},
                    {"label": "external", "factor_name_filter": "external_move"},
                ]
            )
            subprocess.run(
                [
                    *self.base_args(tmp, binary),
                    "--candidate-strategy-replay-json",
                    str(
                        self.replay_file(
                            tmp,
                            "autofactor_formula:mut_spread_adjusted_external_move_full_depth_entry_gate",
                            "tradeable_full_depth_settlement_pnl",
                        )
                    ),
                    "--allowed-target",
                    "tradeable_full_depth_settlement_pnl",
                    "--sweep-json",
                    sweep_json,
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            summary = json.loads((tmp / "out" / "sweep-summary.json").read_text(encoding="utf-8"))
            handoff = json.loads(
                (tmp / "out" / "autofactor-strategy-handoff.json").read_text(encoding="utf-8")
            )

        self.assertEqual(summary["best_variant"], "external")
        self.assertEqual(
            handoff["strategies"][0]["name"],
            "mut_spread_adjusted_external_move_full_depth_entry_gate",
        )
        self.assertEqual(handoff["strategies"][0]["metrics"]["top_bucket_avg_label"], 1.388814)

    def test_hosted_workflow_passes_empty_factor_filter_to_sweep_runner(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")

        self.assertIn("--sweep-json \"${SWEEP_JSON}\"", workflow)
        self.assertIn("--factor-name-filter \"${WALK_FACTOR_NAME_FILTER}\"", workflow)
        self.assertIn("--train-window-hours \"${WALK_TRAIN_WINDOW_HOURS}\"", workflow)
        self.assertIn("--candidate-strategy-replay-json", workflow)
        self.assertIn("--snapshot-manifest-json artifacts/research-snapshot/manifest.json", workflow)
        self.assertIn("--snapshot-data-audit-json artifacts/research-snapshot/data-gap-audit.json", workflow)
        self.assertIn("full_depth_execution_surface_run_id", workflow)
        self.assertIn("Download full-depth execution surface artifact", workflow)
        self.assertIn('[[ "${artifact_name}" == factor-walk-forward-v2-* ]]', workflow)
        self.assertIn("--strip-prefix full-depth-execution-surface", workflow)
        self.assertIn("--full-depth-execution-surface-json", workflow)

    def test_snapshot_manifest_passes_to_candidate_replay_and_promotion(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            snapshot_dir = tmp / "snapshot"
            snapshot_dir.mkdir()
            manifest = snapshot_dir / "manifest.json"
            manifest.write_text(
                json.dumps(
                    {
                        "schema_version": "research_snapshot_manifest_v1",
                        "snapshot_hash": "snapshot:sampled-execution",
                        "source_kind": "complete_sampled_research_snapshot",
                        "source_surfaces": [
                            {
                                "name": "clob_orderbook_snapshots",
                                "gate_category": "required_for_execution",
                                "raw_full_fidelity": True,
                                "snapshot_sampled": True,
                            }
                        ],
                    },
                    indent=2,
                    sort_keys=True,
                ),
                encoding="utf-8",
            )
            binary = self.fake_binary(tmp)
            subprocess.run(
                [
                    *self.base_args(tmp, binary),
                    "--snapshot-manifest-json",
                    str(manifest),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            replay = json.loads(
                (tmp / "out" / "candidate-strategy-replay.json").read_text(encoding="utf-8")
            )
            promotion = json.loads(
                (tmp / "out" / "autofactor-strategy-promotion.json").read_text(
                    encoding="utf-8"
                )
            )

        expected_flags = [
            "sampled_snapshot_required_for_execution_surface:clob_orderbook_snapshots"
        ]
        self.assertEqual(replay["source_snapshot_contract"]["blocking_risk_flags"], expected_flags)
        self.assertEqual(
            promotion["source_snapshot_contract"]["blocking_risk_flags"], expected_flags
        )

    def test_full_depth_execution_surface_passes_to_candidate_replay_and_promotion(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            snapshot_dir = tmp / "snapshot"
            snapshot_dir.mkdir()
            manifest = snapshot_dir / "manifest.json"
            manifest.write_text(
                json.dumps(
                    {
                        "schema_version": "research_snapshot_manifest_v1",
                        "snapshot_hash": "snapshot:sampled-execution",
                        "source_kind": "complete_sampled_research_snapshot",
                        "start": "2026-04-24T00:00:00Z",
                        "end": "2026-05-01T00:00:00Z",
                        "source_surfaces": [
                            {
                                "name": "clob_orderbook_snapshots",
                                "gate_category": "required_for_execution",
                                "raw_full_fidelity": True,
                                "snapshot_sampled": True,
                            }
                        ],
                    },
                    indent=2,
                    sort_keys=True,
                ),
                encoding="utf-8",
            )
            proof = tmp / "full-depth-execution-surface.json"
            proof.write_text(
                json.dumps(
                    {
                        "schema_version": "full_depth_execution_surface.v1",
                        "surface": "clob_orderbook_snapshots",
                        "source": "orderbook_snapshot_archive",
                        "start_ts": "2026-04-24T00:00:00Z",
                        "end_ts": "2026-05-01T00:00:00Z",
                        "checked_hours": 168,
                        "existing_hours": 168,
                        "row_count": 1000,
                        "full_fidelity": True,
                        "incomplete": False,
                    },
                    indent=2,
                    sort_keys=True,
                ),
                encoding="utf-8",
            )
            binary = self.fake_binary(tmp)
            subprocess.run(
                [
                    *self.base_args(tmp, binary),
                    "--snapshot-manifest-json",
                    str(manifest),
                    "--full-depth-execution-surface-json",
                    str(proof),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            replay = json.loads(
                (tmp / "out" / "candidate-strategy-replay.json").read_text(encoding="utf-8")
            )
            promotion = json.loads(
                (tmp / "out" / "autofactor-strategy-promotion.json").read_text(
                    encoding="utf-8"
                )
            )

        self.assertEqual(replay["source_snapshot_contract"]["blocking_risk_flags"], [])
        self.assertEqual(
            promotion["source_snapshot_contract"]["satisfied_execution_surfaces"],
            ["clob_orderbook_snapshots"],
        )

    def test_alpha_search_prior_and_state_args_pass_through_to_factor_binary(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = tmp / "capture_factor_args.py"
            capture = tmp / "captured_args.json"
            binary.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, sys\n"
                f"open({str(capture)!r}, 'w', encoding='utf-8').write(json.dumps(sys.argv[1:]))\n"
                f"{FAKE_REPORT}\n",
                encoding="utf-8",
            )
            binary.chmod(0o755)

            subprocess.run(
                [
                    *self.base_args(tmp, binary),
                    "--alpha-search-output-dir",
                    "artifacts/alpha",
                    "--alpha-search-plan-json",
                    "artifacts/plan/mcts-expansion-plan.json",
                    "--alpha-search-state-json",
                    "artifacts/plan/mcts-state.json",
                    "--alpha-search-llm-prior-json",
                    "tasks/alpha_search_priors/pm5d_settlement_liquidity_prior_20260512.json",
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            captured = json.loads(capture.read_text(encoding="utf-8"))

        self.assertIn("--alpha-search-output-dir", captured)
        alpha_index = captured.index("--alpha-search-output-dir")
        self.assertIn("001-base/alpha-search", captured[alpha_index + 1])
        self.assertIn("--alpha-search-plan-json", captured)
        self.assertIn("--alpha-search-state-json", captured)
        self.assertIn("--alpha-search-llm-prior-json", captured)

    def test_alpha_zoo_snapshot_arg_passes_through_to_factor_binary(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = tmp / "capture_factor_args.py"
            capture = tmp / "captured_args.json"
            binary.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, sys\n"
                f"open({str(capture)!r}, 'w', encoding='utf-8').write(json.dumps(sys.argv[1:]))\n"
                f"{FAKE_REPORT}\n",
                encoding="utf-8",
            )
            binary.chmod(0o755)

            subprocess.run(
                [
                    *self.base_args(tmp, binary),
                    "--alpha-zoo-snapshot-json",
                    "artifacts/alpha-zoo/alpha-zoo-snapshot.json",
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            captured = json.loads(capture.read_text(encoding="utf-8"))

        self.assertIn("--alpha-zoo-snapshot-json", captured)
        zoo_index = captured.index("--alpha-zoo-snapshot-json")
        self.assertEqual(
            captured[zoo_index + 1], "artifacts/alpha-zoo/alpha-zoo-snapshot.json"
        )

    def test_alpha_zoo_snapshot_arg_omitted_when_not_provided(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = tmp / "capture_factor_args.py"
            capture = tmp / "captured_args.json"
            binary.write_text(
                "#!/usr/bin/env python3\n"
                "import json, os, sys\n"
                f"open({str(capture)!r}, 'w', encoding='utf-8').write(json.dumps(sys.argv[1:]))\n"
                f"{FAKE_REPORT}\n",
                encoding="utf-8",
            )
            binary.chmod(0o755)

            subprocess.run(
                self.base_args(tmp, binary),
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            captured = json.loads(capture.read_text(encoding="utf-8"))

        self.assertNotIn("--alpha-zoo-snapshot-json", captured)

    def test_require_deribit_arg_passes_through_to_factor_binary(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = tmp / "capture_factor_args.py"
            capture = tmp / "captured_args.json"
            binary.write_text(
                "#!/usr/bin/env python3\n"
                "import json, sys\n"
                f"open({str(capture)!r}, 'w', encoding='utf-8').write(json.dumps(sys.argv[1:]))\n"
                f"{FAKE_REPORT}\n",
                encoding="utf-8",
            )
            binary.chmod(0o755)

            subprocess.run(
                [*self.base_args(tmp, binary), "--require-deribit"],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            captured = json.loads(capture.read_text(encoding="utf-8"))

        self.assertIn("--require-deribit", captured)

    def test_hour_window_args_pass_through_to_factor_binary(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = tmp / "capture_factor_args.py"
            capture = tmp / "captured_args.json"
            binary.write_text(
                "#!/usr/bin/env python3\n"
                "import json, sys\n"
                f"open({str(capture)!r}, 'w', encoding='utf-8').write(json.dumps(sys.argv[1:]))\n"
                f"{FAKE_REPORT}\n",
                encoding="utf-8",
            )
            binary.chmod(0o755)

            subprocess.run(
                [
                    *self.base_args(tmp, binary),
                    "--train-window-hours",
                    "12",
                    "--test-window-hours",
                    "12",
                    "--step-hours",
                    "12",
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            captured = json.loads(capture.read_text(encoding="utf-8"))

        self.assertIn("--train-window-hours", captured)
        self.assertIn("--test-window-hours", captured)
        self.assertIn("--step-hours", captured)

    def test_promotion_entry_fill_rate_arg_passes_through_to_factor_binary(self):
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            (tmp / "snapshot").mkdir()
            binary = tmp / "capture_factor_args.py"
            capture = tmp / "captured_args.json"
            binary.write_text(
                "#!/usr/bin/env python3\n"
                "import json, sys\n"
                f"open({str(capture)!r}, 'w', encoding='utf-8').write(json.dumps(sys.argv[1:]))\n"
                f"{FAKE_REPORT}\n",
                encoding="utf-8",
            )
            binary.chmod(0o755)

            subprocess.run(
                [*self.base_args(tmp, binary)],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=True,
            )
            captured = json.loads(capture.read_text(encoding="utf-8"))

        self.assertIn("--min-promotion-entry-fill-rate", captured)
        index = captured.index("--min-promotion-entry-fill-rate")
        self.assertEqual(captured[index + 1], "0.30")


if __name__ == "__main__":
    unittest.main()
