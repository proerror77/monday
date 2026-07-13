#!/usr/bin/env python3
"""Run multiple factor_walk_forward_v2 variants after one artifact download/build."""

from __future__ import annotations

import argparse
import json
import re
import shutil
import subprocess
import sys
import time
from dataclasses import dataclass
from pathlib import Path
from typing import Any


SWEEP_KEYS = {
    "label",
    "train_window_days",
    "train_window_hours",
    "test_window_days",
    "test_window_hours",
    "step_days",
    "step_hours",
    "lob_sample_secs",
    "pm_book_sample_secs",
    "observation_sample_secs",
    "max_quote_age_secs",
    "top_n",
    "min_observations",
    "top_quantile",
    "factor_name_filter",
    "report_suite",
    "data_quality_mode",
    "min_event_complete_events",
    "min_event_complete_rows",
    "min_promotion_entry_fill_rate",
}

OPTIONAL_SWEEP_KEYS = {
    "train_window_hours",
    "test_window_hours",
    "step_hours",
}


@dataclass(frozen=True)
class Variant:
    label: str
    values: dict[str, str]


def slugify(raw: str) -> str:
    slug = re.sub(r"[^A-Za-z0-9_.-]+", "-", raw.strip()).strip("-")
    return slug[:80] or "variant"


def load_sweep(raw: str, base: dict[str, str]) -> list[Variant]:
    if not raw.strip():
        return [Variant(label="base", values=dict(base))]
    payload = json.loads(raw)
    if isinstance(payload, dict):
        variants = payload.get("variants")
    else:
        variants = payload
    if not isinstance(variants, list) or not variants:
        raise SystemExit("sweep_json must be a non-empty JSON array or an object with variants[]")

    parsed: list[Variant] = []
    for index, item in enumerate(variants, start=1):
        if not isinstance(item, dict):
            raise SystemExit(f"sweep variant {index} must be an object")
        unknown = sorted(set(item) - SWEEP_KEYS)
        if unknown:
            raise SystemExit(f"sweep variant {index} has unknown keys: {', '.join(unknown)}")
        values = dict(base)
        for key, value in item.items():
            if key == "label":
                continue
            if isinstance(value, (dict, list)):
                raise SystemExit(f"sweep variant {index} value for {key} must be scalar")
            values[key] = str(value)
        label = str(item.get("label") or values.get("factor_name_filter") or f"variant-{index}")
        parsed.append(Variant(label=slugify(label), values=values))
    return parsed


def factor_args(
    args: argparse.Namespace,
    variant: Variant,
    replay_parity_json: str,
    alpha_search_output_dir: str = "",
) -> list[str]:
    values = variant.values
    command = [
        args.binary,
        "--snapshot-dir",
        args.snapshot_dir,
        "--symbols",
        args.symbols,
        "--start-ts",
        args.start_ts,
        "--end-ts",
        args.end_ts,
        "--stake-usd",
        args.stake_usd,
        "--train-window-days",
        values["train_window_days"],
        "--test-window-days",
        values["test_window_days"],
        "--step-days",
        values["step_days"],
        "--lob-sample-secs",
        values["lob_sample_secs"],
        "--pm-book-sample-secs",
        values["pm_book_sample_secs"],
        "--observation-sample-secs",
        values["observation_sample_secs"],
        "--max-quote-age-secs",
        values["max_quote_age_secs"],
        "--min-observations",
        values["min_observations"],
        "--top-quantile",
        values["top_quantile"],
        "--top-n",
        values["top_n"],
        "--report-suite",
        values["report_suite"],
        "--data-quality-mode",
        values["data_quality_mode"],
        "--min-event-complete-events",
        values["min_event_complete_events"],
        "--min-event-complete-rows",
        values["min_event_complete_rows"],
        "--min-promotion-entry-fill-rate",
        values["min_promotion_entry_fill_rate"],
    ]
    if values.get("train_window_hours"):
        command.extend(["--train-window-hours", values["train_window_hours"]])
    if values.get("test_window_hours"):
        command.extend(["--test-window-hours", values["test_window_hours"]])
    if values.get("step_hours"):
        command.extend(["--step-hours", values["step_hours"]])
    if values.get("factor_name_filter"):
        command.extend(["--factor-name-filter", values["factor_name_filter"]])
    if replay_parity_json:
        command.extend(["--replay-parity-json", replay_parity_json])
    if args.candidate_strategy_replay_json:
        command.extend(["--candidate-strategy-replay-json", args.candidate_strategy_replay_json])
    if args.require_deribit:
        command.append("--require-deribit")
    if alpha_search_output_dir:
        command.extend(["--alpha-search-output-dir", alpha_search_output_dir])
    if args.alpha_search_plan_json:
        command.extend(["--alpha-search-plan-json", args.alpha_search_plan_json])
    if args.alpha_search_state_json:
        command.extend(["--alpha-search-state-json", args.alpha_search_state_json])
    if args.alpha_search_llm_prior_json:
        command.extend(["--alpha-search-llm-prior-json", args.alpha_search_llm_prior_json])
    if args.alpha_zoo_snapshot_json:
        command.extend(["--alpha-zoo-snapshot-json", args.alpha_zoo_snapshot_json])
    return command


def promotion_args(
    args: argparse.Namespace,
    report: Path,
    variant_dir: Path,
    candidate_strategy_replay_json: str | None = None,
) -> list[str]:
    command = [
        sys.executable,
        args.evaluator,
        "--report",
        str(report),
        "--output-json",
        str(variant_dir / "autofactor-strategy-promotion.json"),
        "--output-md",
        str(variant_dir / "autofactor-strategy-promotion.md"),
        "--output-registry-json",
        str(variant_dir / "autofactor-factor-registry.json"),
        "--output-handoff-json",
        str(variant_dir / "autofactor-strategy-handoff.json"),
        "--output-handoff-md",
        str(variant_dir / "autofactor-strategy-handoff.md"),
        "--required-strategy-profile",
        args.required_strategy_profile,
    ]
    replay_json = candidate_strategy_replay_json or str(
        variant_dir / "candidate-strategy-replay.json"
    )
    command.extend(["--candidate-strategy-replay-json", replay_json])
    if args.snapshot_manifest_json:
        command.extend(["--snapshot-manifest-json", args.snapshot_manifest_json])
    if args.snapshot_data_audit_json:
        command.extend(["--snapshot-data-audit-json", args.snapshot_data_audit_json])
    if args.full_depth_execution_surface_json:
        command.extend(
            ["--full-depth-execution-surface-json", args.full_depth_execution_surface_json]
        )
    registry_preview = factor_registry_preview_path(variant_dir, args.allowed_target)
    if registry_preview:
        command.extend(["--factor-registry-preview-json", str(registry_preview)])
        command.append("--require-runtime-contract")
    for target in args.allowed_target:
        command.extend(["--allowed-target", target])
    return command


def candidate_replay_args(
    args: argparse.Namespace,
    report: Path,
    variant_dir: Path,
) -> list[str]:
    command = [
        sys.executable,
        args.candidate_replay_builder,
        "--report",
        str(report),
        "--output-json",
        str(variant_dir / "candidate-strategy-replay.json"),
        "--output-md",
        str(variant_dir / "candidate-strategy-replay.md"),
        "--required-strategy-profile",
        args.required_strategy_profile,
        "--stake-usd",
        args.stake_usd,
        "--evidence",
        str(report),
    ]
    registry_preview = factor_registry_preview_path(variant_dir, args.allowed_target)
    if registry_preview:
        command.extend(["--factor-registry-preview-json", str(registry_preview)])
        command.append("--require-runtime-contract")
    if args.snapshot_manifest_json:
        command.extend(["--snapshot-manifest-json", args.snapshot_manifest_json])
    if args.snapshot_data_audit_json:
        command.extend(["--snapshot-data-audit-json", args.snapshot_data_audit_json])
    if args.full_depth_execution_surface_json:
        command.extend(
            ["--full-depth-execution-surface-json", args.full_depth_execution_surface_json]
        )
    for target in args.allowed_target:
        command.extend(["--allowed-target", target])
    return command


def factor_registry_preview_path(variant_dir: Path, allowed_targets: list[str]) -> Path | None:
    alpha_root = variant_dir / "alpha-search"
    if not alpha_root.exists():
        return None
    previews = sorted(alpha_root.rglob("factor-registry-preview.json"))
    if not previews:
        return None

    targets = allowed_targets or ["full_depth_settlement_executable_pnl"]
    for target in targets:
        exact = alpha_root / target / "factor-registry-preview.json"
        if exact.exists():
            return exact

    for target in targets:
        for preview in previews:
            try:
                payload = json.loads(preview.read_text(encoding="utf-8"))
            except (OSError, json.JSONDecodeError) as err:
                raise SystemExit(f"invalid factor registry preview {preview}: {err}") from err
            if payload.get("target") == target:
                return preview

    available = ", ".join(str(path.relative_to(alpha_root).parent) for path in previews)
    requested = ", ".join(targets)
    raise SystemExit(
        "no factor registry preview matched allowed target(s): "
        f"{requested}; available target dirs: {available}"
    )


def ranked_factor_rows(promotion: dict[str, Any], allowed_targets: set[str]) -> list[dict[str, Any]]:
    evaluated = promotion.get("evaluated_factors") or []
    rows: list[dict[str, Any]] = []
    for item in evaluated:
        factor = item.get("factor") or {}
        if factor.get("target") not in allowed_targets:
            continue
        mapping = item.get("runtime_mapping") or {}
        blockers = item.get("blockers") or []
        rows.append(
            {
                "name": factor.get("name", ""),
                "target": factor.get("target", ""),
                "decision": factor.get("decision", ""),
                "reason": factor.get("reason", ""),
                "rank": factor.get("rank", 0),
                "spearman_ic": factor.get("spearman_ic"),
                "pearson_ic": factor.get("pearson_ic"),
                "icir": factor.get("icir"),
                "positive_window_ratio": factor.get("positive_window_ratio"),
                "symbol_positive_ratio": factor.get("symbol_positive_ratio"),
                "monotonicity": factor.get("monotonicity"),
                "n": factor.get("n"),
                "window_count": factor.get("window_count"),
                "top_bucket_n": factor.get("top_bucket_n"),
                "top_bucket_avg_label": factor.get("top_bucket_avg_label"),
                "top_bucket_positive_label_rate": factor.get("top_bucket_positive_label_rate"),
                "top_bucket_full_depth_entry_fill_rate": factor.get(
                    "top_bucket_full_depth_entry_fill_rate"
                ),
                "complexity": factor.get("complexity"),
                "qualified": bool(item.get("qualified")),
                "runtime_mapping": mapping,
                "runtime_mappable": bool(mapping)
                and not any(is_runtime_contract_blocker(str(blocker)) for blocker in blockers)
                and "empty_runtime_strategy_profile" not in blockers,
                "blockers": blockers,
            }
        )
    return rows


def is_runtime_contract_blocker(blocker: str) -> bool:
    return (
        blocker.startswith("runtime_profile_mismatch:")
        or blocker.startswith("runtime_contract_")
        or blocker.startswith("runtime_input_")
        or blocker.startswith("missing_runtime_")
        or blocker.startswith("incomplete_runtime_contract_")
    )


def replay_identity_mismatch_blockers(promotion: dict[str, Any]) -> list[str]:
    prefixes = (
        "candidate_strategy_replay_runtime_score_mismatch:",
        "candidate_strategy_replay_target_mismatch:",
        "candidate_strategy_replay_horizon_mismatch:",
        "candidate_strategy_replay_contract_target_mismatch:",
        "candidate_strategy_replay_contract_horizon_mismatch:",
    )
    evaluated = promotion.get("evaluated_factors") or []
    selected_items: list[dict[str, Any]] = []
    for item in evaluated:
        factor = item.get("factor") if isinstance(item.get("factor"), dict) else {}
        blockers = [str(blocker) for blocker in item.get("blockers") or []]
        if factor.get("decision") != "candidate" or factor.get("reason") != "passed":
            continue
        if not item.get("runtime_mapping"):
            continue
        if "empty_runtime_strategy_profile" in blockers:
            continue
        if any(is_runtime_contract_blocker(blocker) for blocker in blockers):
            continue
        selected_items.append(item)
    if not selected_items:
        return []

    for item in selected_items:
        blockers = [str(blocker) for blocker in item.get("blockers") or []]
        if not any(blocker.startswith(prefixes) for blocker in blockers):
            return []

    def rank(item: dict[str, Any]) -> float:
        factor = item.get("factor") if isinstance(item.get("factor"), dict) else {}
        try:
            return float(factor.get("rank") or 10_000)
        except (TypeError, ValueError):
            return 10_000.0

    selected = min(selected_items, key=rank)
    mismatches: list[str] = []
    for blocker in selected.get("blockers") or []:
        blocker_text = str(blocker)
        if blocker_text.startswith(prefixes):
            mismatches.append(blocker_text)
    return sorted(set(mismatches))


def _factor_score(item: dict[str, Any]) -> tuple[float, float, float, float, float, float]:
    try:
        rank = float(item.get("rank") or 10_000)
    except (TypeError, ValueError):
        rank = 10_000.0
    return (
        1.0 if item["decision"] == "candidate" and item["reason"] == "passed" else 0.0,
        float(item.get("positive_window_ratio") or 0.0),
        float(item.get("symbol_positive_ratio") or 0.0),
        float(item.get("icir") or 0.0),
        float(item.get("spearman_ic") or 0.0),
        -rank,
    )


def best_factor(promotion: dict[str, Any], allowed_targets: set[str]) -> dict[str, Any] | None:
    candidates = ranked_factor_rows(promotion, allowed_targets)
    if not candidates:
        return None

    def score(item: dict[str, Any]) -> tuple[float, float, float, float, float]:
        return (
            1.0 if item["qualified"] else 0.0,
            *_factor_score(item),
        )

    return max(candidates, key=score)


def best_factor_by_kind(
    promotion: dict[str, Any],
    allowed_targets: set[str],
    *,
    kind: str,
) -> dict[str, Any] | None:
    candidates = ranked_factor_rows(promotion, allowed_targets)
    if kind == "qualified":
        candidates = [item for item in candidates if item["qualified"]]
    elif kind == "runtime_mappable":
        candidates = [item for item in candidates if item["runtime_mappable"]]
    elif kind != "discovery":
        raise ValueError(f"unknown factor kind: {kind}")
    if not candidates:
        return None
    return max(candidates, key=_factor_score)


def write_markdown(summary: dict[str, Any], path: Path) -> None:
    lines = [
        "# Factor Walk-Forward Sweep",
        "",
        f"- Variants: `{len(summary['variants'])}`",
        f"- Total elapsed seconds: `{summary['total_elapsed_seconds']:.2f}`",
        f"- Best variant: `{summary.get('best_variant') or '<none>'}`",
        "",
        "| variant | status | decision | qualified | elapsed_s | best qualified strategy | best runtime-mappable factor | best discovery factor | discovery blockers |",
        "| --- | --- | --- | ---: | ---: | --- | --- | --- | --- |",
    ]
    for item in summary["variants"]:
        discovery = item.get("best_discovery_factor") or item.get("best_factor") or {}
        runtime = item.get("best_runtime_mappable_factor") or {}
        qualified = item.get("best_qualified_strategy") or {}
        lines.append(
            "| {variant} | {status} | {decision} | {qualified_count} | {elapsed:.2f} | {qualified_factor} | {runtime_factor} | {discovery_factor} | {blockers} |".format(
                variant=item["label"],
                status=item["status"],
                decision=item.get("decision") or "",
                qualified_count=item.get("qualified_count", 0),
                elapsed=item.get("elapsed_seconds", 0.0),
                qualified_factor=qualified.get("name", ""),
                runtime_factor=runtime.get("name", ""),
                discovery_factor=discovery.get("name", ""),
                blockers=", ".join(discovery.get("blockers") or []),
            )
        )
    path.write_text("\n".join(lines) + "\n", encoding="utf-8")


def promote_best_variant(best: dict[str, Any] | None, output_dir: Path) -> None:
    if not best:
        return
    source = Path(best["path"])
    for name in [
        "report.txt",
        "autofactor-strategy-promotion.json",
        "autofactor-strategy-promotion.md",
        "autofactor-factor-registry.json",
        "autofactor-strategy-handoff.json",
        "autofactor-strategy-handoff.md",
        "candidate-strategy-replay.json",
        "candidate-strategy-replay.md",
        "evaluator-output.json",
    ]:
        src = source / name
        if src.exists():
            shutil.copy2(src, output_dir / name)
    alpha_src = source / "alpha-search"
    alpha_dst = output_dir / "alpha-search"
    if alpha_src.exists():
        if alpha_dst.exists():
            shutil.rmtree(alpha_dst)
        shutil.copytree(alpha_src, alpha_dst)
    (output_dir / "source.txt").write_text(
        f"canonical_result=snapshot_artifact_sweep\nbest_variant={best['label']}\n",
        encoding="utf-8",
    )


def run_variant(
    args: argparse.Namespace,
    variant: Variant,
    index: int,
    replay_parity_json: str,
    allowed_targets: set[str],
) -> dict[str, Any]:
    variant_dir = Path(args.output_dir) / f"{index:03d}-{variant.label}"
    variant_dir.mkdir(parents=True, exist_ok=True)
    (variant_dir / "variant.json").write_text(
        json.dumps({"label": variant.label, "values": variant.values}, indent=2, sort_keys=True),
        encoding="utf-8",
    )
    report_path = variant_dir / "report.txt"
    variant_alpha_search_output_dir = ""
    if args.alpha_search_output_dir:
        variant_alpha_search_output_dir = str(variant_dir / "alpha-search")
    command = factor_args(
        args,
        variant,
        replay_parity_json,
        variant_alpha_search_output_dir,
    )
    started = time.monotonic()
    with report_path.open("w", encoding="utf-8") as report:
        result = subprocess.run(
            command,
            cwd=args.cwd,
            stdout=report,
            stderr=subprocess.STDOUT,
            text=True,
            check=False,
        )
    elapsed = time.monotonic() - started
    item: dict[str, Any] = {
        "label": variant.label,
        "path": str(variant_dir),
        "status": "failed" if result.returncode else "completed",
        "exit_code": result.returncode,
        "elapsed_seconds": elapsed,
        "variant": variant.values,
    }
    if result.returncode:
        return item

    if not args.candidate_strategy_replay_json:
        replay_result = subprocess.run(
            candidate_replay_args(args, report_path, variant_dir),
            cwd=args.cwd,
            text=True,
            capture_output=True,
            check=False,
        )
        (variant_dir / "candidate-strategy-replay-output.json").write_text(
            replay_result.stdout,
            encoding="utf-8",
        )
        if replay_result.stderr:
            (variant_dir / "candidate-strategy-replay-stderr.txt").write_text(
                replay_result.stderr,
                encoding="utf-8",
            )
        item["candidate_replay_exit_code"] = replay_result.returncode
        if replay_result.returncode != 0:
            item["status"] = "candidate_replay_failed"
            return item

    eval_result = subprocess.run(
        promotion_args(
            args,
            report_path,
            variant_dir,
            args.candidate_strategy_replay_json or None,
        ),
        cwd=args.cwd,
        text=True,
        capture_output=True,
        check=False,
    )
    (variant_dir / "evaluator-output.json").write_text(eval_result.stdout, encoding="utf-8")
    if eval_result.stderr:
        (variant_dir / "evaluator-stderr.txt").write_text(eval_result.stderr, encoding="utf-8")
    item["evaluator_exit_code"] = eval_result.returncode
    if eval_result.returncode not in {0, 3}:
        item["status"] = "evaluation_failed"
        return item
    promotion = json.loads(
        (variant_dir / "autofactor-strategy-promotion.json").read_text(encoding="utf-8")
    )
    mismatches = replay_identity_mismatch_blockers(promotion)
    if args.candidate_strategy_replay_json and mismatches:
        item["ignored_candidate_strategy_replay_json"] = args.candidate_strategy_replay_json
        item["ignored_candidate_strategy_replay_reasons"] = mismatches
        replay_result = subprocess.run(
            candidate_replay_args(args, report_path, variant_dir),
            cwd=args.cwd,
            text=True,
            capture_output=True,
            check=False,
        )
        (variant_dir / "candidate-strategy-replay-output.json").write_text(
            replay_result.stdout,
            encoding="utf-8",
        )
        if replay_result.stderr:
            (variant_dir / "candidate-strategy-replay-stderr.txt").write_text(
                replay_result.stderr,
                encoding="utf-8",
            )
        item["candidate_replay_exit_code"] = replay_result.returncode
        if replay_result.returncode != 0:
            item["status"] = "candidate_replay_failed"
            return item
        eval_result = subprocess.run(
            promotion_args(args, report_path, variant_dir),
            cwd=args.cwd,
            text=True,
            capture_output=True,
            check=False,
        )
        (variant_dir / "evaluator-output.json").write_text(eval_result.stdout, encoding="utf-8")
        if eval_result.stderr:
            (variant_dir / "evaluator-stderr.txt").write_text(
                eval_result.stderr,
                encoding="utf-8",
            )
        item["evaluator_exit_code"] = eval_result.returncode
        if eval_result.returncode not in {0, 3}:
            item["status"] = "evaluation_failed"
            return item
        promotion = json.loads(
            (variant_dir / "autofactor-strategy-promotion.json").read_text(encoding="utf-8")
        )
    elif args.candidate_strategy_replay_json:
        source_replay = Path(args.candidate_strategy_replay_json)
        if source_replay.exists():
            shutil.copy2(source_replay, variant_dir / "candidate-strategy-replay.json")
    item["decision"] = promotion.get("decision")
    item["qualified_count"] = len(promotion.get("qualified_strategies") or [])
    item["promotion_gate_ready"] = bool((promotion.get("promotion_gate") or {}).get("ready"))
    item["best_factor"] = best_factor(promotion, allowed_targets)
    item["best_discovery_factor"] = best_factor_by_kind(
        promotion,
        allowed_targets,
        kind="discovery",
    )
    item["best_runtime_mappable_factor"] = best_factor_by_kind(
        promotion,
        allowed_targets,
        kind="runtime_mappable",
    )
    item["best_qualified_strategy"] = best_factor_by_kind(
        promotion,
        allowed_targets,
        kind="qualified",
    )
    return item


def variant_selection_score(item: dict[str, Any]) -> tuple[float, ...]:
    factor = (
        item.get("best_qualified_strategy")
        or item.get("best_runtime_mappable_factor")
        or item.get("best_factor")
        or {}
    )
    return (
        float(item.get("qualified_count", 0) or 0),
        1.0 if item.get("best_qualified_strategy") else 0.0,
        1.0 if item.get("best_runtime_mappable_factor") else 0.0,
        float(factor.get("top_bucket_full_depth_entry_fill_rate") or 0.0),
        float(factor.get("top_bucket_avg_label") or 0.0),
        float(factor.get("icir") or 0.0),
        float(factor.get("positive_window_ratio") or 0.0),
        float(factor.get("spearman_ic") or 0.0),
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--binary", required=True)
    parser.add_argument("--evaluator", default="scripts/evaluate_autofactor_strategy_promotion.py")
    parser.add_argument(
        "--candidate-replay-builder",
        default="scripts/build_autofactor_candidate_strategy_replay.py",
    )
    parser.add_argument("--snapshot-dir", required=True)
    parser.add_argument("--output-dir", required=True)
    parser.add_argument("--symbols", required=True)
    parser.add_argument("--start-ts", required=True)
    parser.add_argument("--end-ts", required=True)
    parser.add_argument("--stake-usd", required=True)
    parser.add_argument("--replay-parity-json", default="")
    parser.add_argument("--candidate-strategy-replay-json", default="")
    parser.add_argument("--snapshot-manifest-json", default="")
    parser.add_argument("--snapshot-data-audit-json", default="")
    parser.add_argument("--full-depth-execution-surface-json", default="")
    parser.add_argument("--alpha-search-output-dir", default="")
    parser.add_argument("--alpha-search-plan-json", default="")
    parser.add_argument("--alpha-search-state-json", default="")
    parser.add_argument("--alpha-search-llm-prior-json", default="")
    parser.add_argument("--alpha-zoo-snapshot-json", default="")
    parser.add_argument("--required-strategy-profile", default="settlement_probability")
    parser.add_argument("--require-deribit", action="store_true")
    parser.add_argument("--allowed-target", action="append", default=[])
    parser.add_argument("--sweep-json", default="")
    parser.add_argument("--cwd", default=".")
    parser.add_argument("--fail-if-all-failed", action="store_true")
    parser.add_argument("--fail-if-blocked", action="store_true")
    parser.add_argument("--dry-run", action="store_true")
    for key in sorted(SWEEP_KEYS - {"label"}):
        parser.add_argument(
            f"--{key.replace('_', '-')}",
            required=key not in OPTIONAL_SWEEP_KEYS,
            default="",
        )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    base = {key: getattr(args, key) for key in sorted(SWEEP_KEYS - {"label"})}
    variants = load_sweep(args.sweep_json, base)
    output_dir = Path(args.output_dir)
    output_dir.mkdir(parents=True, exist_ok=True)
    allowed_targets = set(args.allowed_target or ["full_depth_settlement_executable_pnl"])

    if args.dry_run:
        summary = {
            "schema_version": "factor_walk_forward_sweep_v1",
            "dry_run": True,
            "variants": [{"label": variant.label, "variant": variant.values} for variant in variants],
        }
        (output_dir / "sweep-summary.json").write_text(
            json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8"
        )
        return 0

    started = time.monotonic()
    variant_summaries = [
        run_variant(args, variant, index, args.replay_parity_json, allowed_targets)
        for index, variant in enumerate(variants, start=1)
    ]
    completed = [item for item in variant_summaries if item["status"] == "completed"]
    best = None
    if completed:
        best = max(
            completed,
            key=variant_selection_score,
        )
    summary = {
        "schema_version": "factor_walk_forward_sweep_v1",
        "total_elapsed_seconds": time.monotonic() - started,
        "variant_count": len(variants),
        "completed_count": len(completed),
        "best_variant": best["label"] if best else None,
        "variants": variant_summaries,
    }
    (output_dir / "sweep-summary.json").write_text(
        json.dumps(summary, indent=2, sort_keys=True), encoding="utf-8"
    )
    write_markdown(summary, output_dir / "sweep-summary.md")
    promote_best_variant(best, output_dir)
    if args.fail_if_all_failed and not completed:
        return 2
    if args.fail_if_blocked and (not best or best.get("decision") != "qualified"):
        return 3
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
