from pathlib import Path
import re
import unittest


SCRIPT_PATH = Path(__file__).resolve().parents[1] / "scripts" / "ploy_maintenance.sh"
ORDERBOOK_RETENTION_SCRIPT_PATH = (
    Path(__file__).resolve().parents[1]
    / "scripts"
    / "ploy_orderbook_snapshot_retention.sh"
)
ORDERBOOK_EXPORT_SCRIPT_PATH = (
    Path(__file__).resolve().parents[1]
    / "scripts"
    / "export_clob_orderbook_snapshots_parquet.sh"
)
ORDERBOOK_BACKFILL_SCRIPT_PATH = (
    Path(__file__).resolve().parents[1]
    / "scripts"
    / "archive_clob_orderbook_snapshots_backfill.sh"
)
ORDERBOOK_ARCHIVE_SERVICE_PATH = (
    Path(__file__).resolve().parents[1]
    / "deployment"
    / "systemd"
    / "ploy-orderbook-snapshot-archive.service"
)
DB_RETENTION_VARS = [
    "RETENTION_CLOB_TICKS_DAYS",
    "RETENTION_CLOB_BOOK_DAYS",
    "RETENTION_CLOB_ORDERBOOK_HISTORY_DAYS",
    "RETENTION_CLOB_TRADES_DAYS",
    "RETENTION_CLOB_ALERTS_DAYS",
    "RETENTION_BINANCE_TICKS_DAYS",
    "RETENTION_BINANCE_AGGTRADE_DAYS",
    "RETENTION_BINANCE_LOB_DAYS",
    "RETENTION_DERIBIT_IV_DAYS",
    "RETENTION_NBA_OBS_DAYS",
    "RETENTION_ORDER_EXEC_DAYS",
]
ARTIFACT_RETENTION_VARS = [
    "RETENTION_TMP_DAYS",
    "RETENTION_RECORDING_DAYS",
    "RETENTION_PARQUET_DAYS",
]


def _defaults_by_var() -> dict[str, str]:
    script = SCRIPT_PATH.read_text()
    matches = re.findall(
        r'^(RETENTION_[A-Z_]+_DAYS)="\$\{[A-Z0-9_]+:-([0-9]+)\}"$',
        script,
        re.MULTILINE,
    )
    return dict(matches)


class PloyMaintenanceDefaultsTest(unittest.TestCase):
    def test_db_retention_defaults_are_seven_days(self) -> None:
        defaults = _defaults_by_var()
        actual = {var: defaults[var] for var in DB_RETENTION_VARS}
        expected = {var: "7" for var in DB_RETENTION_VARS}
        self.assertEqual(actual, expected)

    def test_database_url_is_preferred_when_available(self) -> None:
        script = SCRIPT_PATH.read_text()
        self.assertIn('if [[ -n "${DATABASE_URL:-}" ]]; then', script)
        self.assertIn('PSQL=(psql "$DATABASE_URL" -v ON_ERROR_STOP=1)', script)

    def test_artifact_retention_defaults_are_bounded(self) -> None:
        defaults = _defaults_by_var()
        self.assertEqual(defaults["RETENTION_TMP_DAYS"], "2")
        self.assertEqual(defaults["RETENTION_RECORDING_DAYS"], "7")
        self.assertEqual(defaults["RETENTION_PARQUET_DAYS"], "14")

    def test_partition_retention_drops_old_children(self) -> None:
        script = SCRIPT_PATH.read_text()
        self.assertIn("DROP TABLE IF EXISTS %I.%I", script)
        self.assertIn("parent.relname = 'binance_lob_ticks'", script)
        self.assertIn("parent.relname = 'clob_trade_ticks'", script)
        self.assertIn("parent.relname = 'deribit_iv_ticks'", script)

    def test_clob_orderbook_snapshot_cleanup_is_bounded(self) -> None:
        script = SCRIPT_PATH.read_text()
        self.assertIn("CLOB_BOOK_DELETE_BATCH_SIZE", script)
        self.assertIn("CLOB_BOOK_DELETE_MAX_BATCHES", script)
        self.assertIn("$DATA_DIR/lake/orderbook_snapshots", script)
        self.assertIn("LIMIT ${CLOB_BOOK_DELETE_BATCH_SIZE}", script)
        self.assertIn("archived_clob_book_days", script)
        self.assertIn("PLOY_CLOB_BOOK_REQUIRE_ARCHIVE", script)
        self.assertIn("received_at::date IN (SELECT archive_day", script)
        self.assertNotIn(
            "DELETE FROM clob_orderbook_snapshots WHERE received_at <",
            script,
        )
        self.assertNotIn("VACUUM (ANALYZE) clob_orderbook_snapshots", script)
        self.assertIn("ANALYZE clob_orderbook_snapshots", script)

    def test_runtime_artifact_dirs_are_pruned(self) -> None:
        script = SCRIPT_PATH.read_text()
        self.assertIn('prune_tree_dir "$TMP_DIR" "tmp"', script)
        self.assertIn('prune_tree_dir "$DATA_DIR/recordings" "recordings"', script)
        self.assertIn('prune_tree_dir "$DATA_DIR/parquet" "parquet"', script)

    def test_clob_orderbook_partition_drop_is_archive_gated(self) -> None:
        script = SCRIPT_PATH.read_text()
        self.assertIn("parent.relname = 'clob_orderbook_snapshots'", script)
        self.assertIn("SELECT archive_day FROM archived_clob_book_days", script)

    def test_split_orderbook_retention_is_archive_gated(self) -> None:
        script = ORDERBOOK_RETENTION_SCRIPT_PATH.read_text()
        self.assertIn("PLOY_CLOB_BOOK_REQUIRE_ARCHIVE", script)
        self.assertIn("/opt/ploy/data/lake/orderbook_snapshots", script)
        self.assertIn("archived_clob_book_days", script)
        self.assertIn("archive_complete", script)
        self.assertIn("received_at::date IN (SELECT archive_day", script)

    def test_orderbook_archive_export_is_single_threaded(self) -> None:
        script = ORDERBOOK_EXPORT_SCRIPT_PATH.read_text()
        self.assertIn("SET threads=1", script)
        self.assertIn("SET pg_connection_limit=1", script)
        self.assertIn("SET pg_use_ctid_scan=false", script)

    def test_orderbook_archive_timer_runs_gap_filler(self) -> None:
        script = ORDERBOOK_BACKFILL_SCRIPT_PATH.read_text()
        service = ORDERBOOK_ARCHIVE_SERVICE_PATH.read_text()
        self.assertIn("PLOY_CLOB_BOOK_ARCHIVE_LOOKBACK_HOURS", script)
        self.assertIn("PLOY_CLOB_BOOK_ARCHIVE_MAX_HOURS_PER_RUN", script)
        self.assertIn("hour=${export_hour}/_SUCCESS", script)
        self.assertIn("archive_clob_orderbook_snapshots_backfill.sh", service)


if __name__ == "__main__":
    unittest.main()
