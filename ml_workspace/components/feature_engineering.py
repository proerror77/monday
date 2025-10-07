#!/usr/bin/env python3
from __future__ import annotations

"""
Feature engineering facade used by CLI and workflows.

Responsibilities:
- Query ClickHouse for second-aggregated microstructure features
- Build normalized rolling sequences
- Produce train/val splits compatible with DLTrainer
"""

from dataclasses import dataclass
from typing import Any, Dict, List, Tuple
import logging
from datetime import datetime, timedelta, timezone

import numpy as np
import pandas as pd
import torch

from utils.clickhouse_client import ClickHouseClient
from utils.ch_queries import build_feature_sql
from utils.time import to_ms
from utils.cache import cache_enabled, make_features_cache_path, load_cached_df, save_df
from algorithms.dl.sequence import build_sequences
from utils.feature_registry import get_default_ms_feature_names

logger = logging.getLogger(__name__)


def order_feature_columns(columns: List[str]) -> List[str]:
    """Order feature columns according to registry, excluding metadata.

    Keeps only intersection of registry and provided columns, in registry order.
    Metadata columns like symbol/exchange_ts/mid_price/sec are excluded by design.
    """
    exclude = {"sec", "symbol", "exchange_ts", "mid_price"}
    available = [c for c in columns if c not in exclude]
    registry = get_default_ms_feature_names()
    # preserve registry order, keep only those present
    ordered = [c for c in registry if c in available]
    # warn if some registry features are missing (data/source drift)
    missing = [c for c in registry if c not in available]
    if missing:
        logger.warning("Missing features from registry (will proceed without): %s", missing)
    # append any extra columns at the end (rare)
    extras = [c for c in available if c not in registry]
    if extras:
        logger.info("Extra features not in registry (appended at end): %s", extras)
    return ordered + extras


@dataclass
class FEConfig:
    seq_len: int = 60
    horizon_sec: int = 5
    max_samples: int | None = None
    train_ratio: float = 0.8


class FeatureEngineer:
    """High-level feature extractor with a simple API.

    Example output keys:
    - train_features: torch.Tensor [N, T, F]
    - train_labels: torch.Tensor [N]
    - val_features: torch.Tensor [M, T, F]
    - val_labels: torch.Tensor [M]
    - sequence_length, feature_dim
    """

    def __init__(self, cfg: FEConfig | None = None, client: ClickHouseClient | None = None):
        self.cfg = cfg or FEConfig()
        self.client = client or ClickHouseClient()

    async def extract_features_for_training(self, symbol: str, hours: int = 24, refresh: bool = False) -> Dict[str, Any]:
        """Fetch features from ClickHouse, build sequences, and return train/val tensors."""
        end_dt = datetime.now(timezone.utc)
        start_dt = end_dt - timedelta(hours=hours)
        start_ms, end_ms = to_ms(start_dt), to_ms(end_dt)

        # Query per-second features with optional local cache
        cache_path = make_features_cache_path(symbol, start_ms, end_ms, suffix=f"seq{self.cfg.seq_len}")
        df = load_cached_df(cache_path, refresh=refresh) if cache_enabled() else None
        if df is None or df.empty:
            sql = build_feature_sql(symbol, start_ms, end_ms)
            df = self.client.query_to_dataframe(sql)
            if df is not None and not df.empty and cache_enabled():
                save_df(df, cache_path)
        if df is None or df.empty:
            raise RuntimeError(f"No features returned for {symbol} in last {hours}h")

        # Determine feature columns ordered by registry
        feature_cols = order_feature_columns(list(df.columns))
        # Ensure we have mid_price for label construction
        if "mid_price" not in df.columns:
            raise RuntimeError("Feature SQL must include mid_price column for label construction")

        # Build sequences (single horizon for DLTrainer)
        horizons = [int(self.cfg.horizon_sec)]
        X, y_multi = build_sequences(
            df=df, feature_cols=feature_cols, horizons=horizons, seq_len=self.cfg.seq_len, max_samples=self.cfg.max_samples
        )
        if X.numel() == 0:
            raise RuntimeError("Insufficient data to build sequences")

        # Extract single-horizon labels to 1D
        y = y_multi[:, 0].contiguous()

        # Train/val split by ratio
        n = X.size(0)
        n_train = max(1, int(self.cfg.train_ratio * n))
        train_x, val_x = X[:n_train], X[n_train:]
        train_y, val_y = y[:n_train], y[n_train:]
        if val_x.numel() == 0:
            # fallback ensure at least 1 sample in val when tiny dataset
            train_x, val_x = X[:-1], X[-1:]
            train_y, val_y = y[:-1], y[-1:]

        return {
            "train_features": train_x,
            "train_labels": train_y,
            "val_features": val_x,
            "val_labels": val_y,
            "sequence_length": int(self.cfg.seq_len),
            "feature_dim": int(X.size(-1)),
            "feature_columns": feature_cols,
            "symbol": symbol,
            "time_range": {"start_ms": start_ms, "end_ms": end_ms},
        }
