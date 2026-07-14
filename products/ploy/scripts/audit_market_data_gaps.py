#!/usr/bin/env python3
"""Audit live market-data coverage with bounded index probes.

This script is intended to run on the data/runtime host next to PostgreSQL. It
uses psql instead of a Python database dependency so it can be shipped with the
existing lightweight deployment bundle.
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import re
import subprocess
import sys
import time
from dataclasses import dataclass
from datetime import datetime, timedelta, timezone
from pathlib import Path
from typing import Any, Iterable, Optional

try:
    from scripts.psql_connection import psql_environment
except ModuleNotFoundError:
    from psql_connection import psql_environment

DB_URL = (
    os.environ.get("PLOY_DATABASE__URL")
    or os.environ.get("DATABASE_URL")
    or "postgresql://postgres:postgres@localhost:5432/ploy"
)

DEFAULT_SYMBOLS = "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,BNBUSDT"
STATUS_ORDER = {"ok": 0, "warn": 1, "unknown": 2, "critical": 3}
IDENT_RE = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")
ORDERBOOK_ARCHIVE_TZ = timezone(timedelta(hours=8))

SOURCE_PROFILES = {
    "all": ["*"],
    "pm5d-core": [
        "polymarket_quotes",
        "polymarket_quote_quality",
        "polymarket_orderbooks",
        "binance_price",
        "binance_agg_trades",
        "binance_lob",
    ],
    "pm5d-execution": [
        "polymarket_quotes",
        "polymarket_quote_quality",
        "polymarket_orderbooks",
        "binance_price",
        "binance_agg_trades",
        "binance_lob",
    ],
    "pm5d-vol": [
        "polymarket_quotes",
        "polymarket_quote_quality",
        "polymarket_orderbooks",
        "deribit_iv",
        "deribit_atm_greeks",
        "binance_price",
        "binance_agg_trades",
        "binance_lob",
    ],
    "research-windows": ["research_valid_windows"],
    "cex-extended": [
        "binance_futures",
        "okx_lob",
        "bybit_lob",
        "coinbase_lob",
        "kraken_lob",
    ],
}


@dataclass(frozen=True)
class GapTarget:
    source_id: str
    table_name: str
    timestamp_column: str
    stale_after_seconds: int
    max_gap_critical_minutes: int = 15
    max_gap_warn_minutes: int = 5
    ignore_max_gap: bool = False
    ignore_missing_buckets: bool = False
    filter_column: Optional[str] = None
    filter_value: Optional[str] = None


def ident(value: str) -> str:
    if not IDENT_RE.match(value):
        raise ValueError(f"unsafe SQL identifier: {value!r}")
    return value


def sql_literal(value: str) -> str:
    return "'" + value.replace("'", "''") + "'"


def run_sql(query: str, timeout: int) -> str:
    cmd = [
        "psql",
        DB_URL,
        "-q",
        "-t",
        "-A",
        "-v",
        "ON_ERROR_STOP=1",
        "-f",
        "-",
    ]
    # The executable and arguments are fixed; the validated URL is projected only
    # into libpq PG* variables and the SQL travels over stdin.
    result = subprocess.run(
        cmd,  # nosemgrep: python.lang.security.audit.dangerous-subprocess-use-tainted-env-args.dangerous-subprocess-use-tainted-env-args
        input=query,
        env=psql_environment(DB_URL),
        capture_output=True,
        text=True,
        timeout=timeout,
    )
    if result.returncode != 0:
        raise RuntimeError(result.stderr.strip() or result.stdout.strip())
    return result.stdout.strip()


def run_json(query: str, timeout: int) -> dict[str, Any]:
    raw = run_sql(query, timeout)
    if not raw:
        raise RuntimeError("SQL returned no rows")
    return json.loads(raw)


def parse_utc_datetime(raw: str) -> datetime:
    parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def archive_hour_keys(start_ts: str, end_ts: str) -> list[tuple[str, str]]:
    start = parse_utc_datetime(start_ts)
    end = parse_utc_datetime(end_ts)
    current = start.replace(minute=0, second=0, microsecond=0)
    keys: list[tuple[str, str]] = []
    while current < end:
        local = current.astimezone(ORDERBOOK_ARCHIVE_TZ)
        keys.append((local.strftime("%Y-%m-%d"), local.strftime("%H")))
        current += timedelta(hours=1)
    return keys


def audit_orderbook_archive_coverage(
    *,
    archive_root: str,
    start_ts: str,
    end_ts: str,
    bucket_minutes: int,
) -> dict[str, Any]:
    root = Path(archive_root)
    base = root / "orderbook_snapshots"
    hour_keys = archive_hour_keys(start_ts, end_ts)
    manifests: list[dict[str, Any]] = []
    missing_hours: list[dict[str, str]] = []
    invalid_hours: list[dict[str, str]] = []
    row_count = 0
    full_fidelity = True
    for date, hour in hour_keys:
        hour_dir = base / f"date={date}" / f"hour={hour}"
        marker = hour_dir / "_SUCCESS"
        manifest_path = hour_dir / "manifest.json"
        parquet_path = hour_dir / "snapshots.parquet"
        if not marker.exists() or not manifest_path.exists() or not parquet_path.exists():
            missing_hours.append(
                {
                    "date": date,
                    "hour": hour,
                    "path": str(hour_dir),
                }
            )
            continue
        try:
            manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
        except (OSError, json.JSONDecodeError) as exc:
            invalid_hours.append(
                {
                    "date": date,
                    "hour": hour,
                    "path": str(manifest_path),
                    "reason": f"manifest_unreadable:{exc}",
                }
            )
            continue
        try:
            hour_rows = int(manifest.get("row_count") or 0)
        except (TypeError, ValueError):
            hour_rows = 0
        if hour_rows <= 0:
            invalid_hours.append(
                {
                    "date": date,
                    "hour": hour,
                    "path": str(manifest_path),
                    "reason": "row_count_empty",
                }
            )
            continue
        if manifest.get("full_fidelity") is not True:
            full_fidelity = False
            invalid_hours.append(
                {
                    "date": date,
                    "hour": hour,
                    "path": str(manifest_path),
                    "reason": "not_full_fidelity",
                }
            )
            continue
        row_count += hour_rows
        manifests.append(
            {
                "date": date,
                "hour": hour,
                "path": str(manifest_path),
                "row_count": hour_rows,
                "start_ts": manifest.get("start_ts", ""),
                "end_ts": manifest.get("end_ts", ""),
                "sha256": manifest.get("sha256", ""),
            }
        )

    start = parse_utc_datetime(start_ts)
    end = parse_utc_datetime(end_ts)
    expected_buckets = max(0, int((end - start).total_seconds() // (bucket_minutes * 60)))
    expected_hours = len(hour_keys)
    present_hours = len(manifests)
    status = "ok"
    reasons: list[str] = []
    if expected_hours == 0:
        status = "critical"
        reasons.append("archive audit window has no expected hours")
    if missing_hours:
        status = "critical"
        reasons.append(f"missing archive hours {len(missing_hours)}/{expected_hours}")
    if invalid_hours:
        status = "critical"
        reasons.append(f"invalid archive hours {len(invalid_hours)}/{expected_hours}")
    if not full_fidelity:
        status = "critical"
        reasons.append("archive contains non-full-fidelity manifests")
    if row_count <= 0:
        status = "critical"
        reasons.append("archive row_count is empty")
    if not reasons:
        reasons.append("archive full-depth orderbook coverage available")

    return {
        "schema_version": "orderbook_snapshot_archive_coverage.v1",
        "source_id": "polymarket_orderbooks",
        "surface": "clob_orderbook_snapshots",
        "source": "orderbook_snapshot_archive",
        "archive_root": str(root),
        "start_ts": start_ts,
        "end_ts": end_ts,
        "expected_hours": expected_hours,
        "present_hours": present_hours,
        "missing_hours": len(missing_hours),
        "invalid_hours": len(invalid_hours),
        "expected_buckets": expected_buckets,
        "present_buckets": expected_buckets if status == "ok" else 0,
        "coverage_pct": 100.0 if status == "ok" and expected_buckets > 0 else 0.0,
        "row_count": row_count,
        "full_fidelity": full_fidelity,
        "status": status,
        "reasons": reasons,
        "missing_hour_examples": missing_hours[:8],
        "invalid_hour_examples": invalid_hours[:8],
        "manifests": manifests[:24],
    }


def apply_orderbook_archive_coverage(
    row: dict[str, Any],
    target: GapTarget,
    *,
    start_ts: str | None,
    end_ts: str | None,
    bucket_minutes: int,
    orderbook_archive_root: str,
) -> dict[str, Any]:
    if (
        target.source_id != "polymarket_orderbooks"
        or not start_ts
        or not end_ts
        or not orderbook_archive_root
    ):
        return row
    archive = audit_orderbook_archive_coverage(
        archive_root=orderbook_archive_root,
        start_ts=start_ts,
        end_ts=end_ts,
        bucket_minutes=bucket_minutes,
    )
    row["archive_coverage"] = archive
    row["hot_table_expected_buckets"] = row.get("expected_buckets")
    row["hot_table_present_buckets"] = row.get("present_buckets")
    row["hot_table_missing_buckets"] = row.get("missing_buckets")
    row["hot_table_coverage_pct"] = row.get("coverage_pct")
    if archive.get("status") != "ok":
        return row
    row["coverage_source"] = archive["source"]
    row["expected_buckets"] = archive["expected_buckets"]
    row["present_buckets"] = archive["present_buckets"]
    row["missing_buckets"] = 0
    row["coverage_pct"] = archive["coverage_pct"]
    row["max_gap_buckets"] = 0
    row["max_gap_minutes"] = 0
    row["max_gap_start"] = None
    row["max_gap_end"] = None
    return row


def target_predicate(target: GapTarget, alias: str = "t") -> str:
    if target.filter_column is None:
        return "TRUE"
    return f"{alias}.{ident(target.filter_column)} = {sql_literal(target.filter_value or '')}"


def gap_query(
    target: GapTarget,
    *,
    lookback_hours: int,
    bucket_minutes: int,
    recent_minutes: int,
    statement_timeout_seconds: int,
    start_ts: Optional[str] = None,
    end_ts: Optional[str] = None,
) -> str:
    table = ident(target.table_name)
    ts_col = ident(target.timestamp_column)
    predicate = target_predicate(target)
    if start_ts and end_ts:
        start_expr = f"date_trunc('minute', {sql_literal(start_ts)}::timestamptz)"
        end_expr = (
            f"date_trunc('minute', {sql_literal(end_ts)}::timestamptz "
            f"- interval '{bucket_minutes} minutes')"
        )
    else:
        start_expr = f"date_trunc('minute', now() - interval '{lookback_hours} hours')"
        end_expr = f"date_trunc('minute', now() - interval '{bucket_minutes} minutes')"
    return f"""
SET statement_timeout TO '{statement_timeout_seconds}s';
WITH params AS (
  SELECT
    {start_expr} AS start_at,
    {end_expr} AS end_at,
    interval '{bucket_minutes} minutes' AS bucket_width
),
buckets AS (
  SELECT generate_series(start_at, end_at, bucket_width) AS bucket
  FROM params
),
coverage AS (
  SELECT b.bucket, hit.present IS NOT NULL AS present
  FROM buckets b
  CROSS JOIN params p
  LEFT JOIN LATERAL (
    SELECT TRUE AS present
    FROM {table} t
    WHERE {predicate}
      AND t.{ts_col} >= b.bucket
      AND t.{ts_col} < b.bucket + p.bucket_width
    ORDER BY t.{ts_col} DESC
    LIMIT 1
  ) hit ON TRUE
),
missing AS (
  SELECT
    bucket,
    bucket - (row_number() OVER (ORDER BY bucket))::int * (SELECT bucket_width FROM params) AS grp
  FROM coverage
  WHERE NOT present
),
runs AS (
  SELECT min(bucket) AS gap_start, max(bucket) AS gap_last_bucket, count(*)::int AS bucket_count
  FROM missing
  GROUP BY grp
),
top_gap AS (
  SELECT gap_start, gap_last_bucket, bucket_count
  FROM runs
  ORDER BY bucket_count DESC, gap_start DESC
  LIMIT 1
),
latest AS (
  SELECT t.{ts_col} AS latest_at
  FROM {table} t
  WHERE {predicate}
  ORDER BY t.{ts_col} DESC
  LIMIT 1
),
recent AS (
  SELECT count(*)::bigint AS rows_recent
  FROM {table} t
  WHERE {predicate}
    AND t.{ts_col} >= now() - interval '{recent_minutes} minutes'
)
SELECT json_build_object(
  'source_id', {sql_literal(target.source_id)},
  'table_name', {sql_literal(target.table_name)},
  'filter_column', {sql_literal(target.filter_column or '')},
  'filter_value', {sql_literal(target.filter_value or '')},
  'timestamp_column', {sql_literal(target.timestamp_column)},
  'lookback_hours', {lookback_hours},
  'bucket_minutes', {bucket_minutes},
  'recent_minutes', {recent_minutes},
  'start_at', (SELECT start_at FROM params),
  'end_at', (SELECT end_at FROM params),
  'latest_at', (SELECT latest_at FROM latest),
  'latest_lag_seconds', (
    SELECT CASE
      WHEN latest_at IS NULL THEN NULL
      ELSE GREATEST(0, floor(extract(epoch FROM now() - latest_at))::bigint)
    END
    FROM latest
  ),
  'rows_recent', (SELECT rows_recent FROM recent),
  'expected_buckets', (SELECT count(*)::int FROM coverage),
  'present_buckets', (SELECT count(*)::int FROM coverage WHERE present),
  'missing_buckets', (SELECT count(*)::int FROM coverage WHERE NOT present),
  'coverage_pct', (
    SELECT CASE
      WHEN count(*) = 0 THEN NULL
      ELSE round((100.0 * count(*) FILTER (WHERE present) / count(*))::numeric, 3)
    END
    FROM coverage
  ),
  'max_gap_buckets', coalesce((SELECT bucket_count FROM top_gap), 0),
  'max_gap_minutes', coalesce((SELECT bucket_count FROM top_gap), 0) * {bucket_minutes},
  'max_gap_start', (SELECT gap_start FROM top_gap),
  'max_gap_end', (SELECT gap_last_bucket + (SELECT bucket_width FROM params) FROM top_gap)
)::text
"""


def classify_gap(row: dict[str, Any], target: GapTarget) -> tuple[str, list[str]]:
    freshness_status, freshness_reasons = classify_freshness(row, target)
    coverage_status, coverage_reasons = classify_coverage(row, target)
    status = worst_status([freshness_status, coverage_status])
    reasons = []
    if freshness_status != "ok":
        reasons.extend(freshness_reasons)
    if coverage_status != "ok":
        reasons.extend(coverage_reasons)
    if not reasons:
        coverage_source = row.get("coverage_source")
        if coverage_source:
            reasons.append(f"coverage via {coverage_source}")
        else:
            reasons.append("coverage within thresholds")
    return status, reasons


def classify_freshness(row: dict[str, Any], target: GapTarget) -> tuple[str, list[str]]:
    reasons: list[str] = []
    status = "ok"

    latest_at = row.get("latest_at")
    latest_lag = row.get("latest_lag_seconds")
    if latest_at is None:
        return "critical", ["no latest row"]
    if latest_lag is not None and latest_lag > target.stale_after_seconds:
        status = "critical"
        reasons.append(f"latest lag {latest_lag}s > {target.stale_after_seconds}s")

    if not reasons:
        reasons.append("freshness within threshold")
    return status, reasons


def classify_coverage(row: dict[str, Any], target: GapTarget) -> tuple[str, list[str]]:
    reasons: list[str] = []
    status = "ok"

    expected_buckets = int(row.get("expected_buckets") or 0)
    present_buckets = int(row.get("present_buckets") or 0)
    if expected_buckets > 0 and present_buckets == 0:
        status = "critical"
        reasons.append(f"no covered buckets in audited window: 0/{expected_buckets}")

    if not target.ignore_max_gap:
        max_gap_minutes = int(row.get("max_gap_minutes") or 0)
        if max_gap_minutes >= target.max_gap_critical_minutes:
            status = "critical"
            reasons.append(
                f"max gap {max_gap_minutes}m >= {target.max_gap_critical_minutes}m"
            )
        elif max_gap_minutes >= target.max_gap_warn_minutes and STATUS_ORDER[status] < STATUS_ORDER[
            "warn"
        ]:
            status = "warn"
            reasons.append(f"max gap {max_gap_minutes}m >= {target.max_gap_warn_minutes}m")

    if not target.ignore_missing_buckets:
        missing_buckets = int(row.get("missing_buckets") or 0)
        if missing_buckets > 0 and STATUS_ORDER[status] < STATUS_ORDER["warn"]:
            status = "warn"
            reasons.append(f"missing buckets {missing_buckets}")

    if not reasons:
        reasons.append("coverage within thresholds")
    return status, reasons


def classify_gap_for_gate(
    row: dict[str, Any], target: GapTarget, gate_mode: str, historical_window: bool = False
) -> tuple[str, list[str], str, list[str], str, list[str]]:
    freshness_status, freshness_reasons = classify_freshness(row, target)
    coverage_status, coverage_reasons = classify_coverage(row, target)
    if historical_window:
        status = coverage_status
        reasons = list(coverage_reasons)
        if freshness_status != "ok":
            reasons.extend(
                f"freshness not enforced for historical window: {reason}"
                for reason in freshness_reasons
            )
    elif gate_mode == "freshness":
        status = freshness_status
        reasons = list(freshness_reasons)
        if coverage_status != "ok":
            reasons.extend(f"coverage not enforced: {reason}" for reason in coverage_reasons)
    else:
        status, reasons = classify_gap(row, target)
    return (
        status,
        reasons,
        freshness_status,
        freshness_reasons,
        coverage_status,
        coverage_reasons,
    )


def worst_status(statuses: Iterable[str]) -> str:
    status = "ok"
    for candidate in statuses:
        if STATUS_ORDER.get(candidate, STATUS_ORDER["unknown"]) > STATUS_ORDER[status]:
            status = candidate
    return status


def audit_gap_target(
    target: GapTarget,
    *,
    lookback_hours: int,
    bucket_minutes: int,
    recent_minutes: int,
    statement_timeout_seconds: int,
    psql_timeout_seconds: int,
    gate_mode: str,
    start_ts: Optional[str] = None,
    end_ts: Optional[str] = None,
    orderbook_archive_root: str = "",
) -> dict[str, Any]:
    started = time.monotonic()
    historical_window = bool(start_ts and end_ts)
    try:
        row = run_json(
            gap_query(
                target,
                lookback_hours=lookback_hours,
                bucket_minutes=bucket_minutes,
                recent_minutes=recent_minutes,
                statement_timeout_seconds=statement_timeout_seconds,
                start_ts=start_ts,
                end_ts=end_ts,
            ),
            psql_timeout_seconds,
        )
        row = apply_orderbook_archive_coverage(
            row,
            target,
            start_ts=start_ts,
            end_ts=end_ts,
            bucket_minutes=bucket_minutes,
            orderbook_archive_root=orderbook_archive_root,
        )
        (
            status,
            reasons,
            freshness_status,
            freshness_reasons,
            coverage_status,
            coverage_reasons,
        ) = classify_gap_for_gate(row, target, gate_mode, historical_window)
        row["status"] = status
        row["reasons"] = reasons
        row["freshness_status"] = freshness_status
        row["freshness_reasons"] = freshness_reasons
        row["coverage_status"] = coverage_status
        row["coverage_reasons"] = coverage_reasons
    except Exception as exc:  # noqa: BLE001 - this is an operator diagnostic.
        row = {
            "source_id": target.source_id,
            "table_name": target.table_name,
            "filter_column": target.filter_column or "",
            "filter_value": target.filter_value or "",
            "timestamp_column": target.timestamp_column,
            "lookback_hours": lookback_hours,
            "bucket_minutes": bucket_minutes,
            "status": "unknown",
            "reasons": [str(exc)],
            "freshness_status": "unknown",
            "freshness_reasons": [str(exc)],
            "coverage_status": "unknown",
            "coverage_reasons": [str(exc)],
        }
    if historical_window:
        row["audit_window_start_ts"] = start_ts
        row["audit_window_end_ts"] = end_ts
    row["query_ms"] = round((time.monotonic() - started) * 1000)
    return row


def research_windows_query(statement_timeout_seconds: int) -> str:
    return f"""
SET statement_timeout TO '{statement_timeout_seconds}s';
SELECT json_build_object(
  'source_id', 'research_valid_windows',
  'table_name', 'research_valid_windows',
  'timestamp_column', 'end_time',
  'total_rows', count(*)::bigint,
  'rows_7d', count(*) FILTER (WHERE end_time >= now() - interval '7 days')::bigint,
  'latest_end_time', max(end_time),
  'latest_lag_seconds', CASE
    WHEN max(end_time) IS NULL THEN NULL
    ELSE GREATEST(0, floor(extract(epoch FROM now() - max(end_time)))::bigint)
  END
)::text
FROM research_valid_windows
"""


def audit_research_windows(statement_timeout_seconds: int, psql_timeout_seconds: int) -> dict[str, Any]:
    started = time.monotonic()
    try:
        row = run_json(research_windows_query(statement_timeout_seconds), psql_timeout_seconds)
        reasons: list[str] = []
        status = "ok"
        if int(row.get("rows_7d") or 0) == 0:
            status = "critical"
            reasons.append("no valid windows in last 7 days")
        latest_lag = row.get("latest_lag_seconds")
        if latest_lag is None:
            status = "critical"
            reasons.append("no latest end_time")
        elif latest_lag > 12 * 3600:
            status = "critical"
            reasons.append(f"latest window lag {latest_lag}s > 12h")
        elif latest_lag > 6 * 3600 and STATUS_ORDER[status] < STATUS_ORDER["warn"]:
            status = "warn"
            reasons.append(f"latest window lag {latest_lag}s > 6h")
        if not reasons:
            reasons.append("valid windows are populated and recent")
        row["status"] = status
        row["reasons"] = reasons
    except Exception as exc:  # noqa: BLE001 - this is an operator diagnostic.
        row = {
            "source_id": "research_valid_windows",
            "table_name": "research_valid_windows",
            "status": "unknown",
            "reasons": [str(exc)],
        }
    row["query_ms"] = round((time.monotonic() - started) * 1000)
    return row


def pm_quote_quality_query(symbols: list[str], statement_timeout_seconds: int) -> str:
    symbol_list = ", ".join(sql_literal(symbol) for symbol in symbols)
    return f"""
SET statement_timeout TO '{statement_timeout_seconds}s';
WITH active AS (
  SELECT
    m.market_slug,
    m.symbol,
    m.start_time,
    m.end_time,
    token.token_id
  FROM pm_market_metadata m
  CROSS JOIN LATERAL jsonb_array_elements_text(
    (m.raw_market->'markets'->0->>'clobTokenIds')::jsonb
  ) AS token(token_id)
  WHERE m.symbol IN ({symbol_list})
    AND now() >= m.start_time - interval '1 minute'
    AND now() < m.end_time
),
latest AS (
  SELECT
    a.*,
    q.received_at,
    q.best_ask,
    q.ask_size,
    q.best_bid,
    q.bid_size,
    CASE
      WHEN q.received_at IS NULL THEN NULL
      ELSE GREATEST(0, floor(extract(epoch FROM now() - q.received_at))::bigint)
    END AS age_seconds
  FROM active a
  LEFT JOIN LATERAL (
    SELECT received_at, best_ask, ask_size, best_bid, bid_size
    FROM clob_quote_ticks q
    WHERE q.token_id = a.token_id
    ORDER BY q.received_at DESC
    LIMIT 1
  ) q ON TRUE
)
SELECT json_build_object(
  'source_id', 'polymarket_quote_quality',
  'table_name', 'clob_quote_ticks',
  'symbols', ARRAY[{symbol_list}],
  'max_quote_age_seconds', 15,
  'active_tokens', count(*)::int,
  'missing_quotes', count(*) FILTER (WHERE received_at IS NULL)::int,
  'older_than_15s', count(*) FILTER (
    WHERE received_at IS NOT NULL AND age_seconds > 15
  )::int,
  'missing_ask_or_size', count(*) FILTER (
    WHERE best_ask IS NULL OR ask_size IS NULL OR ask_size <= 0
  )::int,
  'missing_bid_or_size', count(*) FILTER (
    WHERE best_bid IS NULL OR bid_size IS NULL OR bid_size <= 0
  )::int,
  'max_age_seconds', max(age_seconds),
  'oldest_latest_quote', min(received_at),
  'newest_latest_quote', max(received_at),
  'missing_ask_or_size_examples', coalesce(
    (
      SELECT json_agg(
        json_build_object(
          'symbol', symbol,
          'market_slug', market_slug,
          'token_id', token_id,
          'end_time', end_time,
          'received_at', received_at,
          'best_ask', best_ask,
          'ask_size', ask_size
        )
        ORDER BY end_time, symbol, token_id
      )
      FROM (
        SELECT *
        FROM latest
        WHERE best_ask IS NULL OR ask_size IS NULL OR ask_size <= 0
        ORDER BY end_time, symbol, token_id
        LIMIT 8
      ) missing_examples
    ),
    '[]'::json
  )
)::text
FROM latest
"""


def classify_pm_quote_quality(row: dict[str, Any]) -> tuple[str, list[str]]:
    reasons: list[str] = []
    status = "ok"

    active_tokens = int(row.get("active_tokens") or 0)
    missing_quotes = int(row.get("missing_quotes") or 0)
    older_than_15s = int(row.get("older_than_15s") or 0)
    missing_ask_or_size = int(row.get("missing_ask_or_size") or 0)
    missing_bid_or_size = int(row.get("missing_bid_or_size") or 0)

    if active_tokens == 0:
        status = "critical"
        reasons.append("no active BTC/ETH PM token rows")
    if missing_quotes > 0:
        status = "critical"
        reasons.append(f"missing quotes for {missing_quotes}/{active_tokens} active tokens")
    if older_than_15s > 0:
        status = "critical"
        reasons.append(f"{older_than_15s}/{active_tokens} active token quotes older than 15s")
    if missing_ask_or_size > 0:
        if missing_ask_or_size == active_tokens:
            status = "critical"
        elif STATUS_ORDER[status] < STATUS_ORDER["warn"]:
            status = "warn"
        reasons.append(
            f"{missing_ask_or_size}/{active_tokens} active token quotes missing ask/ask_size"
        )
    if missing_bid_or_size > 0:
        if missing_bid_or_size == active_tokens:
            status = "critical"
        elif STATUS_ORDER[status] < STATUS_ORDER["warn"]:
            status = "warn"
        reasons.append(
            f"{missing_bid_or_size}/{active_tokens} active token quotes missing bid/bid_size"
        )

    if not reasons:
        max_age = row.get("max_age_seconds")
        reasons.append(f"active token quote quality within thresholds; max_age_seconds={max_age}")
    return status, reasons


def audit_pm_quote_quality(
    symbols: list[str],
    statement_timeout_seconds: int,
    psql_timeout_seconds: int,
) -> dict[str, Any]:
    started = time.monotonic()
    try:
        row = run_json(
            pm_quote_quality_query(symbols, statement_timeout_seconds),
            psql_timeout_seconds,
        )
        status, reasons = classify_pm_quote_quality(row)
        row["status"] = status
        row["reasons"] = reasons
    except Exception as exc:  # noqa: BLE001 - this is an operator diagnostic.
        row = {
            "source_id": "polymarket_quote_quality",
            "table_name": "clob_quote_ticks",
            "status": "unknown",
            "reasons": [str(exc)],
        }
    row["query_ms"] = round((time.monotonic() - started) * 1000)
    return row


def parse_symbols(raw: str) -> list[str]:
    return [item.strip().upper() for item in raw.split(",") if item.strip()]


def gap_targets(symbols: Iterable[str]) -> list[GapTarget]:
    targets = [
        GapTarget(
            "polymarket_quotes",
            "clob_quote_ticks",
            "received_at",
            900,
            ignore_max_gap=True,
            ignore_missing_buckets=True,
            max_gap_critical_minutes=0,
            max_gap_warn_minutes=0,
        ),
        GapTarget(
            "polymarket_orderbooks",
            "clob_orderbook_snapshots",
            "received_at",
            900,
            ignore_max_gap=True,
            ignore_missing_buckets=True,
            max_gap_critical_minutes=0,
            max_gap_warn_minutes=0,
        ),
        GapTarget("deribit_iv", "deribit_iv_ticks", "fetched_at", 900),
        GapTarget("deribit_atm_greeks", "deribit_atm_greeks_ticks", "fetched_at", 900),
        GapTarget(
            "binance_futures",
            "cex_public_market_ticks",
            "event_time",
            300,
            filter_column="source_key",
            filter_value="binance/derivatives_snapshot",
        ),
        GapTarget(
            "okx_lob",
            "cex_public_market_ticks",
            "event_time",
            300,
            filter_column="source_key",
            filter_value="okx/lob",
        ),
        GapTarget(
            "bybit_lob",
            "cex_public_market_ticks",
            "event_time",
            300,
            filter_column="source_key",
            filter_value="bybit/lob",
        ),
        GapTarget(
            "coinbase_lob",
            "cex_public_market_ticks",
            "event_time",
            300,
            filter_column="source_key",
            filter_value="coinbase/lob",
        ),
        GapTarget(
            "kraken_lob",
            "cex_public_market_ticks",
            "event_time",
            300,
            filter_column="source_key",
            filter_value="kraken/lob",
        ),
    ]
    for symbol in symbols:
        targets.extend(
            [
                GapTarget(
                    f"binance_price/{symbol}",
                    "binance_price_ticks",
                    "trade_time",
                    600,
                    filter_column="symbol",
                    filter_value=symbol,
                ),
                GapTarget(
                    f"binance_agg_trades/{symbol}",
                    "binance_agg_trade_ticks",
                    "trade_time",
                    600,
                    filter_column="symbol",
                    filter_value=symbol,
                ),
                GapTarget(
                    f"binance_lob/{symbol}",
                    "binance_lob_ticks",
                    "event_time",
                    900,
                    filter_column="symbol",
                    filter_value=symbol,
                ),
            ]
        )
    return targets


def parse_source_requirements(raw: str) -> list[str]:
    tokens = [item.strip() for item in raw.split(",") if item.strip()]
    if not tokens:
        return ["all"]
    out: list[str] = []
    for token in tokens:
        profile = SOURCE_PROFILES.get(token)
        if profile is None:
            out.append(token)
        else:
            out.extend(profile)
    return out


def parse_audit_timestamp(raw: str, *, label: str) -> str:
    value = raw.strip()
    if not value:
        raise ValueError(f"{label} must not be empty")
    normalized = value.replace("Z", "+00:00")
    parsed = datetime.fromisoformat(normalized)
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc).isoformat().replace("+00:00", "Z")


def source_aliases(target: GapTarget | dict[str, Any]) -> set[str]:
    if isinstance(target, GapTarget):
        source_id = target.source_id
        table_name = target.table_name
    else:
        source_id = str(target.get("source_id") or "")
        table_name = str(target.get("table_name") or "")
    aliases = {source_id, table_name}
    if "/" in source_id:
        aliases.add(source_id.split("/", 1)[0])
    return aliases


def source_is_required(target: GapTarget | dict[str, Any], requirements: list[str]) -> bool:
    aliases = source_aliases(target)
    for requirement in requirements:
        if requirement == "*":
            return True
        for alias in aliases:
            if alias == requirement or fnmatch.fnmatch(alias, requirement):
                return True
    return False


def overall_status(items: Iterable[dict[str, Any]]) -> str:
    status = "ok"
    for item in items:
        candidate = str(item.get("status") or "unknown")
        if STATUS_ORDER[candidate] > STATUS_ORDER[status]:
            status = candidate
    return status


def print_text(payload: dict[str, Any]) -> None:
    print("Market Data Gap Audit")
    print(f"generated_at={payload['generated_at']}")
    print(
        f"overall_status={payload['overall_status']} "
        f"lookback_hours={payload['lookback_hours']} "
        f"bucket_minutes={payload['bucket_minutes']}"
    )
    print(f"required_sources={','.join(payload.get('required_sources') or [])}")
    print()
    for item in payload["gap_audits"]:
        filter_value = item.get("filter_value")
        label = item["source_id"] if not filter_value else f"{item['source_id']}"
        latest = item.get("latest_at") or "none"
        lag = item.get("latest_lag_seconds")
        missing = item.get("missing_buckets", "n/a")
        expected = item.get("expected_buckets", "n/a")
        coverage = item.get("coverage_pct", "n/a")
        max_gap = item.get("max_gap_minutes", "n/a")
        print(
            f"[{item['status'].upper()}] {label} "
            f"latest={latest} lag_s={lag} "
            f"coverage={coverage}% missing={missing}/{expected} "
            f"max_gap_m={max_gap} query_ms={item.get('query_ms')}"
        )
        print(f"  reasons: {'; '.join(item.get('reasons') or [])}")
        if item.get("max_gap_start"):
            print(f"  max_gap: {item['max_gap_start']} -> {item['max_gap_end']}")

    print()
    for item in payload["window_audits"]:
        if item["source_id"] == "polymarket_quote_quality":
            print(
                f"[{item['status'].upper()}] {item['source_id']} "
                f"active_tokens={item.get('active_tokens')} "
                f"missing_quotes={item.get('missing_quotes')} "
                f"older_than_15s={item.get('older_than_15s')} "
                f"missing_ask_or_size={item.get('missing_ask_or_size')} "
                f"max_age_s={item.get('max_age_seconds')} query_ms={item.get('query_ms')}"
            )
        else:
            print(
                f"[{item['status'].upper()}] {item['source_id']} "
                f"rows_7d={item.get('rows_7d')} total_rows={item.get('total_rows')} "
                f"latest_end={item.get('latest_end_time')} "
                f"lag_s={item.get('latest_lag_seconds')} query_ms={item.get('query_ms')}"
            )
        print(f"  reasons: {'; '.join(item.get('reasons') or [])}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lookback-hours", type=int, default=168)
    parser.add_argument(
        "--start-ts",
        default="",
        help="optional explicit audit window start timestamp; makes coverage historical",
    )
    parser.add_argument(
        "--end-ts",
        default="",
        help="optional explicit audit window end timestamp; makes coverage historical",
    )
    parser.add_argument("--bucket-minutes", type=int, default=5)
    parser.add_argument("--recent-minutes", type=int, default=15)
    parser.add_argument("--statement-timeout-seconds", type=int, default=20)
    parser.add_argument("--psql-timeout-seconds", type=int, default=30)
    parser.add_argument(
        "--symbols",
        default=os.environ.get("PLOY_AUDIT_SYMBOLS", DEFAULT_SYMBOLS),
        help=f"comma-separated Binance symbols (default: {DEFAULT_SYMBOLS})",
    )
    parser.add_argument(
        "--required-sources",
        default=os.environ.get("PLOY_AUDIT_REQUIRED_SOURCES", "all"),
        help=(
            "comma-separated source ids, table names, wildcard patterns, or profiles "
            f"({', '.join(sorted(SOURCE_PROFILES))}); default: all"
        ),
    )
    parser.add_argument("--format", choices=("text", "json"), default="text")
    parser.add_argument(
        "--fail-on",
        choices=("critical", "warn", "unknown", "never"),
        default="critical",
        help="exit non-zero on critical, warn-or-higher, unknown-or-critical, or never",
    )
    parser.add_argument(
        "--gate-mode",
        choices=("coverage", "freshness"),
        default=os.environ.get("PLOY_AUDIT_GATE_MODE", "coverage"),
        help=(
            "coverage enforces historical bucket/max-gap checks plus freshness; "
            "freshness enforces only latest-row staleness while still reporting coverage"
        ),
    )
    parser.add_argument(
        "--orderbook-archive-root",
        default=os.environ.get("PLOY_ORDERBOOK_ARCHIVE_ROOT", "/opt/ploy/data/lake"),
        help=(
            "root that contains orderbook_snapshots/date=YYYY-MM-DD/hour=HH archives; "
            "used for explicit historical polymarket_orderbooks coverage"
        ),
    )
    args = parser.parse_args()

    if args.lookback_hours <= 0:
        parser.error("--lookback-hours must be positive")
    if args.bucket_minutes <= 0:
        parser.error("--bucket-minutes must be positive")
    if args.recent_minutes <= 0:
        parser.error("--recent-minutes must be positive")
    start_ts = args.start_ts.strip()
    end_ts = args.end_ts.strip()
    if bool(start_ts) != bool(end_ts):
        parser.error("--start-ts and --end-ts must be provided together")
    if start_ts and end_ts:
        try:
            start_ts = parse_audit_timestamp(start_ts, label="--start-ts")
            end_ts = parse_audit_timestamp(end_ts, label="--end-ts")
        except ValueError as exc:
            parser.error(str(exc))
        if datetime.fromisoformat(start_ts.replace("Z", "+00:00")) >= datetime.fromisoformat(
            end_ts.replace("Z", "+00:00")
        ):
            parser.error("--start-ts must be before --end-ts")

    symbols = parse_symbols(args.symbols)
    source_requirements = parse_source_requirements(args.required_sources)
    selected_gap_targets = [
        target for target in gap_targets(symbols) if source_is_required(target, source_requirements)
    ]
    gap_results = [
        audit_gap_target(
            target,
            lookback_hours=args.lookback_hours,
            bucket_minutes=args.bucket_minutes,
            recent_minutes=args.recent_minutes,
            statement_timeout_seconds=args.statement_timeout_seconds,
            psql_timeout_seconds=args.psql_timeout_seconds,
            gate_mode=args.gate_mode,
            start_ts=start_ts or None,
            end_ts=end_ts or None,
            orderbook_archive_root=args.orderbook_archive_root,
        )
        for target in selected_gap_targets
    ]
    window_results = []
    research_window_target = {
        "source_id": "research_valid_windows",
        "table_name": "research_valid_windows",
    }
    if source_is_required(research_window_target, source_requirements):
        window_results.append(
            audit_research_windows(args.statement_timeout_seconds, args.psql_timeout_seconds)
        )
    pm_quote_quality_target = {
        "source_id": "polymarket_quote_quality",
        "table_name": "clob_quote_ticks",
    }
    if source_is_required(pm_quote_quality_target, source_requirements):
        window_results.append(
            audit_pm_quote_quality(symbols, args.statement_timeout_seconds, args.psql_timeout_seconds)
        )
    items = gap_results + window_results
    if not items:
        parser.error("--required-sources selected no audit targets")
    payload = {
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "database_url_source": "PLOY_DATABASE__URL"
        if os.environ.get("PLOY_DATABASE__URL")
        else ("DATABASE_URL" if os.environ.get("DATABASE_URL") else "default-local"),
        "lookback_hours": args.lookback_hours,
        "audit_window_start_ts": start_ts or None,
        "audit_window_end_ts": end_ts or None,
        "bucket_minutes": args.bucket_minutes,
        "recent_minutes": args.recent_minutes,
        "gate_mode": args.gate_mode,
        "orderbook_archive_root": args.orderbook_archive_root,
        "symbols": symbols,
        "required_sources": source_requirements,
        "overall_status": overall_status(items),
        "gap_audits": gap_results,
        "window_audits": window_results,
    }

    if args.format == "json":
        print(json.dumps(payload, separators=(",", ":"), default=str))
    else:
        print_text(payload)

    if args.fail_on == "never":
        return 0
    if args.fail_on == "warn" and payload["overall_status"] in {"warn", "unknown", "critical"}:
        return 1
    if payload["overall_status"] == "critical":
        return 1
    if args.fail_on == "unknown" and payload["overall_status"] in {"critical", "unknown"}:
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
