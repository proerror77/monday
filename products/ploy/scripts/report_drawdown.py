#!/usr/bin/env python3
"""
Generate simulated drawdown reports (5m/15m) from the trading host Postgres via AWS SSM.

Two granularities are produced:
  - by product series: strategy x product_slug x side (recommended)
  - by individual market: strategy x market_slug x token (drill-down)

Outputs:
  - reports/drawdown_by_product.json
  - reports/drawdown_by_market.json
"""

from __future__ import annotations

import argparse
import base64
import gzip
import json
import math
import os
import re
import shlex
import sqlite3
import subprocess
import sys
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Iterable, List, Optional, Tuple


REPO_ROOT = Path(__file__).resolve().parents[1]
# SSM stdout truncates around ~24KB. Keep per-invocation payloads safely below that.
SSM_B64_CHUNK_BYTES = 20_000


def _run(argv: List[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(argv, check=False, text=True, capture_output=True)


def _infer_instance_id() -> Optional[str]:
    # Prefer explicit env var.
    env = os.environ.get("PLOY_AWS_INSTANCE_ID") or os.environ.get("AWS_SSM_INSTANCE_ID")
    if env:
        return env.strip()

    # Fall back to the repo's deploy script (keeps local config DRY).
    deploy = REPO_ROOT / "deploy_to_tango21.sh"
    if deploy.exists():
        m = re.search(r'^\s*INSTANCE_ID="([^"]+)"\s*$', deploy.read_text(), flags=re.MULTILINE)
        if m:
            return m.group(1).strip()

    return None


@dataclass(frozen=True)
class SsmResult:
    stdout: str
    stderr: str


def run_ssm(instance_id: str, commands: List[str]) -> SsmResult:
    # Send
    send = _run(
        [
            "aws",
            "ssm",
            "send-command",
            "--instance-ids",
            instance_id,
            "--document-name",
            "AWS-RunShellScript",
            "--parameters",
            f"commands={json.dumps(commands)}",
            "--query",
            "Command.CommandId",
            "--output",
            "text",
        ]
    )
    if send.returncode != 0:
        raise RuntimeError(f"aws ssm send-command failed:\n{send.stderr or send.stdout}".strip())

    command_id = (send.stdout or "").strip()
    if not command_id:
        raise RuntimeError("aws ssm send-command returned empty CommandId")

    # Wait
    wait = _run(["aws", "ssm", "wait", "command-executed", "--command-id", command_id, "--instance-id", instance_id])
    if wait.returncode != 0:
        # Still try to fetch output for better error context.
        pass

    # Fetch output
    inv = _run(
        [
            "aws",
            "ssm",
            "get-command-invocation",
            "--command-id",
            command_id,
            "--instance-id",
            instance_id,
            "--query",
            "{Out:StandardOutputContent,Err:StandardErrorContent,Status:Status}",
            "--output",
            "json",
        ]
    )
    if inv.returncode != 0:
        raise RuntimeError(f"aws ssm get-command-invocation failed:\n{inv.stderr or inv.stdout}".strip())

    payload = json.loads(inv.stdout)
    stdout = payload.get("Out") or ""
    stderr = payload.get("Err") or ""
    status = payload.get("Status") or ""
    if status != "Success":
        msg = stderr.strip() or stdout.strip() or f"SSM command status: {status}"
        raise RuntimeError(msg)

    return SsmResult(stdout=stdout, stderr=stderr)


def _build_timeframe_filter(timeframes: Iterable[str], *, column: str) -> Tuple[str, str]:
    """
    Returns:
      - WHERE clause fragment for timeframe filtering
      - CASE expression for timeframe label
    """
    tfs = [t.strip().lower() for t in timeframes if t.strip()]
    if not tfs:
        return "TRUE", "NULL"

    likes = []
    cases = []
    for tf in tfs:
        if not re.fullmatch(r"\d+m", tf):
            raise ValueError(f"Invalid timeframe: {tf!r} (expected like '5m' or '15m')")
        n = tf[:-1]
        likes.append(f"{column} LIKE '%-{n}m-%'")
        cases.append(f"WHEN {column} LIKE '%-{n}m-%' THEN '{tf}'")

    where = "(" + " OR ".join(likes) + ")"
    case = "CASE " + " ".join(cases) + " ELSE NULL END"
    return where, case


def _one_line_sql(sql: str) -> str:
    # SSM/AWS CLI parameter parsing can be finicky with embedded newlines. Keep SQL
    # to a single line so it survives multiple quoting/serialization layers.
    return re.sub(r"\s+", " ", sql).strip()

def _sql_lit(s: str) -> str:
    # Minimal SQL literal escaping for our use-case.
    return "'" + s.replace("'", "''") + "'"


def _select_sql_by_market(*, account_id: str, tf_where: str, tf_case: str, fee_rate: float, initial_capital: float) -> str:
    account_lit = _sql_lit(account_id)
    return _one_line_sql(
        f"""
        WITH closed AS (
          SELECT
            e.recorded_at AS ts,
            e.domain,
            e.market_slug,
            e.token_id,
            e.market_side,
            COALESCE(sh.strategy_id, 'unknown') AS strategy_id,
            COALESCE(sh.symbol, '') AS symbol,
            aoe.shares::numeric AS shares,
            e.entry_price::numeric AS entry_price,
            COALESCE(e.exit_price, aoe.avg_fill_price)::numeric AS exit_price
          FROM exit_reasons e
          JOIN agent_order_executions aoe
            ON aoe.intent_id = e.intent_id
          LEFT JOIN signal_history sh
            ON sh.intent_id = e.intent_id
          WHERE
            e.account_id = {account_lit}
            AND aoe.account_id = {account_lit}
            AND (sh.account_id = {account_lit} OR sh.account_id IS NULL)
            AND aoe.is_buy = FALSE
            AND aoe.dry_run = TRUE
            AND aoe.status = 'Filled'
            AND e.entry_price IS NOT NULL
            AND (e.exit_price IS NOT NULL OR aoe.avg_fill_price IS NOT NULL)
            AND {tf_where}
        ),
        pnl AS (
          SELECT
            ts, domain, market_slug, token_id, market_side, strategy_id, symbol,
            (exit_price - entry_price) * shares AS gross_pnl,
            ((exit_price - entry_price) * shares) - (entry_price * shares * {fee_rate}) AS net_pnl
          FROM closed
        ),
        eq AS (
          SELECT
            *,
            {initial_capital} + sum(net_pnl) OVER (
              PARTITION BY domain, strategy_id, market_slug, token_id
              ORDER BY ts
              ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
            ) AS equity
          FROM pnl
        ),
        dd AS (
          SELECT
            *,
            max(equity) OVER (
              PARTITION BY domain, strategy_id, market_slug, token_id
              ORDER BY ts
              ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
            ) AS peak_equity
          FROM eq
        )
        SELECT
          domain,
          strategy_id,
          {tf_case} AS timeframe,
          market_slug,
          market_side,
          token_id,
          max(symbol) AS symbol,
          count(*)::bigint AS trades,
          round(sum(net_pnl)::numeric, 6) AS total_pnl,
          round(avg(net_pnl)::numeric, 6) AS avg_pnl,
          round(max((peak_equity - equity) / NULLIF(peak_equity, 0))::numeric, 6) AS max_drawdown_pct,
          round(max(peak_equity - equity)::numeric, 6) AS max_drawdown_abs,
          min(ts) AS first_trade_at,
          max(ts) AS last_trade_at
        FROM dd
        GROUP BY domain, strategy_id, timeframe, market_slug, market_side, token_id
        ORDER BY domain, strategy_id, timeframe, market_slug, market_side
        """.strip()
    )


def _select_sql_by_product(*, account_id: str, tf_where: str, tf_case: str, fee_rate: float, initial_capital: float) -> str:
    account_lit = _sql_lit(account_id)
    # product_slug strips the trailing epoch so all 5m/15m "series" roll up together.
    return _one_line_sql(
        f"""
        WITH closed AS (
          SELECT
            e.recorded_at AS ts,
            e.domain,
            e.market_slug,
            regexp_replace(e.market_slug, '-[0-9]+$', '') AS product_slug,
            e.market_side,
            COALESCE(sh.strategy_id, 'unknown') AS strategy_id,
            COALESCE(sh.symbol, '') AS symbol,
            aoe.shares::numeric AS shares,
            e.entry_price::numeric AS entry_price,
            COALESCE(e.exit_price, aoe.avg_fill_price)::numeric AS exit_price
          FROM exit_reasons e
          JOIN agent_order_executions aoe
            ON aoe.intent_id = e.intent_id
          LEFT JOIN signal_history sh
            ON sh.intent_id = e.intent_id
          WHERE
            e.account_id = {account_lit}
            AND aoe.account_id = {account_lit}
            AND (sh.account_id = {account_lit} OR sh.account_id IS NULL)
            AND aoe.is_buy = FALSE
            AND aoe.dry_run = TRUE
            AND aoe.status = 'Filled'
            AND e.entry_price IS NOT NULL
            AND (e.exit_price IS NOT NULL OR aoe.avg_fill_price IS NOT NULL)
            AND {tf_where}
        ),
        pnl AS (
          SELECT
            ts, domain, market_slug, product_slug, market_side, strategy_id, symbol,
            (exit_price - entry_price) * shares AS gross_pnl,
            ((exit_price - entry_price) * shares) - (entry_price * shares * {fee_rate}) AS net_pnl
          FROM closed
        ),
        eq AS (
          SELECT
            *,
            {initial_capital} + sum(net_pnl) OVER (
              PARTITION BY domain, strategy_id, product_slug, market_side
              ORDER BY ts
              ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
            ) AS equity
          FROM pnl
        ),
        dd AS (
          SELECT
            *,
            max(equity) OVER (
              PARTITION BY domain, strategy_id, product_slug, market_side
              ORDER BY ts
              ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
            ) AS peak_equity
          FROM eq
        )
        SELECT
          domain,
          strategy_id,
          {tf_case} AS timeframe,
          product_slug,
          market_side,
          max(symbol) AS symbol,
          count(*)::bigint AS trades,
          count(distinct market_slug)::bigint AS markets,
          round(sum(net_pnl)::numeric, 6) AS total_pnl,
          round(avg(net_pnl)::numeric, 6) AS avg_pnl,
          round(max((peak_equity - equity) / NULLIF(peak_equity, 0))::numeric, 6) AS max_drawdown_pct,
          round(max(peak_equity - equity)::numeric, 6) AS max_drawdown_abs,
          min(ts) AS first_trade_at,
          max(ts) AS last_trade_at
        FROM dd
        GROUP BY domain, strategy_id, timeframe, product_slug, market_side
        ORDER BY domain, strategy_id, timeframe, product_slug, market_side
        """.strip()
    )


def _select_json_agg_sql(select_sql: str) -> str:
    return _one_line_sql(
        f"""
        SELECT COALESCE(jsonb_agg(to_jsonb(t)), '[]'::jsonb)
        FROM ({select_sql}) t
        """.strip()
    )


def _fetch_json_array(*, instance_id: str, select_sql: str) -> list:
    json_sql = _select_json_agg_sql(select_sql)
    remote = f"sudo -u postgres psql ploy -At -c {shlex.quote(json_sql)}"

    res = run_ssm(instance_id, [remote])
    raw = (res.stdout or "").strip()
    if not raw:
        return []
    return json.loads(raw)

def _fetch_rows_via_ssm_file(*, instance_id: str, select_sql: str, label: str) -> list:
    """
    For large result sets, we can't rely on StandardOutputContent (it truncates).

    Strategy:
      1) Run the SELECT on the instance and write NDJSON to /tmp.
      2) gzip + base64 it on the instance.
      3) Pull the base64 in fixed-size chunks via multiple SSM calls.
      4) Decode/decompress locally and parse NDJSON.
    """
    run_id = uuid.uuid4().hex[:12]
    run_dir = f"/tmp/ploy_drawdown_{label}_{run_id}"
    ndjson_path = f"{run_dir}/{label}.ndjson"
    gz_path = f"{ndjson_path}.gz"
    b64_path = f"{gz_path}.b64"

    ndjson_sql = _one_line_sql(f"SELECT row_to_json(t) FROM ({select_sql}) t;")

    prep_cmds = [
        "set -euo pipefail",
        f"mkdir -p {shlex.quote(run_dir)}",
        # Write NDJSON; redirect keeps output out of SSM stdout.
        f"sudo -u postgres psql ploy -At -c {shlex.quote(ndjson_sql)} > {shlex.quote(ndjson_path)}",
        f"gzip -9 -c {shlex.quote(ndjson_path)} > {shlex.quote(gz_path)}",
        f"base64 -w0 {shlex.quote(gz_path)} > {shlex.quote(b64_path)}",
        # Emit base64 byte size so we can chunk fetch.
        f"wc -c < {shlex.quote(b64_path)}",
    ]
    res = run_ssm(instance_id, prep_cmds)
    size_str = (res.stdout or "").strip().splitlines()[-1].strip()
    try:
        b64_size = int(size_str)
    except ValueError as e:
        raise RuntimeError(f"failed to parse remote base64 size: {size_str!r}") from e

    chunks: List[str] = []
    parts = int(math.ceil(b64_size / SSM_B64_CHUNK_BYTES)) if b64_size > 0 else 0
    for i in range(parts):
        # dd bs/skip operate on fixed-size blocks; this cleanly partitions the base64 file.
        dd = (
            f"dd if={shlex.quote(b64_path)} bs={SSM_B64_CHUNK_BYTES} skip={i} count=1 status=none"
        )
        part = run_ssm(instance_id, [dd]).stdout or ""
        chunks.append(part.strip())

    # Best-effort cleanup.
    try:
        run_ssm(instance_id, [f"rm -rf {shlex.quote(run_dir)}"])
    except Exception:
        pass

    b64 = "".join(chunks).strip()
    if not b64:
        return []

    blob = base64.b64decode(b64)
    ndjson_text = gzip.decompress(blob).decode("utf-8", errors="replace")

    rows = []
    for line in ndjson_text.splitlines():
        line = line.strip()
        if not line:
            continue
        rows.append(json.loads(line))
    return rows


def _write_json_report(*, instance_id: str, out_dir: Path, basename: str, select_sql: str, via_file: bool = False) -> list:
    if via_file:
        data = _fetch_rows_via_ssm_file(instance_id=instance_id, select_sql=select_sql, label=basename)
    else:
        data = _fetch_json_array(instance_id=instance_id, select_sql=select_sql)

    json_path = out_dir / f"{basename}.json"
    json_path.write_text(json.dumps(data, indent=2, sort_keys=False))

    print(str(json_path))
    print(f"{basename}: rows={len(data)}")
    return data


def _write_sqlite_table(conn: sqlite3.Connection, table_name: str, rows: list) -> None:
    conn.execute(f'DROP TABLE IF EXISTS "{table_name}"')

    if not rows:
        conn.execute(f'CREATE TABLE "{table_name}" (__empty INTEGER)')
        conn.execute(f'DELETE FROM "{table_name}"')
        return

    columns = sorted({k for row in rows for k in row.keys()})
    col_defs = ", ".join([f'"{c}" TEXT' for c in columns])
    conn.execute(f'CREATE TABLE "{table_name}" ({col_defs})')

    placeholders = ", ".join(["?"] * len(columns))
    quoted_cols = ", ".join([f'"{c}"' for c in columns])
    insert_sql = f'INSERT INTO "{table_name}" ({quoted_cols}) VALUES ({placeholders})'

    for row in rows:
        vals = []
        for c in columns:
            v = row.get(c)
            if v is None:
                vals.append(None)
            elif isinstance(v, (dict, list)):
                vals.append(json.dumps(v, ensure_ascii=False))
            elif isinstance(v, bool):
                vals.append("1" if v else "0")
            else:
                vals.append(str(v))
        conn.execute(insert_sql, vals)


def _write_sqlite_reports(*, sqlite_path: Path, product_rows: list, market_rows: Optional[list]) -> None:
    sqlite_path.parent.mkdir(parents=True, exist_ok=True)
    conn = sqlite3.connect(sqlite_path)
    try:
        conn.execute("PRAGMA journal_mode=WAL")
        _write_sqlite_table(conn, "drawdown_by_product", product_rows)
        if market_rows is not None:
            _write_sqlite_table(conn, "drawdown_by_market", market_rows)
        conn.commit()
    finally:
        conn.close()

    print(str(sqlite_path))
    if market_rows is None:
        print("sqlite: tables=drawdown_by_product")
    else:
        print("sqlite: tables=drawdown_by_product,drawdown_by_market")


def main() -> int:
    p = argparse.ArgumentParser(description="Report simulated drawdown per strategy x market (5m/15m) via AWS SSM")
    p.add_argument("--instance-id", default=_infer_instance_id(), help="SSM instance id (default: inferred)")
    p.add_argument("--account-id", default="default", help="account_id in observability tables (default: default)")
    p.add_argument("--initial-capital", type=float, default=1000.0, help="starting equity per group (default: 1000)")
    p.add_argument("--fee-rate", type=float, default=0.02, help="fee model: fee = entry_cost * fee_rate (default: 0.02)")
    p.add_argument("--timeframes", default="5m,15m", help="comma-separated list (default: 5m,15m)")
    p.add_argument("--include-market", action="store_true", help="also generate per-market drill-down (large; fetched via chunked SSM file)")
    p.add_argument("--out-dir", default=str(REPO_ROOT / "reports"), help="output directory (default: ./reports)")
    p.add_argument(
        "--sqlite-file",
        default=str(REPO_ROOT / "reports" / "drawdown.sqlite"),
        help="optional SQLite output path (default: ./reports/drawdown.sqlite, set empty to disable)",
    )
    args = p.parse_args()

    if not args.instance_id:
        print("ERROR: missing --instance-id (and couldn't infer one).", file=sys.stderr)
        return 2

    tf_where, _tf_case_where = _build_timeframe_filter(args.timeframes.split(","), column="e.market_slug")
    _tf_where_case, tf_case = _build_timeframe_filter(args.timeframes.split(","), column="market_slug")
    fee_rate = float(args.fee_rate)
    initial_capital = float(args.initial_capital)
    account_id = args.account_id

    out_dir = Path(args.out_dir)
    out_dir.mkdir(parents=True, exist_ok=True)

    product_sql = _select_sql_by_product(
        account_id=account_id,
        tf_where=tf_where,
        tf_case=tf_case,
        fee_rate=fee_rate,
        initial_capital=initial_capital,
    )
    product_rows = _write_json_report(
        instance_id=args.instance_id,
        out_dir=out_dir,
        basename="drawdown_by_product",
        select_sql=product_sql,
    )
    market_rows: Optional[list] = None
    if args.include_market:
        market_sql = _select_sql_by_market(
            account_id=account_id,
            tf_where=tf_where,
            tf_case=tf_case,
            fee_rate=fee_rate,
            initial_capital=initial_capital,
        )
        market_rows = _write_json_report(
            instance_id=args.instance_id,
            out_dir=out_dir,
            basename="drawdown_by_market",
            select_sql=market_sql,
            via_file=True,
        )

    sqlite_file = (args.sqlite_file or "").strip()
    if sqlite_file:
        _write_sqlite_reports(
            sqlite_path=Path(sqlite_file),
            product_rows=product_rows,
            market_rows=market_rows,
        )

    return 0


if __name__ == "__main__":
    raise SystemExit(main())
