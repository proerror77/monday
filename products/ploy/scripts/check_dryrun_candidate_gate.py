#!/usr/bin/env python3
"""Gate dry-run report evidence before reset completion or strategy promotion."""

from __future__ import annotations

import argparse
import json
import math
import sys
from pathlib import Path
from typing import Any


DEFAULT_DEPLOYMENT_ID = "pm5d.threelayer.settlement-probability-btc-eth.dryrun"


def number(value: Any, default: float = 0.0, *, allow_infinite: bool = False) -> float:
    if value is None:
        return default
    try:
        parsed = float(value)
    except (TypeError, ValueError):
        return default
    if allow_infinite and math.isinf(parsed):
        return parsed
    if not math.isfinite(parsed):
        return default
    return parsed


def integer(value: Any) -> int:
    return int(number(value, default=0.0))


def load_payload(path: str) -> dict[str, Any]:
    if path == "-":
        return json.load(sys.stdin)
    with Path(path).open(encoding="utf-8") as handle:
        return json.load(handle)


def find_strategy(payload: dict[str, Any], deployment_id: str) -> dict[str, Any] | None:
    for strategy in payload.get("strategies") or []:
        if strategy.get("deployment_id") == deployment_id:
            return strategy
    return None


def diagnostics_summary(strategy: dict[str, Any]) -> dict[str, Any]:
    diagnostics = strategy.get("execution_diagnostics") or {}
    return diagnostics.get("summary") or {}


def clean_baseline_result(strategy: dict[str, Any] | None, deployment_id: str) -> dict[str, Any]:
    if strategy is None:
        return {
            "status": "passed",
            "mode": "clean-baseline",
            "deployment_id": deployment_id,
            "reason": "target_strategy_absent",
        }

    summary = strategy.get("summary") or {}
    diagnostics = diagnostics_summary(strategy)
    counts = {
        "total_trades": integer(summary.get("total_trades")),
        "closed_trades": integer(summary.get("closed_trades")),
        "open_positions": integer(summary.get("open_positions")),
        "total_orders": integer(diagnostics.get("total_orders")),
        "buy_orders": integer(diagnostics.get("buy_orders")),
        "sell_orders": integer(diagnostics.get("sell_orders")),
    }
    residual = {key: value for key, value in counts.items() if value != 0}
    if residual:
        return {
            "status": "blocked",
            "mode": "clean-baseline",
            "deployment_id": deployment_id,
            "reason": "residual_runtime_evidence",
            "residual_counts": residual,
            "counts": counts,
        }
    return {
        "status": "passed",
        "mode": "clean-baseline",
        "deployment_id": deployment_id,
        "reason": "zero_runtime_evidence",
        "counts": counts,
    }


def candidate_quality_result(args: argparse.Namespace, strategy: dict[str, Any] | None) -> dict[str, Any]:
    if strategy is None:
        return {
            "status": "blocked",
            "mode": "candidate-quality",
            "deployment_id": args.deployment_id,
            "reason": "target_strategy_absent",
        }

    summary = strategy.get("summary") or {}
    metrics = strategy.get("metrics") or {}
    diagnostics = diagnostics_summary(strategy)
    profit_factor = number(metrics.get("profit_factor"), allow_infinite=True)
    values = {
        "closed_trades": integer(summary.get("closed_trades")),
        "realized_pnl": number(summary.get("realized_pnl")),
        "profit_factor": "Infinity" if profit_factor == math.inf else profit_factor,
        "max_drawdown": number(metrics.get("max_drawdown")),
        "buy_fill_rate_pct": number(diagnostics.get("buy_fill_rate_pct")),
    }
    failures: dict[str, dict[str, float]] = {}
    if values["closed_trades"] < args.min_closed_trades:
        failures["closed_trades"] = {
            "actual": values["closed_trades"],
            "minimum": args.min_closed_trades,
        }
    if values["realized_pnl"] < args.min_realized_pnl:
        failures["realized_pnl"] = {
            "actual": values["realized_pnl"],
            "minimum": args.min_realized_pnl,
        }
    if profit_factor < args.min_profit_factor:
        failures["profit_factor"] = {
            "actual": values["profit_factor"],
            "minimum": args.min_profit_factor,
        }
    if values["max_drawdown"] < args.max_drawdown_floor:
        failures["max_drawdown"] = {
            "actual": values["max_drawdown"],
            "minimum": args.max_drawdown_floor,
        }
    if values["buy_fill_rate_pct"] < args.min_buy_fill_rate_pct:
        failures["buy_fill_rate_pct"] = {
            "actual": values["buy_fill_rate_pct"],
            "minimum": args.min_buy_fill_rate_pct,
        }

    return {
        "status": "blocked" if failures else "passed",
        "mode": "candidate-quality",
        "deployment_id": args.deployment_id,
        "values": values,
        "failures": failures,
    }


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--dryrun-json", default="-", help="Dry-run report JSON path or '-' for stdin")
    parser.add_argument("--deployment-id", default=DEFAULT_DEPLOYMENT_ID)
    parser.add_argument(
        "--mode",
        choices=("clean-baseline", "candidate-quality"),
        default="clean-baseline",
    )
    parser.add_argument("--min-closed-trades", type=int, default=50)
    parser.add_argument("--min-realized-pnl", type=float, default=0.0)
    parser.add_argument("--min-profit-factor", type=float, default=1.1)
    parser.add_argument("--max-drawdown-floor", type=float, default=-50.0)
    parser.add_argument("--min-buy-fill-rate-pct", type=float, default=95.0)
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    payload = load_payload(args.dryrun_json)
    strategy = find_strategy(payload, args.deployment_id)
    if args.mode == "clean-baseline":
        result = clean_baseline_result(strategy, args.deployment_id)
    else:
        result = candidate_quality_result(args, strategy)
    print(json.dumps(result, indent=2, sort_keys=True))
    return 0 if result["status"] == "passed" else 1


if __name__ == "__main__":
    raise SystemExit(main())
