import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "resolve_recorded_replay_window.py"


def write_ndjson(path, rows):
    path.write_text("\n".join(json.dumps(row) for row in rows) + "\n", encoding="utf-8")


class ResolveRecordedReplayWindowTests(unittest.TestCase):
    def run_script(self, recording_rows, dryrun_payload):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            recording = tmp_path / "recording.ndjson"
            dryrun = tmp_path / "dryrun.json"
            output = tmp_path / "window.json"
            env = tmp_path / "window.env"
            write_ndjson(recording, recording_rows)
            dryrun.write_text(json.dumps(dryrun_payload), encoding="utf-8")

            subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--recording",
                    str(recording),
                    "--dryrun-json",
                    str(dryrun),
                    "--deployment-id",
                    "pm5d.test",
                    "--output-json",
                    str(output),
                    "--output-env",
                    str(env),
                ],
                cwd=ROOT,
                check=True,
                capture_output=True,
                text=True,
            )
            return json.loads(output.read_text(encoding="utf-8")), env.read_text(encoding="utf-8")

    def test_prefers_closed_rows_inside_recording_coverage(self):
        window, env = self.run_script(
            [
                {
                    "recorded_at": "2026-05-12T13:00:00Z",
                    "update": {"kind": "quote", "ts": "2026-05-12T13:00:00Z"},
                },
                {
                    "recorded_at": "2026-05-12T13:10:00Z",
                    "update": {"kind": "event_expired", "end_time": "2026-05-12T13:10:00Z"},
                },
            ],
            {
                "runtime_evidence": {
                    "events": [
                        {
                            "deployment_id": "pm5d.test",
                            "intent_id": "old",
                            "decision_ts": "2026-05-12T12:00:00Z",
                            "settlement": "0",
                        },
                        {
                            "deployment_id": "pm5d.test",
                            "intent_id": "closed",
                            "decision_ts": "2026-05-12T13:05:00Z",
                            "settlement": "1",
                        },
                        {
                            "deployment_id": "pm5d.test",
                            "intent_id": "open",
                            "decision_ts": "2026-05-12T13:06:00Z",
                            "settlement": "open",
                        },
                    ]
                }
            },
        )

        self.assertEqual(window["mode"], "auto_recording_intersection")
        self.assertEqual(window["selected_row_count"], 1)
        self.assertEqual(window["selected_closed_row_count"], 1)
        self.assertEqual(window["since"], "2026-05-12T13:04:00Z")
        self.assertEqual(window["until"], "2026-05-12T13:06:00Z")
        self.assertIn("RESOLVED_WINDOW_MODE=auto_recording_intersection", env)

    def test_falls_back_to_open_rows_when_no_closed_rows_overlap(self):
        window, _ = self.run_script(
            [
                {
                    "recorded_at": "2026-05-12T13:00:00Z",
                    "update": {"kind": "quote", "ts": "2026-05-12T13:00:00Z"},
                },
                {
                    "recorded_at": "2026-05-12T13:10:00Z",
                    "update": {"kind": "quote", "ts": "2026-05-12T13:10:00Z"},
                },
            ],
            {
                "runtime_evidence": {
                    "events": [
                        {
                            "deployment_id": "pm5d.test",
                            "intent_id": "open-1",
                            "decision_ts": "2026-05-12T13:06:00Z",
                            "settlement": "open",
                        }
                    ]
                }
            },
        )

        self.assertEqual(window["selected_row_count"], 1)
        self.assertEqual(window["selected_closed_row_count"], 0)
        self.assertEqual(window["since"], "2026-05-12T13:05:00Z")
        self.assertEqual(window["until"], "2026-05-12T13:07:00Z")


if __name__ == "__main__":
    unittest.main()
