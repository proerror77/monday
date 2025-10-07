"""
专注alpha信号的特征工程
排除可能泄漏的mid_price，专注于订单流和微观结构特征
"""

def get_alpha_focused_feature_sql(symbol: str, start_ms: int, end_ms: int) -> str:
    """
    返回专注alpha信号的特征SQL，排除mid_price
    基于相关性分析，选择最有预测价值的特征
    """

    table_name = 'hft_features_eth_local_ob'

    sql = f"""
    WITH
    -- 计算全局标准化统计量 (排除mid_price)
    global_stats AS (
        SELECT
            -- BBO特征统计 (排除mid_price)
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

            -- 高相关性特征的统计
            avg(cum_book_pressure_1m) as avg_cum_book_pressure_1m,
            stddevPop(cum_book_pressure_1m) + 1e-8 as std_cum_book_pressure_1m,
            avg(order_flow_imbalance_1m) as avg_order_flow_imbalance_1m,
            stddevPop(order_flow_imbalance_1m) + 1e-8 as std_order_flow_imbalance_1m,
            avg(aggressive_trade_ratio_1m) as avg_aggressive_trade_ratio_1m,
            stddevPop(aggressive_trade_ratio_1m) + 1e-8 as std_aggressive_trade_ratio_1m,
            avg(cum_volume_imbalance_1m) as avg_cum_volume_imbalance_1m,
            stddevPop(cum_volume_imbalance_1m) + 1e-8 as std_cum_volume_imbalance_1m,
            avg(cum_volume_imbalance_5m) as avg_cum_volume_imbalance_5m,
            stddevPop(cum_volume_imbalance_5m) + 1e-8 as std_cum_volume_imbalance_5m,
            avg(market_rhythm_cos) as avg_market_rhythm_cos,
            stddevPop(market_rhythm_cos) + 1e-8 as std_market_rhythm_cos,
            avg(market_rhythm_sin) as avg_market_rhythm_sin,
            stddevPop(market_rhythm_sin) + 1e-8 as std_market_rhythm_sin,

            -- 即时交易特征
            avg(instant_trades) as avg_instant_trades,
            stddevPop(instant_trades) + 1e-8 as std_instant_trades,
            avg(instant_volume) as avg_instant_volume,
            stddevPop(instant_volume) + 1e-8 as std_instant_volume,
            avg(instant_volume_imbalance) as avg_instant_volume_imbalance,
            stddevPop(instant_volume_imbalance) + 1e-8 as std_instant_volume_imbalance,
            avg(trade_aggression) as avg_trade_aggression,
            stddevPop(trade_aggression) + 1e-8 as std_trade_aggression,

            -- 价格变化特征 (订单簿动态)
            avg(price_change) as avg_price_change,
            stddevPop(price_change) + 1e-8 as std_price_change,
            avg(bid_change) as avg_bid_change,
            stddevPop(bid_change) + 1e-8 as std_bid_change,
            avg(ask_change) as avg_ask_change,
            stddevPop(ask_change) + 1e-8 as std_ask_change,

            -- 交易量累积特征
            avg(cum_trades_1m) as avg_cum_trades_1m,
            stddevPop(cum_trades_1m) + 1e-8 as std_cum_trades_1m,
            avg(aggressive_trades_1m) as avg_aggressive_trades_1m,
            stddevPop(aggressive_trades_1m) + 1e-8 as std_aggressive_trades_1m,
            avg(trade_frequency) as avg_trade_frequency,
            stddevPop(trade_frequency) + 1e-8 as std_trade_frequency,

            -- 波动性特征
            avg(spread_volatility_1m) as avg_spread_volatility_1m,
            stddevPop(spread_volatility_1m) + 1e-8 as std_spread_volatility_1m,

            -- 组合信号特征
            avg(combined_imbalance_signal) as avg_combined_imbalance_signal,
            stddevPop(combined_imbalance_signal) + 1e-8 as std_combined_imbalance_signal,
            avg(liquidity_stress_indicator) as avg_liquidity_stress_indicator,
            stddevPop(liquidity_stress_indicator) + 1e-8 as std_liquidity_stress_indicator
        FROM {table_name}
        WHERE exchange_ts BETWEEN {start_ms} AND {end_ms}
        AND symbol = '{symbol}'
    )

    SELECT
        exchange_ts,
        symbol,
        -- 不包含mid_price，避免数据泄漏

        -- 标准化BBO特征
        (spread_abs - global_stats.avg_spread_abs) / global_stats.std_spread_abs as spread_abs,
        (spread_pct - global_stats.avg_spread_pct) / global_stats.std_spread_pct as spread_pct,
        (best_bid_size - global_stats.avg_best_bid_size) / global_stats.std_best_bid_size as best_bid_size,
        (best_ask_size - global_stats.avg_best_ask_size) / global_stats.std_best_ask_size as best_ask_size,
        (size_imbalance - global_stats.avg_size_imbalance) / global_stats.std_size_imbalance as size_imbalance,

        -- 高相关性alpha特征 (标准化)
        (cum_book_pressure_1m - global_stats.avg_cum_book_pressure_1m) / global_stats.std_cum_book_pressure_1m as cum_book_pressure_1m,
        (order_flow_imbalance_1m - global_stats.avg_order_flow_imbalance_1m) / global_stats.std_order_flow_imbalance_1m as order_flow_imbalance_1m,
        (aggressive_trade_ratio_1m - global_stats.avg_aggressive_trade_ratio_1m) / global_stats.std_aggressive_trade_ratio_1m as aggressive_trade_ratio_1m,
        (cum_volume_imbalance_1m - global_stats.avg_cum_volume_imbalance_1m) / global_stats.std_cum_volume_imbalance_1m as cum_volume_imbalance_1m,
        (cum_volume_imbalance_5m - global_stats.avg_cum_volume_imbalance_5m) / global_stats.std_cum_volume_imbalance_5m as cum_volume_imbalance_5m,
        (market_rhythm_cos - global_stats.avg_market_rhythm_cos) / global_stats.std_market_rhythm_cos as market_rhythm_cos,
        (market_rhythm_sin - global_stats.avg_market_rhythm_sin) / global_stats.std_market_rhythm_sin as market_rhythm_sin,

        -- 标准化即时交易特征
        (instant_trades - global_stats.avg_instant_trades) / global_stats.std_instant_trades as instant_trades,
        (instant_volume - global_stats.avg_instant_volume) / global_stats.std_instant_volume as instant_volume,
        (instant_volume_imbalance - global_stats.avg_instant_volume_imbalance) / global_stats.std_instant_volume_imbalance as instant_volume_imbalance,
        (trade_aggression - global_stats.avg_trade_aggression) / global_stats.std_trade_aggression as trade_aggression,

        -- 标准化价格变化特征
        (price_change - global_stats.avg_price_change) / global_stats.std_price_change as price_change,
        (bid_change - global_stats.avg_bid_change) / global_stats.std_bid_change as bid_change,
        (ask_change - global_stats.avg_ask_change) / global_stats.std_ask_change as ask_change,

        -- 标准化交易量特征
        (cum_trades_1m - global_stats.avg_cum_trades_1m) / global_stats.std_cum_trades_1m as cum_trades_1m,
        (aggressive_trades_1m - global_stats.avg_aggressive_trades_1m) / global_stats.std_aggressive_trades_1m as aggressive_trades_1m,
        (trade_frequency - global_stats.avg_trade_frequency) / global_stats.std_trade_frequency as trade_frequency,

        -- 标准化波动性特征
        (spread_volatility_1m - global_stats.avg_spread_volatility_1m) / global_stats.std_spread_volatility_1m as spread_volatility_1m,

        -- 标准化组合信号特征
        (combined_imbalance_signal - global_stats.avg_combined_imbalance_signal) / global_stats.std_combined_imbalance_signal as combined_imbalance_signal,
        (liquidity_stress_indicator - global_stats.avg_liquidity_stress_indicator) / global_stats.std_liquidity_stress_indicator as liquidity_stress_indicator,

        -- 即时衍生特征 (无需标准化的比率)
        CASE WHEN instant_trades > 0 THEN instant_volume / instant_trades ELSE 0 END as avg_trade_size_ratio,
        CASE WHEN spread_abs > 0 THEN instant_volume / spread_abs ELSE 0 END as volume_per_spread_ratio,
        CASE WHEN best_bid_size + best_ask_size > 0
             THEN (best_bid_size - best_ask_size) / (best_bid_size + best_ask_size)
             ELSE 0 END as depth_imbalance_ratio

    FROM {table_name}, global_stats
    WHERE exchange_ts BETWEEN {start_ms} AND {end_ms}
    AND symbol = '{symbol}'
    ORDER BY exchange_ts ASC
    """

    return sql


def get_alpha_focused_feature_columns():
    """返回专注alpha信号的特征列名列表（排除mid_price）"""
    return [
        # BBO特征
        'spread_abs', 'spread_pct', 'best_bid_size', 'best_ask_size', 'size_imbalance',

        # 高相关性alpha特征
        'cum_book_pressure_1m', 'order_flow_imbalance_1m', 'aggressive_trade_ratio_1m',
        'cum_volume_imbalance_1m', 'cum_volume_imbalance_5m',
        'market_rhythm_cos', 'market_rhythm_sin',

        # 即时交易特征
        'instant_trades', 'instant_volume', 'instant_volume_imbalance', 'trade_aggression',

        # 价格变化特征
        'price_change', 'bid_change', 'ask_change',

        # 交易量特征
        'cum_trades_1m', 'aggressive_trades_1m', 'trade_frequency',

        # 波动性特征
        'spread_volatility_1m',

        # 组合信号特征
        'combined_imbalance_signal', 'liquidity_stress_indicator',

        # 衍生比率特征
        'avg_trade_size_ratio', 'volume_per_spread_ratio', 'depth_imbalance_ratio'
    ]