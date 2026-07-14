import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "enrich_recording_official_settlement.py"


def record(sequence, kind, event_id, resolved_up_won=None):
    return {
        "sequence": sequence,
        "recorded_at": "2026-05-11T08:00:00Z",
        "update": {
            "kind": kind,
            "event_id": event_id,
            "end_time": "2026-05-11T08:00:00Z",
            "resolved_up_won": resolved_up_won,
        },
    }


class EnrichRecordingOfficialSettlementTests(unittest.TestCase):
    def run_script(self, lines, settlements):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            input_path = tmp_path / "recording.ndjson"
            output_path = tmp_path / "recording-enriched.ndjson"
            settlements_path = tmp_path / "settlements.json"
            report_path = tmp_path / "report.json"
            input_path.write_text(
                "".join(json.dumps(line) + "\n" for line in lines),
                encoding="utf-8",
            )
            settlements_path.write_text(json.dumps(settlements), encoding="utf-8")
            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--input",
                    str(input_path),
                    "--output",
                    str(output_path),
                    "--settlements-json",
                    str(settlements_path),
                    "--report-json",
                    str(report_path),
                ],
                cwd=ROOT,
                check=True,
                capture_output=True,
                text=True,
            )
            output = [
                json.loads(line)
                for line in output_path.read_text(encoding="utf-8").splitlines()
            ]
            report = json.loads(report_path.read_text(encoding="utf-8"))
            return output, report

    def test_enriches_only_missing_event_expired_settlement(self):
        output, report = self.run_script(
            [
                record(1, "event_discovered", "evt-1", None),
                record(2, "event_expired", "evt-1", None),
            ],
            [{"event_id": "evt-1", "resolved_up_won": False}],
        )

        self.assertIsNone(output[0]["update"]["resolved_up_won"])
        self.assertFalse(output[1]["update"]["resolved_up_won"])
        self.assertEqual(report["event_expired_enriched"], 1)
        self.assertEqual(report["event_discovered_seen"], 1)

    def test_preserves_existing_settlement_and_unmapped_events(self):
        output, report = self.run_script(
            [
                record(1, "event_expired", "evt-1", True),
                record(2, "event_expired", "evt-2", None),
            ],
            {"evt-1": False},
        )

        self.assertTrue(output[0]["update"]["resolved_up_won"])
        self.assertIsNone(output[1]["update"]["resolved_up_won"])
        self.assertEqual(report["event_expired_enriched"], 0)
        self.assertEqual(report["event_expired_missing_settlement"], 1)


if __name__ == "__main__":
    unittest.main()
