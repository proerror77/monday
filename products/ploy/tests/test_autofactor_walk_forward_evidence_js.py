import json
import subprocess
import tempfile
import textwrap
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / ".github" / "scripts" / "autofactor-walk-forward-evidence.js"


class AutoFactorWalkForwardEvidenceJsTest(unittest.TestCase):
    def run_builder(self, artifact_dir: Path) -> dict:
        js = textwrap.dedent(
            f"""
            const {{ buildWalkForwardEvidence }} = require({json.dumps(str(SCRIPT))});
            const result = buildWalkForwardEvidence({{
              title: "Hosted factor walk-forward evidence",
              artifactDir: {json.dumps(str(artifact_dir))},
              runnerLabel: "runner:test",
              metadata: {{
                workflow: "Factor Walk-Forward V2 Hosted Artifact",
                runUrl: "https://github.example/run/1",
                gitRef: "test-ref",
                snapshotRunId: "123",
                startDate: "2026-05-18T00:00:00Z",
                endDate: "2026-05-20T00:00:00Z",
                symbols: "BTCUSDT,ETHUSDT",
                artifactName: "factor-walk-forward-v2-1",
              }},
            }});
            process.stdout.write(JSON.stringify(result));
            """
        )
        result = subprocess.run(
            ["node", "-e", js],
            cwd=ROOT,
            text=True,
            capture_output=True,
            check=True,
        )
        return json.loads(result.stdout)

    def test_uses_candidate_blockers_when_global_low_but_top_bucket_fills(
        self,
    ):
        with tempfile.TemporaryDirectory() as tmp:
            artifact_dir = Path(tmp)
            (artifact_dir / "autofactor-strategy-handoff.json").write_text(
                json.dumps(
                    {
                        "status": "blocked",
                        "promotion_gate": {
                            "ready": False,
                            "blocked_gates": [
                                "global_full_depth_entry_fillability: "
                                "global_full_depth_entry_fill_rate=0.1282 "
                                "min_required=0.3000"
                            ],
                        },
                    }
                ),
                encoding="utf-8",
            )
            (artifact_dir / "autofactor-strategy-promotion.json").write_text(
                json.dumps(
                    {
                        "decision": "blocked",
                        "minimums": {
                            "top_bucket_full_depth_entry_fill_rate": 0.30,
                        },
                        "promotion_gate": {
                            "ready": False,
                            "blocked_gates": [
                                "global_full_depth_entry_fillability: "
                                "global_full_depth_entry_fill_rate=0.1282 "
                                "min_required=0.3000"
                            ],
                        },
                        "evaluated_factors": [
                            {
                                "qualified": False,
                                "blockers": [
                                    "promotion_gate_not_ready",
                                    "missing_runtime_strategy_mapping",
                                    "candidate_strategy_replay_not_runtime_replay:"
                                    "factor_walk_forward_top_bucket_aggregate!="
                                    "runtime_market_update_replay",
                                ],
                                "factor": {
                                    "name": (
                                        "mut_bayes_model_contrarian_"
                                        "confidence_weighted_edge_"
                                        "select_entry_price_quality_ge_050"
                                    ),
                                    "decision": "candidate",
                                    "reason": "passed",
                                    "icir": 1.305048,
                                    "top_bucket_avg_label": 2.338923,
                                    "top_bucket_full_depth_entry_fill_rate": 1.0,
                                },
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )

            result = self.run_builder(artifact_dir)

        self.assertEqual(result["decision"], "fix-runtime")
        self.assertIn("missing_runtime_strategy_mapping", result["body"])
        self.assertIn("candidate_strategy_replay_not_runtime_replay", result["body"])
        self.assertNotIn("global_full_depth_entry_fillability", result["body"])

    def test_keeps_data_quality_gate_as_actionable_blocker(self):
        with tempfile.TemporaryDirectory() as tmp:
            artifact_dir = Path(tmp)
            (artifact_dir / "autofactor-strategy-handoff.json").write_text(
                json.dumps(
                    {
                        "status": "blocked",
                        "promotion_gate": {
                            "ready": False,
                            "blocked_gates": [
                                "data_quality: mode=event_complete "
                                "event_complete_events=0 min_events=20"
                            ],
                        },
                    }
                ),
                encoding="utf-8",
            )
            (artifact_dir / "autofactor-strategy-promotion.json").write_text(
                json.dumps(
                    {
                        "decision": "blocked",
                        "promotion_gate": {
                            "ready": False,
                            "blocked_gates": [
                                "data_quality: mode=event_complete "
                                "event_complete_events=0 min_events=20"
                            ],
                        },
                        "evaluated_factors": [
                            {
                                "qualified": False,
                                "blockers": ["missing_runtime_strategy_mapping"],
                                "factor": {
                                    "decision": "candidate",
                                    "reason": "passed",
                                    "top_bucket_full_depth_entry_fill_rate": 1.0,
                                },
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )

            result = self.run_builder(artifact_dir)

        self.assertEqual(result["decision"], "fix-data")
        self.assertIn("data_quality", result["body"])

    def test_closed_loop_revise_prior_overrides_runtime_mapping_noise(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            artifact_dir = root / "factor-walk-forward-v2"
            artifact_dir.mkdir()
            (root / "alpha-search-chain").mkdir()
            (root / "alpha-search-chain" / "closed-loop-decision.json").write_text(
                json.dumps(
                    {
                        "action": "revise_prior",
                        "reason": "missing_runtime_strategy_mapping",
                    }
                ),
                encoding="utf-8",
            )
            (artifact_dir / "autofactor-strategy-handoff.json").write_text(
                json.dumps(
                    {
                        "status": "blocked",
                        "promotion_gate": {
                            "ready": False,
                            "blocked_gates": [
                                "walk_forward_oos: no non-naive model passed",
                            ],
                        },
                    }
                ),
                encoding="utf-8",
            )
            (artifact_dir / "autofactor-strategy-promotion.json").write_text(
                json.dumps(
                    {
                        "decision": "blocked",
                        "minimums": {
                            "top_bucket_full_depth_entry_fill_rate": 0.30,
                        },
                        "promotion_gate": {
                            "ready": False,
                            "blocked_gates": [
                                "walk_forward_oos: no non-naive model passed",
                            ],
                        },
                        "evaluated_factors": [
                            {
                                "qualified": False,
                                "blockers": [
                                    "runtime_contract_unmapped_factor",
                                    "missing_runtime_strategy_mapping",
                                ],
                                "factor": {
                                    "decision": "candidate",
                                    "reason": "passed",
                                    "top_bucket_full_depth_entry_fill_rate": 1.0,
                                },
                            }
                        ],
                    }
                ),
                encoding="utf-8",
            )

            result = self.run_builder(artifact_dir)

        self.assertEqual(result["decision"], "revise")
        self.assertIn("- Decision: revise", result["body"])
        self.assertIn("- Next action: missing_runtime_strategy_mapping", result["body"])
        self.assertIn("- Actionable blockers: `missing_runtime_strategy_mapping`", result["body"])

    def test_reads_closed_loop_from_workflow_staged_upload_dir(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            artifacts = root / "artifacts"
            artifact_dir = artifacts / "factor-walk-forward-v2"
            artifact_dir.mkdir(parents=True)
            staged_alpha = (
                artifacts
                / "factor-walk-forward-v2-upload"
                / "alpha-search-chain"
            )
            staged_alpha.mkdir(parents=True)
            (staged_alpha / "closed-loop-decision.json").write_text(
                json.dumps(
                    {
                        "action": "fix_data",
                        "reason": "promotion_blockers_require_fix_data",
                    }
                ),
                encoding="utf-8",
            )
            (artifact_dir / "autofactor-strategy-handoff.json").write_text(
                json.dumps(
                    {
                        "status": "blocked",
                        "promotion_gate": {
                            "ready": False,
                            "blocked_gates": [
                                "incomplete_runtime_contract_mapping:factor",
                                "runtime_contract_unmapped_factor",
                            ],
                        },
                    }
                ),
                encoding="utf-8",
            )
            (artifact_dir / "autofactor-strategy-promotion.json").write_text(
                json.dumps(
                    {
                        "decision": "blocked",
                        "promotion_gate": {
                            "ready": False,
                            "blocked_gates": [
                                "incomplete_runtime_contract_mapping:factor",
                                "runtime_contract_unmapped_factor",
                            ],
                        },
                    }
                ),
                encoding="utf-8",
            )

            result = self.run_builder(artifact_dir)

        self.assertEqual(result["decision"], "fix-data")
        self.assertIn("- Decision: fix-data", result["body"])
        self.assertIn("- Next action: promotion_blockers_require_fix_data", result["body"])
        self.assertIn("- Actionable blockers: `promotion_blockers_require_fix_data`", result["body"])
        self.assertNotIn("runtime_contract_unmapped_factor", result["body"])


if __name__ == "__main__":
    unittest.main()
