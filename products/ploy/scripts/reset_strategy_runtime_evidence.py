#!/usr/bin/env python3
"""Backup and optionally clear strategy runtime evidence rows.

This intentionally targets runtime evidence tables only:

- strategy_runtime_orders
- strategy_runtime_fills

Track-record reports are views derived from these tables. Raw market data,
quotes, orderbooks, settlements, and research snapshots are not touched.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
from datetime import datetime, timezone
from pathlib import Path


DB_URL = (
    os.environ.get("PLOY_DATABASE__URL")
    or os.environ.get("DATABASE_URL")
    or "postgresql://postgres:postgres@localhost:5432/ploy"
)

DEFAULT_RUNTIME_MODES = ("dry_run", "dryrun", "paper")
CONFIRMATION = "delete-strategy-runtime-evidence"


def sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def validate_timestamp(raw: str | None, flag: str) -> str | None:
    if raw is None or raw == "":
        return None
    try:
        datetime.fromisoformat(raw.replace("Z", "+00:00"))
    except ValueError as exc:
        raise SystemExit(f"{flag} must be ISO-8601 timestamp, got {raw!r}") from exc
    return raw


def split_runtime_modes(raw: str) -> list[str]:
    values = [item.strip() for item in raw.split(",") if item.strip()]
    if not values:
        raise SystemExit("--runtime-modes must include at least one mode")
    return values


def order_predicate(args: argparse.Namespace) -> str:
    clauses = [f"deployment_id = {sql_literal(args.deployment_id)}"]
    modes = ", ".join(sql_literal(mode) for mode in args.runtime_modes)
    clauses.append(f"runtime_mode IN ({modes})")
    if args.strategy_id:
        clauses.append(f"strategy_id = {sql_literal(args.strategy_id)}")
    if args.after_ts:
        clauses.append(f"recorded_at >= {sql_literal(args.after_ts)}::timestamptz")
    if args.before_ts:
        clauses.append(f"recorded_at < {sql_literal(args.before_ts)}::timestamptz")
    return " AND ".join(clauses)


def fill_predicate(args: argparse.Namespace) -> str:
    order_where = order_predicate(args)
    return (
        "EXISTS ("
        "SELECT 1 FROM strategy_runtime_orders o "
        "WHERE o.order_id = strategy_runtime_fills.order_id "
        f"AND {order_where}"
        ")"
    )


def run_psql(sql: str, *, timeout: int = 60) -> str:
    result = subprocess.run(
        [
            "psql",
            DB_URL,
            "-t",
            "-A",
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            sql,
        ],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if result.returncode != 0:
        sys.stderr.write(result.stderr or result.stdout)
        raise SystemExit(result.returncode)
    return result.stdout.strip()


def count_rows(table: str, predicate: str) -> int:
    raw = run_psql(f"SELECT COUNT(*) FROM {table} WHERE {predicate};")
    return int(raw or "0")


def backup_table(table: str, predicate: str, output: Path, order_by: str) -> None:
    sql = f"""
COPY (
  SELECT COALESCE(jsonb_agg(to_jsonb(rows) ORDER BY {order_by}), '[]'::jsonb)
  FROM (
    SELECT *
    FROM {table}
    WHERE {predicate}
  ) rows
) TO STDOUT;
"""
    result = subprocess.run(
        ["psql", DB_URL, "-v", "ON_ERROR_STOP=1", "-c", sql],
        capture_output=True,
        text=True,
        timeout=120,
    )
    if result.returncode != 0:
        sys.stderr.write(result.stderr or result.stdout)
        raise SystemExit(result.returncode)
    output.write_text((result.stdout.strip() or "[]") + "\n", encoding="utf-8")


def execute_delete(predicate: str) -> int:
    sql = f"""
WITH deleted AS (
  DELETE FROM strategy_runtime_orders
  WHERE {predicate}
  RETURNING 1
)
SELECT COUNT(*) FROM deleted;
"""
    return int(run_psql(sql, timeout=120) or "0")


def parse_args(argv: list[str]) -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description=(
            "Backup and optionally clear dry-run strategy_runtime_orders/fills "
            "for one deployment. Raw market data is not touched."
        )
    )
    parser.add_argument("--deployment-id", required=True)
    parser.add_argument("--strategy-id", default="")
    parser.add_argument(
        "--runtime-modes",
        default=",".join(DEFAULT_RUNTIME_MODES),
        help="Comma-separated runtime modes to match; default: dry_run,dryrun,paper",
    )
    parser.add_argument("--after-ts", default="")
    parser.add_argument("--before-ts", default="")
    parser.add_argument("--backup-dir", required=True)
    parser.add_argument("--execute", action="store_true")
    parser.add_argument(
        "--confirm",
        default="",
        help=f"Required with --execute: {CONFIRMATION}",
    )
    args = parser.parse_args(argv)
    args.runtime_modes = split_runtime_modes(args.runtime_modes)
    args.after_ts = validate_timestamp(args.after_ts, "--after-ts")
    args.before_ts = validate_timestamp(args.before_ts, "--before-ts")
    if args.execute and args.confirm != CONFIRMATION:
        raise SystemExit(f"--execute requires --confirm {CONFIRMATION!r}")
    if args.after_ts and args.before_ts:
        after = datetime.fromisoformat(args.after_ts.replace("Z", "+00:00"))
        before = datetime.fromisoformat(args.before_ts.replace("Z", "+00:00"))
        if after >= before:
            raise SystemExit("--after-ts must be earlier than --before-ts")
    return args


def main(argv: list[str] | None = None) -> int:
    args = parse_args(sys.argv[1:] if argv is None else argv)
    backup_dir = Path(args.backup_dir)
    backup_dir.mkdir(parents=True, exist_ok=True)

    orders_where = order_predicate(args)
    fills_where = fill_predicate(args)
    before = {
        "orders": count_rows("strategy_runtime_orders", orders_where),
        "fills": count_rows("strategy_runtime_fills", fills_where),
    }

    backup_table(
        "strategy_runtime_orders",
        orders_where,
        backup_dir / "strategy_runtime_orders.json",
        "rows.recorded_at, rows.order_id",
    )
    backup_table(
        "strategy_runtime_fills",
        fills_where,
        backup_dir / "strategy_runtime_fills.json",
        "rows.fill_timestamp, rows.fill_id",
    )

    deleted_orders = execute_delete(orders_where) if args.execute else 0
    after = {
        "orders": count_rows("strategy_runtime_orders", orders_where),
        "fills": count_rows("strategy_runtime_fills", fills_where),
    }
    manifest = {
        "schema_version": 1,
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "tool": "reset_strategy_runtime_evidence.py",
        "execute": args.execute,
        "deployment_id": args.deployment_id,
        "strategy_id": args.strategy_id or None,
        "runtime_modes": args.runtime_modes,
        "after_ts": args.after_ts,
        "before_ts": args.before_ts,
        "tables_touched_when_execute": ["strategy_runtime_orders", "strategy_runtime_fills"],
        "tables_not_touched": [
            "clob_quote_ticks",
            "clob_orderbook_snapshots",
            "pm_token_settlements",
            "pm_market_metadata",
            "binance_price_ticks",
            "binance_agg_trade_ticks",
            "binance_lob_ticks",
            "deribit_iv_ticks",
            "deribit_atm_greeks_ticks",
        ],
        "before": before,
        "deleted_orders": deleted_orders,
        "after": after,
        "backup_files": {
            "orders": "strategy_runtime_orders.json",
            "fills": "strategy_runtime_fills.json",
        },
    }
    (backup_dir / "manifest.json").write_text(
        json.dumps(manifest, indent=2, sort_keys=True) + "\n",
        encoding="utf-8",
    )
    print(json.dumps(manifest, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
