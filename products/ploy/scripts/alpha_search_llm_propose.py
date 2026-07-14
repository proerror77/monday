#!/usr/bin/env python3
"""Propose the next typed LLM-prior mutation batch via a real model call.

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

The model is asked to produce entries shaped exactly like
`LlmMutationSpec` (crates/ploy-research/src/autofactor.rs:238-255) inside a
`next-llm-prior.json` file compatible with the existing
`--alpha-search-llm-prior-json` flag, so no downstream Rust code needs to
change: `compile_llm_mutation` already validates and compiles whatever
lands there today. The proposal is never trusted blindly — it goes through
the same JSON-schema-shaped validation as a hand-written prior file before
being written out.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
from typing import Any, Protocol

try:
    from alpha_search_closed_loop_agent import (
        ALLOWED_MUTATIONS,
        DIMENSION_TO_MUTATION,
        DEFAULT_TARGET,
        load_artifact,
        selected_nodes,
    )
except ModuleNotFoundError:
    from scripts.alpha_search_closed_loop_agent import (
        ALLOWED_MUTATIONS,
        DIMENSION_TO_MUTATION,
        DEFAULT_TARGET,
        load_artifact,
        selected_nodes,
    )


# Fields accepted by LlmMutationSpec (crates/ploy-research/src/autofactor.rs:238-255).
# Kept in sync by hand with that struct; a mismatch here fails validation loudly
# rather than letting an unknown field silently pass through to the Rust compiler.
REQUIRED_MUTATION_FIELDS = {"base_factor", "mutation_type"}
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
def _mutation_json_schema() -> dict[str, Any]:
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
            }
        },
        "required": ["mutations"],
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
    _mutation_json_schema() — the model cannot return free text that would
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
                            "Propose bounded factor-formula mutations for the "
                            "alpha search loop."
                        ),
                        "input_schema": _mutation_json_schema(),
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
                        "schema": _mutation_json_schema(),
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


def client_from_env(env: dict[str, str]) -> LlmClient:
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
        return AnthropicLlmClient(api_key, model=model or "claude-sonnet-5")
    if provider == "openai":
        return OpenAiLlmClient(api_key, model=model or "gpt-5.5")
    raise RuntimeError(
        f"PLOY_RESEARCH_LLM_PROVIDER={provider!r} is not supported; use "
        "'anthropic' or 'openai'"
    )


class SchemaValidationError(ValueError):
    """Raised when a model response does not match the mutation schema."""


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
                "reward": node.get("reward"),
            }
        )
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
    alpha_zoo_snapshot: Any = None,
    avoided_subtrees: Any = None,
    mutation_limit: int = 6,
) -> str:
    """Build the prompt describing weak dimensions and known-crowded shapes.

    Inputs are the run's own historical artifacts, not external user data,
    so prompt-injection risk is low but not assumed zero: factor names and
    human-written `notes` strings do flow through here. This function only
    embeds structured, already-validated JSON artifact fields (never raw
    free-text notes), which keeps the untrusted-content surface small.
    """
    payload = {
        "task": (
            "Propose up to "
            f"{mutation_limit} bounded factor-formula mutations for the alpha "
            "search loop. Each proposal must pick exactly one mutation_type "
            "from the allowed list and reference an existing base_factor."
        ),
        "target": run.get("target"),
        "allowed_mutation_types": allowed_mutations_description(),
        "weak_dimensions": weak_dimensions_summary(run.get("plan") or {}),
        "crowded_structural_shapes_within_batch": crowded_signatures_summary(
            avoided_subtrees
        ),
        "crowded_root_genes_across_all_history": alpha_zoo_summary(alpha_zoo_snapshot),
        "response_schema": {
            "mutations": [
                {
                    "base_factor": "string, required, must match an existing factor name",
                    "mutation_type": "string, required, one of allowed_mutation_types",
                    "name": "string, optional",
                    "feature": "string, optional",
                    "denominator_feature": "string, optional",
                    "constant": "number, optional",
                    "lo": "number, optional",
                    "hi": "number, optional",
                    "window": "integer, optional",
                }
            ]
        },
    }
    return json.dumps(payload, indent=2, sort_keys=True)


def validate_response(response: Any) -> list[dict[str, Any]]:
    """Validate a model response against the mutation schema.

    Raises SchemaValidationError with a specific reason on any violation.
    This is the fail-closed boundary: a response that doesn't match is
    never partially accepted.
    """
    if not isinstance(response, dict):
        raise SchemaValidationError("response must be a JSON object")
    mutations = response.get("mutations")
    if not isinstance(mutations, list):
        raise SchemaValidationError("response.mutations must be a list")

    validated: list[dict[str, Any]] = []
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
        mutation_type = item.get("mutation_type")
        if mutation_type not in ALLOWED_MUTATIONS:
            raise SchemaValidationError(
                f"mutations[{index}].mutation_type {mutation_type!r} is not in "
                f"the allowed set: {', '.join(sorted(ALLOWED_MUTATIONS))}"
            )
        for numeric_field in ("constant", "lo", "hi"):
            if numeric_field in item and (
                isinstance(item[numeric_field], bool)
                or not isinstance(item[numeric_field], (int, float))
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
        validated.append(item)
    return validated


def propose_mutations(
    client: LlmClient,
    run: dict[str, Any],
    alpha_zoo_snapshot: Any = None,
    avoided_subtrees: Any = None,
    mutation_limit: int = 6,
    max_retries: int = MAX_SCHEMA_RETRIES,
) -> list[dict[str, Any]]:
    """Call the client and validate its response, retrying on schema failure.

    Fails soft by design at the caller level (see main()): if every retry is
    exhausted, this raises, and main() must catch that and proceed without a
    fresh LLM prior — identical to today's behavior when
    --alpha-search-llm-prior-json is simply omitted. LLM availability must
    never become a hard gate on the deterministic search path.
    """
    prompt = build_prompt(
        run,
        alpha_zoo_snapshot=alpha_zoo_snapshot,
        avoided_subtrees=avoided_subtrees,
        mutation_limit=mutation_limit,
    )
    last_error: SchemaValidationError | None = None
    for attempt in range(max_retries + 1):
        response = client.propose(prompt)
        try:
            return validate_response(response)[:mutation_limit]
        except SchemaValidationError as err:
            last_error = err
            if attempt < max_retries:
                prompt = (
                    f"{prompt}\n\nYour previous response was rejected: {err}. "
                    "Return a corrected JSON object matching response_schema exactly."
                )
    assert last_error is not None
    raise last_error


def build_prior_from_mutations(
    target: str, mutations: list[dict[str, Any]]
) -> dict[str, Any]:
    """Build a next-llm-prior.json payload shaped like build_prior()'s output.

    Deliberately matches the schema alpha_search_closed_loop_agent.py's
    build_prior() already produces (schema_version/kind/source/target/
    mutations) so factor_walk_forward_v2.rs's read_llm_prior() and the
    hosted workflow's prior-forwarding logic need no changes to accept
    LLM-sourced mutations alongside deterministic ones.
    """
    return {
        "schema_version": 1,
        "kind": "typed_llm_prior_draft",
        "source": "alpha_search_llm_propose",
        "target": target,
        "mutations": mutations,
        "runtime_avoid_factors": [],
    }


def write_usage_artifact(
    client: LlmClient, output_prior_path: Path, mutation_count: int
) -> None:
    usage = getattr(client, "last_usage", None)
    if usage is None:
        return
    payload = {
        "source": "alpha_search_llm_propose",
        "client": client.__class__.__name__,
        "model": getattr(client, "_model", None),
        "mutation_count": mutation_count,
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
    args = parser.parse_args()

    output_path = Path(args.output_prior_json)
    try:
        run = load_artifact(Path(args.artifact_dir), args.target)
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

        client = client_from_env(env if env is not None else dict(os.environ))
        mutations = propose_mutations(
            client,
            run,
            alpha_zoo_snapshot=alpha_zoo_snapshot,
            avoided_subtrees=avoided_subtrees,
            mutation_limit=max(1, args.mutation_limit),
        )
    except Exception as err:  # noqa: BLE001 - fail soft, never block the search path
        print(f"alpha_search_llm_propose: no LLM prior produced ({err})")
        return

    write_usage_artifact(client, output_path, len(mutations))

    if not mutations:
        print("alpha_search_llm_propose: no LLM prior produced (empty mutation set)")
        return

    prior = build_prior_from_mutations(args.target, mutations)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(prior, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"alpha_search_llm_propose: wrote {len(mutations)} mutation(s) to {output_path}")


if __name__ == "__main__":
    main()
