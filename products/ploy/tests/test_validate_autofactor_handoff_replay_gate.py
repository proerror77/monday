import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "validate_autofactor_handoff_replay_gate.py"


def ready_handoff() -> dict:
    runtime_score = "autofactor_formula:auto_settlement_conservative_settlement_edge"
    return {
        "schema_version": 1,
        "kind": "autofactor_strategy_handoff",
        "status": "ready",
        "candidate_strategy_replay": {
            "ready": True,
            "basis": "runtime_market_update_replay",
            "source_workflow": "runtime-candidate-replay.yml",
            "workflow_run_id": "26306734877",
            "workflow_run_url": "https://github.com/proerror77/ploy/actions/runs/26306734877",
            "artifact_name": "runtime-candidate-replay-26306734877",
            "candidate_replay_id": "candidate_replay:0123456789abcdef0123456789abcdef",
            "runtime_score": runtime_score,
            "source_factor": {
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
            },
            "decision_contract": {
                "event_level": True,
                "one_decision_per_event": True,
                "official_settlement": True,
                "full_depth_entry": True,
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
            },
        },
        "strategies": [
            {
                "name": "auto_settlement_conservative_settlement_edge",
                "runtime_score": runtime_score,
                "strategy_profile": "settlement_probability",
            }
        ],
    }


class ValidateAutoFactorHandoffReplayGateTests(unittest.TestCase):
    def run_script(self, handoff: dict, *, check: bool = True) -> tuple[dict, subprocess.CompletedProcess]:
        with tempfile.TemporaryDirectory() as raw_tmp:
            tmp = Path(raw_tmp)
            handoff_path = tmp / "handoff.json"
            output_path = tmp / "gate.json"
            handoff_path.write_text(json.dumps(handoff, indent=2, sort_keys=True), encoding="utf-8")
            result = subprocess.run(
                [
                    sys.executable,
                    str(SCRIPT),
                    "--handoff-json",
                    str(handoff_path),
                    "--output-json",
                    str(output_path),
                ],
                cwd=ROOT,
                text=True,
                capture_output=True,
                check=check,
            )
            payload = json.loads(output_path.read_text(encoding="utf-8"))
            return payload, result

    def test_accepts_runtime_candidate_replay_handoff(self):
        payload, result = self.run_script(ready_handoff())

        self.assertEqual(result.returncode, 0)
        self.assertTrue(payload["ready"])
        self.assertEqual(payload["blockers"], [])

    def test_blocks_aggregate_candidate_replay_handoff(self):
        handoff = ready_handoff()
        replay = handoff["candidate_strategy_replay"]
        replay["basis"] = "factor_walk_forward_top_bucket_aggregate"
        replay["source_workflow"] = ""
        replay["artifact_name"] = "factor-walk-forward-v2-26306734877"

        payload, result = self.run_script(handoff, check=False)

        self.assertEqual(result.returncode, 2)
        self.assertFalse(payload["ready"])
        self.assertIn(
            "candidate_strategy_replay_not_runtime_replay:"
            "factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
            payload["blockers"],
        )
        self.assertIn(
            "candidate_strategy_replay_source_workflow_mismatch:"
            "<missing>!=runtime-candidate-replay.yml",
            payload["blockers"],
        )
        self.assertIn(
            "candidate_strategy_replay_artifact_mismatch:"
            "factor-walk-forward-v2-26306734877!=runtime-candidate-replay-*",
            payload["blockers"],
        )

    def test_blocks_strategy_runtime_score_mismatch(self):
        handoff = ready_handoff()
        handoff["strategies"][0]["runtime_score"] = "autofactor_formula:other"

        payload, _ = self.run_script(handoff, check=False)

        self.assertFalse(payload["ready"])
        self.assertIn(
            "candidate_strategy_replay_runtime_score_mismatch:"
            "strategy_1:autofactor_formula:auto_settlement_conservative_settlement_edge!="
            "autofactor_formula:other",
            payload["blockers"],
        )

    def test_blocks_contract_horizon_mismatch(self):
        handoff = ready_handoff()
        handoff["candidate_strategy_replay"]["decision_contract"]["horizon"] = "30s"

        payload, _ = self.run_script(handoff, check=False)

        self.assertFalse(payload["ready"])
        self.assertIn(
            "candidate_strategy_replay_contract_horizon_mismatch:30s!=5m",
            payload["blockers"],
        )


if __name__ == "__main__":
    unittest.main()
