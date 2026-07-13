import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "audit_market_data_gaps.py"
SPEC = importlib.util.spec_from_file_location("audit_market_data_gaps", SCRIPT)
audit = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = audit
SPEC.loader.exec_module(audit)


class AuditMarketDataGapsTests(unittest.TestCase):
    def write_archive_hour(
        self,
        root: Path,
        date: str,
        hour: str,
        *,
        row_count: int = 100,
        full_fidelity: bool = True,
    ) -> None:
        hour_dir = root / "orderbook_snapshots" / f"date={date}" / f"hour={hour}"
        hour_dir.mkdir(parents=True)
        (hour_dir / "snapshots.parquet").write_text("placeholder", encoding="utf-8")
        (hour_dir / "_SUCCESS").touch()
        (hour_dir / "manifest.json").write_text(
            json.dumps(
                {
                    "table": "clob_orderbook_snapshots",
                    "row_count": row_count,
                    "full_fidelity": full_fidelity,
                    "start_ts": f"{date} {hour}:00:00+08",
                    "end_ts": f"{date} {hour}:59:59+08",
                    "sha256": "abc123",
                }
            ),
            encoding="utf-8",
        )

    def test_historical_pm_orderbooks_use_archive_coverage_when_hot_table_empty(self):
        target = audit.GapTarget(
            "polymarket_orderbooks",
            "clob_orderbook_snapshots",
            "received_at",
            900,
            ignore_max_gap=True,
            ignore_missing_buckets=True,
        )
        row = {
            "latest_at": None,
            "latest_lag_seconds": None,
            "expected_buckets": 24,
            "present_buckets": 0,
            "max_gap_minutes": 120,
            "missing_buckets": 24,
            "coverage_pct": 0.0,
        }

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.write_archive_hour(root, "2026-05-17", "08")
            self.write_archive_hour(root, "2026-05-17", "09")

            updated = audit.apply_orderbook_archive_coverage(
                row,
                target,
                start_ts="2026-05-17T00:00:00Z",
                end_ts="2026-05-17T02:00:00Z",
                bucket_minutes=5,
                orderbook_archive_root=str(root),
            )
            status, reasons, freshness_status, _, coverage_status, _ = (
                audit.classify_gap_for_gate(
                    updated,
                    target,
                    "coverage",
                    historical_window=True,
                )
            )

        self.assertEqual(status, "ok")
        self.assertEqual(freshness_status, "critical")
        self.assertEqual(coverage_status, "ok")
        self.assertEqual(updated["coverage_source"], "orderbook_snapshot_archive")
        self.assertEqual(updated["present_buckets"], 24)
        self.assertEqual(updated["missing_buckets"], 0)
        self.assertEqual(updated["hot_table_present_buckets"], 0)
        self.assertEqual(updated["archive_coverage"]["present_hours"], 2)
        self.assertIn("freshness not enforced for historical window", "; ".join(reasons))

    def test_historical_pm_orderbooks_fail_closed_when_archive_hour_missing(self):
        target = audit.GapTarget(
            "polymarket_orderbooks",
            "clob_orderbook_snapshots",
            "received_at",
            900,
            ignore_max_gap=True,
            ignore_missing_buckets=True,
        )
        row = {
            "latest_at": None,
            "latest_lag_seconds": None,
            "expected_buckets": 24,
            "present_buckets": 0,
            "max_gap_minutes": 120,
            "missing_buckets": 24,
            "coverage_pct": 0.0,
        }

        with tempfile.TemporaryDirectory() as tmp:
            updated = audit.apply_orderbook_archive_coverage(
                row,
                target,
                start_ts="2026-05-17T00:00:00Z",
                end_ts="2026-05-17T02:00:00Z",
                bucket_minutes=5,
                orderbook_archive_root=tmp,
            )
            status, reasons, _, _, coverage_status, coverage_reasons = (
                audit.classify_gap_for_gate(
                    updated,
                    target,
                    "coverage",
                    historical_window=True,
                )
            )

        self.assertEqual(status, "critical")
        self.assertEqual(coverage_status, "critical")
        self.assertIn("no covered buckets in audited window: 0/24", reasons)
        self.assertIn("no covered buckets in audited window: 0/24", coverage_reasons)
        self.assertEqual(updated["archive_coverage"]["status"], "critical")
        self.assertNotIn("coverage_source", updated)

    def test_archive_hour_keys_use_shanghai_partition_hours(self):
        self.assertEqual(
            audit.archive_hour_keys(
                "2026-05-17T00:00:00Z",
                "2026-05-17T03:00:00Z",
            ),
            [("2026-05-17", "08"), ("2026-05-17", "09"), ("2026-05-17", "10")],
        )

    def test_orderbook_archive_coverage_blocks_non_full_fidelity_manifest(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            self.write_archive_hour(root, "2026-05-17", "08", full_fidelity=False)

            result = audit.audit_orderbook_archive_coverage(
                archive_root=str(root),
                start_ts="2026-05-17T00:00:00Z",
                end_ts="2026-05-17T01:00:00Z",
                bucket_minutes=5,
            )

        self.assertEqual(result["status"], "critical")
        self.assertFalse(result["full_fidelity"])
        self.assertEqual(result["present_hours"], 0)
        self.assertEqual(result["invalid_hours"], 1)
        self.assertIn("archive contains non-full-fidelity manifests", result["reasons"])

    def test_freshness_gate_allows_historical_gap_but_reports_it(self):
        target = audit.GapTarget(
            "binance_price/BTCUSDT",
            "binance_price_ticks",
            "trade_time",
            600,
        )
        row = {
            "latest_at": "2026-05-09T13:05:00+08:00",
            "latest_lag_seconds": 2,
            "max_gap_minutes": 3800,
            "missing_buckets": 1120,
        }

        status, reasons, freshness_status, _, coverage_status, coverage_reasons = (
            audit.classify_gap_for_gate(row, target, "freshness")
        )

        self.assertEqual(status, "ok")
        self.assertEqual(freshness_status, "ok")
        self.assertEqual(coverage_status, "critical")
        self.assertIn("max gap 3800m >= 15m", coverage_reasons)
        self.assertTrue(any(reason.startswith("coverage not enforced") for reason in reasons))

    def test_coverage_gate_keeps_historical_gap_critical(self):
        target = audit.GapTarget(
            "binance_lob/BTCUSDT",
            "binance_lob_ticks",
            "event_time",
            900,
        )
        row = {
            "latest_at": "2026-05-09T13:05:00+08:00",
            "latest_lag_seconds": 1,
            "max_gap_minutes": 3800,
            "missing_buckets": 1120,
        }

        status, reasons, freshness_status, _, coverage_status, _ = audit.classify_gap_for_gate(
            row, target, "coverage"
        )

        self.assertEqual(status, "critical")
        self.assertEqual(freshness_status, "ok")
        self.assertEqual(coverage_status, "critical")
        self.assertIn("max gap 3800m >= 15m", reasons)

    def test_coverage_gate_blocks_zero_coverage_even_when_missing_buckets_are_ignored(self):
        target = audit.GapTarget(
            "polymarket_orderbooks",
            "clob_orderbook_snapshots",
            "received_at",
            900,
            ignore_max_gap=True,
            ignore_missing_buckets=True,
        )
        row = {
            "latest_at": "2026-05-09T13:05:00+08:00",
            "latest_lag_seconds": 1,
            "expected_buckets": 288,
            "present_buckets": 0,
            "max_gap_minutes": 1440,
            "missing_buckets": 288,
        }

        status, reasons, _, _, coverage_status, coverage_reasons = audit.classify_gap_for_gate(
            row,
            target,
            "coverage",
            historical_window=True,
        )

        self.assertEqual(status, "critical")
        self.assertEqual(coverage_status, "critical")
        self.assertIn("no covered buckets in audited window: 0/288", reasons)
        self.assertIn("no covered buckets in audited window: 0/288", coverage_reasons)

    def test_freshness_gate_still_blocks_stale_source(self):
        target = audit.GapTarget(
            "binance_agg_trades/BTCUSDT",
            "binance_agg_trade_ticks",
            "trade_time",
            600,
        )
        row = {
            "latest_at": "2026-05-09T12:00:00+08:00",
            "latest_lag_seconds": 1200,
            "max_gap_minutes": 0,
            "missing_buckets": 0,
        }

        status, reasons, freshness_status, _, coverage_status, _ = audit.classify_gap_for_gate(
            row, target, "freshness"
        )

        self.assertEqual(status, "critical")
        self.assertEqual(freshness_status, "critical")
        self.assertEqual(coverage_status, "ok")
        self.assertIn("latest lag 1200s > 600s", reasons)

    def test_pm_quote_quality_warns_on_one_sided_quote_liquidity(self):
        row = {
            "active_tokens": 20,
            "missing_quotes": 0,
            "older_than_15s": 0,
            "missing_ask_or_size": 1,
            "missing_bid_or_size": 1,
            "max_age_seconds": 4,
        }

        status, reasons = audit.classify_pm_quote_quality(row)

        self.assertEqual(status, "warn")
        self.assertIn("1/20 active token quotes missing ask/ask_size", reasons)
        self.assertIn("1/20 active token quotes missing bid/bid_size", reasons)

    def test_pm_quote_quality_blocks_missing_asks_for_all_active_tokens(self):
        row = {
            "active_tokens": 20,
            "missing_quotes": 0,
            "older_than_15s": 0,
            "missing_ask_or_size": 20,
            "missing_bid_or_size": 0,
            "max_age_seconds": 4,
        }

        status, reasons = audit.classify_pm_quote_quality(row)

        self.assertEqual(status, "critical")
        self.assertIn("20/20 active token quotes missing ask/ask_size", reasons)

    def test_pm_quote_quality_query_excludes_expired_grace_tokens(self):
        query = audit.pm_quote_quality_query(["BTCUSDT"], 20)

        self.assertIn("AND now() < m.end_time", query)
        self.assertNotIn("m.end_time + interval '1 minute'", query)

    def test_pm_quote_quality_query_includes_missing_examples(self):
        query = audit.pm_quote_quality_query(["BTCUSDT"], 20)

        self.assertIn("'missing_ask_or_size_examples'", query)
        self.assertIn("LIMIT 8", query)

    def test_pm_quote_quality_passes_fresh_complete_active_tokens(self):
        row = {
            "active_tokens": 20,
            "missing_quotes": 0,
            "older_than_15s": 0,
            "missing_ask_or_size": 0,
            "missing_bid_or_size": 0,
            "max_age_seconds": 5,
        }

        status, reasons = audit.classify_pm_quote_quality(row)

        self.assertEqual(status, "ok")
        self.assertEqual(
            reasons,
            ["active token quote quality within thresholds; max_age_seconds=5"],
        )


if __name__ == "__main__":
    unittest.main()
