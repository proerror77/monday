#!/usr/bin/env python3
import argparse
import sys
from typing import List, Tuple, Optional

try:
    import clickhouse_connect
except Exception as e:
    clickhouse_connect = None


def parse_args():
    p = argparse.ArgumentParser(
        description="Check ClickHouse time-series continuity and optionally delete data before the last contiguous segment."
    )
    p.add_argument("--url", required=True, help="ClickHouse connection URL, e.g. https://<host>:8443")
    p.add_argument("--user", required=True, help="ClickHouse username")
    p.add_argument("--password", required=True, help="ClickHouse password")
    p.add_argument("--database", required=True, help="Database name")
    p.add_argument("--table", required=True, help="Table name")
    p.add_argument("--ts-col", default="ts", help="Timestamp column name (default: ts)")
    p.add_argument(
        "--key-cols",
        default="",
        help="Comma-separated key columns to check continuity per-series (e.g. symbol,venue). If empty, checks a single series.",
    )
    p.add_argument(
        "--unit",
        choices=["second", "millisecond", "minute", "hour"],
        default="second",
        help="Gap unit for dateDiff (default: second)",
    )
    p.add_argument(
        "--step",
        type=int,
        default=1,
        help="Expected step in given unit (default: 1)",
    )
    p.add_argument(
        "--where",
        default="",
        help="Optional additional WHERE filter, e.g. date >= '2024-10-01'",
    )
    p.add_argument(
        "--apply",
        action="store_true",
        help="Execute ALTER TABLE ... DELETE to drop all rows before cutoff(s). Default is dry-run.",
    )
    p.add_argument(
        "--mutations-sync",
        type=int,
        default=2,
        help="SET mutations_sync value before delete (2 waits for completion).",
    )
    p.add_argument(
        "--sample-gaps",
        type=int,
        default=5,
        help="Show up to N sample gap rows in dry-run summary (default: 5)",
    )
    return p.parse_args()


def q_ident(name: str) -> str:
    # Basic identifier quoting; assumes input is a plain identifier
    return f"`{name}`"


def build_where(extra_where: str) -> str:
    extra_where = (extra_where or "").strip()
    return f"WHERE {extra_where}" if extra_where else ""


def main():
    args = parse_args()
    if clickhouse_connect is None:
        print("clickhouse-connect is required. Install with: pip install clickhouse-connect", file=sys.stderr)
        sys.exit(2)

    client = clickhouse_connect.get_client(
        url=args.url,
        username=args.user,
        password=args.password,
        database=args.database,
    )

    db = args.database
    table = args.table
    ts_col = args.ts_col
    where_sql = build_where(args.where)
    unit = args.unit
    step = args.step

    if args.key_cols.strip():
        key_cols: List[str] = [c.strip() for c in args.key_cols.split(",") if c.strip()]
    else:
        key_cols = []

    fq_table = f"{q_ident(db)}.{q_ident(table)}"

    # Check total gap count first
    if key_cols:
        part_clause = f"PARTITION BY {', '.join(q_ident(k) for k in key_cols)}"
        key_select = ", ".join(q_ident(k) for k in key_cols) + ", "
    else:
        part_clause = ""
        key_select = ""

    base = f"""
        WITH base AS (
            SELECT {key_select}{q_ident(ts_col)} AS ts
            FROM {fq_table}
            {where_sql}
        ), d AS (
            SELECT
                {key_select}
                ts,
                dateDiff('{unit}', lagInFrame(ts) OVER ({part_clause} ORDER BY ts), ts) AS gap_units
            FROM base
        )
        SELECT countIf(gap_units > {step}) AS gaps_total
        FROM d
    """

    gaps_total = client.query(base).first_item()

    print(f"Checked continuity on {db}.{table}")
    print(f"Unit: {unit}, expected step: {step}")
    if args.where:
        print(f"Filter: {args.where}")
    print(f"Key columns: {', '.join(key_cols) if key_cols else '(none)'}")
    print(f"Total gaps found: {gaps_total}")

    # Show sample gaps (dry-run diagnostic)
    if gaps_total:
        if key_cols:
            sample_gap_sql = f"""
                WITH base AS (
                    SELECT {key_select}{q_ident(ts_col)} AS ts
                    FROM {fq_table}
                    {where_sql}
                ), d AS (
                    SELECT
                        {key_select}
                        ts,
                        dateDiff('{unit}', lagInFrame(ts) OVER (PARTITION BY {', '.join(q_ident(k) for k in key_cols)} ORDER BY ts), ts) AS gap_units,
                        lagInFrame(ts) OVER (PARTITION BY {', '.join(q_ident(k) for k in key_cols)} ORDER BY ts) AS prev_ts
                    FROM base
                )
                SELECT {', '.join(q_ident(k) for k in key_cols)}, prev_ts, ts, gap_units
                FROM d
                WHERE gap_units > {step}
                ORDER BY {', '.join(q_ident(k) for k in key_cols)} ASC, ts ASC
                LIMIT {int(args.sample_gaps)}
            """
        else:
            sample_gap_sql = f"""
                WITH base AS (
                    SELECT {q_ident(ts_col)} AS ts
                    FROM {fq_table}
                    {where_sql}
                ), d AS (
                    SELECT
                        ts,
                        dateDiff('{unit}', lagInFrame(ts) OVER (ORDER BY ts), ts) AS gap_units,
                        lagInFrame(ts) OVER (ORDER BY ts) AS prev_ts
                    FROM base
                )
                SELECT prev_ts, ts, gap_units
                FROM d
                WHERE gap_units > {step}
                ORDER BY ts ASC
                LIMIT {int(args.sample_gaps)}
            """
        print("\nSample gaps (up to limit):")
        for row in client.query(sample_gap_sql).result_rows:
            print(row)

    # If no gaps, exit without deletes
    if not gaps_total:
        print("\nNo gaps detected. Nothing to delete.")
        return

    # Compute cutoff(s): start of the last contiguous segment per key (or global)
    if key_cols:
        seg_sql = f"""
            WITH base AS (
                SELECT {key_select}{q_ident(ts_col)} AS ts
                FROM {fq_table}
                {where_sql}
            ), d AS (
                SELECT
                    {key_select}
                    ts,
                    dateDiff('{unit}', lagInFrame(ts) OVER (PARTITION BY {', '.join(q_ident(k) for k in key_cols)} ORDER BY ts), ts) AS gap_units
                FROM base
            ), seg AS (
                SELECT
                    {key_select}
                    ts,
                    (gap_units > {step}) AS is_gap,
                    sum(toUInt8(gap_units > {step})) OVER (
                        PARTITION BY {', '.join(q_ident(k) for k in key_cols)} ORDER BY ts
                        ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
                    ) AS seg_id
                FROM d
            ), last_seg AS (
                SELECT {', '.join(q_ident(k) for k in key_cols)}, max(seg_id) AS last_seg_id
                FROM seg
                GROUP BY {', '.join(q_ident(k) for k in key_cols)}
            )
            SELECT {', '.join('s.' + q_ident(k) for k in key_cols)}, min(s.ts) AS cutoff_ts
            FROM seg s
            INNER JOIN last_seg l USING ({', '.join(q_ident(k) for k in key_cols)})
            WHERE s.seg_id = l.last_seg_id
            GROUP BY {', '.join('s.' + q_ident(k) for k in key_cols)}
            ORDER BY {', '.join('s.' + q_ident(k) for k in key_cols)}
        """
        rows = client.query(seg_sql).result_rows
        print("\nComputed per-key cutoff timestamps (start of last contiguous segment):")
        for r in rows[:10]:
            print(r)
        if len(rows) > 10:
            print(f"... ({len(rows) - 10} more)")
    else:
        seg_sql = f"""
            WITH base AS (
                SELECT {q_ident(ts_col)} AS ts
                FROM {fq_table}
                {where_sql}
            ), d AS (
                SELECT
                    ts,
                    dateDiff('{unit}', lagInFrame(ts) OVER (ORDER BY ts), ts) AS gap_units
                FROM base
            ), seg AS (
                SELECT
                    ts,
                    (gap_units > {step}) AS is_gap,
                    sum(toUInt8(gap_units > {step})) OVER (
                        ORDER BY ts ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW
                    ) AS seg_id
                FROM d
            ), last_seg AS (
                SELECT max(seg_id) AS last_seg_id FROM seg
            )
            SELECT min(s.ts) AS cutoff_ts
            FROM seg s
            CROSS JOIN last_seg l
            WHERE s.seg_id = l.last_seg_id
        """
        rows = client.query(seg_sql).result_rows
        cutoff_ts = rows[0][0]
        print(f"\nComputed cutoff timestamp (start of last contiguous segment): {cutoff_ts}")

    if not args.apply:
        print("\nDry-run only. Re-run with --apply to execute deletions.")
        return

    # Apply deletions
    print("\nApplying deletions (waiting for mutations to complete)...")
    client.command(f"SET mutations_sync = {int(args.mutations_sync)}")

    total_mutations = 0

    def _quote_literal(val):
        if isinstance(val, str):
            return "'" + val.replace("\\", "\\\\").replace("'", "''") + "'"
        if val is None:
            return "NULL"
        return str(val)
    if key_cols:
        # Multiple DELETEs, one per key
        for row in rows:
            *key_vals, cutoff_ts_val = row
            key_predicates = []
            for col, val in zip(key_cols, key_vals):
                key_predicates.append(f"{q_ident(col)} = {_quote_literal(val)}")
            where_pred = " AND ".join(key_predicates) + f" AND {q_ident(ts_col)} < toDateTime('{cutoff_ts_val}')"
            sql = f"ALTER TABLE {fq_table} DELETE WHERE {where_pred}"
            client.command(sql)
            total_mutations += 1
    else:
        where_pred = f"{q_ident(ts_col)} < toDateTime('{cutoff_ts}')"
        sql = f"ALTER TABLE {fq_table} DELETE WHERE {where_pred}"
        client.command(sql)
        total_mutations = 1

    print(f"Done. Submitted {total_mutations} mutation(s).")
    print("Note: physical removal happens asynchronously during merges.")


if __name__ == "__main__":
    main()
