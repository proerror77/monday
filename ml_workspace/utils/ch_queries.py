"""
ClickHouse SQL builders for per-second queue dynamics features.

Provides parameterized SQL that aggregates BBO, Depth, and Trades into a
second-level feature table for a given symbol and time range.
"""

from __future__ import annotations

from typing import Final
from .feature_registry import get_default_ms_feature_names


def get_unified_features_table() -> str:
    """Return the unified ClickHouse features table name."""
    return 'hft_features_unified_v2'

def build_normalized_features_sql(symbol: str, start_ms: int, end_ms: int) -> str:
    """Return SQL to get globally normalized features from ClickHouse.

    This approach does the heavy lifting (normalization) in ClickHouse but
    handles sequence construction in Python for better compatibility.

    Uses the unified features table with symbol filtering.
    """

    # 使用统一特征表
    table_name = get_unified_features_table()

    # 使用默認毫秒級特徵列表（不包含 mid_price/exchange_ts/symbol）
    feature_cols = get_default_ms_feature_names()

    # 构建标准化的特征选择
    normalized_features = []
    for col in feature_cols:
        normalized_features.append(f"({col} - avg_{col}) / stddev_{col} AS {col}")

    # 使用统一特征表，通过symbol字段过滤
    return f"""
    WITH
    -- 1. 计算全局统计量 (基于指定symbol和时间范围)
    global_stats AS (
        SELECT
            {', '.join([f'avg({col}) AS avg_{col}' for col in feature_cols])},
            {', '.join([f'stddevPop({col}) + 0.00000001 AS stddev_{col}' for col in feature_cols])}
        FROM {table_name}
        WHERE symbol = '{symbol}'
          AND exchange_ts BETWEEN {start_ms} AND {end_ms}
    )

    -- 2. 返回标准化的特征
    SELECT
        exchange_ts,
        symbol,
        mid_price,
        {', '.join(normalized_features)}
    FROM {table_name}, global_stats
    WHERE symbol = '{symbol}'
      AND exchange_ts BETWEEN {start_ms} AND {end_ms}
    ORDER BY exchange_ts ASC
    """

def build_feature_sql(symbol: str, start_ms: int, end_ms: int, limit: int = 500000, use_millisecond_features: bool = True, use_alpha_focused: bool = False) -> str:
    """Return SQL to load precomputed features from hft_features_eth_local_ob table.

    修复: 添加LIMIT参数避免超时，与成功训练脚本保持一致。

    Args:
        use_millisecond_features: 如果True，使用适配毫秒级数据的特征工程
        use_alpha_focused: 如果True，使用专注alpha信号的特征（排除mid_price）
    """

    if use_alpha_focused:
        from .alpha_features import get_alpha_focused_feature_sql
        return get_alpha_focused_feature_sql(symbol, start_ms, end_ms)

    if use_millisecond_features:
        from .millisecond_features import get_millisecond_feature_sql
        return get_millisecond_feature_sql(symbol, start_ms, end_ms)

    return f"""
    SELECT
        exchange_ts,
        symbol,

        -- 基础BBO特征
        mid_price,
        spread_abs,
        spread_pct,
        best_bid_size,
        best_ask_size,
        size_imbalance,

        -- 交易特征
        instant_trades,
        instant_volume,
        instant_buy_volume,
        instant_sell_volume,
        instant_volume_imbalance,

        -- 本地订单簿重构特征
        trade_aggression,
        trade_price_deviation_bps,
        price_change,
        bid_change,
        ask_change,
        bid_size_change,
        ask_size_change,
        inferred_book_impact,

        -- 累积特征 1分钟
        cum_buy_volume_1m,
        cum_sell_volume_1m,
        cum_volume_imbalance_1m,
        cum_trades_1m,
        cum_ask_consumption_1m,
        cum_bid_consumption_1m,
        order_flow_imbalance_1m,
        cum_book_pressure_1m,

        -- 累积特征 5分钟
        cum_buy_volume_5m,
        cum_sell_volume_5m,
        cum_volume_imbalance_5m,

        -- 交易侵略性特征
        aggressive_trades_1m,
        aggressive_trade_ratio_1m,
        trade_frequency,

        -- 波动性特征
        price_volatility_1m,
        spread_volatility_1m,

        -- 组合信号特征
        combined_imbalance_signal,
        liquidity_stress_indicator,
        volatility_flow_interaction,
        depth_to_activity_ratio,
        spread_volatility_ratio

    FROM hft_features_eth_local_ob
    WHERE exchange_ts >= {start_ms}
      AND exchange_ts <= {end_ms}
    ORDER BY exchange_ts
    LIMIT {limit}
    """
