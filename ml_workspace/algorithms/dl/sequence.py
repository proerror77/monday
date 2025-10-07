"""
Sequence utilities for DL/forecasting models.

Centralizes common logic used by training/backtest flows:
- compute_multi_horizon_returns: label construction on per-second mid_price
- build_sequences: normalized rolling window sequences and multi-horizon targets
"""

from __future__ import annotations

from typing import List, Tuple

import numpy as np
import pandas as pd
import torch


def compute_multi_horizon_returns(df: pd.DataFrame, horizons: List[int]) -> pd.DataFrame:
    """Compute future returns at multiple horizons on per-second mid_price series.

    Uses alignment by shifting the index (sec+h aligned back to sec) to avoid
    issues when seconds are missing in the time series.
    """
    out = df[["sec", "mid_price"]].copy()
    out = out.dropna().reset_index(drop=True)
    out.set_index("sec", inplace=True)
    for h in horizons:
        fut = out["mid_price"].rename(f"mid_price_fut_{h}")
        fut.index = fut.index - h  # current sec aligns with future sec+h
        joined = out.join(fut, how="left")
        ret = (joined[f"mid_price_fut_{h}"] - joined["mid_price"]) / (joined["mid_price"] + 1e-12)
        out[f"ret_{h}s"] = ret
    out.reset_index(inplace=True)
    return out.drop(columns=["mid_price"])


def build_sequences(
    df: pd.DataFrame,
    feature_cols: List[str],
    horizons: List[int],
    seq_len: int,
    max_samples: int | None = None,
    use_rolling_norm: bool = True,
    random_seed: int = 42,
) -> Tuple[torch.Tensor, torch.Tensor]:
    """Build normalized rolling-window sequences with multi-horizon labels.

    - Sort by sec
    - Compute labels via compute_multi_horizon_returns
    - Rolling-window standardization (consistent with inference/backtest)
    - Return X: [N, T, F], y: [N, H]
    
    Args:
        use_rolling_norm: If True, use rolling window normalization per sequence.
                         If False, use global normalization (legacy behavior).
    """
    df = df.sort_values("sec").reset_index(drop=True)
    labels = compute_multi_horizon_returns(df[["sec", "mid_price"]].copy(), horizons)
    df = df.merge(labels, on="sec", how="inner")

    # drop rows with NaN in any target
    for h in horizons:
        df = df[~df[f"ret_{h}s"].isna()]

    X_list: list[np.ndarray] = []
    y_list: list[np.ndarray] = []
    max_h = max(horizons)
    n = len(df)
    if n <= seq_len + max_h:
        return torch.empty(0), torch.empty(0)

    feats = df[feature_cols].values.astype(np.float32)

    idxs = np.arange(seq_len, n - max_h)
    if max_samples is not None and max_samples < len(idxs):
        rng = np.random.default_rng(random_seed)
        idxs = rng.choice(idxs, size=max_samples, replace=False)

    for i in idxs:
        # Extract raw window
        window = feats[i - seq_len : i, :]
        
        if use_rolling_norm:
            # Rolling window normalization (consistent with inference)
            mean = window.mean(axis=0, keepdims=True)
            std = window.std(axis=0, keepdims=True) + 1e-8
            normalized_window = (window - mean) / std
        else:
            # Global normalization (legacy)
            if i == idxs[0]:  # compute global stats once
                global_mean = feats.mean(axis=0, keepdims=True)
                global_std = feats.std(axis=0, keepdims=True) + 1e-8
            normalized_window = (window - global_mean) / global_std
        
        X_list.append(normalized_window)
        y_list.append(df.loc[i, [f"ret_{h}s" for h in horizons]].values.astype(np.float32))

    X = torch.tensor(np.stack(X_list), dtype=torch.float32)
    y = torch.tensor(np.stack(y_list), dtype=torch.float32)
    return X, y

