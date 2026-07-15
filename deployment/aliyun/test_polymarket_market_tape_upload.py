import importlib.util
import json
import shutil
import sys
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import patch


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

    def test_scan_tape_rejects_malformed_quote_numbers(self):
        rows = self.sample_rows()
        rows[1]["update"]["bid"] = "not-a-number"
        path = self.write_tape(rows)

        with self.assertRaisesRegex(ValueError, "bid must be numeric"):
            UPLOADER.scan_tape(path, "crypto_expiry", 1, 1000)

    def test_scan_tape_rejects_malformed_depth_level(self):
        rows = self.sample_rows()
        rows[1]["update"]["bid_levels"][0]["size"] = None
        path = self.write_tape(rows)

        with self.assertRaisesRegex(ValueError, "requires price and size"):
            UPLOADER.scan_tape(path, "crypto_expiry", 1, 1000)

    def test_scan_tape_marks_quotes_without_event_context_as_non_self_contained(self):
        path = self.write_tape([self.sample_rows()[1]])
        row = json.loads(path.read_text())
        row["sequence"] = 0
        path.write_text(json.dumps(row) + "\n", encoding="utf-8")

        manifest = UPLOADER.scan_tape(path, "crypto_expiry", 1, 1000)

        self.assertFalse(manifest["event_context_complete"])
        self.assertEqual(manifest["quality"]["contextless_quotes"], 1)
        self.assertIn("requires_prior_event_context", manifest["replay_scope"])

    def test_discovery_excludes_active_tape(self):
        self.write_tape(self.sample_rows(), "market-updates.ndjson")
        rotated = self.write_tape(self.sample_rows())

        self.assertEqual(UPLOADER.discover_rotated_tapes(self.spool), [rotated])

    def test_split_tape_by_utc_hour_preserves_contiguous_global_sequences(self):
        rows = self.sample_rows()
        rows.append(
            record(
                3,
                "2026-07-15T02:00:00.000000000Z",
                {"kind": "event_expired", "event_id": "event-1", "end_time": None},
            )
        )
        source = self.write_tape(rows)

        chunks = UPLOADER.split_tape_by_utc_hour(source)

        self.assertEqual(len(chunks), 2)
        first = UPLOADER.scan_tape(chunks[0], "crypto_expiry", 1, 1000)
        second = UPLOADER.scan_tape(chunks[1], "crypto_expiry", 1, 1000)
        self.assertEqual((first["start_sequence"], first["end_sequence"]), (0, 2))
        self.assertEqual((second["start_sequence"], second["end_sequence"]), (3, 3))
        self.assertEqual((first["hour"], second["hour"]), ("01", "02"))

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

    @unittest.skipUnless(shutil.which("zstd"), "zstd is required for artifact check")
    def test_remote_verification_reads_back_hash_bound_triplet(self):
        source = self.write_tape(self.sample_rows())
        artifacts, _ = UPLOADER.prepare_artifacts(
            source, "crypto_expiry", 1, 1000, 30
        )
        remote = {
            path.name: path.read_bytes()
            for path in (artifacts.data, artifacts.manifest, artifacts.success)
        }

        def download(command, **_kwargs):
            Path(command[4]).write_bytes(remote[Path(command[3]).name])

        with patch.object(UPLOADER.subprocess, "run", side_effect=download):
            UPLOADER.verify_remote_artifacts(
                artifacts, "bucket", "endpoint", "region", "profile", 30
            )

    def test_run_continues_after_one_bad_closed_tape(self):
        first = self.write_tape(self.sample_rows(), "market-updates.20260715T010000.ndjson")
        second = self.write_tape(self.sample_rows(), "market-updates.20260715T020000.ndjson")
        args = SimpleNamespace(
            spool_dir=self.spool,
            dataset="crypto_expiry",
            quote_depth_levels=1,
            quote_sample_ms=1000,
            zstd_timeout=30,
            bucket="bucket",
            endpoint="endpoint",
            region="region",
            profile="profile",
            oss_timeout=30,
        )

        def archive(source, _args):
            if source == first:
                raise ValueError("bad tape")
            source.unlink()
            return ["oss://bucket/second"]

        with patch.object(UPLOADER, "archive_source", side_effect=archive) as mocked:
            result = UPLOADER.run(args)

        self.assertEqual(result, 1)
        self.assertEqual(mocked.call_count, 2)
        status = json.loads((self.spool / "upload-status.json").read_text())
        self.assertEqual(status["pending_segments"], 1)
        self.assertEqual(status["failed_segments"][0]["source"], first.name)

    def test_archive_source_rejects_a_truncated_session_prefix(self):
        rows = self.sample_rows()
        for sequence, row in enumerate(rows, start=5):
            row["sequence"] = sequence
        source = self.write_tape(rows)
        args = SimpleNamespace(
            dataset="crypto_expiry",
            quote_depth_levels=1,
            quote_sample_ms=1000,
        )

        with self.assertRaisesRegex(ValueError, "must start at sequence 0"):
            UPLOADER.archive_source(source, args)


if __name__ == "__main__":
    unittest.main()
