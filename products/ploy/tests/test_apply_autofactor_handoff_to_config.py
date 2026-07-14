import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "apply_autofactor_handoff_to_config.py"


READY_HANDOFF = {
    "status": "ready",
    "strategies": [
        {
            "name": "auto_settlement_conservative_settlement_edge_x_near_strike",
            "target": "full_depth_settlement_executable_pnl",
            "strategy_profile": "settlement_probability",
            "strategy_family": "settlement_probability",
            "runtime_score": "autofactor_formula:auto_settlement_conservative_settlement_edge_x_near_strike",
            "metrics": {
                "icir": 1.044245,
                "top_bucket_avg_label": 2.631575,
            },
        }
    ],
}


BASE_CONFIG = """[strategy]
symbols = ["BTCUSDT", "ETHUSDT"]
three_layer_strategy_profile = "settlement_probability"
three_layer_autofactor_runtime_score = "autofactor_formula:auto_settlement_conservative_settlement_edge"
three_layer_min_entry_score = 0.25
"""


class ApplyAutoFactorHandoffToConfigTests(unittest.TestCase):
    def run_script(self, handoff, config_text=BASE_CONFIG, *extra_args, check=True):
        with tempfile.TemporaryDirectory() as tmp:
            handoff_path = Path(tmp) / "handoff.json"
            config_path = Path(tmp) / "strategy.toml"
            summary_path = Path(tmp) / "summary.json"
            summary_md_path = Path(tmp) / "summary.md"
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
                    "--output-json",
                    str(summary_path),
                    "--output-md",
                    str(summary_md_path),
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
            summary_md = summary_md_path.read_text(encoding="utf-8") if summary_md_path.exists() else ""
            return result, config_path.read_text(encoding="utf-8"), summary, summary_md

    def test_updates_runtime_score_from_ready_handoff(self):
        _, config_text, summary, summary_md = self.run_script(READY_HANDOFF)
        runtime_score = READY_HANDOFF["strategies"][0]["runtime_score"]

        self.assertIn(
            f'three_layer_autofactor_runtime_score = "{runtime_score}"',
            config_text,
        )
        self.assertTrue(summary["changed"])
        self.assertEqual(summary["action"], "updated")
        self.assertEqual(
            summary["previous_runtime_score"],
            "autofactor_formula:auto_settlement_conservative_settlement_edge",
        )
        self.assertIn("Top bucket avg label", summary_md)
        self.assertNotIn("Top bucket PnL", summary_md)

    def test_inserts_runtime_score_when_config_has_profile_but_no_score(self):
        config_without_score = """[strategy]
three_layer_strategy_profile = "settlement_probability"
three_layer_min_entry_score = 0.25
"""

        _, config_text, summary, _ = self.run_script(READY_HANDOFF, config_without_score)
        runtime_score = READY_HANDOFF["strategies"][0]["runtime_score"]

        self.assertIn(
            'three_layer_strategy_profile = "settlement_probability"\n'
            f'three_layer_autofactor_runtime_score = "{runtime_score}"',
            config_text,
        )
        self.assertTrue(summary["changed"])
        self.assertEqual(summary["action"], "inserted")

    def test_blocks_non_ready_handoff(self):
        result, _, _, _ = self.run_script(
            {"status": "blocked", "strategies": []},
            BASE_CONFIG,
            check=False,
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("expected ready", result.stderr)

    def test_blocks_profile_mismatch(self):
        result, _, _, _ = self.run_script(
            READY_HANDOFF,
            BASE_CONFIG.replace("settlement_probability", "repricing_momentum", 1),
            check=False,
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("strategy config profile mismatch", result.stderr)


if __name__ == "__main__":
    unittest.main()
