"""
Lightweight configuration loader for ClickHouse and pipeline settings.

- Loads environment variables from `.env` when present (python-dotenv).
- Provides typed dataclasses for ClickHouse and pipeline config.
"""

from __future__ import annotations

import os
from dataclasses import dataclass
from typing import Optional

try:
    # Optional; no hard dependency at runtime if not installed
    from dotenv import load_dotenv

    load_dotenv()
except Exception:
    # Best-effort: ignore if dotenv is not available
    pass


@dataclass
class ClickHouseSettings:
    host: str
    port: int
    username: str
    password: str
    database: str
    secure: bool = True


def env_bool(key: str, default: bool) -> bool:
    val = os.getenv(key)
    if val is None:
        return default
    return str(val).strip().lower() in {"1", "true", "yes", "y", "on"}


def get_clickhouse_settings() -> ClickHouseSettings:
    """Return ClickHouse settings from environment with safe defaults.

    Env vars:
    - CLICKHOUSE_HOST, CLICKHOUSE_PORT, CLICKHOUSE_USERNAME, CLICKHOUSE_PASSWORD,
      CLICKHOUSE_DATABASE, CLICKHOUSE_SECURE
    """
    host = os.getenv(
        "CLICKHOUSE_HOST",
        # Safe placeholder; do not embed credentials in code. Override via .env
        "https://localhost:8443",
    )
    port = int(os.getenv("CLICKHOUSE_PORT", "8443"))
    username = os.getenv("CLICKHOUSE_USERNAME", "default")
    password = os.getenv("CLICKHOUSE_PASSWORD", "")
    database = os.getenv("CLICKHOUSE_DATABASE", "default")
    secure = env_bool("CLICKHOUSE_SECURE", True)

    return ClickHouseSettings(
        host=host,
        port=port,
        username=username,
        password=password,
        database=database,
        secure=secure,
    )


@dataclass
class PipelineSettings:
    symbol: str = "WLFIUSDT"
    seq_len: int = 60
    min_exit_sec: int = 1
    max_exit_sec: int = 60


def get_pipeline_settings() -> PipelineSettings:
    return PipelineSettings(
        symbol=os.getenv("SYMBOL", "WLFIUSDT"),
        seq_len=int(os.getenv("SEQ_LEN", "60")),
        min_exit_sec=int(os.getenv("MIN_EXIT_SEC", "1")),
        max_exit_sec=int(os.getenv("MAX_EXIT_SEC", "60")),
    )

