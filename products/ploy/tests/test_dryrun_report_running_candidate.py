import importlib.util
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SUMMARY_SCRIPT = ROOT / "scripts" / "report_dryrun_summary.py"
CHECK_SCRIPT = ROOT / "scripts" / "check_dryrun_report_contract.py"


def load_summary_module():
    spec = importlib.util.spec_from_file_location("report_dryrun_summary", SUMMARY_SCRIPT)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


class DryRunReportRunningCandidateTests(unittest.TestCase):
    def test_loads_running_deployment_without_trade_rows(self) -> None:
        summary = load_summary_module()
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            config_dir = root / "config" / "deployments"
            config_dir.mkdir(parents=True)
            (config_dir / "candidate.json").write_text(
                json.dumps(
                    {
                        "deployment_id": "pm5d.threelayer.settlement-probability-btc-eth.dryrun",
                        "bundle_id": "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun",
                        "runtime_mode": "paper",
                        "account_id": "acct-pm5d-dryrun",
                        "desired_state": "running",
                    }
                ),
                encoding="utf-8",
            )

            summary.DEPLOYMENT_CONFIG_DIR = config_dir
            summary.DEPLOYMENTS_FILE = root / "missing-state.json"
            summary.DEPLOYMENT_STATUS_FILE = root / "missing-status.json"

            deployments = summary.load_deployments()

        self.assertEqual(len(deployments), 1)
        self.assertEqual(deployments[0]["deployment_id"], "pm5d.threelayer.settlement-probability-btc-eth.dryrun")
        self.assertTrue(summary.deployment_is_running(deployments[0]))

    def test_running_deployment_becomes_zero_activity_strategy_row(self) -> None:
        summary = load_summary_module()
        row = summary.deployment_report_row(
            {
                "deployment_id": "pm5d.threelayer.settlement-probability-btc-eth.dryrun",
                "bundle_id": "02-pm5d-threelayer.settlement-probability-btc-eth-dryrun",
                "runtime_mode": "paper",
                "account_id": "acct-pm5d-dryrun",
                "desired_state": "running",
                "observed_state": "running",
            }
        )

        self.assertEqual(row["activity_status"], "running_no_recent_trades")
        self.assertEqual(row["summary"]["total_trades"], 0)
        self.assertEqual(row["summary"]["closed_trades"], 0)
        self.assertEqual(row["deployment_desired_state"], "running")
        self.assertEqual(row["deployment_observed_state"], "running")
        self.assertEqual(row["execution_diagnostics"]["basis"], "strategy_runtime_orders")

    def test_contract_checker_requires_running_deployment_strategy_row(self) -> None:
        payload = {
            "summary": {"total_trades": 0},
            "metrics": {
                "sharpe_basis": "closed_trade_pnl_sqrt_n",
                "daily_sharpe_basis": "daily_net_pnl_sqrt_365",
            },
            "execution_diagnostics": {"basis": "strategy_runtime_orders"},
            "runtime_evidence": {
                "schema_version": 1,
                "basis": "strategy_runtime_orders_fills_and_events",
                "events": [],
                "orders": [],
                "fills": [],
            },
            "deployments": [
                {
                    "deployment_id": "pm5d.threelayer.settlement-probability-btc-eth.dryrun",
                    "runtime_mode": "paper",
                    "desired_state": "running",
                    "observed_state": "running",
                }
            ],
            "strategies": [],
        }

        missing = subprocess.run(
            [sys.executable, str(CHECK_SCRIPT)],
            input=json.dumps(payload),
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertNotEqual(missing.returncode, 0)
        self.assertIn("running deployment represented as a strategy row", missing.stderr)

        payload["strategies"] = [
            {
                "deployment_id": "pm5d.threelayer.settlement-probability-btc-eth.dryrun",
                "execution_diagnostics": {"basis": "strategy_runtime_orders"},
            }
        ]
        present = subprocess.run(
            [sys.executable, str(CHECK_SCRIPT)],
            input=json.dumps(payload),
            text=True,
            capture_output=True,
            check=False,
        )
        self.assertEqual(present.returncode, 0, present.stderr)


if __name__ == "__main__":
    unittest.main()
