import json
import tempfile
import unittest
from pathlib import Path

from scripts import summarize_alpha_search_chain as summary


def write_json(path: Path, payload: object) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload), encoding="utf-8")


class AlphaSearchChainSummaryTest(unittest.TestCase):
    def make_artifact(
        self,
        root: Path,
        run_id: str,
        *,
        best_reward: float,
        candidates: int,
        passed: int,
        factor: str,
        handoff_status: str,
        action: str,
        chain_reason: str,
    ) -> Path:
        artifact = root / f"factor-walk-forward-v2-{run_id}"
        inner = artifact / "factor-walk-forward-v2"
        target = "full_depth_settlement_executable_pnl"
        write_json(
            inner / "alpha-search" / target / "search-feedback.json",
            {
                "target": target,
                "candidate_count": candidates,
                "passed_count": passed,
                "best_reward": best_reward,
            },
        )
        write_json(
            inner / "alpha-search" / target / "mcts-expansion-plan.json",
            {"selected_nodes": [{"factor_name": factor}]},
        )
        write_json(
            inner / "autofactor-strategy-handoff.json",
            {
                "status": handoff_status,
                "recommended_action": action,
                "qualified_strategies": [{}] if handoff_status == "ready" else [],
            },
        )
        write_json(inner / "autofactor-strategy-promotion.json", {"decision": handoff_status})
        write_json(
            artifact / "alpha-search-chain" / "chain-decision.json",
            {
                "current_run_id": run_id,
                "reason": chain_reason,
                "should_dispatch": chain_reason == "continue",
                "next_remaining": 0,
                "current_best_reward": best_reward,
            },
        )
        return artifact

    def test_build_summary_selects_best_run_and_counts_handoffs(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            first = self.make_artifact(
                root,
                "11111111",
                best_reward=1.5,
                candidates=10,
                passed=3,
                factor="base_edge",
                handoff_status="blocked",
                action="do_not_promote",
                chain_reason="continue",
            )
            second = self.make_artifact(
                root,
                "22222222",
                best_reward=2.25,
                candidates=12,
                passed=5,
                factor="mcts_edge",
                handoff_status="ready",
                action="create_dry_run_handoff",
                chain_reason="ready_handoff",
            )

            result = summary.build_summary([first, second])

            self.assertEqual(result["run_count"], 2)
            self.assertEqual(result["best_run_id"], "22222222")
            self.assertEqual(result["best_reward"], 2.25)
            self.assertEqual(result["best_selected_factor"], "mcts_edge")
            self.assertEqual(result["ready_handoff_count"], 1)
            self.assertEqual(result["blocked_handoff_count"], 1)
            self.assertEqual(result["runs"][0]["candidate_count"], 10)
            self.assertEqual(result["runs"][1]["recommended_action"], "create_dry_run_handoff")

    def test_main_writes_json_and_markdown(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            artifact = self.make_artifact(
                root,
                "33333333",
                best_reward=3.0,
                candidates=20,
                passed=8,
                factor="mcts_branch",
                handoff_status="blocked",
                action="do_not_promote",
                chain_reason="chain_next_run_false",
            )
            output_json = root / "summary.json"
            output_md = root / "summary.md"

            summary.main_args = None
            import sys

            old_argv = sys.argv
            try:
                sys.argv = [
                    "summarize_alpha_search_chain.py",
                    str(artifact),
                    "--output-json",
                    str(output_json),
                    "--output-md",
                    str(output_md),
                ]
                summary.main()
            finally:
                sys.argv = old_argv

            data = json.loads(output_json.read_text(encoding="utf-8"))
            markdown = output_md.read_text(encoding="utf-8")
            self.assertEqual(data["best_run_id"], "33333333")
            self.assertIn("Alpha Search Chain Summary", markdown)
            self.assertIn("mcts_branch", markdown)


if __name__ == "__main__":
    unittest.main()
