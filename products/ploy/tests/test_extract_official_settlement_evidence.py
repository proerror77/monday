import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "extract_official_settlement_evidence.py"


def discovered(event_id, up_token, down_token):
    return {
        "sequence": 1,
        "recorded_at": "2026-05-12T09:00:00Z",
        "update": {
            "kind": "event_discovered",
            "event_id": event_id,
            "symbol": "BTCUSDT",
            "up_token": up_token,
            "down_token": down_token,
            "end_time": "2026-05-12T09:05:00Z",
            "resolved_up_won": None,
        },
    }


class ExtractOfficialSettlementEvidenceTests(unittest.TestCase):
    def run_script(self, recording_rows, db_rows):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            recording = tmp_path / "recording.ndjson"
            db_json = tmp_path / "db-settlements.json"
            output = tmp_path / "official-settlements.json"
            report = tmp_path / "report.json"
            token_ids = tmp_path / "token-ids.json"
            recording.write_text(
                "".join(json.dumps(row) + "\n" for row in recording_rows),
                encoding="utf-8",
            )
            db_json.write_text(json.dumps(db_rows), encoding="utf-8")

            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--recording",
                    str(recording),
                    "--output-token-ids",
                    str(token_ids),
                    "--db-settlements-json",
                    str(db_json),
                    "--output-json",
                    str(output),
                    "--report-json",
                    str(report),
                ],
                cwd=ROOT,
                check=True,
                capture_output=True,
                text=True,
            )
            return (
                json.loads(output.read_text(encoding="utf-8")),
                json.loads(report.read_text(encoding="utf-8")),
                json.loads(token_ids.read_text(encoding="utf-8")),
            )

    def test_builds_event_and_token_settlement_from_official_rows(self):
        settlements, report, token_ids = self.run_script(
            [discovered("evt-1", "token-up", "token-down")],
            [
                {
                    "token_id": "token-up",
                    "settlement": "1.000000",
                    "resolved": True,
                },
                {
                    "token_id": "token-down",
                    "settlement": "0.000000",
                    "resolved": True,
                },
            ],
        )

        self.assertEqual(token_ids, ["token-down", "token-up"])
        self.assertEqual(report["official_settlement_event_count"], 1)
        self.assertEqual(report["official_settlement_count"], 2)
        self.assertEqual(
            settlements,
            [
                {
                    "event_id": "evt-1",
                    "resolved_up_won": True,
                    "settlement": "0.000000",
                    "source": "pm_token_settlements",
                    "token_id": "token-down",
                },
                {
                    "event_id": "evt-1",
                    "resolved_up_won": True,
                    "settlement": "1.000000",
                    "source": "pm_token_settlements",
                    "token_id": "token-up",
                },
            ],
        )

    def test_filters_unmapped_unresolved_and_conflicting_rows(self):
        settlements, report, _ = self.run_script(
            [discovered("evt-1", "token-up", "token-down")],
            [
                {"token_id": "token-up", "settlement": "1", "resolved": True},
                {"token_id": "token-down", "settlement": "1", "resolved": True},
                {"token_id": "other", "settlement": "1", "resolved": True},
                {"token_id": "token-up", "settlement": "1", "resolved": False},
            ],
        )

        self.assertEqual(settlements, [])
        self.assertEqual(report["conflicting_event_count"], 1)
        self.assertEqual(report["skipped"]["unmapped_token"], 1)
        self.assertEqual(report["skipped"]["unresolved"], 1)


if __name__ == "__main__":
    unittest.main()
