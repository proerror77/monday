#!/usr/bin/env python3
"""Apply a ready Event ML dry-run handoff to a strategy TOML config.

This is intentionally narrower than deployment. It updates only the runtime
score and model artifact path needed by the existing three-layer dry-run
strategy to load an Event ML probability model.
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
MODEL_PATH_RE = re.compile(r'^(\s*three_layer_event_ml_model_path\s*=\s*)".*"\s*$')
PROFILE_RE = re.compile(r'^\s*three_layer_strategy_profile\s*=\s*"([^"]+)"\s*$')


def load_json(path: Path) -> dict[str, Any]:
    return json.loads(path.read_text(encoding="utf-8"))


def select_strategy(handoff: dict[str, Any]) -> dict[str, Any]:
    if handoff.get("status") != "ready":
        raise SystemExit(f"handoff status is {handoff.get('status', 'missing')}; expected ready")
    strategy = handoff.get("strategy")
    if not isinstance(strategy, dict):
        raise SystemExit("handoff has no ready strategy")
    if handoff.get("replay_parity_ready") is not True:
        raise SystemExit("handoff replay_parity_ready is not true")
    return strategy


def replace_or_insert_field(
    lines: list[str],
    *,
    regex: re.Pattern[str],
    new_line: str,
    insert_after_index: int,
) -> tuple[str | None, str]:
    previous: str | None = None
    for index, line in enumerate(lines):
        if regex.match(line.rstrip("\n")):
            previous = line.split("=", 1)[1].strip().strip('"')
            lines[index] = new_line
            return previous, "updated"
    lines.insert(insert_after_index + 1, new_line)
    return None, "inserted"


def replace_or_insert_event_ml_config(
    config_text: str,
    *,
    runtime_score: str,
    model_path: str,
    expected_config_profile: str,
) -> tuple[str, dict[str, str | None]]:
    lines = config_text.splitlines(keepends=True)
    profile_index: int | None = None
    observed_profile: str | None = None

    for index, line in enumerate(lines):
        profile_match = PROFILE_RE.match(line.rstrip("\n"))
        if profile_match:
            profile_index = index
            observed_profile = profile_match.group(1)

    if expected_config_profile and observed_profile != expected_config_profile:
        raise SystemExit(
            "strategy config profile mismatch: "
            f"observed={observed_profile!r} expected={expected_config_profile!r}"
        )
    if profile_index is None:
        raise SystemExit("cannot insert Event ML config: three_layer_strategy_profile not found")

    previous_score, score_action = replace_or_insert_field(
        lines,
        regex=RUNTIME_SCORE_RE,
        new_line=f'three_layer_autofactor_runtime_score = "{runtime_score}"\n',
        insert_after_index=profile_index,
    )
    score_index = next(
        index
        for index, line in enumerate(lines)
        if RUNTIME_SCORE_RE.match(line.rstrip("\n"))
    )
    previous_model_path, model_path_action = replace_or_insert_field(
        lines,
        regex=MODEL_PATH_RE,
        new_line=f'three_layer_event_ml_model_path = "{model_path}"\n',
        insert_after_index=score_index,
    )

    return "".join(lines), {
        "previous_runtime_score": previous_score,
        "runtime_score_action": score_action,
        "previous_model_path": previous_model_path,
        "model_path_action": model_path_action,
    }


def render_summary_markdown(summary: dict[str, Any]) -> str:
    strategy = summary["selected_strategy"]
    return "\n".join(
        [
            "# Event ML Config Handoff",
            "",
            f"- Status: `{summary['status']}`",
            f"- Config: `{summary['config_path']}`",
            f"- Changed: `{str(summary['changed']).lower()}`",
            f"- Runtime score action: `{summary['runtime_score_action']}`",
            f"- Model path action: `{summary['model_path_action']}`",
            "- Previous runtime score: "
            f"`{summary.get('previous_runtime_score') or 'none'}`",
            f"- New runtime score: `{summary['runtime_score']}`",
            f"- Previous model path: `{summary.get('previous_model_path') or 'none'}`",
            f"- New model path: `{summary['model_path']}`",
            f"- Handoff strategy profile: `{strategy['strategy_profile']}`",
            f"- Selection rule: `{strategy['selection_rule']}`",
            f"- Test trades: `{strategy['test_trades']}`",
            f"- Test PnL: `{strategy['test_pnl']}`",
            f"- Test ROI: `{strategy['test_roi']}`",
            "",
        ]
    )


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--handoff-json", required=True)
    parser.add_argument("--config", required=True)
    parser.add_argument("--model-path", required=True)
    parser.add_argument("--expected-handoff-profile", default="event_ml_supervised_tabular")
    parser.add_argument("--expected-config-profile", default="settlement_probability")
    parser.add_argument("--dry-run", action="store_true")
    parser.add_argument("--output-json", default="")
    parser.add_argument("--output-md", default="")
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    handoff_path = Path(args.handoff_json)
    config_path = Path(args.config)
    handoff = load_json(handoff_path)
    strategy = select_strategy(handoff)

    runtime_score = str(strategy.get("runtime_score") or "")
    strategy_profile = str(strategy.get("strategy_profile") or "")
    model_path = args.model_path.strip()
    if not runtime_score.startswith("event_ml_model:"):
        raise SystemExit(f"unsupported runtime_score for Event ML config handoff: {runtime_score!r}")
    if args.expected_handoff_profile and strategy_profile != args.expected_handoff_profile:
        raise SystemExit(
            "handoff strategy profile mismatch: "
            f"observed={strategy_profile!r} expected={args.expected_handoff_profile!r}"
        )
    if not model_path:
        raise SystemExit("model_path is required for Event ML config handoff")
    if "\n" in model_path or "\r" in model_path:
        raise SystemExit("model_path must be single-line")

    original = config_path.read_text(encoding="utf-8")
    updated, actions = replace_or_insert_event_ml_config(
        original,
        runtime_score=runtime_score,
        model_path=model_path,
        expected_config_profile=args.expected_config_profile,
    )
    changed = updated != original
    if changed and not args.dry_run:
        config_path.write_text(updated, encoding="utf-8")

    summary = {
        "kind": "event_ml_config_handoff",
        "status": "ready",
        "changed": changed,
        "dry_run": bool(args.dry_run),
        "config_path": str(config_path),
        "handoff_json": str(handoff_path),
        "runtime_score": runtime_score,
        "model_path": model_path,
        "selected_strategy": strategy,
        **actions,
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
