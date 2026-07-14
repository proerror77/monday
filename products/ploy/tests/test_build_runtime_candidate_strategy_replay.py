import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "build_runtime_candidate_strategy_replay.py"


def runtime_eval_payload(*, settled=True, pnl="12.5"):
    settlement = "1" if settled else "open"
    return {
        "artifact_type": "strategy_runtime_evaluation",
        "result": {
            "updates_processed": 1000,
            "intents_submitted": 2,
            "fills_recorded": 2,
            "strategy_diagnostics": {"candidate_events": 2},
        },
        "runtime_evidence": {
            "basis": "trading_runtime_snapshot",
            "intents": [
                {
                    "intent_id": "intent-1",
                    "event_id": "event-1",
                    "quantity": "10",
                    "limit_price": "0.42",
                },
                {
                    "intent_id": "intent-2",
                    "event_id": "event-2",
                    "quantity": "10",
                    "limit_price": "0.43",
                },
            ],
            "orders": [
                {
                    "intent_id": "intent-1",
                    "order_id": "order-1",
                    "event_id": "event-1",
                    "purpose": "ENTRY",
                    "quantity": "10",
                    "filled_quantity": "10",
                    "status": "FILLED",
                },
                {
                    "intent_id": "intent-2",
                    "order_id": "order-2",
                    "event_id": "event-2",
                    "purpose": "ENTRY",
                    "quantity": "10",
                    "filled_quantity": "10",
                    "status": "FILLED",
                },
            ],
            "fills": [
                {
                    "intent_id": "intent-1",
                    "order_id": "order-1",
                    "event_id": "event-1",
                    "purpose": "ENTRY",
                    "quantity": "10",
                    "price": "0.42",
                    "fee": "0",
                },
                {
                    "intent_id": "intent-2",
                    "order_id": "order-2",
                    "event_id": "event-2",
                    "purpose": "ENTRY",
                    "quantity": "10",
                    "price": "0.43",
                    "fee": "0",
                },
            ],
            "events": [
                {
                    "intent_id": "intent-1",
                    "order_id": "order-1",
                    "event_id": "event-1",
                    "signal_inputs": {"purpose": "ENTRY"},
                    "settlement": settlement,
                    "pnl": pnl,
                },
                {
                    "intent_id": "intent-2",
                    "order_id": "order-2",
                    "event_id": "event-2",
                    "signal_inputs": {"purpose": "ENTRY"},
                    "settlement": settlement,
                    "pnl": "0",
                },
            ],
        },
    }


class BuildRuntimeCandidateStrategyReplayTests(unittest.TestCase):
    def run_script(self, payload, *extra_args):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            runtime_json = tmp_path / "runtime-eval.json"
            output_json = tmp_path / "candidate-strategy-replay.json"
            output_md = tmp_path / "candidate-strategy-replay.md"
            runtime_json.write_text(json.dumps(payload), encoding="utf-8")
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--runtime-evaluation-json",
                    str(runtime_json),
                    "--runtime-score",
                    "autofactor_formula:auto_settlement_conservative_settlement_edge",
                    "--recording-path",
                    "/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.20260524T155939.ndjson",
                    "--recording-sha256",
                    "a" * 64,
                    "--output-json",
                    str(output_json),
                    "--output-md",
                    str(output_md),
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

    def test_builds_ready_runtime_market_update_replay_artifact(self):
        payload, markdown = self.run_script(
            runtime_eval_payload(),
            "--full-depth-entry",
            "--min-trade-count",
            "2",
            "--source-target",
            "full_depth_settlement_executable_pnl",
            "--source-horizon",
            "5m",
        )

        self.assertTrue(payload["promotion_ready"])
        self.assertRegex(payload["candidate_replay_id"], r"^candidate_replay:[0-9a-f]{32}$")
        self.assertEqual(payload["basis"], "runtime_market_update_replay")
        self.assertEqual(payload["evidence_stage"], "executable_replay")
        self.assertEqual(payload["promotion_decision"], "promote_to_runtime")
        self.assertEqual(payload["source_workflow"], "runtime-candidate-replay.yml")
        self.assertEqual(payload["identity"]["runtime_score"], payload["runtime_score"])
        self.assertEqual(payload["identity"]["basis"], "runtime_market_update_replay")
        self.assertEqual(payload["identity"]["recording_sha256"], "a" * 64)
        self.assertEqual(payload["source_factor"]["target"], "full_depth_settlement_executable_pnl")
        self.assertEqual(payload["source_factor"]["horizon"], "5m")
        self.assertEqual(payload["decision_contract"]["target"], "full_depth_settlement_executable_pnl")
        self.assertEqual(payload["decision_contract"]["horizon"], "5m")
        self.assertEqual(len(payload["runtime_evaluation_sha256"]), 64)
        self.assertEqual(payload["metrics"]["trade_count"], 2)
        self.assertEqual(payload["metrics"]["unique_event_count"], 2)
        self.assertEqual(payload["metrics"]["settlement_event_count"], 2)
        self.assertEqual(payload["metrics"]["entry_fill_rate"], 1.0)
        self.assertGreater(payload["metrics"]["roi"], 0)
        self.assertEqual(
            payload["acceptance_criteria"],
            {
                "full_depth_entry": True,
                "min_fill_rate": 0.3,
                "min_roi": 0.0,
                "min_trade_count": 2,
            },
        )
        self.assertEqual(payload["blocking_risk_flags"], [])
        self.assertIn("Promotion ready: `true`", markdown)
        self.assertIn("## Acceptance Criteria", markdown)

    def test_blocks_mutable_recording_without_sha256(self):
        payload, _ = self.run_script(
            runtime_eval_payload(),
            "--full-depth-entry",
            "--min-trade-count",
            "2",
            "--recording-path",
            "/opt/ploy/data/recordings/pm5d-threelayer-settlement-probability-btc-eth.ndjson",
            "--recording-sha256",
            "",
        )

        self.assertFalse(payload["promotion_ready"])
        self.assertIn("recording_sha256_missing", payload["blocking_risk_flags"])
        self.assertIn("mutable_recording_without_sha256", payload["blocking_risk_flags"])

    def test_resolves_source_horizon_from_shared_catalog(self):
        payload, _ = self.run_script(
            runtime_eval_payload(),
            "--full-depth-entry",
            "--min-trade-count",
            "2",
            "--source-target",
            "full_depth_settlement_executable_pnl",
        )

        self.assertEqual(payload["source_factor"]["horizon"], "5m")
        self.assertEqual(payload["decision_contract"]["horizon"], "5m")

    def test_blocks_source_horizon_mismatch(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            runtime_json = tmp_path / "runtime-eval.json"
            output_json = tmp_path / "candidate-strategy-replay.json"
            runtime_json.write_text(json.dumps(runtime_eval_payload()), encoding="utf-8")
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--runtime-evaluation-json",
                    str(runtime_json),
                    "--runtime-score",
                    "autofactor_formula:auto_settlement_conservative_settlement_edge",
                    "--output-json",
                    str(output_json),
                    "--source-target",
                    "full_depth_settlement_executable_pnl",
                    "--source-horizon",
                    "30s",
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=False,
            )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("source_horizon_mismatch:30s!=5m", result.stderr)

    def test_blocks_zero_intent_runtime_replay_with_diagnostics(self):
        payload, _ = self.run_script(
            {
                "result": {
                    "updates_processed": 172368,
                    "intents_submitted": 0,
                    "fills_recorded": 0,
                    "strategy_diagnostics": {
                        "candidate_events": 260,
                        "skip_edge_score": 183,
                    },
                },
                "runtime_evidence": {
                    "basis": "trading_runtime_snapshot",
                    "events": [],
                    "orders": [],
                    "fills": [],
                    "intents": [],
                },
            },
            "--full-depth-entry",
            "--min-trade-count",
            "2",
        )

        self.assertFalse(payload["promotion_ready"])
        self.assertEqual(payload["metrics"]["updates_processed"], 172368)
        self.assertEqual(payload["metrics"]["trade_count"], 0)
        self.assertIn("trade_count_too_small:0<2", payload["blocking_risk_flags"])
        self.assertIn("zero_runtime_orders_and_fills", payload["blocking_risk_flags"])
        self.assertEqual(payload["strategy_diagnostics"]["skip_edge_score"], 183)

    def test_adds_score_counterfactual_from_runtime_diagnostics(self):
        payload, markdown = self.run_script(
            {
                "result": {
                    "updates_processed": 1000,
                    "intents_submitted": 0,
                    "strategy_diagnostics": {
                        "settlement_autofactor_formula_evaluations": 20,
                        "settlement_autofactor_depth_fillable": 18,
                        "skip_entry_score": 18,
                        "settlement_autofactor_predictive_score_ge_005": 12,
                        "settlement_autofactor_predictive_score_ge_010": 8,
                        "settlement_autofactor_predictive_score_ge_015": 3,
                        "settlement_autofactor_predictive_score_ge_025": 0,
                        "settlement_autofactor_predictive_reverse_score_ge_005": 2,
                        "settlement_autofactor_predictive_reverse_score_ge_010": 1,
                        "settlement_autofactor_predictive_reverse_score_ge_015": 0,
                        "settlement_autofactor_predictive_reverse_score_ge_025": 0,
                    },
                },
                "runtime_evidence": {
                    "basis": "trading_runtime_snapshot",
                    "events": [],
                    "orders": [],
                    "fills": [],
                    "intents": [],
                },
            },
            "--full-depth-entry",
            "--min-trade-count",
            "2",
        )

        counterfactual = payload["score_counterfactual"]
        self.assertEqual(counterfactual["formula_evaluations"], 20)
        self.assertEqual(counterfactual["depth_fillable"], 18)
        self.assertEqual(counterfactual["entry_score_skips"], 18)
        self.assertEqual(counterfactual["direct_pass_counts"]["0.15"], 3)
        self.assertEqual(counterfactual["direct_pass_counts"]["0.25"], 0)
        self.assertEqual(counterfactual["reverse_direction_pass_counts"]["0.10"], 1)
        self.assertEqual(
            counterfactual["diagnosis"],
            "direct_signal_exists_below_configured_threshold",
        )
        self.assertIn("## Score Counterfactual", markdown)
        self.assertIn("| `0.15` | `3` | `0` |", markdown)

    def test_counterfactual_uses_configured_entry_threshold(self):
        payload, _ = self.run_script(
            {
                "result": {
                    "updates_processed": 1000,
                    "intents_submitted": 0,
                    "strategy_diagnostics": {
                        "settlement_autofactor_formula_evaluations": 20,
                        "settlement_autofactor_predictive_score_ge_005": 12,
                        "settlement_autofactor_predictive_score_ge_010": 8,
                        "settlement_autofactor_predictive_score_ge_015": 3,
                        "settlement_autofactor_predictive_score_ge_025": 0,
                        "settlement_autofactor_predictive_reverse_score_ge_005": 14,
                        "settlement_autofactor_predictive_reverse_score_ge_010": 10,
                        "settlement_autofactor_predictive_reverse_score_ge_015": 4,
                        "settlement_autofactor_predictive_reverse_score_ge_025": 0,
                    },
                },
                "runtime_evidence": {
                    "basis": "trading_runtime_snapshot",
                    "events": [],
                    "orders": [],
                    "fills": [],
                    "intents": [],
                },
            },
            "--full-depth-entry",
            "--configured-entry-threshold",
            "0.15",
            "--min-trade-count",
            "2",
        )

        counterfactual = payload["score_counterfactual"]
        self.assertEqual(counterfactual["configured_entry_threshold"], "0.15")
        self.assertEqual(
            counterfactual["diagnosis"],
            "reverse_direction_stronger_at_configured_threshold",
        )

    def test_rejects_unknown_counterfactual_threshold(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            runtime_json = tmp_path / "runtime-eval.json"
            output_json = tmp_path / "candidate-strategy-replay.json"
            runtime_json.write_text(
                json.dumps(
                    {
                        "result": {
                            "strategy_diagnostics": {
                                "settlement_autofactor_formula_evaluations": 1,
                            },
                        }
                    }
                ),
                encoding="utf-8",
            )
            completed = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--runtime-evaluation-json",
                    str(runtime_json),
                    "--runtime-score",
                    "autofactor_formula:auto_settlement_conservative_settlement_edge",
                    "--output-json",
                    str(output_json),
                    "--configured-entry-threshold",
                    "0.20",
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
            )
            self.assertNotEqual(completed.returncode, 0)
            self.assertIn(
                "--configured-entry-threshold must be one of",
                completed.stderr + completed.stdout,
            )

    def test_blocks_open_settlement_and_unconfirmed_depth(self):
        payload, _ = self.run_script(
            runtime_eval_payload(settled=False),
            "--min-trade-count",
            "2",
        )

        self.assertFalse(payload["promotion_ready"])
        self.assertEqual(payload["promotion_decision"], "blocked")
        self.assertIn("official_settlement_missing:0<2", payload["blocking_risk_flags"])
        self.assertIn("full_depth_entry_not_confirmed", payload["blocking_risk_flags"])

    def test_resolves_missing_order_event_ids_and_ignores_settlement_exits(self):
        payload = runtime_eval_payload()
        evidence = payload["runtime_evidence"]
        evidence["intents"] = []
        evidence["orders"] = [
            {
                "intent_id": "tl_btcusdt_up_100_123",
                "order_id": "entry-order-1",
                "event_id": None,
                "market_id": None,
                "purpose": None,
                "order_side": "UNKNOWN",
                "quantity": "10",
                "filled_quantity": "10",
                "status": "FILLED",
                "token_id": "token-up-100",
            },
            {
                "intent_id": "tl_settle_100_up",
                "order_id": "settle-order-1",
                "event_id": None,
                "market_id": None,
                "purpose": None,
                "order_side": "UNKNOWN",
                "quantity": "10",
                "filled_quantity": "10",
                "status": "FILLED",
                "token_id": "token-up-100",
            },
            {
                "intent_id": "tl_ethusdt_down_101_456",
                "order_id": "entry-order-2",
                "event_id": None,
                "market_id": None,
                "purpose": None,
                "order_side": "UNKNOWN",
                "quantity": "10",
                "filled_quantity": "10",
                "status": "FILLED",
                "token_id": "token-down-101",
            },
        ]
        evidence["fills"] = [
            {
                "intent_id": "tl_btcusdt_up_100_123",
                "order_id": "entry-order-1",
                "event_id": None,
                "market_id": None,
                "fill_side": "BUY",
                "purpose": None,
                "quantity": "10",
                "price": "0.42",
                "token_id": "token-up-100",
            },
            {
                "intent_id": "tl_settle_100_up",
                "order_id": "settle-order-1",
                "event_id": None,
                "market_id": None,
                "fill_side": "SELL",
                "purpose": None,
                "quantity": "10",
                "price": "1",
                "token_id": "token-up-100",
            },
            {
                "intent_id": "tl_ethusdt_down_101_456",
                "order_id": "entry-order-2",
                "event_id": None,
                "market_id": None,
                "fill_side": "BUY",
                "purpose": None,
                "quantity": "10",
                "price": "0.43",
                "token_id": "token-down-101",
            },
        ]
        evidence["events"] = [
            {
                "intent_id": "tl_btcusdt_up_100_123",
                "order_id": "entry-order-1",
                "event_id": "100",
                "market_id": "100",
                "side": "BUY",
                "signal_inputs": {"purpose": "ENTRY"},
                "fill_status": "FILLED",
                "settlement": "1",
                "pnl": "5",
                "token_id": "token-up-100",
            },
            {
                "intent_id": "tl_settle_100_up",
                "order_id": "settle-order-1",
                "event_id": "100",
                "market_id": "100",
                "side": "SELL",
                "signal_inputs": {"purpose": "ENTRY"},
                "fill_status": "FILLED",
                "settlement": "open",
                "pnl": "10",
                "token_id": "token-up-100",
            },
            {
                "intent_id": "tl_ethusdt_down_101_456",
                "order_id": "entry-order-2",
                "event_id": "101",
                "market_id": "101",
                "side": "BUY",
                "signal_inputs": {"purpose": "ENTRY"},
                "fill_status": "FILLED",
                "settlement": "1",
                "pnl": "7",
                "token_id": "token-down-101",
            },
        ]

        artifact, _ = self.run_script(payload, "--full-depth-entry", "--min-trade-count", "2")

        self.assertTrue(artifact["promotion_ready"])
        self.assertEqual(artifact["metrics"]["trade_count"], 2)
        self.assertEqual(artifact["metrics"]["unique_event_count"], 2)
        self.assertEqual(artifact["metrics"]["settlement_event_count"], 2)
        self.assertEqual(artifact["metrics"]["total_pnl"], 12.0)
        self.assertEqual(artifact["metrics"]["entry_fill_rate"], 1.0)

    def test_invalid_decimal_gate_fails_closed(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            runtime_json = tmp_path / "runtime-eval.json"
            output_json = tmp_path / "candidate-strategy-replay.json"
            runtime_json.write_text(json.dumps(runtime_eval_payload()), encoding="utf-8")
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--runtime-evaluation-json",
                    str(runtime_json),
                    "--runtime-score",
                    "autofactor_formula:auto_settlement_conservative_settlement_edge",
                    "--output-json",
                    str(output_json),
                    "--min-roi",
                    "not-a-decimal",
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
            )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("--min-roi must be a decimal value", result.stderr)


if __name__ == "__main__":
    unittest.main()
