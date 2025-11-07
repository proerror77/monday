"""
毫秒级特征工程（使用注册表契约）
为毫秒级HFT数据设计的特征，时间窗口适配到秒级而非分钟级。
"""

from utils.feature_registry import get_default_ms_feature_names

def get_millisecond_feature_sql(symbol: str, start_ms: int, end_ms: int) -> str:
    """
    为毫秒级数据优化的特征选择
    去掉分钟级特征，专注于即时特征和简单衍生特征
    """

    table_name = 'hft_features_eth_local_ob'

    # 简化的毫秒级特征SQL，避免复杂窗口函数
    sql = f"""
    WITH
    -- 计算全局标准化统计量 (只包含现有列)
    global_stats AS (
        SELECT
            avg(spread_abs) as avg_spread_abs,
            stddevPop(spread_abs) + 1e-8 as std_spread_abs,
            avg(spread_pct) as avg_spread_pct,
            stddevPop(spread_pct) + 1e-8 as std_spread_pct,
            avg(best_bid_size) as avg_best_bid_size,
            stddevPop(best_bid_size) + 1e-8 as std_best_bid_size,
            avg(best_ask_size) as avg_best_ask_size,
            stddevPop(best_ask_size) + 1e-8 as std_best_ask_size,
            avg(size_imbalance) as avg_size_imbalance,
            stddevPop(size_imbalance) + 1e-8 as std_size_imbalance,
            avg(instant_trades) as avg_instant_trades,
            stddevPop(instant_trades) + 1e-8 as std_instant_trades,
            avg(instant_volume) as avg_instant_volume,
            stddevPop(instant_volume) + 1e-8 as std_instant_volume,
            avg(instant_buy_volume) as avg_instant_buy_volume,
            stddevPop(instant_buy_volume) + 1e-8 as std_instant_buy_volume,
            avg(instant_sell_volume) as avg_instant_sell_volume,
            stddevPop(instant_sell_volume) + 1e-8 as std_instant_sell_volume,
            avg(instant_volume_imbalance) as avg_instant_volume_imbalance,
            stddevPop(instant_volume_imbalance) + 1e-8 as std_instant_volume_imbalance,
            avg(trade_aggression) as avg_trade_aggression,
            stddevPop(trade_aggression) + 1e-8 as std_trade_aggression,
            avg(trade_price_deviation_bps) as avg_trade_price_deviation_bps,
            stddevPop(trade_price_deviation_bps) + 1e-8 as std_trade_price_deviation_bps,
            avg(price_change) as avg_price_change,
            stddevPop(price_change) + 1e-8 as std_price_change,
            avg(bid_change) as avg_bid_change,
            stddevPop(bid_change) + 1e-8 as std_bid_change,
            avg(ask_change) as avg_ask_change,
            stddevPop(ask_change) + 1e-8 as std_ask_change,
            avg(bid_size_change) as avg_bid_size_change,
            stddevPop(bid_size_change) + 1e-8 as std_bid_size_change,
            avg(ask_size_change) as avg_ask_size_change,
            stddevPop(ask_size_change) + 1e-8 as std_ask_size_change
        FROM {table_name}
        WHERE exchange_ts BETWEEN {start_ms} AND {end_ms}
        AND symbol = '{symbol}'
    )

    SELECT
        exchange_ts,
        symbol,
        mid_price,

        -- 标准化即时特征 (核心BBO特征)
        (spread_abs - global_stats.avg_spread_abs) / global_stats.std_spread_abs as spread_abs,
        (spread_pct - global_stats.avg_spread_pct) / global_stats.std_spread_pct as spread_pct,
        (best_bid_size - global_stats.avg_best_bid_size) / global_stats.std_best_bid_size as best_bid_size,
        (best_ask_size - global_stats.avg_best_ask_size) / global_stats.std_best_ask_size as best_ask_size,
        (size_imbalance - global_stats.avg_size_imbalance) / global_stats.std_size_imbalance as size_imbalance,

        -- 标准化即时交易特征
        (instant_trades - global_stats.avg_instant_trades) / global_stats.std_instant_trades as instant_trades,
        (instant_volume - global_stats.avg_instant_volume) / global_stats.std_instant_volume as instant_volume,
        (instant_buy_volume - global_stats.avg_instant_buy_volume) / global_stats.std_instant_buy_volume as instant_buy_volume,
        (instant_sell_volume - global_stats.avg_instant_sell_volume) / global_stats.std_instant_sell_volume as instant_sell_volume,
        (instant_volume_imbalance - global_stats.avg_instant_volume_imbalance) / global_stats.std_instant_volume_imbalance as instant_volume_imbalance,
        (trade_aggression - global_stats.avg_trade_aggression) / global_stats.std_trade_aggression as trade_aggression,
        (trade_price_deviation_bps - global_stats.avg_trade_price_deviation_bps) / global_stats.std_trade_price_deviation_bps as trade_price_deviation_bps,

        -- 标准化价格变化特征 (反映订单簿动态)
        (price_change - global_stats.avg_price_change) / global_stats.std_price_change as price_change,
        (bid_change - global_stats.avg_bid_change) / global_stats.std_bid_change as bid_change,
        (ask_change - global_stats.avg_ask_change) / global_stats.std_ask_change as ask_change,
        (bid_size_change - global_stats.avg_bid_size_change) / global_stats.std_bid_size_change as bid_size_change,
        (ask_size_change - global_stats.avg_ask_size_change) / global_stats.std_ask_size_change as ask_size_change,

        -- 即时衍生特征 (无需窗口函数)
        CASE WHEN instant_trades > 0 THEN instant_volume / instant_trades ELSE 0 END as avg_trade_size_raw,
        CASE WHEN spread_abs > 0 THEN instant_volume / spread_abs ELSE 0 END as volume_per_spread_raw,
        CASE WHEN best_bid_size + best_ask_size > 0
             THEN (best_bid_size - best_ask_size) / (best_bid_size + best_ask_size)
             ELSE 0 END as depth_imbalance_raw,

        -- 交易强度指标
        CASE WHEN instant_volume > 0 THEN instant_trades / instant_volume ELSE 0 END as trade_intensity,

        -- 价格效率指标
        CASE WHEN abs(price_change) > 0 THEN instant_volume / abs(price_change) ELSE 0 END as liquidity_ratio

    FROM {table_name}, global_stats
    WHERE exchange_ts BETWEEN {start_ms} AND {end_ms}
    AND symbol = '{symbol}'
    ORDER BY exchange_ts ASC
    """

    return sql


def get_millisecond_feature_columns():
    """返回适合毫秒级数据的特征列名列表（由契约提供）"""
    return get_default_ms_feature_names()
