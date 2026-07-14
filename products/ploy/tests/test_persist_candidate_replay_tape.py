from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "scripts" / "persist_candidate_replay_tape.py"
WORKFLOW = ROOT / ".github" / "workflows" / "runtime-candidate-replay.yml"


def load_module():
    spec = importlib.util.spec_from_file_location("persist_candidate_replay_tape", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


class PersistCandidateReplayTapeTest(unittest.TestCase):
    def replay_payload(self) -> dict:
        return {
            "schema_version": 1,
            "kind": "autofactor_candidate_strategy_replay",
            "candidate_replay_id": "candidate_replay:0123456789abcdef0123456789abcdef",
            "basis": "runtime_market_update_replay",
            "evidence_stage": "executable_replay",
            "source_workflow": "Runtime Candidate Replay:main",
            "workflow_run_id": "26548811702",
            "workflow_run_url": "https://github.com/proerror77/ploy/actions/runs/26548811702",
            "artifact_name": "runtime-candidate-replay-26548811702",
            "deployment_id": "pm5d.threelayer.settlement-probability-btc-eth.dryrun",
            "strategy_profile": "settlement_probability",
            "runtime_score": (
                "autofactor_formula:"
                "llm_mut_spread_adjusted_external_move_select_entry_price_quality_ge_075"
            ),
            "recording_path": "/opt/ploy/data/recordings/pm5d.ndjson",
            "recording_sha256": "a" * 64,
            "config_path": "/opt/ploy/config/strategies/pm5d.toml",
            "runner_source": "Runtime Candidate Replay:main",
            "runner_git_sha": "c32729542522f1e95735028177ffc7b527148da9",
            "source_factor": {
                "name": "",
                "dsl_hash": "",
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
            },
            "decision_contract": {
                "event_level": True,
                "full_depth_entry": True,
                "one_decision_per_event": True,
                "official_settlement": False,
                "target": "full_depth_settlement_executable_pnl",
                "horizon": "5m",
            },
            "acceptance_criteria": {"min_trade_count": 50},
            "metrics": {
                "trade_count": 16,
                "unique_event_count": 16,
                "entry_fill_rate": 1.0,
                "roi": 0.3427,
                "total_pnl": 82.25,
            },
            "blocking_risk_flags": [
                "trade_count_too_small:16<50",
                "official_settlement_missing:15<16",
            ],
            "promotion_ready": False,
            "promotion_decision": "blocked",
        }

    def write_payload(self, payload: dict) -> Path:
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        path = Path(tmp.name) / "candidate-strategy-replay.json"
        path.write_text(json.dumps(payload), encoding="utf-8")
        return path

    def test_build_row_persists_standalone_runtime_replay_without_snapshot(self) -> None:
        module = load_module()
        payload = self.replay_payload()
        path = self.write_payload(payload)

        row = module.build_row(
            candidate_replay_json=path,
            run_id="26548811702",
            source_workflow="runtime-candidate-replay.yml",
            workflow_run_id="26548811702",
            workflow_run_url="https://github.com/proerror77/ploy/actions/runs/26548811702",
            artifact_name="runtime-candidate-replay-26548811702",
            data_snapshot_id=None,
        )

        self.assertEqual("26548811702", row.run_id)
        self.assertEqual("runtime-candidate-replay.yml", row.source_workflow)
        self.assertEqual("26548811702", row.workflow_run_id)
        self.assertEqual("runtime_market_update_replay", row.basis)
        self.assertEqual("executable_replay", row.evidence_stage)
        self.assertIsNone(row.data_snapshot_id)
        self.assertEqual(
            "llm_mut_spread_adjusted_external_move_select_entry_price_quality_ge_075",
            row.factor_name,
        )
        self.assertEqual("full_depth_settlement_executable_pnl", row.target)
        self.assertEqual("5m", row.horizon)
        self.assertFalse(row.promotion_ready)
        self.assertEqual(
            ["trade_count_too_small:16<50", "official_settlement_missing:15<16"],
            row.blocking_risk_flags,
        )

    def test_rejects_candidate_replay_stage_basis_mismatch(self) -> None:
        module = load_module()
        payload = self.replay_payload()
        payload["evidence_stage"] = "walk_forward"
        path = self.write_payload(payload)

        with self.assertRaises(SystemExit) as raised:
            module.build_row(
                candidate_replay_json=path,
                run_id="26548811702",
                source_workflow="runtime-candidate-replay.yml",
                workflow_run_id="26548811702",
                workflow_run_url="",
                artifact_name="",
                data_snapshot_id=None,
            )

        self.assertIn("contradicts basis", str(raised.exception))

    def test_runtime_candidate_replay_workflow_persists_frontier_by_default(self) -> None:
        workflow = WORKFLOW.read_text(encoding="utf-8")

        self.assertIn('"persist_research_trace":true', workflow)
        self.assertIn("REPLAY_PERSIST_RESEARCH_TRACE", workflow)
        self.assertIn("scripts/persist_candidate_replay_tape.py", workflow)
        self.assertIn("Durable Research OS replay frontier", workflow)
        self.assertIn("--candidate-replay-json", workflow)
        self.assertIn("--db-url \"${PLOY_DATABASE__URL}\"", workflow)


if __name__ == "__main__":
    unittest.main()
