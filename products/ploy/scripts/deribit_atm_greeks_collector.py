#!/usr/bin/env python3
"""Collect Deribit ATM option greeks into PostgreSQL.

The collector reuses fresh instruments already present in deribit_iv_ticks,
selects the nearest ATM option per configured currency, then calls Deribit's
public get_order_book endpoint for greeks and order-book fields.
"""

from __future__ import annotations

import datetime as dt
import json
import math
import os
import signal
import subprocess
import time
from typing import Any, Dict, List, Optional, Tuple

import requests

try:
    from scripts.psql_connection import psql_environment
except ModuleNotFoundError:
    from psql_connection import psql_environment

API_BASE = os.getenv("DERIBIT_API_BASE", "https://www.deribit.com/api/v2/public")
CURRENCIES = [
    c.strip().upper()
    for c in os.getenv("DERIBIT_GREEKS_CURRENCIES", "BTC,ETH,SOL").split(",")
    if c.strip()
]
POLL_SECS = max(5, int(os.getenv("DERIBIT_GREEKS_POLL_SECS", "30")))
HTTP_TIMEOUT_SECS = int(os.getenv("DERIBIT_GREEKS_HTTP_TIMEOUT_SECS", "20"))

DB_URL = (
    os.getenv("PLOY_DATABASE__URL")
    or os.getenv("DATABASE_URL")
    or "postgresql://postgres:postgres@localhost:5432/ploy"
)
RUNNING = True


class GracefulShutdown(Exception):
    """Raised when an in-flight subprocess is interrupted by service shutdown."""


def _on_signal(signum: int, _frame: Any) -> None:
    global RUNNING
    RUNNING = False
    print(f"[deribit-greeks] received signal={signum}, stopping...", flush=True)


def _run_psql(sql: str, at_mode: bool = False) -> str:
    cmd = ["psql", "-v", "ON_ERROR_STOP=1", "-X"]
    if at_mode:
        cmd.extend(["-A", "-t", "-F", "\t"])
    cmd.extend(["-f", "-"])

    proc = subprocess.run(
        cmd,
        input=sql,
        env=psql_environment(DB_URL),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        output = proc.stderr.strip() or proc.stdout.strip()
        if not RUNNING and not output:
            raise GracefulShutdown("psql interrupted by service shutdown")
        raise RuntimeError(f"psql failed: {output}")
    return proc.stdout


def _sql_text(value: Optional[str]) -> str:
    if value is None:
        return "NULL"
    return "'" + value.replace("'", "''") + "'"


def _sql_num(value: Optional[float]) -> str:
    if value is None:
        return "NULL"
    if not math.isfinite(value):
        return "NULL"
    return str(value)


def _to_float(value: Any) -> Optional[float]:
    if value is None:
        return None
    try:
        num = float(value)
    except (TypeError, ValueError):
        return None
    if not math.isfinite(num):
        return None
    return num


def ensure_table() -> None:
    sql = """
    CREATE TABLE IF NOT EXISTS deribit_atm_greeks_ticks (
        id BIGSERIAL PRIMARY KEY,
        currency TEXT NOT NULL,
        instrument_name TEXT NOT NULL,
        source_ts TIMESTAMPTZ NOT NULL,
        fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
        mark_iv NUMERIC,
        bid_iv NUMERIC,
        ask_iv NUMERIC,
        delta NUMERIC,
        gamma NUMERIC,
        vega NUMERIC,
        theta NUMERIC,
        rho NUMERIC,
        mark_price NUMERIC,
        underlying_price NUMERIC,
        index_price NUMERIC,
        best_bid_price NUMERIC,
        best_ask_price NUMERIC,
        open_interest NUMERIC,
        raw JSONB NOT NULL DEFAULT '{}'::jsonb,
        UNIQUE (currency, instrument_name, source_ts)
    );

    CREATE INDEX IF NOT EXISTS idx_deribit_atm_greeks_ccy_ts
      ON deribit_atm_greeks_ticks(currency, source_ts DESC);
    CREATE INDEX IF NOT EXISTS idx_deribit_atm_greeks_fetched
      ON deribit_atm_greeks_ticks(fetched_at DESC);
    """
    _run_psql(sql)


def pick_atm_instruments() -> List[Tuple[str, str]]:
    if not CURRENCIES:
        return []

    currency_values = ",".join(f"({_sql_text(currency)})" for currency in CURRENCIES)

    # deribit_iv_ticks stores Deribit option metadata in instrument_name rather
    # than separate option_type/strike columns. Bound the lateral scan to fresh
    # rows so it stays cheap on the partitioned IV table.
    sql = f"""
    WITH candidates AS (
      SELECT c.currency,
             t.instrument_name,
             split_part(t.instrument_name, '-', 4) AS option_type,
             NULLIF(split_part(t.instrument_name, '-', 3), '')::numeric AS strike,
             t.underlying_price,
             t.creation_ts,
             abs(NULLIF(split_part(t.instrument_name, '-', 3), '')::numeric - t.underlying_price) AS atm_distance
      FROM (VALUES {currency_values}) AS c(currency)
      CROSS JOIN LATERAL (
        SELECT instrument_name, underlying_price, creation_ts, fetched_at
        FROM deribit_iv_ticks
        WHERE upper(currency) = c.currency
          AND fetched_at >= NOW() - INTERVAL '10 minutes'
          AND creation_ts IS NOT NULL
          AND underlying_price IS NOT NULL
          AND instrument_name ~ '^[^-]+-[0-9]{{1,2}}[A-Z]{{3}}[0-9]{{2}}-[0-9]+(\\.[0-9]+)?-[CP]$'
        ORDER BY fetched_at DESC, creation_ts DESC
        LIMIT 1000
      ) t
    ), ranked AS (
      SELECT
        currency,
        instrument_name,
        row_number() OVER (
          PARTITION BY currency
          ORDER BY
            atm_distance ASC,
            CASE WHEN option_type = 'C' THEN 0 WHEN option_type = 'P' THEN 1 ELSE 2 END,
            creation_ts DESC,
            instrument_name ASC
        ) AS rn
      FROM candidates
    )
    SELECT currency, instrument_name
    FROM ranked
    WHERE rn = 1
    ORDER BY currency;
    """
    out = _run_psql(sql, at_mode=True).strip()
    if not out:
        return []

    pairs: List[Tuple[str, str]] = []
    for line in out.splitlines():
        parts = line.split("\t")
        if len(parts) != 2:
            continue
        currency, instrument = parts[0].strip(), parts[1].strip()
        if currency and instrument:
            pairs.append((currency, instrument))
    return pairs


def fetch_order_book(instrument_name: str) -> Dict[str, Any]:
    url = f"{API_BASE}/get_order_book"
    resp = requests.get(
        url,
        params={"instrument_name": instrument_name},
        timeout=HTTP_TIMEOUT_SECS,
    )
    resp.raise_for_status()
    payload = resp.json()
    result = payload.get("result")
    if not isinstance(result, dict):
        raise RuntimeError("invalid Deribit response: missing result")
    return result


def upsert_greeks(currency: str, instrument_name: str, result: Dict[str, Any]) -> None:
    ts_ms = result.get("timestamp")
    if ts_ms is None:
        source_ts = dt.datetime.now(dt.timezone.utc)
    else:
        source_ts = dt.datetime.fromtimestamp(int(ts_ms) / 1000.0, tz=dt.timezone.utc)

    greeks = result.get("greeks") or {}

    mark_iv = _to_float(result.get("mark_iv"))
    bid_iv = _to_float(result.get("bid_iv"))
    ask_iv = _to_float(result.get("ask_iv"))

    delta = _to_float(greeks.get("delta"))
    gamma = _to_float(greeks.get("gamma"))
    vega = _to_float(greeks.get("vega"))
    theta = _to_float(greeks.get("theta"))
    rho = _to_float(greeks.get("rho"))

    mark_price = _to_float(result.get("mark_price"))
    underlying_price = _to_float(result.get("underlying_price"))
    index_price = _to_float(result.get("index_price"))
    best_bid_price = _to_float(result.get("best_bid_price"))
    best_ask_price = _to_float(result.get("best_ask_price"))
    open_interest = _to_float(result.get("open_interest"))

    raw_json = json.dumps(result, ensure_ascii=True, separators=(",", ":"))

    sql = f"""
    INSERT INTO deribit_atm_greeks_ticks (
      currency, instrument_name, source_ts, fetched_at,
      mark_iv, bid_iv, ask_iv,
      delta, gamma, vega, theta, rho,
      mark_price, underlying_price, index_price,
      best_bid_price, best_ask_price, open_interest,
      raw
    ) VALUES (
      {_sql_text(currency)}, {_sql_text(instrument_name)}, {_sql_text(source_ts.isoformat())}, NOW(),
      {_sql_num(mark_iv)}, {_sql_num(bid_iv)}, {_sql_num(ask_iv)},
      {_sql_num(delta)}, {_sql_num(gamma)}, {_sql_num(vega)}, {_sql_num(theta)}, {_sql_num(rho)},
      {_sql_num(mark_price)}, {_sql_num(underlying_price)}, {_sql_num(index_price)},
      {_sql_num(best_bid_price)}, {_sql_num(best_ask_price)}, {_sql_num(open_interest)},
      {_sql_text(raw_json)}::jsonb
    )
    ON CONFLICT (currency, instrument_name, source_ts) DO UPDATE
    SET
      fetched_at = NOW(),
      mark_iv = EXCLUDED.mark_iv,
      bid_iv = EXCLUDED.bid_iv,
      ask_iv = EXCLUDED.ask_iv,
      delta = EXCLUDED.delta,
      gamma = EXCLUDED.gamma,
      vega = EXCLUDED.vega,
      theta = EXCLUDED.theta,
      rho = EXCLUDED.rho,
      mark_price = EXCLUDED.mark_price,
      underlying_price = EXCLUDED.underlying_price,
      index_price = EXCLUDED.index_price,
      best_bid_price = EXCLUDED.best_bid_price,
      best_ask_price = EXCLUDED.best_ask_price,
      open_interest = EXCLUDED.open_interest,
      raw = EXCLUDED.raw;
    """
    _run_psql(sql)


def run_once() -> Tuple[int, int]:
    if not RUNNING:
        return 0, 0

    pairs = pick_atm_instruments()
    ok = 0
    total = 0

    for currency, instrument_name in pairs:
        if not RUNNING:
            break

        total += 1
        try:
            result = fetch_order_book(instrument_name)
            upsert_greeks(currency, instrument_name, result)
            ok += 1
        except GracefulShutdown:
            raise
        except Exception as exc:
            print(
                f"[deribit-greeks] currency={currency} instrument={instrument_name} error={exc}",
                flush=True,
            )

    return ok, total


def main() -> int:
    try:
        ensure_table()
    except GracefulShutdown:
        print("[deribit-greeks] stopped", flush=True)
        return 0

    print(
        f"[deribit-greeks] started currencies={','.join(CURRENCIES)} poll_secs={POLL_SECS}",
        flush=True,
    )

    while RUNNING:
        started = time.time()
        try:
            ok, total = run_once()
        except GracefulShutdown:
            break

        elapsed = time.time() - started
        print(
            f"[deribit-greeks] cycle ok={ok}/{total} elapsed_s={elapsed:.2f}",
            flush=True,
        )

        sleep_left = POLL_SECS - elapsed
        sleep_until = time.time() + max(0.0, sleep_left)
        while RUNNING and time.time() < sleep_until:
            time.sleep(min(1.0, sleep_until - time.time()))

    print("[deribit-greeks] stopped", flush=True)
    return 0


if __name__ == "__main__":
    signal.signal(signal.SIGINT, _on_signal)
    signal.signal(signal.SIGTERM, _on_signal)
    raise SystemExit(main())
