#!/usr/bin/env python3
"""Backfill official Polymarket settlement rows for bounded PM5D windows."""

from __future__ import annotations

import argparse
import asyncio
import hashlib
import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, TYPE_CHECKING

if TYPE_CHECKING:
    import asyncpg


DEFAULT_SYMBOLS = "BTCUSDT,ETHUSDT,SOLUSDT,XRPUSDT,DOGEUSDT,HYPEUSDT,BNBUSDT"


def parse_symbols(raw: str) -> list[str]:
    return [item.strip().upper() for item in raw.split(",") if item.strip()]


def parse_utc_ts(raw: str) -> datetime | None:
    if not raw:
        return None
    parsed = datetime.fromisoformat(raw.replace("Z", "+00:00"))
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def parser() -> argparse.ArgumentParser:
    parsed = argparse.ArgumentParser()
    parsed.add_argument("legacy_db_url", nargs="?", default="")
    parsed.add_argument("--db-url", default="")
    parsed.add_argument("--start-ts", default="2026-04-05T00:00:00Z")
    parsed.add_argument("--end-ts", default="")
    parsed.add_argument("--symbols", default=DEFAULT_SYMBOLS)
    parsed.add_argument("--dry-run", action="store_true")
    parsed.add_argument("--report-json", type=Path)
    parsed.add_argument("--persist-coverage", action="store_true")
    parsed.add_argument("--run-id", default="")
    parsed.add_argument("--source-workflow", default="repair-official-settlement-coverage.yml")
    parsed.add_argument("--workflow-run-id", default="")
    parsed.add_argument("--workflow-run-url", default="")
    parsed.add_argument("--artifact-name", default="")
    return parsed


async def load_candidate_markets(
    conn: "asyncpg.Connection",
    *,
    start_ts: datetime,
    end_ts: datetime | None,
    symbols: list[str],
) -> list[asyncpg.Record]:
    return await conn.fetch(
        """
        WITH event_tokens AS (
            SELECT
                market_slug,
                trim(both '"' from ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->>0)) AS up_token,
                trim(both '"' from ((raw_market->'markets'->0->>'clobTokenIds')::jsonb->>1)) AS down_token
            FROM pm_market_metadata
            WHERE end_time >= $1::timestamptz
              AND ($2::timestamptz IS NULL OR end_time < $2::timestamptz)
              AND symbol = ANY($3::text[])
        )
        SELECT DISTINCT et.market_slug, et.up_token, et.down_token
        FROM event_tokens et
        WHERE et.market_slug IS NOT NULL
          AND et.up_token IS NOT NULL
          AND et.down_token IS NOT NULL
          AND et.up_token <> ''
          AND et.down_token <> ''
        ORDER BY et.market_slug
        """,
        start_ts,
        end_ts,
        symbols,
    )


def settlement_rows_with_reason_from_gamma(
    data: dict[str, Any],
    *,
    up_token: str,
    down_token: str,
) -> tuple[list[tuple[str, str, float]], str]:
    if data.get("closed") is not True:
        return [], "not_closed"
    try:
        token_ids = json.loads(data.get("clobTokenIds") or "[]")
        outcome_prices = json.loads(data.get("outcomePrices") or "[]")
    except (TypeError, json.JSONDecodeError):
        return [], "malformed_gamma_payload"
    if len(token_ids) != 2 or len(outcome_prices) != 2:
        return [], "malformed_gamma_payload"

    settlements = []
    try:
        price_pairs = [
            (str(token_id), float(raw_price))
            for token_id, raw_price in zip(token_ids, outcome_prices)
        ]
    except (TypeError, ValueError):
        return [], "malformed_gamma_payload"
    for token_id, price in price_pairs:
        if price >= 0.95:
            settlements.append((str(token_id), "winner", 1.0))
        elif price <= 0.05:
            settlements.append((str(token_id), "loser", 0.0))
        else:
            return [], "unresolved_prices"

    matched = [item for item in settlements if item[0] in {up_token, down_token}]
    matched_token_ids = {item[0] for item in matched}
    if len(matched) != 2 or matched_token_ids != {up_token, down_token}:
        return [], "token_mismatch"
    return matched, "settled_prices"


def settlement_rows_from_gamma(
    data: dict[str, Any],
    *,
    up_token: str,
    down_token: str,
) -> list[tuple[str, str, float]]:
    rows, _reason = settlement_rows_with_reason_from_gamma(
        data,
        up_token=up_token,
        down_token=down_token,
    )
    return rows


async def upsert_settlement(
    conn: "asyncpg.Connection",
    *,
    market_slug: str,
    token_id: str,
    outcome: str,
    settled_price: float,
) -> bool:
    result = await conn.execute(
        """
        INSERT INTO pm_token_settlements (
            token_id, market_slug, outcome,
            settled_price, resolved, resolved_at, fetched_at
        ) VALUES ($1, $2, $3, $4, true, NOW(), NOW())
        ON CONFLICT (token_id) DO UPDATE SET
            settled_price = EXCLUDED.settled_price,
            outcome = EXCLUDED.outcome,
            resolved = true,
            resolved_at = COALESCE(pm_token_settlements.resolved_at, NOW()),
            fetched_at = NOW()
        WHERE pm_token_settlements.resolved = false
           OR pm_token_settlements.settled_price IS DISTINCT FROM EXCLUDED.settled_price
           OR pm_token_settlements.outcome IS DISTINCT FROM EXCLUDED.outcome
        """,
        token_id,
        market_slug,
        outcome,
        settled_price,
    )
    return result != "INSERT 0 0"


def report_sha256(report: dict[str, Any]) -> str:
    return hashlib.sha256(
        json.dumps(report, sort_keys=True, separators=(",", ":")).encode("utf-8")
    ).hexdigest()


def coverage_blockers(report: dict[str, Any]) -> list[str]:
    blockers: list[str] = []
    candidate_count = int(report.get("candidate_market_count") or 0)
    settled_count = int(report.get("settled_count") or 0)
    unchanged_count = int(report.get("unchanged_count") or 0)
    settlement_token_count = settled_count + unchanged_count
    if report.get("dry_run"):
        blockers.append("dry_run_not_durable_coverage")
    if candidate_count <= 0:
        blockers.append("candidate_market_count_empty")
    expected_token_count = candidate_count * 2
    if settlement_token_count != expected_token_count:
        blockers.append(f"settlement_token_count:{settlement_token_count}!={expected_token_count}")
    for key in [
        "active_reset_count",
        "open_market_count",
        "malformed_payload_count",
        "unresolved_price_count",
        "token_mismatch_count",
        "skipped_count",
        "error_count",
    ]:
        value = int(report.get(key) or 0)
        if value > 0:
            blockers.append(f"{key}:{value}")
    return sorted(set(blockers))


async def persist_coverage_check(
    conn: "asyncpg.Connection",
    *,
    report: dict[str, Any],
    args: argparse.Namespace,
) -> str:
    blockers = coverage_blockers(report)
    content_sha256 = report_sha256(report)
    settlement_coverage_id = f"official_settlement_coverage:{content_sha256[:32]}"
    settlement_token_count = int(report.get("settled_count") or 0) + int(
        report.get("unchanged_count") or 0
    )
    valid = not blockers
    report["settlement_coverage_id"] = settlement_coverage_id
    report["settlement_token_count"] = settlement_token_count
    report["valid"] = valid
    report["blockers"] = blockers
    artifact_sha256 = report_sha256(report)
    await conn.execute(
        """
        INSERT INTO official_settlement_coverage_checks (
            settlement_coverage_id,
            run_id,
            source_workflow,
            workflow_run_id,
            workflow_run_url,
            artifact_name,
            artifact_sha256,
            artifact_json,
            schema_version,
            surface,
            window_start_ts,
            window_end_ts,
            symbols_json,
            candidate_market_count,
            settlement_token_count,
            settled_count,
            unchanged_count,
            skipped_count,
            error_count,
            valid,
            blockers_json
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8::jsonb, $9, 'pm_token_settlements',
            $10, $11, $12::jsonb, $13, $14, $15, $16, $17, $18, $19, $20::jsonb
        )
        ON CONFLICT (settlement_coverage_id) DO UPDATE SET
            run_id = EXCLUDED.run_id,
            source_workflow = EXCLUDED.source_workflow,
            workflow_run_id = EXCLUDED.workflow_run_id,
            workflow_run_url = EXCLUDED.workflow_run_url,
            artifact_name = EXCLUDED.artifact_name,
            artifact_sha256 = EXCLUDED.artifact_sha256,
            artifact_json = EXCLUDED.artifact_json,
            schema_version = EXCLUDED.schema_version,
            surface = EXCLUDED.surface,
            window_start_ts = EXCLUDED.window_start_ts,
            window_end_ts = EXCLUDED.window_end_ts,
            symbols_json = EXCLUDED.symbols_json,
            candidate_market_count = EXCLUDED.candidate_market_count,
            settlement_token_count = EXCLUDED.settlement_token_count,
            settled_count = EXCLUDED.settled_count,
            unchanged_count = EXCLUDED.unchanged_count,
            skipped_count = EXCLUDED.skipped_count,
            error_count = EXCLUDED.error_count,
            valid = EXCLUDED.valid,
            blockers_json = EXCLUDED.blockers_json
        """,
        settlement_coverage_id,
        args.run_id or args.workflow_run_id or "manual",
        args.source_workflow,
        args.workflow_run_id or None,
        args.workflow_run_url or None,
        args.artifact_name or None,
        artifact_sha256,
        json.dumps(report, sort_keys=True),
        report["schema_version"],
        parse_utc_ts(report["start_ts"]),
        parse_utc_ts(report["end_ts"]),
        json.dumps(report.get("symbols") or [], sort_keys=True),
        int(report.get("candidate_market_count") or 0),
        settlement_token_count,
        int(report.get("settled_count") or 0),
        int(report.get("unchanged_count") or 0),
        int(report.get("skipped_count") or 0),
        int(report.get("error_count") or 0),
        valid,
        json.dumps(blockers, sort_keys=True),
    )
    return settlement_coverage_id


async def run(args: argparse.Namespace) -> dict[str, Any]:
    db_url = args.db_url or args.legacy_db_url
    if not db_url:
        db_url = "postgresql://postgres:postgres@localhost:15432/ploy"
    symbols = parse_symbols(args.symbols)
    if not symbols:
        raise SystemExit("--symbols must include at least one symbol")
    start_ts = parse_utc_ts(args.start_ts)
    if start_ts is None:
        raise SystemExit("--start-ts is required")
    end_ts = parse_utc_ts(args.end_ts)
    import asyncpg
    import httpx

    conn = await asyncpg.connect(db_url)
    try:
        rows = await load_candidate_markets(
            conn,
            start_ts=start_ts,
            end_ts=end_ts,
            symbols=symbols,
        )

        settled = 0
        would_settle = 0
        errors = 0
        skipped = 0
        active_reset = 0
        open_market = 0
        malformed_payload = 0
        unresolved_prices = 0
        token_mismatch = 0
        unchanged = 0

        async with httpx.AsyncClient(timeout=5.0) as client:
            for i, row in enumerate(rows):
                market_slug = row["market_slug"]
                up_token = row["up_token"]
                down_token = row["down_token"]

                try:
                    resp = await client.get(
                        f"https://gamma-api.polymarket.com/markets/{market_slug}",
                        headers={"User-Agent": "ploy-settlement-backfill/official"},
                    )
                    resp.raise_for_status()
                    data = resp.json()

                    if data.get("closed") is False:
                        if not args.dry_run:
                            await conn.execute(
                                """
                                UPDATE pm_token_settlements
                                SET settled_price = NULL,
                                    outcome = NULL,
                                    resolved = FALSE,
                                    resolved_at = NULL,
                                    fetched_at = NOW()
                                WHERE market_slug = $1
                                  AND resolved = TRUE
                                """,
                                market_slug,
                            )
                        active_reset += 1
                        open_market += 1
                        skipped += 1
                        continue

                    settlements, reason = settlement_rows_with_reason_from_gamma(
                        data,
                        up_token=up_token,
                        down_token=down_token,
                    )
                    if not settlements:
                        if reason == "not_closed":
                            open_market += 1
                        elif reason == "malformed_gamma_payload":
                            malformed_payload += 1
                        elif reason == "unresolved_prices":
                            unresolved_prices += 1
                        elif reason == "token_mismatch":
                            token_mismatch += 1
                        skipped += 1
                        continue

                    for token_id, outcome, settled_price in settlements:
                        if args.dry_run:
                            would_settle += 1
                        else:
                            changed = await upsert_settlement(
                                conn,
                                market_slug=market_slug,
                                token_id=token_id,
                                outcome=outcome,
                                settled_price=settled_price,
                            )
                            if changed:
                                settled += 1
                            else:
                                unchanged += 1

                except Exception:
                    errors += 1

                if (i + 1) % 100 == 0:
                    print(
                        "Progress: "
                        f"{i + 1}/{len(rows)} settled={settled} "
                        f"would_settle={would_settle} unchanged={unchanged} "
                        f"skipped={skipped} errors={errors}"
                    )

                await asyncio.sleep(0.2)

        report = {
            "schema_version": "official_settlement_repair.v1",
            "dry_run": args.dry_run,
            "start_ts": args.start_ts,
            "end_ts": args.end_ts,
            "symbols": symbols,
            "candidate_market_count": len(rows),
            "settled_count": settled,
            "would_settle_count": would_settle,
            "active_reset_count": active_reset,
            "open_market_count": open_market,
            "malformed_payload_count": malformed_payload,
            "unresolved_price_count": unresolved_prices,
            "token_mismatch_count": token_mismatch,
            "unchanged_count": unchanged,
            "skipped_count": skipped,
            "error_count": errors,
        }
        report["settlement_token_count"] = settled + unchanged
        report["blockers"] = coverage_blockers(report)
        report["valid"] = not report["blockers"]
        if args.persist_coverage:
            await persist_coverage_check(conn, report=report, args=args)
        return report
    finally:
        await conn.close()


async def main_async() -> None:
    args = parser().parse_args()
    report = await run(args)
    text = json.dumps(report, indent=2, sort_keys=True) + "\n"
    if args.report_json:
        args.report_json.write_text(text, encoding="utf-8")
    print(text)


def main() -> None:
    asyncio.run(main_async())


if __name__ == "__main__":
    main()
