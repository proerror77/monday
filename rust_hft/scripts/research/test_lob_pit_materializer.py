#!/usr/bin/env python3

import hashlib
import json
import subprocess
import tempfile
import unittest
from datetime import datetime, timezone
from pathlib import Path


SCRIPT = Path(__file__).with_name("lob_pit_materializer.py")


def epoch_ns(second: float) -> int:
    base = datetime(2026, 7, 14, tzinfo=timezone.utc).timestamp()
    return int((base + second) * 1_000_000_000)


def snapshot(received_second: float) -> dict:
    return {
        "received_at_ns": epoch_ns(received_second),
        "type": "snapshot",
        "session_id": "session-1",
        "symbol": "BTCUSDT",
        "snapshot": {
            "lastUpdateId": 100,
            "bids": [["100", "10"], ["99", "5"]],
            "asks": [["102", "4"], ["103", "6"]],
        },
    }


def diff(
    received_second: float,
    update_id: int,
    bids=None,
    asks=None,
    pu=None,
    first_update_id=None,
) -> dict:
    return {
        "received_at_ns": epoch_ns(received_second),
        "type": "diff",
        "session_id": "session-1",
        "frame": {
            "stream": "btcusdt@depth@100ms",
            "data": {
                "e": "depthUpdate",
                "E": epoch_ns(received_second) // 1_000_000,
                "s": "BTCUSDT",
                "U": update_id if first_update_id is None else first_update_id,
                "u": update_id,
                "pu": update_id - 1 if pu is None else pu,
                "b": bids or [],
                "a": asks or [],
            },
        },
    }


def checkpoint(received_second: float, update_id: int) -> dict:
    return {
        "received_at_ns": epoch_ns(received_second),
        "type": "checkpoint",
        "reason": "scheduled",
        "replay_safe": True,
        "session_id": "session-1",
        "symbol": "BTCUSDT",
        "last_update_id": update_id,
        "synced": True,
        "bridged": True,
        "bids": [["100", "12"], ["99", "5"]],
        "asks": [["101.5", "5"], ["102", "4"], ["103", "6"]],
    }


class SegmentFixture:
    def __init__(self, directory: Path, events: list[dict]):
        self.data = directory / "part-1.jsonl.zst"
        raw = directory / "part-1.jsonl"
        raw.write_text("".join(json.dumps(event) + "\n" for event in events))
        subprocess.run(
            ["zstd", "-q", "-f", str(raw), "-o", str(self.data)], check=True
        )
        raw.unlink()
        self.sha256 = hashlib.sha256(self.data.read_bytes()).hexdigest()
        self.manifest = self.data.with_name(self.data.name + ".manifest.json")
        self.success = self.data.with_name(self.data.name + "._SUCCESS")
        self.manifest.write_text(
            json.dumps(
                {
                    "schema": "binance.lob_tape.v2",
                    "venue": "binance",
                    "market": "usdm",
                    "dataset": "usdm_perpetual_all",
                    "shard_id": "all",
                    "symbols": ["btcusdt"],
                    "events": len(events),
                    "event_types": {
                        event_type: sum(
                            event["type"] == event_type for event in events
                        )
                        for event_type in {event["type"] for event in events}
                    },
                    "has_replay_safe_checkpoint": True,
                    "start_received_at_ns": events[0]["received_at_ns"],
                    "end_received_at_ns": events[-1]["received_at_ns"],
                    "file": self.data.name,
                    "bytes": self.data.stat().st_size,
                    "sha256": self.sha256,
                }
            )
            + "\n"
        )
        self.success.write_text(self.sha256 + "\n")


def valid_events() -> list[dict]:
    return [
        # USD-M's first bridging event can overlap the REST snapshot while pu
        # still points to the last pre-snapshot event.
        diff(
            0.05,
            101,
            asks=[["101", "8"]],
            pu=98,
            first_update_id=99,
        ),
        snapshot(0.1),
        # After bridging, USD-M continuity is pu == previous u; U can jump.
        diff(0.6, 175, bids=[["100", "10"]], pu=101, first_update_id=150),
        diff(1.4, 176, asks=[["101", "0"]], pu=175),
        diff(2.4, 177, bids=[["101", "3"]], pu=176),
        diff(3.4, 178, asks=[["101.5", "4"]], pu=177),
        diff(4.4, 179, bids=[["101", "0"]], pu=178),
        diff(5.4, 180, bids=[["100", "12"]], pu=179),
        diff(6.4, 181, asks=[["101.5", "5"]], pu=180),
        checkpoint(6.5, 181),
    ]


class LobPitMaterializerCliTest(unittest.TestCase):
    def run_materializer(self, fixture: SegmentFixture, directory: Path):
        output = directory / "features.jsonl"
        manifest = directory / "materialization.manifest.json"
        result = subprocess.run(
            [
                "python3",
                str(SCRIPT),
                "--mission-id",
                "data-btc-usdm-1",
                "--symbol",
                "BTCUSDT",
                "--market",
                "usdm",
                "--bucket-ms",
                "1000",
                "--label-horizon-buckets",
                "2",
                "--segment",
                str(fixture.data),
                "--output",
                str(output),
                "--manifest-out",
                str(manifest),
            ],
            capture_output=True,
            text=True,
        )
        return result, output, manifest

    def test_rejects_segment_without_matching_success_and_sha256(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            fixture = SegmentFixture(directory, valid_events())
            fixture.success.write_text("0" * 64 + "\n")

            result, output, _ = self.run_materializer(fixture, directory)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("SUCCESS marker", result.stderr)
            self.assertFalse(output.exists())

    def test_rejects_a_binance_sequence_gap(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            events = valid_events()
            events[3]["frame"]["data"]["pu"] = 174
            fixture = SegmentFixture(directory, events)

            result, output, _ = self.run_materializer(fixture, directory)

            self.assertNotEqual(result.returncode, 0)
            self.assertIn("sequence gap", result.stderr)
            self.assertFalse(output.exists())

    def test_materializes_point_in_time_features_and_delays_forward_labels(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            fixture = SegmentFixture(directory, valid_events())

            result, output, manifest_path = self.run_materializer(fixture, directory)

            self.assertEqual(result.returncode, 0, result.stderr)
            rows = [json.loads(line) for line in output.read_text().splitlines()]
            self.assertEqual(len(rows), 3)
            first = rows[0]
            self.assertEqual(first["symbol"], "BTCUSDT")
            self.assertEqual(first["modalities"], ["lob"])
            self.assertAlmostEqual(first["features"]["mid_price"], 101.0)
            self.assertAlmostEqual(first["label"], 101.25 / 101.0 - 1.0)
            self.assertEqual(
                first["label_available_time"], "2026-07-14T00:00:04Z"
            )
            self.assertEqual(first["event_time"], "2026-07-14T00:00:02Z")
            self.assertLess(
                first["feature_available_time"], first["label_available_time"]
            )
            materialization = json.loads(manifest_path.read_text())
            self.assertEqual(materialization["rows"], 3)
            self.assertEqual(materialization["source_segments"][0]["sha256"], fixture.sha256)


if __name__ == "__main__":
    unittest.main()
