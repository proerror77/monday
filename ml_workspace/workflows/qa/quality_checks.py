#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
统一数据质量校验（QA）：覆盖/格式/缺口概览

检查内容（按表）：
- 覆盖率：窗口内去重秒数 / 总秒数、行数、最小/最大时间戳
- 必要列：是否包含关键字段（BBO/Depth/Trades 各自不同）

输出：结构化 dict，可被 CLI 捕获或直接打印 JSON。
"""

from __future__ import annotations

import json
import os
import sys
from dataclasses import dataclass
from datetime import datetime, timezone
from typing import Dict, List, Optional, Tuple
import json

import pandas as pd

from utils.clickhouse_client import ClickHouseClient
from utils.time import parse_iso8601, to_ms


@dataclass
class TableSpec:
    name: str
    required_cols: List[str]


DEFAULT_SPECS: List[TableSpec] = [
    TableSpec(name="bitget_futures_bbo", required_cols=["exchange_ts", "symbol", "best_bid", "best_ask", "best_bid_size", "best_ask_size"]),
    TableSpec(name="bitget_futures_depth", required_cols=["exchange_ts", "symbol", "bids", "asks"]),
    TableSpec(name="bitget_futures_trades", required_cols=["exchange_ts", "symbol", "side", "size", "price"]),
]


# Compatibility profiles: expected schemas for training/ETL consumers
# 'spot' profile matches commonly used spot_* tables used in older loaders.
COMPAT_PROFILES: Dict[str, Dict[str, List[str]]] = {
    "spot": {
        "spot_ticker": ["ts", "instId", "bestBid", "bestAsk", "last", "vol24h"],
        "spot_books15": ["ts", "instId", "side", "price", "qty", "orderCnt"],
        "spot_trades": ["ts", "instId", "tradeId", "side", "price", "qty"],
    },
}


def check_table_schema(client: ClickHouseClient, table: str, required_cols: List[str]) -> Dict:
    try:
        df = client.describe_table(table)
    except Exception:
        df = pd.DataFrame()
    cols = set([str(c) for c in (df["name"].tolist() if not df.empty and "name" in df.columns else [])])
    missing = [c for c in required_cols if c not in cols]
    return {"table": table, "ok": len(missing) == 0, "missing": missing, "columns": sorted(list(cols))}


def _detect_timestamp_field(client: ClickHouseClient, table: str) -> str:
    """Detect whether table uses 'ts' or 'exchange_ts' as timestamp field."""
    try:
        df = client.describe_table(table)
        if df is not None and not df.empty:
            cols = set(df["name"].tolist())
            # Prefer 'ts' (DateTime) over 'exchange_ts' (UInt64)
            if "ts" in cols:
                return "ts"
            elif "exchange_ts" in cols:
                return "exchange_ts"
    except Exception:
        pass
    # Default fallback
    return "exchange_ts"


def check_table_coverage(client: ClickHouseClient, table: str, symbol: str, start_ms: int, end_ms: int) -> Dict:
    ts_field = _detect_timestamp_field(client, table)

    # Convert milliseconds to appropriate filter based on field type
    if ts_field == "ts":
        # DateTime field: convert ms to seconds for toUnixTimestamp
        start_sec = start_ms // 1000
        end_sec = end_ms // 1000
        q = f"""
        SELECT
            toInt64(toUnixTimestamp(min({ts_field})) * 1000) AS min_ts,
            toInt64(toUnixTimestamp(max({ts_field})) * 1000) AS max_ts,
            toInt64(count()) AS rows,
            toInt64(countDistinct(toUnixTimestamp({ts_field}))) AS secs_covered
        FROM {table}
        WHERE symbol = '{symbol}'
          AND toUnixTimestamp({ts_field}) BETWEEN {start_sec} AND {end_sec}
        """
    else:
        # UInt64 field in milliseconds
        q = f"""
        SELECT
            toInt64(min({ts_field})) AS min_ts,
            toInt64(max({ts_field})) AS max_ts,
            toInt64(count()) AS rows,
            toInt64(countDistinct(toInt64({ts_field}/1000))) AS secs_covered
        FROM {table}
        WHERE symbol = '{symbol}' AND {ts_field} BETWEEN {start_ms} AND {end_ms}
        """

    df = client.query_to_dataframe(q)
    total_secs = max(0, (end_ms // 1000) - (start_ms // 1000) + 1)
    if df is None or df.empty:
        return {
            "table": table,
            "rows": 0,
            "secs_covered": 0,
            "coverage_pct": 0.0,
            "min_ts": None,
            "max_ts": None,
            "total_secs": total_secs,
        }
    r = df.iloc[0]
    secs_cov = int(r.get("secs_covered", 0))
    rows = int(r.get("rows", 0))
    cov = float(secs_cov / total_secs * 100.0) if total_secs > 0 else 0.0
    return {
        "table": table,
        "rows": rows,
        "secs_covered": secs_cov,
        "coverage_pct": cov,
        "min_ts": int(r.get("min_ts")) if pd.notna(r.get("min_ts")) else None,
        "max_ts": int(r.get("max_ts")) if pd.notna(r.get("max_ts")) else None,
        "total_secs": total_secs,
    }


def list_tables(client: ClickHouseClient) -> List[str]:
    try:
        return client.show_tables()
    except Exception:
        return []


def compute_overlap_window(client: ClickHouseClient, tables: List[str], symbol: str) -> Dict:
    """Compute the overlapping time window across given tables for a symbol.

    Returns dict with per-table min/max and the intersection window.
    """
    bounds: Dict[str, Dict[str, Optional[int]]] = {}
    for t in tables:
        ts_field = _detect_timestamp_field(client, t)

        if ts_field == "ts":
            q = f"""
            SELECT toInt64(toUnixTimestamp(min({ts_field})) * 1000) AS min_ts,
                   toInt64(toUnixTimestamp(max({ts_field})) * 1000) AS max_ts
            FROM {t}
            WHERE symbol = '{symbol}'
            """
        else:
            q = f"""
            SELECT toInt64(min({ts_field})) AS min_ts, toInt64(max({ts_field})) AS max_ts
            FROM {t}
            WHERE symbol = '{symbol}'
            """

        df = client.query_to_dataframe(q)
        if df is None or df.empty:
            bounds[t] = {"min_ts": None, "max_ts": None}
        else:
            r = df.iloc[0]
            bounds[t] = {
                "min_ts": int(r.get("min_ts")) if pd.notna(r.get("min_ts")) else None,
                "max_ts": int(r.get("max_ts")) if pd.notna(r.get("max_ts")) else None,
            }

    # intersection
    mins = [b["min_ts"] for b in bounds.values() if b.get("min_ts") is not None]
    maxs = [b["max_ts"] for b in bounds.values() if b.get("max_ts") is not None]
    if not mins or not maxs:
        overlap = {"start_ts": None, "end_ts": None, "ok": False}
    else:
        start_ts = max(mins)
        end_ts = min(maxs)
        overlap = {"start_ts": start_ts, "end_ts": end_ts, "ok": bool(start_ts < end_ts)}
    return {"per_table": bounds, "overlap": overlap}


def check_profile_compatibility(client: ClickHouseClient, profile: str) -> Dict:
    """Validate that tables in a compatibility profile contain expected columns.

    Returns a dict with per-table results and overall ok.
    """
    expected_map = COMPAT_PROFILES.get(profile)
    if not expected_map:
        return {"ok": False, "error": f"unknown profile: {profile}", "tables": {}}

    results: Dict[str, Dict] = {}
    overall_ok = True
    for table, expected_cols in expected_map.items():
        try:
            df = client.describe_table(table)
            actual = set(df["name"].tolist()) if df is not None and not df.empty and "name" in df.columns else set()
        except Exception:
            actual = set()
        exp = set(expected_cols)
        missing = sorted(list(exp - actual))
        extra = sorted(list(actual - exp))
        ok = len(missing) == 0
        results[table] = {"expected": sorted(list(exp)), "actual": sorted(list(actual)), "missing": missing, "extra": extra, "ok": ok}
        overall_ok = overall_ok and ok
    return {"ok": overall_ok, "tables": results, "profile": profile}


def check_custom_compatibility(client: ClickHouseClient, mapping: Dict[str, List[str]]) -> Dict:
    """Validate that tables in custom mapping contain expected columns.
    mapping: { table_name: [col1, col2, ...] }
    """
    results: Dict[str, Dict] = {}
    overall_ok = True
    for table, expected_cols in mapping.items():
        try:
            df = client.describe_table(table)
            actual = set(df["name"].tolist()) if df is not None and not df.empty and "name" in df.columns else set()
        except Exception:
            actual = set()
        exp = set(expected_cols)
        missing = sorted(list(exp - actual))
        extra = sorted(list(actual - exp))
        ok = len(missing) == 0
        results[table] = {"expected": sorted(list(exp)), "actual": sorted(list(actual)), "missing": missing, "extra": extra, "ok": ok}
        overall_ok = overall_ok and ok
    return {"ok": overall_ok, "tables": results, "profile": "custom"}


def schema_dump(client: ClickHouseClient, tables: List[str]) -> Dict[str, List[Dict[str, str]]]:
    out: Dict[str, List[Dict[str, str]]] = {}
    for t in tables:
        try:
            df = client.describe_table(t)
        except Exception:
            df = None
        if df is not None and not df.empty:
            cols = []
            names = df.get("name")
            types = df.get("type")
            if names is not None and types is not None:
                for n, ty in zip(names.tolist(), types.tolist()):
                    cols.append({"name": str(n), "type": str(ty)})
            out[t] = cols
        else:
            out[t] = []
    return out


def find_tables_with_symbol(client: ClickHouseClient, symbol: str, tables: Optional[List[str]] = None) -> List[str]:
    """Return tables that contain rows matching symbol LIKE %symbol%.

    If tables is None, scan all tables (may be slow).
    """
    if tables is None:
        tables = list_tables(client)
    matched: List[str] = []
    for t in tables:
        # Try symbol field only - instId is OKX specific
        sample_query = f"""
        SELECT count() cnt FROM {t}
        WHERE toString(symbol) LIKE '%{symbol}%'
        LIMIT 1
        """
        try:
            res = client.execute_query(sample_query)
            if res:
                try:
                    v = int(str(res).strip().splitlines()[0])
                    if v > 0:
                        matched.append(t)
                except Exception:
                    pass
        except Exception:
            continue
    return matched


def load_tests(client: ClickHouseClient, symbol: str, hours_back: int) -> Dict:
    out: Dict[str, Dict] = {}
    try:
        bbo = client.get_l1_bbo_data(symbol=symbol, hours_back=hours_back)
        out["bbo"] = {
            "rows": int(len(bbo)) if bbo is not None else 0,
            "columns": list(bbo.columns) if bbo is not None and not bbo.empty else [],
        }
    except Exception as e:
        out["bbo"] = {"error": str(e)}
    try:
        depth = client.get_l2_orderbook_data(symbol=symbol, hours_back=hours_back)
        out["depth"] = {
            "rows": int(len(depth)) if depth is not None else 0,
            "columns": list(depth.columns) if depth is not None and not depth.empty else [],
        }
    except Exception as e:
        out["depth"] = {"error": str(e)}
    try:
        trades = client.get_trades_data(symbol=symbol, hours_back=hours_back)
        out["trades"] = {
            "rows": int(len(trades)) if trades is not None else 0,
            "columns": list(trades.columns) if trades is not None and not trades.empty else [],
        }
    except Exception as e:
        out["trades"] = {"error": str(e)}
    return out


def compare_range(client: ClickHouseClient, raw_table: str, feat_table: str, symbol: str) -> Dict:
    def _build_query(table: str) -> str:
        ts_field = _detect_timestamp_field(client, table)
        if ts_field == "ts":
            return f"""
            SELECT toInt64(toUnixTimestamp(min({ts_field})) * 1000) AS min_ts,
                   toInt64(toUnixTimestamp(max({ts_field})) * 1000) AS max_ts,
                   count() AS total_records,
                   toInt64(toUnixTimestamp(max({ts_field})) - toUnixTimestamp(min({ts_field}))) * 1000 AS duration_ms
            FROM {table}
            WHERE symbol = '{symbol}'
            """
        else:
            return f"""
            SELECT min({ts_field}) AS min_ts, max({ts_field}) AS max_ts, count() AS total_records,
                   (max({ts_field}) - min({ts_field})) AS duration_ms
            FROM {table}
            WHERE symbol = '{symbol}'
            """

    raw = client.query_to_dataframe(_build_query(raw_table))
    feat = client.query_to_dataframe(_build_query(feat_table))
    def _pack(df: Optional['pd.DataFrame']) -> Dict:
        if df is None or df.empty:
            return {"min_ts": None, "max_ts": None, "duration_hours": None, "total_records": 0}
        r = df.iloc[0]
        min_ts = int(r.get("min_ts")) if r.get("min_ts") is not None else None
        max_ts = int(r.get("max_ts")) if r.get("max_ts") is not None else None
        dur_ms = int(r.get("duration_ms")) if r.get("duration_ms") is not None else None
        total = int(r.get("total_records", 0))
        return {
            "min_ts": min_ts,
            "max_ts": max_ts,
            "duration_hours": (dur_ms / (1000 * 3600)) if dur_ms is not None else None,
            "total_records": total,
        }
    raw_p = _pack(raw)
    feat_p = _pack(feat)
    loss_hours = None
    if raw_p.get("duration_hours") is not None and feat_p.get("duration_hours") is not None:
        loss_hours = float(raw_p["duration_hours"]) - float(feat_p["duration_hours"])
    return {"raw": raw_p, "feature": feat_p, "loss_hours": loss_hours, "symbol": symbol, "tables": {"raw": raw_table, "feature": feat_table}}


def run_checks(
    host: Optional[str],
    user: Optional[str],
    password: Optional[str],
    database: Optional[str],
    symbol: str,
    start_iso: str,
    end_iso: str,
    tables: Optional[List[str]] = None,
    top_gaps: int = 0,
    show_tables: bool = False,
    calc_overlap: bool = False,
    compat_profile: Optional[str] = None,
    compat_profile_json: Optional[str] = None,
    schema_dump_tables: Optional[List[str]] = None,
    search_symbol: Optional[str] = None,
    search_tables: Optional[List[str]] = None,
    load_tests_hours: Optional[int] = None,
    range_compare: Optional[Tuple[str, str, str]] = None,
) -> Dict:
    client = ClickHouseClient(host=host, username=user, password=password, database=database)
    if not client.test_connection():
        raise RuntimeError("无法连接 ClickHouse，请检查 host/凭证")

    start_ms = to_ms(parse_iso8601(start_iso))
    end_ms = to_ms(parse_iso8601(end_iso))

    specs: List[TableSpec]
    if tables:
        # Map table -> default required cols if known, else minimal set
        # Use flexible timestamp field detection instead of hardcoding
        default_map = {s.name: s.required_cols for s in DEFAULT_SPECS}
        specs = []
        for t in tables:
            if t in default_map:
                specs.append(TableSpec(name=t, required_cols=default_map[t]))
            else:
                # For unknown tables, just check for 'symbol' - timestamp is auto-detected
                specs.append(TableSpec(name=t, required_cols=["symbol"]))
    else:
        specs = DEFAULT_SPECS

    results: Dict[str, Dict] = {}
    overall_ok = True
    tbl_list: List[str] = []
    if show_tables:
        tbl_list = list_tables(client)

    overlap_info: Dict = {}
    if calc_overlap:
        overlap_info = compute_overlap_window(client, [s.name for s in specs], symbol)

    compat_info: Dict = {}
    if compat_profile_json:
        try:
            with open(compat_profile_json, "r", encoding="utf-8") as f:
                mapping = json.load(f)
            compat_info = check_custom_compatibility(client, mapping)
        except Exception as e:
            compat_info = {"ok": False, "error": f"failed to load custom profile: {e}"}
    elif compat_profile:
        compat_info = check_profile_compatibility(client, compat_profile)

    schema_info: Dict = {}
    if schema_dump_tables:
        schema_info = schema_dump(client, schema_dump_tables)

    search_info: List[str] = []
    if search_symbol:
        search_info = find_tables_with_symbol(client, search_symbol, search_tables)

    load_info: Dict = {}
    if load_tests_hours and int(load_tests_hours) > 0:
        load_info = load_tests(client, symbol=symbol, hours_back=int(load_tests_hours))

    range_info: Dict = {}
    if range_compare:
        raw_tbl, feat_tbl, sym = range_compare
        range_info = compare_range(client, raw_tbl, feat_tbl, sym)

    for spec in specs:
        cov = check_table_coverage(client, spec.name, symbol, start_ms, end_ms)
        sch = check_table_schema(client, spec.name, spec.required_cols)
        gaps = []
        if top_gaps > 0 and cov["rows"] > 0:
            # 尝试取出存在数据的秒序列，计算缺口（仅在窗口不大时建议启用）
            ts_field = _detect_timestamp_field(client, spec.name)
            start_sec = start_ms // 1000
            end_sec = end_ms // 1000

            if ts_field == "ts":
                qsecs = f"""
                SELECT DISTINCT toInt64(toUnixTimestamp({ts_field})) AS sec
                FROM {spec.name}
                WHERE symbol = '{symbol}'
                  AND toUnixTimestamp({ts_field}) BETWEEN {start_sec} AND {end_sec}
                ORDER BY sec
                """
            else:
                qsecs = f"""
                SELECT DISTINCT toInt64({ts_field}/1000) AS sec
                FROM {spec.name}
                WHERE symbol = '{symbol}' AND {ts_field} BETWEEN {start_ms} AND {end_ms}
                ORDER BY sec
                """

            df_secs = client.query_to_dataframe(qsecs)
            if df_secs is not None and not df_secs.empty:
                secs = df_secs["sec"].astype(int).tolist()
                gaps_local = []
                # find gaps between observed secs within [start, end]
                prev = start_ms // 1000 - 1
                end_sec = end_ms // 1000
                for s in secs + [end_sec + 1]:
                    if s > prev + 1:
                        gaps_local.append({"start": prev + 1, "end": s - 1, "length": (s - 1) - (prev + 1) + 1})
                    prev = s
                # sort by length desc and take top
                gaps = sorted(gaps_local, key=lambda g: g["length"], reverse=True)[:top_gaps]
        ok = (cov["rows"] > 0) and (cov["coverage_pct"] > 0.0) and sch["ok"]
        results[spec.name] = {"coverage": cov, "schema": sch, "gaps": gaps, "ok": ok}
        overall_ok = overall_ok and ok

    summary = {
        "symbol": symbol,
        "window": {"start": start_iso, "end": end_iso},
        "tables": tbl_list if show_tables else None,
        "overlap": overlap_info if calc_overlap else None,
        "compat": compat_info if (compat_profile or compat_profile_json) else None,
        "schema_dump": schema_info or None,
        "search_symbol_tables": search_info or None,
        "load_tests": load_info or None,
        "range_compare": range_info or None,
        "results": results,
        "ok": overall_ok,
    }
    return summary


def main() -> int:
    import argparse

    ap = argparse.ArgumentParser(description="数据质量校验（覆盖/格式/缺口概览）")
    ap.add_argument("--host", default=os.getenv("CLICKHOUSE_HOST"))
    ap.add_argument("--user", default=os.getenv("CLICKHOUSE_USERNAME", "default"))
    ap.add_argument("--password", default=os.getenv("CLICKHOUSE_PASSWORD", ""))
    ap.add_argument("--database", default=os.getenv("CLICKHOUSE_DATABASE", "hft"))
    ap.add_argument("--symbol", default="WLFIUSDT")
    ap.add_argument("--start", required=True, help="ISO 开始时间，如 2025-09-03T00:00:00Z")
    ap.add_argument("--end", required=True, help="ISO 结束时间，如 2025-09-05T23:59:59Z")
    ap.add_argument("--tables", default="bitget_futures_bbo,bitget_futures_depth,bitget_futures_trades")
    ap.add_argument("--top_gaps", type=int, default=0, help="显示按秒缺口最长的前N段（可能较慢）")
    args = ap.parse_args()

    tables = [t.strip() for t in str(args.tables).split(",") if t.strip()]
    res = run_checks(
        host=args.host,
        user=args.user,
        password=args.password,
        database=args.database,
        symbol=args.symbol,
        start_iso=args.start,
        end_iso=args.end,
        tables=tables,
        top_gaps=int(args.top_gaps or 0),
    )
    print(json.dumps(res, indent=2, ensure_ascii=False))
    return 0 if res.get("ok") else 1


if __name__ == "__main__":
    raise SystemExit(main())
