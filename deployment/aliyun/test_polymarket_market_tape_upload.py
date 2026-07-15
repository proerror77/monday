import importlib.util
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path


MODULE_PATH = Path(__file__).with_name("polymarket_market_tape_upload.py")
SPEC = importlib.util.spec_from_file_location("polymarket_market_tape_upload", MODULE_PATH)
UPLOADER = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
sys.modules[SPEC.name] = UPLOADER
SPEC.loader.exec_module(UPLOADER)


def record(sequence, recorded_at, update):
    return {"sequence": sequence, "recorded_at": recorded_at, "update": update}


class TapeValidationTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.spool = Path(self.temporary.name)

    def tearDown(self):
        self.temporary.cleanup()

    def write_tape(self, rows, name="market-updates.20260715T010000.ndjson"):
        path = self.spool / name
        path.write_text("".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8")
        return path

    def sample_rows(self):
        return [
            record(
                0,
                "2026-07-15T01:00:00.000000000Z",
                {
                    "kind": "event_discovered",
                    "event_id": "event-1",
                    "symbol": "BTCUSDT",
                    "up_token": "up-1",
                    "down_token": "down-1",
                    "end_time": "2026-07-15T01:05:00Z",
                    "window_secs": 300,
                    "price_to_beat": "100",
                    "resolved_up_won": None,
                },
            ),
            record(
                1,
                "2026-07-15T01:00:01.000000000Z",
                {
                    "kind": "quote",
                    "token_id": "up-1",
                    "bid": "0.49",
                    "ask": "0.51",
                    "bid_size": "10",
                    "ask_size": "11",
                    "bid_levels": [{"price": "0.49", "size": "10"}],
                    "ask_levels": [{"price": "0.51", "size": "11"}],
                    "ts": "2026-07-15T01:00:01Z",
                },
            ),
            record(
                2,
                "2026-07-15T01:00:02.000000000Z",
                {
                    "kind": "reference_price",
                    "symbol": "BTCUSDT",
                    "source": "binance",
                    "asset_class": "crypto",
                    "price": "100",
                    "full_accuracy_value": None,
                    "is_carried_forward": False,
                    "ts": "2026-07-15T01:00:02Z",
                },
            ),
        ]

    def test_scan_tape_builds_replay_and_field_quality_manifest(self):
        path = self.write_tape(self.sample_rows())

        manifest = UPLOADER.scan_tape(path, "crypto_expiry", 1, 1000)

        self.assertEqual(manifest["events"], 3)
        self.assertEqual(manifest["sequence_gaps"], 0)
        self.assertEqual(manifest["event_types"]["quote"], 1)
        self.assertEqual(manifest["symbols"], ["BTCUSDT"])
        self.assertEqual(manifest["token_count"], 1)
        self.assertEqual(manifest["quality"]["crossed_quotes"], 0)
        self.assertEqual(manifest["quality"]["max_bid_levels"], 1)
        self.assertEqual(manifest["field_non_null"]["quote"]["bid"], 1)

    def test_scan_tape_rejects_sequence_gap(self):
        rows = self.sample_rows()
        rows[1]["sequence"] = 2
        path = self.write_tape(rows)

        with self.assertRaisesRegex(ValueError, "sequence gap"):
            UPLOADER.scan_tape(path, "crypto_expiry", 1, 1000)

    def test_scan_tape_rejects_incomplete_final_record(self):
        path = self.write_tape(self.sample_rows())
        with path.open("ab") as handle:
            handle.write(b'{"sequence":3')

        with self.assertRaisesRegex(ValueError, "incomplete record"):
            UPLOADER.scan_tape(path, "crypto_expiry", 1, 1000)

    def test_discovery_excludes_active_tape(self):
        self.write_tape(self.sample_rows(), "market-updates.ndjson")
        rotated = self.write_tape(self.sample_rows())

        self.assertEqual(UPLOADER.discover_rotated_tapes(self.spool), [rotated])

    @unittest.skipUnless(shutil.which("zstd"), "zstd is required for artifact check")
    def test_prepare_artifacts_emits_hash_bound_manifest_and_success(self):
        source = self.write_tape(self.sample_rows())

        artifacts, manifest = UPLOADER.prepare_artifacts(
            source, "crypto_expiry", 1, 1000, 30
        )

        self.assertTrue(source.exists())
        self.assertTrue(artifacts.data.exists())
        self.assertEqual(artifacts.success.read_text().strip(), manifest["sha256"])
        self.assertEqual(json.loads(artifacts.manifest.read_text())["sha256"], manifest["sha256"])
        self.assertEqual(
            artifacts.object_prefix,
            "lake/raw/venue=polymarket/dataset=crypto_expiry/date=2026-07-15/hour=01",
        )


if __name__ == "__main__":
    unittest.main()
