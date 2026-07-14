#!/usr/bin/env python3
"""Emit dry-run strategy performance summary JSON from the local Ploy database."""

import json
import math
import os
import subprocess
import sys
from collections import defaultdict
from datetime import datetime, timedelta, timezone
from pathlib import Path
from statistics import mean, stdev


DB_URL = (
    os.environ.get("PLOY_DATABASE__URL")
    or os.environ.get("DATABASE_URL")
    or "postgresql://postgres:postgres@localhost:5432/ploy"
)

SIMULATED_RUNTIME_MODES = ("dry_run", "dryrun", "paper")
MODE_FILTER = ",".join(f"'{mode}'" for mode in SIMULATED_RUNTIME_MODES)
DEPLOYMENTS_FILE = Path(os.environ.get("PLOY_DEPLOYMENTS_FILE") or "data/state/deployments.json")
DEPLOYMENT_STATUS_FILE = Path(
    os.environ.get("PLOY_DEPLOYMENT_STATUS_FILE") or "run/platform/deployments.json"
)
DEPLOYMENT_CONFIG_DIR = Path(os.environ.get("PLOY_DEPLOYMENT_CONFIG_DIR") or "config/deployments")

EXPERIMENT_LABELS = {
    "pm5d.threelayer.dryrun": "TL v1 Base EVCal",
    "pm5d.threelayer.champion.dryrun": "TL v2 Champion EVCal",
    "pm5d.threelayer.obi-soft.dryrun": "TL v3 OBI-soft EVCal",
    "pm5d.threelayer.obi-hard.dryrun": "TL v4 OBI-hard EVCal",
    "pm5d.threelayer.continuation-soft.dryrun": "TL v5 Continuation-soft EVCal",
    "pm5d.threelayer.settlement-probability-btc-eth.dryrun": "TL Settlement Probability BTC/ETH",
}


EVENTS_QUERY = f"""
WITH events AS (
  SELECT
    t.runtime_mode,
    t.strategy_id,
    t.deployment_id,
    t.trade_key,
    t.event_id,
    t.intent_id,
    t.symbol,
    t.token_id,
    t.market_side,
    t.opened_at,
    t.closed_at,
    t.last_fill_at,
    t.fill_count,
    t.buy_quantity,
    t.buy_notional,
    t.total_fee,
    t.avg_entry_price,
    t.avg_exit_price,
    t.gross_pnl,
    t.net_pnl,
    t.is_closed,
    t.open_quantity,
    CASE
      WHEN ms.market_slug IS NOT NULL THEN 'token_settlement_market_metadata'
      WHEN s.market_slug IS NOT NULL THEN 'token_settlement_without_metadata'
      WHEN me.market_slug IS NOT NULL THEN 'event_track_market_metadata'
      WHEN mt.market_slug IS NOT NULL THEN 'trade_key_market_metadata'
      ELSE 'missing_market_metadata'
    END AS metadata_join_status,
    COALESCE(ms.market_slug, me.market_slug, mt.market_slug, s.market_slug) AS metadata_market_slug,
    CASE
      WHEN COALESCE(ms.end_time, me.end_time, mt.end_time) IS NOT NULL
        AND COALESCE(ms.start_time, me.start_time, mt.start_time) IS NOT NULL
        THEN ROUND(EXTRACT(EPOCH FROM (
          COALESCE(ms.end_time, me.end_time, mt.end_time)
          - COALESCE(ms.start_time, me.start_time, mt.start_time)
        )))::int
      WHEN COALESCE(ms.market_slug, me.market_slug, mt.market_slug, s.market_slug) ILIKE '%15m%'
        OR COALESCE(ms.market_slug, me.market_slug, mt.market_slug, s.market_slug) ILIKE '%15-minute%'
        THEN 900
      WHEN COALESCE(ms.market_slug, me.market_slug, mt.market_slug, s.market_slug) ILIKE '%5m%'
        OR COALESCE(ms.market_slug, me.market_slug, mt.market_slug, s.market_slug) ILIKE '%5-minute%'
        THEN 300
      ELSE NULL
    END AS window_secs,
    CASE
      WHEN COALESCE(ms.end_time, me.end_time, mt.end_time) IS NOT NULL
        AND t.opened_at IS NOT NULL
        THEN ROUND(EXTRACT(EPOCH FROM (COALESCE(ms.end_time, me.end_time, mt.end_time) - t.opened_at)))::int
      ELSE NULL
    END AS entry_time_remaining_secs
  FROM strategy_runtime_event_track_record t
  LEFT JOIN pm_token_settlements s ON s.token_id = t.token_id
  LEFT JOIN pm_market_metadata ms ON ms.market_slug = s.market_slug
  LEFT JOIN pm_market_metadata me ON me.market_slug = t.event_id
  LEFT JOIN pm_market_metadata mt ON mt.market_slug = t.trade_key
  WHERE t.runtime_mode IN ({MODE_FILTER})
)
SELECT COALESCE(json_agg(row_to_json(e) ORDER BY e.opened_at, e.trade_key), '[]'::json)::text
FROM events e;
"""

DAILY_QUERY = f"""
SELECT COALESCE(json_agg(row_to_json(d) ORDER BY d.trading_day_cst, d.runtime_mode, d.strategy_id, d.deployment_id), '[]'::json)::text
FROM (
  SELECT
    runtime_mode,
    strategy_id,
    deployment_id,
    trading_day_cst,
    trade_count,
    closed_trade_count,
    winning_trade_count_all AS wins,
    losing_trade_count_all AS losses,
    confirmed_trade_count,
    net_pnl,
    confirmed_net_pnl,
    total_fee AS fees,
    residual_open_quantity AS open_quantity
  FROM strategy_runtime_daily_track_record
  WHERE runtime_mode IN ({MODE_FILTER})
) d;
"""

PAIRING_QUERY = f"""
SELECT json_build_object(
  'pair_key', 'runtime_mode,strategy_id,deployment_id,event_id',
  'mixed_event_groups', COUNT(*),
  'fills_in_mixed_event_groups', COALESCE(SUM(fill_count), 0),
  'current_view_rows', (
    SELECT COUNT(*)
    FROM strategy_runtime_event_track_record
    WHERE runtime_mode IN ({MODE_FILTER})
  ),
  'side_aware_rows', (
    SELECT COUNT(*)
    FROM (
      SELECT
        runtime_mode,
        strategy_id,
        deployment_id,
        COALESCE(NULLIF(event_id, ''), intent_id) AS event_or_intent,
        COALESCE(NULLIF(token_id, ''), 'unknown') AS token_key,
        COALESCE(NULLIF(market_side, ''), 'unknown') AS side_key
      FROM strategy_runtime_fills
      WHERE runtime_mode IN ({MODE_FILTER})
      GROUP BY runtime_mode, strategy_id, deployment_id, event_or_intent, token_key, side_key
    ) side_groups
  )
)::text AS payload
FROM (
  SELECT
    runtime_mode,
    strategy_id,
    deployment_id,
    event_id,
    COUNT(*) AS fill_count
  FROM strategy_runtime_fills
  WHERE runtime_mode IN ({MODE_FILTER})
    AND event_id IS NOT NULL
    AND event_id <> ''
  GROUP BY runtime_mode, strategy_id, deployment_id, event_id
  HAVING COUNT(DISTINCT token_id) > 1 OR COUNT(DISTINCT market_side) > 1
) mixed;
"""

ORDER_DIAGNOSTICS_QUERY = f"""
SELECT COALESCE(json_agg(row_to_json(d) ORDER BY d.runtime_mode, d.strategy_id, d.deployment_id), '[]'::json)::text
FROM (
  SELECT
    runtime_mode,
    strategy_id,
    deployment_id,
    COUNT(*) AS total_orders,
    COUNT(*) FILTER (WHERE order_side = 'BUY') AS buy_orders,
    COUNT(*) FILTER (WHERE order_side = 'SELL') AS sell_orders,
    COUNT(*) FILTER (
      WHERE LOWER(status) = 'rejected' OR rejection_reason IS NOT NULL
    ) AS rejected_orders,
    COUNT(*) FILTER (
      WHERE order_side = 'BUY'
        AND (LOWER(status) = 'rejected' OR rejection_reason IS NOT NULL)
    ) AS rejected_buy_orders,
    COUNT(*) FILTER (
      WHERE order_side = 'BUY'
        AND filled_quantity > 0
        AND quantity > 0
        AND filled_quantity < quantity * 0.98
    ) AS partial_buy_orders,
    ROUND(COALESCE(SUM(quantity * COALESCE(limit_price, avg_fill_price, 0)) FILTER (WHERE order_side = 'BUY'), 0), 4) AS buy_requested_notional,
    ROUND(COALESCE(SUM(filled_quantity * COALESCE(avg_fill_price, limit_price, 0)) FILTER (WHERE order_side = 'BUY'), 0), 4) AS buy_filled_notional,
    ROUND(
      CASE
        WHEN COALESCE(SUM(quantity) FILTER (WHERE order_side = 'BUY'), 0) > 0
          THEN COALESCE(SUM(filled_quantity) FILTER (WHERE order_side = 'BUY'), 0)
               / SUM(quantity) FILTER (WHERE order_side = 'BUY') * 100
        ELSE 0
      END,
      2
    ) AS buy_fill_rate_pct
  FROM strategy_runtime_orders
  WHERE runtime_mode IN ({MODE_FILTER})
  GROUP BY runtime_mode, strategy_id, deployment_id
) d;
"""

RUNTIME_EVIDENCE_QUERY = f"""
SELECT jsonb_build_object(
  'schema_version', 1,
  'basis', 'strategy_runtime_orders_fills_and_events',
  'events', COALESCE((
    SELECT jsonb_agg(jsonb_build_object(
      'runtime_mode', o.runtime_mode,
      'strategy_id', o.strategy_id,
      'deployment_id', o.deployment_id,
      'event_id', o.event_id,
      'market_id', o.event_id,
      'intent_id', o.intent_id,
      'order_id', o.order_id,
      'token_id', o.token_id,
      'market_side', o.market_side,
      'side', o.order_side,
      'decision_ts', o.recorded_at,
      'quote', COALESCE(o.limit_price, o.avg_fill_price),
      'signal_inputs', jsonb_build_object(
        'purpose', COALESCE(o.context ->> 'purpose', 'ENTRY'),
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
        AND f.strategy_id = o.strategy_id
        AND f.deployment_id = o.deployment_id
        AND f.order_id = o.order_id
    ) fill ON true
    LEFT JOIN strategy_runtime_event_track_record track
      ON track.runtime_mode = o.runtime_mode
      AND track.strategy_id = o.strategy_id
      AND track.deployment_id = o.deployment_id
      AND track.intent_id = o.intent_id
    WHERE o.runtime_mode IN ({MODE_FILTER})
  ), '[]'::jsonb),
  'orders', COALESCE((
    SELECT jsonb_agg(jsonb_build_object(
      'runtime_mode', o.runtime_mode,
      'strategy_id', o.strategy_id,
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
      'context', o.context,
      'created_at', o.recorded_at
    ) ORDER BY o.recorded_at, o.order_id)
    FROM strategy_runtime_orders o
    WHERE o.runtime_mode IN ({MODE_FILTER})
  ), '[]'::jsonb),
  'fills', COALESCE((
    SELECT jsonb_agg(jsonb_build_object(
      'runtime_mode', f.runtime_mode,
      'strategy_id', f.strategy_id,
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
    WHERE f.runtime_mode IN ({MODE_FILTER})
  ), '[]'::jsonb)
)::text;
"""


def run_json_query(query: str, timeout: int = 30):
    result = subprocess.run(
        [
            "psql",
            DB_URL,
            "-t",
            "-A",
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            query,
        ],
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if result.returncode != 0:
        sys.stderr.write(result.stderr or result.stdout)
        raise SystemExit(result.returncode)
    payload = result.stdout.strip()
    if not payload:
        return None
    return json.loads(payload)


def load_json_file(path: Path):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (FileNotFoundError, json.JSONDecodeError, OSError):
        return None


def normalize_state(value) -> str:
    if isinstance(value, str):
        return value.lower()
    return ""


def get_deployment_id(record: dict) -> str:
    return str(record.get("deployment_id") or record.get("id") or "")


def is_simulated_deployment(record: dict) -> bool:
    runtime_mode = normalize_state(record.get("runtime_mode"))
    dep_id = get_deployment_id(record).lower()
    return runtime_mode in SIMULATED_RUNTIME_MODES or dep_id.endswith(".dryrun") or dep_id.endswith(".paper")


def deployment_is_running(record: dict) -> bool:
    desired = normalize_state(record.get("desired_state"))
    observed = normalize_state(record.get("observed_state"))
    return desired == "running" or observed == "running"


def merge_deployment_record(base: dict, update: dict) -> dict:
    merged = dict(base)
    for key, value in update.items():
        if value is not None and value != "":
            merged[key] = value
    return merged


def records_from_json_payload(payload) -> list[dict]:
    if isinstance(payload, list):
        return [item for item in payload if isinstance(item, dict)]
    if isinstance(payload, dict):
        for key in ("deployments", "items", "records"):
            value = payload.get(key)
            if isinstance(value, list):
                return [item for item in value if isinstance(item, dict)]
        if get_deployment_id(payload):
            return [payload]
    return []


def load_deployments() -> list[dict]:
    by_id: dict[str, dict] = {}

    for path in sorted(DEPLOYMENT_CONFIG_DIR.glob("*.json")) if DEPLOYMENT_CONFIG_DIR.exists() else []:
        for record in records_from_json_payload(load_json_file(path)):
            dep_id = get_deployment_id(record)
            if dep_id:
                by_id[dep_id] = merge_deployment_record(by_id.get(dep_id, {}), record)

    for path in (DEPLOYMENTS_FILE, DEPLOYMENT_STATUS_FILE):
        for record in records_from_json_payload(load_json_file(path)):
            dep_id = get_deployment_id(record)
            if dep_id:
                by_id[dep_id] = merge_deployment_record(by_id.get(dep_id, {}), record)

    return sorted(
        (record for record in by_id.values() if is_simulated_deployment(record)),
        key=lambda record: get_deployment_id(record),
    )


def number(value, default=0.0) -> float:
    if value is None:
        return default
    try:
        parsed = float(value)
    except (TypeError, ValueError):
        return default
    if not math.isfinite(parsed):
        return default
    return parsed


def rounded(value, digits=2):
    return round(number(value), digits)


def strategy_key(row) -> tuple[str, str, str]:
    return (
        row.get("runtime_mode") or "",
        row.get("strategy_id") or "",
        row.get("deployment_id") or "",
    )


def humanize_deployment_id(deployment_id: str) -> str:
    if not deployment_id:
        return ""
    if deployment_id.startswith("pm5d.threelayer.") and deployment_id.endswith(".dryrun"):
        variant = deployment_id.removeprefix("pm5d.threelayer.").removesuffix(".dryrun")
        if variant == "":
            variant = "base"
        return f"TL {variant.replace('-', ' ').title()}"
    if deployment_id.startswith("pm5d.") and deployment_id.endswith(".dryrun"):
        variant = deployment_id.removeprefix("pm5d.").removesuffix(".dryrun")
        return f"PM5D {variant.replace('-', ' ').replace('.', ' ').title()}"
    return deployment_id


def experiment_label(runtime_mode: str, strategy_id: str, deployment_id: str) -> str:
    if deployment_id:
        return EXPERIMENT_LABELS.get(deployment_id, humanize_deployment_id(deployment_id))
    if strategy_id:
        return strategy_id
    return runtime_mode or "unknown"


def strategy_label(runtime_mode: str, strategy_id: str, deployment_id: str) -> str:
    return experiment_label(runtime_mode, strategy_id, deployment_id)


def window_label(window_secs) -> str:
    if window_secs == 300:
        return "5m"
    if window_secs == 900:
        return "15m"
    if window_secs is None:
        return "unknown"
    return f"{window_secs}s"


def sort_timestamp(value):
    return value or ""


def day_from_event(row) -> str | None:
    timestamp = row.get("closed_at") or row.get("last_fill_at") or row.get("opened_at")
    if not timestamp:
        return None
    parsed = parse_timestamp(timestamp)
    if parsed is None:
        return timestamp[:10]
    if parsed.tzinfo is None:
        return parsed.date().isoformat()
    return parsed.astimezone(timezone(timedelta(hours=8))).date().isoformat()


def hour_from_event(row) -> str | None:
    timestamp = row.get("closed_at") or row.get("last_fill_at") or row.get("opened_at")
    if not timestamp:
        return None
    parsed = parse_timestamp(timestamp)
    if parsed is None:
        return str(timestamp)[:13] + ":00"
    if parsed.tzinfo is None:
        hour = parsed.replace(minute=0, second=0, microsecond=0)
    else:
        hour = parsed.astimezone(timezone(timedelta(hours=8))).replace(minute=0, second=0, microsecond=0)
    return hour.isoformat()


def parse_timestamp(value):
    if not value:
        return None
    try:
        return datetime.fromisoformat(str(value).replace("Z", "+00:00"))
    except ValueError:
        return None


def win_rate(wins: int, closed_count: int) -> float:
    if closed_count <= 0:
        return 0.0
    return round(wins / closed_count * 100, 1)


def build_summary(events):
    closed = [event for event in events if event.get("is_closed")]
    open_events = [event for event in events if not event.get("is_closed")]
    wins = sum(1 for event in closed if number(event.get("net_pnl")) > 0)
    losses = sum(1 for event in closed if number(event.get("net_pnl")) <= 0)
    latest_opened_at = max((event.get("opened_at") for event in events if event.get("opened_at")), default=None)
    latest_closed_at = max((event.get("closed_at") for event in closed if event.get("closed_at")), default=None)
    return {
        "total_trades": len(events),
        "closed_trades": len(closed),
        "wins": wins,
        "losses": losses,
        "win_rate_pct": win_rate(wins, len(closed)),
        "realized_pnl": rounded(sum(number(event.get("net_pnl")) for event in closed)),
        "total_fees": rounded(sum(number(event.get("total_fee")) for event in closed)),
        "open_positions": len(open_events),
        "open_exposure": rounded(sum(number(event.get("buy_notional")) for event in open_events)),
        "latest_opened_at": latest_opened_at,
        "latest_closed_at": latest_closed_at,
    }


def build_equity_curve(events):
    cumulative = 0.0
    peak = 0.0
    points = []
    closed = sorted(
        (event for event in events if event.get("is_closed")),
        key=lambda event: (sort_timestamp(event.get("closed_at")), event.get("trade_key") or ""),
    )
    for index, event in enumerate(closed, start=1):
        pnl = number(event.get("net_pnl"))
        cumulative += pnl
        peak = max(peak, cumulative)
        points.append(
            {
                "index": index,
                "label": str(index),
                "timestamp": event.get("closed_at"),
                "symbol": event.get("symbol"),
                "pnl": round(pnl, 4),
                "cumulative": round(cumulative, 4),
                "drawdown": round(cumulative - peak, 4),
            }
        )
    return points


def build_metrics(events, equity_curve):
    closed_pnls = [number(event.get("net_pnl")) for event in events if event.get("is_closed")]
    gross_profit = sum(pnl for pnl in closed_pnls if pnl > 0)
    gross_loss = abs(sum(pnl for pnl in closed_pnls if pnl < 0))
    profit_factor = None
    if gross_loss > 0:
        profit_factor = round(gross_profit / gross_loss, 4)
    elif gross_profit > 0:
        profit_factor = "Infinity"

    sharpe_per_trade = None
    if len(closed_pnls) >= 2:
        sigma = stdev(closed_pnls)
        if sigma > 0:
            sharpe_per_trade = round((mean(closed_pnls) / sigma) * math.sqrt(len(closed_pnls)), 4)

    max_drawdown = min((point["drawdown"] for point in equity_curve), default=0.0)
    avg_trade = mean(closed_pnls) if closed_pnls else None
    return {
        "sharpe": sharpe_per_trade,
        "sharpe_per_trade": sharpe_per_trade,
        "sharpe_basis": "closed_trade_pnl_sqrt_n",
        "closed_trade_count_for_sharpe": len(closed_pnls),
        "profit_factor": profit_factor,
        "max_drawdown": round(max_drawdown, 4),
        "avg_trade": None if avg_trade is None else round(avg_trade, 4),
        "gross_profit": round(gross_profit, 4),
        "gross_loss": round(gross_loss, 4),
        "equity_points": len(equity_curve),
    }


def build_daily_sharpe(daily_rows):
    daily_pnls = [number(row.get("net_pnl")) for row in daily_rows]
    if len(daily_pnls) < 2:
        return None
    sigma = stdev(daily_pnls)
    if sigma <= 0:
        return None
    return round((mean(daily_pnls) / sigma) * math.sqrt(365), 4)


def build_window_rows(events):
    grouped = defaultdict(list)
    for event in events:
        grouped[event.get("window_secs")].append(event)
    rows = []
    for window_secs, window_events in grouped.items():
        closed = [event for event in window_events if event.get("is_closed")]
        wins = sum(1 for event in closed if number(event.get("net_pnl")) > 0)
        losses = sum(1 for event in closed if number(event.get("net_pnl")) <= 0)
        entry_values = [number(event.get("avg_entry_price")) for event in closed if event.get("avg_entry_price") is not None]
        ttr_values = [
            int(event.get("entry_time_remaining_secs"))
            for event in window_events
            if event.get("entry_time_remaining_secs") is not None
        ]
        rows.append(
            {
                "window_secs": window_secs,
                "window_label": window_label(window_secs),
                "total_trades": len(window_events),
                "closed_trades": len(closed),
                "wins": wins,
                "losses": losses,
                "win_rate_pct": win_rate(wins, len(closed)),
                "realized_pnl": rounded(sum(number(event.get("net_pnl")) for event in closed)),
                "avg_pnl": None if not closed else rounded(mean(number(event.get("net_pnl")) for event in closed)),
                "avg_entry": None if not entry_values else round(mean(entry_values), 4),
                "min_entry_ttr_secs": min(ttr_values) if ttr_values else None,
                "max_entry_ttr_secs": max(ttr_values) if ttr_values else None,
            }
        )
    return sorted(rows, key=lambda row: (row["window_secs"] is None, row["window_secs"] or 0))


def build_daily_rows(daily_rows):
    grouped = defaultdict(list)
    for row in daily_rows:
        grouped[row.get("trading_day_cst")].append(row)
    rows = []
    for trading_day, day_rows in grouped.items():
        rows.append(
            {
                "trading_day_cst": trading_day,
                "trade_count": int(sum(number(row.get("trade_count")) for row in day_rows)),
                "closed_trade_count": int(sum(number(row.get("closed_trade_count")) for row in day_rows)),
                "wins": int(sum(number(row.get("wins")) for row in day_rows)),
                "losses": int(sum(number(row.get("losses")) for row in day_rows)),
                "confirmed_trade_count": int(sum(number(row.get("confirmed_trade_count")) for row in day_rows)),
                "net_pnl": rounded(sum(number(row.get("net_pnl")) for row in day_rows)),
                "confirmed_pnl": rounded(sum(number(row.get("confirmed_net_pnl")) for row in day_rows)),
                "fees": rounded(sum(number(row.get("fees")) for row in day_rows)),
                "open_quantity": rounded(sum(number(row.get("open_quantity")) for row in day_rows), 4),
            }
        )
    return sorted(rows, key=lambda row: row["trading_day_cst"], reverse=True)


def build_daily_by_window(events):
    grouped = defaultdict(list)
    for event in events:
        day = day_from_event(event)
        if day is not None:
            grouped[(day, event.get("window_secs"))].append(event)
    rows = []
    for (trading_day, window_secs), window_events in grouped.items():
        closed = [event for event in window_events if event.get("is_closed")]
        rows.append(
            {
                "trading_day_cst": trading_day,
                "window_secs": window_secs,
                "window_label": window_label(window_secs),
                "trade_count": len(window_events),
                "closed_trade_count": len(closed),
                "wins": sum(1 for event in closed if number(event.get("net_pnl")) > 0),
                "losses": sum(1 for event in closed if number(event.get("net_pnl")) <= 0),
                "net_pnl": rounded(sum(number(event.get("net_pnl")) for event in closed)),
            }
        )
    return sorted(rows, key=lambda row: (row["trading_day_cst"], row["window_secs"] or 0), reverse=True)


def build_hourly_rows(events):
    grouped = defaultdict(list)
    for event in events:
        hour = hour_from_event(event)
        if hour is not None:
            grouped[hour].append(event)

    cumulative = 0.0
    peak = 0.0
    rows = []
    for hour in sorted(grouped.keys()):
        hour_events = grouped[hour]
        closed = [event for event in hour_events if event.get("is_closed")]
        net_pnl = sum(number(event.get("net_pnl")) for event in closed)
        cumulative += net_pnl
        peak = max(peak, cumulative)
        rows.append(
            {
                "trading_hour_cst": hour,
                "trade_count": len(hour_events),
                "closed_trade_count": len(closed),
                "wins": sum(1 for event in closed if number(event.get("net_pnl")) > 0),
                "losses": sum(1 for event in closed if number(event.get("net_pnl")) <= 0),
                "net_pnl": rounded(net_pnl),
                "cumulative_pnl": round(cumulative, 4),
                "drawdown": round(cumulative - peak, 4),
            }
        )
    return list(reversed(rows))


def build_hourly_by_window(events):
    grouped = defaultdict(list)
    for event in events:
        hour = hour_from_event(event)
        if hour is not None:
            grouped[(hour, event.get("window_secs"))].append(event)
    rows = []
    for (hour, window_secs), window_events in grouped.items():
        closed = [event for event in window_events if event.get("is_closed")]
        rows.append(
            {
                "trading_hour_cst": hour,
                "window_secs": window_secs,
                "window_label": window_label(window_secs),
                "trade_count": len(window_events),
                "closed_trade_count": len(closed),
                "wins": sum(1 for event in closed if number(event.get("net_pnl")) > 0),
                "losses": sum(1 for event in closed if number(event.get("net_pnl")) <= 0),
                "net_pnl": rounded(sum(number(event.get("net_pnl")) for event in closed)),
            }
        )
    return sorted(rows, key=lambda row: (row["trading_hour_cst"], row["window_secs"] or 0), reverse=True)


def build_symbol_rows(events, include_window=False):
    grouped = defaultdict(list)
    for event in events:
        key = (event.get("symbol") or "unknown", event.get("window_secs") if include_window else None)
        grouped[key].append(event)
    rows = []
    for (symbol, window_secs), symbol_events in grouped.items():
        closed = [event for event in symbol_events if event.get("is_closed")]
        entry_values = [number(event.get("avg_entry_price")) for event in closed if event.get("avg_entry_price") is not None]
        row = {
            "symbol": symbol,
            "trades": len(symbol_events),
            "wins": sum(1 for event in closed if number(event.get("net_pnl")) > 0),
            "losses": sum(1 for event in closed if number(event.get("net_pnl")) <= 0),
            "net_pnl": rounded(sum(number(event.get("net_pnl")) for event in closed)),
            "avg_entry": None if not entry_values else round(mean(entry_values), 4),
        }
        if include_window:
            row["window_secs"] = window_secs
            row["window_label"] = window_label(window_secs)
        rows.append(row)
    return sorted(rows, key=lambda row: number(row["net_pnl"]), reverse=True)


def exit_type(event):
    exit_price = number(event.get("avg_exit_price"), default=math.nan)
    if math.isfinite(exit_price):
        if exit_price >= 0.99:
            return "WIN"
        if exit_price <= 0.01:
            return "LOSS"
    return "TP/SL"


def closed_trade_row(event):
    window_secs = event.get("window_secs")
    return {
        "runtime_mode": event.get("runtime_mode"),
        "strategy_id": event.get("strategy_id"),
        "deployment_id": event.get("deployment_id"),
        "experiment_label": experiment_label(
            event.get("runtime_mode") or "",
            event.get("strategy_id") or "",
            event.get("deployment_id") or "",
        ),
        "trade_key": event.get("trade_key"),
        "event_id": event.get("event_id"),
        "symbol": event.get("symbol"),
        "window_secs": window_secs,
        "window_label": window_label(window_secs),
        "market_side": event.get("market_side"),
        "entry_price": None if event.get("avg_entry_price") is None else round(number(event.get("avg_entry_price")), 4),
        "exit_price": None if event.get("avg_exit_price") is None else round(number(event.get("avg_exit_price")), 4),
        "exit_type": exit_type(event),
        "quantity": round(number(event.get("buy_quantity")), 4),
        "notional": round(number(event.get("buy_notional")), 4),
        "net_pnl": round(number(event.get("net_pnl")), 4),
        "entry_time_remaining_secs": event.get("entry_time_remaining_secs"),
        "opened_at": event.get("opened_at"),
        "closed_at": event.get("closed_at"),
    }


def build_closed_trades(events):
    return [
        closed_trade_row(event)
        for event in sorted(
            (event for event in events if event.get("is_closed")),
            key=lambda event: (sort_timestamp(event.get("closed_at")), event.get("trade_key") or ""),
            reverse=True,
        )
    ]


def build_open_positions(events):
    rows = []
    for event in sorted(
        (event for event in events if not event.get("is_closed")),
        key=lambda event: (sort_timestamp(event.get("opened_at")), event.get("trade_key") or ""),
        reverse=True,
    ):
        window_secs = event.get("window_secs")
        rows.append(
            {
                "runtime_mode": event.get("runtime_mode"),
                "strategy_id": event.get("strategy_id"),
                "deployment_id": event.get("deployment_id"),
                "experiment_label": experiment_label(
                    event.get("runtime_mode") or "",
                    event.get("strategy_id") or "",
                    event.get("deployment_id") or "",
                ),
                "trade_key": event.get("trade_key"),
                "event_id": event.get("event_id"),
                "symbol": event.get("symbol"),
                "window_secs": window_secs,
                "window_label": window_label(window_secs),
                "market_side": event.get("market_side"),
                "entry_price": None if event.get("avg_entry_price") is None else round(number(event.get("avg_entry_price")), 4),
                "quantity": round(number(event.get("buy_quantity")), 4),
                "notional": round(number(event.get("buy_notional")), 4),
                "entry_time_remaining_secs": event.get("entry_time_remaining_secs"),
                "opened_at": event.get("opened_at"),
            }
        )
    return rows


def build_report_slice(events, daily_rows):
    equity_curve = build_equity_curve(events)
    closed_trades = build_closed_trades(events)
    metrics = build_metrics(events, equity_curve)
    metrics["sharpe_daily_ann"] = build_daily_sharpe(daily_rows)
    metrics["daily_sharpe_basis"] = "daily_net_pnl_sqrt_365"
    return {
        "summary": build_summary(events),
        "metrics": metrics,
        "equity_curve": equity_curve,
        "by_window": build_window_rows(events),
        "daily": build_daily_rows(daily_rows),
        "daily_by_window": build_daily_by_window(events),
        "hourly": build_hourly_rows(events),
        "hourly_by_window": build_hourly_by_window(events),
        "symbols": build_symbol_rows(events),
        "symbols_by_window": build_symbol_rows(events, include_window=True),
        "closed_trades": closed_trades[:250],
        "recent_closed": closed_trades[:50],
        "open_positions": build_open_positions(events),
    }


def build_execution_diagnostics(rows):
    totals = defaultdict(float)
    for row in rows:
        for key in (
            "total_orders",
            "buy_orders",
            "sell_orders",
            "rejected_orders",
            "rejected_buy_orders",
            "partial_buy_orders",
            "buy_requested_notional",
            "buy_filled_notional",
        ):
            totals[key] += number(row.get(key))

    buy_orders = totals["buy_orders"]
    rejected_buy_orders = totals["rejected_buy_orders"]
    totals["buy_fill_rate_pct"] = (
        round(totals["buy_filled_notional"] / totals["buy_requested_notional"] * 100, 2)
        if totals["buy_requested_notional"] > 0
        else 0
    )
    totals["rejected_buy_rate_pct"] = round(rejected_buy_orders / buy_orders * 100, 2) if buy_orders > 0 else 0

    return {
        "basis": "strategy_runtime_orders",
        "partial_buy_threshold_pct": 98,
        "summary": {
            key: (round(value, 4) if key.endswith("_notional") else int(value) if key.endswith("_orders") else value)
            for key, value in totals.items()
        },
        "strategies": rows,
    }


def deployment_report_row(record: dict, strategy_diagnostics: dict | None = None):
    dep_id = get_deployment_id(record)
    runtime_mode = record.get("runtime_mode") or ("dryrun" if dep_id.endswith(".dryrun") else "paper")
    strategy_id = record.get("strategy_id") or record.get("bundle_id") or record.get("strategy") or ""
    strategy_payload = build_report_slice([], [])
    strategy_payload.update(
        {
            "runtime_mode": runtime_mode,
            "strategy_id": strategy_id,
            "deployment_id": dep_id,
            "label": strategy_label(runtime_mode, strategy_id, dep_id),
            "experiment_label": experiment_label(runtime_mode, strategy_id, dep_id),
            "activity_status": "running_no_recent_trades",
            "deployment_desired_state": normalize_state(record.get("desired_state")),
            "deployment_observed_state": normalize_state(record.get("observed_state")),
            "bundle_id": record.get("bundle_id"),
            "account_id": record.get("account_id"),
            "execution_diagnostics": build_execution_diagnostics(
                [strategy_diagnostics] if strategy_diagnostics else []
            ),
        }
    )
    return strategy_payload


def empty_payload():
    now = datetime.now(timezone.utc).isoformat()
    return {
        "generated_at": now,
        "summary": build_summary([]),
        "metrics": build_metrics([], []),
        "equity_curve": [],
        "by_window": [],
        "daily": [],
        "daily_by_window": [],
        "hourly": [],
        "hourly_by_window": [],
        "symbols": [],
        "symbols_by_window": [],
        "closed_trades": [],
        "recent_closed": [],
        "open_positions": [],
        "strategies": [],
        "deployments": [],
        "pairing": {
            "pair_key": "runtime_mode,strategy_id,deployment_id,event_id",
            "mixed_event_groups": 0,
            "fills_in_mixed_event_groups": 0,
            "current_view_rows": 0,
            "side_aware_rows": 0,
        },
        "execution_diagnostics": build_execution_diagnostics([]),
        "runtime_evidence": {
            "schema_version": 1,
            "basis": "strategy_runtime_orders_fills_and_events",
            "events": [],
            "orders": [],
            "fills": [],
        },
    }


def main() -> int:
    events = run_json_query(EVENTS_QUERY) or []
    daily_rows = run_json_query(DAILY_QUERY) or []
    pairing = run_json_query(PAIRING_QUERY) or empty_payload()["pairing"]
    order_diagnostics = run_json_query(ORDER_DIAGNOSTICS_QUERY) or []
    runtime_evidence = run_json_query(RUNTIME_EVIDENCE_QUERY) or empty_payload()["runtime_evidence"]
    deployments = load_deployments()

    payload = empty_payload()
    payload["generated_at"] = datetime.now(timezone.utc).isoformat()
    payload.update(build_report_slice(events, daily_rows))
    payload["pairing"] = pairing
    payload["execution_diagnostics"] = build_execution_diagnostics(order_diagnostics)
    payload["runtime_evidence"] = runtime_evidence
    payload["deployments"] = deployments

    events_by_strategy = defaultdict(list)
    for event in events:
        events_by_strategy[strategy_key(event)].append(event)

    daily_by_strategy = defaultdict(list)
    for row in daily_rows:
        daily_by_strategy[strategy_key(row)].append(row)

    diagnostics_by_strategy = {strategy_key(row): row for row in order_diagnostics}
    deployments_by_id = {get_deployment_id(record): record for record in deployments}

    strategies = []
    for runtime_mode, strategy_id, deployment_id in sorted(events_by_strategy.keys()):
        strategy_events = events_by_strategy[(runtime_mode, strategy_id, deployment_id)]
        strategy_daily_rows = daily_by_strategy.get((runtime_mode, strategy_id, deployment_id), [])
        strategy_diagnostics = diagnostics_by_strategy.get((runtime_mode, strategy_id, deployment_id))
        deployment_record = deployments_by_id.get(deployment_id, {})
        strategy_payload = build_report_slice(strategy_events, strategy_daily_rows)
        strategy_payload.update(
            {
                "runtime_mode": runtime_mode,
                "strategy_id": strategy_id,
                "deployment_id": deployment_id,
                "label": strategy_label(runtime_mode, strategy_id, deployment_id),
                "experiment_label": experiment_label(runtime_mode, strategy_id, deployment_id),
                "activity_status": "has_track_record",
                "deployment_desired_state": normalize_state(deployment_record.get("desired_state")),
                "deployment_observed_state": normalize_state(deployment_record.get("observed_state")),
                "bundle_id": deployment_record.get("bundle_id"),
                "account_id": deployment_record.get("account_id"),
                "execution_diagnostics": build_execution_diagnostics(
                    [strategy_diagnostics] if strategy_diagnostics else []
                ),
            }
        )
        strategies.append(strategy_payload)

    strategy_keys = {strategy_key(strategy) for strategy in strategies}
    for key, strategy_diagnostics in sorted(diagnostics_by_strategy.items()):
        if key in strategy_keys:
            continue
        runtime_mode, strategy_id, dep_id = key
        deployment_record = deployments_by_id.get(dep_id, {})
        strategy_payload = deployment_report_row(deployment_record, strategy_diagnostics)
        strategy_payload.update(
            {
                "runtime_mode": runtime_mode,
                "strategy_id": strategy_id,
                "deployment_id": dep_id,
                "activity_status": "orders_without_event_track_record",
                "deployment_desired_state": normalize_state(deployment_record.get("desired_state")),
                "deployment_observed_state": normalize_state(deployment_record.get("observed_state")),
            }
        )
        strategies.append(strategy_payload)
        strategy_keys.add(key)

    for record in deployments:
        if not deployment_is_running(record):
            continue
        dep_id = get_deployment_id(record)
        runtime_mode = record.get("runtime_mode") or ("dryrun" if dep_id.endswith(".dryrun") else "paper")
        strategy_id = record.get("strategy_id") or record.get("bundle_id") or record.get("strategy") or ""
        key = (runtime_mode, strategy_id, dep_id)
        if key in strategy_keys or any(strategy.get("deployment_id") == dep_id for strategy in strategies):
            continue
        strategies.append(deployment_report_row(record))

    payload["strategies"] = sorted(
        strategies,
        key=lambda strategy: (
            strategy.get("activity_status") != "has_track_record",
            strategy.get("deployment_id") or "",
            strategy.get("strategy_id") or "",
        ),
    )
    print(json.dumps(payload, separators=(",", ":")))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
