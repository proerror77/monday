#!/usr/bin/env python3
"""Compare replay/backtest evidence with dry-run report evidence.

The script is intentionally schema-tolerant: current research artifacts and
operator reports are still evolving, so it extracts the common strategy
evaluation fields first and records missing strict parity fields as risk flags.
"""

from __future__ import annotations

import argparse
import json
from datetime import datetime, timezone
from decimal import Decimal, InvalidOperation
from pathlib import Path
from typing import Any


STRICT_FIELDS = [
    "event_id",
    "decision_ts",
    "quote",
    "signal_inputs",
    "side",
    "entry_price",
    "fill_status",
    "settlement",
    "pnl",
]

STRICT_FIELD_ALIASES = {
    "event_id": ("event_id", "market_id", "market_slug", "token_id"),
    "decision_ts": ("decision_ts", "opened_at", "timestamp"),
    "quote": ("quote", "observed_quote"),
    "signal_inputs": ("signal_inputs", "decision_features", "features"),
    "side": ("side", "market_side", "direction"),
    "entry_price": ("entry_price", "avg_entry_price"),
    "fill_status": ("fill_status", "status"),
    "settlement": ("settlement", "resolved_up_won"),
    "pnl": ("pnl", "net_pnl", "realized_pnl"),
}

ORDER_STRICT_FIELDS = [
    "deployment_id",
    "intent_id",
    "token_id",
    "quantity",
    "limit_price",
    "filled_quantity",
    "status",
]

FILL_STRICT_FIELDS = [
    "deployment_id",
    "intent_id",
    "token_id",
    "fill_side",
    "quantity",
    "price",
    "fee",
    "fill_timestamp",
]

NUMERIC_FIELDS = {
    "quantity",
    "requested_qty",
    "limit_price",
    "filled_quantity",
    "avg_fill_price",
    "price",
    "fee",
    "quote",
    "entry_price",
    "settlement",
    "pnl",
}

TIMESTAMP_FIELDS = {"created_at", "fill_timestamp", "decision_ts"}

NUMERIC_TOLERANCE = Decimal("0.000001")
TIMESTAMP_TOLERANCE_SECONDS = 1.0

ORDER_KEY_CANDIDATES = (
    ("deployment_id", "intent_id"),
    ("intent_id", "token_id", "order_side", "quantity", "limit_price", "filled_quantity"),
    ("deployment_id", "token_id", "order_side", "quantity", "limit_price", "filled_quantity", "status"),
)

FILL_KEY_CANDIDATES = (
    ("deployment_id", "intent_id"),
    ("intent_id", "token_id", "fill_side", "quantity", "price", "fee"),
    ("deployment_id", "token_id", "fill_side", "quantity", "price", "fee", "fill_timestamp"),
)

EVENT_KEY_CANDIDATES = (
    ("deployment_id", "event_id", "token_id", "side"),
    ("event_id", "token_id", "side"),
    ("deployment_id", "event_id", "side"),
    ("event_id", "side"),
)


def filter_summary(
    *,
    deployment_id: str | None,
    since: datetime | None,
    until: datetime | None,
) -> dict[str, str | None]:
    return {
        "deployment_id": deployment_id,
        "since": since.isoformat().replace("+00:00", "Z") if since else None,
        "until": until.isoformat().replace("+00:00", "Z") if until else None,
    }


def load_json(path: Path) -> Any:
    with path.open() as handle:
        return json.load(handle)


def find_first_json(root: Path) -> Path:
    if root.is_file():
        return root
    candidates = sorted(root.rglob("*.json"))
    if not candidates:
        raise SystemExit(f"no JSON files found under {root}")
    preferred = [path for path in candidates if path.name == "evaluation.json"]
    return preferred[0] if preferred else candidates[0]


def get_path(data: Any, *path: str) -> Any:
    current = data
    for key in path:
        if not isinstance(current, dict) or key not in current:
            return None
        current = current[key]
    return current


def extract_metrics(data: Any) -> dict[str, Any]:
    if not isinstance(data, dict):
        return {}
    metrics = data.get("metrics")
    if isinstance(metrics, dict):
        return metrics
    validation = data.get("validation")
    if isinstance(validation, dict):
        return validation
    report = data.get("report")
    if isinstance(report, dict):
        return {
            "health": report.get("health"),
            "review_count": len(report.get("reviews") or []),
        }
    return {}


def extract_events(data: Any) -> list[dict[str, Any]]:
    if isinstance(data, list):
        return [item for item in data if isinstance(item, dict)]
    if not isinstance(data, dict):
        return []
    events: list[dict[str, Any]] = []
    for path in [
        ("runtime_evidence", "events"),
        ("events",),
        ("trades",),
        ("fills",),
        ("closed_trades",),
        ("recent_closed",),
        ("open_positions",),
        ("report", "events"),
        ("report", "trades"),
        ("report", "closed_trades"),
        ("report", "recent_closed"),
        ("dry_run", "events"),
        ("data", "events"),
    ]:
        value = get_path(data, *path)
        if isinstance(value, list):
            events.extend(item for item in value if isinstance(item, dict))

    strategies = data.get("strategies")
    if isinstance(strategies, list):
        for strategy in strategies:
            if not isinstance(strategy, dict):
                continue
            for key in ("events", "trades", "fills", "closed_trades", "recent_closed", "open_positions"):
                value = strategy.get(key)
                if isinstance(value, list):
                    for item in value:
                        if isinstance(item, dict):
                            enriched = dict(item)
                            for identity_key in ("runtime_mode", "strategy_id", "deployment_id", "experiment_label"):
                                enriched.setdefault(identity_key, strategy.get(identity_key))
                            events.append(enriched)

    if events:
        deduped = {}
        for idx, event in enumerate(events):
            key = (
                event.get("trade_key")
                or event.get("intent_id")
                or event.get("event_id")
                or event.get("token_id")
                or f"row-{idx}"
            )
            deduped[str(key)] = event
        return list(deduped.values())
    return []


def canonical_field(event: dict[str, Any], field: str) -> Any:
    for key in STRICT_FIELD_ALIASES.get(field, (field,)):
        if key in event and event[key] is not None:
            return event[key]
    return None


def available_strict_fields(events: list[dict[str, Any]]) -> list[str]:
    available = set()
    for event in events:
        for field in STRICT_FIELDS:
            if canonical_field(event, field) is not None:
                available.add(field)
    return sorted(available)


def index_events(events: list[dict[str, Any]]) -> dict[str, dict[str, Any]]:
    indexed = {}
    for idx, event in enumerate(events):
        key = (
            event.get("trade_key")
            or event.get("intent_id")
            or event.get("event_id")
            or event.get("market_id")
            or event.get("market_slug")
            or event.get("token_id")
            or f"row-{idx}"
        )
        indexed[str(key)] = event
    return indexed


def first_present(row: dict[str, Any], *keys: str) -> Any:
    for key in keys:
        if key in row and row[key] is not None:
            return row[key]
    return None


def normalize_text(value: Any, *, upper: bool = False) -> str | None:
    if value is None:
        return None
    text = str(value).strip()
    if not text:
        return None
    return text.upper() if upper else text


def normalize_decimal(value: Any) -> str | None:
    if value is None or value == "":
        return None
    try:
        parsed = Decimal(str(value))
    except (InvalidOperation, ValueError):
        return normalize_text(value)
    normalized = parsed.normalize()
    return format(normalized, "f")


def parse_timestamp(value: Any) -> datetime | None:
    if value is None:
        return None
    text = str(value).strip()
    if not text:
        return None
    if text.endswith("Z"):
        text = f"{text[:-1]}+00:00"
    try:
        parsed = datetime.fromisoformat(text)
    except ValueError:
        return None
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=timezone.utc)
    return parsed.astimezone(timezone.utc)


def normalize_timestamp(value: Any) -> str | None:
    parsed = parse_timestamp(value)
    if parsed is None:
        return normalize_text(value)
    return parsed.isoformat().replace("+00:00", "Z")


def row_timestamp(row: dict[str, Any], *keys: str) -> datetime | None:
    for key in keys:
        parsed = parse_timestamp(row.get(key))
        if parsed is not None:
            return parsed
    return None


def row_passes_filters(
    row: dict[str, Any],
    *,
    deployment_id: str | None,
    since: datetime | None,
    until: datetime | None,
    timestamp_keys: tuple[str, ...],
) -> bool:
    if deployment_id and normalize_text(row.get("deployment_id")) != deployment_id:
        return False
    if since is None and until is None:
        return True
    ts = row_timestamp(row, *timestamp_keys)
    if ts is None:
        return False
    if since is not None and ts < since:
        return False
    if until is not None and ts > until:
        return False
    return True


def filter_rows(
    rows: list[dict[str, Any]],
    *,
    deployment_id: str | None,
    since: datetime | None,
    until: datetime | None,
    timestamp_keys: tuple[str, ...],
) -> list[dict[str, Any]]:
    return [
        row
        for row in rows
        if row_passes_filters(
            row,
            deployment_id=deployment_id,
            since=since,
            until=until,
            timestamp_keys=timestamp_keys,
        )
    ]


def filter_orders_with_fill_window_fallback(
    orders: list[dict[str, Any]],
    filtered_fills: list[dict[str, Any]],
    *,
    deployment_id: str | None,
    since: datetime | None,
    until: datetime | None,
) -> list[dict[str, Any]]:
    if since is None and until is None:
        return filter_rows(
            orders,
            deployment_id=deployment_id,
            since=since,
            until=until,
            timestamp_keys=("created_at",),
        )
    fill_intents = {
        (row.get("deployment_id"), row.get("intent_id"))
        for row in filtered_fills
        if row.get("intent_id")
    }
    filtered: list[dict[str, Any]] = []
    for row in orders:
        if row_passes_filters(
            row,
            deployment_id=deployment_id,
            since=since,
            until=until,
            timestamp_keys=("created_at",),
        ):
            filtered.append(row)
            continue
        if deployment_id and row.get("deployment_id") != deployment_id:
            continue
        if (row.get("deployment_id"), row.get("intent_id")) in fill_intents:
            filtered.append(row)
    return filtered


def normalize_status(value: Any) -> str | None:
    text = normalize_text(value, upper=True)
    if text is None:
        return None
    return text.replace("-", "_")


def normalize_order(row: dict[str, Any]) -> dict[str, Any]:
    quantity = first_present(row, "quantity", "requested_qty", "requested_quantity")
    return {
        "deployment_id": normalize_text(first_present(row, "deployment_id")),
        "intent_id": normalize_text(first_present(row, "intent_id")),
        "order_id": normalize_text(first_present(row, "order_id")),
        "venue_order_id": normalize_text(first_present(row, "venue_order_id")),
        "event_id": normalize_text(first_present(row, "event_id", "market_id", "market_slug")),
        "token_id": normalize_text(first_present(row, "token_id")),
        "market_side": normalize_text(first_present(row, "market_side", "direction"), upper=True),
        "order_side": normalize_text(first_present(row, "order_side", "side"), upper=True),
        "purpose": normalize_text(first_present(row, "purpose", "intent_purpose"), upper=True),
        "quantity": normalize_decimal(quantity),
        "requested_qty": normalize_decimal(quantity),
        "limit_price": normalize_decimal(first_present(row, "limit_price", "entry_price")),
        "filled_quantity": normalize_decimal(first_present(row, "filled_quantity", "filled_qty")),
        "avg_fill_price": normalize_decimal(first_present(row, "avg_fill_price", "fill_price")),
        "status": normalize_status(first_present(row, "status", "state", "fill_status")),
        "rejection_reason": normalize_text(first_present(row, "rejection_reason", "last_error")),
        "created_at": normalize_timestamp(first_present(row, "created_at", "recorded_at", "decision_ts", "timestamp")),
    }


def normalize_fill(row: dict[str, Any]) -> dict[str, Any]:
    return {
        "deployment_id": normalize_text(first_present(row, "deployment_id")),
        "intent_id": normalize_text(first_present(row, "intent_id")),
        "order_id": normalize_text(first_present(row, "order_id")),
        "fill_id": normalize_text(first_present(row, "fill_id")),
        "event_id": normalize_text(first_present(row, "event_id", "market_id", "market_slug")),
        "token_id": normalize_text(first_present(row, "token_id")),
        "market_side": normalize_text(first_present(row, "market_side", "direction"), upper=True),
        "fill_side": normalize_text(first_present(row, "fill_side", "side", "order_side"), upper=True),
        "purpose": normalize_text(first_present(row, "purpose", "intent_purpose"), upper=True),
        "quantity": normalize_decimal(first_present(row, "quantity", "filled_qty", "filled_quantity")),
        "price": normalize_decimal(first_present(row, "price", "fill_price", "avg_fill_price")),
        "fee": normalize_decimal(first_present(row, "fee", "fees")),
        "fill_timestamp": normalize_timestamp(first_present(row, "fill_timestamp", "timestamp", "created_at", "recorded_at")),
    }


def normalize_signal_inputs(value: Any) -> Any:
    if value is None:
        return None
    if isinstance(value, dict):
        return {str(key): normalize_signal_inputs(item) for key, item in sorted(value.items())}
    if isinstance(value, list):
        return [normalize_signal_inputs(item) for item in value]
    text = normalize_text(value)
    if text is None:
        return None
    try:
        parsed = json.loads(text)
    except json.JSONDecodeError:
        return normalize_decimal(text) or text
    if isinstance(parsed, (dict, list)):
        return normalize_signal_inputs(parsed)
    return normalize_decimal(parsed) or parsed


def normalize_event(row: dict[str, Any]) -> dict[str, Any]:
    return {
        "deployment_id": normalize_text(first_present(row, "deployment_id")),
        "intent_id": normalize_text(first_present(row, "intent_id")),
        "order_id": normalize_text(first_present(row, "order_id")),
        "event_id": normalize_text(first_present(row, "event_id", "market_id", "market_slug", "token_id")),
        "token_id": normalize_text(first_present(row, "token_id")),
        "decision_ts": normalize_timestamp(canonical_field(row, "decision_ts")),
        "quote": normalize_decimal(canonical_field(row, "quote")),
        "signal_inputs": normalize_signal_inputs(canonical_field(row, "signal_inputs")),
        "side": normalize_text(canonical_field(row, "side"), upper=True),
        "entry_price": normalize_decimal(canonical_field(row, "entry_price")),
        "fill_status": normalize_status(canonical_field(row, "fill_status")),
        "settlement": normalize_decimal(canonical_field(row, "settlement")),
        "pnl": normalize_decimal(canonical_field(row, "pnl")),
    }


def extract_list_at_paths(data: Any, paths: list[tuple[str, ...]]) -> list[dict[str, Any]]:
    rows: list[dict[str, Any]] = []
    for path in paths:
        value = get_path(data, *path)
        if isinstance(value, list):
            rows.extend(item for item in value if isinstance(item, dict))

    if isinstance(data, dict):
        strategies = data.get("strategies")
        if isinstance(strategies, list):
            for strategy in strategies:
                if not isinstance(strategy, dict):
                    continue
                for path in paths:
                    value = get_path(strategy, *path)
                    if isinstance(value, list):
                        for item in value:
                            if isinstance(item, dict):
                                enriched = dict(item)
                                for identity_key in ("runtime_mode", "strategy_id", "deployment_id", "experiment_label"):
                                    enriched.setdefault(identity_key, strategy.get(identity_key))
                                rows.append(enriched)
    return rows


def extract_runtime_orders(data: Any) -> list[dict[str, Any]]:
    rows = extract_list_at_paths(
        data,
        [
            ("runtime_evidence", "orders"),
            ("orders",),
            ("dry_run", "orders"),
            ("data", "orders"),
            ("report", "orders"),
        ],
    )
    return [normalize_order(row) for row in rows]


def extract_runtime_fills(data: Any) -> list[dict[str, Any]]:
    rows = extract_list_at_paths(
        data,
        [
            ("runtime_evidence", "fills"),
            ("fills",),
            ("dry_run", "fills"),
            ("data", "fills"),
            ("report", "fills"),
        ],
    )
    return [normalize_fill(row) for row in rows]


def extract_runtime_events(data: Any) -> list[dict[str, Any]]:
    rows = extract_list_at_paths(
        data,
        [
            ("runtime_evidence", "events"),
        ],
    )
    return [normalize_event(row) for row in rows]


def normalized_key(row: dict[str, Any], key_candidates: tuple[tuple[str, ...], ...]) -> str:
    for fields in key_candidates:
        values = [row.get(field) for field in fields]
        if all(value not in (None, "") for value in values):
            return "|".join(f"{field}={value}" for field, value in zip(fields, values))
    return ""


def index_normalized_rows(
    rows: list[dict[str, Any]],
    key_candidates: tuple[tuple[str, ...], ...],
) -> dict[str, dict[str, Any]]:
    indexed: dict[str, dict[str, Any]] = {}
    for index, row in enumerate(rows):
        key = normalized_key(row, key_candidates)
        if not key:
            key = f"row-{index}"
        indexed[key] = row
    return indexed


def values_match(field: str, left: Any, right: Any) -> bool:
    if field in NUMERIC_FIELDS:
        try:
            return abs(Decimal(str(left)) - Decimal(str(right))) <= NUMERIC_TOLERANCE
        except (InvalidOperation, ValueError):
            return left == right
    if field in TIMESTAMP_FIELDS:
        left_ts = parse_timestamp(left)
        right_ts = parse_timestamp(right)
        if left_ts is None or right_ts is None:
            return left == right
        return abs((left_ts - right_ts).total_seconds()) <= TIMESTAMP_TOLERANCE_SECONDS
    return left == right


def runtime_row_summary(row: dict[str, Any]) -> dict[str, Any]:
    return {
        "deployment_id": row.get("deployment_id"),
        "intent_id": row.get("intent_id"),
        "event_id": row.get("event_id"),
        "token_id": row.get("token_id"),
        "market_side": row.get("market_side"),
        "order_side": row.get("order_side"),
        "fill_side": row.get("fill_side"),
        "purpose": row.get("purpose"),
    }


def is_zero_decimal(value: Any) -> bool:
    if value in (None, ""):
        return False
    try:
        return Decimal(str(value)) == 0
    except (InvalidOperation, ValueError):
        return False


def is_positive_decimal(value: Any) -> bool:
    if value in (None, ""):
        return False
    try:
        return Decimal(str(value)) > 0
    except (InvalidOperation, ValueError):
        return False


def is_non_executed_exit_attempt(row: dict[str, Any]) -> bool:
    """Return true for zero-fill exit attempts that should not block entry parity.

    Recorded replay parity can run with `skip_settlement_exits = true` so the
    replay focuses on entry/fill accounting. Dry-run reports can still carry
    rejected take-profit attempts from live quote polling. These attempts did
    not fill and do not affect cashflow/PnL, so they are tracked as ignored
    metadata instead of blocking strict parity for executable entries.
    """

    intent_id = str(row.get("intent_id") or "").lower()
    status = normalize_status(row.get("status") or row.get("fill_status"))
    if not intent_id.startswith("tl_tp_") or status != "REJECTED":
        return False
    if row.get("filled_quantity") is not None and not is_zero_decimal(row.get("filled_quantity")):
        return False
    if row.get("pnl") is not None and not is_zero_decimal(row.get("pnl")):
        return False
    return True


def filter_non_executed_exit_attempts(
    rows: list[dict[str, Any]],
) -> tuple[list[dict[str, Any]], list[dict[str, Any]]]:
    kept: list[dict[str, Any]] = []
    ignored: list[dict[str, Any]] = []
    for row in rows:
        if is_non_executed_exit_attempt(row):
            ignored.append(row)
        else:
            kept.append(row)
    return kept, ignored


def recompute_strict_parity_ready(comparison: dict[str, Any]) -> bool:
    return (
        bool(comparison["shared_count"])
        and not comparison["missing_strict_fields"]
        and not comparison["mismatches"]
        and not comparison["missing_replay_rows"]
        and not comparison["missing_dryrun_rows"]
    )


def compare_normalized_rows(
    replay_rows: list[dict[str, Any]],
    dryrun_rows: list[dict[str, Any]],
    *,
    row_type: str,
    key_candidates: tuple[tuple[str, ...], ...],
    strict_fields: list[str],
) -> dict[str, Any]:
    replay_index = index_normalized_rows(replay_rows, key_candidates)
    dryrun_index = index_normalized_rows(dryrun_rows, key_candidates)
    shared_keys = sorted(set(replay_index) & set(dryrun_index))
    mismatches: list[dict[str, Any]] = []
    missing_fields = set()

    for key in shared_keys:
        replay = replay_index[key]
        dryrun = dryrun_index[key]
        for field in strict_fields:
            replay_value = replay.get(field)
            dryrun_value = dryrun.get(field)
            if replay_value is None or dryrun_value is None:
                missing_fields.add(f"{row_type}.{field}")
                continue
            if not values_match(field, replay_value, dryrun_value):
                mismatches.append(
                    {
                        "row_type": row_type,
                        "row_key": key,
                        "field": field,
                        "replay": replay_value,
                        "dryrun": dryrun_value,
                        "replay_row": runtime_row_summary(replay),
                        "dryrun_row": runtime_row_summary(dryrun),
                    }
                )

    return {
        "row_type": row_type,
        "replay_count": len(replay_rows),
        "dryrun_count": len(dryrun_rows),
        "shared_count": len(shared_keys),
        "strict_required_fields": strict_fields,
        "missing_replay_rows": sorted(set(dryrun_index) - set(replay_index))[:50],
        "missing_dryrun_rows": sorted(set(replay_index) - set(dryrun_index))[:50],
        "mismatches": mismatches[:100],
        "missing_strict_fields": sorted(missing_fields),
        "strict_parity_ready": bool(shared_keys)
        and not missing_fields
        and not mismatches
        and not (set(dryrun_index) - set(replay_index))
        and not (set(replay_index) - set(dryrun_index)),
    }


def strict_ready_fill_intents(
    replay_fills: list[dict[str, Any]],
    dryrun_fills: list[dict[str, Any]],
) -> set[tuple[str | None, str | None]]:
    replay_index = index_normalized_rows(replay_fills, FILL_KEY_CANDIDATES)
    dryrun_index = index_normalized_rows(dryrun_fills, FILL_KEY_CANDIDATES)
    ready: set[tuple[str | None, str | None]] = set()
    for key in sorted(set(replay_index) & set(dryrun_index)):
        replay = replay_index[key]
        dryrun = dryrun_index[key]
        intent_id = replay.get("intent_id")
        if not intent_id or intent_id != dryrun.get("intent_id"):
            continue
        if not (
            is_positive_decimal(replay.get("quantity"))
            and is_positive_decimal(dryrun.get("quantity"))
        ):
            continue
        if all(
            replay.get(field) is not None
            and dryrun.get(field) is not None
            and values_match(field, replay.get(field), dryrun.get(field))
            for field in FILL_STRICT_FIELDS
        ):
            ready.add((replay.get("deployment_id"), intent_id))
    return ready


def is_partial_fill_residual_cancel_mismatch(
    mismatch: dict[str, Any],
    ready_fill_intents: set[tuple[str | None, str | None]],
) -> bool:
    if mismatch.get("field") not in {"status", "fill_status"}:
        return False
    replay_status = normalize_status(mismatch.get("replay"))
    dryrun_status = normalize_status(mismatch.get("dryrun"))
    if {replay_status, dryrun_status} != {"PARTIALLY_FILLED", "CANCELED"}:
        return False
    replay_row = mismatch.get("replay_row") or {}
    dryrun_row = mismatch.get("dryrun_row") or {}
    if replay_row.get("intent_id") != dryrun_row.get("intent_id"):
        return False
    intent_key = (replay_row.get("deployment_id"), replay_row.get("intent_id"))
    return intent_key in ready_fill_intents


def suppress_partial_fill_residual_cancel_mismatches(
    comparison: dict[str, Any],
    *,
    ready_fill_intents: set[tuple[str | None, str | None]],
) -> dict[str, Any]:
    kept: list[dict[str, Any]] = []
    ignored: list[dict[str, Any]] = []
    for mismatch in comparison["mismatches"]:
        if is_partial_fill_residual_cancel_mismatch(mismatch, ready_fill_intents):
            ignored.append(mismatch)
        else:
            kept.append(mismatch)
    if not ignored:
        return comparison
    updated = dict(comparison)
    updated["mismatches"] = kept[:100]
    updated["ignored_partial_fill_cancel_status_mismatches"] = ignored[:100]
    updated["strict_parity_ready"] = recompute_strict_parity_ready(updated)
    return updated


def is_settlement_exit_row(row: dict[str, Any] | None) -> bool:
    if not row:
        return False
    intent_id = str(row.get("intent_id") or "").lower()
    purpose = str(row.get("purpose") or "").upper()
    order_side = str(row.get("order_side") or "").upper()
    fill_side = str(row.get("fill_side") or "").upper()
    is_sell_exit = order_side == "SELL" or fill_side == "SELL"
    return (
        intent_id.startswith("tl_settle_")
        or purpose in {"SETTLEMENT", "SETTLEMENT_EXIT"}
        or ("SETTLE" in intent_id and purpose in {"CLOSE", "EXIT"} and is_sell_exit)
    )


def settlement_exit_price_mismatches(mismatches: list[dict[str, Any]]) -> list[dict[str, Any]]:
    price_fields = {"limit_price", "price", "avg_fill_price"}
    settlement_mismatches: list[dict[str, Any]] = []
    for mismatch in mismatches:
        if mismatch.get("field") not in price_fields:
            continue
        if not (
            is_settlement_exit_row(mismatch.get("replay_row"))
            or is_settlement_exit_row(mismatch.get("dryrun_row"))
        ):
            continue
        settlement_mismatches.append(mismatch)
    return settlement_mismatches[:100]


def compare_runtime_evidence(
    replay: Any,
    dryrun: Any,
    *,
    deployment_id: str | None,
    since: datetime | None,
    until: datetime | None,
) -> dict[str, Any]:
    replay_events = filter_rows(
        extract_runtime_events(replay),
        deployment_id=deployment_id,
        since=since,
        until=until,
        timestamp_keys=("decision_ts",),
    )
    replay_fills = filter_rows(
        extract_runtime_fills(replay),
        deployment_id=deployment_id,
        since=since,
        until=until,
        timestamp_keys=("fill_timestamp",),
    )
    replay_orders = filter_orders_with_fill_window_fallback(
        extract_runtime_orders(replay),
        replay_fills,
        deployment_id=deployment_id,
        since=since,
        until=until,
    )
    dryrun_fills = filter_rows(
        extract_runtime_fills(dryrun),
        deployment_id=deployment_id,
        since=since,
        until=until,
        timestamp_keys=("fill_timestamp",),
    )
    dryrun_orders = filter_orders_with_fill_window_fallback(
        extract_runtime_orders(dryrun),
        dryrun_fills,
        deployment_id=deployment_id,
        since=since,
        until=until,
    )
    dryrun_events = filter_rows(
        extract_runtime_events(dryrun),
        deployment_id=deployment_id,
        since=since,
        until=until,
        timestamp_keys=("decision_ts",),
    )
    replay_events, ignored_replay_events = filter_non_executed_exit_attempts(replay_events)
    dryrun_events, ignored_dryrun_events = filter_non_executed_exit_attempts(dryrun_events)
    replay_orders, ignored_replay_orders = filter_non_executed_exit_attempts(replay_orders)
    dryrun_orders, ignored_dryrun_orders = filter_non_executed_exit_attempts(dryrun_orders)

    event_comparison = compare_normalized_rows(
        replay_events,
        dryrun_events,
        row_type="event",
        key_candidates=EVENT_KEY_CANDIDATES,
        strict_fields=STRICT_FIELDS,
    )
    order_comparison = compare_normalized_rows(
        replay_orders,
        dryrun_orders,
        row_type="order",
        key_candidates=ORDER_KEY_CANDIDATES,
        strict_fields=ORDER_STRICT_FIELDS,
    )
    fill_comparison = compare_normalized_rows(
        replay_fills,
        dryrun_fills,
        row_type="fill",
        key_candidates=FILL_KEY_CANDIDATES,
        strict_fields=FILL_STRICT_FIELDS,
    )
    ready_fill_intents = strict_ready_fill_intents(replay_fills, dryrun_fills)
    event_comparison = suppress_partial_fill_residual_cancel_mismatches(
        event_comparison,
        ready_fill_intents=ready_fill_intents,
    )
    order_comparison = suppress_partial_fill_residual_cancel_mismatches(
        order_comparison,
        ready_fill_intents=ready_fill_intents,
    )
    missing_strict_fields = sorted(
        set(event_comparison["missing_strict_fields"])
        | set(order_comparison["missing_strict_fields"])
        | set(fill_comparison["missing_strict_fields"])
    )
    mismatches = event_comparison["mismatches"] + order_comparison["mismatches"] + fill_comparison["mismatches"]
    ignored_partial_fill_cancel_status_mismatches = (
        event_comparison.get("ignored_partial_fill_cancel_status_mismatches", [])
        + order_comparison.get("ignored_partial_fill_cancel_status_mismatches", [])
    )
    settlement_mismatches = settlement_exit_price_mismatches(mismatches)
    strict_parity_ready = (
        event_comparison["strict_parity_ready"]
        and order_comparison["strict_parity_ready"]
        and fill_comparison["strict_parity_ready"]
    )
    return {
        "events": event_comparison,
        "orders": order_comparison,
        "fills": fill_comparison,
        "missing_strict_fields": missing_strict_fields,
        "mismatches": mismatches[:100],
        "settlement_exit_mismatches": settlement_mismatches,
        "ignored_partial_fill_cancel_status_mismatches": ignored_partial_fill_cancel_status_mismatches[:100],
        "ignored_non_executed_exit_attempts": {
            "replay_events": [runtime_row_summary(row) for row in ignored_replay_events[:50]],
            "dryrun_events": [runtime_row_summary(row) for row in ignored_dryrun_events[:50]],
            "replay_orders": [runtime_row_summary(row) for row in ignored_replay_orders[:50]],
            "dryrun_orders": [runtime_row_summary(row) for row in ignored_dryrun_orders[:50]],
        },
        "strict_parity_ready": strict_parity_ready,
    }


def has_no_comparable_runtime_sample(runtime_evidence_comparison: dict[str, Any]) -> bool:
    """Return true when the selected window has no replay or dry-run rows to compare."""
    for row_type in ("events", "orders", "fills"):
        comparison = runtime_evidence_comparison.get(row_type) or {}
        if comparison.get("replay_count", 0) != 0:
            return False
        if comparison.get("dryrun_count", 0) != 0:
            return False
        if comparison.get("shared_count", 0) != 0:
            return False
    return True


def compare_events(
    replay_events: list[dict[str, Any]], dryrun_events: list[dict[str, Any]]
) -> dict[str, Any]:
    replay_index = index_events(replay_events)
    dryrun_index = index_events(dryrun_events)
    shared_keys = sorted(set(replay_index) & set(dryrun_index))
    mismatches: list[dict[str, Any]] = []
    missing_fields = set()
    replay_available = available_strict_fields(replay_events)
    dryrun_available = available_strict_fields(dryrun_events)

    for key in shared_keys:
        replay = replay_index[key]
        dryrun = dryrun_index[key]
        normalized_replay = normalize_event(replay)
        normalized_dryrun = normalize_event(dryrun)
        for field in STRICT_FIELDS:
            replay_value = normalized_replay.get(field)
            dryrun_value = normalized_dryrun.get(field)
            if replay_value is None or dryrun_value is None:
                missing_fields.add(field)
                continue
            if not values_match(field, replay_value, dryrun_value):
                mismatches.append(
                    {
                        "event_key": key,
                        "field": field,
                        "replay": replay_value,
                        "dryrun": dryrun_value,
                    }
                )

    return {
        "replay_event_count": len(replay_events),
        "dryrun_event_count": len(dryrun_events),
        "shared_event_count": len(shared_keys),
        "strict_required_fields": STRICT_FIELDS,
        "replay_available_strict_fields": replay_available,
        "dryrun_available_strict_fields": dryrun_available,
        "missing_replay_events": sorted(set(dryrun_index) - set(replay_index))[:50],
        "missing_dryrun_events": sorted(set(replay_index) - set(dryrun_index))[:50],
        "mismatches": mismatches[:100],
        "missing_strict_fields": sorted(missing_fields),
        "strict_parity_ready": bool(shared_keys) and not missing_fields and not mismatches,
    }


def build_result(
    replay: Any,
    dryrun: Any,
    replay_path: Path,
    dryrun_path: Path,
    *,
    deployment_id: str | None = None,
    since: datetime | None = None,
    until: datetime | None = None,
) -> dict[str, Any]:
    replay_events = filter_rows(
        extract_events(replay),
        deployment_id=deployment_id,
        since=since,
        until=until,
        timestamp_keys=("decision_ts", "opened_at", "timestamp", "created_at", "fill_timestamp"),
    )
    dryrun_events = filter_rows(
        extract_events(dryrun),
        deployment_id=deployment_id,
        since=since,
        until=until,
        timestamp_keys=("decision_ts", "opened_at", "timestamp", "created_at", "fill_timestamp"),
    )
    event_comparison = compare_events(replay_events, dryrun_events)
    runtime_evidence_comparison = compare_runtime_evidence(
        replay,
        dryrun,
        deployment_id=deployment_id,
        since=since,
        until=until,
    )
    risk_flags: list[str] = []

    if runtime_evidence_comparison["events"]["replay_count"] == 0:
        risk_flags.append("replay_has_no_event_level_rows")
    if runtime_evidence_comparison["events"]["dryrun_count"] == 0:
        risk_flags.append("dryrun_has_no_event_level_rows")
    if runtime_evidence_comparison["events"]["missing_dryrun_rows"]:
        risk_flags.append("events_present_in_replay_missing_from_dryrun")
    if runtime_evidence_comparison["events"]["missing_replay_rows"]:
        risk_flags.append("events_present_in_dryrun_missing_from_replay")
    if runtime_evidence_comparison["orders"]["replay_count"] == 0:
        risk_flags.append("replay_has_no_order_level_rows")
    if runtime_evidence_comparison["orders"]["dryrun_count"] == 0:
        risk_flags.append("dryrun_has_no_order_level_rows")
    if runtime_evidence_comparison["fills"]["replay_count"] == 0:
        risk_flags.append("replay_has_no_fill_level_rows")
    if runtime_evidence_comparison["fills"]["dryrun_count"] == 0:
        risk_flags.append("dryrun_has_no_fill_level_rows")
    if runtime_evidence_comparison["orders"]["missing_dryrun_rows"]:
        risk_flags.append("orders_present_in_replay_missing_from_dryrun")
    if runtime_evidence_comparison["orders"]["missing_replay_rows"]:
        risk_flags.append("orders_present_in_dryrun_missing_from_replay")
    if runtime_evidence_comparison["fills"]["missing_dryrun_rows"]:
        risk_flags.append("fills_present_in_replay_missing_from_dryrun")
    if runtime_evidence_comparison["fills"]["missing_replay_rows"]:
        risk_flags.append("fills_present_in_dryrun_missing_from_replay")
    if runtime_evidence_comparison["mismatches"]:
        risk_flags.append("runtime_evidence_field_mismatches")
    if runtime_evidence_comparison["settlement_exit_mismatches"]:
        risk_flags.append("settlement_exit_price_mismatches")
    if runtime_evidence_comparison["missing_strict_fields"]:
        risk_flags.append("missing_runtime_evidence_strict_fields")

    legacy_event_flags: list[str] = []
    if not replay_events and runtime_evidence_comparison["events"]["replay_count"] == 0:
        legacy_event_flags.append("replay_has_no_legacy_event_level_rows")
    if not dryrun_events and runtime_evidence_comparison["events"]["dryrun_count"] == 0:
        legacy_event_flags.append("dryrun_has_no_legacy_event_level_rows")
    if (
        event_comparison["missing_dryrun_events"]
        and not runtime_evidence_comparison["events"]["missing_dryrun_rows"]
    ):
        legacy_event_flags.append("legacy_events_present_in_replay_missing_from_dryrun")
    if (
        event_comparison["missing_replay_events"]
        and not runtime_evidence_comparison["events"]["missing_replay_rows"]
    ):
        legacy_event_flags.append("legacy_events_present_in_dryrun_missing_from_replay")
    if event_comparison["mismatches"]:
        legacy_event_flags.append("legacy_event_strict_field_mismatches")
    if event_comparison["missing_strict_fields"]:
        legacy_event_flags.append("legacy_event_missing_strict_parity_fields")

    decision = "continue"
    no_comparable_runtime_sample = has_no_comparable_runtime_sample(runtime_evidence_comparison)
    if no_comparable_runtime_sample:
        decision = "collect-more"
    elif not runtime_evidence_comparison["strict_parity_ready"]:
        decision = "fix-data-or-runtime-mismatch"
    advisory_flags: list[str] = []
    if no_comparable_runtime_sample:
        advisory_flags.append("no_comparable_runtime_sample")
    blocking_risk_flags = [] if no_comparable_runtime_sample else list(risk_flags)

    return {
        "schema_version": 1,
        "artifact_type": "strategy_parity_evaluation",
        "producer": "replay_dryrun_parity",
        "generated_at": datetime.now(timezone.utc).isoformat(),
        "replay_source": str(replay_path),
        "dryrun_source": str(dryrun_path),
        "filters": filter_summary(deployment_id=deployment_id, since=since, until=until),
        "replay_metrics": extract_metrics(replay),
        "dryrun_metrics": extract_metrics(dryrun),
        "event_comparison": event_comparison,
        "runtime_evidence_comparison": runtime_evidence_comparison,
        "legacy_event_flags": legacy_event_flags,
        "risk_flags": risk_flags,
        "blocking_risk_flags": blocking_risk_flags,
        "advisory_flags": advisory_flags,
        "decision": decision,
    }


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--replay-json", required=True)
    parser.add_argument("--dryrun-json", required=True)
    parser.add_argument("--output-json", required=True)
    parser.add_argument("--deployment-id")
    parser.add_argument("--since", help="Inclusive ISO-8601 lower timestamp bound")
    parser.add_argument("--until", help="Inclusive ISO-8601 upper timestamp bound")
    args = parser.parse_args()

    replay_path = find_first_json(Path(args.replay_json))
    dryrun_path = find_first_json(Path(args.dryrun_json))
    since = parse_timestamp(args.since) if args.since else None
    until = parse_timestamp(args.until) if args.until else None
    if args.since and since is None:
        raise SystemExit(f"invalid --since timestamp: {args.since}")
    if args.until and until is None:
        raise SystemExit(f"invalid --until timestamp: {args.until}")
    result = build_result(
        load_json(replay_path),
        load_json(dryrun_path),
        replay_path,
        dryrun_path,
        deployment_id=args.deployment_id,
        since=since,
        until=until,
    )

    output_path = Path(args.output_json)
    output_path.parent.mkdir(parents=True, exist_ok=True)
    with output_path.open("w") as handle:
        json.dump(result, handle, indent=2, sort_keys=True)
        handle.write("\n")
    print(json.dumps(result, indent=2, sort_keys=True))


if __name__ == "__main__":
    main()
