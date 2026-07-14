import json
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from scripts import alpha_search_llm_propose as propose
from scripts.alpha_search_closed_loop_agent import ALLOWED_MUTATIONS


def write_json(path: Path, payload: dict) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(payload), encoding="utf-8")


def artifact(
    root: Path,
    *,
    target: str = "full_depth_settlement_executable_pnl",
    selected_nodes: list[dict] | None = None,
    avoided_subtrees: list[dict] | None = None,
) -> Path:
    factor_root = root / "factor-walk-forward-v2"
    alpha_root = factor_root / "alpha-search" / target
    write_json(
        alpha_root / "search-feedback.json",
        {
            "target": target,
            "candidate_count": 4,
            "best_candidate": "auto_settlement_full_depth_settlement_edge",
            "best_reward": 1.25,
        },
    )
    write_json(
        alpha_root / "mcts-expansion-plan.json",
        {
            "target": target,
            "selected_nodes": selected_nodes
            if selected_nodes is not None
            else [
                {
                    "factor_name": "auto_settlement_full_depth_settlement_edge",
                    "selected_dimension": "execution_quality",
                    "proposed_mutation": "add_capacity_gate",
                    "reward": 0.8,
                }
            ],
        },
    )
    write_json(
        alpha_root / "search-space.json",
        {
            "target": target,
            "feature_pool": ["entry_capacity_score", "side_spread"],
        },
    )
    write_json(
        factor_root / "autofactor-strategy-handoff.json",
        {"status": "blocked", "recommended_action": "do_not_promote"},
    )
    write_json(
        factor_root / "autofactor-strategy-promotion.json",
        {"decision": "blocked", "evaluated_factors": []},
    )
    if avoided_subtrees is not None:
        write_json(alpha_root / "avoided-subtrees.json", avoided_subtrees)
    write_json(
        root / "alpha-search-chain" / "chain-decision.json",
        {"current_run_id": "1000000001"},
    )
    return root


class FakeClient:
    """Returns a queued sequence of canned responses, one per call."""

    def __init__(self, responses: list[dict]) -> None:
        self._responses = list(responses)
        self.calls: list[str] = []

    def propose(self, prompt: str) -> dict:
        self.calls.append(prompt)
        return self._responses.pop(0)


class BuildPromptTests(unittest.TestCase):
    def test_prompt_lists_allowed_mutations_from_shared_constant(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            from scripts.alpha_search_closed_loop_agent import DEFAULT_TARGET, load_artifact

            run = load_artifact(path, DEFAULT_TARGET)
            prompt = propose.build_prompt(run)

        payload = json.loads(prompt)
        listed = set(payload["allowed_mutation_types"].split(", "))
        self.assertEqual(listed, ALLOWED_MUTATIONS)

    def test_prompt_includes_weak_dimensions_from_plan(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                selected_nodes=[
                    {
                        "factor_name": "auto_settlement_x",
                        "selected_dimension": "overfit_risk",
                        "proposed_mutation": "remove_component",
                        "reward": -0.2,
                    }
                ],
            )
            from scripts.alpha_search_closed_loop_agent import DEFAULT_TARGET, load_artifact

            run = load_artifact(path, DEFAULT_TARGET)
            prompt = propose.build_prompt(run)

        payload = json.loads(prompt)
        self.assertEqual(len(payload["weak_dimensions"]), 1)
        self.assertEqual(payload["weak_dimensions"][0]["factor_name"], "auto_settlement_x")
        self.assertEqual(payload["weak_dimensions"][0]["selected_dimension"], "overfit_risk")

    def test_prompt_includes_crowded_structural_shapes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            from scripts.alpha_search_closed_loop_agent import DEFAULT_TARGET, load_artifact

            run = load_artifact(path, DEFAULT_TARGET)
            avoided = [
                {"root_gene": "SafeDiv", "count": 4, "action": "penalize", "reason": "x"},
                {"root_gene": "Add", "count": 1, "action": "keep", "reason": "y"},
            ]
            prompt = propose.build_prompt(run, avoided_subtrees=avoided)

        payload = json.loads(prompt)
        shapes = payload["crowded_structural_shapes_within_batch"]
        self.assertEqual(len(shapes), 1)
        self.assertEqual(shapes[0]["root_gene"], "SafeDiv")

    def test_prompt_includes_alpha_zoo_summary(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            from scripts.alpha_search_closed_loop_agent import DEFAULT_TARGET, load_artifact

            run = load_artifact(path, DEFAULT_TARGET)
            zoo = {
                "version": "alpha_zoo_v1",
                "target": "full_depth_settlement_executable_pnl",
                "entries": [{"root_gene": "Mul", "count": 12}],
            }
            prompt = propose.build_prompt(run, alpha_zoo_snapshot=zoo)

        payload = json.loads(prompt)
        entries = payload["crowded_root_genes_across_all_history"]
        self.assertEqual(entries, [{"root_gene": "Mul", "count": 12}])

    def test_prompt_omits_alpha_zoo_summary_when_absent(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            from scripts.alpha_search_closed_loop_agent import DEFAULT_TARGET, load_artifact

            run = load_artifact(path, DEFAULT_TARGET)
            prompt = propose.build_prompt(run)

        payload = json.loads(prompt)
        self.assertEqual(payload["crowded_root_genes_across_all_history"], [])


class ValidateResponseTests(unittest.TestCase):
    def test_accepts_a_well_formed_response(self) -> None:
        response = {
            "mutations": [
                {
                    "base_factor": "auto_settlement_x",
                    "mutation_type": "add_capacity_gate",
                    "feature": "entry_capacity_score",
                }
            ]
        }
        validated = propose.validate_response(response)
        self.assertEqual(len(validated), 1)
        self.assertEqual(validated[0]["base_factor"], "auto_settlement_x")

    def test_rejects_non_dict_response(self) -> None:
        with self.assertRaises(propose.SchemaValidationError):
            propose.validate_response(["not", "a", "dict"])

    def test_rejects_missing_mutations_list(self) -> None:
        with self.assertRaises(propose.SchemaValidationError):
            propose.validate_response({})

    def test_rejects_unknown_mutation_type(self) -> None:
        response = {
            "mutations": [
                {"base_factor": "auto_settlement_x", "mutation_type": "delete_everything"}
            ]
        }
        with self.assertRaises(propose.SchemaValidationError) as ctx:
            propose.validate_response(response)
        self.assertIn("not in the allowed set", str(ctx.exception))

    def test_rejects_missing_required_field(self) -> None:
        response = {"mutations": [{"mutation_type": "add_capacity_gate"}]}
        with self.assertRaises(propose.SchemaValidationError) as ctx:
            propose.validate_response(response)
        self.assertIn("missing required fields", str(ctx.exception))

    def test_rejects_unknown_field(self) -> None:
        response = {
            "mutations": [
                {
                    "base_factor": "auto_settlement_x",
                    "mutation_type": "add_capacity_gate",
                    "unexpected_field": "value",
                }
            ]
        }
        with self.assertRaises(propose.SchemaValidationError) as ctx:
            propose.validate_response(response)
        self.assertIn("unknown fields", str(ctx.exception))

    def test_rejects_non_numeric_constant(self) -> None:
        response = {
            "mutations": [
                {
                    "base_factor": "auto_settlement_x",
                    "mutation_type": "add_spread_penalty",
                    "constant": "not-a-number",
                }
            ]
        }
        with self.assertRaises(propose.SchemaValidationError):
            propose.validate_response(response)

    def test_rejects_non_integer_window(self) -> None:
        response = {
            "mutations": [
                {
                    "base_factor": "auto_settlement_x",
                    "mutation_type": "change_time_window",
                    "window": 30.5,
                }
            ]
        }
        with self.assertRaises(propose.SchemaValidationError):
            propose.validate_response(response)

    def test_rejects_boolean_window(self) -> None:
        response = {
            "mutations": [
                {
                    "base_factor": "auto_settlement_x",
                    "mutation_type": "change_time_window",
                    "window": True,
                }
            ]
        }
        with self.assertRaises(propose.SchemaValidationError):
            propose.validate_response(response)

    def test_rejects_boolean_numeric_constant(self) -> None:
        response = {
            "mutations": [
                {
                    "base_factor": "auto_settlement_x",
                    "mutation_type": "add_spread_penalty",
                    "constant": False,
                }
            ]
        }
        with self.assertRaises(propose.SchemaValidationError):
            propose.validate_response(response)


class ProposeMutationsTests(unittest.TestCase):
    def _run(self, tmp: str):
        from scripts.alpha_search_closed_loop_agent import DEFAULT_TARGET, load_artifact

        path = artifact(Path(tmp))
        return load_artifact(path, DEFAULT_TARGET)

    def test_returns_validated_mutations_on_first_success(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run = self._run(tmp)
            client = FakeClient(
                [
                    {
                        "mutations": [
                            {
                                "base_factor": "auto_settlement_full_depth_settlement_edge",
                                "mutation_type": "add_capacity_gate",
                                "feature": "entry_capacity_score",
                            }
                        ]
                    }
                ]
            )
            mutations = propose.propose_mutations(client, run)

        self.assertEqual(len(mutations), 1)
        self.assertEqual(len(client.calls), 1)

    def test_retries_on_schema_failure_then_succeeds(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run = self._run(tmp)
            client = FakeClient(
                [
                    {"mutations": [{"mutation_type": "add_capacity_gate"}]},  # missing base_factor
                    {
                        "mutations": [
                            {
                                "base_factor": "auto_settlement_full_depth_settlement_edge",
                                "mutation_type": "add_capacity_gate",
                            }
                        ]
                    },
                ]
            )
            mutations = propose.propose_mutations(client, run, max_retries=2)

        self.assertEqual(len(mutations), 1)
        self.assertEqual(len(client.calls), 2)
        # The retry prompt must mention the rejection reason so the model can self-correct.
        self.assertIn("was rejected", client.calls[1])

    def test_raises_after_exhausting_retries(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run = self._run(tmp)
            client = FakeClient(
                [
                    {"mutations": "not-a-list"},
                    {"mutations": "still-not-a-list"},
                    {"mutations": "nope"},
                ]
            )
            with self.assertRaises(propose.SchemaValidationError):
                propose.propose_mutations(client, run, max_retries=2)
        self.assertEqual(len(client.calls), 3)

    def test_mutation_limit_truncates_results(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run = self._run(tmp)
            many_mutations = [
                {
                    "base_factor": "auto_settlement_full_depth_settlement_edge",
                    "mutation_type": "add_capacity_gate",
                }
                for _ in range(5)
            ]
            client = FakeClient([{"mutations": many_mutations}])
            mutations = propose.propose_mutations(client, run, mutation_limit=2)

        self.assertEqual(len(mutations), 2)


class UnconfiguredLlmClientTests(unittest.TestCase):
    def test_raises_runtime_error(self) -> None:
        with self.assertRaises(RuntimeError):
            propose.UnconfiguredLlmClient().propose("any prompt")


def _fake_response(json_payload: dict, status_ok: bool = True) -> mock.Mock:
    response = mock.Mock()
    response.json.return_value = json_payload
    if status_ok:
        response.raise_for_status.return_value = None
    else:
        response.raise_for_status.side_effect = RuntimeError("HTTP error")
    return response


class AnthropicLlmClientTests(unittest.TestCase):
    def test_propose_extracts_tool_use_input(self) -> None:
        client = propose.AnthropicLlmClient("test-key")
        tool_input = {
            "mutations": [
                {"base_factor": "auto_settlement_x", "mutation_type": "add_capacity_gate"}
            ]
        }
        fake_response = _fake_response(
            {
                "content": [
                    {"type": "tool_use", "name": "propose_mutations", "input": tool_input}
                ],
                "usage": {"input_tokens": 10, "output_tokens": 5},
            }
        )
        with mock.patch("requests.post", return_value=fake_response) as post:
            result = client.propose("a prompt")

        self.assertEqual(result, tool_input)
        # Confirm the call is a tool-forced request, not free-text parsing.
        _, kwargs = post.call_args
        self.assertEqual(kwargs["json"]["tool_choice"], {"type": "tool", "name": "propose_mutations"})
        self.assertEqual(kwargs["headers"]["x-api-key"], "test-key")
        self.assertEqual(client.last_usage, {"input_tokens": 10, "output_tokens": 5})

    def test_propose_raises_when_no_tool_use_block(self) -> None:
        client = propose.AnthropicLlmClient("test-key")
        fake_response = _fake_response({"content": [{"type": "text", "text": "not a tool call"}]})
        with mock.patch("requests.post", return_value=fake_response):
            with self.assertRaises(RuntimeError):
                client.propose("a prompt")

    def test_propose_propagates_http_errors(self) -> None:
        client = propose.AnthropicLlmClient("test-key")
        fake_response = _fake_response({}, status_ok=False)
        with mock.patch("requests.post", return_value=fake_response):
            with self.assertRaises(RuntimeError):
                client.propose("a prompt")


class OpenAiLlmClientTests(unittest.TestCase):
    def test_propose_extracts_output_text_and_parses_json(self) -> None:
        client = propose.OpenAiLlmClient("test-key")
        mutations_payload = {
            "mutations": [
                {"base_factor": "auto_settlement_x", "mutation_type": "add_capacity_gate"}
            ]
        }
        fake_response = _fake_response(
            {
                "output": [
                    {
                        "content": [
                            {"type": "output_text", "text": json.dumps(mutations_payload)}
                        ]
                    }
                ],
                "usage": {"input_tokens": 12, "output_tokens": 6},
            }
        )
        with mock.patch("requests.post", return_value=fake_response) as post:
            result = client.propose("a prompt")

        self.assertEqual(result, mutations_payload)
        _, kwargs = post.call_args
        self.assertEqual(kwargs["json"]["text"]["format"]["type"], "json_schema")
        self.assertEqual(kwargs["headers"]["Authorization"], "Bearer test-key")
        self.assertEqual(client.last_usage, {"input_tokens": 12, "output_tokens": 6})

    def test_propose_raises_when_output_text_missing(self) -> None:
        client = propose.OpenAiLlmClient("test-key")
        fake_response = _fake_response({"output": []})
        with mock.patch("requests.post", return_value=fake_response):
            with self.assertRaises(RuntimeError):
                client.propose("a prompt")

    def test_propose_raises_on_non_object_json(self) -> None:
        client = propose.OpenAiLlmClient("test-key")
        fake_response = _fake_response(
            {"output": [{"content": [{"type": "output_text", "text": "[1, 2, 3]"}]}]}
        )
        with mock.patch("requests.post", return_value=fake_response):
            with self.assertRaises(RuntimeError):
                client.propose("a prompt")


class ClientFromEnvTests(unittest.TestCase):
    def test_returns_unconfigured_when_no_api_key(self) -> None:
        client = propose.client_from_env({})
        self.assertIsInstance(client, propose.UnconfiguredLlmClient)

    def test_defaults_to_anthropic_provider(self) -> None:
        client = propose.client_from_env({"PLOY_RESEARCH_LLM_API_KEY": "key"})
        self.assertIsInstance(client, propose.AnthropicLlmClient)

    def test_selects_openai_provider(self) -> None:
        client = propose.client_from_env(
            {
                "PLOY_RESEARCH_LLM_API_KEY": "key",
                "PLOY_RESEARCH_LLM_PROVIDER": "openai",
            }
        )
        self.assertIsInstance(client, propose.OpenAiLlmClient)

    def test_rejects_unknown_provider(self) -> None:
        with self.assertRaises(RuntimeError):
            propose.client_from_env(
                {
                    "PLOY_RESEARCH_LLM_API_KEY": "key",
                    "PLOY_RESEARCH_LLM_PROVIDER": "not-a-real-provider",
                }
            )

    def test_honors_model_override(self) -> None:
        client = propose.client_from_env(
            {
                "PLOY_RESEARCH_LLM_API_KEY": "key",
                "PLOY_RESEARCH_LLM_PROVIDER": "openai",
                "PLOY_RESEARCH_LLM_MODEL": "gpt-5.5-mini",
            }
        )
        self.assertEqual(client._model, "gpt-5.5-mini")


class BuildPriorFromMutationsTests(unittest.TestCase):
    def test_matches_closed_loop_agent_prior_shape(self) -> None:
        mutations = [
            {"base_factor": "auto_settlement_x", "mutation_type": "add_capacity_gate"}
        ]
        prior = propose.build_prior_from_mutations("full_depth_settlement_executable_pnl", mutations)

        self.assertEqual(prior["schema_version"], 1)
        self.assertEqual(prior["kind"], "typed_llm_prior_draft")
        self.assertEqual(prior["source"], "alpha_search_llm_propose")
        self.assertEqual(prior["target"], "full_depth_settlement_executable_pnl")
        self.assertEqual(prior["mutations"], mutations)
        self.assertEqual(prior["runtime_avoid_factors"], [])


class MainIntegrationTests(unittest.TestCase):
    def test_main_writes_prior_on_success(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            output_path = Path(tmp) / "next-llm-prior.json"
            client = FakeClient(
                [
                    {
                        "mutations": [
                            {
                                "base_factor": "auto_settlement_full_depth_settlement_edge",
                                "mutation_type": "add_capacity_gate",
                                "feature": "entry_capacity_score",
                            }
                        ]
                    }
                ]
            )
            client.last_usage = {"input_tokens": 20, "output_tokens": 8}
            import sys

            argv = sys.argv
            sys.argv = [
                "alpha_search_llm_propose.py",
                str(path),
                "--output-prior-json",
                str(output_path),
            ]
            try:
                with mock.patch.object(
                    propose, "client_from_env", return_value=client
                ), mock.patch.object(
                    propose, "propose_mutations", wraps=propose.propose_mutations
                ) as propose_mutations:
                    propose.main()
            finally:
                sys.argv = argv

            self.assertTrue(output_path.exists())
            prior = json.loads(output_path.read_text(encoding="utf-8"))
            self.assertEqual(prior["source"], "alpha_search_llm_propose")
            self.assertEqual(len(prior["mutations"]), 1)
            self.assertEqual(propose_mutations.call_count, 1)
            usage_path = output_path.with_name("llm-expansion-usage.json")
            usage = json.loads(usage_path.read_text(encoding="utf-8"))
            self.assertEqual(usage["source"], "alpha_search_llm_propose")
            self.assertEqual(usage["mutation_count"], 1)
            self.assertEqual(usage["usage"], {"input_tokens": 20, "output_tokens": 8})

    def test_main_does_not_overwrite_prior_for_empty_mutations(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            output_path = Path(tmp) / "next-llm-prior.json"
            output_path.write_text("existing deterministic prior", encoding="utf-8")
            client = FakeClient([{"mutations": []}])
            import sys

            argv = sys.argv
            sys.argv = [
                "alpha_search_llm_propose.py",
                str(path),
                "--output-prior-json",
                str(output_path),
            ]
            try:
                with mock.patch.object(propose, "client_from_env", return_value=client):
                    propose.main()
            finally:
                sys.argv = argv

            self.assertEqual(
                output_path.read_text(encoding="utf-8"), "existing deterministic prior"
            )

    def test_main_fails_soft_on_corrupt_optional_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            output_path = Path(tmp) / "next-llm-prior.json"
            zoo_path = Path(tmp) / "alpha-zoo-snapshot.json"
            zoo_path.write_text("{not json", encoding="utf-8")
            import sys

            argv = sys.argv
            sys.argv = [
                "alpha_search_llm_propose.py",
                str(path),
                "--output-prior-json",
                str(output_path),
                "--alpha-zoo-snapshot-json",
                str(zoo_path),
            ]
            try:
                propose.main(env={"PLOY_RESEARCH_LLM_API_KEY": ""})
            finally:
                sys.argv = argv

            self.assertFalse(output_path.exists())

    def test_main_fails_soft_when_no_client_is_configured(self) -> None:
        # main() uses UnconfiguredLlmClient when no API key is set, so it must
        # exit cleanly rather than raising or writing a partial prior file.
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            output_path = Path(tmp) / "next-llm-prior.json"
            import sys

            argv = sys.argv
            sys.argv = [
                "alpha_search_llm_propose.py",
                str(path),
                "--output-prior-json",
                str(output_path),
            ]
            try:
                propose.main()
            finally:
                sys.argv = argv

        self.assertFalse(output_path.exists())


if __name__ == "__main__":
    unittest.main()
