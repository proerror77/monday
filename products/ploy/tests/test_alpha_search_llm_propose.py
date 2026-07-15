import hashlib
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


def mission(
    *,
    target: str = "full_depth_settlement_executable_pnl",
    symbol: str = "BTC",
    mutable_scope: list[str] | None = None,
) -> dict:
    payload = {
        "schema_version": "prediction_research_mission.v1",
        "mission_id": f"polymarket-{symbol.lower()}-5m-v1",
        "lane": "prediction_market",
        "objective": f"Predict the official five-minute {symbol} settlement probability.",
        "hypothesis_scope": "CEX microstructure may lead prediction-market repricing.",
        "mutable_scope": mutable_scope
        if mutable_scope is not None
        else ["factor_formula", "probability_blend_weights"],
        "data_snapshot_id": f"sha256:{symbol.lower()}-dataset",
        "target": target,
        "symbols": [symbol],
        "horizon": "5m",
        "prompt_snapshot_id": "pending",
        "search_policy_snapshot_id": "prediction-blend-policy-v1",
        "search_budget": {
            "max_candidates": 6,
            "max_llm_calls": 2,
            "max_seconds": 600,
        },
    }
    payload["prompt_snapshot_id"] = propose.research_brief_snapshot_id(payload)
    return payload


def artifact(
    root: Path,
    *,
    target: str = "full_depth_settlement_executable_pnl",
    selected_nodes: list[dict] | None = None,
    avoided_subtrees: list[dict] | None = None,
    prediction_feedback: dict | None = None,
    input_prior: dict | None = None,
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
    if prediction_feedback is not None:
        write_json(alpha_root / "prediction-research-feedback.json", prediction_feedback)
    if input_prior is not None:
        write_json(
            root
            / "alpha-search-chain"
            / "input-alpha-search-plan"
            / "next-llm-prior.json",
            input_prior,
        )
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
    def test_load_artifact_verifies_content_addressed_prediction_feedback(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            alpha_root = (
                path
                / "factor-walk-forward-v2"
                / "alpha-search"
                / "full_depth_settlement_executable_pnl"
            )
            payload = {"mission_id": "polymarket-btc-5m-v1", "candidates": []}
            raw = (json.dumps(payload, sort_keys=True) + "\n").encode()
            digest = hashlib.sha256(raw).hexdigest()
            feedback_path = alpha_root / f"prediction-research-feedback-{digest}.json"
            feedback_path.write_bytes(raw)
            from scripts.alpha_search_closed_loop_agent import DEFAULT_TARGET, load_artifact

            self.assertEqual(load_artifact(path, DEFAULT_TARGET)["prediction_feedback"], payload)
            feedback_path.write_text("{}\n", encoding="utf-8")
            self.assertEqual(load_artifact(path, DEFAULT_TARGET)["prediction_feedback"], {})

    def test_provider_schema_requires_hypotheses_for_both_candidate_types(self) -> None:
        schema = propose._proposal_json_schema()
        mutation_properties = schema["properties"]["mutations"]["items"]["properties"]
        blend_schema = schema["properties"]["probability_blends"]["items"]

        self.assertIn("hypothesis", mutation_properties)
        self.assertIn(
            "hypothesis", schema["properties"]["mutations"]["items"]["required"]
        )
        self.assertIn("hypothesis", blend_schema["properties"])
        self.assertIn("hypothesis", blend_schema["required"])

    def test_prompt_lists_allowed_mutations_from_shared_constant(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            from scripts.alpha_search_closed_loop_agent import DEFAULT_TARGET, load_artifact

            run = load_artifact(path, DEFAULT_TARGET)
            prompt = propose.build_prompt(run, mission())

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
            prompt = propose.build_prompt(run, mission())

        payload = json.loads(prompt)
        self.assertEqual(len(payload["weak_dimensions"]), 1)
        self.assertEqual(payload["weak_dimensions"][0]["factor_name"], "auto_settlement_x")
        self.assertEqual(payload["weak_dimensions"][0]["selected_dimension"], "overfit_risk")
        self.assertNotIn("reward", payload["weak_dimensions"][0])

    def test_prompt_includes_crowded_structural_shapes(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            from scripts.alpha_search_closed_loop_agent import DEFAULT_TARGET, load_artifact

            run = load_artifact(path, DEFAULT_TARGET)
            avoided = [
                {"root_gene": "SafeDiv", "count": 4, "action": "penalize", "reason": "x"},
                {"root_gene": "Add", "count": 1, "action": "keep", "reason": "y"},
            ]
            prompt = propose.build_prompt(run, mission(), avoided_subtrees=avoided)

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
            prompt = propose.build_prompt(run, mission(), alpha_zoo_snapshot=zoo)

        payload = json.loads(prompt)
        entries = payload["crowded_root_genes_across_all_history"]
        self.assertEqual(entries, [{"root_gene": "Mul", "count": 12}])

    def test_prompt_omits_alpha_zoo_summary_when_absent(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            from scripts.alpha_search_closed_loop_agent import DEFAULT_TARGET, load_artifact

            run = load_artifact(path, DEFAULT_TARGET)
            prompt = propose.build_prompt(run, mission())

        payload = json.loads(prompt)
        self.assertEqual(payload["crowded_root_genes_across_all_history"], [])

    def test_prompt_carries_governed_mission_and_only_qualitative_feedback(self) -> None:
        btc_mission = mission()
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(
                Path(tmp),
                prediction_feedback={
                    "schema_version": "prediction_research_feedback.v1",
                    "mission_id": "polymarket-btc-5m-v1",
                    "target": "full_depth_settlement_executable_pnl",
                    "symbols": ["BTC"],
                    "horizon": "5m",
                    "data_snapshot_id": "sha256:btc-dataset",
                    "prompt_snapshot_id": btc_mission["prompt_snapshot_id"],
                    "search_policy_snapshot_id": "prediction-blend-policy-v1",
                    "candidates": [
                        {
                            "model": "q_llm_microstructure",
                            "hypothesis": "CEX flow improves calibration.",
                            "probability_blend": {
                                "name": "microstructure",
                                "hypothesis": "CEX flow improves calibration.",
                                "market_midpoint_weight": 0.4,
                                "distance_lob_vol_weight": 0.3,
                                "event_surface_weight": 0.2,
                                "existing_model_weight": 0.1,
                            },
                            "verdict": "discard",
                            "reason_codes": ["calibration_gate_failed"],
                            "metrics": {"avg_test_brier_score": 0.42},
                        }
                    ],
                },
            )
            from scripts.alpha_search_closed_loop_agent import DEFAULT_TARGET, load_artifact

            run = load_artifact(path, DEFAULT_TARGET)
            prompt = propose.build_prompt(run, btc_mission)

        payload = json.loads(prompt)
        self.assertEqual(payload["mission"]["symbols"], ["BTC"])
        self.assertEqual(payload["mission"]["horizon"], "5m")
        self.assertIn("CEX microstructure", payload["mission"]["hypothesis_scope"])
        self.assertEqual(
            payload["prior_candidate_outcomes"],
            [
                {
                    "model": "q_llm_microstructure",
                    "hypothesis": "CEX flow improves calibration.",
                    "probability_blend": {
                        "name": "microstructure",
                        "market_midpoint_weight": 0.4,
                        "distance_lob_vol_weight": 0.3,
                        "event_surface_weight": 0.2,
                        "existing_model_weight": 0.1,
                    },
                    "verdict": "discard",
                    "reason_codes": ["calibration_gate_failed"],
                }
            ],
        )
        self.assertNotIn("0.42", prompt)

    def test_prompt_rejects_feedback_from_another_mission(self) -> None:
        feedback = {
            "schema_version": "prediction_research_feedback.v1",
            "mission_id": "polymarket-sol-5m-v1",
            "target": "full_depth_settlement_executable_pnl",
            "symbols": ["SOL"],
            "horizon": "5m",
            "data_snapshot_id": "sha256:sol-dataset",
            "prompt_snapshot_id": "sha256:sol-prompt",
            "search_policy_snapshot_id": "prediction-blend-policy-v1",
            "candidates": [
                {
                    "model": "q_llm_sol_flow",
                    "hypothesis": "SOL flow improves calibration.",
                    "verdict": "discard",
                    "reason_codes": ["calibration_gate_failed"],
                }
            ],
        }
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), prediction_feedback=feedback)
            from scripts.alpha_search_closed_loop_agent import DEFAULT_TARGET, load_artifact

            run = load_artifact(path, DEFAULT_TARGET)
            prompt = propose.build_prompt(run, mission(symbol="BTC"))

        self.assertEqual(json.loads(prompt)["prior_candidate_outcomes"], [])


class MissionValidationTests(unittest.TestCase):
    def test_rejects_cross_target_mission(self) -> None:
        with self.assertRaises(propose.MissionValidationError):
            propose.validate_mission(mission(target="reprice_pnl_10s"), "full_depth_settlement_executable_pnl")

    def test_probability_blends_reject_unsupported_settlement_target(self) -> None:
        target = "tradeable_full_depth_settlement_pnl"
        with self.assertRaises(propose.MissionValidationError):
            propose.validate_mission(mission(target=target), target)

    def test_rejects_multi_symbol_mission(self) -> None:
        payload = mission()
        payload["symbols"] = ["BTC", "SOL"]
        with self.assertRaises(propose.MissionValidationError):
            propose.validate_mission(payload, "full_depth_settlement_executable_pnl")

    def test_rejects_mission_without_mutable_authority(self) -> None:
        with self.assertRaises(propose.MissionValidationError):
            propose.validate_mission(
                mission(mutable_scope=["validator_thresholds"]),
                "full_depth_settlement_executable_pnl",
            )
        with self.assertRaises(propose.MissionValidationError):
            propose.validate_mission(
                mission(
                    mutable_scope=[
                        "probability_blend_weights",
                        "validator_thresholds",
                    ]
                ),
                "full_depth_settlement_executable_pnl",
            )

    def test_rejects_wrong_horizon_and_unresolved_provenance(self) -> None:
        wrong_horizon = mission()
        wrong_horizon["horizon"] = "15m"
        with self.assertRaises(propose.MissionValidationError):
            propose.validate_mission(
                wrong_horizon, "full_depth_settlement_executable_pnl"
            )

        unresolved = mission()
        unresolved["data_snapshot_id"] = "REPLACE_WITH_DATASET"
        with self.assertRaises(propose.MissionValidationError):
            propose.validate_mission(
                unresolved, "full_depth_settlement_executable_pnl"
            )

        changed_brief = mission()
        changed_brief["objective"] = "A different unreviewed objective."
        with self.assertRaisesRegex(
            propose.MissionValidationError, "prompt_snapshot_id"
        ):
            propose.validate_mission(
                changed_brief, "full_depth_settlement_executable_pnl"
            )


class ValidateResponseTests(unittest.TestCase):
    def test_accepts_a_well_formed_response(self) -> None:
        response = {
            "mutations": [
                {
                    "base_factor": "auto_settlement_x",
                    "mutation_type": "add_capacity_gate",
                    "hypothesis": "Capacity gating should remove unfillable false edges.",
                    "feature": "entry_capacity_score",
                }
            ],
            "probability_blends": [
                {
                    "name": "microstructure",
                    "hypothesis": "CEX-informed components improve calibration.",
                    "market_midpoint_weight": 0.4,
                    "distance_lob_vol_weight": 0.3,
                    "event_surface_weight": 0.2,
                    "existing_model_weight": 0.1,
                }
            ],
        }
        validated = propose.validate_response(
            response,
            allowed_base_factors={"auto_settlement_x"},
            mutable_scope={"factor_formula", "probability_blend_weights"},
        )
        self.assertEqual(len(validated["mutations"]), 1)
        self.assertEqual(validated["mutations"][0]["base_factor"], "auto_settlement_x")
        self.assertEqual(validated["probability_blends"][0]["name"], "microstructure")

    def test_rejects_unknown_base_factor(self) -> None:
        response = {
            "mutations": [
                {
                    "base_factor": "invented",
                    "mutation_type": "add_capacity_gate",
                    "hypothesis": "The invented factor should fail closed.",
                }
            ],
            "probability_blends": [],
        }
        with self.assertRaises(propose.SchemaValidationError) as ctx:
            propose.validate_response(
                response,
                allowed_base_factors={"auto_settlement_x"},
                mutable_scope={"factor_formula"},
            )
        self.assertIn("existing base factor", str(ctx.exception))

    def test_rejects_probability_blend_without_mission_authority(self) -> None:
        response = {
            "mutations": [],
            "probability_blends": [
                {
                    "name": "unauthorized",
                    "hypothesis": "This proposal lacks mission authority.",
                    "market_midpoint_weight": 1.0,
                    "distance_lob_vol_weight": 0.0,
                    "event_surface_weight": 0.0,
                    "existing_model_weight": 0.0,
                }
            ],
        }
        with self.assertRaises(propose.SchemaValidationError):
            propose.validate_response(
                response,
                allowed_base_factors=set(),
                mutable_scope={"factor_formula"},
            )

    def test_rejects_negative_or_all_zero_probability_weights(self) -> None:
        for weights in (
            (-0.1, 0.3, 0.3, 0.5),
            (0.0, 0.0, 0.0, 0.0),
            (1e308, 1e308, 0.0, 0.0),
        ):
            response = {
                "mutations": [],
                "probability_blends": [
                    {
                        "name": "invalid",
                        "hypothesis": "Invalid weights must fail closed.",
                        "market_midpoint_weight": weights[0],
                        "distance_lob_vol_weight": weights[1],
                        "event_surface_weight": weights[2],
                        "existing_model_weight": weights[3],
                    }
                ],
            }
            with self.assertRaises(propose.SchemaValidationError):
                propose.validate_response(
                    response,
                    allowed_base_factors=set(),
                    mutable_scope={"probability_blend_weights"},
                )

    def test_rejects_non_dict_response(self) -> None:
        with self.assertRaises(propose.SchemaValidationError):
            propose.validate_response(["not", "a", "dict"])

    def test_rejects_missing_mutations_list(self) -> None:
        with self.assertRaises(propose.SchemaValidationError):
            propose.validate_response({})

    def test_rejects_unknown_mutation_type(self) -> None:
        response = {
            "mutations": [
                {
                    "base_factor": "auto_settlement_x",
                    "mutation_type": "delete_everything",
                    "hypothesis": "Unsupported mutation authority must fail closed.",
                }
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
                    "hypothesis": "Capacity gating should remove unfillable false edges.",
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
                    "hypothesis": "Spread penalization should improve executable edge.",
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
                    "hypothesis": "A different window should expose stable information.",
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
                    "hypothesis": "A different window should expose stable information.",
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
                    "hypothesis": "Spread penalization should improve executable edge.",
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
                                "hypothesis": "Capacity gating should improve executable OOS PnL.",
                                "feature": "entry_capacity_score",
                            }
                        ]
                    }
                ]
            )
            proposal = propose.propose_candidates(client, run, mission())

        self.assertEqual(len(proposal["mutations"]), 1)
        self.assertEqual(len(client.calls), 1)

    def test_retries_on_schema_failure_then_succeeds(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run = self._run(tmp)
            client = FakeClient(
                [
                    {
                        "mutations": [{"mutation_type": "add_capacity_gate"}],
                        "probability_blends": [],
                    },  # missing base_factor
                    {
                        "mutations": [
                            {
                                "base_factor": "auto_settlement_full_depth_settlement_edge",
                                "mutation_type": "add_capacity_gate",
                                "hypothesis": "Capacity gating should improve executable OOS PnL.",
                            }
                        ],
                        "probability_blends": [],
                    },
                ]
            )
            proposal = propose.propose_candidates(client, run, mission(), max_retries=2)

        self.assertEqual(len(proposal["mutations"]), 1)
        self.assertEqual(len(client.calls), 2)
        # The retry prompt must mention the rejection reason so the model can self-correct.
        self.assertIn("was rejected", client.calls[1])

    def test_raises_after_exhausting_retries(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run = self._run(tmp)
            client = FakeClient(
                [
                    {"mutations": "not-a-list", "probability_blends": []},
                    {"mutations": "still-not-a-list", "probability_blends": []},
                    {"mutations": "nope", "probability_blends": []},
                ]
            )
            with self.assertRaises(propose.SchemaValidationError):
                propose.propose_candidates(client, run, mission(), max_retries=2)
        self.assertEqual(len(client.calls), 2)

    def test_mutation_limit_truncates_results(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            run = self._run(tmp)
            many_mutations = [
                {
                    "base_factor": "auto_settlement_full_depth_settlement_edge",
                    "mutation_type": "add_capacity_gate",
                    "hypothesis": "Capacity gating should improve executable OOS PnL.",
                }
                for _ in range(5)
            ]
            client = FakeClient(
                [{"mutations": many_mutations, "probability_blends": []}]
            )
            proposal = propose.propose_candidates(
                client, run, mission(), mutation_limit=2
            )

        self.assertEqual(len(proposal["mutations"]), 2)


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


def _fake_requests_module(response: mock.Mock) -> tuple[mock.Mock, mock.Mock]:
    post = mock.Mock(return_value=response)
    return mock.Mock(post=post), post


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
        requests_module, post = _fake_requests_module(fake_response)
        with mock.patch.dict("sys.modules", {"requests": requests_module}):
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
        requests_module, _ = _fake_requests_module(fake_response)
        with mock.patch.dict("sys.modules", {"requests": requests_module}):
            with self.assertRaises(RuntimeError):
                client.propose("a prompt")

    def test_propose_propagates_http_errors(self) -> None:
        client = propose.AnthropicLlmClient("test-key")
        fake_response = _fake_response({}, status_ok=False)
        requests_module, _ = _fake_requests_module(fake_response)
        with mock.patch.dict("sys.modules", {"requests": requests_module}):
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
        requests_module, post = _fake_requests_module(fake_response)
        with mock.patch.dict("sys.modules", {"requests": requests_module}):
            result = client.propose("a prompt")

        self.assertEqual(result, mutations_payload)
        _, kwargs = post.call_args
        self.assertEqual(kwargs["json"]["text"]["format"]["type"], "json_schema")
        self.assertEqual(kwargs["headers"]["Authorization"], "Bearer test-key")
        self.assertEqual(client.last_usage, {"input_tokens": 12, "output_tokens": 6})

    def test_propose_raises_when_output_text_missing(self) -> None:
        client = propose.OpenAiLlmClient("test-key")
        fake_response = _fake_response({"output": []})
        requests_module, _ = _fake_requests_module(fake_response)
        with mock.patch.dict("sys.modules", {"requests": requests_module}):
            with self.assertRaises(RuntimeError):
                client.propose("a prompt")

    def test_propose_raises_on_non_object_json(self) -> None:
        client = propose.OpenAiLlmClient("test-key")
        fake_response = _fake_response(
            {"output": [{"content": [{"type": "output_text", "text": "[1, 2, 3]"}]}]}
        )
        requests_module, _ = _fake_requests_module(fake_response)
        with mock.patch.dict("sys.modules", {"requests": requests_module}):
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

    def test_honors_mission_derived_provider_timeout(self) -> None:
        client = propose.client_from_env(
            {"PLOY_RESEARCH_LLM_API_KEY": "key"}, timeout_secs=12.5
        )
        self.assertEqual(client._timeout_secs, 12.5)


class BuildPriorFromMutationsTests(unittest.TestCase):
    def test_matches_closed_loop_agent_prior_shape(self) -> None:
        mutations = [
            {
                "base_factor": "auto_settlement_x",
                "mutation_type": "add_capacity_gate",
                "hypothesis": "Capacity gating should improve executable OOS PnL.",
            }
        ]
        probability_blends = [
            {
                "name": "microstructure",
                "hypothesis": "CEX-informed components improve calibration.",
                "market_midpoint_weight": 0.4,
                "distance_lob_vol_weight": 0.3,
                "event_surface_weight": 0.2,
                "existing_model_weight": 0.1,
            }
        ]
        prior = propose.build_prior(
            "full_depth_settlement_executable_pnl",
            mission(),
            {"mutations": mutations, "probability_blends": probability_blends},
        )

        self.assertEqual(prior["schema_version"], 1)
        self.assertEqual(prior["kind"], "typed_llm_prior_draft")
        self.assertEqual(prior["source"], "alpha_search_llm_propose")
        self.assertEqual(prior["target"], "full_depth_settlement_executable_pnl")
        self.assertEqual(prior["mutations"], mutations)
        self.assertEqual(prior["probability_blends"], probability_blends)
        self.assertEqual(prior["mission_id"], "polymarket-btc-5m-v1")
        self.assertEqual(
            prior["mission"]["prompt_snapshot_id"],
            propose.research_brief_snapshot_id(prior["mission"]),
        )
        self.assertEqual(prior["runtime_avoid_factors"], [])


class MainIntegrationTests(unittest.TestCase):
    def test_main_writes_prior_on_success(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            output_path = Path(tmp) / "next-llm-prior.json"
            mission_path = Path(tmp) / "mission.json"
            write_json(mission_path, mission())
            client = FakeClient(
                [
                    {
                        "mutations": [
                            {
                                "base_factor": "auto_settlement_full_depth_settlement_edge",
                                "mutation_type": "add_capacity_gate",
                                "hypothesis": "Capacity gating should improve executable OOS PnL.",
                                "feature": "entry_capacity_score",
                            }
                        ],
                        "probability_blends": [
                            {
                                "name": "microstructure",
                                "hypothesis": "CEX-informed components improve calibration.",
                                "market_midpoint_weight": 0.4,
                                "distance_lob_vol_weight": 0.3,
                                "event_surface_weight": 0.2,
                                "existing_model_weight": 0.1,
                            }
                        ],
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
                "--mission-json",
                str(mission_path),
            ]
            try:
                with mock.patch.object(
                    propose, "client_from_env", return_value=client
                ), mock.patch.object(
                    propose, "propose_candidates", wraps=propose.propose_candidates
                ) as propose_candidates:
                    propose.main()
            finally:
                sys.argv = argv

            self.assertTrue(output_path.exists())
            prior = json.loads(output_path.read_text(encoding="utf-8"))
            self.assertEqual(prior["source"], "alpha_search_llm_propose")
            self.assertEqual(len(prior["mutations"]), 1)
            self.assertEqual(len(prior["probability_blends"]), 1)
            self.assertEqual(prior["mission_id"], "polymarket-btc-5m-v1")
            self.assertEqual(propose_candidates.call_count, 1)
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
            mission_path = Path(tmp) / "mission.json"
            write_json(mission_path, mission())
            client = FakeClient([{"mutations": [], "probability_blends": []}])
            import sys

            argv = sys.argv
            sys.argv = [
                "alpha_search_llm_propose.py",
                str(path),
                "--output-prior-json",
                str(output_path),
                "--mission-json",
                str(mission_path),
            ]
            try:
                with mock.patch.object(propose, "client_from_env", return_value=client):
                    propose.main()
            finally:
                sys.argv = argv

            self.assertEqual(
                output_path.read_text(encoding="utf-8"), "existing deterministic prior"
            )

    def test_main_does_not_call_model_without_explicit_or_carried_mission(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            output_path = Path(tmp) / "next-llm-prior.json"
            client = FakeClient([{"mutations": [], "probability_blends": []}])
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

        self.assertEqual(client.calls, [])
        self.assertFalse(output_path.exists())

    def test_main_continues_with_mission_carried_by_input_prior(self) -> None:
        carried = {
            "mission": mission(),
            "mission_id": "polymarket-btc-5m-v1",
        }
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp), input_prior=carried)
            output_path = Path(tmp) / "next-llm-prior.json"
            client = FakeClient(
                [
                    {
                        "mutations": [],
                        "probability_blends": [
                            {
                                "name": "continuation",
                                "hypothesis": "The retained components improve OOS calibration.",
                                "market_midpoint_weight": 0.5,
                                "distance_lob_vol_weight": 0.5,
                                "event_surface_weight": 0.0,
                                "existing_model_weight": 0.0,
                            }
                        ],
                    }
                ]
            )
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

            prior = json.loads(output_path.read_text(encoding="utf-8"))

        self.assertEqual(prior["mission_id"], "polymarket-btc-5m-v1")
        self.assertEqual(len(prior["probability_blends"]), 1)

    def test_main_fails_soft_on_corrupt_optional_snapshot(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = artifact(Path(tmp))
            output_path = Path(tmp) / "next-llm-prior.json"
            zoo_path = Path(tmp) / "alpha-zoo-snapshot.json"
            mission_path = Path(tmp) / "mission.json"
            write_json(mission_path, mission())
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
                "--mission-json",
                str(mission_path),
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
            mission_path = Path(tmp) / "mission.json"
            write_json(mission_path, mission())
            import sys

            argv = sys.argv
            sys.argv = [
                "alpha_search_llm_propose.py",
                str(path),
                "--output-prior-json",
                str(output_path),
                "--mission-json",
                str(mission_path),
            ]
            try:
                propose.main()
            finally:
                sys.argv = argv

        self.assertFalse(output_path.exists())


if __name__ == "__main__":
    unittest.main()
