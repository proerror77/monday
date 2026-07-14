import importlib.util
import sys
from datetime import timezone
from decimal import Decimal
from pathlib import Path
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
IMPORTER_SCRIPT = ROOT / "scripts" / "import_polymarket_v2_indexer.py"
MIGRATION = ROOT / "migrations" / "040_polymarket_v2_indexer_events.sql"
DEPLOY_WORKFLOW = ROOT / ".github" / "workflows" / "deploy-tango-1-1.yml"
IMPORT_SERVICE = ROOT / "deployment" / "systemd" / "ploy-polymarket-v2-indexer-import.service"
IMPORT_TIMER = ROOT / "deployment" / "systemd" / "ploy-polymarket-v2-indexer-import.timer"


def load_importer_module():
    spec = importlib.util.spec_from_file_location(
        "import_polymarket_v2_indexer", IMPORTER_SCRIPT
    )
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class PolymarketV2IndexerContractTests(unittest.TestCase):
    def test_migration_adds_sidecar_tables_without_touching_runtime_tables(self) -> None:
        migration = MIGRATION.read_text()

        self.assertIn("CREATE TABLE IF NOT EXISTS polymarket_v2_order_fills", migration)
        self.assertIn("CREATE TABLE IF NOT EXISTS polymarket_v2_polyusd_events", migration)
        self.assertIn("CREATE TABLE IF NOT EXISTS polymarket_v2_indexer_sync_state", migration)
        self.assertIn("UNIQUE (chain_id, block_number, log_index)", migration)
        self.assertIn("not realtime trading signals", migration)
        self.assertNotIn("ALTER TABLE strategy_runtime_fills", migration)
        self.assertNotIn("ALTER TABLE clob_quote_ticks", migration)

    def test_order_fill_normalization_preserves_raw_chain_fields(self) -> None:
        importer = load_importer_module()
        normalized = importer.normalize_order_fill(
            {
                "id": "137_84902321_7",
                "orderHash": "0xorder",
                "maker": "0xmaker",
                "taker": "0xtaker",
                "side": 0,
                "tokenId": "12345",
                "market": {"slug": "btc-updown-15m"},
                "makerAmountFilled": "1000000",
                "takerAmountFilled": "420000",
                "fee": "120",
                "builder": "0xbuilder",
                "metadata": "0xmeta",
                "exchange": "0xe111180000d2663c0091e4f400237545b87b996b",
                "timestamp": 1777440000,
                "blockNumber": 84902321,
                "transactionHash": "0xtx",
                "txFrom": "0xsender",
            }
        )

        row = normalized.row
        self.assertEqual(normalized.table, "polymarket_v2_order_fills")
        self.assertEqual(row["chain_id"], 137)
        self.assertEqual(row["block_number"], 84902321)
        self.assertEqual(row["log_index"], 7)
        self.assertEqual(row["token_id"], "12345")
        self.assertEqual(row["market_id"], "btc-updown-15m")
        self.assertEqual(row["maker_amount_raw"], Decimal("1000000"))
        self.assertEqual(row["fee_raw"], Decimal("120"))
        self.assertEqual(row["block_timestamp"].tzinfo, timezone.utc)

    def test_polyusd_wrap_normalization_uses_single_flow_table(self) -> None:
        importer = load_importer_module()
        normalized = importer.normalize_polyusd_wrap(
            {
                "id": "137_84902322_2",
                "eventType": "unwrap",
                "caller": "0xwallet",
                "asset": "0x2791bca1f2de4661ed88a30c99a7a9449aa84174",
                "to": "0xwallet",
                "amount": "25000000",
                "timestamp": "2026-04-29T12:00:00Z",
                "blockNumber": 84902322,
                "transactionHash": "0xtx2",
            }
        )

        self.assertEqual(normalized.table, "polymarket_v2_polyusd_events")
        self.assertEqual(normalized.row["event_type"], "unwrap")
        self.assertEqual(normalized.row["caller"], "0xwallet")
        self.assertEqual(normalized.row["amount_raw"], Decimal("25000000"))

    def test_jsonl_input_requires_entity_and_groups_events(self) -> None:
        importer = load_importer_module()
        with tempfile.NamedTemporaryFile("w", suffix=".jsonl") as handle:
            handle.write('{"entity":"FeeEvent","id":"137_1_1","receiver":"0xfee","amount":"4","timestamp":1,"blockNumber":1,"transactionHash":"0xtx"}\n')
            handle.flush()

            grouped = importer.read_input(Path(handle.name))

        self.assertEqual(list(grouped), ["FeeEvent"])
        self.assertEqual(grouped["FeeEvent"][0]["receiver"], "0xfee")

    def test_importer_and_timer_are_wired_for_deploy_without_realtime_dependency(self) -> None:
        workflow = DEPLOY_WORKFLOW.read_text()
        service = IMPORT_SERVICE.read_text()
        timer = IMPORT_TIMER.read_text()

        self.assertIn("040_polymarket_v2_indexer_events.sql", workflow)
        self.assertIn("scripts/import_polymarket_v2_indexer.py", workflow)
        self.assertIn("ploy-polymarket-v2-indexer-import.timer", workflow)
        self.assertIn("polymarket-v2-indexer.service", workflow)
        self.assertIn("--from-sync-state", service)
        self.assertIn("PLOY_PM_V2_INDEXER_URL not configured; skipping", service)
        self.assertIn("OnUnitActiveSec=10m", timer)


if __name__ == "__main__":
    unittest.main()
