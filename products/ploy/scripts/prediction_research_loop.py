#!/usr/bin/env python3
"""Bounded, resumable research loop for one PLOY BTC or SOL prediction mission."""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import os
import subprocess
import tempfile
import time
from contextlib import contextmanager
from pathlib import Path
from typing import Any, Callable

try:
    from alpha_search_llm_propose import (
        build_prior,
        client_from_env,
        load_artifact,
        propose_candidates,
        research_brief_snapshot_id,
        validate_mission,
    )
except ModuleNotFoundError:
    from scripts.alpha_search_llm_propose import (
        build_prior,
        client_from_env,
        load_artifact,
        propose_candidates,
        research_brief_snapshot_id,
        validate_mission,
    )


TARGET = "full_depth_settlement_executable_pnl"
PREDICTION_EVENT_WINDOW_SECS = 300
STATE_SCHEMA_VERSION = "ploy_prediction_research_loop.v1"
EVIDENCE_SCHEMA_VERSION = "ploy_prediction_research_iteration.v1"
ARCHIVED_LLM_OUTCOMES = {
    "budget_expired_after_response",
    "empty_proposal",
    "no_probability_blend_candidates",
    "proposal_error",
    "provider_failed",
    "response_rejected",
    "response_missing_after_crash",
    "terminal_before_proposal",
}
ARCHIVED_LLM_WITHOUT_RESPONSE = {"provider_failed", "response_missing_after_crash"}
POST_EVALUATOR_FAILURE_REASONS = {
    "prediction_feedback_invalid",
    "prediction_feedback_missing",
}
PLOY_ROOT = Path(__file__).resolve().parents[1]
LOCK_FILENAME = ".prediction-research-loop.lock"
POLICY_ENTRYPOINT_PATHS = (
    "Cargo.lock",
    "Cargo.toml",
    "rust-toolchain.toml",
    "scripts/alpha_search_closed_loop_agent.py",
    "scripts/alpha_search_llm_propose.py",
    "scripts/prediction_research_loop.py",
    "crates/ploy-research/examples/factor_walk_forward_v2.rs",
)
EVALUATOR_COMMAND = (
    "cargo",
    "run",
    "--quiet",
    "-p",
    "ploy-research",
    "--example",
    "factor_walk_forward_v2",
    "--features",
    "db",
    "--",
)

CommandRunner = Callable[[list[str], Path, float], Any]


class LoopValidationError(ValueError):
    """Raised before work when mission, snapshot, or resume identity is unsafe."""


class BudgetExhausted(RuntimeError):
    def __init__(self, reason: str) -> None:
        super().__init__(reason)
        self.reason = reason


def _read_json(path: Path) -> Any:
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as err:
        raise LoopValidationError(f"cannot read JSON {path}: {err}") from err


def _canonical_bytes(payload: Any) -> bytes:
    return (
        json.dumps(payload, ensure_ascii=False, separators=(",", ":"), sort_keys=True)
        + "\n"
    ).encode()


def _payload_hash(payload: Any) -> str:
    return hashlib.sha256(_canonical_bytes(payload)).hexdigest()


def current_policy_snapshot_id() -> str:
    """Hash the exact proposer/evaluator implementation used by this loop."""
    digest = hashlib.sha256()
    paths = {Path(relative) for relative in POLICY_ENTRYPOINT_PATHS}
    paths.update(
        path.relative_to(PLOY_ROOT)
        for pattern in ("crates/**/*.rs", "crates/**/Cargo.toml")
        for path in PLOY_ROOT.glob(pattern)
        if path.is_file()
    )
    for relative_path in sorted(paths, key=lambda path: path.as_posix()):
        relative = relative_path.as_posix()
        path = PLOY_ROOT / relative
        try:
            body = path.read_bytes()
        except OSError as err:
            raise LoopValidationError(f"cannot read policy source {path}: {err}") from err
        digest.update(relative.encode())
        digest.update(b"\0")
        digest.update(body)
        digest.update(b"\0")
    return f"sha256:{digest.hexdigest()}"


def _write_content_addressed(directory: Path, prefix: str, payload: Any) -> tuple[Path, str]:
    body = _canonical_bytes(payload)
    digest = hashlib.sha256(body).hexdigest()
    path = directory / f"{prefix}-{digest}.json"
    directory.mkdir(parents=True, exist_ok=True)
    if path.exists():
        if path.read_bytes() != body:
            raise LoopValidationError(f"content-address collision at {path}")
    else:
        with path.open("xb") as handle:
            handle.write(body)
    return path, digest


def _write_content_addressed_text(
    directory: Path, prefix: str, body: str
) -> tuple[Path, str]:
    raw = body.encode()
    digest = hashlib.sha256(raw).hexdigest()
    path = directory / f"{prefix}-{digest}.txt"
    directory.mkdir(parents=True, exist_ok=True)
    if path.exists():
        if path.read_bytes() != raw:
            raise LoopValidationError(f"content-address collision at {path}")
    else:
        with path.open("xb") as handle:
            handle.write(raw)
    return path, digest


@contextmanager
def _exclusive_output_lock(output_dir: Path):
    """Prevent concurrent LoopRuns from overwriting one mission state ledger."""
    output_dir.mkdir(parents=True, exist_ok=True)
    lock_path = output_dir / LOCK_FILENAME
    handle = lock_path.open("a+b")
    try:
        try:
            fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as err:
            raise LoopValidationError(
                f"prediction research output is already locked: {output_dir}"
            ) from err
        yield
    finally:
        try:
            fcntl.flock(handle.fileno(), fcntl.LOCK_UN)
        finally:
            handle.close()


def _write_once(path: Path, body: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open("x", encoding="utf-8") as handle:
        handle.write(body)


def _write_state(path: Path, state: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    body = json.dumps(state, indent=2, sort_keys=True) + "\n"
    with tempfile.NamedTemporaryFile(
        "w", encoding="utf-8", dir=path.parent, prefix=".state-", delete=False
    ) as handle:
        handle.write(body)
        handle.flush()
        os.fsync(handle.fileno())
        temporary_path = Path(handle.name)
    os.replace(temporary_path, path)


def _default_command_runner(command: list[str], cwd: Path, timeout: float) -> Any:
    return subprocess.run(
        command,
        cwd=cwd,
        timeout=timeout,
        capture_output=True,
        text=True,
        check=False,
    )


def _snapshot_content_hash(snapshot_dir: Path, manifest: dict[str, Any]) -> str:
    artifacts = manifest.get("artifacts")
    if not isinstance(artifacts, dict):
        raise LoopValidationError("snapshot manifest must contain artifacts")
    digest = hashlib.sha256(_canonical_bytes(manifest))
    for field in (
        "observations_json",
        "deribit_snapshots_json",
        "pm_book_snapshots_json",
    ):
        name = artifacts.get(field)
        if not isinstance(name, str) or not name.strip():
            raise LoopValidationError(f"snapshot manifest artifacts.{field} is required")
        relative = Path(name)
        if relative.is_absolute() or ".." in relative.parts:
            raise LoopValidationError(f"snapshot artifact path {name!r} is unsafe")
        path = snapshot_dir / relative
        try:
            handle = path.open("rb")
        except OSError as err:
            raise LoopValidationError(f"cannot read snapshot artifact {path}: {err}") from err
        digest.update(name.encode())
        with handle:
            while chunk := handle.read(1024 * 1024):
                digest.update(chunk)
    return digest.hexdigest()


def _validate_inputs(
    mission_payload: Any, snapshot_dir: Path
) -> tuple[dict[str, Any], dict[str, Any], str, str, str]:
    try:
        mission = validate_mission(mission_payload, TARGET)
    except ValueError as err:
        raise LoopValidationError(str(err)) from err
    symbol = mission["symbols"][0]
    if symbol not in {"BTC", "SOL"}:
        raise LoopValidationError("prediction loop supports exactly one BTC or SOL mission")
    if mission["mutable_scope"] != ["probability_blend_weights"]:
        raise LoopValidationError(
            "prediction loop mission may mutate only probability_blend_weights"
        )
    policy_snapshot_id = current_policy_snapshot_id()
    if mission["search_policy_snapshot_id"] != policy_snapshot_id:
        raise LoopValidationError(
            "mission.search_policy_snapshot_id does not match the current "
            f"proposer/evaluator implementation; expected {policy_snapshot_id}"
        )

    manifest = _read_json(snapshot_dir / "manifest.json")
    if not isinstance(manifest, dict):
        raise LoopValidationError("snapshot manifest must be a JSON object")
    if manifest.get("schema_version") != "research_snapshot_v1":
        raise LoopValidationError("snapshot manifest has an unsupported schema")
    if manifest.get("immutable_input") is not True:
        raise LoopValidationError("snapshot manifest must declare immutable_input=true")
    snapshot_hash = manifest.get("snapshot_hash")
    if not isinstance(snapshot_hash, str) or not snapshot_hash.strip():
        raise LoopValidationError("snapshot manifest must contain snapshot_hash")
    snapshot_contract_hash = manifest.get("snapshot_contract_hash")
    if (
        not isinstance(snapshot_contract_hash, str)
        or not snapshot_contract_hash.startswith("sha256:")
        or len(snapshot_contract_hash) != 71
        or any(character not in "0123456789abcdef" for character in snapshot_contract_hash[7:])
    ):
        raise LoopValidationError(
            "prediction loop requires a snapshot_contract_hash; regenerate legacy snapshots"
        )
    if mission["data_snapshot_id"] != snapshot_contract_hash:
        raise LoopValidationError(
            "mission.data_snapshot_id does not match snapshot manifest "
            "snapshot_contract_hash"
        )

    manifest_symbols = {
        item.strip().upper()
        for item in manifest.get("symbols", [])
        if isinstance(item, str) and item.strip()
    }
    candidates = (symbol, f"{symbol}USDT")
    evaluator_symbol = next((item for item in candidates if item in manifest_symbols), None)
    if evaluator_symbol is None:
        raise LoopValidationError(
            f"snapshot symbols do not contain the isolated {symbol} mission symbol"
        )
    for field in ("start", "end"):
        if not isinstance(manifest.get(field), str) or not manifest[field].strip():
            raise LoopValidationError(f"snapshot manifest must contain {field}")
    return (
        mission,
        manifest,
        evaluator_symbol,
        _snapshot_content_hash(snapshot_dir, manifest),
        policy_snapshot_id,
    )


def _new_state(
    mission: dict[str, Any],
    data_snapshot_id: str,
    snapshot_content_hash: str,
    policy_snapshot_id: str,
) -> dict[str, Any]:
    return {
        "schema_version": STATE_SCHEMA_VERSION,
        "mission_id": mission["mission_id"],
        "mission_sha256": _payload_hash(mission),
        "symbol": mission["symbols"][0],
        "target": mission["target"],
        "horizon": mission["horizon"],
        "data_snapshot_id": data_snapshot_id,
        "snapshot_content_sha256": snapshot_content_hash,
        "policy_implementation_id": policy_snapshot_id,
        "status": "running",
        "phase": "baseline",
        "stop_reason": None,
        "elapsed_seconds": 0.0,
        "candidates_used": 0,
        "llm_calls_used": 0,
        "next_iteration": 1,
        "baseline_artifact_dir": None,
        "baseline_evidence": None,
        "baseline_evidence_sha256": None,
        "last_artifact_dir": None,
        "pending": None,
        "inflight_llm": None,
        "archived_llm_attempts": [],
        "iterations": [],
        "last_failure": None,
    }


def _load_state(
    path: Path,
    mission: dict[str, Any],
    data_snapshot_id: str,
    snapshot_content_hash: str,
    policy_snapshot_id: str,
) -> dict[str, Any]:
    if not path.exists():
        return _new_state(
            mission, data_snapshot_id, snapshot_content_hash, policy_snapshot_id
        )
    state = _read_json(path)
    if not isinstance(state, dict) or state.get("schema_version") != STATE_SCHEMA_VERSION:
        raise LoopValidationError("existing loop state has an unsupported schema")
    expected = {
        "mission_id": mission["mission_id"],
        "mission_sha256": _payload_hash(mission),
        "symbol": mission["symbols"][0],
        "target": mission["target"],
        "horizon": mission["horizon"],
        "data_snapshot_id": data_snapshot_id,
        "snapshot_content_sha256": snapshot_content_hash,
        "policy_implementation_id": policy_snapshot_id,
    }
    if any(state.get(key) != value for key, value in expected.items()):
        raise LoopValidationError(
            "output directory belongs to a different mission or immutable snapshot"
        )
    return state


def _relative(path: Path, output_dir: Path) -> str:
    try:
        return str(path.resolve().relative_to(output_dir.resolve()))
    except ValueError as err:
        raise LoopValidationError(
            f"loop evidence path resolves outside the output directory: {path}"
        ) from err


def _state_path(output_dir: Path, relative: Any, field: str) -> Path:
    if not isinstance(relative, str) or not relative:
        raise LoopValidationError(f"loop state {field} must be a relative path")
    raw = Path(relative)
    if raw.is_absolute() or ".." in raw.parts:
        raise LoopValidationError(f"loop state {field} escapes the output directory")
    root = output_dir.resolve()
    path = (output_dir / raw).resolve()
    try:
        path.relative_to(root)
    except ValueError as err:
        raise LoopValidationError(
            f"loop state {field} resolves outside the output directory"
        ) from err
    return path


def _verify_file_hash(path: Path, expected: Any, field: str) -> None:
    if not isinstance(expected, str) or len(expected) != 64:
        raise LoopValidationError(f"loop state {field} has an invalid SHA-256")
    if not path.is_file():
        raise LoopValidationError(f"loop evidence is missing: {path}")
    if hashlib.sha256(path.read_bytes()).hexdigest() != expected:
        raise LoopValidationError(f"loop evidence hash mismatch: {path}")


def _validate_evaluator_evidence(
    *,
    output_dir: Path,
    evidence_relative: Any,
    evidence_hash: Any,
    phase: str,
    artifact_dir: Path,
    mission: dict[str, Any],
    prior_path: Path | None,
) -> None:
    evidence_path = _state_path(
        output_dir, evidence_relative, f"{phase}.evaluator_evidence"
    )
    _verify_file_hash(
        evidence_path, evidence_hash, f"{phase}.evaluator_evidence_sha256"
    )
    if evidence_path.name != f"evidence-{evidence_hash}.json":
        raise LoopValidationError("evaluator evidence filename is not content-addressed")
    evidence = _read_json(evidence_path)
    if not isinstance(evidence, dict):
        raise LoopValidationError("evaluator evidence must be a JSON object")
    if any(
        (
            evidence.get("kind") != "evaluator_attempt",
            evidence.get("phase") != phase,
            evidence.get("success") is not True,
            evidence.get("returncode") != 0,
            evidence.get("artifacts_present") is not True,
        )
    ):
        raise LoopValidationError("successful evaluator evidence is inconsistent")
    command = evidence.get("command")
    if not isinstance(command, list) or not all(isinstance(arg, str) for arg in command):
        raise LoopValidationError("evaluator evidence command is invalid")

    def command_value(flag: str) -> str:
        try:
            index = command.index(flag)
            return command[index + 1]
        except (ValueError, IndexError) as err:
            raise LoopValidationError(
                f"evaluator evidence command is missing {flag}"
            ) from err

    if command_value("--event-window-secs") != str(PREDICTION_EVENT_WINDOW_SECS):
        raise LoopValidationError("evaluator evidence used the wrong event horizon")
    command_symbol = command_value("--symbols").upper()
    if command_symbol.removesuffix("USDT") != mission["symbols"][0]:
        raise LoopValidationError("evaluator evidence used another mission symbol")
    expected_artifact_root = (artifact_dir / "alpha-search").resolve()
    if Path(command_value("--alpha-search-output-dir")).resolve() != expected_artifact_root:
        raise LoopValidationError("evaluator evidence artifact binding changed")
    if prior_path is None:
        if "--alpha-search-llm-prior-json" in command:
            raise LoopValidationError("baseline evaluator evidence unexpectedly used a prior")
    elif Path(command_value("--alpha-search-llm-prior-json")).resolve() != prior_path.resolve():
        raise LoopValidationError("evaluator evidence prior binding changed")


def _validate_llm_attempt(
    attempt: Any,
    output_dir: Path,
    mission: dict[str, Any],
    *,
    expected_prompt: str | None = None,
    require_response: bool,
) -> dict[str, Any] | None:
    if not isinstance(attempt, dict):
        raise LoopValidationError("LLM attempt state must be an object")
    for field in ("iteration", "call_number"):
        value = attempt.get(field)
        if isinstance(value, bool) or not isinstance(value, int) or value <= 0:
            raise LoopValidationError(f"LLM attempt {field} must be a positive integer")
    if not isinstance(attempt.get("client"), str) or not attempt["client"]:
        raise LoopValidationError("LLM attempt client must be a non-empty string")
    if attempt.get("model") is not None and not isinstance(attempt["model"], str):
        raise LoopValidationError("LLM attempt model must be a string or null")
    attempt_dir = _state_path(output_dir, attempt.get("attempt_dir"), "llm.attempt_dir")
    if not attempt_dir.is_dir():
        raise LoopValidationError("LLM attempt directory is missing")
    prompt_path = _state_path(output_dir, attempt.get("prompt_path"), "llm.prompt_path")
    prompt_hash = attempt.get("prompt_sha256")
    _verify_file_hash(prompt_path, prompt_hash, "llm.prompt_sha256")
    if prompt_path.parent != attempt_dir or prompt_path.name != f"prompt-{prompt_hash}.txt":
        raise LoopValidationError("LLM prompt is not bound to its attempt")
    if expected_prompt is not None and prompt_path.read_text(encoding="utf-8") != expected_prompt:
        raise LoopValidationError("resumed LLM prompt differs from persisted prompt")

    started_path = _state_path(
        output_dir, attempt.get("started_evidence"), "llm.started_evidence"
    )
    started_hash = attempt.get("started_evidence_sha256")
    _verify_file_hash(started_path, started_hash, "llm.started_evidence_sha256")
    started = _read_json(started_path)
    if not isinstance(started, dict):
        raise LoopValidationError("LLM started evidence must be a JSON object")
    if any(
        started.get(field) != value
        for field, value in {
            "kind": "llm_call_started",
            "mission_id": mission["mission_id"],
            "iteration": attempt.get("iteration"),
            "call_number": attempt.get("call_number"),
            "prompt_path": prompt_path.name,
            "prompt_sha256": prompt_hash,
            "client": attempt.get("client"),
            "model": attempt.get("model"),
        }.items()
    ):
        raise LoopValidationError("LLM started evidence does not match attempt state")

    response_relative = attempt.get("response_path")
    if response_relative is None:
        discovered = sorted(attempt_dir.glob("response-*.json"))
        if len(discovered) > 1:
            raise LoopValidationError("LLM attempt has ambiguous persisted responses")
        if not discovered:
            if require_response:
                raise LoopValidationError("LLM response evidence is missing")
            return None
        response_path = discovered[0]
        response_hash = response_path.stem.removeprefix("response-")
    else:
        response_path = _state_path(
            output_dir, response_relative, "llm.response_path"
        )
        response_hash = attempt.get("response_sha256")
    _verify_file_hash(response_path, response_hash, "llm.response_sha256")
    if (
        response_path.parent != attempt_dir
        or response_path.name != f"response-{response_hash}.json"
    ):
        raise LoopValidationError("LLM response is not bound to its attempt")
    response = _read_json(response_path)
    if not isinstance(response, dict):
        raise LoopValidationError("LLM response must be a JSON object")

    returned_relative = attempt.get("returned_evidence")
    if returned_relative is not None:
        returned_path = _state_path(
            output_dir, returned_relative, "llm.returned_evidence"
        )
        returned_hash = attempt.get("returned_evidence_sha256")
        _verify_file_hash(returned_path, returned_hash, "llm.returned_evidence_sha256")
        returned = _read_json(returned_path)
        if not isinstance(returned, dict):
            raise LoopValidationError("LLM returned evidence must be a JSON object")
        if any(
            returned.get(field) != value
            for field, value in {
                "mission_id": mission["mission_id"],
                "iteration": attempt.get("iteration"),
                "call_number": attempt.get("call_number"),
                "response_path": response_path.name,
                "response_sha256": response_hash,
                "client": attempt.get("client"),
                "model": attempt.get("model"),
            }.items()
        ) or returned.get("kind") not in {"llm_call_returned", "llm_call_recovered"}:
            raise LoopValidationError("LLM returned evidence does not match attempt state")
    elif require_response:
        raise LoopValidationError("LLM returned evidence is missing")
    return response


def _attempt_dir(parent: Path, prefix: str) -> Path:
    parent.mkdir(parents=True, exist_ok=True)
    number = 1
    while (candidate := parent / f"{prefix}-{number:04d}").exists():
        number += 1
    candidate.mkdir()
    return candidate


def _manifest_arg(manifest: dict[str, Any], key: str, default: Any) -> str:
    value = manifest.get(key, default)
    if isinstance(value, bool) or not isinstance(value, (int, float)) or value <= 0:
        raise LoopValidationError(f"snapshot manifest {key} must be positive")
    return str(value)


def _evaluator_command(
    snapshot_dir: Path,
    manifest: dict[str, Any],
    symbol: str,
    artifact_dir: Path,
    prior_path: Path | None,
) -> list[str]:
    command = [
        *EVALUATOR_COMMAND,
        "--snapshot-dir",
        str(snapshot_dir.resolve()),
        "--start-ts",
        manifest["start"],
        "--end-ts",
        manifest["end"],
        "--symbols",
        symbol,
        "--event-window-secs",
        str(PREDICTION_EVENT_WINDOW_SECS),
        "--lob-sample-secs",
        _manifest_arg(manifest, "lob_sample_secs", 30),
        "--pm-book-sample-secs",
        _manifest_arg(
            manifest,
            "pm_book_sample_secs",
            manifest.get("lob_sample_secs", 30),
        ),
        "--observation-sample-secs",
        _manifest_arg(manifest, "observation_sample_secs", 30),
        "--max-quote-age-secs",
        _manifest_arg(manifest, "max_quote_age_secs", 30),
        "--stake-usd",
        _manifest_arg(manifest, "stake_usd", 15.0),
        "--report-suite",
        "core",
        "--alpha-search-output-dir",
        str(artifact_dir / "alpha-search"),
    ]
    if prior_path is not None:
        command.extend(["--alpha-search-llm-prior-json", str(prior_path)])
    return command


def _run_evaluator(
    *,
    phase: str,
    attempt_parent: Path,
    snapshot_dir: Path,
    manifest: dict[str, Any],
    symbol: str,
    prior_path: Path | None,
    timeout: float,
    command_runner: CommandRunner,
    clock: Callable[[], float],
) -> dict[str, Any]:
    attempt_dir = _attempt_dir(attempt_parent, "attempt")
    artifact_dir = attempt_dir / "artifacts"
    command = _evaluator_command(snapshot_dir, manifest, symbol, artifact_dir, prior_path)
    started = clock()
    try:
        result = command_runner(command, PLOY_ROOT, timeout)
        returncode = int(result.returncode)
        stdout = result.stdout if isinstance(result.stdout, str) else ""
        stderr = result.stderr if isinstance(result.stderr, str) else ""
        _write_once(attempt_dir / "stdout.txt", stdout)
        _write_once(attempt_dir / "stderr.txt", stderr)
        error_type = None
        timed_out = False
    except subprocess.TimeoutExpired:
        returncode = None
        error_type = "TimeoutExpired"
        timed_out = True
    except Exception as err:  # noqa: BLE001 - persisted and resumable subprocess boundary
        returncode = None
        error_type = type(err).__name__
        timed_out = False

    artifacts_present = (artifact_dir / "alpha-search" / TARGET).is_dir()
    success = returncode == 0 and artifacts_present
    evidence = {
        "schema_version": EVIDENCE_SCHEMA_VERSION,
        "kind": "evaluator_attempt",
        "phase": phase,
        "command": command,
        "timeout_seconds": timeout,
        "elapsed_seconds": max(0.0, clock() - started),
        "returncode": returncode,
        "error_type": error_type,
        "timed_out": timed_out,
        "artifacts_present": artifacts_present,
        "success": success,
    }
    evidence_path, evidence_hash = _write_content_addressed(
        attempt_dir, "evidence", evidence
    )
    return {
        "success": success,
        "timed_out": timed_out,
        "artifact_dir": artifact_dir,
        "evidence_path": evidence_path,
        "evidence_sha256": evidence_hash,
        "failure": (
            None
            if success
            else "evaluator_artifacts_missing"
            if returncode == 0
            else f"process_exit_{returncode}"
            if returncode is not None
            else error_type or "evaluator_error"
        ),
    }


def _find_feedback(artifact_dir: Path) -> tuple[dict[str, Any] | None, Path | None]:
    alpha_root = artifact_dir / "alpha-search" / TARGET
    paths = []
    fixed = alpha_root / "prediction-research-feedback.json"
    if fixed.exists():
        paths.append(fixed)
    paths.extend(sorted(alpha_root.glob("prediction-research-feedback-*.json")))
    if not paths:
        return None, None
    if len(paths) != 1:
        raise LoopValidationError("evaluator produced ambiguous prediction feedback")
    path = paths[0]
    if path != fixed:
        expected_hash = path.stem.removeprefix("prediction-research-feedback-")
        actual_hash = hashlib.sha256(path.read_bytes()).hexdigest()
        if expected_hash != actual_hash:
            raise LoopValidationError("prediction feedback content hash does not match filename")
    payload = _read_json(path)
    if not isinstance(payload, dict):
        raise LoopValidationError("prediction feedback must be a JSON object")
    return payload, path


def _feedback_artifact_snapshot(artifact_dir: Path) -> list[dict[str, str]]:
    alpha_root = artifact_dir / "alpha-search" / TARGET
    paths = []
    fixed = alpha_root / "prediction-research-feedback.json"
    if fixed.exists():
        paths.append(fixed)
    paths.extend(sorted(alpha_root.glob("prediction-research-feedback-*.json")))
    snapshot = []
    for path in paths:
        try:
            raw = path.read_bytes()
        except OSError as err:
            raise LoopValidationError(
                f"cannot snapshot prediction feedback artifact {path}: {err}"
            ) from err
        snapshot.append(
            {
                "path": str(path.relative_to(artifact_dir)),
                "sha256": hashlib.sha256(raw).hexdigest(),
            }
        )
    return snapshot


def _validate_feedback(
    feedback: dict[str, Any],
    mission: dict[str, Any],
    prior: dict[str, Any] | None = None,
) -> list[str]:
    if feedback.get("schema_version") != "prediction_research_feedback.v1":
        raise LoopValidationError("prediction feedback has an unsupported schema")
    expected = {
        "mission_id": mission["mission_id"],
        "target": mission["target"],
        "symbols": mission["symbols"],
        "horizon": mission["horizon"],
        "data_snapshot_id": mission["data_snapshot_id"],
        "prompt_snapshot_id": mission["prompt_snapshot_id"],
        "search_policy_snapshot_id": mission["search_policy_snapshot_id"],
    }
    if any(feedback.get(key) != value for key, value in expected.items()):
        raise LoopValidationError("prediction feedback provenance does not match mission")
    candidates = feedback.get("candidates")
    if not isinstance(candidates, list) or not candidates:
        raise LoopValidationError("prediction feedback contains no evaluated candidates")
    if prior is not None:
        blends = prior.get("probability_blends")
        if not isinstance(blends, list) or not blends:
            raise LoopValidationError("pending prior contains no probability blends")
        expected_candidates = {
            f"q_llm_{blend.get('name')}": blend
            for blend in blends
            if isinstance(blend, dict) and isinstance(blend.get("name"), str)
        }
        actual_models = [
            candidate.get("model")
            for candidate in candidates
            if isinstance(candidate, dict)
        ]
        if (
            len(expected_candidates) != len(blends)
            or len(actual_models) != len(candidates)
            or any(not isinstance(model, str) or not model for model in actual_models)
        ):
            raise LoopValidationError("prediction feedback candidate set differs from prior")
        if len(set(actual_models)) != len(actual_models) or set(
            actual_models
        ) != set(expected_candidates):
            raise LoopValidationError("prediction feedback candidate set differs from prior")
        for candidate in candidates:
            model = candidate["model"]
            expected_blend = expected_candidates[model]
            if (
                candidate.get("probability_blend") != expected_blend
                or candidate.get("hypothesis")
                != str(expected_blend.get("hypothesis") or "").strip()
                or candidate.get("verdict") not in {"keep", "discard"}
            ):
                raise LoopValidationError(
                    "prediction feedback candidate does not match pending prior"
                )
    return [
        str(candidate.get("model"))
        for candidate in candidates
        if isinstance(candidate, dict) and candidate.get("verdict") == "keep"
    ]


def _validate_state_evidence(
    state: dict[str, Any], output_dir: Path, mission: dict[str, Any]
) -> None:
    """Revalidate every state reference before resume or terminal readback."""
    seen_llm_calls: set[int] = set()
    llm_attempt_iterations: dict[int, int] = {}
    accounted_candidates = 0

    def validate_llm_attempt_once(
        attempt: Any, *, require_response: bool
    ) -> dict[str, Any] | None:
        response = _validate_llm_attempt(
            attempt,
            output_dir,
            mission,
            require_response=require_response,
        )
        call_number = attempt["call_number"]
        if call_number in seen_llm_calls:
            raise LoopValidationError("LLM call appears more than once in loop state")
        seen_llm_calls.add(call_number)
        llm_attempt_iterations[call_number] = attempt["iteration"]
        return response

    iterations = state.get("iterations")
    if not isinstance(iterations, list):
        raise LoopValidationError("loop state iterations must be a list")
    seen_iterations: set[int] = set()
    for record in iterations:
        if not isinstance(record, dict):
            raise LoopValidationError("loop state iteration must be an object")
        iteration = record.get("iteration")
        if (
            isinstance(iteration, bool)
            or not isinstance(iteration, int)
            or iteration <= 0
            or iteration in seen_iterations
        ):
            raise LoopValidationError("loop state has an invalid iteration sequence")
        seen_iterations.add(iteration)
        prior_hash = record.get("prior_sha256")
        prior_relative = record.get(
            "prior_path", f"iterations/{iteration:04d}/prior-{prior_hash}.json"
        )
        prior_path = _state_path(output_dir, prior_relative, "iteration.prior_path")
        _verify_file_hash(prior_path, prior_hash, "iteration.prior_sha256")
        if prior_path.name != f"prior-{prior_hash}.json":
            raise LoopValidationError("iteration prior filename is not content-addressed")
        prior = _read_json(prior_path)
        if not isinstance(prior, dict):
            raise LoopValidationError("iteration prior must be a JSON object")
        for field in (
            "mission_id",
            "target",
            "symbols",
            "horizon",
            "data_snapshot_id",
            "prompt_snapshot_id",
            "search_policy_snapshot_id",
        ):
            if prior.get(field) != mission[field]:
                raise LoopValidationError("iteration prior provenance does not match mission")
        if prior.get("mutations") not in ([], None):
            raise LoopValidationError("prediction iteration prior contains factor mutations")
        probability_blends = prior.get("probability_blends")
        candidate_count = record.get("candidate_count")
        if (
            not isinstance(probability_blends, list)
            or not probability_blends
            or isinstance(candidate_count, bool)
            or not isinstance(candidate_count, int)
            or candidate_count != len(probability_blends)
        ):
            raise LoopValidationError("iteration candidate count does not match its prior")
        accounted_candidates += candidate_count
        validate_llm_attempt_once(
            record.get("llm_attempt"), require_response=True
        )
        proposal_path = _state_path(
            output_dir,
            record.get("proposal_evidence"),
            "iteration.proposal_evidence",
        )
        proposal_hash = record.get("proposal_evidence_sha256")
        _verify_file_hash(
            proposal_path, proposal_hash, "iteration.proposal_evidence_sha256"
        )
        proposal = _read_json(proposal_path)
        if not isinstance(proposal, dict):
            raise LoopValidationError("iteration proposal evidence must be a JSON object")
        expected_proposal = {
            "kind": "proposal",
            "mission_id": mission["mission_id"],
            "iteration": iteration,
            "prior_sha256": prior_hash,
            "candidate_count": record.get("candidate_count"),
            "llm_attempt": record.get("llm_attempt"),
            "llm_call_lineage": record.get("llm_call_lineage"),
        }
        if any(proposal.get(key) != value for key, value in expected_proposal.items()):
            raise LoopValidationError("iteration proposal evidence does not match state")

        artifact_dir = _state_path(
            output_dir, record.get("artifact_dir"), "iteration.artifact_dir"
        )
        if not artifact_dir.is_dir():
            raise LoopValidationError("iteration evaluator artifact directory is missing")
        _validate_evaluator_evidence(
            output_dir=output_dir,
            evidence_relative=record.get("evaluator_evidence"),
            evidence_hash=record.get("evaluator_evidence_sha256"),
            phase="evaluate",
            artifact_dir=artifact_dir,
            mission=mission,
            prior_path=prior_path,
        )
        feedback_relative = record.get("feedback_path")
        if feedback_relative is None:
            feedback, feedback_path = _find_feedback(artifact_dir)
            if feedback is None or feedback_path is None:
                raise LoopValidationError("iteration prediction feedback is missing")
        else:
            feedback_path = _state_path(
                output_dir, feedback_relative, "iteration.feedback_path"
            )
            if not feedback_path.is_file():
                raise LoopValidationError("iteration prediction feedback is missing")
            feedback = _read_json(feedback_path)
            if not isinstance(feedback, dict):
                raise LoopValidationError("iteration feedback must be a JSON object")
        _verify_file_hash(
            feedback_path,
            record.get("feedback_sha256"),
            "iteration.feedback_sha256",
        )
        kept_models = _validate_feedback(feedback, mission, prior)
        if record.get("kept_models") != kept_models:
            raise LoopValidationError("iteration kept-model decision changed")

        decision_path = _state_path(
            output_dir,
            record.get("decision_evidence"),
            "iteration.decision_evidence",
        )
        decision_hash = record.get("decision_evidence_sha256")
        _verify_file_hash(
            decision_path, decision_hash, "iteration.decision_evidence_sha256"
        )
        if decision_path.name != f"evidence-{decision_hash}.json":
            raise LoopValidationError("decision evidence filename is not content-addressed")
        decision = _read_json(decision_path)
        if not isinstance(decision, dict):
            raise LoopValidationError("decision evidence must be a JSON object")
        expected_decision = {
            "kind": "deterministic_decision",
            "mission_id": mission["mission_id"],
            "iteration": iteration,
            "prior_sha256": prior_hash,
            "feedback_sha256": record.get("feedback_sha256"),
            "kept_models": kept_models,
            "decision": "keep" if kept_models else "continue",
        }
        if any(decision.get(key) != value for key, value in expected_decision.items()):
            raise LoopValidationError("deterministic decision evidence does not match state")

    if [record.get("iteration") for record in iterations] != list(
        range(1, len(iterations) + 1)
    ):
        raise LoopValidationError("loop iteration history is not contiguous")

    pending = state.get("pending")
    if pending is not None:
        if not isinstance(pending, dict):
            raise LoopValidationError("loop pending prior must be an object")
        if pending.get("iteration") != len(iterations) + 1:
            raise LoopValidationError("pending prior is not the next loop iteration")
        prior_path = _state_path(
            output_dir, pending.get("prior_path"), "pending.prior_path"
        )
        prior_hash = pending.get("prior_sha256")
        _verify_file_hash(prior_path, prior_hash, "pending.prior_sha256")
        if prior_path.name != f"prior-{prior_hash}.json":
            raise LoopValidationError("pending prior filename is not content-addressed")
        prior = _read_json(prior_path)
        if not isinstance(prior, dict):
            raise LoopValidationError("pending prior must be a JSON object")
        for field in (
            "mission_id",
            "target",
            "symbols",
            "horizon",
            "data_snapshot_id",
            "prompt_snapshot_id",
            "search_policy_snapshot_id",
        ):
            if prior.get(field) != mission[field]:
                raise LoopValidationError("pending prior provenance does not match mission")
        if prior.get("mutations") not in ([], None):
            raise LoopValidationError("pending prediction prior contains factor mutations")
        probability_blends = prior.get("probability_blends")
        candidate_count = pending.get("candidate_count")
        if (
            not isinstance(probability_blends, list)
            or not probability_blends
            or isinstance(candidate_count, bool)
            or not isinstance(candidate_count, int)
            or candidate_count != len(probability_blends)
        ):
            raise LoopValidationError("pending candidate count does not match its prior")
        accounted_candidates += candidate_count
        validate_llm_attempt_once(
            pending.get("llm_attempt"), require_response=True
        )
        proposal_path = _state_path(
            output_dir,
            pending.get("proposal_evidence"),
            "pending.proposal_evidence",
        )
        proposal_hash = pending.get("proposal_evidence_sha256")
        _verify_file_hash(
            proposal_path, proposal_hash, "pending.proposal_evidence_sha256"
        )
        proposal = _read_json(proposal_path)
        if not isinstance(proposal, dict):
            raise LoopValidationError("proposal evidence must be a JSON object")
        expected_proposal = {
            "kind": "proposal",
            "mission_id": mission["mission_id"],
            "iteration": pending.get("iteration"),
            "prior_sha256": prior_hash,
            "candidate_count": pending.get("candidate_count"),
            "llm_attempt": pending.get("llm_attempt"),
            "llm_call_lineage": pending.get("llm_call_lineage"),
        }
        if any(proposal.get(key) != value for key, value in expected_proposal.items()):
            raise LoopValidationError("proposal evidence does not match pending state")

    inflight = state.get("inflight_llm")
    if inflight is not None:
        if state.get("phase") != "propose" or pending is not None:
            raise LoopValidationError("in-flight LLM attempt is in an invalid phase")
        if not isinstance(inflight, dict) or inflight.get("iteration") != len(iterations) + 1:
            raise LoopValidationError("in-flight LLM attempt is not the next iteration")
        validate_llm_attempt_once(inflight, require_response=False)

    archived_attempts = state.get("archived_llm_attempts")
    if not isinstance(archived_attempts, list):
        raise LoopValidationError("archived LLM attempts must be a list")
    for archived in archived_attempts:
        if not isinstance(archived, dict):
            raise LoopValidationError("archived LLM attempt must be an object")
        outcome = archived.get("outcome")
        if outcome not in ARCHIVED_LLM_OUTCOMES:
            raise LoopValidationError("archived LLM attempt has an invalid outcome")
        validate_llm_attempt_once(
            archived.get("attempt"),
            require_response=outcome not in ARCHIVED_LLM_WITHOUT_RESPONSE,
        )
        failure_relative = archived.get("failure_evidence")
        if outcome == "provider_failed":
            failure_path = _state_path(
                output_dir, failure_relative, "archived_llm.failure_evidence"
            )
            failure_hash = archived.get("failure_evidence_sha256")
            _verify_file_hash(
                failure_path,
                failure_hash,
                "archived_llm.failure_evidence_sha256",
            )
            failure = _read_json(failure_path)
            if (
                not isinstance(failure, dict)
                or failure.get("kind") != "llm_call_failed"
                or failure.get("mission_id") != mission["mission_id"]
                or failure.get("iteration") != archived["attempt"]["iteration"]
                or failure.get("call_number") != archived["attempt"]["call_number"]
            ):
                raise LoopValidationError("archived LLM failure evidence is inconsistent")
        elif failure_relative is not None:
            raise LoopValidationError("non-failed archived LLM attempt has failure evidence")

    llm_calls_used = state.get("llm_calls_used")
    if (
        isinstance(llm_calls_used, bool)
        or not isinstance(llm_calls_used, int)
        or llm_calls_used < 0
        or llm_calls_used > mission["search_budget"]["max_llm_calls"]
        or seen_llm_calls != set(range(1, llm_calls_used + 1))
    ):
        raise LoopValidationError("LLM call ledger does not match persisted attempts")
    candidates_used = state.get("candidates_used")
    if (
        isinstance(candidates_used, bool)
        or not isinstance(candidates_used, int)
        or candidates_used != accounted_candidates
        or candidates_used > mission["search_budget"]["max_candidates"]
    ):
        raise LoopValidationError("candidate budget ledger does not match persisted priors")
    proposal_records = [*iterations]
    if pending is not None:
        proposal_records.append(pending)
    for record in proposal_records:
        iteration = record["iteration"]
        expected_lineage = sorted(
            call_number
            for call_number, attempt_iteration in llm_attempt_iterations.items()
            if attempt_iteration == iteration
        )
        if record.get("llm_call_lineage") != expected_lineage:
            raise LoopValidationError("proposal LLM lineage is incomplete")

    baseline_artifact_relative = state.get("baseline_artifact_dir")
    if baseline_artifact_relative is not None:
        baseline_artifact_dir = _state_path(
            output_dir, baseline_artifact_relative, "baseline_artifact_dir"
        )
        if not baseline_artifact_dir.is_dir():
            raise LoopValidationError("baseline evaluator artifact directory is missing")
        _validate_evaluator_evidence(
            output_dir=output_dir,
            evidence_relative=state.get("baseline_evidence"),
            evidence_hash=state.get("baseline_evidence_sha256"),
            phase="baseline",
            artifact_dir=baseline_artifact_dir,
            mission=mission,
            prior_path=None,
        )
    elif state.get("phase") != "baseline":
        stopped_during_baseline = (
            state.get("status") == "stopped"
            and state.get("stop_reason") == "budget_max_seconds"
            and not iterations
            and pending is None
            and isinstance(state.get("last_failure"), dict)
        )
        if not stopped_during_baseline:
            raise LoopValidationError("loop state is missing successful baseline evidence")

    last_failure = state.get("last_failure")
    requires_post_evaluator_evidence = state.get("stop_reason") in (
        POST_EVALUATOR_FAILURE_REASONS
    )
    if requires_post_evaluator_evidence:
        if (
            not isinstance(last_failure, dict)
            or last_failure.get("reason") != state.get("stop_reason")
            or not isinstance(last_failure.get("evidence"), str)
            or not isinstance(last_failure.get("evidence_sha256"), str)
            or not isinstance(last_failure.get("artifact_dir"), str)
            or not isinstance(last_failure.get("feedback_artifacts"), list)
        ):
            raise LoopValidationError(
                "post-evaluator terminal failure is missing required evidence"
            )
    if isinstance(last_failure, dict) and last_failure.get("evidence") is not None:
        failure_path = _state_path(
            output_dir, last_failure.get("evidence"), "last_failure.evidence"
        )
        _verify_file_hash(
            failure_path,
            last_failure.get("evidence_sha256"),
            "last_failure.evidence_sha256",
        )
        failed_artifact_relative = last_failure.get("artifact_dir")
        if failed_artifact_relative is not None:
            if pending is None:
                raise LoopValidationError(
                    "post-evaluator failure has no pending prior"
                )
            failed_artifact_dir = _state_path(
                output_dir,
                failed_artifact_relative,
                "last_failure.artifact_dir",
            )
            if not failed_artifact_dir.is_dir():
                raise LoopValidationError(
                    "post-evaluator failure artifact directory is missing"
                )
            failed_prior_path = _state_path(
                output_dir, pending.get("prior_path"), "pending.prior_path"
            )
            _validate_evaluator_evidence(
                output_dir=output_dir,
                evidence_relative=last_failure.get("evidence"),
                evidence_hash=last_failure.get("evidence_sha256"),
                phase="evaluate",
                artifact_dir=failed_artifact_dir,
                mission=mission,
                prior_path=failed_prior_path,
            )
            recorded_feedback_artifacts = last_failure["feedback_artifacts"]
            current_feedback_artifacts = _feedback_artifact_snapshot(
                failed_artifact_dir
            )
            if recorded_feedback_artifacts != current_feedback_artifacts:
                raise LoopValidationError(
                    "post-evaluator feedback artifact snapshot changed"
                )
            if state["stop_reason"] == "prediction_feedback_missing":
                try:
                    failed_feedback, failed_feedback_path = _find_feedback(
                        failed_artifact_dir
                    )
                except LoopValidationError as err:
                    raise LoopValidationError(
                        "feedback missing failure changed into another outcome"
                    ) from err
                if failed_feedback is not None or failed_feedback_path is not None:
                    raise LoopValidationError(
                        "feedback missing failure is no longer reproducible"
                    )
            else:
                invalid_feedback_reproduced = False
                try:
                    failed_feedback, _ = _find_feedback(failed_artifact_dir)
                    if failed_feedback is not None:
                        failed_prior = _read_json(failed_prior_path)
                        if not isinstance(failed_prior, dict):
                            raise LoopValidationError(
                                "post-evaluator failure prior is invalid"
                            )
                        _validate_feedback(failed_feedback, mission, failed_prior)
                except LoopValidationError:
                    invalid_feedback_reproduced = True
                if not invalid_feedback_reproduced:
                    raise LoopValidationError(
                        "feedback invalid failure is no longer reproducible"
                    )
    elif requires_post_evaluator_evidence:
        raise LoopValidationError(
            "post-evaluator terminal failure is missing required evidence"
        )

    last_artifact = state.get("last_artifact_dir")
    if last_artifact is not None:
        artifact_path = _state_path(
            output_dir, last_artifact, "last_artifact_dir"
        )
        if not artifact_path.is_dir():
            raise LoopValidationError("last evaluator artifact directory is missing")
    expected_last_artifact = (
        iterations[-1].get("artifact_dir")
        if iterations
        else state.get("baseline_artifact_dir")
    )
    if last_artifact != expected_last_artifact:
        raise LoopValidationError("last evaluator artifact is not the state frontier")
    if state.get("next_iteration") != len(iterations) + 1:
        raise LoopValidationError("next_iteration does not match iteration history")

    phase = state.get("phase")
    status = state.get("status")
    if status not in {"running", "paused", "completed", "stopped", "failed"}:
        raise LoopValidationError("loop state has an invalid status")
    if phase not in {"baseline", "propose", "evaluate", "done"}:
        raise LoopValidationError("loop state has an invalid phase")
    if (status in {"completed", "stopped", "failed"}) != (phase == "done"):
        raise LoopValidationError("terminal loop status and done phase are inconsistent")
    if phase == "evaluate" and pending is None:
        raise LoopValidationError("evaluate phase has no pending prior")
    if phase in {"baseline", "propose"} and pending is not None:
        raise LoopValidationError("pending prior is present outside evaluate phase")

    if state.get("status") == "completed":
        if not iterations or not iterations[-1].get("kept_models"):
            raise LoopValidationError("completed loop has no verified keep decision")
        if pending is not None or inflight is not None:
            raise LoopValidationError("completed loop retains unfinished work")


def run_prediction_research_loop(
    mission_payload: Any,
    snapshot_dir: Path,
    output_dir: Path,
    *,
    client: Any | None = None,
    command_runner: CommandRunner = _default_command_runner,
    clock: Callable[[], float] = time.monotonic,
    env: dict[str, str] | None = None,
) -> dict[str, Any]:
    """Run one mission while holding exclusive ownership of its state ledger."""
    output_dir = Path(output_dir)
    with _exclusive_output_lock(output_dir):
        return _run_prediction_research_loop_locked(
            mission_payload,
            snapshot_dir,
            output_dir,
            client=client,
            command_runner=command_runner,
            clock=clock,
            env=env,
        )


def _run_prediction_research_loop_locked(
    mission_payload: Any,
    snapshot_dir: Path,
    output_dir: Path,
    *,
    client: Any | None = None,
    command_runner: CommandRunner = _default_command_runner,
    clock: Callable[[], float] = time.monotonic,
    env: dict[str, str] | None = None,
) -> dict[str, Any]:
    """Run or resume one bounded mission without importing any execution path."""
    snapshot_dir = Path(snapshot_dir)
    output_dir = Path(output_dir)
    (
        mission,
        manifest,
        evaluator_symbol,
        snapshot_content_hash,
        policy_snapshot_id,
    ) = _validate_inputs(mission_payload, snapshot_dir)
    state_path = output_dir / "state.json"
    non_lock_entries = [path for path in output_dir.iterdir() if path.name != LOCK_FILENAME]
    if not state_path.exists() and non_lock_entries:
        raise LoopValidationError("non-empty output directory has no resumable state")
    state = _load_state(
        state_path,
        mission,
        manifest["snapshot_contract_hash"],
        snapshot_content_hash,
        policy_snapshot_id,
    )
    _validate_state_evidence(state, output_dir, mission)
    if (
        state.get("status") not in {"completed", "stopped", "failed"}
        and state.get("iterations")
        and state["iterations"][-1].get("kept_models")
    ):
        state["status"] = "completed"
        state["stop_reason"] = "deterministic_keep"
        state["phase"] = "done"
        _write_state(state_path, state)
        _validate_state_evidence(state, output_dir, mission)
        return state
    if state.get("status") in {"completed", "stopped", "failed"}:
        return state

    budget = mission["search_budget"]
    base_elapsed = float(state.get("elapsed_seconds") or 0.0)
    session_started = clock()

    def elapsed() -> float:
        return base_elapsed + max(0.0, clock() - session_started)

    def persist() -> None:
        state["elapsed_seconds"] = round(elapsed(), 6)
        _write_state(state_path, state)

    def finish(status: str, reason: str) -> dict[str, Any]:
        if status in {"completed", "stopped", "failed"} and state.get(
            "inflight_llm"
        ) is not None:
            response = _validate_llm_attempt(
                state["inflight_llm"],
                output_dir,
                mission,
                require_response=False,
            )
            if response is None:
                archive_inflight_llm("response_missing_after_crash")
            else:
                hydrate_inflight_llm(state["inflight_llm"])
                archive_inflight_llm(
                    "budget_expired_after_response"
                    if reason == "budget_max_seconds"
                    else "terminal_before_proposal"
                )
        state["status"] = status
        state["stop_reason"] = reason
        if status in {"completed", "stopped", "failed"}:
            state["phase"] = "done"
        persist()
        return state

    def budget_reason() -> str | None:
        if elapsed() >= budget["max_seconds"]:
            return "budget_max_seconds"
        if state["candidates_used"] >= budget["max_candidates"]:
            return "budget_max_candidates"
        if state["llm_calls_used"] >= budget["max_llm_calls"]:
            return "budget_max_llm_calls"
        return None

    def hydrate_inflight_llm(attempt: dict[str, Any]) -> None:
        """Bind a durably written response discovered after process interruption."""
        attempt_dir = _state_path(
            output_dir, attempt["attempt_dir"], "llm.attempt_dir"
        )
        if attempt.get("response_path") is None:
            response_paths = sorted(attempt_dir.glob("response-*.json"))
            if len(response_paths) != 1:
                raise LoopValidationError(
                    "cannot hydrate an LLM attempt without exactly one response"
                )
            response_path = response_paths[0]
            attempt["response_path"] = _relative(response_path, output_dir)
            attempt["response_sha256"] = response_path.stem.removeprefix("response-")
        else:
            response_path = _state_path(
                output_dir, attempt["response_path"], "llm.response_path"
            )
        if attempt.get("returned_evidence") is None:
            returned_path, returned_hash = _write_content_addressed(
                attempt_dir,
                "evidence",
                {
                    "schema_version": EVIDENCE_SCHEMA_VERSION,
                    "kind": "llm_call_recovered",
                    "mission_id": mission["mission_id"],
                    "iteration": attempt["iteration"],
                    "call_number": attempt["call_number"],
                    "prompt_path": Path(attempt["prompt_path"]).name,
                    "prompt_sha256": attempt["prompt_sha256"],
                    "response_path": response_path.name,
                    "response_sha256": attempt["response_sha256"],
                    "client": attempt["client"],
                    "model": attempt["model"],
                    "usage": None,
                },
            )
            attempt["returned_evidence"] = _relative(returned_path, output_dir)
            attempt["returned_evidence_sha256"] = returned_hash
        state["inflight_llm"] = attempt
        persist()

    def archive_inflight_llm(
        outcome: str,
        *,
        failure_evidence: Path | None = None,
        failure_evidence_sha256: str | None = None,
    ) -> None:
        if outcome not in ARCHIVED_LLM_OUTCOMES:
            raise LoopValidationError(f"cannot archive unknown LLM outcome {outcome}")
        attempt = state.get("inflight_llm")
        if not isinstance(attempt, dict):
            raise LoopValidationError("cannot archive a missing in-flight LLM attempt")
        _validate_llm_attempt(
            attempt,
            output_dir,
            mission,
            require_response=outcome not in ARCHIVED_LLM_WITHOUT_RESPONSE,
        )
        archived = {"outcome": outcome, "attempt": dict(attempt)}
        if failure_evidence is not None:
            archived["failure_evidence"] = _relative(failure_evidence, output_dir)
            archived["failure_evidence_sha256"] = failure_evidence_sha256
        state["archived_llm_attempts"].append(archived)
        state["inflight_llm"] = None
        persist()

    state["status"] = "running"
    state["stop_reason"] = None
    persist()

    while True:
        if state["phase"] == "baseline":
            remaining = budget["max_seconds"] - elapsed()
            if remaining <= 0:
                return finish("stopped", "budget_max_seconds")
            outcome = _run_evaluator(
                phase="baseline",
                attempt_parent=output_dir / "iterations" / "baseline",
                snapshot_dir=snapshot_dir,
                manifest=manifest,
                symbol=evaluator_symbol,
                prior_path=None,
                timeout=remaining,
                command_runner=command_runner,
                clock=clock,
            )
            if not outcome["success"]:
                state["status"] = "paused"
                state["stop_reason"] = (
                    "budget_max_seconds" if outcome["timed_out"] else "baseline_failed"
                )
                state["last_failure"] = {
                    "reason": outcome["failure"],
                    "evidence": _relative(outcome["evidence_path"], output_dir),
                    "evidence_sha256": outcome["evidence_sha256"],
                }
                if outcome["timed_out"]:
                    return finish("stopped", "budget_max_seconds")
                persist()
                return state
            state["last_artifact_dir"] = _relative(outcome["artifact_dir"], output_dir)
            state["baseline_artifact_dir"] = state["last_artifact_dir"]
            state["baseline_evidence"] = _relative(
                outcome["evidence_path"], output_dir
            )
            state["baseline_evidence_sha256"] = outcome["evidence_sha256"]
            state["last_failure"] = None
            state["phase"] = "propose"
            persist()
            if elapsed() >= budget["max_seconds"]:
                return finish("stopped", "budget_max_seconds")

        if state["phase"] == "propose":
            reason = budget_reason()
            if reason is not None and state.get("inflight_llm") is None:
                return finish("stopped", reason)
            remaining_candidates = budget["max_candidates"] - state["candidates_used"]
            remaining_calls = budget["max_llm_calls"] - state["llm_calls_used"]
            artifact_dir = output_dir / state["last_artifact_dir"]
            try:
                run = load_artifact(artifact_dir, TARGET)
                feedback, feedback_path = _find_feedback(artifact_dir)
                if feedback is not None:
                    _validate_feedback(feedback, mission)
                    previous = state["iterations"][-1] if state["iterations"] else None
                    if (
                        previous is not None
                        and feedback_path is not None
                        and hashlib.sha256(feedback_path.read_bytes()).hexdigest()
                        != previous["feedback_sha256"]
                    ):
                        return finish("failed", "prediction_feedback_changed")
                    run["prediction_feedback"] = feedback
            except (LoopValidationError, OSError, ValueError):
                return finish("failed", "research_artifact_invalid")
            class BudgetedClient:
                def __init__(self) -> None:
                    self._returned_in_this_process = False

                def propose(self, prompt: str) -> dict[str, Any]:
                    nonlocal client
                    if self._returned_in_this_process:
                        archive_inflight_llm("response_rejected")
                    else:
                        inflight = state.get("inflight_llm")
                        if inflight is not None:
                            response = _validate_llm_attempt(
                                inflight,
                                output_dir,
                                mission,
                                require_response=False,
                            )
                            if response is not None:
                                hydrate_inflight_llm(inflight)
                                if elapsed() >= budget["max_seconds"]:
                                    archive_inflight_llm(
                                        "budget_expired_after_response"
                                    )
                                    raise BudgetExhausted("budget_max_seconds")
                                self._returned_in_this_process = True
                                return response
                            archive_inflight_llm("response_missing_after_crash")

                    reason = budget_reason()
                    if reason in {"budget_max_seconds", "budget_max_llm_calls"}:
                        raise BudgetExhausted(reason)
                    if client is None:
                        client = client_from_env(
                            env if env is not None else dict(os.environ),
                            timeout_secs=min(
                                60.0,
                                max(1.0, budget["max_seconds"] - elapsed()),
                            ),
                        )
                    remaining_seconds = budget["max_seconds"] - elapsed()
                    current_timeout = getattr(client, "_timeout_secs", None)
                    if isinstance(current_timeout, (int, float)):
                        client._timeout_secs = min(current_timeout, remaining_seconds)
                    call_number = state["llm_calls_used"] + 1
                    attempt_dir = _attempt_dir(
                        output_dir
                        / "iterations"
                        / f"{int(state['next_iteration']):04d}"
                        / "llm-attempts",
                        "attempt",
                    )
                    prompt_path, prompt_hash = _write_content_addressed_text(
                        attempt_dir, "prompt", prompt
                    )
                    started_evidence = {
                        "schema_version": EVIDENCE_SCHEMA_VERSION,
                        "kind": "llm_call_started",
                        "mission_id": mission["mission_id"],
                        "iteration": state["next_iteration"],
                        "call_number": call_number,
                        "prompt_path": prompt_path.name,
                        "prompt_sha256": prompt_hash,
                        "client": client.__class__.__name__,
                        "model": getattr(client, "_model", None),
                        "remaining_seconds": remaining_seconds,
                    }
                    started_path, started_hash = _write_content_addressed(
                        attempt_dir, "evidence", started_evidence
                    )
                    state["llm_calls_used"] += 1
                    state["inflight_llm"] = {
                        "iteration": state["next_iteration"],
                        "call_number": call_number,
                        "attempt_dir": _relative(attempt_dir, output_dir),
                        "prompt_path": _relative(prompt_path, output_dir),
                        "prompt_sha256": prompt_hash,
                        "started_evidence": _relative(started_path, output_dir),
                        "started_evidence_sha256": started_hash,
                        "response_path": None,
                        "response_sha256": None,
                        "returned_evidence": None,
                        "returned_evidence_sha256": None,
                        "client": client.__class__.__name__,
                        "model": getattr(client, "_model", None),
                    }
                    persist()
                    try:
                        response = client.propose(prompt)
                    except Exception as err:
                        failed_path, failed_hash = _write_content_addressed(
                            attempt_dir,
                            "evidence",
                            {
                                "schema_version": EVIDENCE_SCHEMA_VERSION,
                                "kind": "llm_call_failed",
                                "mission_id": mission["mission_id"],
                                "iteration": state["next_iteration"],
                                "call_number": call_number,
                                "error_type": type(err).__name__,
                            },
                        )
                        archive_inflight_llm(
                            "provider_failed",
                            failure_evidence=failed_path,
                            failure_evidence_sha256=failed_hash,
                        )
                        raise
                    response_path, response_hash = _write_content_addressed(
                        attempt_dir, "response", response
                    )
                    returned_path, returned_hash = _write_content_addressed(
                        attempt_dir,
                        "evidence",
                        {
                            "schema_version": EVIDENCE_SCHEMA_VERSION,
                            "kind": "llm_call_returned",
                            "mission_id": mission["mission_id"],
                            "iteration": state["next_iteration"],
                            "call_number": call_number,
                            "prompt_path": prompt_path.name,
                            "prompt_sha256": prompt_hash,
                            "response_path": response_path.name,
                            "response_sha256": response_hash,
                            "client": client.__class__.__name__,
                            "model": getattr(client, "_model", None),
                            "usage": getattr(client, "last_usage", None),
                        },
                    )
                    state["inflight_llm"].update(
                        {
                            "response_path": _relative(response_path, output_dir),
                            "response_sha256": response_hash,
                            "returned_evidence": _relative(returned_path, output_dir),
                            "returned_evidence_sha256": returned_hash,
                        }
                    )
                    persist()
                    if elapsed() >= budget["max_seconds"]:
                        archive_inflight_llm("budget_expired_after_response")
                        raise BudgetExhausted("budget_max_seconds")
                    self._returned_in_this_process = True
                    return response

            try:
                proposal = propose_candidates(
                    BudgetedClient(),
                    run,
                    mission,
                    mutation_limit=remaining_candidates,
                    max_retries=max(0, remaining_calls),
                )
            except BudgetExhausted as err:
                return finish("stopped", err.reason)
            except Exception as err:  # noqa: BLE001 - model failure is explicit and resumable
                if state.get("inflight_llm") is not None:
                    archive_inflight_llm("proposal_error")
                reason = budget_reason()
                if reason is not None:
                    return finish("stopped", reason)
                state["status"] = "paused"
                state["stop_reason"] = "llm_failed"
                state["last_failure"] = {"reason": type(err).__name__}
                persist()
                return state

            candidate_count = len(proposal["mutations"]) + len(
                proposal["probability_blends"]
            )
            if candidate_count == 0:
                archive_inflight_llm("empty_proposal")
                return finish("failed", "empty_proposal")
            if not proposal["probability_blends"]:
                archive_inflight_llm("no_probability_blend_candidates")
                return finish("failed", "no_probability_blend_candidates")
            iteration = int(state["next_iteration"])
            iteration_dir = output_dir / "iterations" / f"{iteration:04d}"
            llm_attempt = state.get("inflight_llm")
            _validate_llm_attempt(
                llm_attempt,
                output_dir,
                mission,
                require_response=True,
            )
            llm_call_lineage = sorted(
                [
                    archived["attempt"]["call_number"]
                    for archived in state["archived_llm_attempts"]
                    if archived["attempt"]["iteration"] == iteration
                ]
                + [llm_attempt["call_number"]]
            )
            prior = build_prior(TARGET, mission, proposal, run.get("input_prior"))
            prior_path, prior_hash = _write_content_addressed(
                iteration_dir, "prior", prior
            )
            proposal_evidence = {
                "schema_version": EVIDENCE_SCHEMA_VERSION,
                "kind": "proposal",
                "mission_id": mission["mission_id"],
                "iteration": iteration,
                "prior_sha256": prior_hash,
                "candidate_count": candidate_count,
                "llm_calls_used": state["llm_calls_used"],
                "llm_attempt": llm_attempt,
                "llm_call_lineage": llm_call_lineage,
            }
            proposal_evidence_path, proposal_evidence_hash = _write_content_addressed(
                iteration_dir, "evidence", proposal_evidence
            )
            state["candidates_used"] += candidate_count
            state["pending"] = {
                "iteration": iteration,
                "candidate_count": candidate_count,
                "prior_path": _relative(prior_path, output_dir),
                "prior_sha256": prior_hash,
                "proposal_evidence": _relative(proposal_evidence_path, output_dir),
                "proposal_evidence_sha256": proposal_evidence_hash,
                "llm_attempt": llm_attempt,
                "llm_call_lineage": llm_call_lineage,
            }
            state["inflight_llm"] = None
            state["phase"] = "evaluate"
            state["last_failure"] = None
            persist()

        if state["phase"] == "evaluate":
            remaining = budget["max_seconds"] - elapsed()
            if remaining <= 0:
                return finish("stopped", "budget_max_seconds")
            pending = state.get("pending")
            if not isinstance(pending, dict):
                return finish("failed", "missing_pending_prior")
            iteration = int(pending["iteration"])
            prior_path = output_dir / pending["prior_path"]
            if (
                not prior_path.is_file()
                or hashlib.sha256(prior_path.read_bytes()).hexdigest()
                != pending["prior_sha256"]
                or prior_path.stem.removeprefix("prior-") != pending["prior_sha256"]
            ):
                return finish("failed", "pending_prior_invalid")
            try:
                prior = _read_json(prior_path)
            except LoopValidationError:
                return finish("failed", "pending_prior_invalid")
            if not isinstance(prior, dict):
                return finish("failed", "pending_prior_invalid")
            outcome = _run_evaluator(
                phase="evaluate",
                attempt_parent=output_dir / "iterations" / f"{iteration:04d}" / "evaluations",
                snapshot_dir=snapshot_dir,
                manifest=manifest,
                symbol=evaluator_symbol,
                prior_path=prior_path,
                timeout=remaining,
                command_runner=command_runner,
                clock=clock,
            )
            if not outcome["success"]:
                state["status"] = "paused"
                state["stop_reason"] = "evaluator_failed"
                state["last_failure"] = {
                    "reason": outcome["failure"],
                    "evidence": _relative(outcome["evidence_path"], output_dir),
                    "evidence_sha256": outcome["evidence_sha256"],
                }
                if outcome["timed_out"]:
                    return finish("stopped", "budget_max_seconds")
                persist()
                return state

            def finish_post_evaluator_failure(reason: str) -> dict[str, Any]:
                state["last_failure"] = {
                    "reason": reason,
                    "evidence": _relative(outcome["evidence_path"], output_dir),
                    "evidence_sha256": outcome["evidence_sha256"],
                    "artifact_dir": _relative(outcome["artifact_dir"], output_dir),
                    "feedback_artifacts": _feedback_artifact_snapshot(
                        outcome["artifact_dir"]
                    ),
                }
                return finish("failed", reason)

            try:
                feedback, feedback_path = _find_feedback(outcome["artifact_dir"])
                if feedback is None or feedback_path is None:
                    return finish_post_evaluator_failure(
                        "prediction_feedback_missing"
                    )
                kept_models = _validate_feedback(feedback, mission, prior)
            except LoopValidationError:
                return finish_post_evaluator_failure(
                    "prediction_feedback_invalid"
                )
            feedback_hash = hashlib.sha256(feedback_path.read_bytes()).hexdigest()
            decision_evidence = {
                "schema_version": EVIDENCE_SCHEMA_VERSION,
                "kind": "deterministic_decision",
                "mission_id": mission["mission_id"],
                "iteration": iteration,
                "prior_sha256": pending["prior_sha256"],
                "feedback_sha256": feedback_hash,
                "kept_models": kept_models,
                "decision": "keep" if kept_models else "continue",
            }
            decision_path, decision_hash = _write_content_addressed(
                output_dir / "iterations" / f"{iteration:04d}",
                "evidence",
                decision_evidence,
            )
            state["iterations"].append(
                {
                    "iteration": iteration,
                    "candidate_count": pending["candidate_count"],
                    "prior_path": pending["prior_path"],
                    "prior_sha256": pending["prior_sha256"],
                    "llm_attempt": pending["llm_attempt"],
                    "llm_call_lineage": pending["llm_call_lineage"],
                    "proposal_evidence": pending["proposal_evidence"],
                    "proposal_evidence_sha256": pending[
                        "proposal_evidence_sha256"
                    ],
                    "artifact_dir": _relative(outcome["artifact_dir"], output_dir),
                    "feedback_path": _relative(feedback_path, output_dir),
                    "feedback_sha256": feedback_hash,
                    "evaluator_evidence": _relative(
                        outcome["evidence_path"], output_dir
                    ),
                    "evaluator_evidence_sha256": outcome["evidence_sha256"],
                    "decision_evidence": _relative(decision_path, output_dir),
                    "decision_evidence_sha256": decision_hash,
                    "kept_models": kept_models,
                }
            )
            state["last_artifact_dir"] = _relative(outcome["artifact_dir"], output_dir)
            state["pending"] = None
            state["next_iteration"] = iteration + 1
            state["last_failure"] = None
            state["phase"] = "propose"
            if kept_models:
                return finish("completed", "deterministic_keep")
            persist()


def main(env: dict[str, str] | None = None) -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("mission_json", nargs="?")
    parser.add_argument("snapshot_dir", nargs="?")
    parser.add_argument("output_dir", nargs="?")
    parser.add_argument("--print-policy-snapshot-id", action="store_true")
    parser.add_argument("--print-brief-snapshot-id")
    args = parser.parse_args()
    if args.print_policy_snapshot_id:
        print(current_policy_snapshot_id())
        return
    if args.print_brief_snapshot_id:
        payload = _read_json(Path(args.print_brief_snapshot_id))
        if not isinstance(payload, dict):
            parser.error("brief mission must be a JSON object")
        print(research_brief_snapshot_id(payload))
        return
    if not args.mission_json or not args.snapshot_dir or not args.output_dir:
        parser.error("mission_json, snapshot_dir, and output_dir are required")
    mission_payload = _read_json(Path(args.mission_json))
    state = run_prediction_research_loop(
        mission_payload,
        Path(args.snapshot_dir),
        Path(args.output_dir),
        env=env,
    )
    print(json.dumps(state, indent=2, sort_keys=True))
    if state["status"] in {"paused", "failed"}:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
