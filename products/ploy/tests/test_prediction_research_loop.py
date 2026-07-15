import hashlib
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import prediction_research_loop as loop


TARGET = "full_depth_settlement_executable_pnl"


def write_json(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload), encoding="utf-8")


def mission(
    snapshot_hash: str,
    *,
    symbol: str = "BTC",
    max_candidates: int = 2,
    max_llm_calls: int = 2,
    max_seconds: int = 60,
) -> dict:
    payload = {
        "schema_version": "prediction_research_mission.v1",
        "mission_id": f"polymarket-{symbol.lower()}-5m-loop-v1",
        "lane": "prediction_market",
        "objective": f"Predict {symbol} five-minute settlement probability.",
        "hypothesis_scope": "Registered probability components may be complementary.",
        "mutable_scope": ["probability_blend_weights"],
        "data_snapshot_id": snapshot_hash,
        "target": TARGET,
        "symbols": [symbol],
        "horizon": "5m",
        "prompt_snapshot_id": "pending",
        "search_policy_snapshot_id": loop.current_policy_snapshot_id(),
        "search_budget": {
            "max_candidates": max_candidates,
            "max_llm_calls": max_llm_calls,
            "max_seconds": max_seconds,
        },
    }
    payload["prompt_snapshot_id"] = loop.research_brief_snapshot_id(payload)
    return payload


def snapshot(root: Path, *, symbol: str = "BTC", immutable: bool = True) -> tuple[Path, str]:
    snapshot_hash = hashlib.sha256(f"{symbol}-legacy-snapshot".encode()).hexdigest()
    snapshot_contract_hash = (
        "sha256:" + hashlib.sha256(f"{symbol}-snapshot-contract".encode()).hexdigest()
    )
    snapshot_dir = root / f"snapshot-{symbol.lower()}"
    artifacts = {
        "observations_json": "observations.json",
        "deribit_snapshots_json": "deribit.json",
        "pm_book_snapshots_json": "pm-books.json",
    }
    for name in artifacts.values():
        write_json(snapshot_dir / name, [])
    write_json(
        snapshot_dir / "manifest.json",
        {
            "schema_version": "research_snapshot_v1",
            "snapshot_hash": snapshot_hash,
            "snapshot_contract_hash": snapshot_contract_hash,
            "immutable_input": immutable,
            "symbols": [f"{symbol}USDT"],
            "start": "2026-07-01T00:00:00Z",
            "end": "2026-07-02T00:00:00Z",
            "lob_sample_secs": 30,
            "pm_book_sample_secs": 30,
            "observation_sample_secs": 30,
            "max_quote_age_secs": 30,
            "stake_usd": 15.0,
            "artifacts": artifacts,
        },
    )
    return snapshot_dir, snapshot_contract_hash


class FakeClient:
    def __init__(self, names: list[str]) -> None:
        self.names = names
        self.calls = 0

    def propose(self, _prompt: str) -> dict:
        name = self.names[min(self.calls, len(self.names) - 1)]
        self.calls += 1
        return {
            "mutations": [],
            "probability_blends": [
                {
                    "name": name,
                    "hypothesis": f"{name} improves held-out calibration and executable PnL.",
                    "market_midpoint_weight": 1.0,
                    "chainlink_digital_weight": 0.0,
                    "distance_lob_vol_weight": 1.0,
                    "event_surface_weight": 1.0,
                    "existing_model_weight": 1.0,
                }
            ],
        }


class FakeEvaluator:
    def __init__(
        self,
        verdicts: list[str],
        *,
        fail_first_evaluation: bool = False,
        model_override: object | None = None,
    ) -> None:
        self.verdicts = verdicts
        self.fail_first_evaluation = fail_first_evaluation
        self.model_override = model_override
        self.commands: list[list[str]] = []
        self.evaluations = 0

    def __call__(self, command: list[str], _cwd: Path, _timeout: float):
        self.commands.append(command)
        output_root = Path(command[command.index("--alpha-search-output-dir") + 1])
        alpha_root = output_root / TARGET
        write_json(alpha_root / "search-feedback.json", {"candidate_count": 1})
        write_json(alpha_root / "mcts-expansion-plan.json", {"selected_nodes": []})

        if "--alpha-search-llm-prior-json" in command:
            self.evaluations += 1
            if self.fail_first_evaluation and self.evaluations == 1:
                return subprocess.CompletedProcess(command, 9, "", "transient evaluator failure")
            prior_path = Path(command[command.index("--alpha-search-llm-prior-json") + 1])
            prior = json.loads(prior_path.read_text(encoding="utf-8"))
            blend = prior["probability_blends"][0]
            verdict = self.verdicts[min(self.evaluations - 1, len(self.verdicts) - 1)]
            write_json(
                alpha_root / "prediction-research-feedback.json",
                {
                    "schema_version": "prediction_research_feedback.v1",
                    "mission_id": prior["mission_id"],
                    "target": prior["target"],
                    "symbols": prior["symbols"],
                    "horizon": prior["horizon"],
                    "data_snapshot_id": prior["data_snapshot_id"],
                    "prompt_snapshot_id": prior["prompt_snapshot_id"],
                    "search_policy_snapshot_id": prior["search_policy_snapshot_id"],
                    "candidates": [
                        {
                            "model": (
                                self.model_override
                                if self.model_override is not None
                                else f"q_llm_{blend['name']}"
                            ),
                            "hypothesis": blend["hypothesis"],
                            "probability_blend": blend,
                            "verdict": verdict,
                            "reason_codes": [] if verdict == "keep" else ["brier_gate_failed"],
                            "metrics": {},
                        }
                    ],
                },
            )
        return subprocess.CompletedProcess(command, 0, "report", "")


class FakeClock:
    def __init__(self) -> None:
        self.now = 0.0

    def __call__(self) -> float:
        return self.now


class PredictionResearchLoopTests(unittest.TestCase):
    def test_runs_baseline_then_evaluation_and_stops_on_keep(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"
            evaluator = FakeEvaluator(["keep"])
            client = FakeClient(["blend_a"])

            state = loop.run_prediction_research_loop(
                mission(snapshot_hash),
                snapshot_dir,
                output_dir,
                client=client,
                command_runner=evaluator,
            )

            self.assertEqual(state["status"], "completed")
            self.assertEqual(state["stop_reason"], "deterministic_keep")
            self.assertEqual(state["candidates_used"], 1)
            self.assertEqual(state["llm_calls_used"], 1)
            self.assertEqual(client.calls, 1)
            self.assertEqual(len(evaluator.commands), 2)
            self.assertNotIn("--alpha-search-llm-prior-json", evaluator.commands[0])
            self.assertIn("--alpha-search-llm-prior-json", evaluator.commands[1])
            for command in evaluator.commands:
                self.assertEqual(
                    command[command.index("--event-window-secs") + 1], "300"
                )
            self.assertEqual(
                evaluator.commands[0][evaluator.commands[0].index("--symbols") + 1],
                "BTCUSDT",
            )
            evidence = list((output_dir / "iterations").rglob("evidence-*.json"))
            self.assertGreaterEqual(len(evidence), 3)
            for path in evidence:
                digest = hashlib.sha256(path.read_bytes()).hexdigest()
                self.assertEqual(path.stem.removeprefix("evidence-"), digest)
            prompts = list((output_dir / "iterations").rglob("prompt-*.txt"))
            self.assertEqual(len(prompts), 1)
            self.assertEqual(
                prompts[0].stem.removeprefix("prompt-"),
                hashlib.sha256(prompts[0].read_bytes()).hexdigest(),
            )

    def test_resumes_pending_evaluation_without_spending_another_llm_call(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"
            evaluator = FakeEvaluator(["keep"], fail_first_evaluation=True)
            client = FakeClient(["blend_a"])

            paused = loop.run_prediction_research_loop(
                mission(snapshot_hash, max_candidates=1, max_llm_calls=1),
                snapshot_dir,
                output_dir,
                client=client,
                command_runner=evaluator,
            )
            self.assertEqual(paused["status"], "paused")
            self.assertEqual(paused["stop_reason"], "evaluator_failed")
            self.assertEqual(paused["phase"], "evaluate")
            self.assertEqual(client.calls, 1)

            completed = loop.run_prediction_research_loop(
                mission(snapshot_hash, max_candidates=1, max_llm_calls=1),
                snapshot_dir,
                output_dir,
                client=client,
                command_runner=evaluator,
            )
            self.assertEqual(completed["status"], "completed")
            self.assertEqual(completed["stop_reason"], "deterministic_keep")
            self.assertEqual(client.calls, 1)
            self.assertEqual(completed["llm_calls_used"], 1)

    def test_stops_at_candidate_budget_after_discard(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            evaluator = FakeEvaluator(["discard"])
            client = FakeClient(["blend_a", "blend_b"])

            state = loop.run_prediction_research_loop(
                mission(snapshot_hash, max_candidates=1, max_llm_calls=4),
                snapshot_dir,
                root / "btc-loop",
                client=client,
                command_runner=evaluator,
            )

            self.assertEqual(state["status"], "stopped")
            self.assertEqual(state["stop_reason"], "budget_max_candidates")
            self.assertEqual(state["candidates_used"], 1)
            self.assertEqual(client.calls, 1)

    def test_stops_at_llm_call_budget_after_discard(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            client = FakeClient(["blend_a", "blend_b"])

            state = loop.run_prediction_research_loop(
                mission(snapshot_hash, max_candidates=4, max_llm_calls=1),
                snapshot_dir,
                root / "btc-loop",
                client=client,
                command_runner=FakeEvaluator(["discard"]),
            )

            self.assertEqual(state["status"], "stopped")
            self.assertEqual(state["stop_reason"], "budget_max_llm_calls")
            self.assertEqual(state["llm_calls_used"], 1)
            self.assertEqual(client.calls, 1)

    def test_rejects_factor_mutation_authority_in_prediction_loop(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            payload = mission(snapshot_hash)
            payload["mutable_scope"] = [
                "factor_formula",
                "probability_blend_weights",
            ]

            with self.assertRaisesRegex(loop.LoopValidationError, "may mutate only"):
                loop.run_prediction_research_loop(
                    payload,
                    snapshot_dir,
                    root / "btc-loop",
                    client=FakeClient(["blend_a"]),
                    command_runner=FakeEvaluator(["keep"]),
                )

    def test_rejects_keep_for_model_not_in_pending_prior(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"
            payload = mission(snapshot_hash)

            state = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=FakeClient(["blend_a"]),
                command_runner=FakeEvaluator(
                    ["keep"], model_override="q_llm_not_in_prior"
                ),
            )

            self.assertEqual(state["status"], "failed")
            self.assertEqual(state["stop_reason"], "prediction_feedback_invalid")
            self.assertIsNotNone(state["last_failure"]["evidence"])
            self.assertIsNotNone(state["last_failure"]["artifact_dir"])
            readback = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=FakeClient(["must_not_run"]),
                command_runner=FakeEvaluator(["keep"]),
            )
            self.assertEqual(readback["stop_reason"], "prediction_feedback_invalid")

    def test_malformed_feedback_model_becomes_explicit_failure(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)

            state = loop.run_prediction_research_loop(
                mission(snapshot_hash),
                snapshot_dir,
                root / "btc-loop",
                client=FakeClient(["blend_a"]),
                command_runner=FakeEvaluator(["keep"], model_override={"bad": "model"}),
            )

            self.assertEqual(state["status"], "failed")
            self.assertEqual(state["stop_reason"], "prediction_feedback_invalid")

    def test_feedback_failure_state_requires_evaluator_evidence_bindings(self) -> None:
        for missing_field in ("evidence", "evidence_sha256", "artifact_dir"):
            with self.subTest(missing_field=missing_field), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                snapshot_dir, snapshot_hash = snapshot(root)
                output_dir = root / "btc-loop"
                payload = mission(snapshot_hash)
                loop.run_prediction_research_loop(
                    payload,
                    snapshot_dir,
                    output_dir,
                    client=FakeClient(["blend_a"]),
                    command_runner=FakeEvaluator(
                        ["keep"], model_override="q_llm_not_in_prior"
                    ),
                )
                state_path = output_dir / "state.json"
                state = json.loads(state_path.read_text(encoding="utf-8"))
                state["last_failure"].pop(missing_field)
                state_path.write_text(
                    json.dumps(state, indent=2, sort_keys=True) + "\n",
                    encoding="utf-8",
                )

                with self.assertRaisesRegex(
                    loop.LoopValidationError, "required evidence"
                ):
                    loop.run_prediction_research_loop(
                        payload,
                        snapshot_dir,
                        output_dir,
                        client=FakeClient(["must_not_run"]),
                        command_runner=FakeEvaluator(["keep"]),
                    )

    def test_terminal_state_requires_done_phase(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"
            payload = mission(snapshot_hash)
            loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=FakeClient(["blend_a"]),
                command_runner=FakeEvaluator(
                    ["keep"], model_override="q_llm_not_in_prior"
                ),
            )
            state_path = output_dir / "state.json"
            state = json.loads(state_path.read_text(encoding="utf-8"))
            state["phase"] = "evaluate"
            state_path.write_text(
                json.dumps(state, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )

            with self.assertRaisesRegex(loop.LoopValidationError, "done phase"):
                loop.run_prediction_research_loop(
                    payload,
                    snapshot_dir,
                    output_dir,
                    client=FakeClient(["must_not_run"]),
                    command_runner=FakeEvaluator(["keep"]),
                )

    def test_feedback_failure_fact_remains_reproducible_on_readback(self) -> None:
        for mutation in ("delete_invalid", "replace_with_valid"):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                snapshot_dir, snapshot_hash = snapshot(root)
                output_dir = root / "btc-loop"
                payload = mission(snapshot_hash)
                state = loop.run_prediction_research_loop(
                    payload,
                    snapshot_dir,
                    output_dir,
                    client=FakeClient(["blend_a"]),
                    command_runner=FakeEvaluator(
                        ["keep"], model_override="q_llm_not_in_prior"
                    ),
                )
                artifact_dir = output_dir / state["last_failure"]["artifact_dir"]
                feedback_path = (
                    artifact_dir
                    / "alpha-search"
                    / TARGET
                    / "prediction-research-feedback.json"
                )
                if mutation == "delete_invalid":
                    feedback_path.unlink()
                else:
                    feedback = json.loads(feedback_path.read_text(encoding="utf-8"))
                    feedback["candidates"][0]["model"] = "q_llm_blend_a"
                    write_json(feedback_path, feedback)

                with self.assertRaisesRegex(
                    loop.LoopValidationError, "artifact snapshot changed"
                ):
                    loop.run_prediction_research_loop(
                        payload,
                        snapshot_dir,
                        output_dir,
                        client=FakeClient(["must_not_run"]),
                        command_runner=FakeEvaluator(["keep"]),
                    )

        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"
            payload = mission(snapshot_hash)

            class MissingFeedbackEvaluator:
                def __call__(self, command: list[str], _cwd: Path, _timeout: float):
                    output_root = Path(
                        command[command.index("--alpha-search-output-dir") + 1]
                    )
                    alpha_root = output_root / TARGET
                    write_json(alpha_root / "search-feedback.json", {"candidate_count": 1})
                    write_json(alpha_root / "mcts-expansion-plan.json", {"selected_nodes": []})
                    return subprocess.CompletedProcess(command, 0, "report", "")

            state = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=FakeClient(["blend_a"]),
                command_runner=MissingFeedbackEvaluator(),
            )
            self.assertEqual(state["stop_reason"], "prediction_feedback_missing")
            artifact_dir = output_dir / state["last_failure"]["artifact_dir"]
            write_json(
                artifact_dir
                / "alpha-search"
                / TARGET
                / "prediction-research-feedback.json",
                {},
            )
            with self.assertRaisesRegex(
                loop.LoopValidationError, "artifact snapshot changed"
            ):
                loop.run_prediction_research_loop(
                    payload,
                    snapshot_dir,
                    output_dir,
                    client=FakeClient(["must_not_run"]),
                    command_runner=MissingFeedbackEvaluator(),
                )

    def test_rejects_mutable_or_mismatched_snapshot_provenance(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root, immutable=False)
            with self.assertRaisesRegex(loop.LoopValidationError, "immutable_input"):
                loop.run_prediction_research_loop(
                    mission(snapshot_hash),
                    snapshot_dir,
                    root / "loop-a",
                    client=FakeClient(["a"]),
                    command_runner=FakeEvaluator(["keep"]),
                )

            immutable_dir, immutable_hash = snapshot(root / "second")
            with self.assertRaisesRegex(loop.LoopValidationError, "data_snapshot_id"):
                loop.run_prediction_research_loop(
                    mission("different-hash"),
                    immutable_dir,
                    root / "loop-b",
                    client=FakeClient(["a"]),
                    command_runner=FakeEvaluator(["keep"]),
                )
            self.assertNotEqual(immutable_hash, "different-hash")

    def test_refuses_to_resume_btc_state_with_sol_mission(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            btc_snapshot, btc_hash = snapshot(root, symbol="BTC")
            output_dir = root / "shared-loop"
            loop.run_prediction_research_loop(
                mission(btc_hash, symbol="BTC", max_candidates=1),
                btc_snapshot,
                output_dir,
                client=FakeClient(["btc"]),
                command_runner=FakeEvaluator(["discard"]),
            )
            sol_snapshot, sol_hash = snapshot(root, symbol="SOL")

            with self.assertRaisesRegex(loop.LoopValidationError, "different mission"):
                loop.run_prediction_research_loop(
                    mission(sol_hash, symbol="SOL"),
                    sol_snapshot,
                    output_dir,
                    client=FakeClient(["sol"]),
                    command_runner=FakeEvaluator(["keep"]),
                )

    def test_rejects_misnamed_content_addressed_feedback(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            artifact_dir = Path(tmp)
            write_json(
                artifact_dir
                / "alpha-search"
                / TARGET
                / "prediction-research-feedback-not-the-hash.json",
                {"schema_version": "prediction_research_feedback.v1"},
            )

            with self.assertRaisesRegex(loop.LoopValidationError, "content hash"):
                loop._find_feedback(artifact_dir)

    def test_snapshot_artifact_change_invalidates_resume(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"
            loop.run_prediction_research_loop(
                mission(snapshot_hash),
                snapshot_dir,
                output_dir,
                client=FakeClient(["blend_a"]),
                command_runner=FakeEvaluator(["keep"], fail_first_evaluation=True),
            )
            write_json(snapshot_dir / "observations.json", [{"changed": True}])

            with self.assertRaisesRegex(loop.LoopValidationError, "immutable snapshot"):
                loop.run_prediction_research_loop(
                    mission(snapshot_hash),
                    snapshot_dir,
                    output_dir,
                    client=FakeClient(["blend_a"]),
                    command_runner=FakeEvaluator(["keep"]),
                )

    def test_stops_before_llm_when_baseline_consumes_seconds_budget(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            clock = FakeClock()
            evaluator = FakeEvaluator(["keep"])

            def slow_evaluator(command: list[str], cwd: Path, timeout: float):
                result = evaluator(command, cwd, timeout)
                clock.now += 2.0
                return result

            client = FakeClient(["blend_a"])
            state = loop.run_prediction_research_loop(
                mission(snapshot_hash, max_seconds=1),
                snapshot_dir,
                root / "btc-loop",
                client=client,
                command_runner=slow_evaluator,
                clock=clock,
            )

            self.assertEqual(state["status"], "stopped")
            self.assertEqual(state["stop_reason"], "budget_max_seconds")
            self.assertEqual(client.calls, 0)

    def test_discards_late_llm_response_and_records_attempt_evidence(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            clock = FakeClock()
            client = FakeClient(["blend_a"])

            original_propose = client.propose

            def slow_propose(prompt: str) -> dict:
                clock.now += 2.0
                return original_propose(prompt)

            client.propose = slow_propose  # type: ignore[method-assign]
            state = loop.run_prediction_research_loop(
                mission(snapshot_hash, max_seconds=1),
                snapshot_dir,
                root / "btc-loop",
                client=client,
                command_runner=FakeEvaluator(["keep"]),
                clock=clock,
            )

            self.assertEqual(state["status"], "stopped")
            self.assertEqual(state["stop_reason"], "budget_max_seconds")
            self.assertEqual(state["candidates_used"], 0)
            kinds = {
                json.loads(path.read_text(encoding="utf-8"))["kind"]
                for path in (root / "btc-loop" / "iterations").rglob("evidence-*.json")
            }
            self.assertIn("llm_call_started", kinds)
            self.assertIn("llm_call_returned", kinds)

    def test_records_failed_llm_call_for_resume(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)

            class FailingClient:
                def propose(self, _prompt: str) -> dict:
                    raise RuntimeError("provider unavailable")

            state = loop.run_prediction_research_loop(
                mission(snapshot_hash),
                snapshot_dir,
                root / "btc-loop",
                client=FailingClient(),
                command_runner=FakeEvaluator(["keep"]),
            )

            self.assertEqual(state["status"], "paused")
            self.assertEqual(state["stop_reason"], "llm_failed")
            kinds = {
                json.loads(path.read_text(encoding="utf-8"))["kind"]
                for path in (root / "btc-loop" / "iterations").rglob("evidence-*.json")
            }
            self.assertIn("llm_call_started", kinds)
            self.assertIn("llm_call_failed", kinds)

    def test_exclusive_lock_rejects_a_second_loop_process(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"
            with loop._exclusive_output_lock(output_dir):
                with self.assertRaisesRegex(loop.LoopValidationError, "already locked"):
                    loop.run_prediction_research_loop(
                        mission(snapshot_hash),
                        snapshot_dir,
                        output_dir,
                        client=FakeClient(["blend_a"]),
                        command_runner=FakeEvaluator(["keep"]),
                    )

    def test_terminal_readback_rejects_missing_feedback_and_evaluator_evidence(self) -> None:
        for missing_field in ("feedback_path", "evaluator_evidence"):
            with self.subTest(missing_field=missing_field), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                snapshot_dir, snapshot_hash = snapshot(root)
                output_dir = root / "btc-loop"
                state = loop.run_prediction_research_loop(
                    mission(snapshot_hash),
                    snapshot_dir,
                    output_dir,
                    client=FakeClient(["blend_a"]),
                    command_runner=FakeEvaluator(["keep"]),
                )
                (output_dir / state["iterations"][0][missing_field]).unlink()

                with self.assertRaisesRegex(loop.LoopValidationError, "missing"):
                    loop.run_prediction_research_loop(
                        mission(snapshot_hash),
                        snapshot_dir,
                        output_dir,
                        client=FakeClient(["unused"]),
                        command_runner=FakeEvaluator(["keep"]),
                    )

    def test_policy_id_changes_when_evaluator_source_changes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            for relative in loop.POLICY_ENTRYPOINT_PATHS:
                path = root / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(relative, encoding="utf-8")
            factors = root / "crates/ploy-research/src/factors.rs"
            factors.parent.mkdir(parents=True, exist_ok=True)
            factors.write_text("version one", encoding="utf-8")
            with mock.patch.object(loop, "PLOY_ROOT", root):
                before = loop.current_policy_snapshot_id()
                factors.write_text("version two", encoding="utf-8")
                after = loop.current_policy_snapshot_id()
            self.assertNotEqual(before, after)

    def test_replays_a_persisted_llm_response_after_crash_without_a_second_call(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"
            client = FakeClient(["blend_a"])
            evaluator = FakeEvaluator(["keep"])
            original_propose = loop.propose_candidates

            def crash_after_response(*args, **kwargs):
                original_propose(*args, **kwargs)
                raise KeyboardInterrupt("simulated process crash")

            with mock.patch.object(
                loop, "propose_candidates", side_effect=crash_after_response
            ):
                with self.assertRaises(KeyboardInterrupt):
                    loop.run_prediction_research_loop(
                        mission(snapshot_hash, max_candidates=1, max_llm_calls=1),
                        snapshot_dir,
                        output_dir,
                        client=client,
                        command_runner=evaluator,
                    )

            crashed_state = json.loads(
                (output_dir / "state.json").read_text(encoding="utf-8")
            )
            self.assertIsNotNone(crashed_state["inflight_llm"]["response_path"])
            self.assertEqual(client.calls, 1)

            completed = loop.run_prediction_research_loop(
                mission(snapshot_hash, max_candidates=1, max_llm_calls=1),
                snapshot_dir,
                output_dir,
                client=client,
                command_runner=evaluator,
            )
            self.assertEqual(completed["status"], "completed")
            self.assertEqual(client.calls, 1)
            self.assertIsNone(completed["inflight_llm"])
            self.assertIn("llm_attempt", completed["iterations"][0])

    def test_resume_rejects_frontier_and_iteration_counter_rebinding(self) -> None:
        for field in ("last_artifact_dir", "next_iteration"):
            with self.subTest(field=field), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                snapshot_dir, snapshot_hash = snapshot(root)
                output_dir = root / "btc-loop"
                loop.run_prediction_research_loop(
                    mission(snapshot_hash, max_candidates=1),
                    snapshot_dir,
                    output_dir,
                    client=FakeClient(["blend_a"]),
                    command_runner=FakeEvaluator(["discard"]),
                )
                state_path = output_dir / "state.json"
                state = json.loads(state_path.read_text(encoding="utf-8"))
                if field == "last_artifact_dir":
                    state[field] = state["baseline_artifact_dir"]
                else:
                    state[field] += 1
                state_path.write_text(
                    json.dumps(state, indent=2, sort_keys=True) + "\n",
                    encoding="utf-8",
                )

                with self.assertRaisesRegex(
                    loop.LoopValidationError, "frontier|next_iteration"
                ):
                    loop.run_prediction_research_loop(
                        mission(snapshot_hash, max_candidates=1),
                        snapshot_dir,
                        output_dir,
                        client=FakeClient(["unused"]),
                        command_runner=FakeEvaluator(["keep"]),
                    )

    def test_empty_proposal_terminal_state_is_re_readable(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"

            class EmptyClient:
                def propose(self, _prompt: str) -> dict:
                    return {"mutations": [], "probability_blends": []}

            payload = mission(snapshot_hash, max_candidates=1, max_llm_calls=1)
            failed = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=EmptyClient(),
                command_runner=FakeEvaluator(["keep"]),
            )
            self.assertEqual(failed["status"], "failed")
            self.assertEqual(failed["stop_reason"], "empty_proposal")
            self.assertIsNone(failed["inflight_llm"])
            self.assertEqual(
                failed["archived_llm_attempts"][0]["outcome"], "empty_proposal"
            )

            readback = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=FakeClient(["must_not_run"]),
                command_runner=FakeEvaluator(["keep"]),
            )
            self.assertEqual(readback["stop_reason"], "empty_proposal")

    def test_invalid_final_response_terminal_state_is_re_readable(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"

            class InvalidClient:
                def propose(self, _prompt: str) -> dict:
                    return {"mutations": "not-a-list", "probability_blends": []}

            payload = mission(snapshot_hash, max_candidates=1, max_llm_calls=1)
            stopped = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=InvalidClient(),
                command_runner=FakeEvaluator(["keep"]),
            )
            self.assertEqual(stopped["status"], "stopped")
            self.assertEqual(stopped["stop_reason"], "budget_max_llm_calls")
            self.assertIsNone(stopped["inflight_llm"])

            readback = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=FakeClient(["must_not_run"]),
                command_runner=FakeEvaluator(["keep"]),
            )
            self.assertEqual(readback["stop_reason"], "budget_max_llm_calls")

    def test_schema_retry_lineage_is_complete_and_fail_closed(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"

            class InvalidThenValidClient:
                def __init__(self) -> None:
                    self.calls = 0

                def propose(self, prompt: str) -> dict:
                    self.calls += 1
                    if self.calls == 1:
                        return {"mutations": "not-a-list", "probability_blends": []}
                    return FakeClient(["blend_a"]).propose(prompt)

            payload = mission(snapshot_hash, max_candidates=1, max_llm_calls=2)
            completed = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=InvalidThenValidClient(),
                command_runner=FakeEvaluator(["keep"]),
            )
            self.assertEqual(completed["status"], "completed")
            self.assertEqual(completed["iterations"][0]["llm_call_lineage"], [1, 2])
            first_attempt = completed["archived_llm_attempts"][0]["attempt"]
            (output_dir / first_attempt["prompt_path"]).unlink()

            with self.assertRaisesRegex(loop.LoopValidationError, "missing"):
                loop.run_prediction_research_loop(
                    payload,
                    snapshot_dir,
                    output_dir,
                    client=FakeClient(["must_not_run"]),
                    command_runner=FakeEvaluator(["keep"]),
                )

    def test_recovers_response_written_before_state_reference_without_new_call(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"
            payload = mission(snapshot_hash, max_candidates=1, max_llm_calls=1)
            original_write = loop._write_content_addressed
            crashed = False

            def crash_after_response_write(directory: Path, prefix: str, value: object):
                nonlocal crashed
                result = original_write(directory, prefix, value)
                if prefix == "response" and not crashed:
                    crashed = True
                    raise KeyboardInterrupt("crash after durable response write")
                return result

            with mock.patch.object(
                loop, "_write_content_addressed", side_effect=crash_after_response_write
            ):
                with self.assertRaises(KeyboardInterrupt):
                    loop.run_prediction_research_loop(
                        payload,
                        snapshot_dir,
                        output_dir,
                        client=FakeClient(["blend_a"]),
                        command_runner=FakeEvaluator(["keep"]),
                    )

            crashed_state = json.loads(
                (output_dir / "state.json").read_text(encoding="utf-8")
            )
            self.assertIsNone(crashed_state["inflight_llm"]["response_path"])
            with mock.patch.object(
                loop,
                "client_from_env",
                side_effect=AssertionError("persisted response must be replayed"),
            ):
                completed = loop.run_prediction_research_loop(
                    payload,
                    snapshot_dir,
                    output_dir,
                    client=None,
                    command_runner=FakeEvaluator(["keep"]),
                )
            self.assertEqual(completed["status"], "completed")
            self.assertEqual(completed["llm_calls_used"], 1)

    def test_replays_invalid_response_then_uses_remaining_retry_budget(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"
            payload = mission(snapshot_hash, max_candidates=1, max_llm_calls=2)
            original_write_state = loop._write_state
            crashed = False

            class InvalidThenValidClient:
                def __init__(self) -> None:
                    self.calls = 0

                def propose(self, prompt: str) -> dict:
                    self.calls += 1
                    if self.calls == 1:
                        return {"mutations": "not-a-list", "probability_blends": []}
                    return FakeClient(["blend_a"]).propose(prompt)

            client = InvalidThenValidClient()

            def crash_after_response_state(path: Path, state: dict) -> None:
                nonlocal crashed
                original_write_state(path, state)
                inflight = state.get("inflight_llm")
                if (
                    isinstance(inflight, dict)
                    and inflight.get("response_path") is not None
                    and not crashed
                ):
                    crashed = True
                    raise KeyboardInterrupt("crash before invalid response validation")

            with mock.patch.object(loop, "_write_state", side_effect=crash_after_response_state):
                with self.assertRaises(KeyboardInterrupt):
                    loop.run_prediction_research_loop(
                        payload,
                        snapshot_dir,
                        output_dir,
                        client=client,
                        command_runner=FakeEvaluator(["keep"]),
                    )

            completed = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=client,
                command_runner=FakeEvaluator(["keep"]),
            )
            self.assertEqual(completed["status"], "completed")
            self.assertEqual(client.calls, 2)
            self.assertEqual(completed["iterations"][0]["llm_call_lineage"], [1, 2])

    def test_missing_response_after_crash_remains_in_call_ledger(self) -> None:
        for max_calls, expected_status in ((1, "stopped"), (2, "completed")):
            with self.subTest(max_calls=max_calls), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                snapshot_dir, snapshot_hash = snapshot(root)
                output_dir = root / "btc-loop"
                payload = mission(
                    snapshot_hash, max_candidates=1, max_llm_calls=max_calls
                )

                class CrashingClient:
                    def propose(self, _prompt: str) -> dict:
                        raise KeyboardInterrupt("provider call interrupted")

                with self.assertRaises(KeyboardInterrupt):
                    loop.run_prediction_research_loop(
                        payload,
                        snapshot_dir,
                        output_dir,
                        client=CrashingClient(),
                        command_runner=FakeEvaluator(["keep"]),
                    )

                resumed = loop.run_prediction_research_loop(
                    payload,
                    snapshot_dir,
                    output_dir,
                    client=FakeClient(["blend_a"]),
                    command_runner=FakeEvaluator(["keep"]),
                )
                self.assertEqual(resumed["status"], expected_status)
                self.assertEqual(
                    resumed["archived_llm_attempts"][0]["outcome"],
                    "response_missing_after_crash",
                )
                self.assertEqual(resumed["llm_calls_used"], max_calls)
                readback = loop.run_prediction_research_loop(
                    payload,
                    snapshot_dir,
                    output_dir,
                    client=FakeClient(["must_not_run"]),
                    command_runner=FakeEvaluator(["keep"]),
                )
                self.assertEqual(readback["status"], expected_status)

    def test_expired_response_resume_archives_attempt_before_terminal_state(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"
            payload = mission(
                snapshot_hash, max_candidates=1, max_llm_calls=1, max_seconds=1
            )
            clock = FakeClock()
            original_write_state = loop._write_state
            crashed = False

            def crash_after_response_state(path: Path, state: dict) -> None:
                nonlocal crashed
                original_write_state(path, state)
                inflight = state.get("inflight_llm")
                if (
                    isinstance(inflight, dict)
                    and inflight.get("response_path") is not None
                    and not crashed
                ):
                    crashed = True
                    raise KeyboardInterrupt("crash before post-response budget check")

            class SlowClient(FakeClient):
                def propose(self, prompt: str) -> dict:
                    clock.now = 2.0
                    return super().propose(prompt)

            with mock.patch.object(loop, "_write_state", side_effect=crash_after_response_state):
                with self.assertRaises(KeyboardInterrupt):
                    loop.run_prediction_research_loop(
                        payload,
                        snapshot_dir,
                        output_dir,
                        client=SlowClient(["blend_a"]),
                        command_runner=FakeEvaluator(["keep"]),
                        clock=clock,
                    )

            stopped = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=FakeClient(["must_not_run"]),
                command_runner=FakeEvaluator(["keep"]),
                clock=clock,
            )
            self.assertEqual(stopped["status"], "stopped")
            self.assertEqual(stopped["stop_reason"], "budget_max_seconds")
            self.assertIsNone(stopped["inflight_llm"])
            self.assertEqual(
                stopped["archived_llm_attempts"][0]["outcome"],
                "budget_expired_after_response",
            )
            readback = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=FakeClient(["must_not_run"]),
                command_runner=FakeEvaluator(["keep"]),
                clock=clock,
            )
            self.assertEqual(readback["stop_reason"], "budget_max_seconds")

    def test_terminal_artifact_failure_archives_inflight_attempt(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"
            payload = mission(snapshot_hash, max_candidates=1, max_llm_calls=1)
            original_propose = loop.propose_candidates

            def crash_after_response(*args, **kwargs):
                original_propose(*args, **kwargs)
                raise KeyboardInterrupt("crash before proposal persistence")

            with mock.patch.object(
                loop, "propose_candidates", side_effect=crash_after_response
            ):
                with self.assertRaises(KeyboardInterrupt):
                    loop.run_prediction_research_loop(
                        payload,
                        snapshot_dir,
                        output_dir,
                        client=FakeClient(["blend_a"]),
                        command_runner=FakeEvaluator(["keep"]),
                    )

            state = json.loads((output_dir / "state.json").read_text(encoding="utf-8"))
            artifact_dir = output_dir / state["baseline_artifact_dir"]
            (artifact_dir / "alpha-search" / TARGET / "search-feedback.json").write_text(
                "not-json", encoding="utf-8"
            )
            failed = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=FakeClient(["must_not_run"]),
                command_runner=FakeEvaluator(["keep"]),
            )
            self.assertEqual(failed["status"], "failed")
            self.assertEqual(failed["stop_reason"], "research_artifact_invalid")
            self.assertIsNone(failed["inflight_llm"])
            self.assertEqual(
                failed["archived_llm_attempts"][0]["outcome"],
                "terminal_before_proposal",
            )
            readback = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=FakeClient(["must_not_run"]),
                command_runner=FakeEvaluator(["keep"]),
            )
            self.assertEqual(readback["stop_reason"], "research_artifact_invalid")

    def test_resume_rejects_candidate_budget_counter_rebinding(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"
            payload = mission(snapshot_hash, max_candidates=1, max_llm_calls=1)
            paused = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=FakeClient(["blend_a"]),
                command_runner=FakeEvaluator(["discard"], fail_first_evaluation=True),
            )
            self.assertEqual(paused["phase"], "evaluate")
            self.assertEqual(paused["candidates_used"], 1)
            state_path = output_dir / "state.json"
            state = json.loads(state_path.read_text(encoding="utf-8"))
            state["candidates_used"] = 0
            state_path.write_text(
                json.dumps(state, indent=2, sort_keys=True) + "\n",
                encoding="utf-8",
            )

            with self.assertRaisesRegex(loop.LoopValidationError, "candidate budget"):
                loop.run_prediction_research_loop(
                    payload,
                    snapshot_dir,
                    output_dir,
                    client=FakeClient(["must_not_run"]),
                    command_runner=FakeEvaluator(["keep"]),
                )

    def test_durable_keep_crash_resumes_without_another_iteration(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            snapshot_dir, snapshot_hash = snapshot(root)
            output_dir = root / "btc-loop"
            payload = mission(snapshot_hash, max_candidates=1, max_llm_calls=1)
            original_write_state = loop._write_state
            crashed = False

            def crash_after_durable_keep(path: Path, state: dict) -> None:
                nonlocal crashed
                original_write_state(path, state)
                if (
                    state.get("status") == "completed"
                    and state.get("iterations")
                    and state["iterations"][-1].get("kept_models")
                    and not crashed
                ):
                    crashed = True
                    raise KeyboardInterrupt("crash after durable keep decision")

            with mock.patch.object(loop, "_write_state", side_effect=crash_after_durable_keep):
                with self.assertRaises(KeyboardInterrupt):
                    loop.run_prediction_research_loop(
                        payload,
                        snapshot_dir,
                        output_dir,
                        client=FakeClient(["blend_a"]),
                        command_runner=FakeEvaluator(["keep"]),
                    )

            next_client = FakeClient(["must_not_run"])
            completed = loop.run_prediction_research_loop(
                payload,
                snapshot_dir,
                output_dir,
                client=next_client,
                command_runner=FakeEvaluator(["keep"]),
            )
            self.assertEqual(completed["status"], "completed")
            self.assertEqual(completed["stop_reason"], "deterministic_keep")
            self.assertEqual(len(completed["iterations"]), 1)
            self.assertEqual(completed["llm_calls_used"], 1)
            self.assertEqual(next_client.calls, 0)


if __name__ == "__main__":
    unittest.main()
