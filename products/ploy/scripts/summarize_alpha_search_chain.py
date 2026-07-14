#!/usr/bin/env python3
"""Summarize downloaded Factor Walk-Forward V2 alpha-search artifacts."""

from __future__ import annotations

import argparse
import json
import re
from pathlib import Path
from typing import Any


RUN_ID_RE = re.compile(r"(\d{8,})")


def load_json(path: Path) -> Any:
    with path.open(encoding="utf-8") as handle:
        return json.load(handle)


def optional_json(path: Path) -> Any:
    if not path.exists():
        return None
    return load_json(path)


def artifact_root(path: Path) -> Path:
    if (path / "factor-walk-forward-v2").is_dir():
        return path / "factor-walk-forward-v2"
    return path


def infer_run_id(path: Path) -> str | None:
    for part in reversed(path.parts):
        if match := RUN_ID_RE.search(part):
            return match.group(1)
    return None


def selected_factor(plan: Any) -> str | None:
    if not isinstance(plan, dict):
        return None
    selected = plan.get("selected_nodes")
    if not isinstance(selected, list) or not selected:
        return None
    first = selected[0]
    if not isinstance(first, dict):
        return None
    value = first.get("factor_name")
    return str(value) if value is not None else None


def summarize_artifact(path: Path) -> dict[str, Any]:
    root = artifact_root(path)
    target = "full_depth_settlement_executable_pnl"
    feedback = optional_json(root / "alpha-search" / target / "search-feedback.json") or {}
    plan = optional_json(root / "alpha-search" / target / "mcts-expansion-plan.json") or {}
    handoff = optional_json(root / "autofactor-strategy-handoff.json") or {}
    promotion = optional_json(root / "autofactor-strategy-promotion.json") or {}
    chain = optional_json(path / "alpha-search-chain" / "chain-decision.json")
    if chain is None:
        chain = optional_json(root.parent / "alpha-search-chain" / "chain-decision.json") or {}

    return {
        "artifact": str(path),
        "run_id": str(chain.get("current_run_id") or infer_run_id(path) or ""),
        "target": str(feedback.get("target") or target),
        "candidate_count": feedback.get("candidate_count"),
        "passed_count": feedback.get("passed_count"),
        "best_reward": feedback.get("best_reward"),
        "best_selected_factor": selected_factor(plan),
        "handoff_status": handoff.get("status"),
        "recommended_action": handoff.get("recommended_action"),
        "qualified_strategy_count": len(handoff.get("qualified_strategies") or []),
        "promotion_decision": promotion.get("decision"),
        "chain_reason": chain.get("reason"),
        "chain_should_dispatch": chain.get("should_dispatch"),
        "chain_next_remaining": chain.get("next_remaining"),
        "chain_previous_best_reward": chain.get("previous_best_reward"),
        "chain_current_best_reward": chain.get("current_best_reward"),
    }


def write_markdown(summary: dict[str, Any], path: Path) -> None:
    rows = summary["runs"]
    lines = [
        "# Alpha Search Chain Summary",
        "",
        f"- Run count: `{len(rows)}`",
        f"- Best run: `{summary.get('best_run_id') or 'n/a'}`",
        f"- Best reward: `{summary.get('best_reward') if summary.get('best_reward') is not None else 'n/a'}`",
        f"- Ready handoff count: `{summary.get('ready_handoff_count')}`",
        "",
        "| Run | Candidates | Passed | Best reward | Best selected factor | Handoff | Action | Chain |",
        "| --- | ---: | ---: | ---: | --- | --- | --- | --- |",
    ]
    for row in rows:
        lines.append(
            "| {run} | {candidates} | {passed} | {reward} | {factor} | {handoff} | {action} | {chain} |".format(
                run=row.get("run_id") or "n/a",
                candidates=row.get("candidate_count") if row.get("candidate_count") is not None else "n/a",
                passed=row.get("passed_count") if row.get("passed_count") is not None else "n/a",
                reward=row.get("best_reward") if row.get("best_reward") is not None else "n/a",
                factor=row.get("best_selected_factor") or "n/a",
                handoff=row.get("handoff_status") or "n/a",
                action=row.get("recommended_action") or "n/a",
                chain=row.get("chain_reason") or "n/a",
            )
        )
    lines.append("")
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("\n".join(lines), encoding="utf-8")


def build_summary(paths: list[Path]) -> dict[str, Any]:
    runs = [summarize_artifact(path) for path in paths]
    best = max(
        (row for row in runs if isinstance(row.get("best_reward"), int | float)),
        key=lambda row: row["best_reward"],
        default=None,
    )
    return {
        "run_count": len(runs),
        "best_run_id": best.get("run_id") if best else None,
        "best_reward": best.get("best_reward") if best else None,
        "best_selected_factor": best.get("best_selected_factor") if best else None,
        "ready_handoff_count": sum(1 for row in runs if row.get("handoff_status") == "ready"),
        "blocked_handoff_count": sum(1 for row in runs if row.get("handoff_status") == "blocked"),
        "runs": runs,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("artifact_dir", nargs="+", help="Downloaded factor-walk-forward artifact directory")
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--output-md", required=True)
    args = parser.parse_args()

    summary = build_summary([Path(value) for value in args.artifact_dir])
    output_json = Path(args.output_json)
    output_json.parent.mkdir(parents=True, exist_ok=True)
    with output_json.open("w", encoding="utf-8") as handle:
        json.dump(summary, handle, indent=2, sort_keys=True)
        handle.write("\n")
    write_markdown(summary, Path(args.output_md))


if __name__ == "__main__":
    main()
