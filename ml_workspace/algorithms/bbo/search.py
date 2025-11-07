#!/usr/bin/env python3
from __future__ import annotations

import random
from dataclasses import dataclass
from typing import Any, Dict, Optional


@dataclass
class BBOConfig:
    n_trials: int = 20
    direction: str = "maximize"  # target: ic
    seed: Optional[int] = None


def _try_import_optuna():
    try:
        import optuna  # type: ignore
        return optuna
    except Exception:
        return None


class BBOOptimizer:
    """Black-box optimizer for DL hyperparameters.

    Works with Optuna if available; falls back to random search otherwise.
    Expects a trainer_factory that accepts a config dict and returns a trainer
    with a .train(features)->{"metrics":..., "model_path": ...} interface.
    """

    def __init__(self, trainer_factory, config: Optional[Dict[str, Any]] = None):
        self.cfg = BBOConfig(**(config or {}))
        self.trainer_factory = trainer_factory
        # Optional seed for reproducibility (random search)
        if self.cfg.seed is not None:
            random.seed(self.cfg.seed)

    def optimize(self, features: Dict[str, Any]) -> Dict[str, Any]:
        optuna = _try_import_optuna()
        best: Dict[str, Any] = {"score": float("-inf"), "params": {}, "metrics": {}, "model_path": ""}

        def objective(trial_params: Dict[str, Any]) -> Dict[str, Any]:
            trainer = self.trainer_factory(trial_params)
            result = trainer.train(features)
            metrics = result.get("metrics", {})
            score = float(metrics.get("ic", 0.0))  # target IC by default
            return {"score": score, "metrics": metrics, "model_path": result.get("model_path", ""), "params": trial_params}

        if optuna is not None:
            def _objective(trial):
                params = {
                    "d_model": trial.suggest_categorical("d_model", [32, 64, 96, 128]),
                    "nhead": trial.suggest_categorical("nhead", [2, 4, 8]),
                    "num_layers": trial.suggest_int("num_layers", 1, 4),
                    "dropout": trial.suggest_float("dropout", 0.05, 0.3),
                    "lr": trial.suggest_float("lr", 1e-4, 5e-3, log=True),
                    "weight_decay": trial.suggest_float("weight_decay", 1e-6, 1e-3, log=True),
                    "batch_size": trial.suggest_categorical("batch_size", [128, 256, 512]),
                    "epochs": trial.suggest_int("epochs", 3, 8),
                }
                res = objective(params)
                # Track best
                nonlocal best
                if res["score"] > best["score"]:
                    best = res
                return res["score"]

            study = optuna.create_study(direction=self.cfg.direction)
            study.optimize(_objective, n_trials=self.cfg.n_trials, show_progress_bar=False)
        else:
            # Fallback random search
            space = {
                "d_model": [32, 64, 96, 128],
                "nhead": [2, 4, 8],
                "num_layers": [1, 2, 3, 4],
                "dropout": [0.05, 0.1, 0.2, 0.3],
                "lr": [1e-4, 3e-4, 1e-3, 3e-3],
                "weight_decay": [1e-6, 1e-5, 1e-4, 1e-3],
                "batch_size": [128, 256, 512],
                "epochs": [3, 4, 5, 6, 8],
            }
            for _ in range(self.cfg.n_trials):
                params = {k: random.choice(v) for k, v in space.items()}
                res = objective(params)
                if res["score"] > best["score"]:
                    best = res

        return {
            "best_score": best["score"],
            "best_params": best["params"],
            "best_metrics": best["metrics"],
            "model_path": best["model_path"],
        }


__all__ = ["BBOOptimizer", "BBOConfig"]
