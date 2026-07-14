#!/usr/bin/env python3
"""Shared AutoFactor target accounting catalog.

The JSON catalog is the single repo-local contract for mapping an AutoFactor
target label to its horizon and accounting lane. Promotion, replay builders,
and Rust alpha-search code should not hand-roll target -> horizon mappings.
"""

from __future__ import annotations

import json
from functools import lru_cache
from pathlib import Path
from typing import Any


CATALOG_SCHEMA_VERSION = "autofactor_accounting_catalog.v1"
CATALOG_PATH = Path(__file__).resolve().parents[1] / "config" / "autofactor_accounting_catalog.json"


@lru_cache(maxsize=1)
def load_autofactor_accounting_catalog() -> dict[str, Any]:
    payload = json.loads(CATALOG_PATH.read_text(encoding="utf-8"))
    if payload.get("schema_version") != CATALOG_SCHEMA_VERSION:
        raise ValueError(
            "unsupported AutoFactor accounting catalog schema: "
            f"{payload.get('schema_version')!r}"
        )
    targets = payload.get("targets")
    if not isinstance(targets, dict):
        raise ValueError("AutoFactor accounting catalog missing targets object")
    return payload


def autofactor_target_contract(target: str) -> dict[str, Any] | None:
    targets = load_autofactor_accounting_catalog()["targets"]
    contract = targets.get(target)
    if contract is None:
        return None
    return dict(contract)


def autofactor_target_horizon(target: str) -> str:
    contract = autofactor_target_contract(target)
    if contract is None:
        return "unknown"
    return str(contract.get("horizon") or "unknown")


def validate_autofactor_source_contract(*, target: str, horizon: str) -> list[str]:
    blockers: list[str] = []
    if not target:
        blockers.append("missing_source_target")
        return blockers
    contract = autofactor_target_contract(target)
    if contract is None:
        blockers.append(f"unknown_source_target:{target}")
        return blockers
    expected_horizon = str(contract.get("horizon") or "")
    if not horizon:
        blockers.append("missing_source_horizon")
    elif expected_horizon and horizon != expected_horizon:
        blockers.append(f"source_horizon_mismatch:{horizon}!={expected_horizon}")
    return blockers
