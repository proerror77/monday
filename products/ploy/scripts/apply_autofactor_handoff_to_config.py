#!/usr/bin/env python3
"""Apply a ready AutoFactor dry-run handoff to a strategy TOML config.

This is the final promotion step after the strict evaluator has produced a
ready `autofactor-strategy-handoff.json`. It intentionally updates only the
runtime score field so the existing dry-run execution, risk, and kill-switch
settings remain under reviewable config ownership.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
from pathlib import Path
from typing import Any


RUNTIME_SCORE_RE = re.compile(
    r'^(\s*three_layer_autofactor_runtime_score\s*=\s*)".*"\s*$'
)
PROFILE_RE = re.compile(r'^\s*three_layer_strategy_profile\s*=\s*"([^"]+)"\s*$')


def load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def select_strategy(handoff: dict[str, Any], strategy_name: str) -> dict[str, Any]:
    if handoff.get("status") != "ready":
        raise SystemExit(f"handoff status is {handoff.get('status', 'missing')}; expected ready")
    strategies = handoff.get("strategies")
    if not isinstance(strategies, list) or not strategies:
        raise SystemExit("handoff has no qualified strategies")
    if strategy_name:
        for strategy in strategies:
            if strategy.get("name") == strategy_name:
                return strategy
        raise SystemExit(f"strategy {strategy_name!r} not found in handoff")
    return strategies[0]


def replace_or_insert_runtime_score(
    config_text: str,
    *,
    runtime_score: str,
    expected_strategy_profile: str,
) -> tuple[str, str | None, str]:
    lines = config_text.splitlines(keepends=True)
    profile_index: int | None = None
    observed_profile: str | None = None
    score_index: int | None = None
    previous_score: str | None = None

    for index, line in enumerate(lines):
        profile_match = PROFILE_RE.match(line.rstrip("\n"))
        if profile_match:
            profile_index = index
            observed_profile = profile_match.group(1)
        score_match = RUNTIME_SCORE_RE.match(line.rstrip("\n"))
        if score_match:
            score_index = index
            previous_score = line.split("=", 1)[1].strip().strip('"')

    if expected_strategy_profile and observed_profile != expected_strategy_profile:
        raise SystemExit(
            "strategy config profile mismatch: "
            f"observed={observed_profile!r} expected={expected_strategy_profile!r}"
        )

    new_line = f'three_layer_autofactor_runtime_score = "{runtime_score}"\n'
    if score_index is not None:
        lines[score_index] = new_line
        action = "updated"
    else:
        if profile_index is None:
            raise SystemExit("cannot insert runtime score: three_layer_strategy_profile not found")
        insert_at = profile_index + 1
        lines.insert(insert_at, new_line)
        action = "inserted"

    return "".join(lines), previous_score, action


def render_summary_markdown(summary: dict[str, Any]) -> str:
    strategy = summary["selected_strategy"]
    return "\n".join(
        [
            "# AutoFactor Config Handoff",
            "",
            f"- Status: `{summary['status']}`",
            f"- Config: `{summary['config_path']}`",
            f"- Changed: `{str(summary['changed']).lower()}`",
            f"- Action: `{summary['action']}`",
            "- Previous runtime score: "
            f"`{summary.get('previous_runtime_score') or 'none'}`",
            f"- New runtime score: `{summary['runtime_score']}`",
            f"- Strategy: `{strategy['name']}`",
            f"- Target: `{strategy['target']}`",
            f"- Strategy profile: `{strategy['strategy_profile']}`",
            f"- ICIR: `{strategy['metrics']['icir']}`",
            f"- Top bucket avg label: `{strategy['metrics']['top_bucket_avg_label']}`",
            "",
        ]
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--handoff-json", required=True)
    parser.add_argument("--config", required=True)
    parser.add_argument("--strategy-name", default="")
    parser.add_argument("--expected-strategy-profile", default="settlement_probability")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--output-json", default="")
    parser.add_argument("--output-md", default="")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    handoff_path = Path(args.handoff_json)
    config_path = Path(args.config)
    handoff = load_json(handoff_path)
    strategy = select_strategy(handoff, args.strategy_name)
    runtime_score = str(strategy.get("runtime_score") or "")
    strategy_profile = str(strategy.get("strategy_profile") or "")
    if not runtime_score.startswith("autofactor_formula:"):
        raise SystemExit(f"unsupported runtime_score for config handoff: {runtime_score!r}")
    if args.expected_strategy_profile and strategy_profile != args.expected_strategy_profile:
        raise SystemExit(
            "handoff strategy profile mismatch: "
            f"observed={strategy_profile!r} expected={args.expected_strategy_profile!r}"
        )

    original = config_path.read_text(encoding="utf-8")
    updated, previous_score, action = replace_or_insert_runtime_score(
        original,
        runtime_score=runtime_score,
        expected_strategy_profile=args.expected_strategy_profile,
    )
    changed = updated != original
    if changed and not args.dry_run:
        config_path.write_text(updated, encoding="utf-8")

    summary = {
        "kind": "autofactor_config_handoff",
        "status": "ready",
        "changed": changed,
        "dry_run": bool(args.dry_run),
        "action": action,
        "config_path": str(config_path),
        "handoff_json": str(handoff_path),
        "previous_runtime_score": previous_score,
        "runtime_score": runtime_score,
        "selected_strategy": strategy,
    }
    if args.output_json:
        Path(args.output_json).write_text(
            json.dumps(summary, indent=2, sort_keys=True) + "\n",
            encoding="utf-8",
        )
    if args.output_md:
        Path(args.output_md).write_text(render_summary_markdown(summary), encoding="utf-8")
    print(json.dumps(summary, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
