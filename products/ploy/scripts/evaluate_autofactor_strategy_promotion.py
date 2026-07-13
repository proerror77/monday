#!/usr/bin/env python3
"""Evaluate whether AutoFactor report rows can become strategy handoffs.

This is intentionally stricter than the AutoFactor IC/ICIR candidate gate.
An AutoFactor row is only a qualified strategy when:

1. all global settlement PRD promotion gates are ready;
2. the row is a `candidate` with reason `passed`;
3. the target is one of the allowed executable targets; and
4. the factor has an explicit runtime strategy-profile mapping; and
5. a strategy-level historical executable replay artifact proves the same
   runtime score under event-level, full-depth, official-settlement accounting.

For `autofactor_formula:*` runtime scores, the PRD model-specific
`symbol_holdout` and `walk_forward_oos` gates are replaced by formula-level
symbol/window stability from the AutoFactor row. Data quality, Deribit,
execution-depth, calibration, and replay-parity gates remain global blockers.
Hard entry gates are still execution filters, not permission to waive failed
configured-stake capacity at the global promotion gate.
Full-depth fillability is also insufficient by itself: if the average entry
sweep slippage is too high, the candidate is executable only by accepting a
materially worse price and must stay blocked.

The current PM5D/PM15D settlement PRD should not silently promote a good
repricing factor into the settlement strategy lane.
"""

from __future__ import annotations

import argparse
import csv
import json
import re
from dataclasses import asdict, dataclass, field
from pathlib import Path
from typing import Any

from autofactor_accounting_catalog import autofactor_target_horizon
from autofactor_runtime_contract import RuntimeContractResolver
from research_snapshot_contract import (
    load_snapshot_execution_contract,
    snapshot_blocks_execution_claim,
)


DEFAULT_ALLOWED_TARGETS = (
    "full_depth_settlement_executable_pnl",
    "tradeable_full_depth_settlement_pnl",
)
ARCHIVED_RECORDING_SUFFIX_RE = re.compile(r"\.\d{8}T\d{6}Z?$")


@dataclass
class PromotionGate:
    ready: bool
    evidence: str
    blocked_gates: list[str] = field(default_factory=list)


@dataclass
class ExecutionQuality:
    avg_entry_sweep_slip_bps: float | None = None


@dataclass
class CandidateStrategyReplay:
    ready: bool
    evidence: str
    runtime_score: str = ""
    strategy_profile: str = ""
    basis: str = ""
    candidate_replay_id: str = ""
    identity: dict[str, Any] = field(default_factory=dict)
    promotion_decision: str = ""
    source_workflow: str = ""
    workflow_run_id: str = ""
    workflow_run_url: str = ""
    artifact_name: str = ""
    source_factor: dict[str, Any] = field(default_factory=dict)
    blockers: list[str] = field(default_factory=list)
    metrics: dict[str, Any] = field(default_factory=dict)
    decision_contract: dict[str, Any] = field(default_factory=dict)


@dataclass
class AutoFactorRow:
    rank: int
    name: str
    target: str
    decision: str
    reason: str
    n: int
    spearman_ic: float
    pearson_ic: float
    window_count: int
    icir: float
    positive_window_ratio: float
    symbol_count: int
    symbol_positive_ratio: float
    monotonicity: float
    top_bucket_n: int
    top_bucket_avg_label: float
    top_bucket_positive_label_rate: float
    top_bucket_full_depth_entry_fill_rate: float
    top_bucket_avg_entry_sweep_slip_bps: float | None
    top_bucket_avg_entry_sweep_levels: float | None
    top_bucket_unique_event_count: int
    top_bucket_max_event_decisions: int
    complexity: int


@dataclass
class EvaluatedFactor:
    factor: AutoFactorRow
    qualified: bool
    blockers: list[str]
    runtime_mapping: dict[str, str] | None
    runtime_contract: dict[str, Any] | None = None


def factor_metrics(row: AutoFactorRow) -> dict[str, Any]:
    return {
        "rank": row.rank,
        "n": row.n,
        "spearman_ic": row.spearman_ic,
        "pearson_ic": row.pearson_ic,
        "window_count": row.window_count,
        "icir": row.icir,
        "positive_window_ratio": row.positive_window_ratio,
        "symbol_count": row.symbol_count,
        "symbol_positive_ratio": row.symbol_positive_ratio,
        "monotonicity": row.monotonicity,
        "top_bucket_n": row.top_bucket_n,
        "top_bucket_avg_label": row.top_bucket_avg_label,
        "top_bucket_positive_label_rate": row.top_bucket_positive_label_rate,
        "top_bucket_full_depth_entry_fill_rate": row.top_bucket_full_depth_entry_fill_rate,
        "top_bucket_avg_entry_sweep_slip_bps": row.top_bucket_avg_entry_sweep_slip_bps,
        "top_bucket_avg_entry_sweep_levels": row.top_bucket_avg_entry_sweep_levels,
        "top_bucket_unique_event_count": row.top_bucket_unique_event_count,
        "top_bucket_max_event_decisions": row.top_bucket_max_event_decisions,
        "complexity": row.complexity,
    }


def parse_float(raw: str) -> float:
    try:
        return float(raw)
    except ValueError:
        return float("nan")


def parse_int(raw: str) -> int:
    try:
        return int(raw)
    except ValueError:
        return 0


def finite_or_none(value: float) -> float | None:
    return value if value == value else None


def parse_promotion_gate(report_text: str) -> PromotionGate:
    ready: bool | None = None
    evidence = ""
    blocked: list[str] = []
    in_gate_table = False

    for line in report_text.splitlines():
        if line.startswith("ready_for_dry_run_handoff="):
            evidence = line
            ready = line.split("=", 1)[1].split(maxsplit=1)[0].lower() == "true"
            continue
        if line == "gate,passed,evidence":
            in_gate_table = True
            continue
        if in_gate_table:
            if not line.strip():
                break
            parts = line.split(",", 2)
            if len(parts) != 3:
                continue
            gate, passed, gate_evidence = parts
            if gate == "recorded_replay_parity":
                continue
            if passed.lower() != "true":
                blocked.append(f"{gate}: {gate_evidence}")

    if ready is None:
        return PromotionGate(
            ready=False,
            evidence="missing ready_for_dry_run_handoff line",
            blocked_gates=["missing_promotion_gate"],
        )
    return PromotionGate(ready=ready, evidence=evidence, blocked_gates=blocked)


def parse_execution_quality(report_text: str) -> ExecutionQuality:
    prefix = "full_depth_entry_fill_rate="
    marker = "avg_entry_sweep_slip_bps="
    for line in report_text.splitlines():
        if not line.startswith(prefix) or marker not in line:
            continue
        tail = line.split(marker, 1)[1]
        raw = tail.split(maxsplit=1)[0]
        value = parse_float(raw)
        if value == value:
            return ExecutionQuality(avg_entry_sweep_slip_bps=value)
    return ExecutionQuality()


def load_candidate_strategy_replay(path: str | None) -> CandidateStrategyReplay:
    if not path:
        return CandidateStrategyReplay(
            ready=False,
            evidence="missing candidate strategy executable replay artifact",
            blockers=["missing_candidate_strategy_replay"],
        )
    replay_path = Path(path)
    if not replay_path.is_file():
        return CandidateStrategyReplay(
            ready=False,
            evidence=f"candidate strategy executable replay artifact not found: {path}",
            blockers=["missing_candidate_strategy_replay"],
        )
    payload = json.loads(replay_path.read_text(encoding="utf-8"))
    metrics = payload.get("metrics") if isinstance(payload.get("metrics"), dict) else {}
    contract = (
        payload.get("decision_contract")
        if isinstance(payload.get("decision_contract"), dict)
        else {}
    )
    blockers = payload.get("blocking_risk_flags") or payload.get("blockers") or []
    if not isinstance(blockers, list):
        blockers = [str(blockers)]
    ready = bool(payload.get("promotion_ready") is True or payload.get("status") == "ready")
    evidence = payload.get("evidence") or payload.get("run_url") or str(replay_path)
    return CandidateStrategyReplay(
        ready=ready,
        evidence=str(evidence),
        runtime_score=str(payload.get("runtime_score", "")),
        strategy_profile=str(payload.get("strategy_profile", "")),
        basis=str(payload.get("basis", "")),
        candidate_replay_id=str(payload.get("candidate_replay_id", "")),
        identity=payload.get("identity") if isinstance(payload.get("identity"), dict) else {},
        promotion_decision=str(payload.get("promotion_decision", "")),
        source_workflow=str(payload.get("source_workflow", "")),
        workflow_run_id=str(payload.get("workflow_run_id", "")),
        workflow_run_url=str(payload.get("workflow_run_url", "")),
        artifact_name=str(payload.get("artifact_name", "")),
        source_factor=payload.get("source_factor")
        if isinstance(payload.get("source_factor"), dict)
        else {},
        blockers=[str(item) for item in blockers],
        metrics=metrics,
        decision_contract=contract,
    )


def numeric_metric(metrics: dict[str, Any], *names: str) -> float | None:
    for name in names:
        if name not in metrics:
            continue
        try:
            value = float(metrics[name])
        except (TypeError, ValueError):
            continue
        if value == value:
            return value
    return None


def candidate_strategy_replay_blockers(
    replay: CandidateStrategyReplay,
    *,
    mapping: dict[str, str] | None,
    expected_target: str,
    expected_horizon: str,
    required_strategy_profile: str,
    min_replay_trade_count: int,
    min_replay_fill_rate: float,
    min_replay_roi: float,
) -> list[str]:
    blockers = list(replay.blockers)
    if not replay.ready:
        blockers.append("candidate_strategy_replay_not_ready")
    if replay.basis != "runtime_market_update_replay":
        blockers.append(
            "candidate_strategy_replay_not_runtime_replay:"
            f"{replay.basis or '<missing>'}!=runtime_market_update_replay"
        )
    if not replay.candidate_replay_id:
        blockers.append("candidate_strategy_replay_missing_candidate_replay_id")
    elif not replay.candidate_replay_id.startswith("candidate_replay:"):
        blockers.append(
            "candidate_strategy_replay_invalid_candidate_replay_id:"
            f"{replay.candidate_replay_id}"
        )
    else:
        digest = replay.candidate_replay_id.split(":", 1)[1]
        if len(digest) < 16 or any(ch not in "0123456789abcdef" for ch in digest.lower()):
            blockers.append(
                "candidate_strategy_replay_invalid_candidate_replay_id:"
                f"{replay.candidate_replay_id}"
            )
    if not replay.identity:
        blockers.append("candidate_strategy_replay_missing_identity")
    elif replay.identity.get("basis") != "runtime_market_update_replay":
        blockers.append(
            "candidate_strategy_replay_identity_basis_mismatch:"
            f"{replay.identity.get('basis') or '<missing>'}!=runtime_market_update_replay"
        )
    if replay.identity:
        recording_path = str(replay.identity.get("recording_path") or "")
        recording_sha256 = str(replay.identity.get("recording_sha256") or "")
        if not recording_path:
            blockers.append("candidate_strategy_replay_missing_recording_path")
        if not recording_sha256:
            blockers.append("candidate_strategy_replay_missing_recording_sha256")
            if looks_like_mutable_recording_path(recording_path):
                blockers.append("candidate_strategy_replay_mutable_recording_without_sha256")
    provenance_fields = {
        "source_workflow": replay.source_workflow,
        "workflow_run_id": replay.workflow_run_id,
        "workflow_run_url": replay.workflow_run_url,
        "artifact_name": replay.artifact_name,
    }
    for field_name, value in provenance_fields.items():
        if not value:
            blockers.append(f"candidate_strategy_replay_missing_{field_name}")

    source_target = str(replay.source_factor.get("target") or "")
    source_horizon = str(replay.source_factor.get("horizon") or "")
    if not source_target:
        blockers.append("candidate_strategy_replay_missing_source_target")
    elif source_target != expected_target:
        blockers.append(
            "candidate_strategy_replay_target_mismatch:"
            f"{source_target}!={expected_target}"
        )
    if not source_horizon:
        blockers.append("candidate_strategy_replay_missing_source_horizon")
    elif source_horizon != expected_horizon:
        blockers.append(
            "candidate_strategy_replay_horizon_mismatch:"
            f"{source_horizon}!={expected_horizon}"
        )
    contract_target = replay.decision_contract.get("target")
    contract_horizon = replay.decision_contract.get("horizon")
    if contract_target is not None and contract_target != expected_target:
        blockers.append(
            "candidate_strategy_replay_contract_target_mismatch:"
            f"{contract_target}!={expected_target}"
        )
    if contract_horizon is not None and contract_horizon != expected_horizon:
        blockers.append(
            "candidate_strategy_replay_contract_horizon_mismatch:"
            f"{contract_horizon}!={expected_horizon}"
        )

    if replay.strategy_profile != required_strategy_profile:
        blockers.append(
            "candidate_strategy_replay_profile_mismatch:"
            f"{replay.strategy_profile or '<missing>'}!={required_strategy_profile}"
        )

    runtime_score = (mapping or {}).get("runtime_score", "")
    if not runtime_score:
        blockers.append("candidate_strategy_replay_missing_expected_runtime_score")
    elif replay.runtime_score != runtime_score:
        blockers.append(
            "candidate_strategy_replay_runtime_score_mismatch:"
            f"{replay.runtime_score or '<missing>'}!={runtime_score}"
        )

    required_contract_flags = (
        "event_level",
        "one_decision_per_event",
        "official_settlement",
        "full_depth_entry",
    )
    for flag in required_contract_flags:
        if replay.decision_contract.get(flag) is not True:
            blockers.append(f"candidate_strategy_replay_missing_contract:{flag}")

    trades = numeric_metric(replay.metrics, "trade_count", "closed_trades")
    if trades is None:
        blockers.append("candidate_strategy_replay_missing_metric:trade_count")
    elif trades < min_replay_trade_count:
        blockers.append(
            "candidate_strategy_replay_trade_count_too_small:"
            f"{trades:.0f}<{min_replay_trade_count}"
        )

    unique_events = numeric_metric(replay.metrics, "unique_event_count")
    if unique_events is None:
        blockers.append("candidate_strategy_replay_missing_metric:unique_event_count")
    elif trades is not None and unique_events < min(trades, min_replay_trade_count):
        blockers.append(
            "candidate_strategy_replay_unique_event_count_too_small:"
            f"{unique_events:.0f}<{min(trades, min_replay_trade_count):.0f}"
        )

    fill_rate = numeric_metric(replay.metrics, "entry_fill_rate", "buy_fill_rate")
    if fill_rate is None:
        blockers.append("candidate_strategy_replay_missing_metric:entry_fill_rate")
    elif fill_rate < min_replay_fill_rate:
        blockers.append(
            "candidate_strategy_replay_entry_fill_rate_too_low:"
            f"{fill_rate:.4f}<{min_replay_fill_rate:.4f}"
        )

    roi = numeric_metric(replay.metrics, "roi", "return_on_notional")
    total_pnl = numeric_metric(replay.metrics, "total_pnl", "realized_pnl")
    if roi is None and total_pnl is None:
        blockers.append("candidate_strategy_replay_missing_profit_metric")
    elif roi is not None and roi < min_replay_roi:
        blockers.append(f"candidate_strategy_replay_roi_too_low:{roi:.6f}<{min_replay_roi:.6f}")
    elif roi is None and total_pnl is not None and total_pnl <= 0.0:
        blockers.append(f"candidate_strategy_replay_total_pnl_nonpositive:{total_pnl:.6f}")

    return blockers


def parse_autofactor_rows(report_text: str) -> list[AutoFactorRow]:
    rows: list[AutoFactorRow] = []
    in_section = False
    header: list[str] | None = None
    current_target = ""

    for line in report_text.splitlines():
        if line.startswith("# AutoFactor target="):
            current_target = line.split("=", 1)[1].strip()
            in_section = True
            header = None
            continue
        if not in_section:
            continue
        if line.startswith("rank,name,target,"):
            header = next(csv.reader([line]))
            continue
        if not line.strip():
            in_section = False
            header = None
            current_target = ""
            continue
        if header is None or line.startswith("===") or line.startswith("target labels"):
            continue
        values = next(csv.reader([line]))
        if len(values) != len(header):
            continue
        item = dict(zip(header, values))
        rows.append(
            AutoFactorRow(
                rank=parse_int(item["rank"]),
                name=item["name"],
                target=item.get("target") or current_target,
                decision=item["decision"],
                reason=item["reason"],
                n=parse_int(item["n"]),
                spearman_ic=parse_float(item["spearman_ic"]),
                pearson_ic=parse_float(item["pearson_ic"]),
                window_count=parse_int(item["window_count"]),
                icir=parse_float(item["icir"]),
                positive_window_ratio=parse_float(item["positive_window_ratio"]),
                symbol_count=parse_int(item.get("symbol_count", "0")),
                symbol_positive_ratio=parse_float(item.get("symbol_positive_ratio", "nan")),
                monotonicity=parse_float(item["monotonicity"]),
                top_bucket_n=parse_int(item.get("top_bucket_n", "0")),
                top_bucket_avg_label=parse_float(item["top_bucket_avg_label"]),
                top_bucket_positive_label_rate=parse_float(item["top_bucket_positive_label_rate"]),
                top_bucket_full_depth_entry_fill_rate=parse_float(
                    item.get("top_bucket_full_depth_entry_fill_rate", "nan")
                ),
                top_bucket_avg_entry_sweep_slip_bps=finite_or_none(
                    parse_float(item.get("top_bucket_avg_entry_sweep_slip_bps", "nan"))
                ),
                top_bucket_avg_entry_sweep_levels=finite_or_none(
                    parse_float(item.get("top_bucket_avg_entry_sweep_levels", "nan"))
                ),
                top_bucket_unique_event_count=parse_int(
                    item.get("top_bucket_unique_event_count", "0")
                ),
                top_bucket_max_event_decisions=parse_int(
                    item.get("top_bucket_max_event_decisions", "0")
                ),
                complexity=parse_int(item["complexity"]),
            )
        )

    return rows


MODEL_SPECIFIC_PRD_GATE_PREFIXES = (
    "symbol_holdout:",
    "walk_forward_oos:",
)


def is_autofactor_formula(mapping: dict[str, str] | None) -> bool:
    if not mapping:
        return False
    return mapping.get("runtime_score", "").startswith("autofactor_formula:")


def looks_like_mutable_recording_path(raw_path: str) -> bool:
    if not raw_path:
        return False
    name = Path(raw_path).name
    if not name.endswith(".ndjson"):
        return False
    return ARCHIVED_RECORDING_SUFFIX_RE.search(name.removesuffix(".ndjson")) is None


def global_gate_blockers(
    gate: PromotionGate, *, formula_specific: bool, suppress_global_fillability: bool
) -> list[str]:
    if gate.ready:
        return []
    if not formula_specific and not suppress_global_fillability:
        return ["promotion_gate_not_ready"]
    blockers = [
        item
        for item in gate.blocked_gates
        if not item.startswith(MODEL_SPECIFIC_PRD_GATE_PREFIXES)
    ]
    if suppress_global_fillability:
        blockers = [
            item
            for item in blockers
            if not item.startswith("global_full_depth_entry_fillability:")
        ]
    if not formula_specific and suppress_global_fillability and blockers:
        return ["promotion_gate_not_ready"]
    if blockers:
        return [f"global_promotion_gate_not_ready:{item}" for item in blockers]
    return []


def execution_quality_blockers(
    row: AutoFactorRow,
    quality: ExecutionQuality,
    *,
    max_avg_entry_sweep_slip_bps: float,
    max_top_bucket_entry_sweep_levels: float,
) -> list[str]:
    blockers: list[str] = []
    slip = row.top_bucket_avg_entry_sweep_slip_bps
    if slip is not None:
        if slip > max_avg_entry_sweep_slip_bps:
            blockers.append(
                "top_bucket_entry_sweep_slippage_too_high:"
                f"{slip:.2f}>{max_avg_entry_sweep_slip_bps:.2f}"
            )
    else:
        global_slip = quality.avg_entry_sweep_slip_bps
        if global_slip is not None and global_slip > max_avg_entry_sweep_slip_bps:
            blockers.append(
                "global_entry_sweep_slippage_too_high:"
                f"{global_slip:.2f}>{max_avg_entry_sweep_slip_bps:.2f}"
            )

    levels = row.top_bucket_avg_entry_sweep_levels
    if levels is not None and levels > max_top_bucket_entry_sweep_levels:
        blockers.append(
            "top_bucket_entry_sweep_levels_too_high:"
            f"{levels:.2f}>{max_top_bucket_entry_sweep_levels:.2f}"
        )
    return blockers


def evaluate(
    report_text: str,
    *,
    candidate_strategy_replay: CandidateStrategyReplay,
    allowed_targets: tuple[str, ...],
    required_strategy_profile: str,
    runtime_contracts: RuntimeContractResolver,
    min_factor_n: int,
    min_top_bucket_n: int,
    min_top_bucket_entry_fill_rate: float,
    max_avg_entry_sweep_slip_bps: float,
    max_top_bucket_entry_sweep_levels: float,
    min_window_count: int,
    min_replay_trade_count: int,
    min_replay_fill_rate: float,
    min_replay_roi: float,
    snapshot_contract: dict[str, Any] | None = None,
) -> dict[str, Any]:
    gate = parse_promotion_gate(report_text)
    quality = parse_execution_quality(report_text)
    rows = parse_autofactor_rows(report_text)
    snapshot_contract = snapshot_contract or {}
    sampled_snapshot_blocks_execution = snapshot_blocks_execution_claim(snapshot_contract)
    evaluated: list[EvaluatedFactor] = []

    for row in rows:
        blockers: list[str] = []
        resolution = runtime_contracts.resolve(row.name)
        mapping = resolution.runtime_mapping
        blockers.extend(resolution.blockers)
        formula_specific = is_autofactor_formula(mapping)
        suppress_global_fillability = (
            formula_specific
            and not sampled_snapshot_blocks_execution
            and row.top_bucket_full_depth_entry_fill_rate == row.top_bucket_full_depth_entry_fill_rate
            and row.top_bucket_full_depth_entry_fill_rate >= min_top_bucket_entry_fill_rate
            and row.top_bucket_avg_entry_sweep_slip_bps is not None
            and row.top_bucket_avg_entry_sweep_levels is not None
        )
        if not gate.ready and formula_specific and sampled_snapshot_blocks_execution:
            blockers.extend(
                f"snapshot_contract_blocks_execution_claim:{item}"
                for item in snapshot_contract.get("blocking_risk_flags", [])
            )
        blockers.extend(
            global_gate_blockers(
                gate,
                formula_specific=formula_specific,
                suppress_global_fillability=suppress_global_fillability,
            )
        )
        blockers.extend(
            execution_quality_blockers(
                row,
                quality,
                max_avg_entry_sweep_slip_bps=max_avg_entry_sweep_slip_bps,
                max_top_bucket_entry_sweep_levels=max_top_bucket_entry_sweep_levels,
            )
        )
        if row.target not in allowed_targets:
            blockers.append("target_not_allowed")
        if row.decision != "candidate" or row.reason != "passed":
            blockers.append(f"autofactor_not_candidate:{row.decision}:{row.reason}")
        if row.n < min_factor_n:
            blockers.append(f"factor_sample_too_small:{row.n}<{min_factor_n}")
        if row.top_bucket_n < min_top_bucket_n:
            blockers.append(
                f"top_bucket_sample_too_small:{row.top_bucket_n}<{min_top_bucket_n}"
            )
        if (
            row.top_bucket_full_depth_entry_fill_rate != row.top_bucket_full_depth_entry_fill_rate
            or row.top_bucket_full_depth_entry_fill_rate < min_top_bucket_entry_fill_rate
        ):
            blockers.append(
                "top_bucket_full_depth_entry_fill_rate_too_low:"
                f"{row.top_bucket_full_depth_entry_fill_rate:.4f}<"
                f"{min_top_bucket_entry_fill_rate:.4f}"
            )
        if row.window_count < min_window_count:
            blockers.append(f"window_count_too_small:{row.window_count}<{min_window_count}")
        if row.top_bucket_unique_event_count <= 0 or row.top_bucket_max_event_decisions <= 0:
            blockers.append("missing_one_event_decision_gate")
        elif row.top_bucket_max_event_decisions > 1:
            blockers.append(
                "one_event_decision_violation:"
                f"max_event_decisions={row.top_bucket_max_event_decisions}"
            )
        if not mapping:
            blockers.append("missing_runtime_strategy_mapping")
        else:
            mapped_profile = mapping.get("strategy_profile", "")
            if not mapped_profile:
                blockers.append("empty_runtime_strategy_profile")
            elif mapped_profile != required_strategy_profile:
                blockers.append(
                    f"runtime_profile_mismatch:{mapped_profile}!={required_strategy_profile}"
                )
        blockers.extend(
            candidate_strategy_replay_blockers(
                candidate_strategy_replay,
                mapping=mapping,
                expected_target=row.target,
                expected_horizon=autofactor_target_horizon(row.target),
                required_strategy_profile=required_strategy_profile,
                min_replay_trade_count=min_replay_trade_count,
                min_replay_fill_rate=min_replay_fill_rate,
                min_replay_roi=min_replay_roi,
            )
        )
        if formula_specific:
            if row.symbol_count < 2:
                blockers.append("formula_symbol_holdout_too_few_symbols")
            elif row.symbol_positive_ratio < 0.60:
                blockers.append(
                    f"formula_symbol_holdout_unstable:{row.symbol_positive_ratio:.4f}<0.60"
                )
        evaluated.append(
            EvaluatedFactor(
                factor=row,
                qualified=not blockers,
                blockers=blockers,
                runtime_mapping=mapping,
                runtime_contract=resolution.runtime_contract,
            )
        )

    qualified = [item for item in evaluated if item.qualified]
    return {
        "schema_version": 1,
        "decision": "qualified" if qualified else "blocked",
        "required_strategy_profile": required_strategy_profile,
        "allowed_targets": list(allowed_targets),
        "minimums": {
            "factor_n": min_factor_n,
            "top_bucket_n": min_top_bucket_n,
            "top_bucket_full_depth_entry_fill_rate": min_top_bucket_entry_fill_rate,
            "max_avg_entry_sweep_slip_bps": max_avg_entry_sweep_slip_bps,
            "max_top_bucket_entry_sweep_levels": max_top_bucket_entry_sweep_levels,
            "window_count": min_window_count,
            "candidate_strategy_replay_trade_count": min_replay_trade_count,
            "candidate_strategy_replay_entry_fill_rate": min_replay_fill_rate,
            "candidate_strategy_replay_roi": min_replay_roi,
        },
        "promotion_gate": asdict(gate),
        "execution_quality": asdict(quality),
        "source_snapshot_contract": snapshot_contract,
        "candidate_strategy_replay": asdict(candidate_strategy_replay),
        "qualified_strategies": [
            {
                "factor": asdict(item.factor),
                "runtime_mapping": item.runtime_mapping,
                "runtime_contract": item.runtime_contract,
            }
            for item in qualified
        ],
        "evaluated_factors": [
            {
                "factor": asdict(item.factor),
                "qualified": item.qualified,
                "blockers": item.blockers,
                "runtime_mapping": item.runtime_mapping,
                "runtime_contract": item.runtime_contract,
            }
            for item in evaluated
        ],
    }


def build_factor_registry(result: dict[str, Any]) -> dict[str, Any]:
    entries = []
    for item in result["evaluated_factors"]:
        factor = item["factor"]
        mapping = item["runtime_mapping"] or {}
        entries.append(
            {
                "name": factor["name"],
                "target": factor["target"],
                "status": "qualified" if item["qualified"] else "blocked",
                "autofactor_decision": factor["decision"],
                "autofactor_reason": factor["reason"],
                "blockers": item["blockers"],
                "runtime_mapping": mapping,
                "runtime_contract": item.get("runtime_contract") or {},
                "metrics": factor_metrics(AutoFactorRow(**factor)),
            }
        )
    return {
        "schema_version": 1,
        "kind": "autofactor_strategy_registry",
        "decision": result["decision"],
        "required_strategy_profile": result["required_strategy_profile"],
        "allowed_targets": result["allowed_targets"],
        "promotion_gate": result["promotion_gate"],
        "execution_quality": result["execution_quality"],
        "candidate_strategy_replay": result["candidate_strategy_replay"],
        "entries": entries,
    }


def build_strategy_handoff(result: dict[str, Any]) -> dict[str, Any]:
    strategies = []
    for item in result["qualified_strategies"]:
        factor = item["factor"]
        mapping = item["runtime_mapping"] or {}
        runtime_contract = item.get("runtime_contract") or {}
        strategies.append(
            {
                "name": factor["name"],
                "target": factor["target"],
                "strategy_profile": mapping.get("strategy_profile", ""),
                "strategy_family": mapping.get("strategy_family", ""),
                "runtime_score": mapping.get("runtime_score", ""),
                "runtime_contract": runtime_contract,
                "promotion_status": "ready_for_dry_run_handoff",
                "metrics": factor_metrics(AutoFactorRow(**factor)),
            }
        )

    return {
        "schema_version": 1,
        "kind": "autofactor_strategy_handoff",
        "status": "ready" if strategies else "blocked",
        "recommended_action": "create_dry_run_handoff" if strategies else "do_not_promote",
        "required_strategy_profile": result["required_strategy_profile"],
        "allowed_targets": result["allowed_targets"],
        "promotion_gate": result["promotion_gate"],
        "execution_quality": result["execution_quality"],
        "source_snapshot_contract": result["source_snapshot_contract"],
        "candidate_strategy_replay": result["candidate_strategy_replay"],
        "strategies": strategies,
        "blocked_factor_count": sum(
            1 for item in result["evaluated_factors"] if not item["qualified"]
        ),
    }


def render_handoff_markdown(handoff: dict[str, Any]) -> str:
    lines = [
        "# AutoFactor Dry-Run Strategy Handoff",
        "",
        f"Status: `{handoff['status']}`",
        f"Recommended action: `{handoff['recommended_action']}`",
        f"Required strategy profile: `{handoff['required_strategy_profile']}`",
        f"Allowed targets: `{', '.join(handoff['allowed_targets'])}`",
        f"Avg entry sweep slip bps: `{handoff['execution_quality'].get('avg_entry_sweep_slip_bps')}`",
        f"Candidate strategy replay ready: `{str(handoff['candidate_strategy_replay'].get('ready')).lower()}`",
        f"Candidate strategy replay id: `{handoff['candidate_strategy_replay'].get('candidate_replay_id')}`",
        f"Candidate strategy replay runtime score: `{handoff['candidate_strategy_replay'].get('runtime_score')}`",
        "",
    ]
    if handoff["status"] != "ready":
        lines.extend(
            [
                "No dry-run handoff issue or config should be created from this artifact.",
                "",
                f"Blocked factor count: `{handoff['blocked_factor_count']}`",
                "",
            ]
        )
        return "\n".join(lines)

    lines.extend(
        [
            "## Draft Issue",
            "",
            "Title: Promote AutoFactor strategy handoff to dry-run",
            "",
            "## Qualified Strategies",
            "",
            "| factor | target | strategy profile | runtime score | icir | top bucket avg label |",
            "| --- | --- | --- | --- | --- | --- |",
        ]
    )
    for strategy in handoff["strategies"]:
        metrics = strategy["metrics"]
        lines.append(
            "| {name} | {target} | {profile} | {runtime_score} | {icir:.6f} | {label:.6f} |".format(
                name=strategy["name"],
                target=strategy["target"],
                profile=strategy["strategy_profile"],
                runtime_score=strategy["runtime_score"],
                icir=metrics["icir"],
                label=metrics["top_bucket_avg_label"],
            )
        )

    lines.extend(
        [
            "",
            "## Dry-Run Config Contract",
            "",
            "```toml",
        ]
    )
    for idx, strategy in enumerate(handoff["strategies"], start=1):
        lines.extend(
            [
                f"[[autofactor_strategy_handoff]] # {idx}",
                f'name = "{strategy["name"]}"',
                f'target = "{strategy["target"]}"',
                f'strategy_profile = "{strategy["strategy_profile"]}"',
                f'strategy_family = "{strategy["strategy_family"]}"',
                f'runtime_score = "{strategy["runtime_score"]}"',
                'promotion_status = "ready_for_dry_run_handoff"',
                "",
            ]
        )
    lines.extend(
        [
            "```",
            "",
            "## Acceptance Criteria",
            "",
            "- Confirm the runtime score is implemented in the shared scorer.",
            "- Confirm the dry-run config uses the same full-depth execution gates as research.",
            "- Confirm replay parity remains ready for the source report.",
            "- Confirm the candidate strategy executable replay artifact remains attached and ready.",
            "- Start with small fixed stake and the existing kill switch.",
            "",
        ]
    )
    return "\n".join(lines)


def render_markdown(result: dict[str, Any]) -> str:
    lines = [
        "# AutoFactor Strategy Promotion Report",
        "",
        f"Decision: `{result['decision']}`",
        f"Required strategy profile: `{result['required_strategy_profile']}`",
        f"Allowed targets: `{', '.join(result['allowed_targets'])}`",
        "",
        "## Promotion Gate",
        "",
        f"- Ready: `{str(result['promotion_gate']['ready']).lower()}`",
        f"- Evidence: `{result['promotion_gate']['evidence']}`",
        f"- Avg entry sweep slip bps: `{result['execution_quality'].get('avg_entry_sweep_slip_bps')}`",
        "",
        "## Candidate Strategy Replay",
        "",
        f"- Ready: `{str(result['candidate_strategy_replay']['ready']).lower()}`",
        f"- Evidence: `{result['candidate_strategy_replay']['evidence']}`",
        f"- Candidate replay id: `{result['candidate_strategy_replay']['candidate_replay_id']}`",
        f"- Runtime score: `{result['candidate_strategy_replay']['runtime_score']}`",
        "",
        "## Qualified Strategies",
        "",
    ]
    if result["qualified_strategies"]:
        lines.append("| factor | target | runtime profile | icir | top bucket avg label |")
        lines.append("| --- | --- | --- | --- | --- |")
        for item in result["qualified_strategies"]:
            factor = item["factor"]
            mapping = item["runtime_mapping"] or {}
            lines.append(
                "| {name} | {target} | {profile} | {icir:.6f} | {label:.6f} |".format(
                    name=factor["name"],
                    target=factor["target"],
                    profile=mapping.get("strategy_profile", ""),
                    icir=factor["icir"],
                    label=factor["top_bucket_avg_label"],
                )
            )
    else:
        lines.append("No AutoFactor row qualifies for strategy handoff under the requested profile.")

    lines.extend(["", "## Evaluated Factors", ""])
    lines.append("| factor | target | decision | blockers |")
    lines.append("| --- | --- | --- | --- |")
    for item in result["evaluated_factors"]:
        factor = item["factor"]
        blockers = ", ".join(item["blockers"]) if item["blockers"] else "none"
        lines.append(
            f"| {factor['name']} | {factor['target']} | {factor['decision']} | {blockers} |"
        )
    lines.append("")
    return "\n".join(lines)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--report", required=True, help="factor_walk_forward_v2 report.txt")
    parser.add_argument("--output-json", default="")
    parser.add_argument("--output-md", default="")
    parser.add_argument("--output-registry-json", default="")
    parser.add_argument("--output-handoff-json", default="")
    parser.add_argument("--output-handoff-md", default="")
    parser.add_argument("--runtime-mapping-json", default="")
    parser.add_argument("--factor-registry-preview-json", default="")
    parser.add_argument("--require-runtime-contract", action="store_true")
    parser.add_argument("--snapshot-manifest-json", default="")
    parser.add_argument("--snapshot-data-audit-json", default="")
    parser.add_argument("--full-depth-execution-surface-json", default="")
    parser.add_argument(
        "--candidate-strategy-replay-json",
        default="",
        help="Strategy-level historical executable replay artifact for the selected runtime score.",
    )
    parser.add_argument(
        "--allowed-target",
        action="append",
        default=[],
        help="Allowed AutoFactor target. May be repeated.",
    )
    parser.add_argument("--required-strategy-profile", default="settlement_probability")
    parser.add_argument("--fail-if-blocked", action="store_true")
    parser.add_argument("--min-factor-n", type=int, default=100)
    parser.add_argument("--min-top-bucket-n", type=int, default=50)
    parser.add_argument("--min-top-bucket-entry-fill-rate", type=float, default=0.30)
    parser.add_argument("--max-avg-entry-sweep-slip-bps", type=float, default=200.0)
    parser.add_argument("--max-top-bucket-entry-sweep-levels", type=float, default=3.0)
    parser.add_argument("--min-window-count", type=int, default=4)
    parser.add_argument("--min-replay-trade-count", type=int, default=50)
    parser.add_argument("--min-replay-fill-rate", type=float, default=0.30)
    parser.add_argument("--min-replay-roi", type=float, default=0.0)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    report_text = Path(args.report).read_text(encoding="utf-8")
    allowed_targets = tuple(args.allowed_target or DEFAULT_ALLOWED_TARGETS)
    result = evaluate(
        report_text,
        candidate_strategy_replay=load_candidate_strategy_replay(
            args.candidate_strategy_replay_json or None
        ),
        allowed_targets=allowed_targets,
        required_strategy_profile=args.required_strategy_profile,
        runtime_contracts=RuntimeContractResolver(
            registry_preview_json=args.factor_registry_preview_json or None,
            runtime_mapping_json=args.runtime_mapping_json or None,
            require_runtime_contract=args.require_runtime_contract,
        ),
        min_factor_n=args.min_factor_n,
        min_top_bucket_n=args.min_top_bucket_n,
        min_top_bucket_entry_fill_rate=args.min_top_bucket_entry_fill_rate,
        max_avg_entry_sweep_slip_bps=args.max_avg_entry_sweep_slip_bps,
        max_top_bucket_entry_sweep_levels=args.max_top_bucket_entry_sweep_levels,
        min_window_count=args.min_window_count,
        min_replay_trade_count=args.min_replay_trade_count,
        min_replay_fill_rate=args.min_replay_fill_rate,
        min_replay_roi=args.min_replay_roi,
        snapshot_contract=load_snapshot_execution_contract(
            args.snapshot_manifest_json or None,
            full_depth_execution_surface_json=args.full_depth_execution_surface_json or None,
            data_audit_report_json=args.snapshot_data_audit_json or None,
        ),
    )

    if args.output_json:
        output = Path(args.output_json)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(json.dumps(result, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    if args.output_md:
        output = Path(args.output_md)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(render_markdown(result), encoding="utf-8")
    handoff = build_strategy_handoff(result)
    if args.output_registry_json:
        output = Path(args.output_registry_json)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(
            json.dumps(build_factor_registry(result), indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    if args.output_handoff_json:
        output = Path(args.output_handoff_json)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(
            json.dumps(handoff, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    if args.output_handoff_md:
        output = Path(args.output_handoff_md)
        output.parent.mkdir(parents=True, exist_ok=True)
        output.write_text(render_handoff_markdown(handoff), encoding="utf-8")

    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["decision"] == "qualified" or not args.fail_if_blocked else 3


if __name__ == "__main__":
    raise SystemExit(main())
