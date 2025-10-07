#!/usr/bin/env python3
from __future__ import annotations

import os
from pathlib import Path
from typing import Optional
import time

import pandas as pd


def _env_bool(key: str, default: bool = True) -> bool:
    v = os.getenv(key)
    if v is None:
        return default
    return str(v).strip().lower() in {"1", "true", "yes", "y", "on"}


def default_cache_dir() -> Path:
    return Path(os.getenv("FEATURE_CACHE_DIR", "./cache/features")).resolve()


def make_features_cache_path(symbol: str, start_ms: int, end_ms: int, suffix: str = "features") -> Path:
    base = default_cache_dir()
    base.mkdir(parents=True, exist_ok=True)
    fname = f"{symbol}_{start_ms}_{end_ms}_{suffix}.parquet"
    return base / fname


def get_cache_ttl_hours() -> Optional[float]:
    """Return cache TTL (hours) from env FEATURE_CACHE_TTL_HOURS.

    If unset or invalid, default to 24 hours. If set to 0, treat as no TTL (always valid until deleted).
    """
    v = os.getenv("FEATURE_CACHE_TTL_HOURS")
    if v is None:
        return 24.0
    try:
        return float(v)
    except Exception:
        return 24.0


def _is_valid_by_ttl(path: Path, ttl_hours: Optional[float]) -> bool:
    if not path.exists():
        return False
    if ttl_hours is None:
        ttl_hours = get_cache_ttl_hours()
    try:
        if ttl_hours == 0:
            return True
        age_seconds = time.time() - path.stat().st_mtime
        return age_seconds <= ttl_hours * 3600.0
    except Exception:
        return False


def load_cached_df(path: Path, refresh: bool = False, ttl_hours: Optional[float] = None) -> Optional[pd.DataFrame]:
    try:
        if (not refresh) and _is_valid_by_ttl(path, ttl_hours):
            return pd.read_parquet(path)
    except Exception:
        return None
    return None


def save_df(df: pd.DataFrame, path: Path) -> None:
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        df.to_parquet(path, index=False)
    except Exception:
        # best-effort cache
        pass


def cache_enabled() -> bool:
    return _env_bool("FEATURE_CACHE_ENABLE", True)
