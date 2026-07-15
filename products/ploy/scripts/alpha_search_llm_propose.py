#!/usr/bin/env python3
"""Propose the next governed typed-prior candidate batch via a real model call.

This is the Priority 3 "genuine LLM-driven Expansion" script referenced in
tasks/todo.md. It is deliberately separate from
alpha_search_closed_loop_agent.py, which stays model-free and owns
chain/promotion decisions (single responsibility per script).

Stage B built prompt construction, response schema validation, and retry
logic against an injectable `LlmClient` protocol, with zero real network
calls in tests. Stage C (this file, current state) adds real provider
implementations (`AnthropicLlmClient`, `OpenAiLlmClient`) that call the
provider's HTTP API directly — never the Codex/Claude Code CLI, which is
built for interactive, locally-authenticated sessions and would be fragile
to wire into unattended CI. `client_from_env()` returns
`UnconfiguredLlmClient` (which fails soft, matching how the search path
already degrades when `--alpha-search-llm-prior-json` is omitted) unless
`PLOY_RESEARCH_LLM_API_KEY` is set in the environment — so this script is a
no-op everywhere until that secret is explicitly configured.

The first turn requires a versioned prediction mission. The model may return
only authorities named by that mission: typed `LlmMutationSpec` entries for
AutoFactor diagnostics and/or typed non-negative probability blends for the
event-disjoint prediction evaluator. The proposal is never trusted blindly;
schema, mission scope, target, horizon, symbol, provenance, and budget checks
all run before a `next-llm-prior.json` is written.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import re
from pathlib import Path
from typing import Any, Protocol

try:
    from alpha_search_closed_loop_agent import (
        ALLOWED_MUTATIONS,
        DEFAULT_TARGET,
        load_artifact,
        selected_nodes,
    )
except ModuleNotFoundError:
    from scripts.alpha_search_closed_loop_agent import (
        ALLOWED_MUTATIONS,
        DEFAULT_TARGET,
        load_artifact,
        selected_nodes,
    )


# Fields accepted by LlmMutationSpec (crates/ploy-research/src/autofactor.rs:238-255).
# Kept in sync by hand with that struct; a mismatch here fails validation loudly
# rather than letting an unknown field silently pass through to the Rust compiler.
REQUIRED_MUTATION_FIELDS = {"base_factor", "mutation_type", "hypothesis"}
OPTIONAL_MUTATION_FIELDS = {
    "name",
    "feature",
    "denominator_feature",
    "constant",
    "lo",
    "hi",
    "window",
}
ALL_MUTATION_FIELDS = REQUIRED_MUTATION_FIELDS | OPTIONAL_MUTATION_FIELDS

REQUIRED_PROBABILITY_BLEND_FIELDS = {
    "name",
    "hypothesis",
    "market_midpoint_weight",
    "distance_lob_vol_weight",
    "event_surface_weight",
    "existing_model_weight",
}
PROBABILITY_COMPONENTS = (
    "market_midpoint",
    "distance_lob_vol",
    "event_surface",
    "existing_model",
)
FORMULA_MUTABLE_SCOPES = {"factor_ast", "factor_formula"}
PROBABILITY_BLEND_MUTABLE_SCOPE = "probability_blend_weights"
MISSION_SCHEMA_VERSION = "prediction_research_mission.v1"
SAFE_CANDIDATE_NAME = re.compile(r"^[A-Za-z0-9_-]{1,80}$")
TARGET_HORIZONS = {
    "full_depth_settlement_executable_pnl": "5m",
}

MAX_SCHEMA_RETRIES = 2


class LlmClient(Protocol):
    """Minimal seam between prompt construction and a real model call.

    A production implementation calls the model provider's API directly
    (not the Codex/Claude Code CLI — see tasks/todo.md Priority 3's
    architecture-decision note on why CI should use a plain API key rather
    than CLI auth). Tests inject a fake that returns canned responses so
    prompt construction, schema validation, and retry logic can be verified
    with zero network calls.
    """

    def propose(self, prompt: str) -> dict[str, Any]:
        """Return a parsed JSON object matching the requested response schema."""
        ...


class UnconfiguredLlmClient:
    """Default client: fails loudly instead of silently doing nothing.

    Used whenever no provider API key is configured in the environment.
    main() catches the resulting error and fails soft (see module docstring
    on the fail-soft contract) rather than treating this as fatal.
    """

    def propose(self, prompt: str) -> dict[str, Any]:
        raise RuntimeError(
            "alpha_search_llm_propose has no LlmClient configured for a real "
            "model call. Set PLOY_RESEARCH_LLM_API_KEY (and optionally "
            "PLOY_RESEARCH_LLM_PROVIDER=anthropic|openai) to enable a real "
            "provider call."
        )


# Response schema shared by both providers' structured-output / tool-calling
# request, generated from the same field sets used by validate_response() so
# the model is asked for exactly what will be accepted — not a hand-copied
# third description of the same shape.
def _proposal_json_schema() -> dict[str, Any]:
    return {
        "type": "object",
        "properties": {
            "mutations": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "base_factor": {"type": "string"},
                        "mutation_type": {
                            "type": "string",
                            "enum": sorted(ALLOWED_MUTATIONS),
                        },
                        "name": {"type": "string"},
                        "hypothesis": {"type": "string"},
                        "feature": {"type": "string"},
                        "denominator_feature": {"type": "string"},
                        "constant": {"type": "number"},
                        "lo": {"type": "number"},
                        "hi": {"type": "number"},
                        "window": {"type": "integer"},
                    },
                    "required": sorted(REQUIRED_MUTATION_FIELDS),
                    "additionalProperties": False,
                },
            },
            "probability_blends": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": {"type": "string"},
                        "hypothesis": {"type": "string"},
                        "market_midpoint_weight": {"type": "number", "minimum": 0},
                        "distance_lob_vol_weight": {"type": "number", "minimum": 0},
                        "event_surface_weight": {"type": "number", "minimum": 0},
                        "existing_model_weight": {"type": "number", "minimum": 0},
                    },
                    "required": sorted(REQUIRED_PROBABILITY_BLEND_FIELDS),
                    "additionalProperties": False,
                },
            },
        },
        "required": ["mutations", "probability_blends"],
        "additionalProperties": False,
    }


class AnthropicLlmClient:
    """Real LlmClient backed by a direct Anthropic Messages API call.

    Deliberately calls the HTTP API directly rather than the Claude Code
    CLI: the CLI is built for interactive, locally-authenticated sessions,
    and wiring its auth into unattended CI would be fragile and opaque
    compared to a plain API key in a GitHub secret (see tasks/todo.md
    Priority 3's architecture-decision note).

    Uses tool-calling to force a structured response matching
    _proposal_json_schema() — the model cannot return free text that would
    need fragile parsing; the SDK/API either returns a well-formed tool
    call or the request fails outright.
    """

    API_URL = "https://api.anthropic.com/v1/messages"
    API_VERSION = "2023-06-01"
    TOOL_NAME = "propose_mutations"

    def __init__(
        self,
        api_key: str,
        model: str = "claude-sonnet-5",
        timeout_secs: float = 60.0,
    ) -> None:
        self._api_key = api_key
        self._model = model
        self._timeout_secs = timeout_secs
        self.last_usage: Any = None

    def propose(self, prompt: str) -> dict[str, Any]:
        import requests

        response = requests.post(
            self.API_URL,
            headers={
                "x-api-key": self._api_key,
                "anthropic-version": self.API_VERSION,
                "content-type": "application/json",
            },
            json={
                "model": self._model,
                "max_tokens": 2048,
                "tools": [
                    {
                        "name": self.TOOL_NAME,
                        "description": (
                            "Propose mission-authorized typed factor mutations "
                            "and probability blends for the research loop."
                        ),
                        "input_schema": _proposal_json_schema(),
                    }
                ],
                "tool_choice": {"type": "tool", "name": self.TOOL_NAME},
                "messages": [{"role": "user", "content": prompt}],
            },
            timeout=self._timeout_secs,
        )
        response.raise_for_status()
        payload = response.json()
        self.last_usage = payload.get("usage")
        for block in payload.get("content", []):
            if isinstance(block, dict) and block.get("type") == "tool_use":
                tool_input = block.get("input")
                if isinstance(tool_input, dict):
                    return tool_input
        raise RuntimeError(
            "Anthropic response contained no tool_use block with the "
            f"expected tool name {self.TOOL_NAME!r}"
        )


class OpenAiLlmClient:
    """Real LlmClient backed by a direct OpenAI Responses API call.

    Same rationale as AnthropicLlmClient for calling the HTTP API directly
    instead of the Codex CLI. Uses a JSON-schema-constrained structured
    output request (`response_format`) rather than free-text parsing.
    """

    API_URL = "https://api.openai.com/v1/responses"

    def __init__(
        self,
        api_key: str,
        model: str = "gpt-5.5",
        timeout_secs: float = 60.0,
    ) -> None:
        self._api_key = api_key
        self._model = model
        self._timeout_secs = timeout_secs
        self.last_usage: Any = None

    def propose(self, prompt: str) -> dict[str, Any]:
        import requests

        response = requests.post(
            self.API_URL,
            headers={
                "Authorization": f"Bearer {self._api_key}",
                "content-type": "application/json",
            },
            json={
                "model": self._model,
                "input": prompt,
                "text": {
                    "format": {
                        "type": "json_schema",
                        "name": "propose_mutations",
                        "schema": _proposal_json_schema(),
                        "strict": True,
                    }
                },
            },
            timeout=self._timeout_secs,
        )
        response.raise_for_status()
        payload = response.json()
        self.last_usage = payload.get("usage")
        output_text = _extract_openai_output_text(payload)
        if output_text is None:
            raise RuntimeError(
                "OpenAI Responses API payload contained no output_text content"
            )
        parsed = json.loads(output_text)
        if not isinstance(parsed, dict):
            raise RuntimeError("OpenAI structured output did not decode to a JSON object")
        return parsed


def _extract_openai_output_text(payload: dict[str, Any]) -> str | None:
    output = payload.get("output")
    if not isinstance(output, list):
        return None
    for item in output:
        if not isinstance(item, dict):
            continue
        for content in item.get("content", []):
            if isinstance(content, dict) and content.get("type") == "output_text":
                text = content.get("text")
                if isinstance(text, str):
                    return text
    return None


def client_from_env(env: dict[str, str], timeout_secs: float = 60.0) -> LlmClient:
    """Build a real client from environment variables, or fail soft.

    Returns UnconfiguredLlmClient (which raises on use, caught by main()'s
    fail-soft handling) when no API key is configured — this must never be
    treated as fatal by a caller, matching how the search path already
    degrades when --alpha-search-llm-prior-json is simply omitted.
    """
    api_key = env.get("PLOY_RESEARCH_LLM_API_KEY", "").strip()
    if not api_key:
        return UnconfiguredLlmClient()
    provider = env.get("PLOY_RESEARCH_LLM_PROVIDER", "anthropic").strip().lower()
    model = env.get("PLOY_RESEARCH_LLM_MODEL", "").strip()
    if provider == "anthropic":
        return AnthropicLlmClient(
            api_key, model=model or "claude-sonnet-5", timeout_secs=timeout_secs
        )
    if provider == "openai":
        return OpenAiLlmClient(
            api_key, model=model or "gpt-5.5", timeout_secs=timeout_secs
        )
    raise RuntimeError(
        f"PLOY_RESEARCH_LLM_PROVIDER={provider!r} is not supported; use "
        "'anthropic' or 'openai'"
    )


class SchemaValidationError(ValueError):
    """Raised when a model response does not match the typed proposal schema."""


class MissionValidationError(ValueError):
    """Raised when a prediction-research mission expands or omits authority."""


def _mission_text(payload: dict[str, Any], field: str) -> str:
    value = payload.get(field)
    if not isinstance(value, str) or not value.strip():
        raise MissionValidationError(f"mission.{field} must be a non-empty string")
    return value.strip()


def _mission_provenance(payload: dict[str, Any], field: str) -> str:
    value = _mission_text(payload, field)
    if value.startswith("REPLACE_WITH_"):
        raise MissionValidationError(
            f"mission.{field} placeholder must be replaced with recorded provenance"
        )
    return value


def research_brief_snapshot_id(payload: dict[str, Any]) -> str:
    """Content address the exact human research brief exposed to the model."""
    brief = {
        "hypothesis_scope": _mission_text(payload, "hypothesis_scope"),
        "objective": _mission_text(payload, "objective"),
    }
    body = json.dumps(
        brief, ensure_ascii=False, separators=(",", ":"), sort_keys=True
    ).encode()
    return f"sha256:{hashlib.sha256(body).hexdigest()}"


def validate_mission(payload: Any, expected_target: str) -> dict[str, Any]:
    """Validate and reduce the cross-workspace prediction mission protocol.

    PLOY and alpha-harness remain separate runtimes.  This JSON brief is the
    narrow shared seam: it grants proposal authority, but never evaluator or
    execution authority.  Returning a new object also prevents unrelated
    mission fields from flowing into the model prompt.
    """
    if not isinstance(payload, dict):
        raise MissionValidationError("mission must be a JSON object")
    if payload.get("schema_version") != MISSION_SCHEMA_VERSION:
        raise MissionValidationError(
            f"mission.schema_version must be {MISSION_SCHEMA_VERSION!r}"
        )
    if payload.get("lane") != "prediction_market":
        raise MissionValidationError("mission.lane must be 'prediction_market'")

    target = _mission_text(payload, "target")
    if target != expected_target:
        raise MissionValidationError(
            f"mission.target {target!r} does not match run target {expected_target!r}"
        )
    if target not in TARGET_HORIZONS:
        raise MissionValidationError(
            f"mission.target {target!r} is not supported by the prediction loop"
        )

    symbols = payload.get("symbols")
    if (
        not isinstance(symbols, list)
        or len(symbols) != 1
        or not isinstance(symbols[0], str)
        or not symbols[0].strip()
    ):
        raise MissionValidationError(
            "mission.symbols must contain exactly one non-empty symbol; "
            "BTC and SOL require separate missions"
        )

    mutable_scope = payload.get("mutable_scope")
    if not isinstance(mutable_scope, list) or any(
        not isinstance(item, str) or not item.strip() for item in mutable_scope
    ):
        raise MissionValidationError(
            "mission.mutable_scope must be a non-empty string list"
        )
    normalized_scope = sorted({item.strip() for item in mutable_scope})
    allowed_scope = FORMULA_MUTABLE_SCOPES | {PROBABILITY_BLEND_MUTABLE_SCOPE}
    unknown_scope = sorted(set(normalized_scope) - allowed_scope)
    if unknown_scope:
        raise MissionValidationError(
            "mission.mutable_scope contains unsupported authority: "
            + ", ".join(unknown_scope)
        )
    if not set(normalized_scope).intersection(allowed_scope):
        raise MissionValidationError(
            "mission.mutable_scope grants neither factor-formula nor "
            "probability-blend authority"
        )

    budget = payload.get("search_budget")
    if not isinstance(budget, dict):
        raise MissionValidationError("mission.search_budget must be an object")
    normalized_budget: dict[str, int] = {}
    for field in ("max_candidates", "max_llm_calls", "max_seconds"):
        value = budget.get(field)
        if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
            raise MissionValidationError(
                f"mission.search_budget.{field} must be a positive integer"
            )
        normalized_budget[field] = value

    horizon = _mission_text(payload, "horizon")
    expected_horizon = TARGET_HORIZONS.get(expected_target)
    if expected_horizon is not None and horizon != expected_horizon:
        raise MissionValidationError(
            f"mission.horizon {horizon!r} does not match target horizon "
            f"{expected_horizon!r}"
        )

    objective = _mission_text(payload, "objective")
    hypothesis_scope = _mission_text(payload, "hypothesis_scope")
    prompt_snapshot_id = _mission_provenance(payload, "prompt_snapshot_id")
    expected_prompt_snapshot_id = research_brief_snapshot_id(payload)
    if prompt_snapshot_id != expected_prompt_snapshot_id:
        raise MissionValidationError(
            "mission.prompt_snapshot_id does not content-address objective and "
            f"hypothesis_scope; expected {expected_prompt_snapshot_id}"
        )

    return {
        "schema_version": MISSION_SCHEMA_VERSION,
        "mission_id": _mission_text(payload, "mission_id"),
        "lane": "prediction_market",
        "objective": objective,
        "hypothesis_scope": hypothesis_scope,
        "mutable_scope": normalized_scope,
        "data_snapshot_id": _mission_provenance(payload, "data_snapshot_id"),
        "target": target,
        "symbols": [symbols[0].strip().upper()],
        "horizon": horizon,
        "prompt_snapshot_id": prompt_snapshot_id,
        "search_policy_snapshot_id": _mission_provenance(
            payload, "search_policy_snapshot_id"
        ),
        "search_budget": normalized_budget,
    }


def resolve_mission(
    run: dict[str, Any], explicit_mission: Any, expected_target: str
) -> dict[str, Any]:
    candidate = explicit_mission
    if candidate is None:
        prior = run.get("input_prior")
        candidate = prior.get("mission") if isinstance(prior, dict) else None
    if candidate is None:
        raise MissionValidationError(
            "a governed --mission-json is required for the first LLM turn"
        )
    return validate_mission(candidate, expected_target)


def allowed_mutations_description() -> str:
    """Render the allowed mutation-type list for the prompt.

    Generated from the same ALLOWED_MUTATIONS set used for validation in
    alpha_search_closed_loop_agent.py (which mirrors compile_llm_mutation's
    match arms in autofactor.rs) rather than a hand-duplicated third copy,
    so the prompt cannot silently drift out of sync with what the Rust
    compiler actually accepts.
    """
    return ", ".join(sorted(ALLOWED_MUTATIONS))


def weak_dimensions_summary(plan: dict[str, Any]) -> list[dict[str, Any]]:
    """Summarize weak search dimensions from mcts-expansion-plan.json.

    Mirrors the same `selected_dimension` / `proposed_mutation` fields
    alpha_search_closed_loop_agent.py's mutation_from_node() reads, so the
    LLM sees the same weak-dimension signal the deterministic path already
    uses to pick a default mutation type.
    """
    out = []
    for node in selected_nodes(plan):
        out.append(
            {
                "factor_name": node.get("factor_name"),
                "selected_dimension": node.get("selected_dimension"),
                "proposed_mutation": node.get("proposed_mutation"),
            }
        )
    return out


def qualitative_prediction_feedback(
    payload: Any, mission: dict[str, Any]
) -> list[dict[str, Any]]:
    """Expose same-mission verdicts and candidate definitions, never evaluator metrics."""
    if (
        not isinstance(payload, dict)
        or payload.get("schema_version") != "prediction_research_feedback.v1"
        or any(
            payload.get(field) != mission[field]
            for field in (
                "mission_id",
                "target",
                "symbols",
                "horizon",
                "data_snapshot_id",
                "prompt_snapshot_id",
                "search_policy_snapshot_id",
            )
        )
    ):
        return []
    candidates = payload.get("candidates")
    if not isinstance(candidates, list):
        return []
    out = []
    for candidate in candidates[-8:]:
        if not isinstance(candidate, dict):
            continue
        model = candidate.get("model")
        hypothesis = candidate.get("hypothesis")
        verdict = candidate.get("verdict")
        reasons = candidate.get("reason_codes")
        blend = candidate.get("probability_blend")
        if not isinstance(model, str) or verdict not in {"keep", "discard"}:
            continue
        item = {
            "model": model,
            "hypothesis": hypothesis.strip()[:500]
            if isinstance(hypothesis, str) and hypothesis.strip()
            else "<not-recorded>",
            "verdict": verdict,
            "reason_codes": [reason for reason in reasons if isinstance(reason, str)][:8]
            if isinstance(reasons, list)
            else [],
        }
        if isinstance(blend, dict):
            candidate_definition = {
                field: blend.get(field)
                for field in REQUIRED_PROBABILITY_BLEND_FIELDS
                if field != "hypothesis"
            }
            weights = [
                candidate_definition.get(f"{component}_weight")
                for component in PROBABILITY_COMPONENTS
            ]
            if (
                isinstance(candidate_definition.get("name"), str)
                and all(
                    isinstance(weight, (int, float))
                    and not isinstance(weight, bool)
                    and math.isfinite(float(weight))
                    and float(weight) >= 0.0
                    for weight in weights
                )
                and math.isfinite(sum(float(weight) for weight in weights))
                and sum(float(weight) for weight in weights) > 0.0
            ):
                item["probability_blend"] = candidate_definition
        out.append(item)
    return out


def crowded_signatures_summary(avoided_subtrees: Any) -> list[dict[str, Any]]:
    """Summarize batch-local Frequent-Subtree-Avoidance crowding, if present.

    `avoided-subtrees.json` may not exist (older artifacts, or PR #728's
    structural_signature() not yet merged) — treat absence as "no known
    crowded shapes" rather than an error.
    """
    if not isinstance(avoided_subtrees, list):
        return []
    out = []
    for item in avoided_subtrees:
        if not isinstance(item, dict):
            continue
        if str(item.get("action") or "") != "penalize":
            continue
        out.append(
            {
                "root_gene": item.get("root_gene"),
                "count": item.get("count"),
                "reason": item.get("reason"),
            }
        )
    return out


def alpha_zoo_summary(alpha_zoo_snapshot: Any) -> list[dict[str, Any]]:
    """Summarize an Alpha Zoo snapshot, if present.

    The snapshot is optional input (--alpha-zoo-snapshot-json); when absent,
    the LLM simply has no cross-run historical-crowding signal to avoid.
    """
    if not isinstance(alpha_zoo_snapshot, dict):
        return []
    entries = alpha_zoo_snapshot.get("entries")
    if not isinstance(entries, list):
        return []
    out = []
    for entry in entries:
        if isinstance(entry, dict):
            out.append({"root_gene": entry.get("root_gene"), "count": entry.get("count")})
    return out


def build_prompt(
    run: dict[str, Any],
    mission: dict[str, Any],
    alpha_zoo_snapshot: Any = None,
    avoided_subtrees: Any = None,
    mutation_limit: int = 6,
) -> str:
    """Build the governed prompt and bounded qualitative feedback context.

    The mission objective and hypothesis scope are reviewed research inputs,
    not authority-bearing instructions. The response still passes independent
    schema and mutable-scope checks. Raw labels, metrics, and gate thresholds
    are intentionally absent.
    """
    weak_dimensions = weak_dimensions_summary(run.get("plan") or {})
    available_base_factors = sorted(
        {
            str(item.get("factor_name") or "").strip()
            for item in weak_dimensions
            if str(item.get("factor_name") or "").strip()
        }
    )
    payload = {
        "task": (
            f"Propose up to {mutation_limit} typed candidates for this governed "
            "prediction-market mission. Change only fields named by mutable_scope. "
            "Factor mutations must reference an available_base_factor. Probability "
            "blends and factor mutations must state one falsifiable hypothesis. "
            "Probability blends may only assign finite "
            "non-negative weights to registered components, and must have a positive "
            "total weight. Do not change labels, "
            "evaluation gates, costs, settlement rules, or execution settings."
        ),
        "target": run.get("target"),
        "mission": mission,
        "allowed_mutation_types": allowed_mutations_description(),
        "available_base_factors": available_base_factors,
        "registered_probability_components": list(PROBABILITY_COMPONENTS),
        "weak_dimensions": weak_dimensions,
        "prior_candidate_outcomes": qualitative_prediction_feedback(
            run.get("prediction_feedback"), mission
        ),
        "crowded_structural_shapes_within_batch": crowded_signatures_summary(
            avoided_subtrees
        ),
        "crowded_root_genes_across_all_history": alpha_zoo_summary(alpha_zoo_snapshot),
        "response_schema": {
            "mutations": [
                {
                    "base_factor": "string, required, must match an existing factor name",
                    "mutation_type": "string, required, one of allowed_mutation_types",
                    "hypothesis": "one falsifiable sentence, required",
                    "name": "string, optional",
                    "feature": "string, optional",
                    "denominator_feature": "string, optional",
                    "constant": "number, optional",
                    "lo": "number, optional",
                    "hi": "number, optional",
                    "window": "integer, optional",
                }
            ],
            "probability_blends": [
                {
                    "name": "safe short identifier, required",
                    "hypothesis": "one falsifiable sentence, required",
                    "market_midpoint_weight": "non-negative number, required",
                    "distance_lob_vol_weight": "non-negative number, required",
                    "event_surface_weight": "non-negative number, required",
                    "existing_model_weight": "non-negative number, required",
                }
            ],
        },
    }
    return json.dumps(payload, indent=2, sort_keys=True)


def validate_response(
    response: Any,
    *,
    allowed_base_factors: set[str] | None = None,
    mutable_scope: set[str] | None = None,
) -> dict[str, list[dict[str, Any]]]:
    """Validate a model response against mission-authorized typed schemas.

    Raises SchemaValidationError with a specific reason on any violation.
    This is the fail-closed boundary: a response that doesn't match is
    never partially accepted.
    """
    if not isinstance(response, dict):
        raise SchemaValidationError("response must be a JSON object")
    unknown_top_level = sorted(set(response) - {"mutations", "probability_blends"})
    if unknown_top_level:
        raise SchemaValidationError(
            "response has unknown fields: " + ", ".join(unknown_top_level)
        )
    mutations = response.get("mutations")
    if not isinstance(mutations, list):
        raise SchemaValidationError("response.mutations must be a list")
    probability_blends = response.get("probability_blends", [])
    if not isinstance(probability_blends, list):
        raise SchemaValidationError("response.probability_blends must be a list")

    granted_scope = (
        mutable_scope
        if mutable_scope is not None
        else FORMULA_MUTABLE_SCOPES | {PROBABILITY_BLEND_MUTABLE_SCOPE}
    )
    if mutations and not granted_scope.intersection(FORMULA_MUTABLE_SCOPES):
        raise SchemaValidationError(
            "mission mutable_scope does not authorize factor-formula mutations"
        )
    if probability_blends and PROBABILITY_BLEND_MUTABLE_SCOPE not in granted_scope:
        raise SchemaValidationError(
            "mission mutable_scope does not authorize probability-blend weights"
        )

    validated_mutations: list[dict[str, Any]] = []
    for index, item in enumerate(mutations):
        if not isinstance(item, dict):
            raise SchemaValidationError(f"mutations[{index}] must be an object")
        unknown = sorted(set(item) - ALL_MUTATION_FIELDS)
        if unknown:
            raise SchemaValidationError(
                f"mutations[{index}] has unknown fields: {', '.join(unknown)}"
            )
        missing = sorted(REQUIRED_MUTATION_FIELDS - set(item))
        if missing:
            raise SchemaValidationError(
                f"mutations[{index}] missing required fields: {', '.join(missing)}"
            )
        base_factor = item.get("base_factor")
        if not isinstance(base_factor, str) or not base_factor.strip():
            raise SchemaValidationError(
                f"mutations[{index}].base_factor must be a non-empty string"
            )
        if (
            allowed_base_factors is not None
            and base_factor not in allowed_base_factors
        ):
            raise SchemaValidationError(
                f"mutations[{index}].base_factor must reference an existing base factor"
            )
        mutation_type = item.get("mutation_type")
        if mutation_type not in ALLOWED_MUTATIONS:
            raise SchemaValidationError(
                f"mutations[{index}].mutation_type {mutation_type!r} is not in "
                f"the allowed set: {', '.join(sorted(ALLOWED_MUTATIONS))}"
            )
        hypothesis = item.get("hypothesis")
        if (
            not isinstance(hypothesis, str)
            or not hypothesis.strip()
            or len(hypothesis) > 500
        ):
            raise SchemaValidationError(
                f"mutations[{index}].hypothesis must be a non-empty string "
                "of at most 500 characters"
            )
        for numeric_field in ("constant", "lo", "hi"):
            if numeric_field in item and (
                isinstance(item[numeric_field], bool)
                or not isinstance(item[numeric_field], (int, float))
                or not math.isfinite(item[numeric_field])
            ):
                raise SchemaValidationError(
                    f"mutations[{index}].{numeric_field} must be numeric"
                )
        if "window" in item and (
            isinstance(item["window"], bool) or not isinstance(item["window"], int)
        ):
            raise SchemaValidationError(f"mutations[{index}].window must be an integer")
        for string_field in ("name", "feature", "denominator_feature"):
            if string_field in item and not isinstance(item[string_field], str):
                raise SchemaValidationError(
                    f"mutations[{index}].{string_field} must be a string"
                )
        validated_mutations.append(item)

    validated_blends: list[dict[str, Any]] = []
    seen_names: set[str] = set()
    for index, item in enumerate(probability_blends):
        if not isinstance(item, dict):
            raise SchemaValidationError(
                f"probability_blends[{index}] must be an object"
            )
        unknown = sorted(set(item) - REQUIRED_PROBABILITY_BLEND_FIELDS)
        if unknown:
            raise SchemaValidationError(
                f"probability_blends[{index}] has unknown fields: {', '.join(unknown)}"
            )
        missing = sorted(REQUIRED_PROBABILITY_BLEND_FIELDS - set(item))
        if missing:
            raise SchemaValidationError(
                f"probability_blends[{index}] missing required fields: "
                + ", ".join(missing)
            )
        name = item.get("name")
        if not isinstance(name, str) or SAFE_CANDIDATE_NAME.fullmatch(name) is None:
            raise SchemaValidationError(
                f"probability_blends[{index}].name must match "
                "[A-Za-z0-9_-]{1,80}"
            )
        if name in seen_names:
            raise SchemaValidationError(
                f"probability_blends[{index}].name is duplicated"
            )
        seen_names.add(name)
        hypothesis = item.get("hypothesis")
        if (
            not isinstance(hypothesis, str)
            or not hypothesis.strip()
            or len(hypothesis) > 500
        ):
            raise SchemaValidationError(
                f"probability_blends[{index}].hypothesis must be a non-empty "
                "string of at most 500 characters"
            )
        total_weight = 0.0
        for field in REQUIRED_PROBABILITY_BLEND_FIELDS - {"name", "hypothesis"}:
            value = item.get(field)
            if (
                isinstance(value, bool)
                or not isinstance(value, (int, float))
                or not math.isfinite(value)
                or value < 0.0
            ):
                raise SchemaValidationError(
                    f"probability_blends[{index}].{field} must be finite and non-negative"
                )
            total_weight += float(value)
        if not math.isfinite(total_weight) or total_weight <= 0.0:
            raise SchemaValidationError(
                f"probability_blends[{index}] must have positive total weight"
            )
        validated_blends.append(item)
    return {
        "mutations": validated_mutations,
        "probability_blends": validated_blends,
    }


def propose_candidates(
    client: LlmClient,
    run: dict[str, Any],
    mission: dict[str, Any],
    alpha_zoo_snapshot: Any = None,
    avoided_subtrees: Any = None,
    mutation_limit: int = 6,
    max_retries: int = MAX_SCHEMA_RETRIES,
) -> dict[str, list[dict[str, Any]]]:
    """Call the client and validate its response, retrying on schema failure.

    Fails soft by design at the caller level (see main()): if every retry is
    exhausted, this raises, and main() must catch that and proceed without a
    fresh LLM prior — identical to today's behavior when
    --alpha-search-llm-prior-json is simply omitted. LLM availability must
    never become a hard gate on the deterministic search path.
    """
    prompt = build_prompt(
        run,
        mission,
        alpha_zoo_snapshot=alpha_zoo_snapshot,
        avoided_subtrees=avoided_subtrees,
        mutation_limit=mutation_limit,
    )
    allowed_base_factors = {
        str(item.get("factor_name") or "").strip()
        for item in weak_dimensions_summary(run.get("plan") or {})
        if str(item.get("factor_name") or "").strip()
    }
    mutable_scope = set(mission["mutable_scope"])
    last_error: SchemaValidationError | None = None
    retry_limit = min(
        max(0, max_retries), mission["search_budget"]["max_llm_calls"] - 1
    )
    for attempt in range(retry_limit + 1):
        response = client.propose(prompt)
        try:
            validated = validate_response(
                response,
                allowed_base_factors=allowed_base_factors,
                mutable_scope=mutable_scope,
            )
            probability_blends = validated["probability_blends"][:mutation_limit]
            mutation_slots = max(0, mutation_limit - len(probability_blends))
            return {
                "mutations": validated["mutations"][:mutation_slots],
                "probability_blends": probability_blends,
            }
        except SchemaValidationError as err:
            last_error = err
            if attempt < retry_limit:
                prompt = (
                    f"{prompt}\n\nYour previous response was rejected: {err}. "
                    "Return a corrected JSON object matching response_schema exactly."
                )
    assert last_error is not None
    raise last_error


def build_prior(
    target: str,
    mission: dict[str, Any],
    proposal: dict[str, list[dict[str, Any]]],
    source_prior: Any = None,
) -> dict[str, Any]:
    """Build a provenance-bound next-llm-prior.json payload.

    Existing mutation and avoidance fields stay compatible with the closed-loop
    agent, while mission provenance and probability blends remain explicit for
    the prediction evaluator.
    """
    source_prior = source_prior if isinstance(source_prior, dict) else {}
    return {
        "schema_version": 1,
        "kind": "typed_llm_prior_draft",
        "source": "alpha_search_llm_propose",
        "target": target,
        "mission_id": mission["mission_id"],
        "data_snapshot_id": mission["data_snapshot_id"],
        "prompt_snapshot_id": mission["prompt_snapshot_id"],
        "search_policy_snapshot_id": mission["search_policy_snapshot_id"],
        "mission": mission,
        "symbols": mission["symbols"],
        "horizon": mission["horizon"],
        "mutations": proposal["mutations"],
        "probability_blends": proposal["probability_blends"],
        "runtime_avoid_factors": source_prior.get("runtime_avoid_factors", []),
        "structural_avoid_signatures": source_prior.get(
            "structural_avoid_signatures", []
        ),
    }


def write_usage_artifact(
    client: LlmClient,
    output_prior_path: Path,
    mutation_count: int,
    probability_blend_count: int,
) -> None:
    usage = getattr(client, "last_usage", None)
    if usage is None:
        return
    payload = {
        "source": "alpha_search_llm_propose",
        "client": client.__class__.__name__,
        "model": getattr(client, "_model", None),
        "mutation_count": mutation_count,
        "probability_blend_count": probability_blend_count,
        "usage": usage,
    }
    usage_path = output_prior_path.with_name("llm-expansion-usage.json")
    usage_path.parent.mkdir(parents=True, exist_ok=True)
    usage_path.write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def main(env: dict[str, str] | None = None) -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("artifact_dir", help="Downloaded alpha-search artifact directory")
    parser.add_argument("--target", default=DEFAULT_TARGET)
    parser.add_argument("--output-prior-json", required=True)
    parser.add_argument("--mutation-limit", type=int, default=6)
    parser.add_argument("--alpha-zoo-snapshot-json")
    parser.add_argument("--mission-json")
    args = parser.parse_args()

    output_path = Path(args.output_prior_json)
    try:
        run = load_artifact(Path(args.artifact_dir), args.target)
        explicit_mission = None
        if args.mission_json:
            explicit_mission = json.loads(
                Path(args.mission_json).read_text(encoding="utf-8")
            )
        mission = resolve_mission(run, explicit_mission, args.target)
        alpha_zoo_snapshot = None
        if args.alpha_zoo_snapshot_json:
            zoo_path = Path(args.alpha_zoo_snapshot_json)
            if zoo_path.exists():
                alpha_zoo_snapshot = json.loads(zoo_path.read_text(encoding="utf-8"))

        alpha_root = run["root"] / "alpha-search" / args.target
        avoided_subtrees_path = alpha_root / "avoided-subtrees.json"
        avoided_subtrees = None
        if avoided_subtrees_path.exists():
            avoided_subtrees = json.loads(
                avoided_subtrees_path.read_text(encoding="utf-8")
            )

        provider_timeout = min(
            60.0,
            max(
                1.0,
                mission["search_budget"]["max_seconds"]
                / mission["search_budget"]["max_llm_calls"],
            ),
        )
        client = client_from_env(
            env if env is not None else dict(os.environ),
            timeout_secs=provider_timeout,
        )
        candidate_limit = min(
            max(1, args.mutation_limit), mission["search_budget"]["max_candidates"]
        )
        proposal = propose_candidates(
            client,
            run,
            mission,
            alpha_zoo_snapshot=alpha_zoo_snapshot,
            avoided_subtrees=avoided_subtrees,
            mutation_limit=candidate_limit,
        )
    except Exception as err:  # noqa: BLE001 - fail soft, never block the search path
        print(f"alpha_search_llm_propose: no LLM prior produced ({err})")
        return

    mutations = proposal["mutations"]
    probability_blends = proposal["probability_blends"]
    write_usage_artifact(
        client,
        output_path,
        len(mutations),
        len(probability_blends),
    )

    if not mutations and not probability_blends:
        print("alpha_search_llm_propose: no LLM prior produced (empty candidate set)")
        return

    prior = build_prior(args.target, mission, proposal, run.get("input_prior"))
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(prior, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(
        "alpha_search_llm_propose: wrote "
        f"{len(mutations)} mutation(s) and {len(probability_blends)} "
        f"probability blend(s) to {output_path}"
    )


if __name__ == "__main__":
    main()
