import json
from pathlib import Path
import subprocess
import sys
import unittest


ROOT = Path(__file__).resolve().parents[1]
CHECK_SCRIPT = ROOT / "scripts" / "check_dryrun_report_contract.py"
SIDE_KEY_MIGRATION = ROOT / "migrations" / "039_fix_strategy_track_record_side_key.sql"
SIDE_RESIDUAL_REPAIR_MIGRATION = ROOT / "migrations" / "041_repair_strategy_track_record_side_residual.sql"


class DryRunReportContractTests(unittest.TestCase):
    def test_report_contract_checker_accepts_clean_empty_dryrun(self) -> None:
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
            "strategies": [],
        }

        result = subprocess.run(
            [sys.executable, str(CHECK_SCRIPT)],
            input=json.dumps(payload),
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertEqual(result.returncode, 0, result.stderr)

    def test_report_contract_checker_validates_strategy_rows_when_present(self) -> None:
        payload = {
            "summary": {"total_trades": 1},
            "metrics": {
                "sharpe_basis": "closed_trade_pnl_sqrt_n",
                "daily_sharpe_basis": "daily_net_pnl_sqrt_365",
            },
            "execution_diagnostics": {"basis": "strategy_runtime_orders"},
            "strategies": [{"deployment_id": "test-deploy", "execution_diagnostics": {}}],
        }

        result = subprocess.run(
            [sys.executable, str(CHECK_SCRIPT)],
            input=json.dumps(payload),
            text=True,
            capture_output=True,
            check=False,
        )

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("strategies[test-deploy].execution_diagnostics.basis", result.stderr)

    def test_side_key_migration_groups_by_token_and_market_side(self) -> None:
        migration = SIDE_KEY_MIGRATION.read_text()

        self.assertIn("trade_key,\n        token_id,\n        market_side", migration)
        self.assertIn("GROUP BY\n        runtime_mode,\n        strategy_id,\n        deployment_id,\n        trade_key,\n        token_id,\n        market_side", migration)

    def test_migration_versions_are_unique(self) -> None:
        migration_versions = [
            path.name.split("_", 1)[0]
            for path in (ROOT / "migrations").glob("[0-9][0-9][0-9]_*.sql")
        ]

        duplicates = {
            version
            for version in migration_versions
            if migration_versions.count(version) > 1
        }
        self.assertEqual(duplicates, set())

    def test_side_residual_repair_preserves_official_settlement_accounting(self) -> None:
        migration = SIDE_RESIDUAL_REPAIR_MIGRATION.read_text()

        self.assertIn("041_repair_strategy_track_record_side_residual", migration)
        self.assertIn("GROUP BY\n        runtime_mode,\n        strategy_id,\n        deployment_id,\n        trade_key,\n        token_id,\n        market_side", migration)
        self.assertIn("official_residual_quantity", migration)
        self.assertIn("recorded_sell_quantity", migration)
        self.assertIn("settlement_exit_quantity", migration)
        self.assertIn("settlement_corrected", migration)


if __name__ == "__main__":
    unittest.main()
