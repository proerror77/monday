from __future__ import annotations

import json
import os
import shutil
import time
from pathlib import Path
from typing import Any, Dict

import torch


def export_runtime_artifact(
    model_path: str,
    out_dir: str | os.PathLike,
    strategy: str = "prediction_gate",
    strategy_params: Dict[str, Any] | None = None,
) -> str:
    ckpt = torch.load(model_path, map_location="cpu")
    feature_dim = int(ckpt.get("feature_dim", 0))
    sequence_length = int(ckpt.get("sequence_length", 0))
    cfg = ckpt.get("config", {})

    out = Path(out_dir)
    out.mkdir(parents=True, exist_ok=True)

    # Copy model
    model_dst = out / Path(model_path).name
    shutil.copy2(model_path, model_dst)

    # Optional TorchScript
    ts_path = None
    for cand in (Path(model_path).with_name(Path(model_path).stem + "_ts.pt"),):
        if cand.exists():
            ts_path = out / cand.name
            shutil.copy2(cand, ts_path)

    manifest = {
        "created_at": int(time.time()),
        "model": {
            "format": "torch",
            "path": str(model_dst.name),
            "torchscript": str(ts_path.name) if ts_path else None,
            "feature_dim": feature_dim,
            "sequence_length": sequence_length,
            "train_config": cfg,
        },
        "strategy": {
            "name": strategy,
            "params": strategy_params or {},
        },
        "feature_spec": {
            "clock": "event",
            "resample_ms": 0,
            "note": "Demo export. Align online feature builder with offline DataPreprocessor.",
        },
        "version": "v1-demo",
    }

    with open(out / "manifest.json", "w") as f:
        json.dump(manifest, f, indent=2)

    return str(out)

