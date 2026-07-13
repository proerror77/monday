#!/usr/bin/env python3
"""Export Tango strategy_runtime_orders/fills as replay parity evidence JSON."""

from __future__ import annotations

import argparse
import json
import os
import subprocess
from datetime import datetime, timezone
from pathlib import Path


def sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def sql_array(values: list[str]) -> str:
    return "ARRAY[" + ", ".join(sql_literal(value) for value in values) + "]::text[]"


def run_psql(db_url: str, sql: str, timeout: int) -> dict:
    result = subprocess.run(
        ["psql", db_url, "-t", "-A", "-v", "ON_ERROR_STOP=1", "-c", sql],
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    if result.returncode != 0:
        raise SystemExit(result.stderr or result.stdout or f"psql failed with {result.returncode}")
    payload = result.stdout.strip()
    if not payload:
        raise SystemExit("psql returned no JSON payload")
    return json.loads(payload)


def build_sql(args: argparse.Namespace) -> str:
    conditions = [f"runtime_mode = {sql_literal(args.runtime_mode)}"]
    if args.deployment_id:
        conditions.append(f"deployment_id = ANY({sql_array(args.deployment_id)})")
    if args.since:
        conditions.append(f"recorded_at >= {sql_literal(args.since)}::timestamptz")
    if args.until:
        conditions.append(f"recorded_at < {sql_literal(args.until)}::timestamptz")

    order_where = " AND ".join(f"o.{condition}" for condition in conditions)
    fill_where = " AND ".join(
        f"f.{condition}" if condition.startswith("runtime_mode") or condition.startswith("deployment_id") else f"f.{condition}"
        for condition in conditions
    )

    return f"""
SELECT jsonb_build_object(
  'schema_version', 1,
  'artifact_type', 'strategy_runtime_db_evidence',
  'producer', 'export_strategy_runtime_evidence',
  'generated_at', {sql_literal(datetime.now(timezone.utc).isoformat())},
  'runtime_mode', {sql_literal(args.runtime_mode)},
  'deployment_ids', to_jsonb({sql_array(args.deployment_id) if args.deployment_id else "ARRAY[]::text[]"}),
  'runtime_evidence', jsonb_build_object(
    'schema_version', 1,
    'basis', 'strategy_runtime_orders_fills_and_events',
    'events', COALESCE((
      SELECT jsonb_agg(jsonb_build_object(
        'deployment_id', o.deployment_id,
        'intent_id', o.intent_id,
        'order_id', o.order_id,
        'event_id', o.event_id,
        'market_id', o.event_id,
        'token_id', o.token_id,
        'market_side', o.market_side,
        'side', o.order_side,
        'decision_ts', o.recorded_at,
        'quote', COALESCE(o.limit_price, o.avg_fill_price),
        'signal_inputs', jsonb_build_object(
          'purpose', 'ENTRY',
          'requested_qty', o.quantity,
          'limit_price', o.limit_price
        ),
        'entry_price', COALESCE(fill.avg_fill_price, o.avg_fill_price, o.limit_price),
        'fill_status', o.status,
        'settlement', CASE
          WHEN COALESCE(track.is_closed, false)
            THEN COALESCE(track.avg_exit_price::text, 'closed')
          ELSE 'open'
        END,
        'pnl', COALESCE(track.net_pnl, fill.pnl, 0)
      ) ORDER BY o.recorded_at, o.order_id)
      FROM strategy_runtime_orders o
      LEFT JOIN LATERAL (
        SELECT
          CASE
            WHEN COALESCE(SUM(f.quantity), 0) > 0
              THEN SUM(f.quantity * f.price) / SUM(f.quantity)
            ELSE NULL
          END AS avg_fill_price,
          SUM(
            CASE
              WHEN f.fill_side = 'SELL' THEN f.quantity * f.price
              ELSE -(f.quantity * f.price)
            END - f.fee
          ) AS pnl
        FROM strategy_runtime_fills f
        WHERE f.runtime_mode = o.runtime_mode
          AND f.deployment_id = o.deployment_id
          AND f.order_id = o.order_id
      ) fill ON true
      LEFT JOIN strategy_runtime_event_track_record track
        ON track.runtime_mode = o.runtime_mode
        AND track.deployment_id = o.deployment_id
        AND track.intent_id = o.intent_id
      WHERE {order_where}
    ), '[]'::jsonb),
    'orders', COALESCE((
      SELECT jsonb_agg(jsonb_build_object(
        'deployment_id', o.deployment_id,
        'intent_id', o.intent_id,
        'order_id', o.order_id,
        'venue_order_id', o.venue_order_id,
        'event_id', o.event_id,
        'market_id', o.event_id,
        'token_id', o.token_id,
        'market_side', o.market_side,
        'order_side', o.order_side,
        'quantity', o.quantity,
        'requested_qty', o.quantity,
        'limit_price', o.limit_price,
        'filled_quantity', o.filled_quantity,
        'avg_fill_price', o.avg_fill_price,
        'status', o.status,
        'rejection_reason', o.rejection_reason,
        'created_at', o.recorded_at
      ) ORDER BY o.recorded_at, o.order_id)
      FROM strategy_runtime_orders o
      WHERE {order_where}
    ), '[]'::jsonb),
    'fills', COALESCE((
      SELECT jsonb_agg(jsonb_build_object(
        'deployment_id', f.deployment_id,
        'intent_id', f.intent_id,
        'order_id', f.order_id,
        'fill_id', f.fill_id,
        'event_id', f.event_id,
        'market_id', f.event_id,
        'token_id', f.token_id,
        'market_side', f.market_side,
        'fill_side', f.fill_side,
        'quantity', f.quantity,
        'price', f.price,
        'fee', f.fee,
        'fill_timestamp', f.fill_timestamp
      ) ORDER BY f.fill_timestamp, f.fill_id)
      FROM strategy_runtime_fills f
      WHERE {fill_where}
    ), '[]'::jsonb)
  )
)::text;
"""


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--db-url", default=os.environ.get("DATABASE_URL"))
    parser.add_argument("--runtime-mode", default="dry_run")
    parser.add_argument("--deployment-id", action="append", default=[])
    parser.add_argument("--since", help="inclusive recorded_at lower bound, timestamptz")
    parser.add_argument("--until", help="exclusive recorded_at upper bound, timestamptz")
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--timeout", type=int, default=30)
    args = parser.parse_args()

    if not args.db_url:
        raise SystemExit("--db-url or DATABASE_URL is required")

    payload = run_psql(args.db_url, build_sql(args), args.timeout)
    output_path = Path(args.output_json)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    output_path.write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(json.dumps(payload, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
