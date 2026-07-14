import pathlib
import unittest


ROOT = pathlib.Path(__file__).resolve().parents[1]


class ReplayBacktestEvidenceContractTest(unittest.TestCase):
    def test_run_backtest_artifact_names_promotion_blockers(self):
        source = (ROOT / "crates/ploy-strategy-bundles/examples/run_backtest.rs").read_text()

        required_snippets = [
            '"evidence_stage"',
            '"promotion_ready": false',
            '"promotion_decision": "pending replay/dry-run parity review"',
            '"source"',
            '"data_surfaces"',
            '"blocking_risk_flags"',
            '"advisory_flags"',
            '"missing_full_depth_clob_fillability"',
            '"missing_replay_dryrun_parity"',
            '"missing_runtime_scorer_parity"',
            '"parquet_stream_uses_quote_ticks_not_full_clob_lake"',
            '"max_drawdown"',
            '"unique_event_count"',
            '"max_event_decisions"',
            '"full_depth_fills_observed"',
            '"incomplete_event_lifecycle_accounting"',
            '"lifecycle_without_entry_decision"',
            '"lifecycle_without_entry_decision_count"',
        ]
        for snippet in required_snippets:
            self.assertIn(snippet, source)

    def test_backtest_workflow_surfaces_gate_fields(self):
        workflow = (ROOT / ".github/workflows/backtest.yml").read_text()

        required_snippets = [
            "full-depth orderbook_snapshots preferred",
            "orderbook_snapshots/date=${day}",
            "evidence_stage",
            "promotion_ready",
            "promotion_decision",
            "blocking_risk_flags",
            "Blocking promotion flags",
            "parity:blocked",
            "evidence-stage.json",
            '"evidence_stage": stage',
            '"promotion_ready": False',
            '"promotion_decision": decision',
            "diagnostic_backtest_not_promotion_evidence",
            "evidence:diagnostic",
            "evidence:executable-replay",
            "full-depth archive checksum mismatch",
            "TZ=Asia/Shanghai",
            "process.env.ISSUE_NUMBER",
            "process.env.GIT_REF",
        ]
        for snippet in required_snippets:
            self.assertIn(snippet, workflow)
        self.assertNotIn('--git-ref "${{ github.event.inputs.git_ref }}"', workflow)
        self.assertNotIn('Number("${{ github.event.inputs.issue_number }}")', workflow)

        parquet_source = (
            ROOT / "crates/ploy-strategy-bundles/src/feed/parquet_stream.rs"
        ).read_text()
        self.assertIn("sha256_file(&parquet_path)? != expected_sha256", parquet_source)

    def test_research_runbook_preserves_backtest_caveat(self):
        runbook = (ROOT / "docs/runbooks/strategy-research-cicd.md").read_text()

        required_snippets = [
            "not as dry-run or live promotion evidence",
            "/opt/ploy/data/lake/orderbook_snapshots",
            "full-depth CLOB fillability",
            "full_depth_sweep",
            "`evidence_stage`, `promotion_ready`, `blocking_risk_flags`, and",
        ]
        for snippet in required_snippets:
            self.assertIn(snippet, runbook)


if __name__ == "__main__":
    unittest.main()
