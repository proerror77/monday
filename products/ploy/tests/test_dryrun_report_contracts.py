import importlib.util
import json
import math
from pathlib import Path
import subprocess
import sys
import unittest


ROOT = Path(__file__).resolve().parents[1]
SUMMARY_SCRIPT = ROOT / "scripts" / "report_dryrun_summary.py"
HTML_SCRIPT = ROOT / "scripts" / "report_strategy.py"
CHECK_SCRIPT = ROOT / "scripts" / "check_dryrun_report_contract.py"
SIDE_KEY_MIGRATION = ROOT / "migrations" / "039_fix_strategy_track_record_side_key.sql"
SIDE_RESIDUAL_REPAIR_MIGRATION = ROOT / "migrations" / "041_repair_strategy_track_record_side_residual.sql"


def load_summary_module():
    spec = importlib.util.spec_from_file_location("report_dryrun_summary", SUMMARY_SCRIPT)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


class DryRunReportContractTests(unittest.TestCase):
    def test_summary_metadata_join_uses_token_settlement_bridge(self) -> None:
        script = SUMMARY_SCRIPT.read_text()

        self.assertIn("LEFT JOIN pm_token_settlements s ON s.token_id = t.token_id", script)
        self.assertIn("LEFT JOIN pm_market_metadata ms ON ms.market_slug = s.market_slug", script)
        self.assertIn("LEFT JOIN pm_market_metadata me ON me.market_slug = t.event_id", script)
        self.assertIn("LEFT JOIN pm_market_metadata mt ON mt.market_slug = t.trade_key", script)
        self.assertIn("COALESCE(ms.market_slug, me.market_slug, mt.market_slug, s.market_slug)", script)
        self.assertLess(script.index("LEFT JOIN pm_market_metadata ms"), script.index("LEFT JOIN pm_market_metadata me"))

    def test_summary_metrics_expose_explicit_sharpe_basis(self) -> None:
        summary = load_summary_module()
        events = [
            {"is_closed": True, "net_pnl": 1.0, "closed_at": "2026-04-29T01:00:00+00:00", "trade_key": "a"},
            {"is_closed": True, "net_pnl": -0.5, "closed_at": "2026-04-29T02:00:00+00:00", "trade_key": "b"},
            {"is_closed": True, "net_pnl": 1.5, "closed_at": "2026-04-29T03:00:00+00:00", "trade_key": "c"},
        ]

        metrics = summary.build_report_slice(events, [{"net_pnl": 1.0}, {"net_pnl": -0.5}])["metrics"]

        expected_trade_sharpe = (sum([1.0, -0.5, 1.5]) / 3) / math.sqrt(13 / 12) * math.sqrt(3)
        self.assertAlmostEqual(metrics["sharpe_per_trade"], expected_trade_sharpe, places=4)
        self.assertEqual(metrics["sharpe"], metrics["sharpe_per_trade"])
        self.assertEqual(metrics["sharpe_basis"], "closed_trade_pnl_sqrt_n")
        self.assertEqual(metrics["daily_sharpe_basis"], "daily_net_pnl_sqrt_365")

    def test_summary_daily_by_window_uses_cst_day(self) -> None:
        summary = load_summary_module()

        self.assertEqual(
            summary.day_from_event({"closed_at": "2026-04-28T17:30:00+00:00"}),
            "2026-04-29",
        )

    def test_summary_hourly_rows_include_pnl_and_drawdown(self) -> None:
        summary = load_summary_module()
        events = [
            {
                "is_closed": True,
                "net_pnl": 5.0,
                "closed_at": "2026-04-28T17:30:00+00:00",
                "window_secs": 300,
            },
            {
                "is_closed": True,
                "net_pnl": -8.0,
                "closed_at": "2026-04-28T18:15:00+00:00",
                "window_secs": 900,
            },
        ]

        hourly = summary.build_hourly_rows(events)
        hourly_by_window = summary.build_hourly_by_window(events)

        self.assertEqual(hourly[0]["trading_hour_cst"], "2026-04-29T02:00:00+08:00")
        self.assertEqual(hourly[0]["trade_count"], 1)
        self.assertEqual(hourly[0]["net_pnl"], -8.0)
        self.assertEqual(hourly[0]["drawdown"], -8.0)
        self.assertEqual(hourly[1]["trading_hour_cst"], "2026-04-29T01:00:00+08:00")
        self.assertEqual(hourly_by_window[0]["window_label"], "15m")

    def test_execution_diagnostics_contract(self) -> None:
        summary = load_summary_module()
        diagnostics = summary.build_execution_diagnostics(
            [
                {
                    "total_orders": 3,
                    "buy_orders": 2,
                    "sell_orders": 1,
                    "rejected_orders": 1,
                    "rejected_buy_orders": 1,
                    "partial_buy_orders": 1,
                    "buy_requested_notional": 30,
                    "buy_filled_notional": 15,
                }
            ]
        )

        self.assertEqual(diagnostics["basis"], "strategy_runtime_orders")
        self.assertEqual(diagnostics["partial_buy_threshold_pct"], 98)
        self.assertEqual(diagnostics["summary"]["rejected_buy_orders"], 1)
        self.assertEqual(diagnostics["summary"]["buy_fill_rate_pct"], 50)

    def test_empty_payload_exposes_runtime_evidence_contract(self) -> None:
        summary = load_summary_module()
        payload = summary.empty_payload()

        self.assertEqual(payload["runtime_evidence"]["schema_version"], 1)
        self.assertEqual(payload["runtime_evidence"]["basis"], "strategy_runtime_orders_fills_and_events")
        self.assertEqual(payload["runtime_evidence"]["events"], [])
        self.assertEqual(payload["runtime_evidence"]["orders"], [])
        self.assertEqual(payload["runtime_evidence"]["fills"], [])

    def test_runtime_evidence_query_exports_event_order_and_fill_rows(self) -> None:
        script = SUMMARY_SCRIPT.read_text()

        self.assertIn("RUNTIME_EVIDENCE_QUERY", script)
        self.assertIn('"runtime_evidence"', script)
        self.assertIn("FROM strategy_runtime_orders o", script)
        self.assertIn("LEFT JOIN strategy_runtime_event_track_record track", script)
        self.assertIn("FROM strategy_runtime_orders o", script)
        self.assertIn("FROM strategy_runtime_fills f", script)
        self.assertIn("'events'", script)
        self.assertIn("'orders'", script)
        self.assertIn("'fills'", script)
        self.assertIn("'signal_inputs'", script)
        self.assertIn("'context', o.context", script)

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

    def test_strategy_label_prefers_versioned_experiment_label(self) -> None:
        summary = load_summary_module()

        self.assertEqual(
            summary.strategy_label(
                "dry_run",
                "three_layer",
                "pm5d.threelayer.obi-hard.dryrun",
            ),
            "TL v4 OBI-hard EVCal",
        )
        self.assertEqual(
            summary.strategy_label(
                "dry_run",
                "three_layer",
                "pm5d.threelayer.some-new-gate.dryrun",
            ),
            "TL Some New Gate",
        )

    def test_html_report_aggregates_orders_and_names_sharpe_bases(self) -> None:
        script = HTML_SCRIPT.read_text()

        self.assertIn("buy_requested_notional", script)
        self.assertIn("rejected_buy_orders", script)
        self.assertIn("Rejected BUY", script)
        self.assertIn("Sharpe / Trade", script)
        self.assertIn("Sharpe Daily Ann", script)
        self.assertNotIn("ORDER BY o.created_at ASC\n  LIMIT 1", script)
        self.assertNotIn("else 0.001", script)

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
