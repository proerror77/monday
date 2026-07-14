#!/usr/bin/env python3
"""Build a true runtime-replay candidate strategy artifact.

This consumes the JSON written by `ploy-runner run --output-json` after running
the candidate strategy config over an ordered MarketUpdate replay. Unlike the
AutoFactor top-bucket aggregate helper, this artifact represents the deployed
runtime scorer's actual intent/order/fill behavior on the replay stream.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import re
from collections import Counter
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any

from autofactor_accounting_catalog import (
    autofactor_target_horizon,
    validate_autofactor_source_contract,
)


RUNTIME_REPLAY_BASIS = "runtime_market_update_replay"
ARCHIVED_RECORDING_SUFFIX_RE = re.compile(r"\.\d{8}T\d{6}Z?$")
COUNTERFACTUAL_THRESHOLDS = (
    ("0.05", "005"),
    ("0.10", "010"),
    ("0.15", "015"),
    ("0.25", "025"),
)


def decimal_value(raw: Any, default: Decimal = Decimal("0")) -> Decimal:
    try:
        if raw is None:
            return default
        return Decimal(str(raw))
    except (InvalidOperation, ValueError):
        return default


def decimal_arg(raw: Any, name: str) -> Decimal:
    try:
        value = Decimal(str(raw))
    except (InvalidOperation, ValueError):
        raise SystemExit(f"{name} must be a decimal value: {raw!r}") from None
    if not value.is_finite():
        raise SystemExit(f"{name} must be finite: {raw!r}")
    return value


def sha256_file_if_present(raw_path: str) -> str:
    if not raw_path:
        return ""
    path = Path(raw_path)
    if not path.exists() or not path.is_file():
        return ""
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def looks_like_mutable_recording_path(raw_path: str) -> bool:
    """Detect the active recording path, as opposed to timestamped archives."""
    if not raw_path:
        return False
    name = Path(raw_path).name
    if not name.endswith(".ndjson"):
        return False
    stem = name.removesuffix(".ndjson")
    return ARCHIVED_RECORDING_SUFFIX_RE.search(stem) is None


def canonical_sha256(payload: dict[str, Any]) -> str:
    encoded = json.dumps(payload, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def build_identity(
    *,
    basis: str,
    runtime_score: str,
    strategy_profile: str,
    deployment_id: str,
    workflow_run_id: str,
    recording_path: str,
    recording_sha256: str,
    config_path: str,
    config_sha256: str,
    runner_git_sha: str,
    runtime_evaluation_sha256: str,
) -> dict[str, Any]:
    return {
        "basis": basis,
        "runtime_score": runtime_score,
        "strategy_profile": strategy_profile,
        "deployment_id": deployment_id,
        "workflow_run_id": workflow_run_id,
        "recording_path": recording_path,
        "recording_sha256": recording_sha256,
        "config_path": config_path,
        "config_sha256": config_sha256,
        "runner_git_sha": runner_git_sha,
        "runtime_evaluation_sha256": runtime_evaluation_sha256,
    }


def is_closed_settlement(raw: Any) -> bool:
    if raw is None:
        return False
    text = str(raw).strip().lower()
    return text not in {"", "open", "none", "null", "nan"}


def runtime_evidence(payload: dict[str, Any]) -> dict[str, Any]:
    evidence = payload.get("runtime_evidence")
    return evidence if isinstance(evidence, dict) else {}


def result_payload(payload: dict[str, Any]) -> dict[str, Any]:
    result = payload.get("result")
    return result if isinstance(result, dict) else {}


def int_metric(payload: dict[str, Any], key: str) -> int:
    try:
        return int(payload.get(key) or 0)
    except (TypeError, ValueError):
        return 0


def normalize_counterfactual_threshold(raw: str) -> str:
    value = decimal_arg(raw, "--configured-entry-threshold")
    for label, _suffix in COUNTERFACTUAL_THRESHOLDS:
        if value == Decimal(label):
            return label
    allowed = ", ".join(label for label, _suffix in COUNTERFACTUAL_THRESHOLDS)
    raise SystemExit(f"--configured-entry-threshold must be one of: {allowed}")


def resolve_source_horizon(source_target: str, source_horizon: str) -> str:
    if not source_target and not source_horizon:
        return ""
    if source_horizon:
        blockers = validate_autofactor_source_contract(
            target=source_target,
            horizon=source_horizon,
        )
        if blockers:
            raise SystemExit("; ".join(blockers))
        return source_horizon
    resolved = autofactor_target_horizon(source_target)
    if resolved == "unknown":
        blockers = validate_autofactor_source_contract(target=source_target, horizon="")
        raise SystemExit("; ".join(blockers))
    return resolved


def score_counterfactual(
    diagnostics: dict[str, Any], configured_entry_threshold: str
) -> dict[str, Any]:
    if not diagnostics:
        return {}
    formula_evaluations = int_metric(diagnostics, "settlement_autofactor_formula_evaluations")
    direct: dict[str, int] = {}
    reverse: dict[str, int] = {}
    for label, suffix in COUNTERFACTUAL_THRESHOLDS:
        direct[label] = int_metric(
            diagnostics,
            f"settlement_autofactor_predictive_score_ge_{suffix}",
        )
        reverse[label] = int_metric(
            diagnostics,
            f"settlement_autofactor_predictive_reverse_score_ge_{suffix}",
        )
    if not formula_evaluations and not any(direct.values()) and not any(reverse.values()):
        return {}

    threshold_label = normalize_counterfactual_threshold(configured_entry_threshold)
    configured_direct = direct[threshold_label]
    configured_reverse = reverse[threshold_label]
    if configured_reverse > configured_direct:
        diagnosis = "reverse_direction_stronger_at_configured_threshold"
    elif configured_direct == 0 and any(value > 0 for key, value in direct.items() if key != "0.25"):
        diagnosis = "direct_signal_exists_below_configured_threshold"
    elif configured_direct == 0:
        diagnosis = "direct_signal_too_weak_at_all_reported_thresholds"
    else:
        diagnosis = "direct_signal_passes_configured_threshold"

    return {
        "formula_evaluations": formula_evaluations,
        "depth_fillable": int_metric(diagnostics, "settlement_autofactor_depth_fillable"),
        "entry_score_skips": int_metric(diagnostics, "skip_entry_score"),
        "configured_entry_threshold": threshold_label,
        "direct_pass_counts": direct,
        "reverse_direction_pass_counts": reverse,
        "diagnosis": diagnosis,
    }


def purpose_is_entry(row: dict[str, Any]) -> bool:
    intent_id = str(row.get("intent_id") or "")
    side = str(row.get("side") or row.get("fill_side") or row.get("order_side") or "").upper()
    if intent_id.startswith("tl_settle_") or side == "SELL":
        return False
    purpose = row.get("purpose") or row.get("signal_inputs", {}).get("purpose")
    if purpose is not None:
        return str(purpose).upper() == "ENTRY"
    return side == "BUY"


def row_key(row: dict[str, Any], key: str) -> str | None:
    raw = row.get(key)
    if raw is None:
        return None
    text = str(raw).strip()
    return text or None


def build_event_id_lookup(rows: list[dict[str, Any]]) -> dict[tuple[str, str], str]:
    lookup: dict[tuple[str, str], str] = {}
    for row in rows:
        event_id = row_key(row, "event_id") or row_key(row, "market_id")
        if not event_id:
            continue
        for key in ("intent_id", "order_id", "token_id"):
            value = row_key(row, key)
            if value:
                lookup[(key, value)] = event_id
    return lookup


def resolved_event_id(row: dict[str, Any], lookup: dict[tuple[str, str], str]) -> str | None:
    event_id = row_key(row, "event_id") or row_key(row, "market_id")
    if event_id:
        return event_id
    for key in ("intent_id", "order_id", "token_id"):
        value = row_key(row, key)
        if value and (key, value) in lookup:
            return lookup[(key, value)]
    return None


def build_artifact(
    runtime_eval: dict[str, Any],
    *,
    runtime_score: str,
    strategy_profile: str,
    deployment_id: str,
    source_workflow: str,
    workflow_run_id: str,
    workflow_run_url: str,
    artifact_name: str,
    recording_path: str,
    recording_sha256: str,
    config_path: str,
    config_sha256: str,
    runner_source: str,
    runner_git_sha: str,
    runtime_evaluation_sha256: str,
    source_factor_name: str,
    source_dsl_hash: str,
    source_target: str,
    source_horizon: str,
    stake_usd: Decimal,
    full_depth_entry: bool,
    min_trade_count: int,
    min_fill_rate: Decimal,
    min_roi: Decimal,
    configured_entry_threshold: str,
    evidence: str,
) -> dict[str, Any]:
    evidence_rows = runtime_evidence(runtime_eval)
    result = result_payload(runtime_eval)
    events = [row for row in evidence_rows.get("events") or [] if isinstance(row, dict)]
    orders = [row for row in evidence_rows.get("orders") or [] if isinstance(row, dict)]
    fills = [row for row in evidence_rows.get("fills") or [] if isinstance(row, dict)]
    intents = [row for row in evidence_rows.get("intents") or [] if isinstance(row, dict)]

    entry_orders = [row for row in orders if purpose_is_entry(row)]
    entry_fills = [row for row in fills if purpose_is_entry(row)]
    entry_events = [row for row in events if purpose_is_entry(row)]

    event_lookup = build_event_id_lookup(events + intents + orders + fills)
    decision_rows = entry_events or entry_orders
    event_ids = [
        event_id
        for row in decision_rows
        if (event_id := resolved_event_id(row, event_lookup))
    ]
    unique_event_count = len(set(event_ids))
    event_decision_counts = Counter(event_ids)
    max_event_decisions = max(event_decision_counts.values(), default=0)

    filled_order_ids = {
        str(row.get("order_id"))
        for row in entry_fills
        if row.get("order_id") and decimal_value(row.get("quantity")) > 0
    }
    trade_count = len(filled_order_ids)
    if entry_events:
        filled_entry_event_ids = {
            event_id
            for row in entry_events
            if (
                str(row.get("fill_status") or "").upper() == "FILLED"
                or (row_key(row, "order_id") in filled_order_ids)
            )
            and (event_id := resolved_event_id(row, event_lookup))
        }
        trade_count = len(filled_entry_event_ids)
    entry_order_ids = {order_id for row in entry_orders if (order_id := row_key(row, "order_id"))}
    entry_order_ids.update(
        order_id for row in entry_events if (order_id := row_key(row, "order_id"))
    )
    order_count = len(entry_order_ids)
    entry_intents = [row for row in intents if purpose_is_entry(row)]
    intent_count = max(len(entry_intents), len(entry_events))
    if intent_count == 0:
        intent_count = int(result.get("intents_submitted") or 0)
    denominator = max(order_count, intent_count, 1)
    entry_fill_rate = Decimal(trade_count) / Decimal(denominator)

    settled_event_count = sum(1 for row in entry_events if is_closed_settlement(row.get("settlement")))
    total_pnl = sum((decimal_value(row.get("pnl")) for row in entry_events), Decimal("0"))
    if not entry_events:
        total_pnl = decimal_value(result.get("net_pnl"), decimal_value(result.get("realized_pnl")))
    notional = stake_usd * Decimal(trade_count)
    roi = total_pnl / notional if notional > 0 else Decimal("0")

    blocking_flags: list[str] = []
    if trade_count < min_trade_count:
        blocking_flags.append(f"trade_count_too_small:{trade_count}<{min_trade_count}")
    if unique_event_count < min(trade_count, min_trade_count):
        blocking_flags.append(
            f"unique_event_count_too_small:{unique_event_count}<{min(trade_count, min_trade_count)}"
        )
    if max_event_decisions != 1 and trade_count > 0:
        blocking_flags.append(f"one_event_decision_violation:{max_event_decisions}")
    if entry_fill_rate < min_fill_rate:
        blocking_flags.append(
            f"entry_fill_rate_too_low:{entry_fill_rate:.4f}<{min_fill_rate:.4f}"
        )
    if settled_event_count < trade_count:
        blocking_flags.append(f"official_settlement_missing:{settled_event_count}<{trade_count}")
    if not full_depth_entry:
        blocking_flags.append("full_depth_entry_not_confirmed")
    if roi < min_roi:
        blocking_flags.append(f"roi_too_low:{roi:.6f}<{min_roi:.6f}")
    if not entry_orders and not entry_fills:
        blocking_flags.append("zero_runtime_orders_and_fills")
    if not recording_path:
        blocking_flags.append("recording_path_missing")
    if not recording_sha256:
        blocking_flags.append("recording_sha256_missing")
        if looks_like_mutable_recording_path(recording_path):
            blocking_flags.append("mutable_recording_without_sha256")

    diagnostics = runtime_eval.get("strategy_diagnostics") or result.get("strategy_diagnostics") or {}
    counterfactual = score_counterfactual(diagnostics, configured_entry_threshold)
    identity = build_identity(
        basis=RUNTIME_REPLAY_BASIS,
        runtime_score=runtime_score,
        strategy_profile=strategy_profile,
        deployment_id=deployment_id,
        workflow_run_id=workflow_run_id,
        recording_path=recording_path,
        recording_sha256=recording_sha256,
        config_path=config_path,
        config_sha256=config_sha256,
        runner_git_sha=runner_git_sha,
        runtime_evaluation_sha256=runtime_evaluation_sha256,
    )
    source_factor = {
        "name": source_factor_name,
        "dsl_hash": source_dsl_hash,
        "target": source_target,
        "horizon": source_horizon,
    }

    artifact = {
        "schema_version": 1,
        "kind": "autofactor_candidate_strategy_replay",
        "candidate_replay_id": f"candidate_replay:{canonical_sha256(identity)[:32]}",
        "identity": identity,
        "evidence_stage": "executable_replay",
        "basis": RUNTIME_REPLAY_BASIS,
        "strategy_profile": strategy_profile,
        "runtime_score": runtime_score,
        "promotion_ready": not blocking_flags,
        "promotion_decision": "promote_to_runtime" if not blocking_flags else "blocked",
        "source_workflow": source_workflow,
        "workflow_run_id": workflow_run_id,
        "workflow_run_url": workflow_run_url,
        "artifact_name": artifact_name,
        "deployment_id": deployment_id,
        "recording_path": recording_path,
        "recording_sha256": recording_sha256,
        "config_path": config_path,
        "config_sha256": config_sha256,
        "runner_source": runner_source,
        "runner_git_sha": runner_git_sha,
        "runtime_evaluation_sha256": runtime_evaluation_sha256,
        "evidence": evidence,
        "source_factor": source_factor,
        "decision_contract": {
            "event_level": trade_count > 0 and unique_event_count == trade_count,
            "one_decision_per_event": trade_count > 0 and max_event_decisions == 1,
            "official_settlement": trade_count > 0 and settled_event_count >= trade_count,
            "full_depth_entry": full_depth_entry,
            "target": source_target,
            "horizon": source_horizon,
            "stake_usd": float(stake_usd),
        },
        "acceptance_criteria": {
            "min_trade_count": min_trade_count,
            "min_fill_rate": float(min_fill_rate),
            "min_roi": float(min_roi),
            "full_depth_entry": full_depth_entry,
        },
        "metrics": {
            "updates_processed": int(result.get("updates_processed") or 0),
            "intents_submitted": intent_count,
            "runtime_intents_submitted": int(result.get("intents_submitted") or intent_count),
            "orders": order_count,
            "fills": len(entry_fills),
            "trade_count": trade_count,
            "unique_event_count": unique_event_count,
            "settlement_event_count": settled_event_count,
            "max_event_decisions": max_event_decisions,
            "entry_fill_rate": float(entry_fill_rate),
            "total_pnl": float(total_pnl),
            "roi": float(roi),
        },
        "strategy_diagnostics": diagnostics,
        "blocking_risk_flags": blocking_flags,
    }
    if counterfactual:
        artifact["score_counterfactual"] = counterfactual
    return artifact


def render_markdown(artifact: dict[str, Any]) -> str:
    metrics = artifact.get("metrics") or {}
    flags = artifact.get("blocking_risk_flags") or []
    lines = [
        "# Runtime Candidate Strategy Replay",
        "",
        f"- Promotion ready: `{str(artifact.get('promotion_ready')).lower()}`",
        f"- Basis: `{artifact.get('basis')}`",
        f"- Runtime score: `{artifact.get('runtime_score')}`",
        f"- Strategy profile: `{artifact.get('strategy_profile')}`",
        f"- Recording: `{artifact.get('recording_path')}`",
        f"- Recording SHA256: `{artifact.get('recording_sha256')}`",
        f"- Updates processed: `{metrics.get('updates_processed')}`",
        f"- Intents submitted: `{metrics.get('intents_submitted')}`",
        f"- Trades: `{metrics.get('trade_count')}`",
        f"- Unique events: `{metrics.get('unique_event_count')}`",
        f"- Entry fill rate: `{metrics.get('entry_fill_rate')}`",
        f"- Total PnL: `{metrics.get('total_pnl')}`",
        f"- ROI: `{metrics.get('roi')}`",
        "",
        "## Acceptance Criteria",
        "",
    ]
    criteria = artifact.get("acceptance_criteria") or {}
    for key in ("min_trade_count", "min_fill_rate", "min_roi", "full_depth_entry"):
        lines.append(f"- {key}: `{criteria.get(key)}`")
    lines.extend(
        [
            "",
            "## Blocking Risk Flags",
            "",
        ]
    )
    lines.extend(f"- `{flag}`" for flag in flags) if flags else lines.append("- `<none>`")
    counterfactual = artifact.get("score_counterfactual")
    if isinstance(counterfactual, dict):
        lines.extend(
            [
                "",
                "## Score Counterfactual",
                "",
                f"- Diagnosis: `{counterfactual.get('diagnosis')}`",
                f"- Formula evaluations: `{counterfactual.get('formula_evaluations')}`",
                f"- Depth-fillable evaluations: `{counterfactual.get('depth_fillable')}`",
                f"- Entry score skips: `{counterfactual.get('entry_score_skips')}`",
                "",
                "| threshold | direct passes | reverse-direction passes |",
                "| --- | ---: | ---: |",
            ]
        )
        direct = counterfactual.get("direct_pass_counts") or {}
        reverse = counterfactual.get("reverse_direction_pass_counts") or {}
        for threshold, _suffix in COUNTERFACTUAL_THRESHOLDS:
            lines.append(f"| `{threshold}` | `{direct.get(threshold, 0)}` | `{reverse.get(threshold, 0)}` |")
    lines.append("")
    return "\n".join(lines)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--runtime-evaluation-json", required=True)
    parser.add_argument("--runtime-score", required=True)
    parser.add_argument("--strategy-profile", default="settlement_probability")
    parser.add_argument("--deployment-id", default="")
    parser.add_argument("--source-workflow", default="runtime-candidate-replay.yml")
    parser.add_argument("--workflow-run-id", default="")
    parser.add_argument("--workflow-run-url", default="")
    parser.add_argument("--artifact-name", default="")
    parser.add_argument("--recording-path", default="")
    parser.add_argument("--recording-sha256", default="")
    parser.add_argument("--config-path", default="")
    parser.add_argument("--config-sha256", default="")
    parser.add_argument("--runner-source", default="")
    parser.add_argument("--runner-git-sha", default="")
    parser.add_argument("--source-factor-name", default="")
    parser.add_argument("--source-dsl-hash", default="")
    parser.add_argument("--source-target", default="")
    parser.add_argument("--source-horizon", default="")
    parser.add_argument("--stake-usd", default="15")
    parser.add_argument("--full-depth-entry", action="store_true")
    parser.add_argument("--min-trade-count", type=int, default=50)
    parser.add_argument("--min-fill-rate", default="0.30")
    parser.add_argument("--min-roi", default="0")
    parser.add_argument("--configured-entry-threshold", default="0.25")
    parser.add_argument("--evidence", default="")
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", default="")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    runtime_path = Path(args.runtime_evaluation_json)
    runtime_eval = json.loads(runtime_path.read_text(encoding="utf-8"))
    runtime_evaluation_sha256 = sha256_file_if_present(str(runtime_path))
    recording_sha256 = args.recording_sha256 or sha256_file_if_present(args.recording_path)
    config_sha256 = args.config_sha256 or sha256_file_if_present(args.config_path)
    artifact = build_artifact(
        runtime_eval,
        runtime_score=args.runtime_score,
        strategy_profile=args.strategy_profile,
        deployment_id=args.deployment_id,
        source_workflow=args.source_workflow,
        workflow_run_id=args.workflow_run_id,
        workflow_run_url=args.workflow_run_url,
        artifact_name=args.artifact_name,
        recording_path=args.recording_path,
        recording_sha256=recording_sha256,
        config_path=args.config_path,
        config_sha256=config_sha256,
        runner_source=args.runner_source,
        runner_git_sha=args.runner_git_sha,
        runtime_evaluation_sha256=runtime_evaluation_sha256,
        source_factor_name=args.source_factor_name,
        source_dsl_hash=args.source_dsl_hash,
        source_target=args.source_target,
        source_horizon=resolve_source_horizon(args.source_target, args.source_horizon),
        stake_usd=decimal_arg(args.stake_usd, "--stake-usd"),
        full_depth_entry=args.full_depth_entry,
        min_trade_count=args.min_trade_count,
        min_fill_rate=decimal_arg(args.min_fill_rate, "--min-fill-rate"),
        min_roi=decimal_arg(args.min_roi, "--min-roi"),
        configured_entry_threshold=args.configured_entry_threshold,
        evidence=args.evidence or str(runtime_path),
    )
    output_json = Path(args.output_json)
    output_json.parent.mkdir(parents=True, exist_ok=True)
    output_json.write_text(json.dumps(artifact, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if args.output_md:
        output_md = Path(args.output_md)
        output_md.parent.mkdir(parents=True, exist_ok=True)
        output_md.write_text(render_markdown(artifact), encoding="utf-8")
    print(json.dumps(artifact, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
