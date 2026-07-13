import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "apply_event_ml_handoff_to_config.py"


READY_HANDOFF = {
    "status": "ready",
    "required_strategy_profile": "event_ml_supervised_tabular",
    "runtime_score": "event_ml_model:baseline_v1",
    "replay_parity_ready": True,
    "blocked_gate_ids": [],
    "strategy": {
        "strategy_profile": "event_ml_supervised_tabular",
        "runtime_score": "event_ml_model:baseline_v1",
        "selection_rule": "best_validation_roi",
        "window_count": 3,
        "test_trades": 42,
        "test_pnl": 12.5,
        "test_roi": 0.071,
        "weighted_avg_entry": 0.62,
        "max_window_drawdown": -4.0,
    },
}


BASE_CONFIG = """[strategy]
symbols = ["BTCUSDT", "ETHUSDT"]
three_layer_strategy_profile = "settlement_probability"
three_layer_autofactor_runtime_score = "autofactor_formula:auto_settlement_conservative_settlement_edge"
three_layer_min_entry_score = 0.25
"""


class ApplyEventMlHandoffToConfigTests(unittest.TestCase):
    def run_script(self, handoff, config_text=BASE_CONFIG, *extra_args, check=True):
        with tempfile.TemporaryDirectory() as tmp:
            handoff_path = Path(tmp) / "handoff.json"
            config_path = Path(tmp) / "strategy.toml"
            summary_path = Path(tmp) / "summary.json"
            handoff_path.write_text(json.dumps(handoff), encoding="utf-8")
            config_path.write_text(config_text, encoding="utf-8")
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--handoff-json",
                    str(handoff_path),
                    "--config",
                    str(config_path),
                    "--model-path",
                    "/opt/ploy/models/event_ml/baseline_metrics.json",
                    "--output-json",
                    str(summary_path),
                    *extra_args,
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=check,
            )
            summary = (
                json.loads(summary_path.read_text(encoding="utf-8"))
                if summary_path.exists()
                else None
            )
            return result, config_path.read_text(encoding="utf-8"), summary

    def test_updates_runtime_score_and_inserts_model_path(self):
        _, config_text, summary = self.run_script(READY_HANDOFF)

        self.assertIn(
            'three_layer_autofactor_runtime_score = "event_ml_model:baseline_v1"',
            config_text,
        )
        self.assertIn(
            'three_layer_event_ml_model_path = "/opt/ploy/models/event_ml/baseline_metrics.json"',
            config_text,
        )
        self.assertTrue(summary["changed"])
        self.assertEqual(summary["runtime_score_action"], "updated")
        self.assertEqual(summary["model_path_action"], "inserted")

    def test_updates_existing_model_path(self):
        config_with_model_path = (
            BASE_CONFIG
            + 'three_layer_event_ml_model_path = "/old/model.json"\n'
        )

        _, config_text, summary = self.run_script(READY_HANDOFF, config_with_model_path)

        self.assertNotIn("/old/model.json", config_text)
        self.assertEqual(summary["previous_model_path"], "/old/model.json")
        self.assertEqual(summary["model_path_action"], "updated")

    def test_unchanged_config_reports_changed_false(self):
        current_config = """[strategy]
three_layer_strategy_profile = "settlement_probability"
three_layer_autofactor_runtime_score = "event_ml_model:baseline_v1"
three_layer_event_ml_model_path = "/opt/ploy/models/event_ml/baseline_metrics.json"
"""

        _, _, summary = self.run_script(READY_HANDOFF, current_config)

        self.assertFalse(summary["changed"])

    def test_blocks_non_ready_handoff(self):
        result, _, _ = self.run_script(
            {"status": "blocked", "strategy": None, "replay_parity_ready": True},
            BASE_CONFIG,
            check=False,
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected ready", result.stderr)

    def test_blocks_missing_replay_parity(self):
        handoff = dict(READY_HANDOFF, replay_parity_ready=False)

        result, _, _ = self.run_script(handoff, BASE_CONFIG, check=False)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("replay_parity_ready is not true", result.stderr)

    def test_blocks_non_event_ml_runtime_score(self):
        handoff = json.loads(json.dumps(READY_HANDOFF))
        handoff["strategy"]["runtime_score"] = "autofactor_formula:edge"

        result, _, _ = self.run_script(handoff, BASE_CONFIG, check=False)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("unsupported runtime_score", result.stderr)

    def test_blocks_handoff_profile_mismatch(self):
        handoff = json.loads(json.dumps(READY_HANDOFF))
        handoff["strategy"]["strategy_profile"] = "settlement_probability"

        result, _, _ = self.run_script(handoff, BASE_CONFIG, check=False)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("handoff strategy profile mismatch", result.stderr)

    def test_blocks_config_profile_mismatch(self):
        result, _, _ = self.run_script(
            READY_HANDOFF,
            BASE_CONFIG.replace("settlement_probability", "repricing_momentum", 1),
            check=False,
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("strategy config profile mismatch", result.stderr)


if __name__ == "__main__":
    unittest.main()
