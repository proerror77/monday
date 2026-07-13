#!/usr/bin/env python3
"""Shared AutoFactor runtime contract loading and validation.

Factor discovery may infer many research-side expressions, but promotion and
candidate replay must only advance rows that carry a typed runtime contract
when a registry preview is present. The fallback name mapping below exists only
for legacy artifacts that predate registry contracts.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from functools import lru_cache
from pathlib import Path
from typing import Any


SETTLEMENT_RUNTIME_MAPPING = {
    "strategy_profile": "settlement_probability",
    "strategy_family": "settlement_probability",
}
RUNTIME_CONTRACT_CATALOG_SCHEMA_VERSION = "autofactor_runtime_contract_catalog.v1"
RUNTIME_CONTRACT_CATALOG_PATH = (
    Path(__file__).resolve().parents[1] / "config" / "autofactor_runtime_contract_catalog.json"
)

PREDICTIVE_FORMULA_BASES = (
    "amplitude_weighted_momentum_30s_sigma",
    "poly_lag_pressure",
    "spread_adjusted_external_move",
)

SETTLEMENT_FORMULA_BASES = (
    "auto_settlement_full_depth_settlement_edge",
    "auto_settlement_conservative_settlement_edge",
    "auto_settlement_model_full_depth_settlement_edge",
    "auto_settlement_model_conservative_settlement_edge",
)

LEGACY_BUILTIN_RUNTIME_MAPPINGS: dict[str, dict[str, str]] = {
    "spread_adjusted_external_move": {
        "strategy_profile": "repricing_momentum",
        "strategy_family": "repricing",
        "runtime_score": "spread_adjusted_external_move_score",
    },
    "repricing_gap_side_10s": {
        "strategy_profile": "repricing_momentum",
        "strategy_family": "repricing",
        "runtime_score": "repricing_gap_side_10s",
    },
    "amplitude_weighted_momentum_30s_sigma": {
        "strategy_profile": "settlement_probability",
        "strategy_family": "predictive_settlement_probability",
        "runtime_score": "autofactor_formula:amplitude_weighted_momentum_30s_sigma",
    },
    "mut_amplitude_weighted_momentum_30s_sigma_full_depth_entry_gate": {
        "strategy_profile": "settlement_probability",
        "strategy_family": "predictive_settlement_probability",
        "runtime_score": "autofactor_formula:mut_amplitude_weighted_momentum_30s_sigma_full_depth_entry_gate",
    },
    "mut_spread_adjusted_external_move_full_depth_entry_gate": {
        "strategy_profile": "settlement_probability",
        "strategy_family": "predictive_settlement_probability",
        "runtime_score": "autofactor_formula:mut_spread_adjusted_external_move_full_depth_entry_gate",
    },
    "settlement_fair_edge": {
        "strategy_profile": "",
        "strategy_family": "settlement_probability",
        "runtime_score": "",
    },
}

for _base in SETTLEMENT_FORMULA_BASES:
    for _suffix in (
        "",
        "_x_near_strike",
        "_x_capacity",
        "_x_entry_price_quality",
        "_x_near_strike_x_capacity",
        "_x_near_strike_x_capacity_x_entry_price_quality",
        "_spread_adjusted",
        "_x_external_pressure",
        "_x_iv_change",
    ):
        _name = f"{_base}{_suffix}"
        LEGACY_BUILTIN_RUNTIME_MAPPINGS[_name] = {
            **SETTLEMENT_RUNTIME_MAPPING,
            "runtime_score": f"autofactor_formula:{_name}",
        }


@lru_cache(maxsize=1)
def load_runtime_contract_catalog() -> dict[str, Any]:
    payload = json.loads(RUNTIME_CONTRACT_CATALOG_PATH.read_text(encoding="utf-8"))
    if payload.get("schema_version") != RUNTIME_CONTRACT_CATALOG_SCHEMA_VERSION:
        raise ValueError(
            "unsupported AutoFactor runtime contract catalog schema: "
            f"{payload.get('schema_version')!r}"
        )
    mappings = payload.get("research_input_mappings")
    if not isinstance(mappings, dict):
        raise ValueError("AutoFactor runtime contract catalog missing research_input_mappings")
    return payload


def runtime_input_contract(input_name: str) -> dict[str, Any] | None:
    contract = load_runtime_contract_catalog()["research_input_mappings"].get(input_name)
    if not isinstance(contract, dict):
        return None
    return dict(contract)


def runtime_contract_supported_runtime_inputs() -> set[str]:
    supported: set[str] = set()
    for raw_contract in load_runtime_contract_catalog()["research_input_mappings"].values():
        if not isinstance(raw_contract, dict):
            continue
        if raw_contract.get("blocker"):
            continue
        runtime_names = raw_contract.get("runtime_input_names")
        if isinstance(runtime_names, list):
            supported.update(str(item) for item in runtime_names if str(item))
    return supported


def normalize_formula_name(name: str) -> str:
    while True:
        for prefix in ("llm_", "mut2_", "mut_", "mcts_"):
            if name.startswith(prefix):
                name = name[len(prefix) :]
                break
        else:
            return name


def settlement_formula_suffix_supported(suffix: str) -> bool:
    if not suffix:
        return True
    tokens = suffix.removeprefix("_").split("_")
    applied: set[str] = set()
    for token in tokens:
        effect = {
            "strike": "near_strike",
            "capacity": "capacity",
            "quality": "entry_price_quality",
            "adjusted": "spread_adjusted",
            "pressure": "external_pressure",
            "change": "iv_change",
            "gate": "full_depth_entry_gate",
            "squashed": "squashed",
        }.get(token)
        if effect:
            if effect in applied:
                return False
            applied.add(effect)
            continue
        if token not in {
            "x",
            "near",
            "full",
            "depth",
            "entry",
            "price",
            "spread",
            "external",
            "iv",
        }:
            return False
    return True


def is_settlement_formula(name: str) -> bool:
    normalized = normalize_formula_name(name)
    for base in SETTLEMENT_FORMULA_BASES:
        if normalized.startswith(base):
            return settlement_formula_suffix_supported(normalized[len(base) :])
    return False


def is_settlement_predictive_formula(name: str) -> bool:
    normalized = normalize_formula_name(name)
    if normalized == "spread_adjusted_external_move":
        return False
    return any(normalized.startswith(base) for base in PREDICTIVE_FORMULA_BASES)


def inferred_runtime_mapping(name: str) -> dict[str, str] | None:
    if name in LEGACY_BUILTIN_RUNTIME_MAPPINGS:
        return dict(LEGACY_BUILTIN_RUNTIME_MAPPINGS[name])
    if is_settlement_formula(name):
        return {
            **SETTLEMENT_RUNTIME_MAPPING,
            "runtime_score": f"autofactor_formula:{name}",
        }
    if is_settlement_predictive_formula(name):
        return {
            **SETTLEMENT_RUNTIME_MAPPING,
            "strategy_family": "predictive_settlement_probability",
            "runtime_score": f"autofactor_formula:{name}",
        }
    return None


def runtime_input_blockers(input_names: list[str]) -> list[str]:
    blockers: list[str] = []
    for name in sorted({str(item) for item in input_names if str(item)}):
        contract = runtime_input_contract(name)
        if contract is None:
            blockers.append(f"runtime_input_unsupported:{name}")
            continue
        blocker = contract.get("blocker")
        if blocker:
            blockers.append(str(blocker))
            continue
        runtime_names = contract.get("runtime_input_names")
        if not isinstance(runtime_names, list) or not runtime_names:
            blockers.append(f"runtime_input_unsupported:{name}")
    return blockers


def runtime_contract_input_names(contract: dict[str, Any]) -> list[str]:
    runtime_input_names = contract.get("runtime_input_names")
    if isinstance(runtime_input_names, list) and runtime_input_names:
        return [str(v) for v in runtime_input_names if str(v)]
    legacy_input_names = contract.get("input_names")
    if isinstance(legacy_input_names, list) and legacy_input_names:
        return [str(v) for v in legacy_input_names if str(v)]
    return []


@dataclass
class RuntimeContractResolution:
    factor_name: str
    runtime_mapping: dict[str, str] | None = None
    runtime_contract: dict[str, Any] | None = None
    blockers: list[str] = field(default_factory=list)
    source: str = "legacy_inference"


class RuntimeContractResolver:
    def __init__(
        self,
        *,
        registry_preview_json: str | None = None,
        runtime_mapping_json: str | None = None,
        require_runtime_contract: bool = False,
    ) -> None:
        self.registry_preview_json = registry_preview_json
        self.require_runtime_contract = require_runtime_contract
        self.registry_contracts: dict[str, RuntimeContractResolution] = {}
        self.legacy_mappings = dict(LEGACY_BUILTIN_RUNTIME_MAPPINGS)
        if runtime_mapping_json:
            self._load_legacy_runtime_mapping(runtime_mapping_json)
        if registry_preview_json:
            self._load_registry_preview(registry_preview_json)

    @property
    def registry_present(self) -> bool:
        return bool(self.registry_preview_json)

    def _load_legacy_runtime_mapping(self, path: str) -> None:
        payload = json.loads(Path(path).read_text(encoding="utf-8"))
        for name, value in payload.items():
            if isinstance(value, dict):
                self.legacy_mappings[str(name)] = {str(k): str(v) for k, v in value.items()}

    def _load_registry_preview(self, path: str) -> None:
        payload = json.loads(Path(path).read_text(encoding="utf-8"))
        factors = payload.get("factors")
        if not isinstance(factors, list):
            raise ValueError(f"runtime registry preview has no factors array: {path}")
        for item in factors:
            if not isinstance(item, dict):
                continue
            name = str(item.get("factor_name") or item.get("name") or "")
            if not name:
                continue
            contract = item.get("runtime_contract")
            blockers = [str(v) for v in item.get("blockers") or [] if str(v)]
            if not isinstance(contract, dict):
                blockers.append(f"missing_runtime_contract:{name}")
                self.registry_contracts[name] = RuntimeContractResolution(
                    factor_name=name,
                    blockers=sorted(set(blockers)),
                    source="registry_preview",
                )
                continue
            if contract.get("version") != "autofactor_runtime_contract_v1":
                blockers.append("unsupported_runtime_contract_version")
            blockers.extend(str(v) for v in contract.get("blockers") or [] if str(v))
            input_names = runtime_contract_input_names(contract)
            if not input_names:
                blockers.append(f"missing_runtime_contract_input_names:{name}")
            blockers.extend(runtime_input_blockers(input_names))
            mapping = {
                "strategy_profile": str(contract.get("strategy_profile") or ""),
                "strategy_family": str(contract.get("strategy_family") or ""),
                "runtime_score": str(contract.get("runtime_score") or ""),
            }
            if not mapping["strategy_profile"] or not mapping["runtime_score"]:
                blockers.append(f"incomplete_runtime_contract_mapping:{name}")
            self.registry_contracts[name] = RuntimeContractResolution(
                factor_name=name,
                runtime_mapping=mapping,
                runtime_contract=contract,
                blockers=sorted(set(blockers)),
                source="registry_preview",
            )

    def resolve(self, factor_name: str) -> RuntimeContractResolution:
        if self.registry_present:
            resolved = self.registry_contracts.get(factor_name)
            if resolved:
                return resolved
            return RuntimeContractResolution(
                factor_name=factor_name,
                blockers=[f"missing_runtime_contract:{factor_name}"],
                source="registry_preview",
            )
        if self.require_runtime_contract:
            return RuntimeContractResolution(
                factor_name=factor_name,
                blockers=["missing_runtime_contract_registry"],
                source="required_contract_missing_registry",
            )
        mapping = self.legacy_mappings.get(factor_name) or inferred_runtime_mapping(factor_name)
        return RuntimeContractResolution(
            factor_name=factor_name,
            runtime_mapping=mapping,
            runtime_contract=None,
            blockers=[],
            source="legacy_inference",
        )
