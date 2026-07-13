#!/usr/bin/env python3
"""Generate a self-contained HTML strategy performance report.

Usage:
    python3 scripts/report_strategy.py [--since YYYY-MM-DD] [--host HOST]

Output: reports/strategy_report.html
"""

import json
import os
import subprocess
import sys
from datetime import datetime
from pathlib import Path

HOST = os.environ.get("PLOY_RESEARCH_HOST", "tango-1-1")
SINCE = None
CONFIG_PATH = "config/strategies/02-pm5d-threelayer.unified.toml"
DB_URL = (
    os.environ.get("PLOY_DATABASE__URL")
    or os.environ.get("DATABASE_URL")
    or "postgresql://postgres:postgres@localhost:5432/ploy"
)

for i, arg in enumerate(sys.argv[1:], 1):
    if arg == "--since" and i < len(sys.argv) - 1:
        SINCE = sys.argv[i + 1]
    elif arg == "--host" and i < len(sys.argv) - 1:
        HOST = sys.argv[i + 1]

SINCE_FILTER = f"AND COALESCE(closed_at, opened_at) >= '{SINCE}T00:00:00+08'" if SINCE else ""
DAILY_FILTER = f"AND trading_day_cst >= '{SINCE}'" if SINCE else ""
ORDERS_FILTER = f"AND created_at >= '{SINCE}T00:00:00+08'" if SINCE else ""


def run_sql(query: str) -> str:
    if HOST in {"local", "localhost", "127.0.0.1", "self"}:
        cmd = [
            "psql",
            DB_URL,
            "-t",
            "-A",
            "-F",
            "|",
            "-v",
            "ON_ERROR_STOP=1",
            "-c",
            query,
        ]
    else:
        cmd = [
            "ssh",
            HOST,
            f'PGPASSWORD=postgres psql -U postgres -d ploy -t -A -F"|" -c "{query}"',
        ]
    result = subprocess.run(cmd, capture_output=True, text=True, timeout=30)
    lines = [
        l for l in result.stdout.strip().split("\n")
        if l and not l.startswith("perl:") and "LANGUAGE" not in l
        and "LC_" not in l and "LANG" not in l and "are supported" not in l
    ]
    return "\n".join(lines)

# --- Fetch data ---

print(f"Fetching data from {HOST}...")

# Q1: All closed trades for cumulative PnL + detail table.
# Strategy sizing is requested in USD stake, while fills are recorded as shares.
# Show both requested stake and actual filled notional so liquidity partial fills
# are visible instead of being mistaken for a smaller configured stake.
trades_raw = run_sql(f"""
WITH closed AS (
  SELECT *
  FROM strategy_runtime_event_track_record
  WHERE runtime_mode = 'dry_run' AND is_closed {SINCE_FILTER}
)
SELECT t.symbol, t.market_side,
       ROUND(t.avg_entry_price::numeric, 4),
       ROUND(t.avg_exit_price::numeric, 4),
       ROUND(t.buy_quantity::numeric, 2),
       ROUND(COALESCE(NULLIF(o.buy_filled_notional, 0), t.buy_notional)::numeric, 2),
       ROUND(COALESCE(NULLIF(o.buy_requested_notional, 0), t.buy_notional)::numeric, 2),
       ROUND(COALESCE(NULLIF(o.buy_filled_quantity, 0), t.buy_quantity)::numeric, 2),
       COALESCE(o.latest_buy_status, ''),
       COALESCE(o.buy_orders, 0),
       COALESCE(o.rejected_buy_orders, 0),
       ROUND(t.net_pnl::numeric, 2),
       ROUND(t.total_fee::numeric, 4),
       to_char(t.opened_at AT TIME ZONE 'Asia/Shanghai', 'YYYY-MM-DD HH24:MI'),
       to_char(t.closed_at AT TIME ZONE 'Asia/Shanghai', 'YYYY-MM-DD HH24:MI')
FROM closed t
LEFT JOIN LATERAL (
  SELECT
    COALESCE(SUM(quantity * COALESCE(limit_price, avg_fill_price, 0)) FILTER (WHERE order_side = 'BUY'), 0) AS buy_requested_notional,
    COALESCE(SUM(filled_quantity * COALESCE(avg_fill_price, limit_price, 0)) FILTER (WHERE order_side = 'BUY'), 0) AS buy_filled_notional,
    COALESCE(SUM(filled_quantity) FILTER (WHERE order_side = 'BUY'), 0) AS buy_filled_quantity,
    COUNT(*) FILTER (WHERE order_side = 'BUY') AS buy_orders,
    COUNT(*) FILTER (
      WHERE order_side = 'BUY'
        AND (LOWER(status) = 'rejected' OR rejection_reason IS NOT NULL)
    ) AS rejected_buy_orders,
    MAX(status) FILTER (WHERE order_side = 'BUY') AS latest_buy_status
  FROM strategy_runtime_orders o
  WHERE o.runtime_mode = t.runtime_mode
    AND o.strategy_id = t.strategy_id
    AND (
      o.intent_id = t.intent_id
      OR (o.event_id = t.event_id AND o.token_id = t.token_id)
    )
) o ON true
ORDER BY t.closed_at
""")

trades = []
cum_pnl = 0.0
for line in trades_raw.split("\n"):
    if not line.strip():
        continue
    parts = line.split("|")
    if len(parts) < 15:
        continue
    pnl = float(parts[11])
    cum_pnl += pnl
    exit_px = float(parts[3]) if parts[3] else 0
    if exit_px >= 0.99:
        exit_type = "WIN"
    elif exit_px <= 0.01:
        exit_type = "LOSS"
    else:
        exit_type = "TP/SL"
    trades.append({
        "symbol": parts[0], "side": parts[1],
        "entry": float(parts[2]), "exit": exit_px,
        "qty": float(parts[4]),
        "filled_notional": float(parts[5]),
        "requested_notional": float(parts[6]),
        "filled_qty": float(parts[7]),
        "order_status": parts[8],
        "buy_orders": int(parts[9] or 0),
        "rejected_buy_orders": int(parts[10] or 0),
        "pnl": pnl, "fee": float(parts[12]),
        "exit_type": exit_type,
        "opened": parts[13], "closed": parts[14],
        "cum_pnl": round(cum_pnl, 2),
    })

# Q2: Daily aggregation
daily_raw = run_sql(f"""
SELECT trading_day_cst,
       trade_count, closed_trade_count,
       winning_trade_count_all, losing_trade_count_all,
       ROUND(net_pnl::numeric, 2),
       ROUND(total_fee::numeric, 2)
FROM strategy_runtime_daily_track_record
WHERE runtime_mode = 'dry_run' {DAILY_FILTER}
ORDER BY trading_day_cst
""")

daily = []
for line in daily_raw.split("\n"):
    if not line.strip():
        continue
    parts = line.split("|")
    if len(parts) < 7:
        continue
    daily.append({
        "day": parts[0], "trades": int(parts[1]), "closed": int(parts[2]),
        "wins": int(parts[3]), "losses": int(parts[4]),
        "pnl": float(parts[5]), "fees": float(parts[6]),
    })

# Q3: Open positions
open_raw = run_sql(f"""
SELECT symbol, market_side,
       ROUND(avg_entry_price::numeric, 4),
       ROUND(buy_quantity::numeric, 2),
       ROUND(COALESCE(NULLIF(o.buy_filled_notional, 0), buy_notional)::numeric, 2),
       ROUND(COALESCE(NULLIF(o.buy_requested_notional, 0), buy_notional)::numeric, 2),
       ROUND(COALESCE(NULLIF(o.buy_filled_quantity, 0), buy_quantity)::numeric, 2),
       COALESCE(o.latest_buy_status, ''),
       COALESCE(o.buy_orders, 0),
       COALESCE(o.rejected_buy_orders, 0),
       to_char(opened_at AT TIME ZONE 'Asia/Shanghai', 'YYYY-MM-DD HH24:MI')
FROM strategy_runtime_event_track_record t
LEFT JOIN LATERAL (
  SELECT
    COALESCE(SUM(quantity * COALESCE(limit_price, avg_fill_price, 0)) FILTER (WHERE order_side = 'BUY'), 0) AS buy_requested_notional,
    COALESCE(SUM(filled_quantity * COALESCE(avg_fill_price, limit_price, 0)) FILTER (WHERE order_side = 'BUY'), 0) AS buy_filled_notional,
    COALESCE(SUM(filled_quantity) FILTER (WHERE order_side = 'BUY'), 0) AS buy_filled_quantity,
    COUNT(*) FILTER (WHERE order_side = 'BUY') AS buy_orders,
    COUNT(*) FILTER (
      WHERE order_side = 'BUY'
        AND (LOWER(status) = 'rejected' OR rejection_reason IS NOT NULL)
    ) AS rejected_buy_orders,
    MAX(status) FILTER (WHERE order_side = 'BUY') AS latest_buy_status
  FROM strategy_runtime_orders o
  WHERE o.runtime_mode = t.runtime_mode
    AND o.strategy_id = t.strategy_id
    AND (
      o.intent_id = t.intent_id
      OR (o.event_id = t.event_id AND o.token_id = t.token_id)
    )
) o ON true
WHERE t.runtime_mode = 'dry_run' AND NOT t.is_closed {SINCE_FILTER}
ORDER BY t.opened_at DESC
""")

open_positions = []
for line in open_raw.split("\n"):
    if not line.strip():
        continue
    parts = line.split("|")
    if len(parts) < 11:
        continue
    open_positions.append({
        "symbol": parts[0], "side": parts[1], "entry": float(parts[2]),
        "qty": float(parts[3]),
        "notional": float(parts[4]),
        "requested_notional": float(parts[5]),
        "filled_qty": float(parts[6]),
        "order_status": parts[7],
        "buy_orders": int(parts[8] or 0),
        "rejected_buy_orders": int(parts[9] or 0),
        "opened": parts[10],
    })

# Q4: Order diagnostics independent of the trade view, so rejected orders with
# no fill still show up in the report.
orders_raw = run_sql(f"""
SELECT
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
  ROUND(COALESCE(SUM(quantity * COALESCE(limit_price, avg_fill_price, 0)) FILTER (WHERE order_side = 'BUY'), 0), 2) AS buy_requested_notional,
  ROUND(COALESCE(SUM(filled_quantity * COALESCE(avg_fill_price, limit_price, 0)) FILTER (WHERE order_side = 'BUY'), 0), 2) AS buy_filled_notional
FROM strategy_runtime_orders
WHERE runtime_mode = 'dry_run' {ORDERS_FILTER}
""")

order_diagnostics = {
    "total_orders": 0,
    "buy_orders": 0,
    "sell_orders": 0,
    "rejected_orders": 0,
    "rejected_buy_orders": 0,
    "partial_buy_orders": 0,
    "buy_requested_notional": 0.0,
    "buy_filled_notional": 0.0,
}
for line in orders_raw.split("\n"):
    if not line.strip():
        continue
    parts = line.split("|")
    if len(parts) < 8:
        continue
    order_diagnostics = {
        "total_orders": int(parts[0] or 0),
        "buy_orders": int(parts[1] or 0),
        "sell_orders": int(parts[2] or 0),
        "rejected_orders": int(parts[3] or 0),
        "rejected_buy_orders": int(parts[4] or 0),
        "partial_buy_orders": int(parts[5] or 0),
        "buy_requested_notional": float(parts[6] or 0),
        "buy_filled_notional": float(parts[7] or 0),
    }

# --- Compute summary stats ---
import math

closed_trades = [t for t in trades if True]  # all are closed
total = len(closed_trades)
wins = sum(1 for t in closed_trades if t["pnl"] > 0)
losses = total - wins
win_rate = (wins / total * 100) if total > 0 else 0
total_pnl = sum(t["pnl"] for t in closed_trades)
total_fees = sum(t["fee"] for t in closed_trades)
avg_pnl = total_pnl / total if total > 0 else 0
total_requested_notional = sum(t["requested_notional"] for t in closed_trades)
total_filled_notional = sum(t["filled_notional"] for t in closed_trades)
avg_requested_notional = total_requested_notional / total if total > 0 else 0
avg_filled_notional = total_filled_notional / total if total > 0 else 0
partial_fills = sum(
    1 for t in closed_trades
    if t["requested_notional"] > 0 and t["filled_notional"] < t["requested_notional"] * 0.98
)
rejected_buy_orders = order_diagnostics["rejected_buy_orders"]
buy_orders = order_diagnostics["buy_orders"]
rejected_buy_rate = rejected_buy_orders / buy_orders * 100 if buy_orders > 0 else 0

# Sharpe ratios use explicit bases. Per-trade matches the JSON dry-run report;
# daily annualized is kept as a secondary diagnostic.
trade_pnls = [t["pnl"] for t in closed_trades]
if len(trade_pnls) > 1:
    mean_t = sum(trade_pnls) / len(trade_pnls)
    var_t = sum((x - mean_t) ** 2 for x in trade_pnls) / (len(trade_pnls) - 1)
    std_t = math.sqrt(var_t)
    trade_sharpe = (mean_t / std_t) * math.sqrt(len(trade_pnls)) if std_t > 0 else 0
else:
    trade_sharpe = 0

daily_pnls = [d["pnl"] for d in daily]
if len(daily_pnls) > 1:
    mean_d = sum(daily_pnls) / len(daily_pnls)
    var_d = sum((x - mean_d) ** 2 for x in daily_pnls) / (len(daily_pnls) - 1)
    std_d = math.sqrt(var_d)
    daily_sharpe_ann = (mean_d / std_d) * math.sqrt(365) if std_d > 0 else 0
else:
    daily_sharpe_ann = 0

# Max drawdown
peak = 0
max_dd = 0
for t in trades:
    if t["cum_pnl"] > peak:
        peak = t["cum_pnl"]
    dd = peak - t["cum_pnl"]
    if dd > max_dd:
        max_dd = dd

# Per-symbol stats
symbol_stats = {}
for t in closed_trades:
    s = t["symbol"]
    if s not in symbol_stats:
        symbol_stats[s] = {"trades": 0, "wins": 0, "losses": 0, "pnl": 0, "entries": []}
    symbol_stats[s]["trades"] += 1
    if t["pnl"] > 0:
        symbol_stats[s]["wins"] += 1
    else:
        symbol_stats[s]["losses"] += 1
    symbol_stats[s]["pnl"] += t["pnl"]
    symbol_stats[s]["entries"].append(t["entry"])

symbols_data = []
for s, v in sorted(symbol_stats.items(), key=lambda x: -x[1]["pnl"]):
    avg_entry = sum(v["entries"]) / len(v["entries"]) if v["entries"] else 0
    wr = v["wins"] / v["trades"] * 100 if v["trades"] > 0 else 0
    symbols_data.append({
        "symbol": s, "trades": v["trades"], "wins": v["wins"], "losses": v["losses"],
        "win_rate": round(wr, 1), "pnl": round(v["pnl"], 2), "avg_entry": round(avg_entry, 4),
    })

# --- Read strategy config ---
config_lines = []
config_path = Path(CONFIG_PATH)
if config_path.exists():
    config_lines = config_path.read_text().strip().split("\n")

# --- Generate HTML ---
print(f"Generating report: {total} trades, PnL ${total_pnl:.2f}")

cum_labels = json.dumps([t["closed"] for t in trades])
cum_values = json.dumps([t["cum_pnl"] for t in trades])
daily_labels = json.dumps([d["day"] for d in daily])
daily_values = json.dumps([d["pnl"] for d in daily])
daily_colors = json.dumps(["#22c55e" if d["pnl"] >= 0 else "#ef4444" for d in daily])
sym_labels = json.dumps([s["symbol"] for s in symbols_data])
sym_values = json.dumps([s["pnl"] for s in symbols_data])
sym_colors = json.dumps(["#22c55e" if s["pnl"] >= 0 else "#ef4444" for s in symbols_data])

since_label = f" (since {SINCE})" if SINCE else ""
generated = datetime.now().strftime("%Y-%m-%d %H:%M")

# Config params for display
config_display = []
for line in config_lines:
    line = line.strip()
    if line.startswith("#") or line.startswith("[") or not line or "=" not in line:
        continue
    config_display.append(line)

recent = trades[-30:] if len(trades) > 30 else trades
recent.reverse()

# Build symbol table rows
sym_rows = ""
for s in symbols_data:
    color = "#22c55e" if s["pnl"] >= 0 else "#ef4444"
    sym_rows += f"""<tr>
      <td>{s['symbol']}</td><td>{s['trades']}</td>
      <td>{s['wins']}</td><td>{s['losses']}</td>
      <td>{s['win_rate']}%</td>
      <td style="color:{color};font-weight:600">${s['pnl']:,.2f}</td>
      <td>{s['avg_entry']:.4f}</td>
    </tr>"""

# Build trade detail rows
trade_rows = ""
for t in recent:
    color = "#22c55e" if t["pnl"] >= 0 else "#ef4444"
    et_color = {"WIN": "#22c55e", "LOSS": "#ef4444", "TP/SL": "#f59e0b"}.get(t["exit_type"], "#888")
    fill_ratio = (
        t["filled_notional"] / t["requested_notional"] * 100
        if t["requested_notional"] > 0 else 0
    )
    fill_color = "#f59e0b" if fill_ratio < 98 else "#cbd5e1"
    trade_rows += f"""<tr>
      <td>{t['symbol']}</td><td>{t['side']}</td>
      <td>{t['entry']:.4f}</td><td>{t['exit']:.4f}</td>
      <td style="color:{et_color};font-weight:600">{t['exit_type']}</td>
      <td>${t['requested_notional']:.2f}</td>
      <td style="color:{fill_color};font-weight:600">${t['filled_notional']:.2f}</td>
      <td style="color:{fill_color}">{fill_ratio:.0f}%</td>
      <td>{t['buy_orders']}</td>
      <td>{t['rejected_buy_orders']}</td>
      <td>{t['qty']:.1f}</td>
      <td style="color:{color};font-weight:600">${t['pnl']:,.2f}</td>
      <td>{t['opened']}</td><td>{t['closed']}</td>
    </tr>"""

# Build open positions rows
open_rows = ""
for p in open_positions:
    fill_ratio = (
        p["notional"] / p["requested_notional"] * 100
        if p["requested_notional"] > 0 else 0
    )
    fill_color = "#f59e0b" if fill_ratio < 98 else "#cbd5e1"
    open_rows += f"""<tr>
      <td>{p['symbol']}</td><td>{p['side']}</td>
      <td>{p['entry']:.4f}</td>
      <td>${p['requested_notional']:.2f}</td>
      <td style="color:{fill_color};font-weight:600">${p['notional']:.2f}</td>
      <td style="color:{fill_color}">{fill_ratio:.0f}%</td>
      <td>{p['buy_orders']}</td><td>{p['rejected_buy_orders']}</td>
      <td>{p['qty']:.1f}</td><td>{p['opened']}</td>
    </tr>"""

# Config rows
config_rows = ""
for line in config_display:
    k, v = line.split("=", 1)
    config_rows += f"<tr><td>{k.strip()}</td><td>{v.strip()}</td></tr>"

pnl_color = "#22c55e" if total_pnl >= 0 else "#ef4444"
dd_pct = (max_dd / peak * 100) if peak > 0 else 0

html = f"""<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="UTF-8">
<meta name="viewport" content="width=device-width, initial-scale=1.0">
<title>Three-Layer Strategy Report{since_label}</title>
<script src="https://cdn.jsdelivr.net/npm/chart.js@4"></script>
<style>
  * {{ margin: 0; padding: 0; box-sizing: border-box; }}
  body {{ font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
         background: #0f172a; color: #e2e8f0; padding: 24px; }}
  .container {{ max-width: 1200px; margin: 0 auto; }}
  h1 {{ font-size: 1.5rem; margin-bottom: 4px; }}
  .subtitle {{ color: #94a3b8; font-size: 0.85rem; margin-bottom: 24px; }}
  .cards {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(160px, 1fr));
            gap: 12px; margin-bottom: 24px; }}
  .card {{ background: #1e293b; border-radius: 8px; padding: 16px; }}
  .card .label {{ color: #94a3b8; font-size: 0.75rem; text-transform: uppercase; }}
  .card .value {{ font-size: 1.4rem; font-weight: 700; margin-top: 4px; }}
  .chart-box {{ background: #1e293b; border-radius: 8px; padding: 16px; margin-bottom: 24px; }}
  .chart-box h2 {{ font-size: 1rem; margin-bottom: 12px; color: #cbd5e1; }}
  table {{ width: 100%; border-collapse: collapse; font-size: 0.8rem; }}
  th {{ text-align: left; padding: 8px; color: #94a3b8; border-bottom: 1px solid #334155;
       font-weight: 500; text-transform: uppercase; font-size: 0.7rem; }}
  td {{ padding: 6px 8px; border-bottom: 1px solid #1e293b; }}
  tr:hover {{ background: #1e293b; }}
  .section {{ margin-bottom: 24px; }}
  .section h2 {{ font-size: 1rem; color: #cbd5e1; margin-bottom: 12px; }}
  .config-grid {{ display: grid; grid-template-columns: 1fr 1fr; gap: 0; }}
  .config-grid td {{ font-family: 'SF Mono', 'Fira Code', monospace; font-size: 0.75rem; }}
  .config-grid td:first-child {{ color: #94a3b8; }}
</style>
</head>
<body>
<div class="container">

<h1>Three-Layer Scoring Strategy</h1>
<p class="subtitle">Dry-run report{since_label} &middot; Generated {generated}</p>

<div class="cards">
  <div class="card"><div class="label">Net PnL</div>
    <div class="value" style="color:{pnl_color}">${total_pnl:,.2f}</div></div>
  <div class="card"><div class="label">Trades</div>
    <div class="value">{total}</div></div>
  <div class="card"><div class="label">Win Rate</div>
    <div class="value">{win_rate:.1f}%</div></div>
  <div class="card"><div class="label">Sharpe / Trade</div>
    <div class="value">{trade_sharpe:.1f}</div></div>
  <div class="card"><div class="label">Sharpe Daily Ann</div>
    <div class="value">{daily_sharpe_ann:.1f}</div></div>
  <div class="card"><div class="label">Max Drawdown</div>
    <div class="value" style="color:#ef4444">${max_dd:,.2f}</div></div>
  <div class="card"><div class="label">Avg PnL/Trade</div>
    <div class="value">${avg_pnl:,.2f}</div></div>
  <div class="card"><div class="label">Avg Req Stake</div>
    <div class="value">${avg_requested_notional:,.2f}</div></div>
  <div class="card"><div class="label">Avg Filled Stake</div>
    <div class="value">${avg_filled_notional:,.2f}</div></div>
  <div class="card"><div class="label">Partial Fills</div>
    <div class="value">{partial_fills}</div></div>
  <div class="card"><div class="label">Rejected BUY</div>
    <div class="value">{rejected_buy_orders}</div></div>
  <div class="card"><div class="label">BUY Reject Rate</div>
    <div class="value">{rejected_buy_rate:.1f}%</div></div>
  <div class="card"><div class="label">Total Fees</div>
    <div class="value">${total_fees:,.2f}</div></div>
  <div class="card"><div class="label">Open Positions</div>
    <div class="value">{len(open_positions)}</div></div>
</div>

<div class="chart-box">
  <h2>Cumulative PnL</h2>
  <canvas id="cumChart" height="80"></canvas>
</div>

<div class="chart-box">
  <h2>Daily PnL</h2>
  <canvas id="dailyChart" height="60"></canvas>
</div>


<div class="chart-box">
  <h2>PnL by Symbol</h2>
  <canvas id="symChart" height="50"></canvas>
</div>

<div class="section chart-box">
  <h2>Per Symbol</h2>
  <table>
    <tr><th>Symbol</th><th>Trades</th><th>Wins</th><th>Losses</th>
        <th>Win Rate</th><th>Net PnL</th><th>Avg Entry</th></tr>
    {sym_rows}
  </table>
</div>

<div class="section chart-box">
  <h2>Recent Trades (last 30)</h2>
  <table>
    <tr><th>Symbol</th><th>Side</th><th>Entry</th><th>Exit</th>
        <th>Type</th><th>Req Stake</th><th>Filled</th><th>Fill %</th>
        <th>Orders</th><th>Rejected</th><th>Shares</th><th>Net PnL</th><th>Opened</th><th>Closed</th></tr>
    {trade_rows}
  </table>
</div>

{"<div class='section chart-box'><h2>Open Positions</h2><table><tr><th>Symbol</th><th>Side</th><th>Entry</th><th>Req Stake</th><th>Filled</th><th>Fill %</th><th>Orders</th><th>Rejected</th><th>Shares</th><th>Opened</th></tr>" + open_rows + "</table></div>" if open_positions else ""}

<div class="section chart-box">
  <h2>Strategy Parameters</h2>
  <table class="config-grid">
    {config_rows}
  </table>
</div>

</div>

<script>
const cumCtx = document.getElementById('cumChart').getContext('2d');
new Chart(cumCtx, {{
  type: 'line',
  data: {{
    labels: {cum_labels},
    datasets: [{{
      label: 'Cumulative PnL ($)',
      data: {cum_values},
      borderColor: '#22c55e', backgroundColor: 'rgba(34,197,94,0.1)',
      fill: true, tension: 0.3, pointRadius: 0, borderWidth: 2
    }}]
  }},
  options: {{
    responsive: true,
    plugins: {{ legend: {{ display: false }} }},
    scales: {{
      x: {{ display: true, ticks: {{ maxTicksLimit: 10, color: '#64748b', font: {{ size: 10 }} }},
            grid: {{ color: '#1e293b' }} }},
      y: {{ ticks: {{ color: '#64748b', callback: v => '$' + v }}, grid: {{ color: '#334155' }} }}
    }}
  }}
}});

const dailyCtx = document.getElementById('dailyChart').getContext('2d');
new Chart(dailyCtx, {{
  type: 'bar',
  data: {{
    labels: {daily_labels},
    datasets: [{{
      label: 'Daily PnL ($)',
      data: {daily_values},
      backgroundColor: {daily_colors},
      borderRadius: 4
    }}]
  }},
  options: {{
    responsive: true,
    plugins: {{ legend: {{ display: false }} }},
    scales: {{
      x: {{ ticks: {{ color: '#64748b', font: {{ size: 10 }} }}, grid: {{ display: false }} }},
      y: {{ ticks: {{ color: '#64748b', callback: v => '$' + v }}, grid: {{ color: '#334155' }} }}
    }}
  }}
}});

const symCtx = document.getElementById('symChart').getContext('2d');
new Chart(symCtx, {{
  type: 'bar',
  data: {{
    labels: {sym_labels},
    datasets: [{{
      label: 'Net PnL ($)',
      data: {sym_values},
      backgroundColor: {sym_colors},
      borderRadius: 4
    }}]
  }},
  options: {{
    responsive: true, indexAxis: 'y',
    plugins: {{ legend: {{ display: false }} }},
    scales: {{
      x: {{ ticks: {{ color: '#64748b', callback: v => '$' + v }}, grid: {{ color: '#334155' }} }},
      y: {{ ticks: {{ color: '#e2e8f0', font: {{ size: 11 }} }}, grid: {{ display: false }} }}
    }}
  }}
}});
</script>
</body></html>
"""

out_path = Path("reports/strategy_report.html")
out_path.parent.mkdir(parents=True, exist_ok=True)
out_path.write_text(html)

print(f"Report saved to {out_path}")
print(
    f"  Trades: {total} | Win rate: {win_rate:.1f}% | PnL: ${total_pnl:,.2f} "
    f"| Sharpe/trade: {trade_sharpe:.1f} | Sharpe daily ann: {daily_sharpe_ann:.1f} "
    f"| Rejected BUY: {rejected_buy_orders}"
)
