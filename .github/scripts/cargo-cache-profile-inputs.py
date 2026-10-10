#!/usr/bin/env python3
"""Bind selected owning-workspace profiles without hashing dependency declarations."""

import json
from pathlib import Path
import sys
import tomllib


def profile_inputs(root: Path, inputs: dict) -> dict:
    registry = json.loads((root / "rust_hft/workspaces.json").read_text())
    if registry.get("schema") != "monday.cargo_workspaces.v1":
        raise ValueError("unsupported workspace registry")
    owners = registry["workspaces"]
    if not owners or any(set(owner) != {"id", "manifest"} for owner in owners):
        raise ValueError("unsupported workspace entry")
    by_manifest = {owner["manifest"]: owner for owner in owners}
    if len(by_manifest) != len(owners) or len({owner["id"] for owner in owners}) != len(owners):
        raise ValueError("duplicate workspace entry")
    selected = sorted({recipe["manifest"] for recipe in inputs["recipes"]})
    if not selected or any(manifest not in by_manifest for manifest in selected):
        raise ValueError("unadmitted cache owner")
    profiles = {}
    for manifest in selected:
        path = root / "rust_hft" / manifest
        if not path.resolve().is_relative_to((root / "rust_hft").resolve()):
            raise ValueError("cache owner escaped repository")
        with path.open("rb") as source:
            parsed = tomllib.load(source)
        if not isinstance(parsed.get("workspace"), dict) or not isinstance(parsed.get("profile", {}), dict):
            raise ValueError("cache owner is not a standalone workspace with valid profiles")
        # Every custom profile, inherited profile, package override and build
        # override remains bound. Cargo resolves dependencies from the exact key.
        profiles[manifest] = parsed.get("profile", {})
    return {"schema": "monday.cargo-cache-profiles.v1",
            "owners": [by_manifest[manifest] for manifest in selected], "profiles": profiles}


if __name__ == "__main__":
    try:
        result = profile_inputs(Path(sys.argv[1]), json.loads(Path(sys.argv[2]).read_text()))
        output = json.dumps(result, sort_keys=True, separators=(",", ":"), allow_nan=False)
    except (ValueError, KeyError, TypeError, OSError, IndexError) as error:
        sys.exit(f"invalid Cargo cache profile inputs: {error}")
    print(output)
