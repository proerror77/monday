import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "extract_dryrun_settlement_evidence.py"


class ExtractDryrunSettlementEvidenceTests(unittest.TestCase):
    def run_script(self, payload, extra_args=None):
        with tempfile.TemporaryDirectory() as tmp:
            tmp_path = Path(tmp)
            input_path = tmp_path / "dryrun.json"
            output_path = tmp_path / "settlements.json"
            input_path.write_text(json.dumps(payload), encoding="utf-8")
            args = [
                sys.executable,
                str(SCRIPT),
                "--dryrun-json",
                str(input_path),
                "--output-json",
                str(output_path),
            ]
            if extra_args:
                args.extend(extra_args)
            subprocess.run(args, cwd=ROOT, check=True, capture_output=True, text=True)
            return json.loads(output_path.read_text(encoding="utf-8"))

    def test_extracts_buy_event_settlement_for_replay_enrichment(self):
        payload = {
            "runtime_evidence": {
                "events": [
                    {
                        "deployment_id": "pm5d.threelayer.test",
                        "decision_ts": "2026-05-11T19:48:12.316+08:00",
                        "event_id": "2221812",
                        "token_id": "token-up",
                        "intent_id": "tl_btcusdt_up_2221812_1778500092316",
                        "market_side": "UP",
                        "side": "BUY",
                        "settlement": "1.00000000000000000000",
                    },
                    {
                        "deployment_id": "pm5d.threelayer.test",
                        "decision_ts": "2026-05-11T19:50:00+08:00",
                        "event_id": "2221812",
                        "token_id": "token-up",
                        "intent_id": "tl_settle_2221812_up",
                        "market_side": "UP",
                        "side": "SELL",
                        "settlement": "open",
                    },
                ]
            }
        }

        settlements = self.run_script(
            payload,
            [
                "--deployment-id",
                "pm5d.threelayer.test",
                "--since",
                "2026-05-11T19:47:00+08:00",
                "--until",
                "2026-05-11T19:50:30+08:00",
            ],
        )

        self.assertEqual(
            settlements,
            [
                {
                    "event_id": "2221812",
                    "token_id": "token-up",
                    "settlement": "1.00000000000000000000",
                    "resolved_up_won": True,
                }
            ],
        )

    def test_filters_outside_window_and_open_rows(self):
        payload = {
            "runtime_evidence": {
                "events": [
                    {
                        "deployment_id": "pm5d.threelayer.test",
                        "decision_ts": "2026-05-11T19:46:59+08:00",
                        "event_id": "before",
                        "token_id": "token",
                        "market_side": "UP",
                        "side": "BUY",
                        "settlement": "1",
                    },
                    {
                        "deployment_id": "pm5d.threelayer.test",
                        "decision_ts": "2026-05-11T19:48:00+08:00",
                        "event_id": "open",
                        "token_id": "token",
                        "market_side": "UP",
                        "side": "BUY",
                        "settlement": "open",
                    },
                ]
            }
        }

        settlements = self.run_script(
            payload,
            [
                "--deployment-id",
                "pm5d.threelayer.test",
                "--since",
                "2026-05-11T19:47:00+08:00",
                "--until",
                "2026-05-11T19:50:30+08:00",
            ],
        )

        self.assertEqual(settlements, [])


if __name__ == "__main__":
    unittest.main()
