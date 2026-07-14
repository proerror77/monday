import json
import subprocess
import sys
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "check_dryrun_candidate_gate.py"
DEPLOYMENT_ID = "pm5d.threelayer.settlement-probability-btc-eth.dryrun"


def run_gate(payload, *args):
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args],
        input=json.dumps(payload),
        text=True,
        capture_output=True,
        check=False,
    )


class CheckDryrunCandidateGateTests(unittest.TestCase):
    def test_clean_baseline_passes_when_target_strategy_is_absent(self):
        result = run_gate({"strategies": []})

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('"reason": "target_strategy_absent"', result.stdout)

    def test_clean_baseline_blocks_residual_target_rows(self):
        payload = {
            "strategies": [
                {
                    "deployment_id": DEPLOYMENT_ID,
                    "summary": {
                        "total_trades": 20,
                        "closed_trades": 20,
                        "open_positions": 0,
                    },
                    "execution_diagnostics": {
                        "summary": {
                            "total_orders": 185,
                            "buy_orders": 20,
                            "sell_orders": 165,
                        }
                    },
                }
            ]
        }

        result = run_gate(payload)

        self.assertEqual(result.returncode, 1)
        self.assertIn('"reason": "residual_runtime_evidence"', result.stdout)
        self.assertIn('"total_orders": 185', result.stdout)

    def test_clean_baseline_accepts_zero_target_counts(self):
        payload = {
            "strategies": [
                {
                    "deployment_id": DEPLOYMENT_ID,
                    "summary": {
                        "total_trades": 0,
                        "closed_trades": 0,
                        "open_positions": 0,
                    },
                    "execution_diagnostics": {
                        "summary": {
                            "total_orders": 0,
                            "buy_orders": 0,
                            "sell_orders": 0,
                        }
                    },
                }
            ]
        }

        result = run_gate(payload)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('"reason": "zero_runtime_evidence"', result.stdout)

    def test_candidate_quality_blocks_negative_sample(self):
        payload = {
            "strategies": [
                {
                    "deployment_id": DEPLOYMENT_ID,
                    "summary": {
                        "closed_trades": 20,
                        "realized_pnl": -115.17,
                    },
                    "metrics": {
                        "profit_factor": 0.2927,
                        "max_drawdown": -136.3244,
                    },
                    "execution_diagnostics": {
                        "summary": {
                            "buy_fill_rate_pct": 97.93,
                        }
                    },
                }
            ]
        }

        result = run_gate(payload, "--mode", "candidate-quality")

        self.assertEqual(result.returncode, 1)
        self.assertIn('"realized_pnl"', result.stdout)
        self.assertIn('"profit_factor"', result.stdout)
        self.assertIn('"max_drawdown"', result.stdout)

    def test_candidate_quality_treats_infinite_profit_factor_as_passing_metric(self):
        payload = {
            "strategies": [
                {
                    "deployment_id": DEPLOYMENT_ID,
                    "summary": {
                        "closed_trades": 4,
                        "realized_pnl": 26.74,
                    },
                    "metrics": {
                        "profit_factor": "Infinity",
                        "max_drawdown": 0.0,
                    },
                    "execution_diagnostics": {
                        "summary": {
                            "buy_fill_rate_pct": 94.22,
                        }
                    },
                }
            ]
        }

        result = run_gate(payload, "--mode", "candidate-quality")
        output = json.loads(result.stdout)

        self.assertEqual(result.returncode, 1)
        self.assertEqual(output["values"]["profit_factor"], "Infinity")
        self.assertNotIn("profit_factor", output["failures"])
        self.assertIn("closed_trades", output["failures"])
        self.assertIn("buy_fill_rate_pct", output["failures"])


if __name__ == "__main__":
    unittest.main()
