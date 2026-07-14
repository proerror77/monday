import json
import sys
import unittest
from pathlib import Path
from unittest import mock

from scripts import run_settlement_probability_prd_gate as gate


ROOT = Path(__file__).resolve().parents[1]
WORKFLOW = ROOT / ".github" / "workflows" / "settlement-probability-prd-gate.yml"

READY_REPORT = """=== Settlement Probability PRD Promotion Gate ===
ready_for_dry_run_handoff=true stake_usd=15.00 min_entry_fill_rate=0.3000 max_ece=0.0500 min_positive_window_ratio=0.60 require_deribit=false include_deribit=false data_quality_mode=event_complete event_complete_events=100 event_complete_rows=200 replay_parity_ready=true
gate,passed,evidence
data_quality,true,mode=event_complete
recorded_replay_parity,true,blocking_flags=<none>
"""


def ready_handoff() -> dict:
    runtime_score = "autofactor_formula:auto_settlement_conservative_settlement_edge"
    return {
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
        "strategies": [{"runtime_score": runtime_score}],
    }


class SettlementProbabilityPrdGateTests(unittest.TestCase):
    def run_main(self, argv, *, dispatch):
        with mock.patch.object(sys, "argv", ["run_settlement_probability_prd_gate.py", *argv]):
            with mock.patch.object(
                gate,
                "refresh_run",
                return_value=gate.WorkflowRun(
                    database_id=12345,
                    status="completed",
                    conclusion="success",
                    url="https://example.invalid/run/12345",
                    created_at=gate.parse_created_at("2026-05-22T00:00:00Z"),
                ),
            ):
                with mock.patch.object(gate, "dispatch_workflow", side_effect=dispatch):
                    return gate.main()

    def test_existing_snapshot_leaves_snapshot_bound_sampling_for_manifest_resolution(self):
        dispatches = []

        def capture_dispatch(workflow, fields, *, workflow_ref, dry_run):
            dispatches.append((workflow, fields, workflow_ref, dry_run))
            return None

        status = self.run_main(
            [
                "--git-ref",
                "main",
                "--start-date",
                "2026-05-16",
                "--end-date",
                "2026-05-20",
                "--snapshot-run-id",
                "12345",
                "--replay-parity-run-id",
                "26301157711:recorded-replay-parity-26301157711",
                "--dry-run",
            ],
            dispatch=capture_dispatch,
        )

        self.assertEqual(status, 0)
        self.assertEqual(len(dispatches), 1)
        workflow, fields, _, _ = dispatches[0]
        self.assertEqual(workflow, gate.HOSTED_WALK_FORWARD_WORKFLOW)
        options = json.loads(fields["options_json"])
        self.assertEqual(options["lob_sample_secs"], "")
        self.assertEqual(options["pm_book_sample_secs"], "")
        self.assertEqual(options["observation_sample_secs"], "")
        self.assertEqual(options["max_quote_age_secs"], "")
        self.assertEqual(options["replay_parity_run_id"], "26301157711")
        self.assertEqual(
            options["replay_parity_artifact_name"],
            "recorded-replay-parity-26301157711",
        )

    def test_blocks_missing_snapshot_by_default(self):
        dispatches = []

        def capture_dispatch(workflow, fields, *, workflow_ref, dry_run):
            dispatches.append((workflow, fields, workflow_ref, dry_run))
            return None

        status = self.run_main(
            [
                "--git-ref",
                "main",
                "--start-date",
                "2026-05-16",
                "--end-date",
                "2026-05-20",
                "--dry-run",
            ],
            dispatch=capture_dispatch,
        )

        self.assertEqual(status, 2)
        self.assertEqual(dispatches, [])

    def test_legacy_snapshot_build_flag_is_removed(self):
        with mock.patch.object(
            sys,
            "argv",
            [
                "run_settlement_probability_prd_gate.py",
                "--git-ref",
                "main",
                "--start-date",
                "2026-05-16",
                "--end-date",
                "2026-05-20",
                "--allow-legacy-snapshot-build",
                "--dry-run",
            ],
        ):
            with self.assertRaises(SystemExit):
                gate.parse_args()

    def test_workflow_requires_snapshot_and_does_not_expose_legacy_build(self):
        workflow = WORKFLOW.read_text(encoding="utf-8")

        self.assertIn("Required run id with a complete sampled research-snapshot artifact", workflow)
        self.assertIn("required: true", workflow)
        self.assertIn("missing-required-snapshot", workflow)
        self.assertNotIn("legacy-build", workflow)
        self.assertNotIn("--allow-legacy-snapshot-build", workflow)

    def test_download_gate_requires_runtime_replay_handoff(self):
        def fake_download(args, *, dry_run=False):
            output_dir = Path(args[args.index("--dir") + 1])
            artifact = output_dir / "factor-walk-forward-v2"
            artifact.mkdir(parents=True)
            (artifact / "report.txt").write_text(READY_REPORT, encoding="utf-8")
            handoff = ready_handoff()
            handoff["candidate_strategy_replay"]["basis"] = "factor_walk_forward_top_bucket_aggregate"
            (artifact / "autofactor-strategy-handoff.json").write_text(
                json.dumps(handoff, indent=2, sort_keys=True),
                encoding="utf-8",
            )
            return ""

        with mock.patch.object(gate, "run_command", side_effect=fake_download):
            result = gate.download_and_evaluate_promotion_gate(12345)

        self.assertFalse(result.ready)
        self.assertIn(
            "handoff_replay_gate:candidate_strategy_replay_not_runtime_replay:"
            "factor_walk_forward_top_bucket_aggregate!=runtime_market_update_replay",
            result.blocked_gates,
        )

    def test_download_gate_accepts_runtime_replay_handoff(self):
        def fake_download(args, *, dry_run=False):
            output_dir = Path(args[args.index("--dir") + 1])
            artifact = output_dir / "factor-walk-forward-v2"
            artifact.mkdir(parents=True)
            (artifact / "report.txt").write_text(READY_REPORT, encoding="utf-8")
            (artifact / "autofactor-strategy-handoff.json").write_text(
                json.dumps(ready_handoff(), indent=2, sort_keys=True),
                encoding="utf-8",
            )
            return ""

        with mock.patch.object(gate, "run_command", side_effect=fake_download):
            result = gate.download_and_evaluate_promotion_gate(12345)

        self.assertTrue(result.ready)
        self.assertEqual(result.blocked_gates, ())


if __name__ == "__main__":
    unittest.main()
