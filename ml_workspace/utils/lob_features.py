#!/usr/bin/env python3
"""
Order Book Dynamics and MLOFI Features
訂單簿動力學和MLOFI特徵計算
"""

import numpy as np
import pandas as pd
from typing import Dict, List, Optional, Tuple


class LOBDynamicsFeatures:
    """計算訂單簿動力學特徵，包括MLOFI"""
    
    @staticmethod
    def compute_mlofi(
        bid_prices: np.ndarray,
        bid_sizes: np.ndarray,
        ask_prices: np.ndarray,
        ask_sizes: np.ndarray,
        mid_price: float,
        decay_factor: float = 0.5
    ) -> float:
        """
        計算Market Liquidity Order Flow Imbalance (MLOFI)
        
        MLOFI考慮價格距離的流動性分佈，距離mid price越遠的訂單權重越低
        
        Args:
            bid_prices: 買方價格數組
            bid_sizes: 買方數量數組
            ask_prices: 賣方價格數組
            ask_sizes: 賣方數量數組
            mid_price: 中間價
            decay_factor: 價格距離衰減因子
            
        Returns:
            MLOFI值 (-1 到 1)
        """
        # 計算價格距離權重
        bid_distances = np.abs(bid_prices - mid_price) / mid_price
        ask_distances = np.abs(ask_prices - mid_price) / mid_price
        
        # 指數衰減權重
        bid_weights = np.exp(-decay_factor * bid_distances * 10000)  # Convert to bps
        ask_weights = np.exp(-decay_factor * ask_distances * 10000)
        
        # 加權流動性
        weighted_bid_liquidity = np.sum(bid_sizes * bid_weights)
        weighted_ask_liquidity = np.sum(ask_sizes * ask_weights)
        
        # MLOFI
        total_liquidity = weighted_bid_liquidity + weighted_ask_liquidity
        if total_liquidity > 0:
            mlofi = (weighted_bid_liquidity - weighted_ask_liquidity) / total_liquidity
        else:
            mlofi = 0.0
            
        return mlofi
    
    @staticmethod
    def compute_lob_slope(
        prices: np.ndarray,
        sizes: np.ndarray,
        side: str = 'bid'
    ) -> float:
        """
        計算訂單簿斜率（流動性供給曲線的斜率）
        
        Args:
            prices: 價格數組
            sizes: 數量數組
            side: 'bid' 或 'ask'
            
        Returns:
            斜率值
        """
        if len(prices) < 2:
            return 0.0
            
        # 累積數量
        cum_sizes = np.cumsum(sizes)
        
        # 價格變化
        if side == 'bid':
            price_changes = prices[0] - prices  # Distance from best bid
        else:
            price_changes = prices - prices[0]  # Distance from best ask
            
        # 避免除零
        price_changes = np.maximum(price_changes, 1e-8)
        
        # 計算斜率（累積量 / 價格變化）
        slopes = cum_sizes / price_changes
        
        # 返回平均斜率
        return np.mean(slopes[1:])  # Skip first point (zero distance)
    
    @staticmethod
    def compute_order_book_pressure(
        bid_sizes: np.ndarray,
        ask_sizes: np.ndarray,
        levels: int = 5
    ) -> Dict[str, float]:
        """
        計算訂單簿壓力指標
        
        Args:
            bid_sizes: 買方數量數組
            ask_sizes: 賣方數量數組
            levels: 計算的深度層級
            
        Returns:
            壓力指標字典
        """
        bid_total = np.sum(bid_sizes[:levels])
        ask_total = np.sum(ask_sizes[:levels])
        
        # 基礎壓力
        if bid_total + ask_total > 0:
            pressure = (bid_total - ask_total) / (bid_total + ask_total)
        else:
            pressure = 0.0
        
        # 加權壓力（近端權重更高）
        weights = np.exp(-np.arange(levels) * 0.2)
        weighted_bid = np.sum(bid_sizes[:levels] * weights[:len(bid_sizes[:levels])])
        weighted_ask = np.sum(ask_sizes[:levels] * weights[:len(ask_sizes[:levels])])
        
        if weighted_bid + weighted_ask > 0:
            weighted_pressure = (weighted_bid - weighted_ask) / (weighted_bid + weighted_ask)
        else:
            weighted_pressure = 0.0
        
        return {
            'pressure': pressure,
            'weighted_pressure': weighted_pressure,
            'bid_total': bid_total,
            'ask_total': ask_total
        }
    
    @staticmethod
    def compute_microprice(
        bid_price: float,
        bid_size: float,
        ask_price: float,
        ask_size: float
    ) -> float:
        """
        計算Microprice（考慮size的加權中間價）
        
        Microprice = (bid_price * ask_size + ask_price * bid_size) / (bid_size + ask_size)
        
        Args:
            bid_price: 最佳買價
            bid_size: 最佳買量
            ask_price: 最佳賣價
            ask_size: 最佳賣量
            
        Returns:
            Microprice
        """
        total_size = bid_size + ask_size
        if total_size > 0:
            return (bid_price * ask_size + ask_price * bid_size) / total_size
        else:
            return (bid_price + ask_price) / 2
    
    @staticmethod
    def compute_volume_clock_imbalance(
        trades: pd.DataFrame,
        window: int = 100
    ) -> float:
        """
        計算Volume Clock Imbalance（基於成交量的不平衡）
        
        Args:
            trades: 交易數據DataFrame (需要columns: side, size)
            window: 計算窗口（交易筆數）
            
        Returns:
            Volume clock imbalance
        """
        if len(trades) < window:
            return 0.0
            
        recent_trades = trades.tail(window)
        buy_volume = recent_trades[recent_trades['side'] == 'buy']['size'].sum()
        sell_volume = recent_trades[recent_trades['side'] == 'sell']['size'].sum()
        
        total_volume = buy_volume + sell_volume
        if total_volume > 0:
            return (buy_volume - sell_volume) / total_volume
        else:
            return 0.0
    
    @staticmethod
    def compute_kyle_lambda(
        price_changes: np.ndarray,
        signed_volumes: np.ndarray
    ) -> float:
        """
        計算Kyle's Lambda（價格影響係數）
        
        Lambda = Cov(ΔP, signed_volume) / Var(signed_volume)
        
        Args:
            price_changes: 價格變化數組
            signed_volumes: 帶符號的成交量（買為正，賣為負）
            
        Returns:
            Kyle's lambda
        """
        if len(price_changes) < 2:
            return 0.0
            
        # Remove NaN and inf
        mask = np.isfinite(price_changes) & np.isfinite(signed_volumes)
        price_changes = price_changes[mask]
        signed_volumes = signed_volumes[mask]
        
        if len(signed_volumes) < 2:
            return 0.0
            
        var_volume = np.var(signed_volumes)
        if var_volume > 0:
            return np.cov(price_changes, signed_volumes)[0, 1] / var_volume
        else:
            return 0.0


def enhance_features_with_lob_dynamics(df: pd.DataFrame) -> pd.DataFrame:
    """
    增強數據框架，添加LOB動力學特徵
    
    Args:
        df: 原始特徵DataFrame
        
    Returns:
        包含LOB動力學特徵的DataFrame
    """
    # 創建LOB特徵計算器
    lob_calc = LOBDynamicsFeatures()
    
    # 添加MLOFI
    if all(col in df.columns for col in ['bid_price', 'ask_price', 'bid_size', 'ask_size', 'mid_price']):
        # 簡化版MLOFI（只用L1數據）
        df['mlofi_l1'] = df.apply(
            lambda row: lob_calc.compute_mlofi(
                np.array([row['bid_price']]),
                np.array([row['bid_size']]),
                np.array([row['ask_price']]),
                np.array([row['ask_size']]),
                row['mid_price'],
                decay_factor=0.5
            ),
            axis=1
        )
    
    # 添加Microprice
    if all(col in df.columns for col in ['bid_price', 'bid_size', 'ask_price', 'ask_size']):
        df['microprice'] = df.apply(
            lambda row: lob_calc.compute_microprice(
                row['bid_price'],
                row['bid_size'],
                row['ask_price'],
                row['ask_size']
            ),
            axis=1
        )
        
        # Microprice vs Midprice差異
        df['microprice_diff'] = (df['microprice'] - df['mid_price']) / df['mid_price'] * 10000  # in bps
    
    # 添加訂單簿壓力的時間變化
    if 'bbo_imbalance' in df.columns:
        df['bbo_imbalance_change'] = df['bbo_imbalance'].diff()
        df['bbo_imbalance_ma5'] = df['bbo_imbalance'].rolling(5).mean()
        df['bbo_imbalance_std5'] = df['bbo_imbalance'].rolling(5).std()
    
    if 'depth_imbalance_5' in df.columns:
        df['depth_imbalance_5_change'] = df['depth_imbalance_5'].diff()
        df['depth_imbalance_5_ma5'] = df['depth_imbalance_5'].rolling(5).mean()
        
    if 'depth_imbalance_10' in df.columns:
        df['depth_imbalance_10_change'] = df['depth_imbalance_10'].diff()
        df['depth_imbalance_10_ma5'] = df['depth_imbalance_10'].rolling(5).mean()
    
    # 添加成交量動量
    if 'order_flow_imbalance' in df.columns:
        df['ofi_momentum'] = df['order_flow_imbalance'].rolling(10).mean()
        df['ofi_acceleration'] = df['ofi_momentum'].diff()
    
    # 添加價格動量特徵
    if 'mid_price' in df.columns:
        df['price_momentum_5'] = df['mid_price'].pct_change(5)
        df['price_momentum_10'] = df['mid_price'].pct_change(10)
        df['price_momentum_30'] = df['mid_price'].pct_change(30)
        
        # 價格波動率
        df['price_volatility_10'] = df['mid_price'].pct_change().rolling(10).std()
        df['price_volatility_30'] = df['mid_price'].pct_change().rolling(30).std()
    
    # 添加spread動態
    if 'spread_bps' in df.columns:
        df['spread_ma5'] = df['spread_bps'].rolling(5).mean()
        df['spread_std5'] = df['spread_bps'].rolling(5).std()
        df['spread_change'] = df['spread_bps'].diff()
    
    # 添加成交量特徵
    if all(col in df.columns for col in ['buy_vol_1s', 'sell_vol_1s', 'volume_1s']):
        # 成交量比率
        df['buy_ratio'] = df['buy_vol_1s'] / (df['volume_1s'] + 1e-8)
        df['sell_ratio'] = df['sell_vol_1s'] / (df['volume_1s'] + 1e-8)
        
        # 成交量移動平均
        df['volume_ma10'] = df['volume_1s'].rolling(10).mean()
        df['volume_ratio'] = df['volume_1s'] / (df['volume_ma10'] + 1e-8)
        
        # 大單檢測（volume spike）
        df['volume_spike'] = (df['volume_1s'] > df['volume_ma10'] * 2).astype(float)
    
    # 填充NaN值
    df = df.fillna(0)
    
    return df